use anyhow::{bail, Result};
use std::path::Path;

use crate::db::Database;
use crate::plugin::config::PluginConfig;
use crate::plugin::sync::print_sync_summary;
use crate::plugin::PluginManager;

/// List all configured plugins and their status.
pub fn list(chainlink_dir: &Path) -> Result<()> {
    let config_path = chainlink_dir.join("plugins.toml");
    if !config_path.exists() {
        println!("No plugins configured. Run 'chainlink plugin configure <name>' to set up a plugin.");
        println!("Available plugins: jira, github, linear");
        return Ok(());
    }

    let config = PluginConfig::load(&config_path)?;
    let mut found = false;

    if let Some(ref jira) = config.jira {
        found = true;
        let status = if jira.enabled { "enabled" } else { "disabled" };
        println!(
            "  jira     [{}]  {} project={}",
            status, jira.instance, jira.project
        );
    }

    if let Some(ref gh) = config.github {
        found = true;
        let status = if gh.enabled { "enabled" } else { "disabled" };
        println!(
            "  github   [{}]  {}/{}",
            status, gh.owner, gh.repo
        );
    }

    if let Some(ref linear) = config.linear {
        found = true;
        let status = if linear.enabled { "enabled" } else { "disabled" };
        println!("  linear   [{}]  team={}", status, linear.team);
    }

    if !found {
        println!("No plugins configured in plugins.toml");
    }

    Ok(())
}

/// Interactive configure for a specific plugin.
pub fn configure(chainlink_dir: &Path, name: &str) -> Result<()> {
    let config_path = chainlink_dir.join("plugins.toml");
    let mut config = if config_path.exists() {
        PluginConfig::load(&config_path)?
    } else {
        PluginConfig::default()
    };

    match name {
        "jira" => {
            println!("Jira Cloud Configuration");
            println!("========================");
            println!("Set these environment variables:");
            println!("  CHAINLINK_JIRA_TOKEN  - Your Jira API token");
            println!("  CHAINLINK_JIRA_EMAIL  - Your Jira email (or set 'email' in config)");
            println!();
            println!("Edit {} to configure:", config_path.display());
            println!();

            if config.jira.is_none() {
                config.jira = Some(crate::plugin::config::JiraConfig {
                    enabled: true,
                    instance: "https://YOUR_INSTANCE.atlassian.net".to_string(),
                    project: "PROJ".to_string(),
                    email: None,
                    default_issue_type: "Story".to_string(),
                    field_map: Default::default(),
                    sync: Default::default(),
                });
            }

            config.save(&config_path)?;
            println!("Template written to {}", config_path.display());
            println!("Edit the file and update 'instance' and 'project', then run:");
            println!("  chainlink plugin validate jira");
        }
        "github" => {
            println!("GitHub Issues Configuration");
            println!("===========================");
            println!("Set this environment variable:");
            println!("  CHAINLINK_GITHUB_TOKEN  - Your GitHub personal access token");
            println!();

            if config.github.is_none() {
                config.github = Some(crate::plugin::config::GithubConfig {
                    enabled: true,
                    owner: "YOUR_ORG".to_string(),
                    repo: "YOUR_REPO".to_string(),
                    field_map: Default::default(),
                    sync: Default::default(),
                });
            }

            config.save(&config_path)?;
            println!("Template written to {}", config_path.display());
            println!("Edit the file and update 'owner' and 'repo', then run:");
            println!("  chainlink plugin validate github");
        }
        "linear" => {
            println!("Linear Configuration");
            println!("====================");
            println!("Set this environment variable:");
            println!("  CHAINLINK_LINEAR_TOKEN  - Your Linear API key");
            println!();

            if config.linear.is_none() {
                config.linear = Some(crate::plugin::config::LinearConfig {
                    enabled: true,
                    team: "ENG".to_string(),
                    field_map: Default::default(),
                    sync: Default::default(),
                });
            }

            config.save(&config_path)?;
            println!("Template written to {}", config_path.display());
            println!("Edit the file and update 'team', then run:");
            println!("  chainlink plugin validate linear");
        }
        _ => bail!(
            "Unknown plugin '{}'. Available: jira, github, linear",
            name
        ),
    }

    Ok(())
}

/// Validate plugin configuration and credentials.
pub fn validate(chainlink_dir: &Path, name: Option<&str>, db: &Database) -> Result<()> {
    let config_path = chainlink_dir.join("plugins.toml");
    if !config_path.exists() {
        bail!("No plugins.toml found. Run 'chainlink plugin configure <name>' first.");
    }

    let config = PluginConfig::load(&config_path)?;
    let manager = PluginManager::from_config(&config)?;

    if manager.is_empty() {
        println!("No plugins enabled.");
        return Ok(());
    }

    for plugin in manager.plugins() {
        if name.is_some_and(|n| n != plugin.name()) {
            continue;
        }
        match plugin.validate_config() {
            Ok(()) => {}
            Err(e) => eprintln!("[{}] Validation failed: {}", plugin.name(), e),
        }
    }

    // Suppress unused variable warning
    let _ = db;

    Ok(())
}

/// Run a full bidirectional sync.
pub fn sync(chainlink_dir: &Path, db: &Database, plugin_filter: Option<&str>) -> Result<()> {
    let config_path = chainlink_dir.join("plugins.toml");
    if !config_path.exists() {
        bail!("No plugins.toml found. Run 'chainlink plugin configure <name>' first.");
    }

    let config = PluginConfig::load(&config_path)?;
    let manager = PluginManager::from_config(&config)?;

    if manager.is_empty() {
        println!("No plugins enabled.");
        return Ok(());
    }

    println!("Syncing...");

    // Pull first, then push
    for (name, result) in manager.pull_all(db) {
        if plugin_filter.is_some_and(|f| f != name) {
            continue;
        }
        match result {
            Ok(report) => {
                print_sync_summary(&name, report.pulled, 0, &report.errors);
                for conflict in &report.conflicts {
                    eprintln!(
                        "[{}] Unresolved conflict: issue #{} field={} local='{}' remote='{}'",
                        name, conflict.local_issue_id, conflict.field,
                        conflict.local_value, conflict.remote_value
                    );
                }
            }
            Err(e) => eprintln!("[{}] Pull failed: {}", name, e),
        }
    }

    for (name, result) in manager.push_all(db) {
        if plugin_filter.is_some_and(|f| f != name) {
            continue;
        }
        match result {
            Ok(report) => {
                print_sync_summary(&name, 0, report.pushed, &report.errors);
            }
            Err(e) => eprintln!("[{}] Push failed: {}", name, e),
        }
    }

    println!("Sync complete.");
    Ok(())
}

/// Show sync status for all plugins.
pub fn status(chainlink_dir: &Path, db: &Database) -> Result<()> {
    let config_path = chainlink_dir.join("plugins.toml");
    if !config_path.exists() {
        println!("No plugins configured.");
        return Ok(());
    }

    let config = PluginConfig::load(&config_path)?;
    let mut has_syncs = false;

    let plugin_names: Vec<&str> = {
        let mut names = Vec::new();
        if config.jira.as_ref().is_some_and(|c| c.enabled) {
            names.push("jira");
        }
        if config.github.as_ref().is_some_and(|c| c.enabled) {
            names.push("github");
        }
        if config.linear.as_ref().is_some_and(|c| c.enabled) {
            names.push("linear");
        }
        names
    };

    for name in &plugin_names {
        let syncs = db.list_plugin_syncs(name)?;
        if !syncs.is_empty() {
            has_syncs = true;
            println!("[{}] {} synced issues:", name, syncs.len());
            for sync in &syncs {
                let issue = db.get_issue(sync.local_issue_id)?;
                let title = issue.map(|i| i.title).unwrap_or_else(|| "(deleted)".to_string());
                let url = sync.remote_url.as_deref().unwrap_or(&sync.remote_id);
                println!(
                    "  #{:<4} -> {}  (synced: {})",
                    sync.local_issue_id,
                    url,
                    sync.last_synced_at.format("%Y-%m-%d %H:%M")
                );
                let _ = title; // used above inline
            }
        }
    }

    if !has_syncs {
        println!("No synced issues. Run 'chainlink plugin sync' to sync.");
    }

    Ok(())
}

/// Manually link a local issue to a remote issue.
pub fn link(db: &Database, chainlink_dir: &Path, local_id: i64, remote_id: &str, plugin_name: &str) -> Result<()> {
    let config_path = chainlink_dir.join("plugins.toml");
    if !config_path.exists() {
        bail!("No plugins.toml found.");
    }

    db.require_issue(local_id)?;

    let valid_plugins = ["jira", "github", "linear"];
    if !valid_plugins.contains(&plugin_name) {
        bail!(
            "Unknown plugin '{}'. Available: {}",
            plugin_name,
            valid_plugins.join(", ")
        );
    }

    db.upsert_plugin_sync(plugin_name, local_id, remote_id, None, None, "both")?;
    println!("Linked #{} -> {} ({})", local_id, remote_id, plugin_name);
    Ok(())
}

/// Remove a sync mapping.
pub fn unlink(db: &Database, local_id: i64, plugin_name: Option<&str>) -> Result<()> {
    let plugins = if let Some(name) = plugin_name {
        vec![name.to_string()]
    } else {
        vec!["jira".to_string(), "github".to_string(), "linear".to_string()]
    };

    let mut unlinked = false;
    for name in &plugins {
        if db.delete_plugin_sync(name, local_id)? {
            println!("Unlinked #{} from {}", local_id, name);
            unlinked = true;
        }
    }

    if !unlinked {
        println!("No sync mappings found for #{}", local_id);
    }

    Ok(())
}
