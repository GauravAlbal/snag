use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

/// Full build-provenance version string, e.g.
/// `0.3.0-dev (rev 98e61c6-dirty, built 2026-08-06)` — the `, internal`
/// flavor suffix appears only on internal-lane builds (see build.rs).
pub const BUILD_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (rev ",
    env!("SNAG_BUILD_REV"),
    ", built ",
    env!("SNAG_BUILD_DATE"),
    env!("SNAG_BUILD_FLAVOR_SUFFIX"),
    ")",
);

#[derive(Parser)]
#[command(author, version = BUILD_VERSION, about, long_about = None)]
pub struct Cli {
    /// The title of the observation (fast path — equivalent to `snag report <title>` with the flags below)
    pub title: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,

    // Fast-path structured flags (mirror ReportArgs so `snag "<title>"
    // --kind bug --severity minor` works without the `report` subcommand).
    /// asserted kind (bug|tooling|papercut|friction|usability|probe|feature) — the class of problem
    #[arg(long)]
    pub kind: Option<String>,

    #[arg(
        long,
        help = "asserted impact (blocker|major|medium|minor|low) — a prior, not a posterior: reviewers re-rank on disposition; reserve blocker/major for fleet-blocking classes"
    )]
    pub severity: Option<String>,

    /// what should have happened — the violated contract
    #[arg(long)]
    pub expected: Option<String>,

    /// what actually happened — the observed evidence
    #[arg(long)]
    pub observed: Option<String>,

    /// how the issue was worked around, if at all
    #[arg(long)]
    pub workaround: Option<String>,

    /// minimal reproduction steps
    #[arg(long)]
    pub repro: Option<String>,

    /// JSON output; when TITLE names an existing file (or '-'), it becomes JSON intake instead
    #[arg(long)]
    pub json: bool,

    /// read the observation payload from stdin (with --json, stdin owns intake and --json only selects JSON output)
    #[arg(long)]
    pub stdin: bool,

    /// attach one or more artifact files (repeatable)
    #[arg(long = "artifact")]
    pub artifacts: Vec<PathBuf>,

    /// stable attempt-local key — same key + same payload replays the original observation; same key + different payload is a typed conflict
    #[arg(long)]
    pub idempotency_key: Option<String>,

    /// override the detected repository identity
    #[arg(long)]
    pub repo_id: Option<String>,

    /// the repo/lane that owns the fix (id, alias, or `current`) — distinct
    /// from the filing context; summary and review list group by this when set
    #[arg(long)]
    pub owner: Option<String>,

    /// declare that no repository owner is currently known
    #[arg(long, conflicts_with = "owner")]
    pub unowned: bool,

    /// session identifier recorded with the observation
    #[arg(long)]
    pub session_id: Option<String>,

    /// task identifier recorded with the observation
    #[arg(long)]
    pub task_id: Option<String>,

    /// attempt identifier recorded with the observation
    #[arg(long)]
    pub attempt_id: Option<String>,

    /// additional affected repositories (repeatable)
    #[arg(long = "affected-repo")]
    pub affected_repos: Vec<String>,
}

impl Cli {
    pub fn wants_json(&self) -> bool {
        match &self.command {
            Some(Command::Report(args)) => args.json,
            Some(Command::List(args)) => args.format.as_deref() == Some("json"),
            Some(Command::Context(args)) => args.format.as_deref() == Some("json"),
            Some(Command::Export(args)) => args.format.as_deref() == Some("json"),
            Some(Command::Review(cmd)) => match cmd {
                ReviewCommand::Next(args) => args.format.as_deref() == Some("agent"),
                ReviewCommand::List(args) => args.format.as_deref() == Some("json"),
                ReviewCommand::Summary(args) => args.format.as_deref() == Some("json"),
                #[cfg(snag_internal)]
                ReviewCommand::Retro(args) => args.format.as_deref() == Some("json"),
                _ => false,
            },
            Some(_) => false,
            None => self.json,
        }
    }
}

#[allow(clippy::large_enum_variant)]
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
    /// Produces a deterministic JSONL export stream
    Export(ExportArgs),
    /// Creates and verifies a point-in-time database backup and its manifest
    Backup(BackupArgs),
    /// Verifies the SQLite integrity and observation hash chain
    Verify(VerifyArgs),
    /// Checks configuration, backup freshness, and system context
    Doctor(DoctorArgs),
    /// Adds a retraction action without deleting the original observation
    Retract(RetractArgs),
    /// Installs the capture-and-move-on agent instructions into a repo
    Init(InitArgs),
    /// Restores the database from a backup
    Restore(RestoreArgs),
    /// Rebuilds the database from an export stream
    Rebuild(RebuildArgs),
    /// Remediation queue: retrieval, claim leases, adjudication, lineage
    #[command(subcommand)]
    Review(ReviewCommand),
}

#[derive(Args)]
pub struct ReportArgs {
    #[arg(required_unless_present_any = ["json", "stdin"])]
    pub title: Option<String>,

    /// asserted kind (bug|tooling|papercut|friction|usability|probe|feature) — the class of problem
    #[arg(long)]
    pub kind: Option<String>,

    #[arg(
        long,
        help = "asserted impact (blocker|major|medium|minor|low) — a prior, not a posterior: reviewers re-rank on disposition; reserve blocker/major for fleet-blocking classes"
    )]
    pub severity: Option<String>,

    /// what should have happened — the violated contract
    #[arg(long)]
    pub expected: Option<String>,

    /// what actually happened — the observed evidence
    #[arg(long)]
    pub observed: Option<String>,

    /// how the issue was worked around, if at all
    #[arg(long)]
    pub workaround: Option<String>,

    /// minimal reproduction steps
    #[arg(long)]
    pub repro: Option<String>,

    /// JSON output; when TITLE names an existing file (or '-'), it becomes JSON intake instead
    #[arg(long)]
    pub json: bool,

    /// read the observation payload from stdin (with --json, stdin owns intake and --json only selects JSON output)
    #[arg(long)]
    pub stdin: bool,

    /// attach one or more artifact files (repeatable)
    #[arg(long = "artifact")]
    pub artifacts: Vec<PathBuf>,

    /// stable attempt-local key — same key + same payload replays the original observation; same key + different payload is a typed conflict
    #[arg(long)]
    pub idempotency_key: Option<String>,

    /// override the detected repository identity
    #[arg(long)]
    pub repo_id: Option<String>,

    /// the repo/lane that owns the fix (id, alias, or `current`) — distinct
    /// from the filing context; summary and review list group by this when set
    #[arg(long)]
    pub owner: Option<String>,

    /// declare that no repository owner is currently known
    #[arg(long, conflicts_with = "owner")]
    pub unowned: bool,

    /// session identifier recorded with the observation
    #[arg(long)]
    pub session_id: Option<String>,

    /// task identifier recorded with the observation
    #[arg(long)]
    pub task_id: Option<String>,

    /// attempt identifier recorded with the observation
    #[arg(long)]
    pub attempt_id: Option<String>,

    /// additional affected repositories (repeatable)
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

    /// asserted kind (bug|tooling|papercut|friction|usability|probe|feature) — the class of problem
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

    /// Emit a machine-readable store fingerprint (store_id, through_sequence,
    /// head_hash, record_count) after verification
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct DoctorArgs {}

#[derive(Args)]
pub struct RetractArgs {
    pub observation_id: String,
}

#[derive(Args)]
pub struct InitArgs {
    /// Agent to tailor the setup note for (claude-code, codex, gemini-cli, opencode, generic)
    #[arg(long)]
    pub agent: Option<String>,

    /// Target file to write (default: AGENTS.md)
    #[arg(long)]
    pub file: Option<PathBuf>,

    /// Print what would be written without modifying anything
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args)]
pub struct RestoreArgs {
    pub archive: PathBuf,
}

#[derive(Args)]
pub struct RebuildArgs {
    /// Path to the export stream to rebuild from
    #[arg(long = "from-export")]
    pub from_export: PathBuf,

    /// Snag data directory to rebuild into (a fresh snag.sqlite is created
    /// inside it; the directory must not already contain a store). This is a
    /// directory, NOT a database file path — see `snag rebuild` docs.
    #[arg(long)]
    pub destination: PathBuf,
}

/// `snag review …` — the remediation interface. All mutations are append-only
/// records in the global stream; claims are leases, not ownership.
#[derive(Subcommand)]
pub enum ReviewCommand {
    /// Pull the next unhandled observation matching the filters
    Next(ReviewNextArgs),
    /// Acquire a claim lease on an observation
    ///
    /// Claim only observations in your lane: check the repos and labels for
    /// ownership before claiming. The lease expires — process death never
    /// strands work.
    Claim(ReviewClaimArgs),
    /// Release an active claim lease
    Release(ReviewReleaseArgs),
    /// Extend an active claim lease
    Heartbeat(ReviewHeartbeatArgs),
    /// List observations with their review state
    List(ReviewListArgs),
    /// Per-owner open-observation materiality summary (dispatch aid)
    Summary(ReviewSummaryArgs),
    /// Show remediation-health metrics over a bounded historical window
    #[cfg(snag_internal)]
    Retro(ReviewRetroArgs),
    /// Assign or reassign the repository that owns the fix (append-only)
    AssignOwner(ReviewAssignOwnerArgs),
    /// Adjudicate an observation with a disposition
    ///
    /// Negative dispositions are first-class outcomes. `confirmed` commits
    /// your lane to the fix; observations owned by another lane should be
    /// `deferred` with the owner lane in `--rationale`, then
    /// `reopen-remediation` to keep them visible in the queue.
    Disposition(ReviewDispositionArgs),
    /// Reopen a previous disposition (append-only)
    Reopen(ReviewReopenArgs),
    /// Assert a relationship between two observations
    Relate(ReviewRelateArgs),
    /// Retract a relationship assertion (append-only)
    Unrelate(ReviewUnrelateArgs),
    /// Promote a confirmed observation to a finding
    ///
    /// Requires the observation to have disposition `confirmed`.
    Promote(ReviewPromoteArgs),
    /// Attach owned work (multiple task ids supported)
    ///
    /// Requires the observation to have disposition `confirmed`.
    AttachTask(ReviewAttachTaskArgs),
    /// Attach a candidate fixing commit
    ///
    /// Requires the observation to have disposition `confirmed`.
    AttachFix(ReviewAttachFixArgs),
    /// Attach verification evidence (accepted is the only verifying status)
    ///
    /// Requires the observation to have disposition `confirmed`.
    AttachVerification(ReviewAttachVerificationArgs),
    /// Declare an observation durably handled
    MarkHandled(ReviewMarkHandledArgs),
    /// Reopen a handled remediation (append-only)
    ReopenRemediation(ReviewReopenRemediationArgs),
    /// Inspect the full evidence packet for one observation
    Show(ReviewShowArgs),
    /// List the observation's remediation event history
    History(ReviewHistoryArgs),
    /// Validate a completion report against the recorded events
    VerifyReport(ReviewVerifyReportArgs),
}
#[cfg(snag_internal)]
#[derive(Args)]
pub struct ReviewRetroArgs {
    /// Restrict to a repository (id, alias, or `current`)
    #[arg(long)]
    pub repo: Option<String>,

    /// Restrict to an asserted severity
    #[arg(long)]
    pub severity: Option<String>,

    /// Inclusive UTC window start (RFC 3339 date or timestamp)
    #[arg(long = "from")]
    pub from: Option<String>,

    /// Exclusive UTC window end (RFC 3339 date or timestamp)
    #[arg(long = "to")]
    pub to: Option<String>,

    /// Time bucket: day or week
    #[arg(long, default_value = "day")]
    pub bucket: String,

    /// Add the JSON detail object; for text, add severity and trend drilldowns
    #[arg(long)]
    pub detail: bool,

    /// Output format: text (default) or json (review_retro_v1 envelope)
    #[arg(long, value_parser = ["text", "json"])]
    pub format: Option<String>,
}

#[derive(Args)]
pub struct ReviewNextArgs {
    /// Restrict to a fix-owner repository/lane (id, alias, or `current`).
    /// Unlike top-level `snag list --repo`, this never filters broader
    /// reporter/affected repository relationships.
    #[arg(long)]
    pub repo: Option<String>,

    /// Restrict to an asserted kind (bug|tooling|papercut|friction|usability|probe|feature) — the class of problem
    #[arg(long)]
    pub kind: Option<String>,

    /// Restrict to an asserted severity
    #[arg(
        long,
        help = "restrict to an asserted severity (blocker|major|medium|minor|low) — the reporter's prior; disposition re-ranks"
    )]
    pub severity: Option<String>,

    /// Only observations with no review activity yet
    #[arg(long)]
    pub unreviewed: bool,

    /// Restrict to the canonical current work status (actionable|active|resolved|terminal)
    #[arg(long, value_parser = clap::value_parser!(crate::cli::WorkStatusArg))]
    pub work_status: Option<crate::cli::WorkStatusArg>,

    /// Include observations deferred by a prior disposition
    #[arg(long)]
    pub include_deferred: bool,

    /// Output format: `agent` (versioned JSON packet) or `text` (default)
    #[arg(long)]
    pub format: Option<String>,

    /// Claim the returned observation atomically (fold-in: with --task,
    /// link the claim to owned work in the same step)
    #[arg(long)]
    pub claim: bool,

    #[arg(long)]
    pub reviewer: Option<String>,

    #[arg(long)]
    pub session_id: Option<String>,
}

#[derive(Args)]
pub struct ReviewClaimArgs {
    pub observation_id: String,

    /// Lease duration (e.g. 30m, 2h); default from SNAG_REVIEW_LEASE or 30m
    #[arg(long)]
    pub lease: Option<String>,

    /// Link the claim to an owned work item (the "fixing in <task>" marker)
    #[arg(long)]
    pub task: Option<String>,

    #[arg(long)]
    pub reviewer: Option<String>,

    #[arg(long)]
    pub session_id: Option<String>,
}

#[derive(Args)]
pub struct ReviewReleaseArgs {
    pub observation_id: String,

    #[arg(long)]
    pub reason: Option<String>,

    #[arg(long)]
    pub reviewer: Option<String>,

    #[arg(long)]
    pub session_id: Option<String>,
}

#[derive(Args)]
pub struct ReviewHeartbeatArgs {
    pub observation_id: String,

    /// Lease duration for the extension (e.g. 30m); default SNAG_REVIEW_LEASE
    /// or 30m
    #[arg(long)]
    pub lease: Option<String>,

    #[arg(long)]
    pub reviewer: Option<String>,

    #[arg(long)]
    pub session_id: Option<String>,
}

#[derive(Args)]
pub struct ReviewListArgs {
    /// Restrict to a fix-owner repository/lane (id, alias, or `current`).
    /// Unlike top-level `snag list --repo`, this never filters broader
    /// reporter/affected repository relationships.
    #[arg(long)]
    pub repo: Option<String>,

    /// Restrict to an asserted kind (bug|tooling|papercut|friction|usability|probe|feature) — the class of problem
    #[arg(long)]
    pub kind: Option<String>,

    /// Restrict to an asserted severity (blocker|major|medium|minor|low) — the reporter's prior; disposition re-ranks
    #[arg(long)]
    pub severity: Option<String>,

    /// Restrict to the canonical current work status (actionable|active|resolved|terminal)
    #[arg(long, value_parser = clap::value_parser!(crate::cli::WorkStatusArg))]
    pub work_status: Option<crate::cli::WorkStatusArg>,
    /// Only observations with no review activity yet
    #[arg(long)]
    pub unreviewed: bool,

    /// Include observations deferred by a prior disposition (with --unhandled; deferred marks handled=true in the reducer)
    #[arg(long)]
    pub include_deferred: bool,

    /// Only observations claimed by this reviewer/session
    #[arg(long)]
    pub claimed_by: Option<String>,

    /// Only observations with this disposition
    #[arg(long)]
    pub disposition: Option<String>,

    /// Only observations in this derived state
    #[arg(long)]
    pub status: Option<String>,

    #[arg(long)]
    pub handled: bool,

    #[arg(long)]
    pub unhandled: bool,

    /// Maximum rows to print; 0 (default) = unbounded
    #[arg(long, default_value_t = 0)]
    pub limit: usize,

    /// Rows to skip before printing (for paging with --limit)
    #[arg(long, default_value_t = 0)]
    pub offset: usize,

    #[arg(long)]
    pub format: Option<String>,
}

#[derive(Args)]
pub struct ReviewSummaryArgs {
    /// Restrict to a fix-owner repository/lane (id, alias, or `current`);
    /// unlike top-level `snag list --repo`, this evaluates only the fix-owner
    /// lane and never broader reporter/affected relationships.
    #[arg(long)]
    pub repo: Option<String>,

    /// Dispatch threshold: exit 1 when any evaluated owner lane or the unowned
    /// bucket has at least COUNT open actionable observations at SEVERITY
    /// (repeatable; e.g. major=3)
    #[arg(long = "at-least", value_name = "severity=count")]
    pub at_least: Vec<String>,

    /// Maximum owner lanes to print; 0 (default) = unbounded. The exit code
    /// always evaluates every owner lane regardless of this limit.
    #[arg(long, default_value_t = 0)]
    pub limit: usize,

    /// Output format: text (default) or json (review_summary_v1 envelope)
    #[arg(long)]
    pub format: Option<String>,
}

#[derive(Args)]
pub struct ReviewAssignOwnerArgs {
    pub observation_id: String,

    /// Fix-owner repository (id, alias, local path, or `current`)
    pub repository: String,

    #[arg(long)]
    pub reviewer: Option<String>,

    #[arg(long)]
    pub session_id: Option<String>,

    #[arg(long)]
    pub idempotency_key: Option<String>,
}

#[derive(Args)]
pub struct ReviewDispositionArgs {
    pub observation_id: String,

    /// One of: confirmed, duplicate, expected-behavior, environmental,
    /// insufficient-evidence, deferred, superseded
    pub disposition: String,

    /// Target observation (required for `duplicate`)
    #[arg(long = "of")]
    pub of: Option<String>,

    /// Successor observation (required for `superseded`)
    #[arg(long = "by")]
    pub by: Option<String>,

    #[arg(long)]
    pub rationale: Option<String>,

    #[arg(long)]
    pub evidence: Option<String>,

    #[arg(long)]
    pub reviewer: Option<String>,

    #[arg(long)]
    pub session_id: Option<String>,

    #[arg(long)]
    pub idempotency_key: Option<String>,
}

#[derive(Args)]
pub struct ReviewReopenArgs {
    pub observation_id: String,

    #[arg(long)]
    pub rationale: Option<String>,

    #[arg(long)]
    pub reviewer: Option<String>,

    #[arg(long)]
    pub session_id: Option<String>,

    #[arg(long)]
    pub idempotency_key: Option<String>,
}

#[derive(Args)]
pub struct ReviewRelateArgs {
    pub left: String,
    pub right: String,

    /// One of: same-finding, duplicate-of, upstream-cause,
    /// downstream-symptom, related, supersedes
    #[arg(long)]
    pub relation: String,

    #[arg(long)]
    pub rationale: Option<String>,

    #[arg(long)]
    pub evidence: Option<String>,

    #[arg(long)]
    pub reviewer: Option<String>,

    #[arg(long)]
    pub session_id: Option<String>,

    #[arg(long)]
    pub idempotency_key: Option<String>,
}

#[derive(Args)]
pub struct ReviewUnrelateArgs {
    pub relationship_id: String,

    #[arg(long)]
    pub rationale: Option<String>,

    #[arg(long)]
    pub reviewer: Option<String>,

    #[arg(long)]
    pub session_id: Option<String>,

    #[arg(long)]
    pub idempotency_key: Option<String>,
}

#[derive(Args)]
pub struct ReviewPromoteArgs {
    pub observation_id: String,

    #[arg(long)]
    pub finding_id: String,

    #[arg(long)]
    pub reviewer: Option<String>,

    #[arg(long)]
    pub session_id: Option<String>,

    #[arg(long)]
    pub idempotency_key: Option<String>,
}

#[derive(Args)]
pub struct ReviewAttachTaskArgs {
    pub observation_id: String,

    #[arg(long)]
    pub task_id: String,

    #[arg(long)]
    pub reviewer: Option<String>,

    #[arg(long)]
    pub session_id: Option<String>,

    #[arg(long)]
    pub idempotency_key: Option<String>,
}

#[derive(Args)]
pub struct ReviewAttachFixArgs {
    pub observation_id: String,

    /// The candidate fixing commit SHA
    #[arg(long)]
    pub commit: String,

    #[arg(long)]
    pub repo: String,

    #[arg(long)]
    pub reviewer: Option<String>,

    #[arg(long)]
    pub session_id: Option<String>,

    #[arg(long)]
    pub idempotency_key: Option<String>,
}

#[derive(Args)]
pub struct ReviewAttachVerificationArgs {
    pub observation_id: String,

    /// Verification receipt reference
    #[arg(long)]
    pub receipt: String,

    /// accepted | rejected | abstained | invalid | unknown
    #[arg(long)]
    pub status: String,

    #[arg(long)]
    pub reviewer: Option<String>,

    #[arg(long)]
    pub session_id: Option<String>,

    #[arg(long)]
    pub idempotency_key: Option<String>,
}

#[derive(Args)]
pub struct ReviewMarkHandledArgs {
    pub observation_id: String,

    #[arg(long)]
    pub rationale: Option<String>,

    #[arg(long)]
    pub reviewer: Option<String>,

    #[arg(long)]
    pub session_id: Option<String>,

    #[arg(long)]
    pub idempotency_key: Option<String>,
}

#[derive(Args)]
pub struct ReviewReopenRemediationArgs {
    pub observation_id: String,

    #[arg(long)]
    pub rationale: Option<String>,

    #[arg(long)]
    pub reviewer: Option<String>,

    #[arg(long)]
    pub session_id: Option<String>,

    #[arg(long)]
    pub idempotency_key: Option<String>,
}

#[derive(Args)]
pub struct ReviewShowArgs {
    pub observation_id: String,

    #[arg(long)]
    pub format: Option<String>,
}

#[derive(Args)]
pub struct ReviewHistoryArgs {
    pub observation_id: String,

    #[arg(long)]
    pub format: Option<String>,
}

#[derive(Args)]
pub struct ReviewVerifyReportArgs {
    /// Path to the completion report (YAML or JSON)
    pub report: std::path::PathBuf,
}

/// CLI value for the canonical `--work-status` filter. Parses to the
/// reducer's `WorkStatus`; invalid values are rejected at the argument
/// boundary so filters never silently match nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkStatusArg(pub crate::remediation::reducer::WorkStatus);

impl std::str::FromStr for WorkStatusArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "actionable" => Ok(WorkStatusArg(
                crate::remediation::reducer::WorkStatus::Actionable,
            )),
            "active" => Ok(WorkStatusArg(
                crate::remediation::reducer::WorkStatus::Active,
            )),
            "resolved" => Ok(WorkStatusArg(
                crate::remediation::reducer::WorkStatus::Resolved,
            )),
            "terminal" => Ok(WorkStatusArg(
                crate::remediation::reducer::WorkStatus::Terminal,
            )),
            other => Err(format!(
                "invalid work status {other:?} (expected actionable|active|resolved|terminal)"
            )),
        }
    }
}
