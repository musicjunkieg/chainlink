use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;

use crate::db::Database;
use crate::plugin::config::{resolve_env_token, LinearConfig};
use crate::plugin::sync::ConflictStrategy;
use crate::plugin::{ChainlinkEvent, Plugin, SyncConflict, SyncReport};

const LINEAR_API: &str = "https://api.linear.app/graphql";

/// Linear plugin — bidirectional sync via GraphQL API.
pub struct LinearPlugin {
    config: LinearConfig,
    client: Client,
    token: String,
}

impl LinearPlugin {
    pub fn new(config: LinearConfig) -> Result<Self> {
        let token = resolve_env_token("CHAINLINK_LINEAR_TOKEN")?;

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            config,
            client,
            token,
        })
    }

    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&self.token).unwrap(),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers
    }

    fn graphql<T: serde::de::DeserializeOwned>(&self, query: &str, variables: serde_json::Value) -> Result<T> {
        let body = serde_json::json!({
            "query": query,
            "variables": variables,
        });

        let resp = self
            .client
            .post(LINEAR_API)
            .headers(self.headers())
            .json(&body)
            .send()
            .context("Failed to call Linear API")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            bail!("Linear API error ({}): {}", status, text);
        }

        let gql_resp: GraphQLResponse<T> = resp.json().context("Failed to parse Linear response")?;

        if let Some(errors) = gql_resp.errors {
            if !errors.is_empty() {
                let msgs: Vec<String> = errors.iter().map(|e| e.message.clone()).collect();
                bail!("Linear GraphQL errors: {}", msgs.join("; "));
            }
        }

        gql_resp
            .data
            .context("Linear API returned no data")
    }

    fn map_priority_to_linear(&self, chainlink_priority: &str) -> i32 {
        // Linear priorities: 0=No priority, 1=Urgent, 2=High, 3=Medium, 4=Low
        if let Some(name) = self.config.field_map.priority.get(chainlink_priority) {
            match name.to_lowercase().as_str() {
                "urgent" | "1" => 1,
                "high" | "2" => 2,
                "medium" | "3" => 3,
                "low" | "4" => 4,
                _ => 3,
            }
        } else {
            match chainlink_priority {
                "critical" => 1,
                "high" => 2,
                "medium" => 3,
                "low" => 4,
                _ => 3,
            }
        }
    }

    fn map_priority_from_linear(&self, linear_priority: i32) -> String {
        match linear_priority {
            1 => "critical".to_string(),
            2 => "high".to_string(),
            3 => "medium".to_string(),
            4 => "low".to_string(),
            _ => "medium".to_string(),
        }
    }

    fn get_team_id(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct TeamsData {
            teams: TeamsConnection,
        }
        #[derive(Deserialize)]
        struct TeamsConnection {
            nodes: Vec<TeamNode>,
        }
        #[derive(Deserialize)]
        struct TeamNode {
            id: String,
            key: String,
        }

        let data: TeamsData = self.graphql(
            r#"query($filter: TeamFilter) {
                teams(filter: $filter) {
                    nodes { id key }
                }
            }"#,
            serde_json::json!({
                "filter": { "key": { "eq": self.config.team } }
            }),
        )?;

        data.teams
            .nodes
            .into_iter()
            .find(|t| t.key == self.config.team)
            .map(|t| t.id)
            .context(format!("Linear team '{}' not found", self.config.team))
    }

    fn create_remote_issue(
        &self,
        team_id: &str,
        title: &str,
        description: Option<&str>,
        priority: i32,
    ) -> Result<LinearIssuePayload> {
        #[derive(Deserialize)]
        struct CreateData {
            #[serde(rename = "issueCreate")]
            issue_create: IssueCreateResult,
        }
        #[derive(Deserialize)]
        struct IssueCreateResult {
            success: bool,
            issue: Option<LinearIssuePayload>,
        }

        let data: CreateData = self.graphql(
            r#"mutation($input: IssueCreateInput!) {
                issueCreate(input: $input) {
                    success
                    issue { id identifier title url priority }
                }
            }"#,
            serde_json::json!({
                "input": {
                    "teamId": team_id,
                    "title": title,
                    "description": description.unwrap_or(""),
                    "priority": priority,
                }
            }),
        )?;

        if !data.issue_create.success {
            bail!("Linear issue creation failed");
        }

        data.issue_create
            .issue
            .context("Linear returned success but no issue")
    }

    fn update_remote_issue(
        &self,
        issue_id: &str,
        title: Option<&str>,
        description: Option<&str>,
        priority: Option<i32>,
    ) -> Result<()> {
        let mut input = serde_json::Map::new();
        if let Some(t) = title {
            input.insert("title".to_string(), serde_json::json!(t));
        }
        if let Some(d) = description {
            input.insert("description".to_string(), serde_json::json!(d));
        }
        if let Some(p) = priority {
            input.insert("priority".to_string(), serde_json::json!(p));
        }

        if input.is_empty() {
            return Ok(());
        }

        #[derive(Deserialize)]
        struct UpdateData {
            #[serde(rename = "issueUpdate")]
            issue_update: UpdateResult,
        }
        #[derive(Deserialize)]
        struct UpdateResult {
            success: bool,
        }

        let data: UpdateData = self.graphql(
            r#"mutation($id: String!, $input: IssueUpdateInput!) {
                issueUpdate(id: $id, input: $input) { success }
            }"#,
            serde_json::json!({
                "id": issue_id,
                "input": serde_json::Value::Object(input),
            }),
        )?;

        if !data.issue_update.success {
            bail!("Linear issue update failed");
        }

        Ok(())
    }

    fn archive_remote_issue(&self, issue_id: &str) -> Result<()> {
        #[derive(Deserialize)]
        struct ArchiveData {
            #[serde(rename = "issueArchive")]
            issue_archive: ArchiveResult,
        }
        #[derive(Deserialize)]
        struct ArchiveResult {
            success: bool,
        }

        let data: ArchiveData = self.graphql(
            r#"mutation($id: String!) {
                issueArchive(id: $id) { success }
            }"#,
            serde_json::json!({ "id": issue_id }),
        )?;

        if !data.issue_archive.success {
            bail!("Linear issue archive failed");
        }

        Ok(())
    }

    fn unarchive_remote_issue(&self, issue_id: &str) -> Result<()> {
        #[derive(Deserialize)]
        struct UnarchiveData {
            #[serde(rename = "issueUnarchive")]
            issue_unarchive: UnarchiveResult,
        }
        #[derive(Deserialize)]
        struct UnarchiveResult {
            success: bool,
        }

        let data: UnarchiveData = self.graphql(
            r#"mutation($id: String!) {
                issueUnarchive(id: $id) { success }
            }"#,
            serde_json::json!({ "id": issue_id }),
        )?;

        if !data.issue_unarchive.success {
            bail!("Linear issue unarchive failed");
        }

        Ok(())
    }

    fn list_team_issues(&self, team_id: &str) -> Result<Vec<LinearIssuePayload>> {
        #[derive(Deserialize)]
        struct IssuesData {
            issues: IssuesConnection,
        }
        #[derive(Deserialize)]
        struct IssuesConnection {
            nodes: Vec<LinearIssuePayload>,
        }

        let data: IssuesData = self.graphql(
            r#"query($filter: IssueFilter) {
                issues(filter: $filter, first: 250) {
                    nodes {
                        id
                        identifier
                        title
                        description
                        url
                        priority
                        state { name type }
                    }
                }
            }"#,
            serde_json::json!({
                "filter": { "team": { "id": { "eq": team_id } } }
            }),
        )?;

        Ok(data.issues.nodes)
    }

    fn list_cycles(&self, team_id: &str) -> Result<Vec<LinearCycle>> {
        #[derive(Deserialize)]
        struct CyclesData {
            cycles: CyclesConnection,
        }
        #[derive(Deserialize)]
        struct CyclesConnection {
            nodes: Vec<LinearCycle>,
        }

        let data: CyclesData = self.graphql(
            r#"query($filter: CycleFilter) {
                cycles(filter: $filter, first: 50) {
                    nodes { id number name startsAt endsAt completedAt }
                }
            }"#,
            serde_json::json!({
                "filter": { "team": { "id": { "eq": team_id } } }
            }),
        )?;

        Ok(data.cycles.nodes)
    }

    fn create_comment(&self, issue_id: &str, body: &str) -> Result<()> {
        #[derive(Deserialize)]
        struct CommentData {
            #[serde(rename = "commentCreate")]
            comment_create: CommentResult,
        }
        #[derive(Deserialize)]
        struct CommentResult {
            success: bool,
        }

        let data: CommentData = self.graphql(
            r#"mutation($input: CommentCreateInput!) {
                commentCreate(input: $input) { success }
            }"#,
            serde_json::json!({
                "input": {
                    "issueId": issue_id,
                    "body": body,
                }
            }),
        )?;

        if !data.comment_create.success {
            bail!("Linear comment creation failed");
        }

        Ok(())
    }
}

impl Plugin for LinearPlugin {
    fn name(&self) -> &str {
        "linear"
    }

    fn on_event(&self, event: &ChainlinkEvent, db: &Database) -> Result<()> {
        if self.config.sync.on_mutate == "none" {
            return Ok(());
        }

        match event {
            ChainlinkEvent::IssueCreated { issue } => {
                let team_id = self.get_team_id()?;
                let priority = self.map_priority_to_linear(&issue.priority);
                let resp = self.create_remote_issue(
                    &team_id,
                    &issue.title,
                    issue.description.as_deref(),
                    priority,
                )?;

                db.upsert_plugin_sync(
                    "linear",
                    issue.id,
                    &resp.id,
                    resp.url.as_deref(),
                    None,
                    "both",
                )?;
                println!("[linear] Created {}", resp.identifier);
            }
            ChainlinkEvent::IssueUpdated {
                issue,
                changed_fields,
            } => {
                if let Some(sync) = db.get_plugin_sync("linear", issue.id)? {
                    let title = if changed_fields.contains(&"title".to_string()) {
                        Some(issue.title.as_str())
                    } else {
                        None
                    };
                    let desc = if changed_fields.contains(&"description".to_string()) {
                        issue.description.as_deref()
                    } else {
                        None
                    };
                    let priority = if changed_fields.contains(&"priority".to_string()) {
                        Some(self.map_priority_to_linear(&issue.priority))
                    } else {
                        None
                    };

                    self.update_remote_issue(&sync.remote_id, title, desc, priority)?;
                    println!("[linear] Updated {}", sync.remote_id);
                }
            }
            ChainlinkEvent::IssueClosed { issue } => {
                if let Some(sync) = db.get_plugin_sync("linear", issue.id)? {
                    // Linear uses state changes; archiving is the closest to "close"
                    // In practice you'd want to transition to a "Done" state
                    self.archive_remote_issue(&sync.remote_id)?;
                    println!("[linear] Archived {}", sync.remote_id);
                }
            }
            ChainlinkEvent::IssueReopened { issue } => {
                if let Some(sync) = db.get_plugin_sync("linear", issue.id)? {
                    self.unarchive_remote_issue(&sync.remote_id)?;
                    println!("[linear] Unarchived {}", sync.remote_id);
                }
            }
            ChainlinkEvent::CommentAdded { issue_id, comment } => {
                if let Some(sync) = db.get_plugin_sync("linear", *issue_id)? {
                    self.create_comment(&sync.remote_id, &comment.content)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn pull_sync(&self, db: &Database) -> Result<SyncReport> {
        let mut report = SyncReport::default();
        let strategy = ConflictStrategy::parse(&self.config.sync.conflict);
        let team_id = self.get_team_id()?;

        // Pull cycles as milestones
        match self.list_cycles(&team_id) {
            Ok(cycles) => {
                for cycle in cycles {
                    let existing =
                        db.get_milestone_sync_by_remote("linear", &cycle.id)?;
                    if existing.is_none() {
                        let name = cycle
                            .name
                            .unwrap_or_else(|| format!("Cycle {}", cycle.number));
                        let ms_id = db.create_milestone(&name, None)?;
                        if cycle.completed_at.is_some() {
                            db.close_milestone(ms_id)?;
                        }
                        db.upsert_milestone_sync("linear", ms_id, &cycle.id)?;
                        report.pulled += 1;
                    }
                }
            }
            Err(e) => report.errors.push(format!("cycles: {}", e)),
        }

        // Pull issues
        match self.list_team_issues(&team_id) {
            Ok(remote_issues) => {
                for remote in remote_issues {
                    let existing =
                        db.get_plugin_sync_by_remote("linear", &remote.id)?;

                    if let Some(sync_record) = existing {
                        if let Some(local) = db.get_issue(sync_record.local_issue_id)? {
                            if local.title != remote.title {
                                let conflict = SyncConflict {
                                    local_issue_id: local.id,
                                    remote_id: remote.identifier.clone(),
                                    field: "title".to_string(),
                                    local_value: local.title.clone(),
                                    remote_value: remote.title.clone(),
                                };

                                if super::sync::resolve_conflict(&conflict, &strategy)? {
                                    db.update_issue(local.id, Some(&remote.title), None, None)?;
                                    report.pulled += 1;
                                } else {
                                    report.conflicts.push(conflict);
                                }
                            }

                            db.upsert_plugin_sync(
                                "linear",
                                local.id,
                                &remote.id,
                                remote.url.as_deref(),
                                None,
                                "both",
                            )?;
                        }
                    } else {
                        let priority =
                            self.map_priority_from_linear(remote.priority.unwrap_or(3));
                        let id = db.create_issue(
                            &remote.title,
                            remote.description.as_deref(),
                            &priority,
                        )?;

                        // Check if issue is in a completed state
                        if remote.is_completed() {
                            db.close_issue(id)?;
                        }

                        db.upsert_plugin_sync(
                            "linear",
                            id,
                            &remote.id,
                            remote.url.as_deref(),
                            None,
                            "both",
                        )?;
                        report.pulled += 1;
                    }
                }
            }
            Err(e) => report.errors.push(format!("issues: {}", e)),
        }

        Ok(report)
    }

    fn push_sync(&self, db: &Database) -> Result<SyncReport> {
        let mut report = SyncReport::default();
        let team_id = self.get_team_id()?;

        let all_issues = db.list_issues(None, None, None)?;
        let synced = db.list_plugin_syncs("linear")?;
        let synced_ids: Vec<i64> = synced.iter().map(|s| s.local_issue_id).collect();

        for issue in &all_issues {
            if !synced_ids.contains(&issue.id) {
                let priority = self.map_priority_to_linear(&issue.priority);
                match self.create_remote_issue(
                    &team_id,
                    &issue.title,
                    issue.description.as_deref(),
                    priority,
                ) {
                    Ok(resp) => {
                        db.upsert_plugin_sync(
                            "linear",
                            issue.id,
                            &resp.id,
                            resp.url.as_deref(),
                            None,
                            "both",
                        )?;

                        if issue.status == "closed" {
                            self.archive_remote_issue(&resp.id)?;
                        }

                        report.pushed += 1;
                    }
                    Err(e) => report.errors.push(format!("push #{}: {}", issue.id, e)),
                }
            }
        }

        Ok(report)
    }

    fn validate_config(&self) -> Result<()> {
        let team_id = self.get_team_id()?;
        println!(
            "[linear] Connected to team {} (id: {})",
            self.config.team, team_id
        );
        Ok(())
    }
}

// ==================== Linear API Types ====================

#[derive(Debug, Deserialize)]
struct GraphQLResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GraphQLError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQLError {
    message: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LinearIssuePayload {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub url: Option<String>,
    pub priority: Option<i32>,
    pub state: Option<LinearState>,
}

impl LinearIssuePayload {
    pub fn is_completed(&self) -> bool {
        self.state
            .as_ref()
            .map(|s| {
                s.r#type == "completed" || s.r#type == "canceled"
            })
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LinearState {
    pub name: String,
    pub r#type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LinearCycle {
    pub id: String,
    pub number: i32,
    pub name: Option<String>,
    #[serde(rename = "startsAt")]
    pub starts_at: Option<String>,
    #[serde(rename = "endsAt")]
    pub ends_at: Option<String>,
    #[serde(rename = "completedAt")]
    pub completed_at: Option<String>,
}
