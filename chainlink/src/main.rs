mod commands;
mod daemon;
mod db;
mod models;
mod plugin;
mod utils;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::env;
use std::path::{Path, PathBuf};

use db::Database;
use plugin::config::PluginConfig;
use plugin::{ChainlinkEvent, PluginManager};

#[derive(Parser)]
#[command(name = "chainlink")]
#[command(about = "A simple, lean issue tracker CLI")]
#[command(version)]
struct Cli {
    /// Quiet mode: only output essential data (IDs, counts)
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Output as JSON (supported by list, show, search, session status)
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize chainlink in the current directory
    Init {
        /// Force update hooks even if already initialized
        #[arg(short, long)]
        force: bool,
    },

    /// Create a new issue
    Create {
        /// Issue title
        title: String,
        /// Issue description
        #[arg(short, long)]
        description: Option<String>,
        /// Priority (low, medium, high, critical)
        #[arg(short, long, default_value = "medium")]
        priority: String,
        /// Template (bug, feature, refactor, research)
        #[arg(short, long)]
        template: Option<String>,
        /// Add labels to the issue
        #[arg(short, long)]
        label: Vec<String>,
        /// Set as current session work item
        #[arg(short, long)]
        work: bool,
    },

    /// Quick-create an issue and start working on it (create + label + session work)
    Quick {
        /// Issue title
        title: String,
        /// Issue description
        #[arg(short, long)]
        description: Option<String>,
        /// Priority (low, medium, high, critical)
        #[arg(short, long, default_value = "medium")]
        priority: String,
        /// Template (bug, feature, refactor, research)
        #[arg(short, long)]
        template: Option<String>,
        /// Add labels to the issue
        #[arg(short, long)]
        label: Vec<String>,
    },

    /// Create a subissue under a parent issue
    Subissue {
        /// Parent issue ID
        parent: i64,
        /// Subissue title
        title: String,
        /// Subissue description
        #[arg(short, long)]
        description: Option<String>,
        /// Priority (low, medium, high, critical)
        #[arg(short, long, default_value = "medium")]
        priority: String,
        /// Add labels to the subissue
        #[arg(short, long)]
        label: Vec<String>,
        /// Set as current session work item
        #[arg(short, long)]
        work: bool,
    },

    /// List issues
    List {
        /// Filter by status (open, closed, all)
        #[arg(short, long, default_value = "open")]
        status: String,
        /// Filter by label
        #[arg(short, long)]
        label: Option<String>,
        /// Filter by priority
        #[arg(short, long)]
        priority: Option<String>,
    },

    /// Search issues by text
    Search {
        /// Search query
        query: String,
    },

    /// Show issue details
    Show {
        /// Issue ID
        id: i64,
    },

    /// Update an issue
    Update {
        /// Issue ID
        id: i64,
        /// New title
        #[arg(short, long)]
        title: Option<String>,
        /// New description
        #[arg(short, long)]
        description: Option<String>,
        /// New priority
        #[arg(short, long)]
        priority: Option<String>,
    },

    /// Close an issue
    Close {
        /// Issue ID
        id: i64,
        /// Skip changelog entry
        #[arg(long)]
        no_changelog: bool,
    },

    /// Close all issues matching filters
    CloseAll {
        /// Filter by label
        #[arg(short, long)]
        label: Option<String>,
        /// Filter by priority
        #[arg(short, long)]
        priority: Option<String>,
        /// Skip changelog entries
        #[arg(long)]
        no_changelog: bool,
    },

    /// Reopen a closed issue
    Reopen {
        /// Issue ID
        id: i64,
    },

    /// Delete an issue
    Delete {
        /// Issue ID
        id: i64,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Add a comment to an issue
    Comment {
        /// Issue ID
        id: i64,
        /// Comment text
        text: String,
    },

    /// Add a label to an issue
    Label {
        /// Issue ID
        id: i64,
        /// Label name
        label: String,
    },

    /// Remove a label from an issue
    Unlabel {
        /// Issue ID
        id: i64,
        /// Label name
        label: String,
    },

    /// Mark an issue as blocked by another
    Block {
        /// Issue ID that is blocked
        id: i64,
        /// Issue ID that is blocking
        blocker: i64,
    },

    /// Remove a blocking relationship
    Unblock {
        /// Issue ID that was blocked
        id: i64,
        /// Issue ID that was blocking
        blocker: i64,
    },

    /// List blocked issues
    Blocked,

    /// List issues ready to work on (no open blockers)
    Ready,

    /// Link two related issues
    Relate {
        /// First issue ID
        id: i64,
        /// Second issue ID
        related: i64,
    },

    /// Remove a relation between issues
    Unrelate {
        /// First issue ID
        id: i64,
        /// Second issue ID
        related: i64,
    },

    /// List related issues
    Related {
        /// Issue ID
        id: i64,
    },

    /// Suggest the next issue to work on
    Next,

    /// Show issues as a tree hierarchy
    Tree {
        /// Filter by status (open, closed, all)
        #[arg(short, long, default_value = "all")]
        status: String,
    },

    /// Start a timer for an issue
    Start {
        /// Issue ID
        id: i64,
    },

    /// Stop the current timer
    Stop,

    /// Show current timer status
    Timer,

    /// Mark tests as run (resets test reminder)
    Tested,

    /// Export issues to JSON or markdown
    Export {
        /// Output file path (defaults to stdout)
        #[arg(short, long)]
        output: Option<String>,
        /// Format (json, markdown)
        #[arg(short, long, default_value = "json")]
        format: String,
    },

    /// Import issues from JSON file
    Import {
        /// Input file path
        input: String,
    },

    /// Archive management
    Archive {
        #[command(subcommand)]
        action: ArchiveCommands,
    },

    /// Milestone management
    Milestone {
        #[command(subcommand)]
        action: MilestoneCommands,
    },

    /// Session management
    Session {
        #[command(subcommand)]
        action: SessionCommands,
    },

    /// Daemon management
    Daemon {
        #[command(subcommand)]
        action: DaemonCommands,
    },

    /// Code clone detection via cpitd
    Cpitd {
        #[command(subcommand)]
        action: CpitdCommands,
    },

    /// Plugin management for external integrations (Jira, GitHub, Linear)
    Plugin {
        #[command(subcommand)]
        action: PluginCommands,
    },
}

#[derive(Subcommand)]
enum ArchiveCommands {
    /// Archive a closed issue
    Add {
        /// Issue ID
        id: i64,
    },
    /// Unarchive an issue (restore to closed)
    Remove {
        /// Issue ID
        id: i64,
    },
    /// List archived issues
    List,
    /// Archive all issues closed more than N days ago
    Older {
        /// Days threshold
        days: i64,
    },
}

#[derive(Subcommand)]
enum MilestoneCommands {
    /// Create a new milestone
    Create {
        /// Milestone name
        name: String,
        /// Description
        #[arg(short, long)]
        description: Option<String>,
    },
    /// List milestones
    List {
        /// Filter by status (open, closed, all)
        #[arg(short, long, default_value = "open")]
        status: String,
    },
    /// Show milestone details
    Show {
        /// Milestone ID
        id: i64,
    },
    /// Add issues to a milestone
    Add {
        /// Milestone ID
        id: i64,
        /// Issue IDs to add
        issues: Vec<i64>,
    },
    /// Remove an issue from a milestone
    Remove {
        /// Milestone ID
        id: i64,
        /// Issue ID to remove
        issue: i64,
    },
    /// Close a milestone
    Close {
        /// Milestone ID
        id: i64,
    },
    /// Delete a milestone
    Delete {
        /// Milestone ID
        id: i64,
    },
}

#[derive(Subcommand)]
enum SessionCommands {
    /// Start a new session
    Start,
    /// End the current session
    End {
        /// Handoff notes for the next session
        #[arg(short, long)]
        notes: Option<String>,
    },
    /// Show current session status
    Status,
    /// Set the issue being worked on
    Work {
        /// Issue ID
        id: i64,
    },
    /// Show handoff notes from the previous session
    LastHandoff,
    /// Record last action for context compression breadcrumbs
    Action {
        /// Description of what you just did or are doing
        text: String,
    },
}

#[derive(Subcommand)]
enum CpitdCommands {
    /// Scan for code clones and create issues
    Scan {
        /// Paths to scan (defaults to current directory)
        paths: Vec<String>,
        /// Minimum token sequence length to report
        #[arg(long, default_value = "50")]
        min_tokens: u32,
        /// Glob patterns to exclude (repeatable)
        #[arg(long)]
        ignore: Vec<String>,
        /// Show what would be created without creating issues
        #[arg(long)]
        dry_run: bool,
    },
    /// Show open clone issues
    Status,
    /// Close all open clone issues
    Clear,
}

#[derive(Subcommand)]
enum DaemonCommands {
    /// Start the background daemon
    Start,
    /// Stop the background daemon
    Stop,
    /// Check daemon status
    Status,
    /// Internal: run the daemon loop (used by start)
    #[command(hide = true)]
    Run {
        #[arg(long)]
        dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum PluginCommands {
    /// List configured plugins and their status
    List,
    /// Configure a plugin (jira, github, linear)
    Configure {
        /// Plugin name
        name: String,
    },
    /// Validate plugin configuration and credentials
    Validate {
        /// Plugin name (validates all if omitted)
        name: Option<String>,
    },
    /// Run a full bidirectional sync
    Sync {
        /// Only sync this plugin
        #[arg(short, long)]
        plugin: Option<String>,
    },
    /// Show sync status for all plugins
    Status,
    /// Manually link a local issue to a remote issue
    Link {
        /// Local issue ID
        id: i64,
        /// Remote issue ID/key (e.g. "PROJ-123", "42")
        remote_id: String,
        /// Plugin name (jira, github, linear)
        #[arg(short, long)]
        plugin: String,
    },
    /// Remove a sync mapping
    Unlink {
        /// Local issue ID
        id: i64,
        /// Plugin name (removes all if omitted)
        #[arg(short, long)]
        plugin: Option<String>,
    },
}

fn find_chainlink_dir() -> Result<PathBuf> {
    let mut current = env::current_dir()?;

    loop {
        let candidate = current.join(".chainlink");
        if candidate.is_dir() {
            return Ok(candidate);
        }

        if !current.pop() {
            bail!("Not a chainlink repository (or any parent). Run 'chainlink init' first.");
        }
    }
}

fn get_db() -> Result<Database> {
    let chainlink_dir = find_chainlink_dir()?;
    let db_path = chainlink_dir.join("issues.db");
    Database::open(&db_path).context("Failed to open database")
}

/// Try to load the plugin manager from plugins.toml. Returns None if no config exists.
fn try_get_plugin_manager(chainlink_dir: &Path) -> Option<PluginManager> {
    let config_path = chainlink_dir.join("plugins.toml");
    if !config_path.exists() {
        return None;
    }
    let config = PluginConfig::load(&config_path).ok()?;
    if !config.has_enabled_plugins() {
        return None;
    }
    PluginManager::from_config(&config).ok()
}

/// Emit a plugin event if plugins are configured. Non-fatal: errors are printed as warnings.
fn maybe_emit_event(db: &Database, event: ChainlinkEvent) {
    if let Ok(chainlink_dir) = find_chainlink_dir() {
        if let Some(manager) = try_get_plugin_manager(&chainlink_dir) {
            let errors = manager.emit(&event, db);
            for err in errors {
                eprintln!("Warning: plugin sync error: {}", err);
            }
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init { force } => {
            let cwd = env::current_dir()?;
            commands::init::run(&cwd, force)
        }

        Commands::Create {
            title,
            description,
            priority,
            template,
            label,
            work,
        } => {
            let db = get_db()?;
            let opts = commands::create::CreateOpts {
                labels: &label,
                work,
                quiet: cli.quiet,
            };
            commands::create::run(
                &db,
                &title,
                description.as_deref(),
                &priority,
                template.as_deref(),
                &opts,
            )?;

            // Emit IssueCreated event for the most recently created issue
            // The issue ID is the last inserted row
            if let Ok(issues) = db.list_issues(Some("open"), None, None) {
                if let Some(issue) = issues.into_iter().rev().find(|i| i.title == title) {
                    maybe_emit_event(&db, ChainlinkEvent::IssueCreated { issue });
                }
            }

            Ok(())
        }

        Commands::Quick {
            title,
            description,
            priority,
            template,
            label,
        } => {
            let db = get_db()?;
            let opts = commands::create::CreateOpts {
                labels: &label,
                work: true,
                quiet: cli.quiet,
            };
            commands::create::run(
                &db,
                &title,
                description.as_deref(),
                &priority,
                template.as_deref(),
                &opts,
            )?;

            if let Ok(issues) = db.list_issues(Some("open"), None, None) {
                if let Some(issue) = issues.into_iter().rev().find(|i| i.title == title) {
                    maybe_emit_event(&db, ChainlinkEvent::IssueCreated { issue });
                }
            }

            Ok(())
        }

        Commands::Subissue {
            parent,
            title,
            description,
            priority,
            label,
            work,
        } => {
            let db = get_db()?;
            let opts = commands::create::CreateOpts {
                labels: &label,
                work,
                quiet: cli.quiet,
            };
            commands::create::run_subissue(
                &db,
                parent,
                &title,
                description.as_deref(),
                &priority,
                &opts,
            )?;

            if let Ok(issues) = db.list_issues(Some("open"), None, None) {
                if let Some(issue) = issues.into_iter().rev().find(|i| i.title == title) {
                    maybe_emit_event(&db, ChainlinkEvent::IssueCreated { issue });
                }
            }

            Ok(())
        }

        Commands::List {
            status,
            label,
            priority,
        } => {
            let db = get_db()?;
            if cli.json {
                commands::list::run_json(&db, Some(&status), label.as_deref(), priority.as_deref())
            } else {
                commands::list::run(&db, Some(&status), label.as_deref(), priority.as_deref())
            }
        }

        Commands::Search { query } => {
            let db = get_db()?;
            if cli.json {
                commands::search::run_json(&db, &query)
            } else {
                commands::search::run(&db, &query)
            }
        }

        Commands::Show { id } => {
            let db = get_db()?;
            if cli.json {
                commands::show::run_json(&db, id)
            } else {
                commands::show::run(&db, id)
            }
        }

        Commands::Update {
            id,
            title,
            description,
            priority,
        } => {
            let db = get_db()?;
            commands::update::run(
                &db,
                id,
                title.as_deref(),
                description.as_deref(),
                priority.as_deref(),
            )?;

            // Emit update event
            if let Ok(Some(issue)) = db.get_issue(id) {
                let mut changed = Vec::new();
                if title.is_some() {
                    changed.push("title".to_string());
                }
                if description.is_some() {
                    changed.push("description".to_string());
                }
                if priority.is_some() {
                    changed.push("priority".to_string());
                }
                maybe_emit_event(
                    &db,
                    ChainlinkEvent::IssueUpdated {
                        issue,
                        changed_fields: changed,
                    },
                );
            }

            Ok(())
        }

        Commands::Close { id, no_changelog } => {
            let db = get_db()?;
            let chainlink_dir = find_chainlink_dir()?;

            // Get issue before closing for the event
            let issue_before = db.get_issue(id)?;

            if cli.quiet {
                commands::status::close_quiet(&db, id, !no_changelog, &chainlink_dir)?;
            } else {
                commands::status::close(&db, id, !no_changelog, &chainlink_dir)?;
            }

            if let Some(mut issue) = issue_before {
                issue.status = "closed".to_string();
                maybe_emit_event(&db, ChainlinkEvent::IssueClosed { issue });
            }

            Ok(())
        }

        Commands::CloseAll {
            label,
            priority,
            no_changelog,
        } => {
            let db = get_db()?;
            let chainlink_dir = find_chainlink_dir()?;
            commands::status::close_all(
                &db,
                label.as_deref(),
                priority.as_deref(),
                !no_changelog,
                &chainlink_dir,
            )
        }

        Commands::Reopen { id } => {
            let db = get_db()?;
            commands::status::reopen(&db, id)?;

            if let Ok(Some(issue)) = db.get_issue(id) {
                maybe_emit_event(&db, ChainlinkEvent::IssueReopened { issue });
            }

            Ok(())
        }

        Commands::Delete { id, force } => {
            let db = get_db()?;
            commands::delete::run(&db, id, force)
        }

        Commands::Comment { id, text } => {
            let db = get_db()?;
            commands::comment::run(&db, id, &text)?;

            // Emit comment event
            if let Ok(comments) = db.get_comments(id) {
                if let Some(comment) = comments.into_iter().last() {
                    maybe_emit_event(
                        &db,
                        ChainlinkEvent::CommentAdded {
                            issue_id: id,
                            comment,
                        },
                    );
                }
            }

            Ok(())
        }

        Commands::Label { id, label } => {
            let db = get_db()?;
            commands::label::add(&db, id, &label)?;

            maybe_emit_event(
                &db,
                ChainlinkEvent::LabelAdded {
                    issue_id: id,
                    label,
                },
            );

            Ok(())
        }

        Commands::Unlabel { id, label } => {
            let db = get_db()?;
            commands::label::remove(&db, id, &label)?;

            maybe_emit_event(
                &db,
                ChainlinkEvent::LabelRemoved {
                    issue_id: id,
                    label,
                },
            );

            Ok(())
        }

        Commands::Block { id, blocker } => {
            let db = get_db()?;
            commands::deps::block(&db, id, blocker)
        }

        Commands::Unblock { id, blocker } => {
            let db = get_db()?;
            commands::deps::unblock(&db, id, blocker)
        }

        Commands::Blocked => {
            let db = get_db()?;
            commands::deps::list_blocked(&db)
        }

        Commands::Ready => {
            let db = get_db()?;
            commands::deps::list_ready(&db)
        }

        Commands::Relate { id, related } => {
            let db = get_db()?;
            commands::relate::add(&db, id, related)
        }

        Commands::Unrelate { id, related } => {
            let db = get_db()?;
            commands::relate::remove(&db, id, related)
        }

        Commands::Related { id } => {
            let db = get_db()?;
            commands::relate::list(&db, id)
        }

        Commands::Next => {
            let db = get_db()?;
            commands::next::run(&db)
        }

        Commands::Tree { status } => {
            let db = get_db()?;
            commands::tree::run(&db, Some(&status))
        }

        Commands::Start { id } => {
            let db = get_db()?;
            commands::timer::start(&db, id)
        }

        Commands::Stop => {
            let db = get_db()?;
            commands::timer::stop(&db)
        }

        Commands::Timer => {
            let db = get_db()?;
            commands::timer::status(&db)
        }

        Commands::Tested => {
            let chainlink_dir = find_chainlink_dir()?;
            commands::tested::run(&chainlink_dir)
        }

        Commands::Export { output, format } => {
            let db = get_db()?;
            match format.as_str() {
                "json" => commands::export::run_json(&db, output.as_deref()),
                "markdown" | "md" => commands::export::run_markdown(&db, output.as_deref()),
                _ => {
                    bail!("Unknown format '{}'. Use 'json' or 'markdown'", format);
                }
            }
        }

        Commands::Import { input } => {
            let db = get_db()?;
            let path = std::path::Path::new(&input);
            commands::import::run_json(&db, path)
        }

        Commands::Archive { action } => {
            let db = get_db()?;
            match action {
                ArchiveCommands::Add { id } => commands::archive::archive(&db, id),
                ArchiveCommands::Remove { id } => commands::archive::unarchive(&db, id),
                ArchiveCommands::List => commands::archive::list(&db),
                ArchiveCommands::Older { days } => commands::archive::archive_older(&db, days),
            }
        }

        Commands::Milestone { action } => {
            let db = get_db()?;
            match action {
                MilestoneCommands::Create { name, description } => {
                    commands::milestone::create(&db, &name, description.as_deref())?;

                    // Emit milestone created event
                    if let Ok(milestones) = db.list_milestones(Some("open")) {
                        if let Some(ms) = milestones.into_iter().rev().find(|m| m.name == name) {
                            maybe_emit_event(
                                &db,
                                ChainlinkEvent::MilestoneCreated { milestone: ms },
                            );
                        }
                    }

                    Ok(())
                }
                MilestoneCommands::List { status } => commands::milestone::list(&db, Some(&status)),
                MilestoneCommands::Show { id } => commands::milestone::show(&db, id),
                MilestoneCommands::Add { id, issues } => commands::milestone::add(&db, id, &issues),
                MilestoneCommands::Remove { id, issue } => {
                    commands::milestone::remove(&db, id, issue)
                }
                MilestoneCommands::Close { id } => {
                    commands::milestone::close(&db, id)?;

                    if let Ok(Some(ms)) = db.get_milestone(id) {
                        maybe_emit_event(
                            &db,
                            ChainlinkEvent::MilestoneClosed { milestone: ms },
                        );
                    }

                    Ok(())
                }
                MilestoneCommands::Delete { id } => commands::milestone::delete(&db, id),
            }
        }

        Commands::Session { action } => {
            let db = get_db()?;
            match action {
                SessionCommands::Start => {
                    commands::session::start(&db)?;

                    // Auto-pull from plugins on session start
                    if let Ok(chainlink_dir) = find_chainlink_dir() {
                        if let Some(manager) = try_get_plugin_manager(&chainlink_dir) {
                            if !manager.is_empty() {
                                println!("Syncing with plugins...");
                                for (name, result) in manager.pull_all(&db) {
                                    match result {
                                        Ok(report) => {
                                            plugin::sync::print_sync_summary(
                                                &name,
                                                report.pulled,
                                                0,
                                                &report.errors,
                                            );
                                        }
                                        Err(e) => eprintln!("[{}] Pull failed: {}", name, e),
                                    }
                                }
                            }
                        }
                    }

                    // Emit session started event
                    if let Ok(Some(session)) = db.get_current_session() {
                        maybe_emit_event(
                            &db,
                            ChainlinkEvent::SessionStarted { session },
                        );
                    }

                    Ok(())
                }
                SessionCommands::End { notes } => {
                    // Auto-push to plugins on session end
                    if let Ok(chainlink_dir) = find_chainlink_dir() {
                        if let Some(manager) = try_get_plugin_manager(&chainlink_dir) {
                            if !manager.is_empty() {
                                println!("Pushing changes to plugins...");
                                for (name, result) in manager.push_all(&db) {
                                    match result {
                                        Ok(report) => {
                                            plugin::sync::print_sync_summary(
                                                &name,
                                                0,
                                                report.pushed,
                                                &report.errors,
                                            );
                                        }
                                        Err(e) => eprintln!("[{}] Push failed: {}", name, e),
                                    }
                                }
                            }
                        }
                    }

                    commands::session::end(&db, notes.as_deref())?;

                    if let Ok(Some(session)) = db.get_last_session() {
                        maybe_emit_event(
                            &db,
                            ChainlinkEvent::SessionEnded { session },
                        );
                    }

                    Ok(())
                }
                SessionCommands::Status => commands::session::status(&db),
                SessionCommands::Work { id } => commands::session::work(&db, id),
                SessionCommands::LastHandoff => commands::session::last_handoff(&db),
                SessionCommands::Action { text } => commands::session::action(&db, &text),
            }
        }

        Commands::Daemon { action } => match action {
            DaemonCommands::Start => {
                let chainlink_dir = find_chainlink_dir()?;
                daemon::start(&chainlink_dir)
            }
            DaemonCommands::Stop => {
                let chainlink_dir = find_chainlink_dir()?;
                daemon::stop(&chainlink_dir)
            }
            DaemonCommands::Status => {
                let chainlink_dir = find_chainlink_dir()?;
                daemon::status(&chainlink_dir)
            }
            DaemonCommands::Run { dir } => daemon::run_daemon(&dir),
        },

        Commands::Cpitd { action } => {
            let db = get_db()?;
            match action {
                CpitdCommands::Scan {
                    paths,
                    min_tokens,
                    ignore,
                    dry_run,
                } => commands::cpitd::scan(&db, &paths, min_tokens, &ignore, dry_run, cli.quiet),
                CpitdCommands::Status => commands::cpitd::status(&db),
                CpitdCommands::Clear => commands::cpitd::clear(&db),
            }
        }

        Commands::Plugin { action } => {
            let chainlink_dir = find_chainlink_dir()?;
            match action {
                PluginCommands::List => commands::plugin::list(&chainlink_dir),
                PluginCommands::Configure { name } => {
                    commands::plugin::configure(&chainlink_dir, &name)
                }
                PluginCommands::Validate { name } => {
                    let db = get_db()?;
                    commands::plugin::validate(&chainlink_dir, name.as_deref(), &db)
                }
                PluginCommands::Sync { plugin } => {
                    let db = get_db()?;
                    commands::plugin::sync(&chainlink_dir, &db, plugin.as_deref())
                }
                PluginCommands::Status => {
                    let db = get_db()?;
                    commands::plugin::status(&chainlink_dir, &db)
                }
                PluginCommands::Link {
                    id,
                    remote_id,
                    plugin,
                } => {
                    let db = get_db()?;
                    commands::plugin::link(&db, &chainlink_dir, id, &remote_id, &plugin)
                }
                PluginCommands::Unlink { id, plugin } => {
                    let db = get_db()?;
                    commands::plugin::unlink(&db, id, plugin.as_deref())
                }
            }
        }
    }
}
