use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::db::Database;
use crate::models::Issue;
use crate::plugin::config::{resolve_env_optional, resolve_env_token, JiraConfig};
use crate::plugin::sync::ConflictStrategy;
use crate::plugin::{ChainlinkEvent, Plugin, SyncConflict, SyncReport};

/// Jira Cloud plugin — bidirectional sync with Jira REST API v3.
pub struct JiraPlugin {
    config: JiraConfig,
    client: Client,
    auth_header: String,
}

impl JiraPlugin {
    pub fn new(config: JiraConfig) -> Result<Self> {
        let token = resolve_env_token("CHAINLINK_JIRA_TOKEN")?;
        let email = config
            .email
            .clone()
            .or_else(|| resolve_env_optional("CHAINLINK_JIRA_EMAIL"))
            .context("Jira email required: set 'email' in config or CHAINLINK_JIRA_EMAIL env var")?;

        let credentials = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", email, token));
        let auth_header = format!("Basic {}", credentials);

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            config,
            client,
            auth_header,
        })
    }

    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&self.auth_header).unwrap(),
        );
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}/rest/api/3/{}", self.config.instance.trim_end_matches('/'), path)
    }

    fn map_priority_to_jira(&self, chainlink_priority: &str) -> Option<String> {
        self.config
            .field_map
            .priority
            .get(chainlink_priority)
            .cloned()
    }

    fn map_priority_from_jira(&self, jira_priority: &str) -> String {
        for (cl_priority, j_priority) in &self.config.field_map.priority {
            if j_priority.eq_ignore_ascii_case(jira_priority) {
                return cl_priority.clone();
            }
        }
        "medium".to_string()
    }

    fn map_issue_type(&self, labels: &[String]) -> String {
        for label in labels {
            if let Some(jira_type) = self.config.field_map.type_map.get(label) {
                return jira_type.clone();
            }
        }
        self.config.default_issue_type.clone()
    }

    fn create_remote_issue(&self, issue: &Issue, db: &Database) -> Result<JiraIssueResponse> {
        let labels = db.get_labels(issue.id)?;
        let issue_type = self.map_issue_type(&labels);

        let mut fields: HashMap<String, serde_json::Value> = HashMap::new();
        fields.insert(
            "project".to_string(),
            serde_json::json!({ "key": self.config.project }),
        );
        fields.insert("summary".to_string(), serde_json::json!(issue.title));
        fields.insert(
            "issuetype".to_string(),
            serde_json::json!({ "name": issue_type }),
        );

        if let Some(ref desc) = issue.description {
            fields.insert(
                "description".to_string(),
                serde_json::json!({
                    "type": "doc",
                    "version": 1,
                    "content": [{
                        "type": "paragraph",
                        "content": [{
                            "type": "text",
                            "text": desc
                        }]
                    }]
                }),
            );
        }

        if let Some(jira_priority) = self.map_priority_to_jira(&issue.priority) {
            fields.insert(
                "priority".to_string(),
                serde_json::json!({ "name": jira_priority }),
            );
        }

        let body = serde_json::json!({ "fields": fields });

        let resp = self
            .client
            .post(&self.api_url("issue"))
            .headers(self.headers())
            .json(&body)
            .send()
            .context("Failed to create Jira issue")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            bail!("Jira API error ({}): {}", status, text);
        }

        resp.json::<JiraIssueResponse>()
            .context("Failed to parse Jira response")
    }

    fn update_remote_issue(
        &self,
        remote_key: &str,
        issue: &Issue,
        changed_fields: &[String],
    ) -> Result<()> {
        let mut fields: HashMap<String, serde_json::Value> = HashMap::new();

        for field in changed_fields {
            match field.as_str() {
                "title" => {
                    fields.insert("summary".to_string(), serde_json::json!(issue.title));
                }
                "description" => {
                    if let Some(ref desc) = issue.description {
                        fields.insert(
                            "description".to_string(),
                            serde_json::json!({
                                "type": "doc",
                                "version": 1,
                                "content": [{
                                    "type": "paragraph",
                                    "content": [{
                                        "type": "text",
                                        "text": desc
                                    }]
                                }]
                            }),
                        );
                    }
                }
                "priority" => {
                    if let Some(jira_priority) = self.map_priority_to_jira(&issue.priority) {
                        fields.insert(
                            "priority".to_string(),
                            serde_json::json!({ "name": jira_priority }),
                        );
                    }
                }
                _ => {}
            }
        }

        if fields.is_empty() {
            return Ok(());
        }

        let body = serde_json::json!({ "fields": fields });
        let resp = self
            .client
            .put(&self.api_url(&format!("issue/{}", remote_key)))
            .headers(self.headers())
            .json(&body)
            .send()
            .context("Failed to update Jira issue")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            bail!("Jira API error updating {} ({}): {}", remote_key, status, text);
        }

        Ok(())
    }

    fn transition_issue(&self, remote_key: &str, target_status: &str) -> Result<()> {
        // First, get available transitions
        let resp = self
            .client
            .get(&self.api_url(&format!("issue/{}/transitions", remote_key)))
            .headers(self.headers())
            .send()
            .context("Failed to get Jira transitions")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            bail!("Jira API error getting transitions ({}): {}", status, text);
        }

        let transitions: JiraTransitionsResponse = resp
            .json()
            .context("Failed to parse transitions response")?;

        let target = target_status.to_lowercase();
        let transition = transitions.transitions.iter().find(|t| {
            t.name.to_lowercase().contains(&target)
                || t.to.as_ref().is_some_and(|to| to.name.to_lowercase().contains(&target))
        });

        if let Some(transition) = transition {
            let body = serde_json::json!({
                "transition": { "id": transition.id }
            });

            let resp = self
                .client
                .post(&self.api_url(&format!("issue/{}/transitions", remote_key)))
                .headers(self.headers())
                .json(&body)
                .send()
                .context("Failed to transition Jira issue")?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().unwrap_or_default();
                bail!("Jira transition error ({}): {}", status, text);
            }
        }

        Ok(())
    }

    fn search_project_issues(&self, updated_since: Option<&str>) -> Result<Vec<JiraSearchIssue>> {
        let mut jql = format!("project = {}", self.config.project);
        if let Some(since) = updated_since {
            jql.push_str(&format!(" AND updated >= \"{}\"", since));
        }

        let mut all_issues = Vec::new();
        let mut start_at = 0;
        let max_results = 50;

        loop {
            let resp = self
                .client
                .get(&self.api_url("search"))
                .headers(self.headers())
                .query(&[
                    ("jql", jql.as_str()),
                    ("startAt", &start_at.to_string()),
                    ("maxResults", &max_results.to_string()),
                    ("fields", "summary,description,status,priority,issuetype,fixVersions,updated"),
                ])
                .send()
                .context("Failed to search Jira issues")?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().unwrap_or_default();
                bail!("Jira search error ({}): {}", status, text);
            }

            let search_result: JiraSearchResponse =
                resp.json().context("Failed to parse Jira search")?;

            let count = search_result.issues.len();
            all_issues.extend(search_result.issues);

            if count < max_results || all_issues.len() >= search_result.total {
                break;
            }
            start_at += count;
        }

        Ok(all_issues)
    }

    fn fetch_versions(&self) -> Result<Vec<JiraVersion>> {
        let resp = self
            .client
            .get(&self.api_url(&format!("project/{}/versions", self.config.project)))
            .headers(self.headers())
            .send()
            .context("Failed to fetch Jira versions")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            bail!("Jira versions error ({}): {}", status, text);
        }

        resp.json().context("Failed to parse Jira versions")
    }
}

use base64::Engine;

impl Plugin for JiraPlugin {
    fn name(&self) -> &str {
        "jira"
    }

    fn on_event(&self, event: &ChainlinkEvent, db: &Database) -> Result<()> {
        if self.config.sync.on_mutate == "none" {
            return Ok(());
        }

        match event {
            ChainlinkEvent::IssueCreated { issue } => {
                let response = self.create_remote_issue(issue, db)?;
                let url = format!(
                    "{}/browse/{}",
                    self.config.instance.trim_end_matches('/'),
                    response.key
                );
                db.upsert_plugin_sync(
                    "jira",
                    issue.id,
                    &response.key,
                    Some(&url),
                    None,
                    "both",
                )?;
                println!("[jira] Created {}", response.key);
            }
            ChainlinkEvent::IssueUpdated {
                issue,
                changed_fields,
            } => {
                if let Some(sync) = db.get_plugin_sync("jira", issue.id)? {
                    self.update_remote_issue(&sync.remote_id, issue, changed_fields)?;
                    println!("[jira] Updated {}", sync.remote_id);
                }
            }
            ChainlinkEvent::IssueClosed { issue } => {
                if let Some(sync) = db.get_plugin_sync("jira", issue.id)? {
                    self.transition_issue(&sync.remote_id, "done")?;
                    println!("[jira] Closed {}", sync.remote_id);
                }
            }
            ChainlinkEvent::IssueReopened { issue } => {
                if let Some(sync) = db.get_plugin_sync("jira", issue.id)? {
                    self.transition_issue(&sync.remote_id, "to do")?;
                    println!("[jira] Reopened {}", sync.remote_id);
                }
            }
            ChainlinkEvent::CommentAdded { issue_id, comment } => {
                if let Some(sync) = db.get_plugin_sync("jira", *issue_id)? {
                    let body = serde_json::json!({
                        "body": {
                            "type": "doc",
                            "version": 1,
                            "content": [{
                                "type": "paragraph",
                                "content": [{
                                    "type": "text",
                                    "text": comment.content
                                }]
                            }]
                        }
                    });

                    let resp = self
                        .client
                        .post(&self.api_url(&format!("issue/{}/comment", sync.remote_id)))
                        .headers(self.headers())
                        .json(&body)
                        .send()
                        .context("Failed to add Jira comment")?;

                    if !resp.status().is_success() {
                        let status = resp.status();
                        let text = resp.text().unwrap_or_default();
                        bail!("Jira comment error ({}): {}", status, text);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn pull_sync(&self, db: &Database) -> Result<SyncReport> {
        let mut report = SyncReport::default();
        let strategy = ConflictStrategy::parse(&self.config.sync.conflict);

        // Pull versions as milestones
        match self.fetch_versions() {
            Ok(versions) => {
                for version in versions {
                    let existing =
                        db.get_milestone_sync_by_remote("jira", &version.id)?;
                    if existing.is_none() {
                        let ms_id = db.create_milestone(
                            &version.name,
                            version.description.as_deref(),
                        )?;
                        db.upsert_milestone_sync("jira", ms_id, &version.id)?;
                        report.pulled += 1;
                    }
                }
            }
            Err(e) => report.errors.push(format!("versions: {}", e)),
        }

        // Pull issues
        match self.search_project_issues(None) {
            Ok(remote_issues) => {
                for remote in remote_issues {
                    let existing =
                        db.get_plugin_sync_by_remote("jira", &remote.key)?;

                    if let Some(sync_record) = existing {
                        // Update existing local issue if remote changed
                        if let Some(local) = db.get_issue(sync_record.local_issue_id)? {
                            let remote_title = remote.fields.summary.clone();
                            let remote_priority =
                                self.map_priority_from_jira(&remote.fields.priority_name());

                            if local.title != remote_title {
                                let conflict = SyncConflict {
                                    local_issue_id: local.id,
                                    remote_id: remote.key.clone(),
                                    field: "title".to_string(),
                                    local_value: local.title.clone(),
                                    remote_value: remote_title.clone(),
                                };

                                if super::sync::resolve_conflict(&conflict, &strategy)? {
                                    db.update_issue(
                                        local.id,
                                        Some(&remote_title),
                                        None,
                                        None,
                                    )?;
                                    report.pulled += 1;
                                } else {
                                    report.conflicts.push(conflict);
                                }
                            }

                            if local.priority != remote_priority {
                                let conflict = SyncConflict {
                                    local_issue_id: local.id,
                                    remote_id: remote.key.clone(),
                                    field: "priority".to_string(),
                                    local_value: local.priority.clone(),
                                    remote_value: remote_priority.clone(),
                                };

                                if super::sync::resolve_conflict(&conflict, &strategy)? {
                                    db.update_issue(
                                        local.id,
                                        None,
                                        None,
                                        Some(&remote_priority),
                                    )?;
                                    report.pulled += 1;
                                } else {
                                    report.conflicts.push(conflict);
                                }
                            }

                            db.upsert_plugin_sync(
                                "jira",
                                local.id,
                                &remote.key,
                                Some(&format!(
                                    "{}/browse/{}",
                                    self.config.instance.trim_end_matches('/'),
                                    remote.key
                                )),
                                None,
                                "both",
                            )?;
                        }
                    } else {
                        // Create new local issue
                        let priority =
                            self.map_priority_from_jira(&remote.fields.priority_name());
                        let desc = remote.fields.description_text();
                        let id = db.create_issue(
                            &remote.fields.summary,
                            desc.as_deref(),
                            &priority,
                        )?;

                        // If issue is done in Jira, close locally
                        if remote.fields.is_done() {
                            db.close_issue(id)?;
                        }

                        let url = format!(
                            "{}/browse/{}",
                            self.config.instance.trim_end_matches('/'),
                            remote.key
                        );
                        db.upsert_plugin_sync("jira", id, &remote.key, Some(&url), None, "both")?;
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

        // Find local issues not yet synced
        let all_issues = db.list_issues(None, None, None)?;
        let synced = db.list_plugin_syncs("jira")?;
        let synced_ids: Vec<i64> = synced.iter().map(|s| s.local_issue_id).collect();

        for issue in &all_issues {
            if !synced_ids.contains(&issue.id) {
                match self.create_remote_issue(issue, db) {
                    Ok(response) => {
                        let url = format!(
                            "{}/browse/{}",
                            self.config.instance.trim_end_matches('/'),
                            response.key
                        );
                        db.upsert_plugin_sync(
                            "jira",
                            issue.id,
                            &response.key,
                            Some(&url),
                            None,
                            "both",
                        )?;
                        report.pushed += 1;
                    }
                    Err(e) => {
                        report.errors.push(format!("push #{}: {}", issue.id, e));
                    }
                }
            }
        }

        Ok(report)
    }

    fn validate_config(&self) -> Result<()> {
        let resp = self
            .client
            .get(&self.api_url(&format!("project/{}", self.config.project)))
            .headers(self.headers())
            .send()
            .context("Failed to validate Jira connection")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            bail!(
                "Jira validation failed ({}): {}. Check instance URL, project key, and credentials.",
                status,
                text
            );
        }

        println!("[jira] Connected to {} project {}", self.config.instance, self.config.project);
        Ok(())
    }
}

// ==================== Jira API Response Types ====================

#[derive(Debug, Deserialize)]
pub struct JiraIssueResponse {
    pub id: String,
    pub key: String,
    #[serde(rename = "self")]
    pub self_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct JiraSearchResponse {
    pub total: usize,
    pub issues: Vec<JiraSearchIssue>,
}

#[derive(Debug, Deserialize)]
pub struct JiraSearchIssue {
    pub id: String,
    pub key: String,
    pub fields: JiraIssueFields,
}

#[derive(Debug, Deserialize)]
pub struct JiraIssueFields {
    pub summary: String,
    pub description: Option<serde_json::Value>,
    pub status: Option<JiraStatus>,
    pub priority: Option<JiraPriority>,
    pub issuetype: Option<JiraIssueType>,
    #[serde(rename = "fixVersions")]
    pub fix_versions: Option<Vec<JiraVersion>>,
    pub updated: Option<String>,
}

impl JiraIssueFields {
    pub fn priority_name(&self) -> String {
        self.priority
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Medium".to_string())
    }

    pub fn is_done(&self) -> bool {
        self.status
            .as_ref()
            .map(|s| {
                let name = s.name.to_lowercase();
                name == "done" || name == "closed" || name == "resolved"
            })
            .unwrap_or(false)
    }

    pub fn description_text(&self) -> Option<String> {
        self.description.as_ref().and_then(|d| {
            // ADF document: extract text from paragraph content
            d.get("content")
                .and_then(|c| c.as_array())
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|block| {
                            block.get("content").and_then(|c| c.as_array()).map(|items| {
                                items
                                    .iter()
                                    .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                                    .collect::<Vec<_>>()
                                    .join("")
                            })
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct JiraStatus {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct JiraPriority {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct JiraIssueType {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JiraVersion {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub released: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct JiraTransitionsResponse {
    pub transitions: Vec<JiraTransition>,
}

#[derive(Debug, Deserialize)]
pub struct JiraTransition {
    pub id: String,
    pub name: String,
    pub to: Option<JiraStatus>,
}
