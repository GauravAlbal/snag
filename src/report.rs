use crate::artifacts::ArtifactStorage;
use crate::cli::ReportArgs;
use crate::context::gather_context;
use crate::error::SnagError;
use crate::parser::{
    JsonInput, MAX_ARTIFACTS, MAX_INTAKE_BYTES, MAX_REPOSITORIES, MAX_STRING_BYTES, parse_prose,
    read_bounded, validate_json_input, validate_json_nesting, validate_prose, validate_string,
};
use crate::store::Store;
use crate::types::{ArtifactReference, Observation, generate_id};
use anyhow::Result;
use serde_json::json;
use std::io;
use std::path::{Path, PathBuf};

/// Crash-injection failpoint (T6): see `crate::failpoint::failpoint`.
#[derive(Debug)]
enum OwnershipDeclaration {
    Repository(String),
    Unowned,
}

impl OwnershipDeclaration {
    fn repository(&self) -> Option<&str> {
        match self {
            Self::Repository(repository) => Some(repository),
            Self::Unowned => None,
        }
    }

    fn was_explicitly_unowned(&self) -> bool {
        matches!(self, Self::Unowned)
    }
}

fn ownership_declaration(
    owner: Option<String>,
    unowned: Option<bool>,
) -> Result<OwnershipDeclaration> {
    match (owner, unowned) {
        (Some(owner), None) if !owner.trim().is_empty() => {
            Ok(OwnershipDeclaration::Repository(owner.trim().to_string()))
        }
        (Some(_), None) => Err(SnagError::Validation(
            "--owner must name a non-empty repository; pass --owner <repository> or --unowned"
                .to_string(),
        )
        .into()),
        (None, Some(true)) => Ok(OwnershipDeclaration::Unowned),
        (None, Some(false)) => Err(SnagError::Validation(
            "an explicit unowned declaration must be true; pass --owner <repository> or --unowned"
                .to_string(),
        )
        .into()),
        _ => Err(SnagError::Validation(
            "ownership is required: pass --owner <repository> or --unowned".to_string(),
        )
        .into()),
    }
}

/// Parsed report inputs after CLI/JSON/prose merging (CLI explicit flags win).
struct ReportInputs {
    title: String,
    summary: Option<String>,
    kind: Option<String>,
    severity: Option<String>,
    expected_behavior: Option<String>,
    observed_behavior: Option<String>,
    repro: Option<String>,
    workaround: Option<String>,
    impact: Option<String>,
    confidence: Option<f64>,
    sensitivity: Option<String>,
    labels: Option<std::collections::BTreeMap<String, String>>,
    idempotency_key: Option<String>,
    affected_repos: Vec<String>,
    ownership: OwnershipDeclaration,
    source_override: Option<crate::types::SourceInfo>,
    context_override: Option<crate::types::ContextInfo>,
    artifact_paths: Vec<PathBuf>,
}

struct ReportInputDraft {
    title: Option<String>,
    summary: Option<String>,
    kind: Option<String>,
    severity: Option<String>,
    expected_behavior: Option<String>,
    observed_behavior: Option<String>,
    repro: Option<String>,
    workaround: Option<String>,
    impact: Option<String>,
    confidence: Option<f64>,
    sensitivity: Option<String>,
    labels: Option<std::collections::BTreeMap<String, String>>,
    idempotency_key: Option<String>,
    affected_repos: Vec<String>,
    owner: Option<String>,
    unowned: Option<bool>,
    invalid_unowned: Option<String>,
    source_override: Option<crate::types::SourceInfo>,
    context_override: Option<crate::types::ContextInfo>,
    artifact_paths: Vec<PathBuf>,
}

impl ReportInputDraft {
    fn from_args(args: &ReportArgs) -> Self {
        Self {
            title: args.title.clone(),
            summary: None,
            kind: args.kind.clone(),
            severity: args.severity.clone(),
            expected_behavior: args.expected.clone(),
            observed_behavior: args.observed.clone(),
            repro: args.repro.clone(),
            workaround: args.workaround.clone(),
            impact: None,
            confidence: None,
            sensitivity: None,
            labels: None,
            idempotency_key: args.idempotency_key.clone(),
            affected_repos: args.affected_repos.clone(),
            owner: None,
            unowned: None,
            invalid_unowned: None,
            source_override: None,
            context_override: None,
            artifact_paths: args.artifacts.clone(),
        }
    }

    fn merge_prose(&mut self, args: &ReportArgs) -> Result<()> {
        if !args.stdin {
            return Ok(());
        }
        let buffer = read_bounded(io::stdin(), MAX_INTAKE_BYTES, "prose report input")
            .map_err(anyhow::Error::from)?;
        let parsed = parse_prose(&buffer);
        validate_prose(&parsed).map_err(anyhow::Error::from)?;
        if !parsed.title.is_empty() && self.title.is_none() {
            self.title = Some(parsed.title);
        }
        macro_rules! take {
            ($dst:expr, $src:expr) => {
                if let Some(value) = $src {
                    $dst = Some(value);
                }
            };
        }
        take!(self.summary, parsed.summary);
        take!(self.expected_behavior, parsed.expected);
        take!(self.observed_behavior, parsed.observed);
        take!(self.repro, parsed.repro);
        take!(self.workaround, parsed.workaround);
        take!(self.impact, parsed.impact);
        take!(self.owner, parsed.owner);
        if let Some(value) = parsed.unowned {
            match value.as_str() {
                "true" => self.unowned = Some(true),
                "false" => self.unowned = Some(false),
                _ => self.invalid_unowned = Some(value),
            }
        }
        Ok(())
    }

    fn merge_json(&mut self, args: &ReportArgs) -> Result<()> {
        if !uses_json_intake(args) {
            return Ok(());
        }
        let path = args.title.clone().unwrap_or_else(|| "-".to_string());
        let buffer = if path == "-" {
            read_bounded(io::stdin(), MAX_INTAKE_BYTES, "JSON report input")
                .map_err(anyhow::Error::from)?
        } else {
            let file = std::fs::File::open(&path).map_err(|error| {
                SnagError::Validation(format!(
                    "Could not read JSON file: {} — with --json, a TITLE that is not an existing file is JSON output; use --stdin for JSON intake on stdin, or a valid file path for file intake",
                    error
                ))
            })?;
            read_bounded(file, MAX_INTAKE_BYTES, "JSON report input")
                .map_err(anyhow::Error::from)?
        };
        validate_json_nesting(&buffer).map_err(anyhow::Error::from)?;
        let json_input: JsonInput = serde_json::from_str(&buffer)
            .map_err(|error| SnagError::Validation(format!("Invalid JSON: {}", error)))?;
        validate_json_input(&json_input).map_err(anyhow::Error::from)?;
        let schema_version = json_input.schema_version.unwrap_or(1);
        if !matches!(schema_version, 1 | 2) {
            return Err(SnagError::UnsupportedSchema(schema_version.to_string()).into());
        }
        macro_rules! take {
            ($dst:expr, $src:expr) => {
                if let Some(value) = $src {
                    $dst = Some(value);
                }
            };
        }
        take!(self.title, json_input.title);
        take!(self.summary, json_input.summary);
        take!(self.kind, json_input.kind_assertion);
        take!(self.severity, json_input.severity_assertion);
        take!(self.expected_behavior, json_input.expected_behavior);
        take!(self.observed_behavior, json_input.observed_behavior);
        take!(self.repro, json_input.reproduction);
        take!(self.workaround, json_input.workaround);
        take!(self.impact, json_input.impact);
        take!(self.idempotency_key, json_input.idempotency_key);
        if let Some(repositories) = json_input.affected_repositories {
            self.affected_repos = repositories;
        }
        take!(self.confidence, json_input.confidence);
        take!(self.sensitivity, json_input.sensitivity);
        take!(self.labels, json_input.labels);
        take!(self.source_override, json_input.source);
        take!(self.context_override, json_input.context);
        if let Some(artifacts) = json_input.artifacts {
            self.artifact_paths
                .extend(artifacts.into_iter().map(PathBuf::from));
        }
        if schema_version == 2 {
            take!(self.owner, json_input.owner);
            if json_input.unowned.is_some() {
                self.unowned = json_input.unowned;
            }
        }
        Ok(())
    }

    fn apply_cli_overrides(&mut self, args: &ReportArgs) {
        if args.kind.is_some() {
            self.kind = args.kind.clone();
        }
        if args.severity.is_some() {
            self.severity = args.severity.clone();
        }
        if args.owner.is_some() || args.unowned {
            self.owner = args.owner.clone();
            self.unowned = args.unowned.then_some(true);
            self.invalid_unowned = None;
        }
    }

    fn validate(&self) -> Result<()> {
        validate_string("title", self.title.as_deref().unwrap_or_default())
            .map_err(anyhow::Error::from)?;
        for (name, value) in [
            ("kind", self.kind.as_ref()),
            ("severity", self.severity.as_ref()),
            ("summary", self.summary.as_ref()),
            ("expected_behavior", self.expected_behavior.as_ref()),
            ("observed_behavior", self.observed_behavior.as_ref()),
            ("reproduction", self.repro.as_ref()),
            ("workaround", self.workaround.as_ref()),
            ("impact", self.impact.as_ref()),
            ("idempotency_key", self.idempotency_key.as_ref()),
            ("owner", self.owner.as_ref()),
        ] {
            if let Some(value) = value {
                validate_string(name, value).map_err(anyhow::Error::from)?;
            }
        }
        validate_collections(&self.affected_repos, &self.artifact_paths)?;
        validate_vocabulary(self.kind.as_deref(), self.severity.as_deref())?;
        if self.title.as_deref().unwrap_or_default().is_empty() {
            return Err(SnagError::Validation("Title is required".to_string()).into());
        }
        if self.invalid_unowned.is_some() {
            return Err(SnagError::Validation(
                "the prose Unowned section must contain true".to_string(),
            )
            .into());
        }
        Ok(())
    }

    fn finish(self) -> Result<ReportInputs> {
        let ownership = ownership_declaration(self.owner, self.unowned)?;
        Ok(ReportInputs {
            title: self.title.unwrap_or_default(),
            summary: self.summary,
            kind: self.kind,
            severity: self.severity,
            expected_behavior: self.expected_behavior,
            observed_behavior: self.observed_behavior,
            repro: self.repro,
            workaround: self.workaround,
            impact: self.impact,
            confidence: self.confidence,
            sensitivity: self.sensitivity,
            labels: self.labels,
            idempotency_key: self.idempotency_key,
            affected_repos: self.affected_repos,
            ownership,
            source_override: self.source_override,
            context_override: self.context_override,
            artifact_paths: self.artifact_paths,
        })
    }
}

fn uses_json_intake(args: &ReportArgs) -> bool {
    args.json
        && !args.stdin
        && (args.title.is_none()
            || args.title.as_deref() == Some("-")
            || Path::new(args.title.as_deref().unwrap()).is_file())
}

fn validate_collections(affected_repos: &[String], artifact_paths: &[PathBuf]) -> Result<()> {
    if affected_repos.len() > MAX_REPOSITORIES {
        return Err(SnagError::Validation(format!(
            "affected repositories exceed the {MAX_REPOSITORIES}-item limit"
        ))
        .into());
    }
    for value in affected_repos {
        validate_string("affected repository", value).map_err(anyhow::Error::from)?;
    }
    if artifact_paths.len() > MAX_ARTIFACTS {
        return Err(SnagError::Validation(format!(
            "artifacts exceed the {MAX_ARTIFACTS}-item limit"
        ))
        .into());
    }
    for value in artifact_paths {
        if value.to_string_lossy().len() > MAX_STRING_BYTES {
            return Err(SnagError::Validation(format!(
                "artifact path exceeds the {MAX_STRING_BYTES}-byte string limit"
            ))
            .into());
        }
    }
    Ok(())
}

fn validate_vocabulary(kind: Option<&str>, severity: Option<&str>) -> Result<()> {
    if let Some(kind) = kind
        && !crate::parser::KINDS.contains(&kind)
    {
        return Err(SnagError::Validation(format!(
            "invalid --kind '{}'; allowed: {}",
            kind,
            crate::parser::KINDS.join("|")
        ))
        .into());
    }
    if let Some(severity) = severity
        && !crate::parser::SEVERITIES.contains(&severity)
    {
        return Err(SnagError::Validation(format!(
            "invalid --severity '{}'; allowed: {}",
            severity,
            crate::parser::SEVERITIES.join("|")
        ))
        .into());
    }
    Ok(())
}

fn parse_inputs(args: &ReportArgs) -> Result<ReportInputs> {
    let mut draft = ReportInputDraft::from_args(args);
    draft.merge_prose(args)?;
    draft.merge_json(args)?;
    draft.apply_cli_overrides(args);
    draft.validate()?;
    draft.finish()
}

/// Apply explicit source/context overrides from JSON input onto gathered
/// context, preserving auto-detected repository identity unless replaced.
fn apply_overrides(
    mut source: crate::types::SourceInfo,
    mut context: crate::types::ContextInfo,
    source_override: Option<crate::types::SourceInfo>,
    context_override: Option<crate::types::ContextInfo>,
) -> (crate::types::SourceInfo, crate::types::ContextInfo) {
    if let Some(src) = source_override {
        source = src;
    }
    if let Some(ctxt) = context_override {
        if let Some(exec) = ctxt.execution
            && let Some(cur) = context.execution.as_mut()
        {
            if exec.workspace_id.is_some() {
                cur.workspace_id = exec.workspace_id;
            }
            if exec.program_id.is_some() {
                cur.program_id = exec.program_id;
            }
            if exec.session_id.is_some() {
                cur.session_id = exec.session_id;
            }
            if exec.task_id.is_some() {
                cur.task_id = exec.task_id;
            }
            if exec.attempt_id.is_some() {
                cur.attempt_id = exec.attempt_id;
            }
        }
        if ctxt.extra.is_some() {
            context.extra = ctxt.extra;
        }
    }
    (source, context)
}

fn ingest_artifacts<'a>(
    artifact_storage: &'a ArtifactStorage,
    artifact_paths: &[PathBuf],
) -> Result<(
    Vec<ArtifactReference>,
    crate::artifacts::ArtifactAttempt<'a>,
)> {
    artifact_storage.preflight(artifact_paths)?;
    let mut attempt = artifact_storage.begin_attempt();
    let mut artifacts = Vec::with_capacity(artifact_paths.len());
    let mut total_artifact_bytes = 0_u64;
    for artifact_path in artifact_paths {
        let (digest, size) = attempt.ingest_file(artifact_path)?;
        total_artifact_bytes = total_artifact_bytes.checked_add(size).ok_or_else(|| {
            anyhow::Error::from(SnagError::ArtifactTooLarge(
                "aggregate artifact size overflows".to_string(),
            ))
        })?;
        if total_artifact_bytes > crate::artifacts::MAX_ARTIFACT_BYTES {
            return Err(SnagError::ArtifactTooLarge(
                "total artifacts size exceeds 250 MiB limit".to_string(),
            )
            .into());
        }
        let name = artifact_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        artifacts.push(ArtifactReference {
            digest,
            byte_length: size,
            media_type: None,
            original_name: name,
            created_at: time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap(),
        });
    }
    Ok((artifacts, attempt))
}

fn context_git_context(context: &crate::types::ContextInfo) -> crate::git::GitContext {
    let mut git_ctx = crate::git::GitContext::default();
    if let Some(repo_ctx) = &context.repository {
        git_ctx.git_common_dir = repo_ctx.git_common_dir.clone();
        git_ctx.git_remote_aliases = repo_ctx.git_remote_aliases.clone();
        git_ctx.repository_root = repo_ctx.repository_root.clone();
    }
    git_ctx
}

fn owner_git_context(
    context: &crate::types::ContextInfo,
    owner: &str,
) -> Result<Option<crate::git::GitContext>> {
    if owner == "current" {
        return Ok(Some(context_git_context(context)));
    }
    let path = Path::new(owner);
    if path.exists() {
        return Ok(Some(crate::git::collect_git_context(path)?));
    }
    Ok(None)
}

/// Resolve the reporter repository (explicit-ID precedence) and every affected
/// repository before the observation transaction. Owner resolution is deferred
/// until that transaction so identity materialization rolls back with a failed
/// report.
/// Returns (reporter_repo_id, resolved_affected).
fn resolve_identity(
    store: &mut Store,
    context: &mut crate::types::ContextInfo,
    affected_repos: &[String],
) -> Result<(Option<String>, Vec<String>)> {
    let mut primary_repo_id: Option<String> = None;
    let temp_git = context_git_context(context);
    if let Some(repo_ctx) = context.repository.as_mut() {
        let res = crate::identity::resolve_repository(
            store,
            &temp_git,
            repo_ctx.repository_id.as_deref(),
        )?;
        if !res.repository_id.is_empty() {
            repo_ctx.repository_id = Some(res.repository_id.clone());
            repo_ctx.checkout_id = res.checkout_id;
            repo_ctx.worktree_id = res.worktree_id;
            primary_repo_id = Some(res.repository_id);
            for w in &res.warnings {
                eprintln!("snag: {}", w);
            }
        }
    }
    let mut resolved_affected: Vec<String> = Vec::new();
    for raw in affected_repos {
        let rid = crate::identity::resolve_affected_repository(store, raw, &temp_git)?;
        if !resolved_affected.contains(&rid) {
            resolved_affected.push(rid);
        }
    }
    Ok((primary_repo_id, resolved_affected))
}

/// Outcome of the idempotency check for a report attempt.
enum ReplayOutcome {
    Replayed,
    Proceed,
}

/// G32: same key + same semantic digest replays the original observation;
/// same key + different digest is a typed conflict.
fn try_idempotency_replay(
    tx: &rusqlite::Transaction,
    idempotency_key: &Option<String>,
    semantic_digest: &str,
    store_id: &str,
    want_json: bool,
    obs: &Observation,
) -> Result<ReplayOutcome> {
    let Some(ik) = idempotency_key else {
        return Ok(ReplayOutcome::Proceed);
    };
    let existing: rusqlite::Result<(String, i64, String, Option<String>)> = tx.query_row(
        "SELECT o.observation_id, o.local_sequence, o.record_hash, o.semantic_digest
         FROM observations o
         WHERE o.idempotency_key = ?1 AND o.semantic_digest IS NOT NULL
         LIMIT 1",
        rusqlite::params![ik],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    );
    if let Ok((old_id, old_seq, old_hash, old_digest)) = existing {
        match old_digest {
            Some(d) if d == semantic_digest => {
                if want_json {
                    let result = json!({
                        "schema_version": 1,
                        "observation_id": old_id,
                        "store_id": store_id,
                        "local_sequence": old_seq,
                        "record_hash": old_hash,
                        "created": false,
                        "idempotent_replay": true,
                        "sync_state": "local",
                        "context": {
                            "repository": obs.context.repository.is_some(),
                            "execution": obs.context.execution.is_some(),
                        },
                        "artifacts": obs.artifacts.len(),
                        "warnings": []
                    });
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!(
                        "Observation already exists: {}  [sequence {}]",
                        old_id, old_seq
                    );
                }
                return Ok(ReplayOutcome::Replayed);
            }
            _ => {
                return Err(SnagError::IdempotencyConflict(format!(
                    "idempotency key {} already used with a different semantic payload",
                    ik
                ))
                .into());
            }
        }
    }
    Ok(ReplayOutcome::Proceed)
}

/// Hash-chain values recorded alongside a created observation.
struct RecordHashBundle<'a> {
    canonical_payload: &'a str,
    previous_record_hash: &'a str,
    record_hash: &'a str,
    semantic_digest: &'a str,
}

/// Insert the records row, normalized observation, artifacts, and repository
/// relationships for a newly created observation.
fn insert_created_observation(
    tx: &rusqlite::Transaction,
    obs: &Observation,
    hash: &RecordHashBundle<'_>,
    primary_repo_id: Option<&str>,
    resolved_affected: &[String],
    resolved_owner: Option<&str>,
) -> Result<()> {
    tx.execute(
        "INSERT INTO records (local_sequence, record_id, record_type, entity_id, captured_at, canonical_payload_json, previous_record_hash, record_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            obs.local_sequence as i64,
            &obs.observation_id,
            "observation_created",
            &obs.observation_id,
            &obs.created_at,
            hash.canonical_payload,
            hash.previous_record_hash,
            hash.record_hash,
        ],
    )?;
    crate::failpoint::failpoint("after_record_insert");

    tx.execute(
        "INSERT INTO observations (
            observation_id, store_id, local_sequence, schema_version, captured_at, source_kind,
            idempotency_key, title, summary, kind_assertion, severity_assertion, expected_behavior,
            observed_behavior, reproduction, workaround, impact, confidence, sensitivity, context_json,
            canonical_payload_json, previous_record_hash, record_hash, semantic_digest, labels_json
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24
        )",
        rusqlite::params![
            &obs.observation_id,
            &obs.store_id,
            obs.local_sequence as i64,
            obs.schema_version,
            &obs.created_at,
            &obs.source.kind,
            &obs.idempotency_key,
            &obs.title,
            &obs.summary,
            &obs.kind_assertion,
            &obs.severity_assertion,
            &obs.expected_behavior,
            &obs.observed_behavior,
            &obs.reproduction,
            &obs.workaround,
            &obs.impact,
            obs.confidence,
            serde_json::from_str::<String>(&serde_json::to_string(&obs.sensitivity)?).unwrap_or_default(),
            serde_json::to_string(&obs.context)?,
            hash.canonical_payload,
            hash.previous_record_hash,
            hash.record_hash,
            hash.semantic_digest,
            serde_json::to_string(&obs.labels).unwrap_or_else(|_| "null".to_string()),
        ],
    )?;
    crate::failpoint::failpoint("after_obs_insert");

    for art in &obs.artifacts {
        tx.execute(
            "INSERT OR IGNORE INTO artifacts (digest, byte_length, media_type, original_name, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                &art.digest,
                art.byte_length as i64,
                &art.media_type,
                &art.original_name,
                &art.created_at,
            ],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO observation_artifacts (observation_id, digest) VALUES (?1, ?2)",
            rusqlite::params![&obs.observation_id, &art.digest],
        )?;
    }
    if let Some(p) = primary_repo_id {
        tx.execute(
            "INSERT OR IGNORE INTO observation_repositories (observation_id, repository_id, role) VALUES (?1, ?2, 'reporter')",
            rusqlite::params![&obs.observation_id, p],
        )?;
    }
    if let Some(o) = resolved_owner {
        tx.execute(
            "INSERT OR IGNORE INTO observation_repositories (observation_id, repository_id, role) VALUES (?1, ?2, 'owner')",
            rusqlite::params![&obs.observation_id, o],
        )?;
    }
    for repo_id in resolved_affected {
        tx.execute(
            "INSERT OR IGNORE INTO observation_repositories (observation_id, repository_id, role) VALUES (?1, ?2, 'affected')",
            rusqlite::params![&obs.observation_id, repo_id],
        )?;
    }
    crate::failpoint::failpoint("after_artifacts");
    Ok(())
}

fn emit_response(want_json: bool, obs: &Observation, record_hash: &str) -> Result<()> {
    let repro_key = obs
        .labels
        .as_ref()
        .and_then(|l| l.get("repro_key"))
        .cloned();
    if want_json {
        let mut result = json!({
            "schema_version": 1,
            "observation_id": obs.observation_id,
            "store_id": obs.store_id,
            "local_sequence": obs.local_sequence,
            "record_hash": record_hash,
            "created": true,
            "sync_state": "local",
            "context": {
                "repository": obs.context.repository.is_some(),
                "execution": obs.context.execution.is_some(),
            },
            "artifacts": obs.artifacts.len(),
            "warnings": []
        });
        if let Some(k) = &repro_key {
            result["repro_key"] = serde_json::json!(k);
        }
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "Recorded {}  [sequence {}]",
            obs.observation_id, obs.local_sequence
        );
        println!("artifacts: {}", obs.artifacts.len());
        println!("sync: local");
        if let Some(k) = &repro_key {
            println!("repro key: {}", k);
            println!(
                "  record this key in the session notes so a session-search tool can localize this observation's filing session"
            );
        }
        // Severity microcopy: a high-severity assertion with a thin body is
        // the classic inflation signal — the queue trusts the assertion as a
        // prior, so the body is what lets the reviewer re-rank honestly.
        let high_severity = matches!(
            obs.severity_assertion.as_deref(),
            Some("major" | "medium" | "blocker")
        );
        if high_severity
            && obs.expected_behavior.is_none()
            && obs.observed_behavior.is_none()
            && obs.reproduction.is_none()
        {
            println!(
                "note: high severity with a thin body — add expected/observed/repro so the queue can prioritize honestly (severity is a prior)"
            );
        }
    }
    Ok(())
}

pub fn handle(args: ReportArgs) -> Result<()> {
    let inputs = parse_inputs(&args)?;

    // 2. Gather Context, then apply explicit overrides.
    let (source, context, gathered_idempotency_key) = gather_context(&args)?;
    let idempotency_key = inputs.idempotency_key.clone().or(gathered_idempotency_key);
    let (source, mut context) = apply_overrides(
        source,
        context,
        inputs.source_override.clone(),
        inputs.context_override.clone(),
    );

    // 3. Artifact storage setup + ingestion.
    let mut store = Store::open_read_write()?;
    let artifact_storage = ArtifactStorage::new(&store.data_dir)?;
    let (artifacts, artifact_attempt) =
        ingest_artifacts(&artifact_storage, &inputs.artifact_paths)?;

    // Reporter and affected identities may be resolved before the transaction;
    // owner materialization is deliberately deferred until it can roll back
    // with the observation.
    let (primary_repo_id, resolved_affected) =
        resolve_identity(&mut store, &mut context, &inputs.affected_repos)?;
    let owner_input = inputs.ownership.repository().map(str::to_string);
    let owner_git_ctx = owner_input
        .as_deref()
        .map(|owner| owner_git_context(&context, owner))
        .transpose()?
        .flatten();

    // 4. Begin Transaction and allocate.
    crate::failpoint::failpoint("before_tx");
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let resolved_owner = owner_input
        .as_deref()
        .map(|owner| {
            crate::identity::resolve_assignment_repository(&tx, owner, owner_git_ctx.as_ref(), &now)
        })
        .transpose()?;
    let local_sequence: i64 = tx.query_row(
        "SELECT COALESCE(MAX(local_sequence), 0) + 1 FROM records",
        [],
        |row| row.get(0),
    )?;
    let previous_record_hash: String = tx
        .query_row(
            "SELECT record_hash FROM records ORDER BY local_sequence DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| {
            "0000000000000000000000000000000000000000000000000000000000000000".to_string()
        });
    crate::failpoint::failpoint("after_seq");

    let obs_id = generate_id("obs");
    let mut obs = Observation {
        schema_version: 1,
        observation_id: obs_id.clone(),
        store_id: store.store_id.clone(),
        local_sequence: local_sequence as u64,
        idempotency_key: idempotency_key.clone(),
        created_at: now.clone(),
        source,
        title: inputs.title,
        summary: inputs.summary,
        kind_assertion: inputs.kind,
        severity_assertion: inputs.severity,
        expected_behavior: inputs.expected_behavior,
        observed_behavior: inputs.observed_behavior,
        reproduction: inputs.repro,
        workaround: inputs.workaround,
        impact: inputs.impact,
        confidence: inputs.confidence,
        sensitivity: crate::parser::sensitivity_from_str(inputs.sensitivity.as_deref()),
        labels: inputs.labels,
        context,
        artifacts: artifacts.clone(),
        affected_repository_ids: inputs.affected_repos,
        owner_repository_id: resolved_owner.clone(),
        owner_was_explicitly_unowned: inputs.ownership.was_explicitly_unowned(),
    };

    // repro_key: a deterministic, store-scoped hash key that localizes this
    // observation's filing session. Derived from the semantic digest
    // (stable across idempotent replays), attached as a snag-owned label so
    // it flows into the canonical payload and the agent packet, and printed
    // at filing so the reporter can echo it into the session — that line is
    // a session-search tool indexes that line verbatim.
    // The digest function strips repro_key so tooling metadata never perturbs
    // idempotency semantics.
    let semantic_digest = crate::idempotency::observation_semantic_digest(&obs);
    let repro_key = {
        let mut h = blake3::Hasher::new();
        h.update(store.store_id.as_bytes());
        h.update(b"|");
        h.update(semantic_digest.as_bytes());
        h.finalize().to_hex()[..24].to_string()
    };
    let mut labels = obs.labels.clone().unwrap_or_default();
    labels.insert("repro_key".to_string(), repro_key);
    obs.labels = Some(labels);

    use crate::record::{CanonicalRecordV1, RecordPayload};
    let canonical_record = CanonicalRecordV1 {
        local_sequence: local_sequence as u64,
        record_id: obs.observation_id.clone(),
        record_type: "observation_created".to_string(),
        entity_id: obs.observation_id.clone(),
        captured_at: obs.created_at.clone(),
        payload: RecordPayload::Observation(obs.clone()),
    };
    let canonical_payload = serde_json::to_string(&canonical_record.payload)?;

    if matches!(
        try_idempotency_replay(
            &tx,
            &idempotency_key,
            &semantic_digest,
            &store.store_id,
            args.json,
            &obs,
        )?,
        ReplayOutcome::Replayed
    ) {
        return Ok(());
    }

    let record_hash = canonical_record.compute_hash(&store.store_id, &previous_record_hash);
    let hash = RecordHashBundle {
        canonical_payload: &canonical_payload,
        previous_record_hash: &previous_record_hash,
        record_hash: &record_hash,
        semantic_digest: &semantic_digest,
    };
    insert_created_observation(
        &tx,
        &obs,
        &hash,
        primary_repo_id.as_deref(),
        &resolved_affected,
        resolved_owner.as_deref(),
    )?;

    tx.commit()?;
    artifact_attempt.commit();

    // Crash after the transaction is committed but before the response is
    // written: the observation must be durably present.
    crate::failpoint::failpoint("after_commit");

    emit_response(args.json, &obs, &record_hash)
}

fn list_json(rows: &mut rusqlite::Rows) -> anyhow::Result<()> {
    let mut obs = Vec::new();
    while let Some(row) = rows.next()? {
        let observation_id: String = row.get(0)?;
        let local_sequence: i64 = row.get(1)?;
        let captured_at: String = row.get(2)?;
        let title: String = row.get(3)?;
        let retracted: bool = row.get(4)?;
        obs.push(json!({
            "observation_id": observation_id,
            "local_sequence": local_sequence,
            "captured_at": captured_at,
            "title": title,
            "retracted": retracted,
        }));
    }
    // G36: JSON uses a versioned envelope, not a raw array.
    let envelope = json!({
        "schema_version": 1,
        "count": obs.len(),
        "observations": obs,
    });
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    Ok(())
}

fn list_table(rows: &mut rusqlite::Rows) -> anyhow::Result<()> {
    let mut data: Vec<Vec<String>> = Vec::new();
    while let Some(row) = rows.next()? {
        let observation_id: String = row.get(0)?;
        let local_sequence: i64 = row.get(1)?;
        let captured_at: String = row.get(2)?;
        let title: String = row.get(3)?;
        let retracted: bool = row.get(4)?;
        let mark = if retracted { "RETRACTED" } else { "" };
        data.push(vec![
            observation_id,
            local_sequence.to_string(),
            captured_at,
            title,
            mark.to_string(),
        ]);
    }
    crate::remediation::render_table(
        &["ID", "Seq", "Date", "Title", "Retracted"],
        &[
            crate::remediation::TableAlign::Left,
            crate::remediation::TableAlign::Right,
            crate::remediation::TableAlign::Left,
            crate::remediation::TableAlign::Left,
            crate::remediation::TableAlign::Left,
        ],
        &data,
    );
    Ok(())
}

/// Parse a relative duration like `7d`, `12h`, `30m`, `45s` into seconds, with
/// a typed error for anything malformed (G36).
fn parse_since(s: &str) -> anyhow::Result<i64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.len().saturating_sub(1));
    let n: i64 = num.parse().map_err(|_| {
        crate::error::SnagError::Validation(format!("invalid --since duration: {}", s))
    })?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => {
            return Err(crate::error::SnagError::Validation(format!(
                "invalid --since duration unit: {}",
                s
            ))
            .into());
        }
    };
    Ok(secs)
}

pub fn list(args: crate::cli::ListArgs) -> anyhow::Result<()> {
    if let Some(fmt) = &args.format
        && fmt != "json"
        && fmt != "table"
    {
        return Err(
            crate::error::SnagError::Validation(format!("invalid --format: {}", fmt)).into(),
        );
    }

    let store = Store::open_read_only()?;

    let mut query = String::from(
        "SELECT r.record_id, r.local_sequence, r.captured_at, o.title,
                EXISTS (SELECT 1 FROM records rr WHERE rr.entity_id = o.observation_id AND rr.record_type = 'observation_retracted') AS retracted
         FROM records r 
         JOIN observations o ON r.record_id = o.observation_id 
         WHERE r.record_type = 'observation_created'"
    );

    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(sk) = &args.source {
        query.push_str(" AND o.source_kind = ?");
        params.push(Box::new(sk.clone()));
    }

    if let Some(k) = &args.kind {
        query.push_str(" AND o.kind_assertion = ?");
        params.push(Box::new(k.clone()));
    }

    if let Some(since) = &args.since {
        let secs = parse_since(since)?;
        let cutoff = (time::OffsetDateTime::now_utc() - time::Duration::seconds(secs))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        query.push_str(" AND r.captured_at >= ?");
        params.push(Box::new(cutoff));
    }

    if let Some(repo) = &args.repo {
        // `current` resolves through the actual current context (G36).
        if repo == "current" {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let git_ctx = crate::git::collect_git_context(&cwd).unwrap_or_default();
            let repo_id: Option<String> = if let Some(dir) = &git_ctx.git_common_dir {
                store
                    .conn
                    .query_row(
                        "SELECT repository_id FROM checkouts WHERE git_common_dir = ?1",
                        rusqlite::params![dir],
                        |r| r.get(0),
                    )
                    .ok()
            } else {
                None
            };
            match repo_id {
                Some(rid) => {
                    query.push_str(" AND EXISTS (SELECT 1 FROM observation_repositories u WHERE u.observation_id = o.observation_id AND u.repository_id = ?)");
                    params.push(Box::new(rid));
                }
                None => query.push_str(" AND 0"),
            }
        } else {
            // A repository ID or an unambiguous alias both work.
            query.push_str(" AND EXISTS (SELECT 1 FROM observation_repositories u WHERE u.observation_id = o.observation_id AND (u.repository_id = ? OR u.repository_id IN (SELECT repository_id FROM repository_aliases WHERE alias = ?)))");
            params.push(Box::new(repo.clone()));
            params.push(Box::new(repo.clone()));
        }
    }

    query.push_str(" ORDER BY r.local_sequence DESC");

    if let Some(limit) = args.limit {
        query.push_str(" LIMIT ?");
        params.push(Box::new(limit as i64));
    }

    let mut stmt = store.conn.prepare(&query)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut rows = stmt.query(param_refs.as_slice())?;

    if args.format.as_deref() == Some("json") {
        list_json(&mut rows)?;
    } else {
        list_table(&mut rows)?;
    }
    Ok(())
}

pub fn show(mut args: crate::cli::ShowArgs) -> anyhow::Result<()> {
    let store = Store::open_read_only()?;
    args.observation_id =
        crate::remediation::resolve_observation_id(&store.conn, &args.observation_id)?;
    let payload: String = store.conn.query_row(
        "SELECT canonical_payload_json FROM records WHERE record_id = ?1 AND record_type = 'observation_created'",
        rusqlite::params![&args.observation_id],
        |row| row.get(0),
    )?;

    println!("{}", payload);
    Ok(())
}

pub fn retract(mut args: crate::cli::RetractArgs) -> anyhow::Result<()> {
    let mut store = Store::open_read_write()?;
    args.observation_id =
        crate::remediation::resolve_observation_id(&store.conn, &args.observation_id)?;
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM observations WHERE observation_id = ?1)",
        rusqlite::params![&args.observation_id],
        |row| row.get(0),
    )?;

    if !exists {
        anyhow::bail!("Observation not found");
    }

    let local_sequence: i64 = tx.query_row(
        "SELECT COALESCE(MAX(local_sequence), 0) + 1 FROM records",
        [],
        |row| row.get(0),
    )?;

    let previous_record_hash: String = tx
        .query_row(
            "SELECT record_hash FROM records ORDER BY local_sequence DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| {
            "0000000000000000000000000000000000000000000000000000000000000000".to_string()
        });

    let action_id = generate_id("act");
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let action_type = "retracted";
    use crate::record::{CanonicalRecordV1, RecordPayload, RetractionPayload};

    let canonical_record = CanonicalRecordV1 {
        local_sequence: local_sequence as u64,
        record_id: action_id.clone(),
        record_type: "observation_retracted".to_string(),
        entity_id: args.observation_id.clone(),
        captured_at: now.clone(),
        payload: RecordPayload::Retraction(RetractionPayload {
            reason: "manual retraction".to_string(),
        }),
    };

    let action_payload_json = serde_json::to_string(&canonical_record.payload)?;
    let record_hash = canonical_record.compute_hash(&store.store_id, &previous_record_hash);
    tx.execute(
        "INSERT INTO records (local_sequence, record_id, record_type, entity_id, captured_at, canonical_payload_json, previous_record_hash, record_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            local_sequence,
            &action_id,
            "observation_retracted",
            &args.observation_id,
            &now,
            &action_payload_json,
            &previous_record_hash,
            &record_hash,
        ],
    )?;

    tx.execute(
        "INSERT INTO observation_actions (action_id, observation_id, action_type, action_payload_json, created_at, local_sequence, previous_record_hash, record_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            &action_id,
            &args.observation_id,
            action_type,
            &action_payload_json,
            &now,
            local_sequence,
            &previous_record_hash,
            &record_hash,
        ],
    )?;

    tx.commit()?;

    println!("Retracted {}", args.observation_id);

    Ok(())
}
