use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
#[allow(unused_imports)]
use std::io::{self, BufRead, Write};

use crate::db::Database;

#[allow(unused_imports)]
use super::SyncConflict;

/// A record mapping a local issue to its remote counterpart in a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSyncRecord {
    pub id: i64,
    pub plugin_name: String,
    pub local_issue_id: i64,
    pub remote_id: String,
    pub remote_url: Option<String>,
    pub remote_etag: Option<String>,
    pub last_synced_at: DateTime<Utc>,
    pub sync_direction: String,
}

/// A record mapping a local milestone to a remote version/cycle/milestone.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct PluginMilestoneSyncRecord {
    pub id: i64,
    pub plugin_name: String,
    pub local_milestone_id: i64,
    pub remote_id: String,
    pub last_synced_at: DateTime<Utc>,
}

/// Conflict resolution strategy.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum ConflictStrategy {
    Ask,
    RemoteWins,
    LocalWins,
}

#[allow(dead_code)]
impl ConflictStrategy {
    pub fn parse(s: &str) -> Self {
        match s {
            "remote-wins" => Self::RemoteWins,
            "local-wins" => Self::LocalWins,
            _ => Self::Ask,
        }
    }
}

/// Resolve a conflict by asking the user via stdin/stdout.
/// Returns true if the user chose the remote value, false if local.
#[allow(dead_code)]
pub fn resolve_conflict_interactive(conflict: &SyncConflict) -> Result<bool> {
    println!("\nConflict detected on issue #{} (remote: {})", conflict.local_issue_id, conflict.remote_id);
    println!("  Field: {}", conflict.field);
    println!("  Local:  {}", conflict.local_value);
    println!("  Remote: {}", conflict.remote_value);
    print!("  Keep [r]emote or [l]ocal? ");
    io::stdout().flush()?;

    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    let choice = line.trim().to_lowercase();

    Ok(choice.starts_with('r'))
}

/// Resolve a conflict based on strategy. Returns true if remote wins.
#[allow(dead_code)]
pub fn resolve_conflict(conflict: &SyncConflict, strategy: &ConflictStrategy) -> Result<bool> {
    match strategy {
        ConflictStrategy::RemoteWins => Ok(true),
        ConflictStrategy::LocalWins => Ok(false),
        ConflictStrategy::Ask => resolve_conflict_interactive(conflict),
    }
}

/// Print a summary of sync results.
pub fn print_sync_summary(plugin_name: &str, pulled: usize, pushed: usize, errors: &[String]) {
    if pulled > 0 || pushed > 0 {
        println!(
            "[{}] Synced: {} pulled, {} pushed",
            plugin_name, pulled, pushed
        );
    }
    for err in errors {
        eprintln!("[{}] Error: {}", plugin_name, err);
    }
}

#[allow(dead_code)]
impl Database {
    /// Insert or update a plugin sync record.
    pub fn upsert_plugin_sync(
        &self,
        plugin_name: &str,
        local_issue_id: i64,
        remote_id: &str,
        remote_url: Option<&str>,
        remote_etag: Option<&str>,
        sync_direction: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn().execute(
            "INSERT INTO plugin_sync (plugin_name, local_issue_id, remote_id, remote_url, remote_etag, last_synced_at, sync_direction)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(plugin_name, local_issue_id) DO UPDATE SET
                remote_id = ?3, remote_url = ?4, remote_etag = ?5, last_synced_at = ?6, sync_direction = ?7",
            rusqlite::params![plugin_name, local_issue_id, remote_id, remote_url, remote_etag, now, sync_direction],
        ).context("Failed to upsert plugin sync record")?;
        Ok(())
    }

    /// Get the sync record for a specific local issue and plugin.
    pub fn get_plugin_sync(
        &self,
        plugin_name: &str,
        local_issue_id: i64,
    ) -> Result<Option<PluginSyncRecord>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, plugin_name, local_issue_id, remote_id, remote_url, remote_etag, last_synced_at, sync_direction
             FROM plugin_sync WHERE plugin_name = ?1 AND local_issue_id = ?2",
        )?;

        let result = stmt.query_row(
            rusqlite::params![plugin_name, local_issue_id],
            |row| {
                let synced_str: String = row.get(6)?;
                Ok(PluginSyncRecord {
                    id: row.get(0)?,
                    plugin_name: row.get(1)?,
                    local_issue_id: row.get(2)?,
                    remote_id: row.get(3)?,
                    remote_url: row.get(4)?,
                    remote_etag: row.get(5)?,
                    last_synced_at: DateTime::parse_from_rfc3339(&synced_str)
                        .unwrap_or_else(|_| Utc::now().into())
                        .with_timezone(&Utc),
                    sync_direction: row.get(7)?,
                })
            },
        );

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get the sync record by plugin name and remote ID.
    pub fn get_plugin_sync_by_remote(
        &self,
        plugin_name: &str,
        remote_id: &str,
    ) -> Result<Option<PluginSyncRecord>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, plugin_name, local_issue_id, remote_id, remote_url, remote_etag, last_synced_at, sync_direction
             FROM plugin_sync WHERE plugin_name = ?1 AND remote_id = ?2",
        )?;

        let result = stmt.query_row(
            rusqlite::params![plugin_name, remote_id],
            |row| {
                let synced_str: String = row.get(6)?;
                Ok(PluginSyncRecord {
                    id: row.get(0)?,
                    plugin_name: row.get(1)?,
                    local_issue_id: row.get(2)?,
                    remote_id: row.get(3)?,
                    remote_url: row.get(4)?,
                    remote_etag: row.get(5)?,
                    last_synced_at: DateTime::parse_from_rfc3339(&synced_str)
                        .unwrap_or_else(|_| Utc::now().into())
                        .with_timezone(&Utc),
                    sync_direction: row.get(7)?,
                })
            },
        );

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// List all sync records for a plugin.
    pub fn list_plugin_syncs(&self, plugin_name: &str) -> Result<Vec<PluginSyncRecord>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, plugin_name, local_issue_id, remote_id, remote_url, remote_etag, last_synced_at, sync_direction
             FROM plugin_sync WHERE plugin_name = ?1",
        )?;

        let rows = stmt.query_map(rusqlite::params![plugin_name], |row| {
            let synced_str: String = row.get(6)?;
            Ok(PluginSyncRecord {
                id: row.get(0)?,
                plugin_name: row.get(1)?,
                local_issue_id: row.get(2)?,
                remote_id: row.get(3)?,
                remote_url: row.get(4)?,
                remote_etag: row.get(5)?,
                last_synced_at: DateTime::parse_from_rfc3339(&synced_str)
                    .unwrap_or_else(|_| Utc::now().into())
                    .with_timezone(&Utc),
                sync_direction: row.get(7)?,
            })
        })?;

        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    /// Delete a sync record for a local issue.
    pub fn delete_plugin_sync(&self, plugin_name: &str, local_issue_id: i64) -> Result<bool> {
        let rows = self.conn().execute(
            "DELETE FROM plugin_sync WHERE plugin_name = ?1 AND local_issue_id = ?2",
            rusqlite::params![plugin_name, local_issue_id],
        )?;
        Ok(rows > 0)
    }

    /// Insert or update a milestone sync record.
    pub fn upsert_milestone_sync(
        &self,
        plugin_name: &str,
        local_milestone_id: i64,
        remote_id: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn().execute(
            "INSERT INTO plugin_milestone_sync (plugin_name, local_milestone_id, remote_id, last_synced_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(plugin_name, local_milestone_id) DO UPDATE SET
                remote_id = ?3, last_synced_at = ?4",
            rusqlite::params![plugin_name, local_milestone_id, remote_id, now],
        ).context("Failed to upsert milestone sync record")?;
        Ok(())
    }

    /// Get the milestone sync record.
    pub fn get_milestone_sync(
        &self,
        plugin_name: &str,
        local_milestone_id: i64,
    ) -> Result<Option<PluginMilestoneSyncRecord>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, plugin_name, local_milestone_id, remote_id, last_synced_at
             FROM plugin_milestone_sync WHERE plugin_name = ?1 AND local_milestone_id = ?2",
        )?;

        let result = stmt.query_row(
            rusqlite::params![plugin_name, local_milestone_id],
            |row| {
                let synced_str: String = row.get(4)?;
                Ok(PluginMilestoneSyncRecord {
                    id: row.get(0)?,
                    plugin_name: row.get(1)?,
                    local_milestone_id: row.get(2)?,
                    remote_id: row.get(3)?,
                    last_synced_at: DateTime::parse_from_rfc3339(&synced_str)
                        .unwrap_or_else(|_| Utc::now().into())
                        .with_timezone(&Utc),
                })
            },
        );

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get the milestone sync record by remote ID.
    pub fn get_milestone_sync_by_remote(
        &self,
        plugin_name: &str,
        remote_id: &str,
    ) -> Result<Option<PluginMilestoneSyncRecord>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, plugin_name, local_milestone_id, remote_id, last_synced_at
             FROM plugin_milestone_sync WHERE plugin_name = ?1 AND remote_id = ?2",
        )?;

        let result = stmt.query_row(
            rusqlite::params![plugin_name, remote_id],
            |row| {
                let synced_str: String = row.get(4)?;
                Ok(PluginMilestoneSyncRecord {
                    id: row.get(0)?,
                    plugin_name: row.get(1)?,
                    local_milestone_id: row.get(2)?,
                    remote_id: row.get(3)?,
                    last_synced_at: DateTime::parse_from_rfc3339(&synced_str)
                        .unwrap_or_else(|_| Utc::now().into())
                        .with_timezone(&Utc),
                })
            },
        );

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_test_db() -> (Database, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        (db, dir)
    }

    #[test]
    fn test_upsert_and_get_plugin_sync() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test", None, "medium").unwrap();

        db.upsert_plugin_sync("jira", issue_id, "PROJ-123", Some("https://jira.example.com/PROJ-123"), Some("etag1"), "both")
            .unwrap();

        let record = db.get_plugin_sync("jira", issue_id).unwrap().unwrap();
        assert_eq!(record.plugin_name, "jira");
        assert_eq!(record.local_issue_id, issue_id);
        assert_eq!(record.remote_id, "PROJ-123");
        assert_eq!(record.remote_url.as_deref(), Some("https://jira.example.com/PROJ-123"));
        assert_eq!(record.remote_etag.as_deref(), Some("etag1"));
        assert_eq!(record.sync_direction, "both");
    }

    #[test]
    fn test_upsert_updates_existing() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test", None, "medium").unwrap();

        db.upsert_plugin_sync("jira", issue_id, "PROJ-1", None, None, "both").unwrap();
        db.upsert_plugin_sync("jira", issue_id, "PROJ-2", Some("url2"), Some("etag2"), "push").unwrap();

        let record = db.get_plugin_sync("jira", issue_id).unwrap().unwrap();
        assert_eq!(record.remote_id, "PROJ-2");
        assert_eq!(record.remote_url.as_deref(), Some("url2"));
        assert_eq!(record.sync_direction, "push");
    }

    #[test]
    fn test_get_nonexistent_sync() {
        let (db, _dir) = setup_test_db();
        let result = db.get_plugin_sync("jira", 999).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_by_remote_id() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test", None, "medium").unwrap();
        db.upsert_plugin_sync("github", issue_id, "42", None, None, "both").unwrap();

        let record = db.get_plugin_sync_by_remote("github", "42").unwrap().unwrap();
        assert_eq!(record.local_issue_id, issue_id);

        let none = db.get_plugin_sync_by_remote("github", "999").unwrap();
        assert!(none.is_none());
    }

    #[test]
    fn test_list_plugin_syncs() {
        let (db, _dir) = setup_test_db();
        let id1 = db.create_issue("Test 1", None, "medium").unwrap();
        let id2 = db.create_issue("Test 2", None, "medium").unwrap();

        db.upsert_plugin_sync("jira", id1, "PROJ-1", None, None, "both").unwrap();
        db.upsert_plugin_sync("jira", id2, "PROJ-2", None, None, "both").unwrap();
        db.upsert_plugin_sync("github", id1, "1", None, None, "both").unwrap();

        let jira_syncs = db.list_plugin_syncs("jira").unwrap();
        assert_eq!(jira_syncs.len(), 2);

        let gh_syncs = db.list_plugin_syncs("github").unwrap();
        assert_eq!(gh_syncs.len(), 1);
    }

    #[test]
    fn test_delete_plugin_sync() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test", None, "medium").unwrap();
        db.upsert_plugin_sync("jira", issue_id, "PROJ-1", None, None, "both").unwrap();

        assert!(db.delete_plugin_sync("jira", issue_id).unwrap());
        assert!(db.get_plugin_sync("jira", issue_id).unwrap().is_none());

        // Delete non-existent returns false
        assert!(!db.delete_plugin_sync("jira", issue_id).unwrap());
    }

    #[test]
    fn test_milestone_sync_roundtrip() {
        let (db, _dir) = setup_test_db();
        let ms_id = db.create_milestone("v1.0", None).unwrap();

        db.upsert_milestone_sync("jira", ms_id, "10001").unwrap();

        let record = db.get_milestone_sync("jira", ms_id).unwrap().unwrap();
        assert_eq!(record.plugin_name, "jira");
        assert_eq!(record.local_milestone_id, ms_id);
        assert_eq!(record.remote_id, "10001");
    }

    #[test]
    fn test_milestone_sync_by_remote() {
        let (db, _dir) = setup_test_db();
        let ms_id = db.create_milestone("v2.0", None).unwrap();
        db.upsert_milestone_sync("github", ms_id, "5").unwrap();

        let record = db.get_milestone_sync_by_remote("github", "5").unwrap().unwrap();
        assert_eq!(record.local_milestone_id, ms_id);

        let none = db.get_milestone_sync_by_remote("github", "999").unwrap();
        assert!(none.is_none());
    }

    #[test]
    fn test_conflict_strategy_from_str() {
        assert_eq!(ConflictStrategy::parse("ask"), ConflictStrategy::Ask);
        assert_eq!(ConflictStrategy::parse("remote-wins"), ConflictStrategy::RemoteWins);
        assert_eq!(ConflictStrategy::parse("local-wins"), ConflictStrategy::LocalWins);
        assert_eq!(ConflictStrategy::parse("unknown"), ConflictStrategy::Ask);
    }

    #[test]
    fn test_cascade_delete_removes_sync() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test", None, "medium").unwrap();
        db.upsert_plugin_sync("jira", issue_id, "PROJ-1", None, None, "both").unwrap();

        // Deleting the issue should cascade-delete the sync record
        db.delete_issue(issue_id).unwrap();
        let record = db.get_plugin_sync("jira", issue_id).unwrap();
        assert!(record.is_none());
    }
}
