use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::plugin::config::{resolve_env_token, GithubConfig};
use crate::plugin::sync::ConflictStrategy;
use crate::plugin::{ChainlinkEvent, Plugin, SyncConflict, SyncReport};

/// GitHub Issues plugin — bidirectional sync with GitHub REST API.
pub struct GithubPlugin {
    config: GithubConfig,
    client: Client,
    token: String,
}

impl GithubPlugin {
    pub fn new(config: GithubConfig) -> Result<Self> {
        let token = resolve_env_token("CHAINLINK_GITHUB_TOKEN")?;

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
            HeaderValue::from_str(&format!("Bearer {}", self.token)).unwrap(),
        );
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static("chainlink-tracker"));
        headers
    }

    fn api_url(&self, path: &str) -> String {
        format!(
            "https://api.github.com/repos/{}/{}/{}",
            self.config.owner, self.config.repo, path
        )
    }

    fn map_priority_to_label(&self, chainlink_priority: &str) -> Option<String> {
        self.config
            .field_map
            .priority
            .get(chainlink_priority)
            .cloned()
    }

    fn map_priority_from_labels(&self, gh_labels: &[GhLabel]) -> String {
        for (cl_priority, gh_label) in &self.config.field_map.priority {
            if gh_labels
                .iter()
                .any(|l| l.name.eq_ignore_ascii_case(gh_label))
            {
                return cl_priority.clone();
            }
        }
        "medium".to_string()
    }

    fn create_remote_issue(
        &self,
        title: &str,
        body: Option<&str>,
        labels: &[String],
        priority: &str,
    ) -> Result<GhIssueResponse> {
        let mut gh_labels: Vec<String> = labels.to_vec();
        if let Some(priority_label) = self.map_priority_to_label(priority) {
            gh_labels.push(priority_label);
        }

        let payload = serde_json::json!({
            "title": title,
            "body": body.unwrap_or(""),
            "labels": gh_labels,
        });

        let resp = self
            .client
            .post(&self.api_url("issues"))
            .headers(self.headers())
            .json(&payload)
            .send()
            .context("Failed to create GitHub issue")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            bail!("GitHub API error ({}): {}", status, text);
        }

        resp.json::<GhIssueResponse>()
            .context("Failed to parse GitHub response")
    }

    fn update_remote_issue(&self, number: u64, title: &str, body: Option<&str>) -> Result<()> {
        let mut payload = serde_json::json!({ "title": title });
        if let Some(b) = body {
            payload["body"] = serde_json::json!(b);
        }

        let resp = self
            .client
            .patch(&self.api_url(&format!("issues/{}", number)))
            .headers(self.headers())
            .json(&payload)
            .send()
            .context("Failed to update GitHub issue")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            bail!("GitHub update error ({}): {}", status, text);
        }

        Ok(())
    }

    fn close_remote_issue(&self, number: u64) -> Result<()> {
        let payload = serde_json::json!({ "state": "closed" });
        let resp = self
            .client
            .patch(&self.api_url(&format!("issues/{}", number)))
            .headers(self.headers())
            .json(&payload)
            .send()
            .context("Failed to close GitHub issue")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            bail!("GitHub close error ({}): {}", status, text);
        }

        Ok(())
    }

    fn reopen_remote_issue(&self, number: u64) -> Result<()> {
        let payload = serde_json::json!({ "state": "open" });
        let resp = self
            .client
            .patch(&self.api_url(&format!("issues/{}", number)))
            .headers(self.headers())
            .json(&payload)
            .send()
            .context("Failed to reopen GitHub issue")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            bail!("GitHub reopen error ({}): {}", status, text);
        }

        Ok(())
    }

    fn add_comment(&self, number: u64, body: &str) -> Result<()> {
        let payload = serde_json::json!({ "body": body });
        let resp = self
            .client
            .post(&self.api_url(&format!("issues/{}/comments", number)))
            .headers(self.headers())
            .json(&payload)
            .send()
            .context("Failed to add GitHub comment")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            bail!("GitHub comment error ({}): {}", status, text);
        }

        Ok(())
    }

    fn list_remote_issues(&self, state: &str) -> Result<Vec<GhIssueResponse>> {
        let mut all = Vec::new();
        let mut page = 1;

        loop {
            let resp = self
                .client
                .get(&self.api_url("issues"))
                .headers(self.headers())
                .query(&[
                    ("state", state),
                    ("per_page", "100"),
                    ("page", &page.to_string()),
                ])
                .send()
                .context("Failed to list GitHub issues")?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().unwrap_or_default();
                bail!("GitHub list error ({}): {}", status, text);
            }

            let issues: Vec<GhIssueResponse> =
                resp.json().context("Failed to parse GitHub issues")?;

            if issues.is_empty() {
                break;
            }

            all.extend(issues);
            page += 1;

            // Safety limit
            if page > 20 {
                break;
            }
        }

        // Filter out pull requests (GitHub returns them in the issues endpoint)
        all.retain(|i| i.pull_request.is_none());

        Ok(all)
    }

    fn list_remote_milestones(&self) -> Result<Vec<GhMilestone>> {
        let resp = self
            .client
            .get(&self.api_url("milestones"))
            .headers(self.headers())
            .query(&[("state", "all"), ("per_page", "100")])
            .send()
            .context("Failed to list GitHub milestones")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            bail!("GitHub milestones error ({}): {}", status, text);
        }

        resp.json().context("Failed to parse GitHub milestones")
    }

    fn parse_issue_number(remote_id: &str) -> Option<u64> {
        remote_id.parse().ok()
    }
}

impl Plugin for GithubPlugin {
    fn name(&self) -> &str {
        "github"
    }

    fn on_event(&self, event: &ChainlinkEvent, db: &Database) -> Result<()> {
        if self.config.sync.on_mutate == "none" {
            return Ok(());
        }

        match event {
            ChainlinkEvent::IssueCreated { issue } => {
                let labels = db.get_labels(issue.id)?;
                let resp = self.create_remote_issue(
                    &issue.title,
                    issue.description.as_deref(),
                    &labels,
                    &issue.priority,
                )?;
                let url = resp.html_url.clone().unwrap_or_default();
                db.upsert_plugin_sync(
                    "github",
                    issue.id,
                    &resp.number.to_string(),
                    Some(&url),
                    None,
                    "both",
                )?;
                println!("[github] Created #{}", resp.number);
            }
            ChainlinkEvent::IssueUpdated { issue, .. } => {
                if let Some(sync) = db.get_plugin_sync("github", issue.id)? {
                    if let Some(number) = Self::parse_issue_number(&sync.remote_id) {
                        self.update_remote_issue(
                            number,
                            &issue.title,
                            issue.description.as_deref(),
                        )?;
                        println!("[github] Updated #{}", number);
                    }
                }
            }
            ChainlinkEvent::IssueClosed { issue } => {
                if let Some(sync) = db.get_plugin_sync("github", issue.id)? {
                    if let Some(number) = Self::parse_issue_number(&sync.remote_id) {
                        self.close_remote_issue(number)?;
                        println!("[github] Closed #{}", number);
                    }
                }
            }
            ChainlinkEvent::IssueReopened { issue } => {
                if let Some(sync) = db.get_plugin_sync("github", issue.id)? {
                    if let Some(number) = Self::parse_issue_number(&sync.remote_id) {
                        self.reopen_remote_issue(number)?;
                        println!("[github] Reopened #{}", number);
                    }
                }
            }
            ChainlinkEvent::CommentAdded { issue_id, comment } => {
                if let Some(sync) = db.get_plugin_sync("github", *issue_id)? {
                    if let Some(number) = Self::parse_issue_number(&sync.remote_id) {
                        self.add_comment(number, &comment.content)?;
                    }
                }
            }
            ChainlinkEvent::LabelAdded { issue_id, label } => {
                if let Some(sync) = db.get_plugin_sync("github", *issue_id)? {
                    if let Some(number) = Self::parse_issue_number(&sync.remote_id) {
                        let payload = serde_json::json!({ "labels": [label] });
                        let _ = self
                            .client
                            .post(&self.api_url(&format!("issues/{}/labels", number)))
                            .headers(self.headers())
                            .json(&payload)
                            .send();
                    }
                }
            }
            ChainlinkEvent::LabelRemoved { issue_id, label } => {
                if let Some(sync) = db.get_plugin_sync("github", *issue_id)? {
                    if let Some(number) = Self::parse_issue_number(&sync.remote_id) {
                        let _ = self
                            .client
                            .delete(&self.api_url(&format!("issues/{}/labels/{}", number, label)))
                            .headers(self.headers())
                            .send();
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

        // Pull milestones
        match self.list_remote_milestones() {
            Ok(milestones) => {
                for ms in milestones {
                    let remote_id = ms.number.to_string();
                    let existing =
                        db.get_milestone_sync_by_remote("github", &remote_id)?;
                    if existing.is_none() {
                        let ms_id = db.create_milestone(
                            &ms.title,
                            ms.description.as_deref(),
                        )?;
                        if ms.state == "closed" {
                            db.close_milestone(ms_id)?;
                        }
                        db.upsert_milestone_sync("github", ms_id, &remote_id)?;
                        report.pulled += 1;
                    }
                }
            }
            Err(e) => report.errors.push(format!("milestones: {}", e)),
        }

        // Pull issues
        match self.list_remote_issues("all") {
            Ok(remote_issues) => {
                for remote in remote_issues {
                    let remote_id = remote.number.to_string();
                    let existing =
                        db.get_plugin_sync_by_remote("github", &remote_id)?;

                    if let Some(sync_record) = existing {
                        if let Some(local) = db.get_issue(sync_record.local_issue_id)? {
                            if local.title != remote.title {
                                let conflict = SyncConflict {
                                    local_issue_id: local.id,
                                    remote_id: remote_id.clone(),
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
                                "github",
                                local.id,
                                &remote_id,
                                remote.html_url.as_deref(),
                                None,
                                "both",
                            )?;
                        }
                    } else {
                        let priority =
                            self.map_priority_from_labels(&remote.labels.clone().unwrap_or_default());
                        let id = db.create_issue(
                            &remote.title,
                            remote.body.as_deref(),
                            &priority,
                        )?;

                        if remote.state == "closed" {
                            db.close_issue(id)?;
                        }

                        // Sync labels
                        if let Some(ref labels) = remote.labels {
                            for label in labels {
                                db.add_label(id, &label.name)?;
                            }
                        }

                        db.upsert_plugin_sync(
                            "github",
                            id,
                            &remote_id,
                            remote.html_url.as_deref(),
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

        let all_issues = db.list_issues(None, None, None)?;
        let synced = db.list_plugin_syncs("github")?;
        let synced_ids: Vec<i64> = synced.iter().map(|s| s.local_issue_id).collect();

        for issue in &all_issues {
            if !synced_ids.contains(&issue.id) {
                let labels = db.get_labels(issue.id)?;
                match self.create_remote_issue(
                    &issue.title,
                    issue.description.as_deref(),
                    &labels,
                    &issue.priority,
                ) {
                    Ok(resp) => {
                        let url = resp.html_url.clone().unwrap_or_default();
                        db.upsert_plugin_sync(
                            "github",
                            issue.id,
                            &resp.number.to_string(),
                            Some(&url),
                            None,
                            "both",
                        )?;

                        if issue.status == "closed" {
                            self.close_remote_issue(resp.number)?;
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
        let resp = self
            .client
            .get(&format!(
                "https://api.github.com/repos/{}/{}",
                self.config.owner, self.config.repo
            ))
            .headers(self.headers())
            .send()
            .context("Failed to validate GitHub connection")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            bail!(
                "GitHub validation failed ({}): {}. Check owner, repo, and token.",
                status,
                text
            );
        }

        println!(
            "[github] Connected to {}/{}",
            self.config.owner, self.config.repo
        );
        Ok(())
    }
}

// ==================== GitHub API Response Types ====================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhIssueResponse {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub html_url: Option<String>,
    pub labels: Option<Vec<GhLabel>>,
    pub milestone: Option<GhMilestone>,
    pub pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhLabel {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhMilestone {
    pub number: u64,
    pub title: String,
    pub description: Option<String>,
    pub state: String,
}
