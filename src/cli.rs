use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// The title of the observation (fast path)
    pub title: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    pub fn wants_json(&self) -> bool {
        match &self.command {
            Some(Command::Report(args)) => args.json,
            Some(Command::List(args)) => args.format.as_deref() == Some("json"),
            Some(Command::Context(args)) => args.format.as_deref() == Some("json"),
            Some(Command::Export(args)) => args.format.as_deref() == Some("json"),
            _ => false,
        }
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// Durably records one observation
    Report(ReportArgs),
    /// Lists locally captured observations
    List(ListArgs),
    /// Displays the immutable payload, context, artifacts, etc.
    Show(ShowArgs),
    /// Shows what Snag would attach to a report from the current process
    Context(ContextArgs),
    /// Produces deterministic Panopticon-ready records
    Export(ExportArgs),
    /// Creates and verifies a point-in-time database backup and its manifest
    Backup(BackupArgs),
    /// Verifies the SQLite integrity and observation hash chain
    Verify(VerifyArgs),
    /// Checks configuration, backup freshness, and system context
    Doctor(DoctorArgs),
    /// Adds a retraction action without deleting the original observation
    Retract(RetractArgs),
}

#[derive(Args)]
pub struct ReportArgs {
    #[arg(required_unless_present_any = ["json", "stdin"])]
    pub title: Option<String>,

    #[arg(long)]
    pub kind: Option<String>,

    #[arg(long)]
    pub severity: Option<String>,

    #[arg(long)]
    pub expected: Option<String>,

    #[arg(long)]
    pub observed: Option<String>,

    #[arg(long)]
    pub workaround: Option<String>,

    #[arg(long)]
    pub repro: Option<String>,

    #[arg(long)]
    pub json: bool,

    #[arg(long)]
    pub stdin: bool,

    #[arg(long = "artifact")]
    pub artifacts: Vec<PathBuf>,

    #[arg(long)]
    pub idempotency_key: Option<String>,

    #[arg(long)]
    pub repo_id: Option<String>,

    #[arg(long)]
    pub session_id: Option<String>,

    #[arg(long)]
    pub pearl_id: Option<String>,

    #[arg(long)]
    pub attempt_id: Option<String>,

    #[arg(long = "affected-repo")]
    pub affected_repos: Vec<String>,
}

#[derive(Args)]
pub struct ListArgs {
    #[arg(long)]
    pub repo: Option<String>,

    #[arg(long)]
    pub since: Option<String>,

    #[arg(long)]
    pub source: Option<String>,
    
    #[arg(long)]
    pub kind: Option<String>,
    
    #[arg(long)]
    pub limit: Option<usize>,

    #[arg(long)]
    pub format: Option<String>,
}

#[derive(Args)]
pub struct ShowArgs {
    pub observation_id: String,
}

#[derive(Args)]
pub struct ContextArgs {
    #[arg(long)]
    pub format: Option<String>,
}

#[derive(Args)]
pub struct ExportArgs {
    #[arg(long)]
    pub format: Option<String>,

    #[arg(long)]
    pub after_sequence: Option<u64>,

    #[arg(long)]
    pub through_sequence: Option<u64>,

    #[arg(long)]
    pub output: Option<PathBuf>,
}

#[derive(Args)]
pub struct BackupArgs {}

#[derive(Args)]
pub struct VerifyArgs {
    #[arg(long)]
    pub quick: bool,

    #[arg(long)]
    pub full: bool,

    #[arg(long)]
    pub backup: Option<PathBuf>,
}

#[derive(Args)]
pub struct DoctorArgs {}

#[derive(Args)]
pub struct RetractArgs {
    pub observation_id: String,
}
