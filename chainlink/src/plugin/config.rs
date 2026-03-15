use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::path::Path;

/// Top-level plugin configuration loaded from `.chainlink/plugins.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginConfig {
    pub jira: Option<JiraConfig>,
    pub github: Option<GithubConfig>,
    pub linear: Option<LinearConfig>,
}

impl PluginConfig {
    /// Load plugin configuration from a TOML file.
    pub fn load(path: &Path) -> Result<Self> {
        let content =
            std::fs::read_to_string(path).context("Failed to read plugins.toml")?;
        let config: PluginConfig =
            toml::from_str(&content).context("Failed to parse plugins.toml")?;
        Ok(config)
    }

    /// Check if any plugin is enabled.
    pub fn has_enabled_plugins(&self) -> bool {
        self.jira.as_ref().is_some_and(|c| c.enabled)
            || self.github.as_ref().is_some_and(|c| c.enabled)
            || self.linear.as_ref().is_some_and(|c| c.enabled)
    }

    /// Write the config to a TOML file.
    pub fn save(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;
        std::fs::write(path, content).context("Failed to write plugins.toml")?;
        Ok(())
    }
}

/// Jira Cloud plugin configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraConfig {
    #[serde(default)]
    pub enabled: bool,
    pub instance: String,
    pub project: String,
    pub email: Option<String>,
    #[serde(default = "default_jira_issue_type")]
    pub default_issue_type: String,
    #[serde(default)]
    pub field_map: FieldMap,
    #[serde(default)]
    pub sync: SyncConfig,
}

fn default_jira_issue_type() -> String {
    "Story".to_string()
}

/// GitHub Issues plugin configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubConfig {
    #[serde(default)]
    pub enabled: bool,
    pub owner: String,
    pub repo: String,
    #[serde(default)]
    pub field_map: FieldMap,
    #[serde(default)]
    pub sync: SyncConfig,
}

/// Linear plugin configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinearConfig {
    #[serde(default)]
    pub enabled: bool,
    pub team: String,
    #[serde(default)]
    pub field_map: FieldMap,
    #[serde(default)]
    pub sync: SyncConfig,
}

/// Maps chainlink fields to remote field values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FieldMap {
    /// Maps chainlink priority names to remote priority names.
    /// e.g. { "critical": "Highest", "high": "High" }
    #[serde(default)]
    pub priority: HashMap<String, String>,

    /// Maps chainlink label names to remote issue type names.
    /// e.g. { "bug": "Bug", "feature": "Story" }
    #[serde(default)]
    pub type_map: HashMap<String, String>,

    /// Which remote field to use for milestones.
    /// Jira: "fixVersion", GitHub: "milestone", Linear: "cycle"
    #[serde(default)]
    pub milestone_field: Option<String>,
}

/// Controls when sync operations happen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    /// What to do on session start: "pull", "push", "both", "none"
    #[serde(default = "default_pull")]
    pub on_session_start: String,

    /// What to do on session end: "pull", "push", "both", "none"
    #[serde(default = "default_push")]
    pub on_session_end: String,

    /// What to do after each mutation: "push", "none"
    #[serde(default = "default_push")]
    pub on_mutate: String,

    /// Conflict resolution strategy: "ask", "remote-wins", "local-wins"
    #[serde(default = "default_ask")]
    pub conflict: String,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            on_session_start: "pull".to_string(),
            on_session_end: "push".to_string(),
            on_mutate: "push".to_string(),
            conflict: "ask".to_string(),
        }
    }
}

fn default_pull() -> String {
    "pull".to_string()
}
fn default_push() -> String {
    "push".to_string()
}
fn default_ask() -> String {
    "ask".to_string()
}

/// Resolve an auth token from environment variables.
#[allow(dead_code)]
pub fn resolve_env_token(var_name: &str) -> Result<String> {
    env::var(var_name).with_context(|| {
        format!(
            "Environment variable {} not set. Set it to authenticate with the remote service.",
            var_name
        )
    })
}

/// Resolve an optional env var (returns None if not set instead of erroring).
#[allow(dead_code)]
pub fn resolve_env_optional(var_name: &str) -> Option<String> {
    env::var(var_name).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_parse_minimal_config() {
        let toml_str = r#"
[jira]
enabled = true
instance = "https://example.atlassian.net"
project = "PROJ"
"#;
        let config: PluginConfig = toml::from_str(toml_str).unwrap();
        let jira = config.jira.unwrap();
        assert!(jira.enabled);
        assert_eq!(jira.instance, "https://example.atlassian.net");
        assert_eq!(jira.project, "PROJ");
        assert_eq!(jira.default_issue_type, "Story");
        assert_eq!(jira.sync.conflict, "ask");
    }

    #[test]
    fn test_parse_full_config() {
        let toml_str = r#"
[jira]
enabled = true
instance = "https://myco.atlassian.net"
project = "DEV"
email = "user@myco.com"
default_issue_type = "Task"

[jira.field_map]
milestone_field = "fixVersion"

[jira.field_map.priority]
critical = "Highest"
high = "High"
medium = "Medium"
low = "Low"

[jira.field_map.type_map]
bug = "Bug"
feature = "Story"

[jira.sync]
on_session_start = "pull"
on_session_end = "push"
on_mutate = "push"
conflict = "ask"

[github]
enabled = true
owner = "myorg"
repo = "myrepo"

[github.field_map.priority]
critical = "P0"
high = "P1"

[github.sync]
conflict = "remote-wins"

[linear]
enabled = false
team = "ENG"
"#;
        let config: PluginConfig = toml::from_str(toml_str).unwrap();

        let jira = config.jira.unwrap();
        assert!(jira.enabled);
        assert_eq!(jira.default_issue_type, "Task");
        assert_eq!(jira.field_map.priority.get("critical").unwrap(), "Highest");
        assert_eq!(jira.field_map.type_map.get("bug").unwrap(), "Bug");
        assert_eq!(
            jira.field_map.milestone_field.as_deref(),
            Some("fixVersion")
        );

        let gh = config.github.unwrap();
        assert!(gh.enabled);
        assert_eq!(gh.owner, "myorg");
        assert_eq!(gh.sync.conflict, "remote-wins");

        let linear = config.linear.unwrap();
        assert!(!linear.enabled);
        assert_eq!(linear.team, "ENG");
    }

    #[test]
    fn test_has_enabled_plugins() {
        let config = PluginConfig::default();
        assert!(!config.has_enabled_plugins());

        let config_with_jira: PluginConfig = toml::from_str(
            r#"
[jira]
enabled = true
instance = "https://x.atlassian.net"
project = "X"
"#,
        )
        .unwrap();
        assert!(config_with_jira.has_enabled_plugins());
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.toml");

        let config: PluginConfig = toml::from_str(
            r#"
[jira]
enabled = true
instance = "https://test.atlassian.net"
project = "TEST"
"#,
        )
        .unwrap();

        config.save(&path).unwrap();
        let loaded = PluginConfig::load(&path).unwrap();
        let jira = loaded.jira.unwrap();
        assert!(jira.enabled);
        assert_eq!(jira.project, "TEST");
    }

    #[test]
    fn test_empty_config() {
        let config: PluginConfig = toml::from_str("").unwrap();
        assert!(config.jira.is_none());
        assert!(config.github.is_none());
        assert!(config.linear.is_none());
        assert!(!config.has_enabled_plugins());
    }

    #[test]
    fn test_invalid_toml_errors() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plugins.toml");
        std::fs::write(&path, "this is not valid toml {{{{").unwrap();
        let result = PluginConfig::load(&path);
        assert!(result.is_err());
    }

    #[test]
    fn test_default_sync_config() {
        let sync = SyncConfig::default();
        assert_eq!(sync.on_session_start, "pull");
        assert_eq!(sync.on_session_end, "push");
        assert_eq!(sync.on_mutate, "push");
        assert_eq!(sync.conflict, "ask");
    }

    #[test]
    fn test_resolve_env_token_missing() {
        let result = resolve_env_token("CHAINLINK_TEST_NONEXISTENT_VAR_12345");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("CHAINLINK_TEST_NONEXISTENT_VAR_12345"));
    }
}
