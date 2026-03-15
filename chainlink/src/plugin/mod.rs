use anyhow::Result;
use serde::{Deserialize, Serialize};

#[allow(unused_imports)]
use crate::db::Database;
use crate::models::{Comment, Issue, Milestone, Session};

pub mod config;
pub mod sync;

#[cfg(feature = "github")]
pub mod github;
#[cfg(feature = "jira")]
pub mod jira;
#[cfg(feature = "linear")]
pub mod linear;

/// Events emitted by chainlink mutations. Plugins subscribe to these.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChainlinkEvent {
    IssueCreated {
        issue: Issue,
    },
    IssueUpdated {
        issue: Issue,
        changed_fields: Vec<String>,
    },
    IssueClosed {
        issue: Issue,
    },
    IssueReopened {
        issue: Issue,
    },
    CommentAdded {
        issue_id: i64,
        comment: Comment,
    },
    LabelAdded {
        issue_id: i64,
        label: String,
    },
    LabelRemoved {
        issue_id: i64,
        label: String,
    },
    MilestoneCreated {
        milestone: Milestone,
    },
    MilestoneClosed {
        milestone: Milestone,
    },
    SessionStarted {
        session: Session,
    },
    SessionEnded {
        session: Session,
    },
}

/// Result of a sync operation (pull or push).
#[derive(Debug, Default)]
pub struct SyncReport {
    pub pulled: usize,
    pub pushed: usize,
    pub conflicts: Vec<SyncConflict>,
    pub errors: Vec<String>,
}

/// A field-level conflict between local and remote state.
#[derive(Debug)]
#[allow(dead_code)]
pub struct SyncConflict {
    pub local_issue_id: i64,
    pub remote_id: String,
    pub field: String,
    pub local_value: String,
    pub remote_value: String,
}

/// Trait implemented by every plugin. Compiled plugins implement this directly.
pub trait Plugin: Send + Sync {
    /// Unique name used in config and DB (e.g. "jira", "github", "linear").
    fn name(&self) -> &str;

    /// Called after a local mutation. Push the change to the remote if configured.
    fn on_event(&self, event: &ChainlinkEvent, db: &Database) -> Result<()>;

    /// Pull remote changes into the local database.
    fn pull_sync(&self, db: &Database) -> Result<SyncReport>;

    /// Push local changes to the remote.
    fn push_sync(&self, db: &Database) -> Result<SyncReport>;

    /// Validate that the plugin's configuration and credentials are correct.
    fn validate_config(&self) -> Result<()>;
}

/// Manages all enabled plugins. Dispatches events and orchestrates sync.
#[derive(Default)]
pub struct PluginManager {
    plugins: Vec<Box<dyn Plugin>>,
}

impl PluginManager {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub fn register(&mut self, plugin: Box<dyn Plugin>) {
        self.plugins.push(plugin);
    }

    pub fn plugins(&self) -> &[Box<dyn Plugin>] {
        &self.plugins
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Dispatch an event to all registered plugins.
    /// Errors from individual plugins are collected, not fatal.
    pub fn emit(&self, event: &ChainlinkEvent, db: &Database) -> Vec<String> {
        let mut errors = Vec::new();
        for plugin in &self.plugins {
            if let Err(e) = plugin.on_event(event, db) {
                errors.push(format!("[{}] {}", plugin.name(), e));
            }
        }
        errors
    }

    /// Pull from all plugins.
    pub fn pull_all(&self, db: &Database) -> Vec<(String, Result<SyncReport>)> {
        self.plugins
            .iter()
            .map(|p| (p.name().to_string(), p.pull_sync(db)))
            .collect()
    }

    /// Push to all plugins.
    pub fn push_all(&self, db: &Database) -> Vec<(String, Result<SyncReport>)> {
        self.plugins
            .iter()
            .map(|p| (p.name().to_string(), p.push_sync(db)))
            .collect()
    }

    /// Build a PluginManager from configuration, registering all enabled plugins.
    #[allow(unused_mut, unused_variables)]
    pub fn from_config(plugin_config: &config::PluginConfig) -> Result<Self> {
        let mut manager = Self::new();

        #[cfg(feature = "jira")]
        if let Some(ref jira_cfg) = plugin_config.jira {
            if jira_cfg.enabled {
                manager.register(Box::new(jira::JiraPlugin::new(jira_cfg.clone())?));
            }
        }

        #[cfg(feature = "github")]
        if let Some(ref gh_cfg) = plugin_config.github {
            if gh_cfg.enabled {
                manager.register(Box::new(github::GithubPlugin::new(gh_cfg.clone())?));
            }
        }

        #[cfg(feature = "linear")]
        if let Some(ref lin_cfg) = plugin_config.linear {
            if lin_cfg.enabled {
                manager.register(Box::new(linear::LinearPlugin::new(lin_cfg.clone())?));
            }
        }

        Ok(manager)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockPlugin {
        name: String,
        fail_on_event: bool,
    }

    impl Plugin for MockPlugin {
        fn name(&self) -> &str {
            &self.name
        }

        fn on_event(&self, _event: &ChainlinkEvent, _db: &Database) -> Result<()> {
            if self.fail_on_event {
                anyhow::bail!("mock error");
            }
            Ok(())
        }

        fn pull_sync(&self, _db: &Database) -> Result<SyncReport> {
            Ok(SyncReport::default())
        }

        fn push_sync(&self, _db: &Database) -> Result<SyncReport> {
            Ok(SyncReport::default())
        }

        fn validate_config(&self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_plugin_manager_new_is_empty() {
        let pm = PluginManager::new();
        assert!(pm.is_empty());
        assert_eq!(pm.plugins().len(), 0);
    }

    #[test]
    fn test_plugin_manager_register() {
        let mut pm = PluginManager::new();
        pm.register(Box::new(MockPlugin {
            name: "test".to_string(),
            fail_on_event: false,
        }));
        assert!(!pm.is_empty());
        assert_eq!(pm.plugins().len(), 1);
        assert_eq!(pm.plugins()[0].name(), "test");
    }

    #[test]
    fn test_emit_collects_errors() {
        let mut pm = PluginManager::new();
        pm.register(Box::new(MockPlugin {
            name: "ok_plugin".to_string(),
            fail_on_event: false,
        }));
        pm.register(Box::new(MockPlugin {
            name: "bad_plugin".to_string(),
            fail_on_event: true,
        }));

        let event = ChainlinkEvent::IssueCreated {
            issue: Issue {
                id: 1,
                title: "test".to_string(),
                description: None,
                status: "open".to_string(),
                priority: "medium".to_string(),
                parent_id: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                closed_at: None,
            },
        };

        let db_dir = tempfile::tempdir().unwrap();
        let db = Database::open(&db_dir.path().join("test.db")).unwrap();

        let errors = pm.emit(&event, &db);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("bad_plugin"));
        assert!(errors[0].contains("mock error"));
    }

    #[test]
    fn test_event_serialization_roundtrip() {
        let event = ChainlinkEvent::IssueCreated {
            issue: Issue {
                id: 1,
                title: "test issue".to_string(),
                description: Some("desc".to_string()),
                status: "open".to_string(),
                priority: "high".to_string(),
                parent_id: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                closed_at: None,
            },
        };

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: ChainlinkEvent = serde_json::from_str(&json).unwrap();

        match deserialized {
            ChainlinkEvent::IssueCreated { issue } => {
                assert_eq!(issue.id, 1);
                assert_eq!(issue.title, "test issue");
            }
            _ => panic!("wrong variant"),
        }
    }
}
