# Chainlink Plugin System - Implementation Plan

## Overview

Add a compiled, feature-flag-based plugin system to chainlink that enables bidirectional sync with external issue trackers. First release includes Jira Cloud, GitHub Issues, and Linear plugins.

**Design decisions (from user):**
- Compiled feature-flag plugins only (no process-based protocol yet)
- Conflict resolution: always ask the user
- Auth: API tokens via environment variables
- Integrations: Jira Cloud + GitHub Issues + Linear

---

## Architecture

### Event-driven model

Every mutation command in chainlink emits a `ChainlinkEvent`. A `PluginManager` dispatches events to enabled plugins. Plugins can also be queried for pull-sync operations.

```
User → `chainlink close 5`
  → db updates locally (existing)
  → PluginManager::emit(IssueClosed { issue })
  → JiraPlugin::on_event() → PUT /rest/api/3/issue/.../transitions
```

### New source files

```
src/
  plugin/
    mod.rs          — Plugin trait, ChainlinkEvent enum, PluginManager
    config.rs       — plugins.toml parsing, PluginConfig types
    sync.rs         — Sync engine: pull/push/conflict resolution
    jira.rs         — Jira Cloud plugin (behind `jira` feature)
    github.rs       — GitHub Issues plugin (behind `github` feature)
    linear.rs       — Linear plugin (behind `linear` feature)
  commands/
    plugin.rs       — CLI: plugin list/configure/sync/status
```

### Database additions (schema v9)

```sql
CREATE TABLE IF NOT EXISTS plugin_sync (
    id INTEGER PRIMARY KEY,
    plugin_name TEXT NOT NULL,
    local_issue_id INTEGER NOT NULL,
    remote_id TEXT NOT NULL,
    remote_url TEXT,
    remote_etag TEXT,
    last_synced_at TEXT NOT NULL,
    sync_direction TEXT NOT NULL DEFAULT 'both',
    UNIQUE(plugin_name, local_issue_id),
    FOREIGN KEY (local_issue_id) REFERENCES issues(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS plugin_milestone_sync (
    id INTEGER PRIMARY KEY,
    plugin_name TEXT NOT NULL,
    local_milestone_id INTEGER NOT NULL,
    remote_id TEXT NOT NULL,
    last_synced_at TEXT NOT NULL,
    UNIQUE(plugin_name, local_milestone_id),
    FOREIGN KEY (local_milestone_id) REFERENCES milestones(id) ON DELETE CASCADE
);
```

---

## Implementation Steps

### Step 1: Core plugin trait and event system (#12)

**Files:** `src/plugin/mod.rs`, `src/lib.rs`

Define:
```rust
pub enum ChainlinkEvent {
    IssueCreated { issue: Issue },
    IssueUpdated { issue: Issue, changed_fields: Vec<String> },
    IssueClosed { issue: Issue },
    IssueReopened { issue: Issue },
    CommentAdded { issue_id: i64, comment: Comment },
    LabelAdded { issue_id: i64, label: String },
    LabelRemoved { issue_id: i64, label: String },
    MilestoneCreated { milestone: Milestone },
    MilestoneClosed { milestone: Milestone },
    SessionStarted { session: Session },
    SessionEnded { session: Session },
}

pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn on_event(&self, event: &ChainlinkEvent, db: &Database) -> Result<()>;
    fn pull_sync(&self, db: &Database) -> Result<SyncReport>;
    fn push_sync(&self, db: &Database) -> Result<SyncReport>;
    fn validate_config(&self) -> Result<()>;
}

pub struct SyncReport {
    pub pulled: usize,
    pub pushed: usize,
    pub conflicts: Vec<SyncConflict>,
    pub errors: Vec<String>,
}

pub struct SyncConflict {
    pub local_issue_id: i64,
    pub remote_id: String,
    pub field: String,
    pub local_value: String,
    pub remote_value: String,
}

pub struct PluginManager {
    plugins: Vec<Box<dyn Plugin>>,
}

impl PluginManager {
    pub fn new() -> Self;
    pub fn register(&mut self, plugin: Box<dyn Plugin>);
    pub fn emit(&self, event: &ChainlinkEvent, db: &Database) -> Result<()>;
    pub fn pull_all(&self, db: &Database) -> Result<Vec<SyncReport>>;
    pub fn push_all(&self, db: &Database) -> Result<Vec<SyncReport>>;
    pub fn from_config(config: &PluginConfig, db: &Database) -> Result<Self>;
}
```

### Step 2: Plugin configuration system (#14)

**Files:** `src/plugin/config.rs`

Parse `.chainlink/plugins.toml`:
```toml
[jira]
enabled = true
instance = "https://mycompany.atlassian.net"
project = "PROJ"
email = "user@company.com"  # or CHAINLINK_JIRA_EMAIL env var
default_issue_type = "Story"

[jira.field_map]
# chainlink priority → Jira priority name
priority.critical = "Highest"
priority.high = "High"
priority.medium = "Medium"
priority.low = "Low"
# chainlink label → Jira issue type override
type_map.bug = "Bug"
type_map.feature = "Story"
type_map.task = "Task"
# chainlink milestone → Jira field
milestone_field = "fixVersion"

[jira.sync]
on_session_start = "pull"
on_session_end = "push"
on_mutate = "push"
conflict = "ask"          # "ask", "remote-wins", "local-wins"

[github]
enabled = true
owner = "myorg"
repo = "myrepo"

[github.field_map]
priority.critical = "P0"
priority.high = "P1"
priority.medium = "P2"
priority.low = "P3"
milestone_field = "milestone"

[github.sync]
on_session_start = "pull"
on_session_end = "push"
on_mutate = "push"
conflict = "ask"

[linear]
enabled = true
team = "ENG"

[linear.field_map]
priority.critical = "Urgent"
priority.high = "High"
priority.medium = "Medium"
priority.low = "Low"
type_map.bug = "Bug"
type_map.feature = "Feature"

[linear.sync]
on_session_start = "pull"
on_session_end = "push"
on_mutate = "push"
conflict = "ask"
```

Add `toml` crate as dependency. Config types:
```rust
pub struct PluginConfig {
    pub jira: Option<JiraConfig>,
    pub github: Option<GithubConfig>,
    pub linear: Option<LinearConfig>,
}

pub struct JiraConfig {
    pub enabled: bool,
    pub instance: String,
    pub project: String,
    pub email: Option<String>,
    pub default_issue_type: String,
    pub field_map: FieldMap,
    pub sync: SyncConfig,
}
// ... similar for GitHub and Linear
```

Auth tokens resolved from env vars:
- `CHAINLINK_JIRA_TOKEN` + `CHAINLINK_JIRA_EMAIL`
- `CHAINLINK_GITHUB_TOKEN`
- `CHAINLINK_LINEAR_TOKEN`

### Step 3: Sync engine and database (#13)

**Files:** `src/plugin/sync.rs`, `src/db.rs`

- Add `plugin_sync` and `plugin_milestone_sync` tables in schema v9 migration
- Add db methods: `upsert_plugin_sync`, `get_plugin_sync`, `get_unsynced_issues`, `delete_plugin_sync`
- Sync engine handles:
  - **Pull**: fetch remote issues since last sync, match by remote_id, create/update local issues
  - **Push**: find local issues changed since last sync, match by plugin_sync mapping, create/update remote
  - **Conflict detection**: compare local updated_at vs remote_etag/updated_at
  - **Conflict resolution**: when `conflict = "ask"`, print both versions to stdout and prompt user via stdin

### Step 4: Jira Cloud plugin (#15)

**Files:** `src/plugin/jira.rs`

**Feature flag:** `jira` (adds `reqwest` with `blocking` feature, `base64`)

Jira REST API v3 operations:
- `GET /rest/api/3/search?jql=project=PROJ` — pull issues
- `POST /rest/api/3/issue` — create issue
- `PUT /rest/api/3/issue/{id}` — update issue
- `POST /rest/api/3/issue/{id}/transitions` — close/reopen
- `GET /rest/api/3/project/{key}/versions` — pull versions → milestones
- `POST /rest/api/3/version` — create version

Auth: Basic auth with email + API token (base64 encoded).

Field mapping applies the user's `field_map` config when translating between chainlink and Jira representations.

### Step 5: GitHub Issues plugin (#16)

**Files:** `src/plugin/github.rs`

**Feature flag:** `github` (shares `reqwest`)

GitHub REST API operations:
- `GET /repos/{owner}/{repo}/issues` — pull issues
- `POST /repos/{owner}/{repo}/issues` — create
- `PATCH /repos/{owner}/{repo}/issues/{number}` — update/close/reopen
- `GET /repos/{owner}/{repo}/milestones` — pull milestones
- `POST /repos/{owner}/{repo}/milestones` — create milestone

Auth: Bearer token via `CHAINLINK_GITHUB_TOKEN`.

Labels in GitHub map to chainlink labels. Priority is mapped to GitHub labels (e.g., "P0", "P1").

### Step 6: Linear plugin (#17)

**Files:** `src/plugin/linear.rs`

**Feature flag:** `linear` (shares `reqwest`, adds `serde_json` for GraphQL)

Linear GraphQL API:
- Query issues by team
- Create/update issues via mutations
- Map Linear cycles → chainlink milestones
- Map Linear labels → chainlink labels

Auth: Bearer token via `CHAINLINK_LINEAR_TOKEN`.

### Step 7: Plugin CLI commands (#18)

**Files:** `src/commands/plugin.rs`, `src/main.rs`

```
chainlink plugin list                    # show installed plugins + status
chainlink plugin configure <name>        # guided setup: prompts for instance/project/auth
chainlink plugin sync [--plugin <name>]  # manual full bidirectional sync
chainlink plugin status                  # show last sync times, pending changes, errors
chainlink plugin link <id> <remote_id>   # manually link local issue to remote
chainlink plugin unlink <id>             # remove sync mapping
```

`plugin configure` writes to `.chainlink/plugins.toml` and validates auth by making a test API call.

### Step 8: Wire events into existing commands (#19)

**Files:** Every command module that mutates state

Add a helper function:
```rust
fn maybe_emit(db: &Database, event: ChainlinkEvent) -> Result<()> {
    let chainlink_dir = find_chainlink_dir()?;
    let config_path = chainlink_dir.join("plugins.toml");
    if config_path.exists() {
        let config = PluginConfig::load(&config_path)?;
        let manager = PluginManager::from_config(&config, db)?;
        manager.emit(&event, db)?;
    }
    Ok(())
}
```

Wire into:
- `commands/create.rs` → `IssueCreated`
- `commands/update.rs` → `IssueUpdated`
- `commands/status.rs` (close/reopen) → `IssueClosed` / `IssueReopened`
- `commands/comment.rs` → `CommentAdded`
- `commands/label.rs` → `LabelAdded` / `LabelRemoved`
- `commands/milestone.rs` → `MilestoneCreated` / `MilestoneClosed`
- `commands/session.rs` (start) → `SessionStarted` + auto pull-sync
- `commands/session.rs` (end) → auto push-sync + `SessionEnded`

---

## Cargo.toml changes

```toml
[features]
default = []
jira = ["reqwest", "base64"]
github = ["reqwest"]
linear = ["reqwest"]
all-plugins = ["jira", "github", "linear"]

[dependencies]
toml = "0.8"                                          # always (for config parsing)
reqwest = { version = "0.12", features = ["blocking", "json"], optional = true }
base64 = { version = "0.22", optional = true }        # for Jira basic auth
```

Users build with plugins they need:
```bash
cargo install chainlink-tracker --features jira
cargo install chainlink-tracker --features all-plugins
```

---

## Testing strategy

1. **Unit tests** for each plugin: mock HTTP responses, verify correct API calls and field mapping
2. **Integration tests** for sync engine: create local issues, simulate remote changes, verify bidirectional sync
3. **Config parsing tests**: valid/invalid TOML, missing fields, env var resolution
4. **Conflict resolution tests**: simulate conflicts, verify "ask" mode output format

---

## What this does NOT include (future work)

- Process-based external plugin protocol (JSON-RPC over stdin/stdout)
- Webhook receiver for real-time remote→local push (requires running a server)
- OAuth2 browser flow auth
- Plugin marketplace/registry
- Automatic field discovery from Jira/GitHub/Linear APIs
