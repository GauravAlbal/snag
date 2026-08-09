//! The remediation protocol: queue retrieval, claim leases, append-only
//! adjudication, relationships, and remediation lineage over the global
//! append-only record stream.
//!
//! Event types and the reducer live in `events` and `reducer`; identity
//! resolution and the queue projection live in `identity` and `queue`. This
//! module dispatches the `snag review …` commands and implements the claim
//! lifecycle (leases are transactional database records, never filesystem
//! locks).

pub mod events;
pub mod identity;
pub mod queue;
pub mod reducer;
pub mod report_check;
pub mod verify;

#[cfg(snag_internal)]
#[rustfmt::skip]
#[path = "../../src_internal/retro.rs"]
pub mod retro;

use crate::cli::ReviewCommand;
use crate::error::SnagError;
use crate::record::RecordPayload;
use crate::remediation::events::*;
use crate::remediation::identity::{RemediationIdentity, lease_expiry, resolve_identity, utc_now};
use crate::remediation::queue::{NextFilters, agent_packet, render_next_text};
use crate::remediation::reducer::{STATE_VERIFIED_FIXED, WorkStatus};
use crate::store::Store;
use crate::types::generate_id;
use anyhow::Result;
use rusqlite::OptionalExtension;

/// Dispatch `snag review …`.
pub fn handle_review(cmd: ReviewCommand) -> Result<()> {
    match cmd {
        ReviewCommand::Next(args) => next(args),
        ReviewCommand::Claim(args) => claim(args),
        ReviewCommand::Release(args) => release(args),
        ReviewCommand::Heartbeat(args) => heartbeat(args),
        ReviewCommand::List(args) => list(args),
        ReviewCommand::Summary(args) => summary(args),
        #[cfg(snag_internal)]
        ReviewCommand::Retro(args) => retro::run(args),
        ReviewCommand::AssignOwner(args) => assign_owner(args),
        ReviewCommand::Disposition(args) => disposition(args),
        ReviewCommand::Reopen(args) => reopen(args),
        ReviewCommand::Relate(args) => relate(args),
        ReviewCommand::Unrelate(args) => unrelate(args),
        ReviewCommand::Promote(args) => promote(args),
        ReviewCommand::AttachTask(args) => attach_task(args),
        ReviewCommand::AttachFix(args) => attach_fix(args),
        ReviewCommand::AttachVerification(args) => attach_verification(args),
        ReviewCommand::MarkHandled(args) => mark_handled(args),
        ReviewCommand::ReopenRemediation(args) => reopen_remediation(args),
        ReviewCommand::Show(args) => show(args),
        ReviewCommand::History(args) => history(args),
        ReviewCommand::VerifyReport(args) => verify_report_cmd(args),
    }
}

/// `snag review assign-owner <observation-id> <repository>` — append an
/// authoritative ownership transition and refresh its singular projection.
fn assign_owner(mut args: crate::cli::ReviewAssignOwnerArgs) -> Result<()> {
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let repository = args.repository.trim();
    if repository.is_empty() {
        return Err(SnagError::Validation("owner repository must be non-empty".to_string()).into());
    }
    let mut store = Store::open_read_write()?;
    args.observation_id = resolve_observation_id(&store.conn, &args.observation_id)?;
    let now = utc_now();
    let repository_path = std::path::Path::new(repository);
    let owner_git_ctx = if repository == "current" {
        Some(crate::git::collect_git_context(&std::env::current_dir()?)?)
    } else if repository_path.exists() {
        Some(crate::git::collect_git_context(repository_path)?)
    } else {
        None
    };
    let store_id = store.store_id.clone();
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let owner_repository_id = crate::identity::resolve_assignment_repository(
        &tx,
        repository,
        owner_git_ctx.as_ref(),
        &now,
    )?;
    crate::failpoint::failpoint("remediation_before_tx");

    let appended = append_event(
        &tx,
        &store_id,
        RECORD_OWNER_ASSIGNED,
        &args.observation_id,
        RecordPayload::Remediation(RemediationEvent::OwnerAssigned(OwnerAssignedPayload {
            owner_repository_id: owner_repository_id.clone(),
            reviewer: identity.reviewer,
            review_session_id: identity.session_id,
            created_at: now,
            idempotency_key: args.idempotency_key,
        })),
    )?;
    let reduced = crate::remediation::reducer::reduce_observation(&tx, &args.observation_id)?;
    let current_owner = reduced.owner_repository_id.as_deref().ok_or_else(|| {
        SnagError::Validation("owner assignment event did not reduce to an owner".to_string())
    })?;
    tx.execute(
        "DELETE FROM observation_repositories
         WHERE observation_id = ?1 AND role = 'owner'",
        rusqlite::params![&args.observation_id],
    )?;
    tx.execute(
        "INSERT INTO observation_repositories
         (observation_id, repository_id, role)
         VALUES (?1, ?2, 'owner')",
        rusqlite::params![&args.observation_id, current_owner],
    )?;
    crate::remediation::reducer::upsert_review_state(&tx, &reduced)?;
    crate::failpoint::failpoint("remediation_before_commit");
    tx.commit()?;
    crate::failpoint::failpoint("remediation_after_commit");
    println!(
        "Assigned owner {} -> {} (sequence {})",
        args.observation_id, current_owner, appended.local_sequence
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Claims: leases, not ownership.
// ---------------------------------------------------------------------------

/// Outcome of a claim acquisition.
enum ClaimOutcome {
    Acquired {
        claim_id: String,
        lease_expires_at: String,
        record_hash: String,
    },
    /// Same-session re-acquisition: the existing lease stands (idempotent).
    Replayed {
        claim_id: String,
        lease_expires_at: String,
        record_hash: String,
    },
}

/// Expire any lapsed leases on the observation and record the expiry events.
fn expire_lapsed_claims(
    store_id: &str,
    tx: &rusqlite::Transaction,
    observation_id: &str,
    now: &str,
) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT claim_id FROM remediation_claims
         WHERE observation_id = ?1 AND released_at IS NULL AND lease_expires_at <= ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![observation_id, now], |row| {
        row.get::<_, String>(0)
    })?;
    let mut lapsed = Vec::new();
    for r in rows {
        lapsed.push(r?);
    }
    for claim_id in lapsed {
        append_event(
            tx,
            store_id,
            RECORD_CLAIM_EXPIRED,
            observation_id,
            RecordPayload::Remediation(RemediationEvent::ClaimExpired(ClaimExpiredPayload {
                claim_id: claim_id.clone(),
                expired_at: now.to_string(),
                created_at: now.to_string(),
            })),
        )?;
        tx.execute(
            "UPDATE remediation_claims SET released_at = ?1, release_reason = 'lease expired'
             WHERE claim_id = ?2 AND released_at IS NULL",
            rusqlite::params![now, claim_id],
        )?;
    }
    Ok(())
}

/// The shared claim acquisition: transactional lease semantics.
///
/// * unclaimed observation -> new lease;
/// * expired lease -> the old lease is recorded as expired, then a new lease;
/// * active lease owned by another session -> `CLAIM_CONFLICT`;
/// * active lease owned by this session -> idempotent replay.
fn claim_observation(
    store_id: &str,
    tx: &rusqlite::Transaction,
    observation_id: &str,
    identity: &RemediationIdentity,
    lease_seconds: u64,
    task: Option<&str>,
    now: &str,
) -> Result<ClaimOutcome> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM observations WHERE observation_id = ?1)",
        rusqlite::params![observation_id],
        |row| row.get(0),
    )?;
    if !exists {
        anyhow::bail!(SnagError::NotFound(format!("observation {observation_id}")));
    }

    let active: Option<(String, String, String)> = tx
        .query_row(
            "SELECT claim_id, claim_session_id, lease_expires_at FROM remediation_claims
             WHERE observation_id = ?1 AND released_at IS NULL AND lease_expires_at > ?2
             ORDER BY claimed_at DESC LIMIT 1",
            rusqlite::params![observation_id, now],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;

    if let Some((claim_id, session, expires)) = active {
        if session == identity.session_id {
            let record_hash: String = tx.query_row(
                "SELECT record_hash FROM records
                 WHERE entity_id = ?1 AND record_type = 'observation_claimed'
                   AND json_extract(canonical_payload_json, '$.claim_id') = ?2
                 ORDER BY local_sequence ASC LIMIT 1",
                rusqlite::params![observation_id, claim_id],
                |row| row.get(0),
            )?;
            // Fold-in: if the caller asserts owned work, ensure the link
            // exists (additive and idempotent).
            if let Some(t) = task {
                attach_task_if_absent(store_id, tx, observation_id, identity, t, now)?;
            }
            return Ok(ClaimOutcome::Replayed {
                claim_id,
                lease_expires_at: expires,
                record_hash,
            });
        }
        anyhow::bail!(SnagError::ClaimConflict(format!(
            "observation {observation_id} is actively claimed by session {session}"
        )));
    }

    expire_lapsed_claims(store_id, tx, observation_id, now)?;

    let claim_id = generate_id("claim");
    let expires = lease_expiry(now, lease_seconds);
    let appended = append_event(
        tx,
        store_id,
        RECORD_CLAIMED,
        observation_id,
        RecordPayload::Remediation(RemediationEvent::Claimed(ClaimedPayload {
            claim_id: claim_id.clone(),
            claimed_by: identity.reviewer.clone(),
            claim_session_id: identity.session_id.clone(),
            lease_expires_at: expires.clone(),
            created_at: now.to_string(),
            idempotency_key: None,
        })),
    )?;
    tx.execute(
        "INSERT INTO remediation_claims (
            claim_id, observation_id, claimed_by, claim_session_id, claimed_at,
            lease_expires_at, source_record_sequence
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            claim_id,
            observation_id,
            &identity.reviewer,
            &identity.session_id,
            now,
            &expires,
            appended.local_sequence,
        ],
    )?;
    crate::failpoint::failpoint("remediation_after_normalized_insert");

    if let Some(t) = task {
        attach_task_if_absent(store_id, tx, observation_id, identity, t, now)?;
    }

    // Recompute the materialized projection inside the same transaction.
    refresh_review_state(tx, observation_id)?;
    Ok(ClaimOutcome::Acquired {
        claim_id,
        lease_expires_at: expires,
        record_hash: appended.record_hash,
    })
}

/// Attach a task link unless one with the same task id already exists
/// (fold-in: the "fixing in <task>" marker, additive and idempotent).
fn attach_task_if_absent(
    store_id: &str,
    tx: &rusqlite::Transaction,
    observation_id: &str,
    identity: &RemediationIdentity,
    task_id: &str,
    now: &str,
) -> Result<()> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM remediation_links
         WHERE observation_id = ?1 AND link_type = 'task' AND target_id = ?2)",
        rusqlite::params![observation_id, task_id],
        |row| row.get(0),
    )?;
    if exists {
        return Ok(());
    }
    let appended = append_event(
        tx,
        store_id,
        RECORD_TASK_ATTACHED,
        observation_id,
        RecordPayload::Remediation(RemediationEvent::TaskAttached(TaskAttachedPayload {
            task_id: task_id.to_string(),
            reviewer: identity.reviewer.clone(),
            review_session_id: identity.session_id.clone(),
            created_at: now.to_string(),
            idempotency_key: None,
        })),
    )?;
    tx.execute(
        "INSERT INTO remediation_links (
            link_id, observation_id, link_type, target_id, created_at, source_record_sequence
        ) VALUES (?1, ?2, 'task', ?3, ?4, ?5)",
        rusqlite::params![
            appended.record_id,
            observation_id,
            task_id,
            now,
            appended.local_sequence,
        ],
    )?;
    Ok(())
}

/// Recompute the observation's reduced state from the stream and upsert the
/// materialized projection (same transaction as the mutation).
fn refresh_review_state(tx: &rusqlite::Transaction, observation_id: &str) -> Result<()> {
    // The transaction derefs to the connection; the reducer is a pure read of
    // the stream and must not contend with the open write transaction.
    let reduced = crate::remediation::reducer::reduce_observation(tx, observation_id)?;
    crate::remediation::reducer::upsert_review_state(tx, &reduced)?;
    Ok(())
}

/// `snag review claim <observation-id> [--lease 30m] [--task <id>]`
fn claim(mut args: crate::cli::ReviewClaimArgs) -> Result<()> {
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let lease_seconds = match &args.lease {
        Some(raw) => identity::parse_duration(raw).map_err(SnagError::Validation)?,
        None => identity::default_lease_seconds(),
    };
    let mut store = Store::open_read_write()?;
    args.observation_id = resolve_observation_id(&store.conn, &args.observation_id)?;
    let now = utc_now();
    let store_id = store.store_id.clone();
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    crate::failpoint::failpoint("remediation_before_tx");
    let outcome = claim_observation(
        &store_id,
        &tx,
        &args.observation_id,
        &identity,
        lease_seconds,
        args.task.as_deref(),
        &now,
    )?;
    crate::failpoint::failpoint("remediation_before_commit");
    tx.commit()?;
    crate::failpoint::failpoint("remediation_after_commit");
    match outcome {
        ClaimOutcome::Acquired {
            claim_id,
            lease_expires_at,
            record_hash,
        } => {
            println!("claim_id: {}", claim_id);
            println!("observation_id: {}", args.observation_id);
            println!("claimed_by: {}", identity.reviewer);
            println!("session_id: {}", identity.session_id);
            println!("lease_expires_at: {}", lease_expires_at);
            println!("record_hash: {}", record_hash);
            if let Some(t) = &args.task {
                println!("task_id: {}", t);
            }
            println!(
                "lane check: claim only observations in your lane — verify repos/labels before working; release when done"
            );
        }
        ClaimOutcome::Replayed {
            claim_id,
            lease_expires_at,
            record_hash,
        } => {
            println!("claim_id: {} (already held by this session)", claim_id);
            println!("observation_id: {}", args.observation_id);
            println!("lease_expires_at: {}", lease_expires_at);
            println!("record_hash: {}", record_hash);
        }
    }
    crate::failpoint::failpoint("remediation_before_response");
    Ok(())
}

/// `snag review release <observation-id> [--reason ...]`
fn release(mut args: crate::cli::ReviewReleaseArgs) -> Result<()> {
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let mut store = Store::open_read_write()?;
    args.observation_id = resolve_observation_id(&store.conn, &args.observation_id)?;
    let now = utc_now();
    let store_id = store.store_id.clone();
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let active: Option<(String, String)> = tx
        .query_row(
            "SELECT claim_id, claim_session_id FROM remediation_claims
             WHERE observation_id = ?1 AND released_at IS NULL
             ORDER BY claimed_at DESC LIMIT 1",
            rusqlite::params![args.observation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (claim_id, session) = active.ok_or_else(|| {
        SnagError::ClaimConflict(format!(
            "observation {} has no active claim to release",
            args.observation_id
        ))
    })?;
    if session != identity.session_id {
        anyhow::bail!(SnagError::ClaimConflict(format!(
            "observation {} is claimed by session {session}, not {}",
            args.observation_id, identity.session_id
        )));
    }
    let reason = args.reason.unwrap_or_else(|| "released".to_string());
    append_event(
        &tx,
        &store_id,
        RECORD_CLAIM_RELEASED,
        &args.observation_id,
        RecordPayload::Remediation(RemediationEvent::ClaimReleased(ClaimReleasedPayload {
            claim_id: claim_id.clone(),
            released_by: identity.reviewer.clone(),
            release_session_id: identity.session_id.clone(),
            release_reason: reason.clone(),
            released_at: now.clone(),
            created_at: now.clone(),
            idempotency_key: None,
        })),
    )?;
    tx.execute(
        "UPDATE remediation_claims SET released_at = ?1, release_reason = ?2 WHERE claim_id = ?3",
        rusqlite::params![now, reason, claim_id],
    )?;
    refresh_review_state(&tx, &args.observation_id)?;
    tx.commit()?;
    println!("Released {} (claim {})", args.observation_id, claim_id);
    Ok(())
}

/// `snag review heartbeat <observation-id> [--lease 30m]`
fn heartbeat(mut args: crate::cli::ReviewHeartbeatArgs) -> Result<()> {
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let lease_seconds = match &args.lease {
        Some(raw) => identity::parse_duration(raw).map_err(SnagError::Validation)?,
        None => identity::default_lease_seconds(),
    };
    let mut store = Store::open_read_write()?;
    args.observation_id = resolve_observation_id(&store.conn, &args.observation_id)?;
    let now = utc_now();
    let store_id = store.store_id.clone();
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let active: Option<(String, String)> = tx
        .query_row(
            "SELECT claim_id, claim_session_id FROM remediation_claims
             WHERE observation_id = ?1 AND released_at IS NULL AND lease_expires_at > ?2
             ORDER BY claimed_at DESC LIMIT 1",
            rusqlite::params![args.observation_id, now],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (claim_id, session) = active.ok_or_else(|| {
        SnagError::ClaimConflict(format!(
            "observation {} has no active claim to extend",
            args.observation_id
        ))
    })?;
    if session != identity.session_id {
        anyhow::bail!(SnagError::ClaimConflict(format!(
            "observation {} is claimed by session {session}, not {}",
            args.observation_id, identity.session_id
        )));
    }
    let expires = lease_expiry(&now, lease_seconds);
    append_event(
        &tx,
        &store_id,
        RECORD_CLAIM_HEARTBEAT,
        &args.observation_id,
        RecordPayload::Remediation(RemediationEvent::ClaimHeartbeat(ClaimHeartbeatPayload {
            claim_id: claim_id.clone(),
            claimed_by: identity.reviewer.clone(),
            claim_session_id: identity.session_id.clone(),
            lease_expires_at: expires.clone(),
            created_at: now.clone(),
            idempotency_key: None,
        })),
    )?;
    tx.execute(
        "UPDATE remediation_claims SET lease_expires_at = ?1 WHERE claim_id = ?2 AND released_at IS NULL",
        rusqlite::params![expires, claim_id],
    )?;
    refresh_review_state(&tx, &args.observation_id)?;
    tx.commit()?;
    println!(
        "Heartbeat {} (claim {}, expires {})",
        args.observation_id, claim_id, expires
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Dispositions (append-only adjudication).
// ---------------------------------------------------------------------------

/// Check whether adding edge (observation -> target) of the given disposition
/// type would close a cycle among current (non-retracted) directional
/// disposition edges. Adding L->R is a cycle iff R can already reach L, so
/// the walk seeds from the target and tests for the observation.
fn disposition_cycle_would_form(
    tx: &rusqlite::Transaction,
    disposition: &str,
    observation: &str,
    target: &str,
) -> Result<bool> {
    let mut stmt = tx.prepare(
        "WITH RECURSIVE reach(x) AS (
             SELECT target_observation_id FROM observation_dispositions
             WHERE observation_id = ?3 AND disposition = ?1
               AND retracted_by_record_sequence IS NULL AND target_observation_id IS NOT NULL
             UNION
             SELECT d.target_observation_id FROM observation_dispositions d
             JOIN reach r ON d.observation_id = r.x
             WHERE d.disposition = ?1 AND d.retracted_by_record_sequence IS NULL
               AND d.target_observation_id IS NOT NULL
         )
         SELECT 1 FROM reach WHERE x = ?2 LIMIT 1",
    )?;
    let hit = stmt
        .query_row(rusqlite::params![disposition, observation, target], |row| {
            row.get::<_, i64>(0)
        })
        .optional()?;
    Ok(hit.is_some())
}

/// `snag review disposition <observation-id> <disposition> [--of|--by] …`
fn disposition(mut args: crate::cli::ReviewDispositionArgs) -> Result<()> {
    // CLI surface accepts hyphenated forms (`expected-behavior`); the event
    // vocabulary is underscored (`expected_behavior`). Normalize in place so
    // every downstream use (payloads, rows, cycle checks) is normalized.
    args.disposition = args.disposition.replace('-', "_");
    if !DISPOSITIONS.contains(&args.disposition.as_str()) {
        anyhow::bail!(SnagError::Validation(format!(
            "unknown disposition '{}'; allowed: {}",
            args.disposition,
            DISPOSITIONS.join(", ")
        )));
    }
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let mut store = Store::open_read_write()?;
    args.observation_id = resolve_observation_id(&store.conn, &args.observation_id)?;
    let store_id = store.store_id.clone();
    let now = utc_now();
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM observations WHERE observation_id = ?1)",
        rusqlite::params![args.observation_id],
        |row| row.get(0),
    )?;
    if !exists {
        anyhow::bail!(SnagError::NotFound(format!(
            "observation {}",
            args.observation_id
        )));
    }

    // Target validation: duplicate --of, superseded --by, others forbid both.
    let target = match args.disposition.as_str() {
        DISP_DUPLICATE => {
            let t = args.of.ok_or_else(|| {
                SnagError::Validation("duplicate requires --of <observation-id>".to_string())
            })?;
            Some(resolve_observation_id(&tx, &t)?)
        }
        DISP_SUPERSEDED => {
            let t = args.by.ok_or_else(|| {
                SnagError::Validation("superseded requires --by <observation-id>".to_string())
            })?;
            Some(resolve_observation_id(&tx, &t)?)
        }
        _ => {
            if args.of.is_some() || args.by.is_some() {
                anyhow::bail!(SnagError::Validation(
                    "--of/--by are only valid for duplicate/superseded dispositions".to_string()
                ));
            }
            None
        }
    };
    if let Some(t) = &target {
        if t == &args.observation_id {
            anyhow::bail!(SnagError::Validation(
                "an observation cannot be its own target".to_string()
            ));
        }
        let target_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM observations WHERE observation_id = ?1)",
            rusqlite::params![t],
            |row| row.get(0),
        )?;
        if !target_exists {
            anyhow::bail!(SnagError::NotFound(format!("target observation {t}")));
        }
        // Directional terminal chains must stay acyclic.
        if disposition_cycle_would_form(&tx, &args.disposition, &args.observation_id, t)? {
            anyhow::bail!(SnagError::Validation(format!(
                "{} would create a cycle: {} already leads to {}",
                args.disposition, t, args.observation_id
            )));
        }
    }

    // The disposition record's id is bound inside its own payload.
    let disp_id = generate_id("disp");
    let _reviewed = append_event(
        &tx,
        &store_id,
        RECORD_REVIEWED,
        &args.observation_id,
        RecordPayload::Remediation(RemediationEvent::Reviewed(ReviewedPayload {
            disposition: args.disposition.clone(),
            target_observation_id: target.clone(),
            rationale: args.rationale.clone(),
            evidence_json: args.evidence.clone(),
            reviewer: identity.reviewer.clone(),
            review_session_id: identity.session_id.clone(),
            created_at: now.clone(),
            idempotency_key: args.idempotency_key.clone(),
        })),
    )?;
    let disposition_set = append_event_with_id(
        &tx,
        &store_id,
        RECORD_DISPOSITION_SET,
        &args.observation_id,
        RecordPayload::Remediation(RemediationEvent::DispositionSet(DispositionSetPayload {
            disposition_id: disp_id.clone(),
            disposition: args.disposition.clone(),
            target_observation_id: target.clone(),
            rationale: args.rationale.clone(),
            evidence_json: args.evidence.clone(),
            reviewer: identity.reviewer.clone(),
            review_session_id: identity.session_id.clone(),
            created_at: now.clone(),
            idempotency_key: args.idempotency_key.clone(),
        })),
        Some(disp_id.clone()),
    )?;

    if !disposition_set.replayed {
        tx.execute(
            "INSERT INTO observation_dispositions (
                disposition_id, observation_id, disposition, target_observation_id,
                rationale, evidence_json, reviewer, review_session_id, created_at,
                source_record_sequence, idempotency_key
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                disp_id,
                &args.observation_id,
                &args.disposition,
                &target,
                &args.rationale,
                &args.evidence,
                &identity.reviewer,
                &identity.session_id,
                &now,
                disposition_set.local_sequence,
                &args.idempotency_key,
            ],
        )?;
    }
    refresh_review_state(&tx, &args.observation_id)?;
    crate::failpoint::failpoint("remediation_before_commit");
    tx.commit()?;
    crate::failpoint::failpoint("remediation_after_commit");
    println!(
        "Disposition {} -> {} (sequence {})",
        args.observation_id, args.disposition, disposition_set.local_sequence
    );
    // Lane microcopy at the decision point.
    match args.disposition.as_str() {
        DISP_CONFIRMED => {
            println!(
                "lane check: confirmed commits YOUR lane to the fix — if this belongs to another lane, use deferred with the owner lane in --rationale, then reopen-remediation to keep it visible"
            );
        }
        DISP_DEFERRED => {
            println!(
                "ownership: name the owner lane in --rationale; reopen-remediation keeps the observation visible in the queue instead of handled"
            );
        }
        _ => {}
    }
    Ok(())
}

/// `snag review reopen <observation-id> [--rationale …]`
///
/// Reopening is append-only: the earlier disposition events remain, the
/// current disposition row is marked retracted, and the observation returns
/// to the queue.
fn reopen(mut args: crate::cli::ReviewReopenArgs) -> Result<()> {
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let mut store = Store::open_read_write()?;
    args.observation_id = resolve_observation_id(&store.conn, &args.observation_id)?;
    let store_id = store.store_id.clone();
    let now = utc_now();
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let has_disposition: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM observation_dispositions
         WHERE observation_id = ?1 AND retracted_by_record_sequence IS NULL)",
        rusqlite::params![args.observation_id],
        |row| row.get(0),
    )?;
    if !has_disposition {
        anyhow::bail!(SnagError::Validation(format!(
            "observation {} has no current disposition to reopen",
            args.observation_id
        )));
    }

    let appended = append_event(
        &tx,
        &store_id,
        RECORD_REOPENED,
        &args.observation_id,
        RecordPayload::Remediation(RemediationEvent::Reopened(ReopenedPayload {
            rationale: args.rationale.clone(),
            reviewer: identity.reviewer.clone(),
            review_session_id: identity.session_id.clone(),
            created_at: now.clone(),
            idempotency_key: args.idempotency_key.clone(),
        })),
    )?;
    if !appended.replayed {
        tx.execute(
            "UPDATE observation_dispositions SET retracted_by_record_sequence = ?1
             WHERE observation_id = ?2 AND retracted_by_record_sequence IS NULL",
            rusqlite::params![appended.local_sequence, args.observation_id],
        )?;
    }
    refresh_review_state(&tx, &args.observation_id)?;
    tx.commit()?;
    println!(
        "Reopened {} (sequence {})",
        args.observation_id, appended.local_sequence
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Relationships (explicit reviewer assertions).
// ---------------------------------------------------------------------------

/// Canonical endpoint ordering for symmetric relations (left < right by id).
fn canonical_endpoints(relation: &str, left: &str, right: &str) -> (String, String) {
    if SYMMETRIC_RELATIONSHIPS.contains(&relation) && left > right {
        (right.to_string(), left.to_string())
    } else {
        (left.to_string(), right.to_string())
    }
}

/// Check whether adding edge (left -> right) of the given directional relation
/// would close a cycle among current (non-retracted) edges of the same type.
/// Adding L->R is a cycle iff R can already reach L, so the walk seeds from
/// the right endpoint and tests for the left.
fn relationship_cycle_would_form(
    tx: &rusqlite::Transaction,
    relation: &str,
    left: &str,
    right: &str,
) -> Result<bool> {
    let mut stmt = tx.prepare(
        "WITH RECURSIVE reach(x) AS (
             SELECT right_observation_id FROM observation_relationships
             WHERE left_observation_id = ?3 AND relation = ?1
               AND retracted_by_record_sequence IS NULL
             UNION
             SELECT r2.right_observation_id FROM observation_relationships r2
             JOIN reach r ON r2.left_observation_id = r.x
             WHERE r2.relation = ?1 AND r2.retracted_by_record_sequence IS NULL
         )
         SELECT 1 FROM reach WHERE x = ?2 LIMIT 1",
    )?;
    let hit = stmt
        .query_row(rusqlite::params![relation, left, right], |row| {
            row.get::<_, i64>(0)
        })
        .optional()?;
    Ok(hit.is_some())
}

/// `snag review relate <left> <right> --relation <relation> [--rationale …]`
fn relate(mut args: crate::cli::ReviewRelateArgs) -> Result<()> {
    // CLI surface accepts hyphenated forms (`same-finding`); the event
    // vocabulary is underscored (`same_finding`). Normalize in place so every
    // downstream use (payloads, rows, canonical ordering, cycle checks) is
    // normalized.
    args.relation = args.relation.replace('-', "_");
    if !RELATIONSHIPS.contains(&args.relation.as_str()) {
        anyhow::bail!(SnagError::Validation(format!(
            "unknown relation '{}'; allowed: {}",
            args.relation,
            RELATIONSHIPS.join(", ")
        )));
    }
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let mut store = Store::open_read_write()?;
    args.left = resolve_observation_id(&store.conn, &args.left)?;
    args.right = resolve_observation_id(&store.conn, &args.right)?;
    let store_id = store.store_id.clone();
    let now = utc_now();
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    if args.left == args.right {
        anyhow::bail!(SnagError::Validation(
            "an observation cannot relate to itself".to_string()
        ));
    }
    for endpoint in [&args.left, &args.right] {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM observations WHERE observation_id = ?1)",
            rusqlite::params![endpoint],
            |row| row.get(0),
        )?;
        if !exists {
            anyhow::bail!(SnagError::NotFound(format!("observation {endpoint}")));
        }
    }

    let (left, right) = canonical_endpoints(&args.relation, &args.left, &args.right);

    // Duplicate assertion (same canonical endpoints + relation, still live) is
    // idempotent: return the existing relationship, no new event.
    let existing: Option<String> = tx
        .query_row(
            "SELECT relationship_id FROM observation_relationships
             WHERE left_observation_id = ?1 AND right_observation_id = ?2
               AND relation = ?3 AND retracted_by_record_sequence IS NULL
             ORDER BY source_record_sequence ASC LIMIT 1",
            rusqlite::params![left, right, args.relation],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(rid) = existing {
        println!("Relationship already exists: {} (idempotent)", rid);
        tx.commit()?;
        return Ok(());
    }

    if DIRECTIONAL_RELATIONSHIPS.contains(&args.relation.as_str())
        && relationship_cycle_would_form(&tx, &args.relation, &left, &right)?
    {
        anyhow::bail!(SnagError::Validation(format!(
            "{} would create a cycle between {} and {}",
            args.relation, left, right
        )));
    }

    let rel_id = generate_id("rel");
    let appended = append_event_with_id(
        &tx,
        &store_id,
        RECORD_RELATIONSHIP_ADDED,
        &args.left,
        RecordPayload::Remediation(RemediationEvent::RelationshipAdded(
            RelationshipAddedPayload {
                relationship_id: rel_id.clone(),
                left_observation_id: left.clone(),
                right_observation_id: right.clone(),
                relation: args.relation.clone(),
                rationale: args.rationale.clone(),
                evidence_json: args.evidence.clone(),
                reviewer: identity.reviewer.clone(),
                review_session_id: identity.session_id.clone(),
                created_at: now.clone(),
                idempotency_key: args.idempotency_key.clone(),
            },
        )),
        Some(rel_id.clone()),
    )?;
    if !appended.replayed {
        tx.execute(
            "INSERT INTO observation_relationships (
                relationship_id, left_observation_id, right_observation_id, relation,
                rationale, evidence_json, reviewer, review_session_id, created_at,
                source_record_sequence, idempotency_key
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                rel_id,
                left,
                right,
                &args.relation,
                &args.rationale,
                &args.evidence,
                &identity.reviewer,
                &identity.session_id,
                &now,
                appended.local_sequence,
                &args.idempotency_key,
            ],
        )?;
    }
    tx.commit()?;
    println!(
        "Related {} {} {} (sequence {})",
        args.left, args.relation, args.right, appended.local_sequence
    );
    Ok(())
}

/// `snag review unrelate <relationship-id> [--rationale …]` — append-only
/// retraction, no hard delete.
fn unrelate(args: crate::cli::ReviewUnrelateArgs) -> Result<()> {
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let mut store = Store::open_read_write()?;
    let store_id = store.store_id.clone();
    let now = utc_now();
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    let live: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM observation_relationships
         WHERE relationship_id = ?1 AND retracted_by_record_sequence IS NULL)",
        rusqlite::params![args.relationship_id],
        |row| row.get(0),
    )?;
    if !live {
        anyhow::bail!(SnagError::NotFound(format!(
            "relationship {}",
            args.relationship_id
        )));
    }
    let appended = append_event(
        &tx,
        &store_id,
        RECORD_RELATIONSHIP_RETRACTED,
        &args.relationship_id,
        RecordPayload::Remediation(RemediationEvent::RelationshipRetracted(
            RelationshipRetractedPayload {
                relationship_id: args.relationship_id.clone(),
                rationale: args.rationale.clone(),
                reviewer: identity.reviewer.clone(),
                review_session_id: identity.session_id.clone(),
                created_at: now.clone(),
                idempotency_key: args.idempotency_key.clone(),
            },
        )),
    )?;
    if !appended.replayed {
        tx.execute(
            "UPDATE observation_relationships SET retracted_by_record_sequence = ?1
             WHERE relationship_id = ?2 AND retracted_by_record_sequence IS NULL",
            rusqlite::params![appended.local_sequence, args.relationship_id],
        )?;
    }
    tx.commit()?;
    println!(
        "Unrelated {} (sequence {})",
        args.relationship_id, appended.local_sequence
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Promotion and remediation lineage.
// ---------------------------------------------------------------------------

/// Load the reduced state for a validation check (inside the mutation tx).
fn reduced_in_tx(
    tx: &rusqlite::Transaction,
    observation_id: &str,
) -> Result<crate::remediation::reducer::ReducedObservation> {
    crate::remediation::reducer::reduce_observation(tx, observation_id)
}

/// `snag review promote <observation-id> --finding-id <finding-id>`
fn promote(mut args: crate::cli::ReviewPromoteArgs) -> Result<()> {
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let mut store = Store::open_read_write()?;
    args.observation_id = resolve_observation_id(&store.conn, &args.observation_id)?;
    let store_id = store.store_id.clone();
    let now = utc_now();
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let reduced = reduced_in_tx(&tx, &args.observation_id)?;
    if reduced.disposition.as_deref() != Some(DISP_CONFIRMED) {
        anyhow::bail!(SnagError::Validation(format!(
            "promotion requires a confirmed disposition (current: {})",
            reduced.disposition.as_deref().unwrap_or("none")
        )));
    }
    let appended = append_event(
        &tx,
        &store_id,
        RECORD_PROMOTED,
        &args.observation_id,
        RecordPayload::Remediation(RemediationEvent::Promoted(PromotedPayload {
            finding_id: args.finding_id.clone(),
            reviewer: identity.reviewer.clone(),
            review_session_id: identity.session_id.clone(),
            created_at: now.clone(),
            idempotency_key: args.idempotency_key.clone(),
        })),
    )?;
    if !appended.replayed {
        tx.execute(
            "INSERT INTO remediation_links (
                link_id, observation_id, link_type, target_id, created_at,
                source_record_sequence, idempotency_key
            ) VALUES (?1, ?2, 'finding', ?3, ?4, ?5, ?6)",
            rusqlite::params![
                appended.record_id,
                &args.observation_id,
                &args.finding_id,
                &now,
                appended.local_sequence,
                &args.idempotency_key,
            ],
        )?;
    }
    refresh_review_state(&tx, &args.observation_id)?;
    tx.commit()?;
    println!(
        "Promoted {} -> finding {} (sequence {})",
        args.observation_id, args.finding_id, appended.local_sequence
    );
    Ok(())
}

/// `snag review attach-task <observation-id> --task-id <task-id>` (multiple
/// task ids supported; each event is its own link).
fn attach_task(mut args: crate::cli::ReviewAttachTaskArgs) -> Result<()> {
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let mut store = Store::open_read_write()?;
    args.observation_id = resolve_observation_id(&store.conn, &args.observation_id)?;
    let store_id = store.store_id.clone();
    let now = utc_now();
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    // Requires a confirmed disposition (owned work belongs to a confirmed
    // problem); the claim-time fold-in (`claim --task`) already implies the
    // claim context but still requires confirmation to attach lineage.
    let reduced = reduced_in_tx(&tx, &args.observation_id)?;
    if reduced.disposition.as_deref() != Some(DISP_CONFIRMED) {
        anyhow::bail!(SnagError::Validation(format!(
            "task links require a confirmed disposition (current: {})",
            reduced.disposition.as_deref().unwrap_or("none")
        )));
    }
    attach_task_if_absent(
        &store_id,
        &tx,
        &args.observation_id,
        &identity,
        &args.task_id,
        &now,
    )?;
    refresh_review_state(&tx, &args.observation_id)?;
    tx.commit()?;
    println!("Attached task {} to {}", args.task_id, args.observation_id);
    Ok(())
}

/// `snag review attach-fix <observation-id> --commit <sha> --repo <repo-id>`
/// A commit alone never implies success (the reducer keeps the state at
/// `candidate_fix` until accepted verification arrives).
fn attach_fix(mut args: crate::cli::ReviewAttachFixArgs) -> Result<()> {
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let mut store = Store::open_read_write()?;
    args.observation_id = resolve_observation_id(&store.conn, &args.observation_id)?;
    let store_id = store.store_id.clone();
    let now = utc_now();
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let reduced = reduced_in_tx(&tx, &args.observation_id)?;
    if reduced.disposition.as_deref() != Some(DISP_CONFIRMED) {
        anyhow::bail!(SnagError::Validation(format!(
            "fix links require a confirmed disposition (current: {})",
            reduced.disposition.as_deref().unwrap_or("none")
        )));
    }
    let appended = append_event(
        &tx,
        &store_id,
        RECORD_FIX_ATTACHED,
        &args.observation_id,
        RecordPayload::Remediation(RemediationEvent::FixAttached(FixAttachedPayload {
            commit_sha: args.commit.clone(),
            repository_id: args.repo.clone(),
            reviewer: identity.reviewer.clone(),
            review_session_id: identity.session_id.clone(),
            created_at: now.clone(),
            idempotency_key: args.idempotency_key.clone(),
        })),
    )?;
    if !appended.replayed {
        tx.execute(
            "INSERT INTO remediation_links (
                link_id, observation_id, link_type, target_id, repository_id, created_at,
                source_record_sequence, idempotency_key
            ) VALUES (?1, ?2, 'commit', ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                appended.record_id,
                &args.observation_id,
                &args.commit,
                &args.repo,
                &now,
                appended.local_sequence,
                &args.idempotency_key,
            ],
        )?;
    }
    refresh_review_state(&tx, &args.observation_id)?;
    tx.commit()?;
    println!(
        "Attached fix {}@{} to {} (sequence {})",
        args.repo, args.commit, args.observation_id, appended.local_sequence
    );
    Ok(())
}

/// `snag review attach-verification <observation-id> --receipt <ref> --status <s>`
/// `accepted` is the only status that yields `verified_fixed`; rejected and
/// invalid leave remediation open.
fn attach_verification(mut args: crate::cli::ReviewAttachVerificationArgs) -> Result<()> {
    if !VERIFICATION_STATUSES.contains(&args.status.as_str()) {
        anyhow::bail!(SnagError::Validation(format!(
            "unknown verification status '{}'; allowed: {}",
            args.status,
            VERIFICATION_STATUSES.join(", ")
        )));
    }
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let mut store = Store::open_read_write()?;
    args.observation_id = resolve_observation_id(&store.conn, &args.observation_id)?;
    let store_id = store.store_id.clone();
    let now = utc_now();
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let reduced = reduced_in_tx(&tx, &args.observation_id)?;
    if reduced.disposition.as_deref() != Some(DISP_CONFIRMED) {
        anyhow::bail!(SnagError::Validation(format!(
            "verification requires a confirmed disposition (current: {})",
            reduced.disposition.as_deref().unwrap_or("none")
        )));
    }
    let appended = append_event(
        &tx,
        &store_id,
        RECORD_VERIFICATION_ATTACHED,
        &args.observation_id,
        RecordPayload::Remediation(RemediationEvent::VerificationAttached(
            VerificationAttachedPayload {
                receipt_ref: args.receipt.clone(),
                status: args.status.clone(),
                reviewer: identity.reviewer.clone(),
                review_session_id: identity.session_id.clone(),
                created_at: now.clone(),
                idempotency_key: args.idempotency_key.clone(),
            },
        )),
    )?;
    if !appended.replayed {
        tx.execute(
            "INSERT INTO remediation_links (
                link_id, observation_id, link_type, target_id, status, created_at,
                source_record_sequence, idempotency_key
            ) VALUES (?1, ?2, 'verification', ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                appended.record_id,
                &args.observation_id,
                &args.receipt,
                &args.status,
                &now,
                appended.local_sequence,
                &args.idempotency_key,
            ],
        )?;
    }
    refresh_review_state(&tx, &args.observation_id)?;
    tx.commit()?;
    println!(
        "Attached verification {} ({}) to {} (sequence {})",
        args.receipt, args.status, args.observation_id, appended.local_sequence
    );
    Ok(())
}

/// `snag review mark-handled <observation-id> [--rationale …]`
///
/// Rules: negative dispositions may be marked handled without a patch;
/// confirmed observations require a defer, at least one task link, or
/// verification evidence; an observation with neither a disposition nor any
/// remediation evidence cannot be marked handled.
fn mark_handled(mut args: crate::cli::ReviewMarkHandledArgs) -> Result<()> {
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let mut store = Store::open_read_write()?;
    args.observation_id = resolve_observation_id(&store.conn, &args.observation_id)?;
    let store_id = store.store_id.clone();
    let now = utc_now();
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let reduced = reduced_in_tx(&tx, &args.observation_id)?;
    match reduced.disposition.as_deref() {
        Some(DISP_CONFIRMED) => {
            let has_evidence = !reduced.task_ids.is_empty()
                || !reduced.verification_receipts.is_empty()
                || reduced.state == STATE_VERIFIED_FIXED;
            if !has_evidence {
                anyhow::bail!(SnagError::Validation(
                    "confirmed observations require a task link, verification evidence, or a defer disposition before mark-handled".to_string()
                ));
            }
        }
        Some(_) => {}
        None => {
            let has_evidence = !reduced.task_ids.is_empty()
                || !reduced.verification_receipts.is_empty()
                || reduced.promoted_finding_id.is_some();
            if !has_evidence {
                anyhow::bail!(SnagError::Validation(
                    "cannot mark handled an observation with no disposition and no remediation evidence".to_string()
                ));
            }
        }
    }
    let appended = append_event(
        &tx,
        &store_id,
        RECORD_MARKED_HANDLED,
        &args.observation_id,
        RecordPayload::Remediation(RemediationEvent::MarkedHandled(MarkedHandledPayload {
            rationale: args.rationale.clone(),
            reviewer: identity.reviewer.clone(),
            review_session_id: identity.session_id.clone(),
            created_at: now.clone(),
            idempotency_key: args.idempotency_key.clone(),
        })),
    )?;
    refresh_review_state(&tx, &args.observation_id)?;
    tx.commit()?;
    println!(
        "Marked {} handled (sequence {})",
        args.observation_id, appended.local_sequence
    );
    Ok(())
}

/// `snag review reopen-remediation <observation-id> --rationale …` — append-only
/// reopening of a handled remediation.
fn reopen_remediation(mut args: crate::cli::ReviewReopenRemediationArgs) -> Result<()> {
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let mut store = Store::open_read_write()?;
    args.observation_id = resolve_observation_id(&store.conn, &args.observation_id)?;
    let store_id = store.store_id.clone();
    let now = utc_now();
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let reduced = reduced_in_tx(&tx, &args.observation_id)?;
    if !reduced.handled {
        anyhow::bail!(SnagError::Validation(format!(
            "observation {} is not handled; nothing to reopen",
            args.observation_id
        )));
    }
    let appended = append_event(
        &tx,
        &store_id,
        RECORD_REMEDIATION_REOPENED,
        &args.observation_id,
        RecordPayload::Remediation(RemediationEvent::RemediationReopened(
            RemediationReopenedPayload {
                rationale: args.rationale.clone(),
                reviewer: identity.reviewer.clone(),
                review_session_id: identity.session_id.clone(),
                created_at: now.clone(),
                idempotency_key: args.idempotency_key.clone(),
            },
        )),
    )?;
    refresh_review_state(&tx, &args.observation_id)?;
    tx.commit()?;
    println!(
        "Reopened remediation for {} (sequence {})",
        args.observation_id, appended.local_sequence
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Inspection (show / history).
// ---------------------------------------------------------------------------

/// `snag review show <observation-id> [--format json]` — the full evidence
/// packet (the same versioned envelope `next --format agent` emits), so a
/// remediation session can re-inspect any queued observation.
fn show(mut args: crate::cli::ReviewShowArgs) -> Result<()> {
    let store = Store::open_read_only()?;
    args.observation_id = resolve_observation_id(&store.conn, &args.observation_id)?;
    let packet = agent_packet(&store, &args.observation_id)?;
    if args.format.as_deref() == Some("json") || args.format.as_deref() == Some("agent") {
        println!("{}", serde_json::to_string_pretty(&packet)?);
    } else {
        render_show_text(&args.observation_id, &packet);
    }
    Ok(())
}

fn render_show_text(observation_id: &str, packet: &serde_json::Value) {
    // Observation id first, then the state line and the body fields.
    let observation = &packet["observation"];
    println!("{}", terminal_safe(observation_id));
    println!(
        "title: {}  severity: {}  kind: {}",
        terminal_safe(observation["title"].as_str().unwrap_or("")),
        terminal_safe(observation["severity_assertion"].as_str().unwrap_or("-")),
        terminal_safe(observation["kind_assertion"].as_str().unwrap_or("-"))
    );
    println!(
        "state: {}  disposition: {}  handled: {}",
        terminal_safe(
            packet["current_state"]["remediation_status"]
                .as_str()
                .unwrap_or("")
        ),
        terminal_safe(
            packet["current_state"]["disposition"]
                .as_str()
                .unwrap_or("-")
        ),
        packet["current_state"]["handled"]
    );
    render_work_status(packet);
    if let Some(owner) = packet["current_state"]["owner_repository_id"].as_str() {
        println!("owner: {}", terminal_safe(owner));
    }
    if let Some(claim) = packet["current_state"]["active_claim"].as_object() {
        println!(
            "claim: {} by {} (session {}) until {}",
            terminal_safe(claim["claim_id"].as_str().unwrap_or("?")),
            terminal_safe(claim["claimed_by"].as_str().unwrap_or("?")),
            terminal_safe(claim["claim_session_id"].as_str().unwrap_or("?")),
            terminal_safe(claim["lease_expires_at"].as_str().unwrap_or("?"))
        );
    }
    if let Some(expected) = observation["expected_behavior"].as_str() {
        println!("expected: {}", terminal_safe(expected));
    }
    if let Some(observed) = observation["observed_behavior"].as_str() {
        println!("observed: {}", terminal_safe(observed));
    }
    if let Some(reproduction) = observation["reproduction"].as_str() {
        println!("repro: {}", terminal_safe(reproduction));
    }
    if packet["body_gap"].as_bool() == Some(true) {
        println!("warning: thin body (severity above minor, no expected/observed/repro)");
    }
    let lineage = &packet["lineage"];
    println!(
        "lineage: finding={} tasks={} commits={} receipts={}",
        terminal_safe(lineage["finding_id"].as_str().unwrap_or("-")),
        lineage["task_ids"].as_array().map(|v| v.len()).unwrap_or(0),
        lineage["commits"].as_array().map(|v| v.len()).unwrap_or(0),
        lineage["verification_receipts"]
            .as_array()
            .map(|v| v.len())
            .unwrap_or(0)
    );
}
fn render_work_status(packet: &serde_json::Value) {
    // Canonical current-state line: the one-call agent read answers
    // "what is the work status and why" without re-deriving it.
    let work_status = packet["current_state"]["work_status"]
        .as_str()
        .unwrap_or("actionable");
    let reopened = packet["current_state"]["reopened"].as_bool() == Some(true);
    let redirect = packet["current_state"]["redirect_observation_id"].as_str();
    let has_claim = packet["current_state"]["active_claim"].is_object();
    let lineage = &packet["lineage"];
    let task_n = lineage["task_ids"].as_array().map(|v| v.len()).unwrap_or(0);
    let commit_n = lineage["commits"].as_array().map(|v| v.len()).unwrap_or(0);
    let verification = packet["current_state"]["verification"]
        .as_str()
        .unwrap_or("none");
    println!("work: {}", terminal_safe(work_status));
    match work_status {
        "terminal" => {
            if let Some(redirect) = redirect {
                println!("  → {}", terminal_safe(redirect));
            } else {
                println!("  no redirect");
            }
        }
        "resolved" => {
            println!(
                "  fixes {commit_n}; tasks {task_n}; verification {}",
                terminal_safe(verification)
            )
        }
        "active" => println!("  claim {has_claim}; tasks {task_n}; fixes {commit_n}"),
        _ => println!("  reopened {reopened}; no active claim/task/fix"),
    }
}

/// `snag review history <observation-id> [--format json]` — every remediation
/// event for the observation in stream order (append-only audit surface).
fn history(mut args: crate::cli::ReviewHistoryArgs) -> Result<()> {
    let store = Store::open_read_only()?;
    args.observation_id = resolve_observation_id(&store.conn, &args.observation_id)?;
    let packet = agent_packet(&store, &args.observation_id)?;
    let events = packet["remediation_history"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if args.format.as_deref() == Some("json") {
        println!("{}", serde_json::to_string_pretty(&events)?);
        return Ok(());
    }
    if events.is_empty() {
        println!("no remediation events for {}", args.observation_id);
        return Ok(());
    }
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(events.len());
    for ev in &events {
        let seq = ev["local_sequence"].as_i64().unwrap_or(0);
        let typ = ev["record_type"].as_str().unwrap_or("?");
        rows.push(vec![seq.to_string(), typ.to_string()]);
    }
    render_table(
        &["SEQ", "RECORD"],
        &[TableAlign::Right, TableAlign::Left],
        &rows,
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Completion-report validation.
// ---------------------------------------------------------------------------

/// `snag review verify-report <file>` — validate a remediation agent's
/// completion report (YAML or JSON) against the recorded events. Reports ALL
/// mismatches in one pass; exit 1 when any claim fails to trace.
fn verify_report_cmd(args: crate::cli::ReviewVerifyReportArgs) -> Result<()> {
    let store = Store::open_read_only()?;
    let failures = crate::remediation::report_check::verify_report(&store, &args.report)?;
    if failures.is_empty() {
        println!(
            "Completion report OK ({} item(s) consistent with recorded events)",
            crate::remediation::report_check::item_count(&args.report)?
        );
        return Ok(());
    }
    for f in &failures {
        eprintln!("{}: {}", f.observation_id, f.message);
    }
    anyhow::bail!(SnagError::Validation(format!(
        "completion report failed {} check(s)",
        failures.len()
    )));
}

// ---------------------------------------------------------------------------
// Observation id resolution (GitHub-style short prefixes).
// ---------------------------------------------------------------------------

/// Resolve a possibly-abbreviated observation id: an exact match wins; a
/// unique prefix of the full id (`obs_01kz8…` abbreviated) resolves;
/// ambiguity and misses are typed errors. Keeps the CLI usable when agents
/// copy/truncate ids mid-session.
pub fn resolve_observation_id(conn: &rusqlite::Connection, input: &str) -> Result<String> {
    let exact: Option<String> = conn
        .query_row(
            "SELECT observation_id FROM observations WHERE observation_id = ?1",
            rusqlite::params![input],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = exact {
        return Ok(id);
    }
    let mut stmt = conn.prepare(
        "SELECT observation_id FROM observations WHERE observation_id LIKE ?1 ORDER BY observation_id",
    )?;
    let rows: Vec<String> = {
        let matches = stmt.query_map(rusqlite::params![format!("{input}%")], |row| {
            row.get::<_, String>(0)
        })?;
        let mut v = Vec::new();
        for r in matches {
            v.push(r?);
        }
        v
    };
    match rows.len() {
        0 => anyhow::bail!(SnagError::NotFound(format!(
            "observation matching '{input}'"
        ))),
        1 => Ok(rows[0].clone()),
        n => anyhow::bail!(SnagError::Validation(format!(
            "observation id '{input}' is ambiguous: matches {n} observations"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Queue retrieval.
// ---------------------------------------------------------------------------

/// Resolve a repository filter without materializing identity state.
///
/// Read commands must never create repositories, aliases, checkouts, or
/// worktrees. Exact canonical ids win; `current` then checks the existing
/// checkout binding before consulting confirmed remote aliases.
fn resolve_repo_filter_read(
    conn: &rusqlite::Connection,
    repo: Option<&str>,
) -> Result<Option<String>> {
    let Some(repo) = repo else { return Ok(None) };

    if repo != "current" {
        let exact: Option<String> = conn
            .query_row(
                "SELECT repository_id FROM repositories WHERE repository_id = ?1",
                rusqlite::params![repo],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = exact {
            return Ok(Some(id));
        }

        let mut stmt = conn.prepare(
            "SELECT repository_id FROM repository_aliases
             WHERE alias = ?1 AND confirmed = 1
             ORDER BY repository_id",
        )?;
        let candidates = stmt
            .query_map(
                rusqlite::params![crate::git::normalize_remote_alias(repo)],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        return match candidates.as_slice() {
            [] => anyhow::bail!(SnagError::RepositoryNotFound(repo.to_string())),
            [id] => Ok(Some(id.clone())),
            _ => anyhow::bail!(SnagError::RepositoryAmbiguous(format!(
                "alias {repo:?} matches multiple repositories: {candidates:?}"
            ))),
        };
    }

    let git_ctx = crate::git::collect_git_context(&std::env::current_dir()?)?;
    let git_common_dir = git_ctx.git_common_dir.as_deref().ok_or_else(|| {
        anyhow::anyhow!(SnagError::RepositoryNotFound(
            "current checkout".to_string()
        ))
    })?;
    let checkout_owner: Option<String> = conn
        .query_row(
            "SELECT repository_id FROM checkouts WHERE git_common_dir = ?1",
            rusqlite::params![git_common_dir],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = checkout_owner {
        return Ok(Some(id));
    }

    let mut candidates = std::collections::BTreeSet::new();
    for alias in &git_ctx.git_remote_aliases {
        let mut stmt = conn.prepare(
            "SELECT repository_id FROM repository_aliases
             WHERE alias = ?1 AND confirmed = 1
             ORDER BY repository_id",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![crate::git::normalize_remote_alias(alias)],
            |row| row.get::<_, String>(0),
        )?;
        for candidate in rows {
            candidates.insert(candidate?);
        }
    }
    match candidates.into_iter().collect::<Vec<_>>().as_slice() {
        [id] => Ok(Some(id.clone())),
        [] => anyhow::bail!(SnagError::RepositoryNotFound(
            "current checkout".to_string()
        )),
        candidates => anyhow::bail!(SnagError::RepositoryAmbiguous(format!(
            "aliases {:?} match multiple repositories: {candidates:?}",
            git_ctx.git_remote_aliases
        ))),
    }
}

fn render_empty_queue(
    format: Option<&str>,
    store_id: Option<&str>,
    db_path: &std::path::Path,
    observation_count: i64,
) -> Result<()> {
    if format == Some("agent") {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 1,
                "queue": "empty",
                "store": {
                    "store_id": store_id,
                    "db_path": db_path.display().to_string(),
                    "observations": observation_count,
                },
                "message": "no unhandled observations match the filters",
            }))?
        );
    } else {
        println!("empty queue: no unhandled observations match the filters");
        println!(
            "store: {} ({observation_count} observations)",
            db_path.display()
        );
    }
    Ok(())
}

/// `snag review next [filters] [--format agent] [--claim]`
fn next(args: crate::cli::ReviewNextArgs) -> Result<()> {
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    if !args.claim {
        let (_, db_path) = Store::paths()?;
        if !db_path.exists() {
            render_empty_queue(args.format.as_deref(), None, &db_path, 0)?;
            return Ok(());
        }
    }
    let mut store = if args.claim {
        Store::open_read_write()?
    } else {
        Store::open_read_only()?
    };
    let now = utc_now();
    let store_id = store.store_id.clone();
    let repository_id = resolve_repo_filter_read(&store.conn, args.repo.as_deref())?;
    let work_status_ids = args
        .work_status
        .map(|ws| crate::remediation::reducer::work_status_matching_ids(&store.conn, ws.0))
        .transpose()?;
    let filters = NextFilters {
        repository_id,
        kind: args.kind,
        severity: args.severity,
        unreviewed: args.unreviewed,
        include_deferred: args.include_deferred,
        my_session: identity.session_id.clone(),
        now: now.clone(),
        work_status_ids,
    };
    let tx = if args.claim {
        store
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?
    } else {
        store.conn.transaction()?
    };
    let selected = queue::select_next(&tx, &filters)?;

    let Some(observation_id) = selected else {
        // Typed empty-queue response, not an error. It names the active store
        // so a wrong-store (e.g. leaked XDG_DATA_HOME) is one-glance obvious
        // instead of a baffling empty queue.
        let observation_count: i64 = tx
            .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
            .unwrap_or(0);
        render_empty_queue(
            args.format.as_deref(),
            Some(&store.store_id),
            &store.db_path,
            observation_count,
        )?;
        tx.commit()?;
        return Ok(());
    };

    if args.claim {
        let outcome = claim_observation(
            &store_id,
            &tx,
            &observation_id,
            &identity,
            identity::default_lease_seconds(),
            None,
            &now,
        )?;
        tx.commit()?;
        crate::failpoint::failpoint("remediation_after_commit");
        if args.format.as_deref() == Some("agent") {
            println!(
                "{}",
                serde_json::to_string_pretty(&agent_packet(&store, &observation_id)?)?
            );
        } else {
            let reduced =
                crate::remediation::reducer::reduce_observation(&store.conn, &observation_id)?;
            render_next_text(&observation_id, &reduced);
            match outcome {
                ClaimOutcome::Acquired { claim_id, .. }
                | ClaimOutcome::Replayed { claim_id, .. } => {
                    println!("claimed: {} (session {})", claim_id, identity.session_id);
                }
            }
        }
        return Ok(());
    }

    tx.commit()?;
    if args.format.as_deref() == Some("agent") {
        println!(
            "{}",
            serde_json::to_string_pretty(&agent_packet(&store, &observation_id)?)?
        );
    } else {
        let reduced =
            crate::remediation::reducer::reduce_observation(&store.conn, &observation_id)?;
        render_next_text(&observation_id, &reduced);
    }
    Ok(())
}

/// Push the repository/kind/severity scope clauses for `review list`.
/// Extracted so `list` stays under the airlock complexity floor.
fn push_list_scope_clauses(
    sql: &mut String,
    params: &mut Vec<Box<dyn rusqlite::ToSql>>,
    repository_id: Option<&str>,
    args: &crate::cli::ReviewListArgs,
) {
    if let Some(rid) = repository_id {
        sql.push_str(
            " AND EXISTS (
                SELECT 1 FROM observation_repositories owner_r
                WHERE owner_r.observation_id = o.observation_id
                  AND owner_r.repository_id = ?
                  AND owner_r.role = 'owner'
            )",
        );
        params.push(Box::new(rid.to_string()));
    }
    if let Some(k) = &args.kind {
        sql.push_str(" AND o.kind_assertion = ?");
        params.push(Box::new(k.clone()));
    }
    if let Some(sev) = &args.severity {
        sql.push_str(" AND o.severity_assertion = ?");
        params.push(Box::new(sev.clone()));
    }
}

/// Push the review-state clauses (unreviewed, claimed-by, disposition, status).
fn push_list_state_clauses(
    sql: &mut String,
    params: &mut Vec<Box<dyn rusqlite::ToSql>>,
    args: &crate::cli::ReviewListArgs,
) {
    if args.unreviewed {
        sql.push_str(" AND (rs.observation_id IS NULL OR rs.state = 'unreviewed')");
    }
    if let Some(cb) = &args.claimed_by {
        sql.push_str(
            " AND rs.active_claim_id IS NOT NULL AND rs.active_claim_expires_at IS NOT NULL",
        );
        sql.push_str(" AND EXISTS (SELECT 1 FROM remediation_claims c WHERE c.observation_id = o.observation_id AND c.claim_session_id = ? AND c.released_at IS NULL)");
        params.push(Box::new(cb.clone()));
    }
    if let Some(d) = &args.disposition {
        sql.push_str(" AND rs.disposition = ?");
        params.push(Box::new(d.clone()));
    }
    if let Some(s) = &args.status {
        sql.push_str(" AND COALESCE(rs.state, 'unreviewed') = ?");
        params.push(Box::new(s.clone()));
    }
}

/// Push the handled/unhandled clauses; `--include-deferred` widens `--unhandled`
/// because deferred marks handled=true in the reducer.
fn push_list_handled_clauses(sql: &mut String, args: &crate::cli::ReviewListArgs) {
    if args.handled {
        sql.push_str(" AND COALESCE(rs.handled, 0) = 1");
    }
    if args.unhandled {
        if args.include_deferred {
            sql.push_str(" AND (COALESCE(rs.handled, 0) = 0 OR rs.state = 'deferred')");
        } else {
            sql.push_str(" AND COALESCE(rs.handled, 0) = 0");
        }
    }
}

/// `snag review list [filters] [--limit N] [--offset N] [--format json]`
fn list(args: crate::cli::ReviewListArgs) -> Result<()> {
    let store = Store::open_read_only()?;
    let repository_id = resolve_repo_filter_read(&store.conn, args.repo.as_deref())?;
    let mut sql = String::from(
        "SELECT o.observation_id, o.title, o.severity_assertion, o.kind_assertion,
                COALESCE(rs.state, 'unreviewed') AS state,
                rs.disposition, COALESCE(rs.handled, 0) AS handled, rs.active_claim_id,
                c.claim_session_id, c.claimed_by,
                (SELECT owner_r.repository_id FROM observation_repositories owner_r
                 WHERE owner_r.observation_id = o.observation_id AND owner_r.role = 'owner')
                 AS owner_repository_id
         FROM observations o
         LEFT JOIN observation_review_state rs ON rs.observation_id = o.observation_id
         LEFT JOIN remediation_claims c ON c.claim_id = rs.active_claim_id
         WHERE 1=1",
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    push_list_scope_clauses(&mut sql, &mut params, repository_id.as_deref(), &args);
    push_list_state_clauses(&mut sql, &mut params, &args);
    push_list_handled_clauses(&mut sql, &args);
    // Canonical work-status filter: ids derived by the reducer, injected as
    // a json_each IN-clause — the filter runs against the canonical
    // derivation, never a parallel SQL re-implementation.
    let reduced = if args.format.as_deref() == Some("json") {
        Some(crate::remediation::reducer::replay_all(&store.conn)?)
    } else {
        None
    };
    if let Some(ws) = args.work_status {
        let ids = crate::remediation::reducer::work_status_matching_ids(&store.conn, ws.0)?;
        let ids_json = serde_json::to_string(&ids)?;
        sql.push_str(" AND o.observation_id IN (SELECT value FROM json_each(?))");
        params.push(Box::new(ids_json));
    }
    sql.push_str(" ORDER BY o.captured_at ASC, o.local_sequence ASC");
    if args.limit > 0 {
        sql.push_str(" LIMIT ? OFFSET ?");
        params.push(Box::new(args.limit as i64));
        params.push(Box::new(args.offset as i64));
    }

    let mut stmt = store.conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<String>>(8)?,
            row.get::<_, Option<String>>(9)?,
            row.get::<_, Option<String>>(10)?,
        ))
    })?;
    let mut out = Vec::new();
    // Canonical current-state per row comes from the reducer (single stream
    // pass, computed above), never a parallel SQL derivation — the parity
    // invariant: every read surface answers "what is current" identically.
    for r in rows {
        let (id, title, sev, kind, state, disp, handled, claim, claim_session, claimed_by, owner) =
            r?;
        let reduced_row = reduced.as_ref().and_then(|m| m.get(&id));
        let work_status = reduced_row
            .map(|r| r.work_status.as_str())
            .unwrap_or(WorkStatus::Actionable.as_str());
        let reopened = reduced_row.map(|r| r.reopened).unwrap_or(false);
        let redirect = reduced_row.and_then(|r| r.disposition_target.clone());
        let task_ids = reduced_row.map(|r| r.task_ids.clone()).unwrap_or_default();
        let commits: Vec<String> = reduced_row
            .map(|r| {
                r.commits
                    .iter()
                    .map(|c| c.commit_sha.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let verification = reduced_row.and_then(|r| r.latest_verification_status.clone());
        if args.format.as_deref() == Some("json") {
            out.push(serde_json::json!({
                "observation_id": id,
                "title": title,
                "severity": sev,
                "kind": kind,
                "state": state,
                "disposition": disp,
                "work_status": work_status,
                "reopened": reopened,
                "redirect_observation_id": redirect,
                "handled": handled == 1,
                "active_claim_id": claim,
                "active_claim_session_id": claim_session,
                "active_claim_claimed_by": claimed_by,
                "owner_repository_id": owner,
                "task_ids": task_ids,
                "commits": commits,
                "verification": verification,
            }));
        } else {
            // Observation ids first: they became the cross-session language.
            println!("{}  {}", terminal_safe(&id), terminal_safe(&title));
            println!(
                "  state: {}  disposition: {}  severity: {}  handled: {}  owner: {}",
                terminal_safe(&state),
                terminal_safe(disp.as_deref().unwrap_or("-")),
                terminal_safe(sev.as_deref().unwrap_or("-")),
                handled == 1,
                terminal_safe(owner.as_deref().unwrap_or("-")),
            );
        }
    }
    if args.format.as_deref() == Some("json") {
        println!("{}", serde_json::to_string_pretty(&out)?);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Summary (per-repo open-observation materiality for dispatch).
// ---------------------------------------------------------------------------

/// Severity weights for the display materiality column (blocker=4, major=3,
/// medium=2, minor=1, low=0.5). Display only — the exit code is driven by the
/// explicit `--at-least` thresholds, never by this score.
const MATERIALITY_WEIGHTS: &[(&str, f64)] = &[
    (crate::parser::SEV_BLOCKER, 4.0),
    (crate::parser::SEV_MAJOR, 3.0),
    (crate::parser::SEV_MEDIUM, 2.0),
    (crate::parser::SEV_MINOR, 1.0),
    (crate::parser::SEV_LOW, 0.5),
];

/// In-flight states excluded from threshold counts: someone has an active
/// claim, attached commit, or attached task, so dispatching a fresh agent on
/// that obs is wasteful. (Still shown in `open`.)
const INFLIGHT_STATES: &[&str] = &[
    crate::remediation::reducer::STATE_CANDIDATE_FIX,
    crate::remediation::reducer::STATE_REMEDIATION_IN_PROGRESS,
];

/// Append the per-lane aggregate SELECT columns (open severity counts,
/// actionable severity counts, unreviewed, oldest). Shared verbatim by the
/// per-repo and unowned queries so the two lanes stay definitionally identical.
fn push_lane_aggregate_columns(sql: &mut String) {
    let inflight = INFLIGHT_STATES
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ");
    sql.push_str(
        &format!(
            "COALESCE(COUNT(*), 0) AS open_count,
         COALESCE(SUM(CASE WHEN o.severity_assertion = 'blocker' THEN 1 ELSE 0 END), 0) AS sev_blocker,
         COALESCE(SUM(CASE WHEN o.severity_assertion = 'major' THEN 1 ELSE 0 END), 0) AS sev_major,
         COALESCE(SUM(CASE WHEN o.severity_assertion = 'medium' THEN 1 ELSE 0 END), 0) AS sev_medium,
         COALESCE(SUM(CASE WHEN o.severity_assertion = 'minor' THEN 1 ELSE 0 END), 0) AS sev_minor,
         COALESCE(SUM(CASE WHEN o.severity_assertion = 'low' THEN 1 ELSE 0 END), 0) AS sev_low,
         COALESCE(SUM(CASE WHEN o.severity_assertion IS NULL
                                OR o.severity_assertion NOT IN ('blocker', 'major', 'medium', 'minor', 'low')
                           THEN 1 ELSE 0 END), 0) AS sev_unknown,
         COALESCE(SUM(CASE WHEN o.severity_assertion = 'blocker'
                                AND COALESCE(rs.state, 'unreviewed') NOT IN ({inflight})
                                AND ac.observation_id IS NULL
                           THEN 1 ELSE 0 END), 0) AS act_blocker,
         COALESCE(SUM(CASE WHEN o.severity_assertion = 'major'
                                AND COALESCE(rs.state, 'unreviewed') NOT IN ({inflight})
                                AND ac.observation_id IS NULL
                           THEN 1 ELSE 0 END), 0) AS act_major,
         COALESCE(SUM(CASE WHEN o.severity_assertion = 'medium'
                                AND COALESCE(rs.state, 'unreviewed') NOT IN ({inflight})
                                AND ac.observation_id IS NULL
                           THEN 1 ELSE 0 END), 0) AS act_medium,
         COALESCE(SUM(CASE WHEN o.severity_assertion = 'minor'
                                AND COALESCE(rs.state, 'unreviewed') NOT IN ({inflight})
                                AND ac.observation_id IS NULL
                           THEN 1 ELSE 0 END), 0) AS act_minor,
         COALESCE(SUM(CASE WHEN o.severity_assertion = 'low'
                                AND COALESCE(rs.state, 'unreviewed') NOT IN ({inflight})
                                AND ac.observation_id IS NULL
                           THEN 1 ELSE 0 END), 0) AS act_low,
         COALESCE(SUM(CASE WHEN (o.severity_assertion IS NULL
                                  OR o.severity_assertion NOT IN ('blocker', 'major', 'medium', 'minor', 'low'))
                                AND COALESCE(rs.state, 'unreviewed') NOT IN ({inflight})
                                AND ac.observation_id IS NULL
                           THEN 1 ELSE 0 END), 0) AS act_unknown,
         COALESCE(SUM(CASE WHEN COALESCE(rs.state, 'unreviewed') = 'unreviewed'
                                AND ac.observation_id IS NULL
                           THEN 1 ELSE 0 END), 0) AS unreviewed,
         MIN(o.captured_at) AS oldest_open",
        )
    );
}

/// One lane's aggregate: `repo_id` is None for the unowned bucket.
struct LaneAggregate {
    repo_id: Option<String>,
    display: String,
    /// How the human label relates to repository identities.
    identity_status: String,
    /// Number of repository ids represented by the selected label, including
    /// this explicit id when it has no alias row of its own.
    label_repository_count: i64,
    open_count: i64,
    /// Open severity counts, indexed by SEVERITIES order plus unknown.
    severity_counts: [i64; 6],
    /// Actionable severity counts (open, not in-flight), SEVERITIES order plus unknown.
    actionable_counts: [i64; 6],
    unreviewed: i64,
    oldest_open: Option<String>,
    materiality: f64,
}

impl LaneAggregate {
    fn severity_index(sev: &str) -> usize {
        crate::parser::SEVERITIES
            .iter()
            .position(|s| *s == sev)
            .unwrap_or(5)
    }

    fn crossed(&self, thresholds: &[(String, i64)]) -> bool {
        thresholds.iter().any(|(sev, count)| {
            let idx = Self::severity_index(sev);
            self.actionable_counts[idx] >= *count
        })
    }
    fn actionable(&self) -> i64 {
        self.actionable_counts.iter().sum()
    }

    fn in_flight(&self) -> i64 {
        self.open_count - self.actionable()
    }
}

fn read_lane_row(
    row: &rusqlite::Row<'_>,
    repo_id: Option<String>,
    display: String,
) -> rusqlite::Result<LaneAggregate> {
    let sev: [i64; 6] = [
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
    ];
    let act: [i64; 6] = [
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
    ];
    let materiality = MATERIALITY_WEIGHTS
        .iter()
        .zip(act.iter())
        .map(|((_, w), n)| w * (*n as f64))
        .sum();
    let identity_status = match repo_id.as_deref() {
        Some(id) if id.starts_with("repo_") => "id-only",
        Some(_) => "explicit-id",
        None => "unowned",
    }
    .to_string();
    let label_repository_count = i64::from(repo_id.is_some());
    Ok(LaneAggregate {
        repo_id,
        display,
        identity_status,
        label_repository_count,
        open_count: row.get(1)?,
        severity_counts: sev,
        actionable_counts: act,
        unreviewed: row.get(14)?,
        oldest_open: row.get(15)?,
        materiality,
    })
}

/// Parse `--at-least severity=count` values into validated thresholds.
fn parse_thresholds(raw: &[String]) -> Result<Vec<(String, i64)>> {
    let mut thresholds = Vec::new();
    for item in raw {
        let (sev, cnt) = item.split_once('=').ok_or_else(|| {
            SnagError::Validation(format!(
                "--at-least expects <severity>=<count>, got '{item}'"
            ))
        })?;
        if !crate::parser::SEVERITIES.contains(&sev) {
            anyhow::bail!(SnagError::Validation(format!(
                "unknown severity '{sev}'; allowed: {}",
                crate::parser::SEVERITIES.join(", ")
            )));
        }
        let count: i64 = cnt.parse().map_err(|_| {
            SnagError::Validation(format!("--at-least count '{cnt}' is not an integer"))
        })?;
        if count <= 0 {
            anyhow::bail!(SnagError::Validation(
                "--at-least count must be >= 1".to_string()
            ));
        }
        thresholds.push((sev.to_string(), count));
    }
    Ok(thresholds)
}

/// Query fix-owner lanes, open (not handled) observations only. A filing
/// reporter never defines a lane. Optional `repository_id` narrows to one
/// owner lane.
fn query_repo_lanes(
    conn: &rusqlite::Connection,
    repository_id: Option<&str>,
) -> Result<Vec<LaneAggregate>> {
    let mut sql = String::from("SELECT owner_r.repository_id AS lane_id, ");
    push_lane_aggregate_columns(&mut sql);
    sql.push_str(
        " FROM observations o
         JOIN observation_repositories owner_r
           ON owner_r.observation_id = o.observation_id AND owner_r.role = 'owner'
         LEFT JOIN observation_review_state rs ON rs.observation_id = o.observation_id
         LEFT JOIN (SELECT DISTINCT observation_id FROM active_claims) ac
           ON ac.observation_id = o.observation_id
         WHERE COALESCE(rs.handled, 0) = 0
           AND NOT EXISTS (
               SELECT 1 FROM records retracted
               WHERE retracted.entity_id = o.observation_id
                 AND retracted.record_type = 'observation_retracted'
           )",
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(rid) = repository_id {
        sql.push_str(" AND owner_r.repository_id = ?");
        params.push(Box::new(rid.to_string()));
    }
    sql.push_str(" GROUP BY owner_r.repository_id");

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut lanes = stmt
        .query_map(param_refs.as_slice(), |row| {
            let rid: String = row.get(0)?;
            read_lane_row(row, Some(rid), String::new())
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    let repo_ids: Vec<String> = lanes
        .iter()
        .filter_map(|lane| lane.repo_id.clone())
        .collect();
    let display_names = bulk_display_names(conn, &repo_ids)?;
    for lane in &mut lanes {
        if let Some(repo_id) = lane.repo_id.as_deref() {
            lane.display = display_names
                .get(repo_id)
                .cloned()
                .unwrap_or_else(|| abbreviated_repo_id(repo_id));
            apply_identity_evidence(conn, lane);
        }
    }
    Ok(lanes)
}

/// Query the unowned bucket (open observations without a fix owner), including
/// observations that have a filing reporter; None when empty.
fn query_unowned_lane(conn: &rusqlite::Connection) -> Result<Option<LaneAggregate>> {
    let mut usql = String::from("SELECT NULL, ");
    push_lane_aggregate_columns(&mut usql);
    usql.push_str(
        " FROM observations o
         LEFT JOIN observation_review_state rs ON rs.observation_id = o.observation_id
         LEFT JOIN (SELECT DISTINCT observation_id FROM active_claims) ac
           ON ac.observation_id = o.observation_id
         WHERE COALESCE(rs.handled, 0) = 0
           AND NOT EXISTS (
               SELECT 1 FROM records retracted
               WHERE retracted.entity_id = o.observation_id
                 AND retracted.record_type = 'observation_retracted'
           )
           AND NOT EXISTS (
               SELECT 1 FROM observation_repositories r
               WHERE r.observation_id = o.observation_id AND r.role = 'owner'
           )",
    );
    let mut ustmt = conn.prepare(&usql)?;
    let mut urows = ustmt.query([])?;
    // The aggregate without GROUP BY always yields one row (COALESCE'd
    // zeros) even when no unowned obs exist — treat open_count == 0 as
    // "no bucket" so an empty store never renders a phantom (unowned) row.
    if let Some(urow) = urows.next()? {
        let lane = read_lane_row(urow, None, "(unowned)".to_string())?;
        if lane.open_count > 0 {
            return Ok(Some(lane));
        }
    }
    Ok(None)
}

/// Exit code: 1 when ANY evaluated lane (or the unowned bucket) crosses a
/// threshold, else 0. `--limit` truncates DISPLAY only — every lane is still
/// evaluated.
fn summary_exit_code(
    lanes: &[LaneAggregate],
    unowned: &Option<LaneAggregate>,
    thresholds: &[(String, i64)],
) -> i32 {
    if lanes
        .iter()
        .chain(unowned.iter())
        .any(|l| l.crossed(thresholds))
    {
        1
    } else {
        0
    }
}

/// Severity-count map in SEVERITIES order plus an additive unknown bucket.
fn severity_counts_json(counts: &[i64; 6]) -> serde_json::Value {
    serde_json::json!({
        "blocker": counts[0],
        "major": counts[1],
        "medium": counts[2],
        "minor": counts[3],
        "low": counts[4],
        "unknown": counts[5],
    })
}

fn render_summary_json(
    lanes: &[LaneAggregate],
    unowned: &Option<LaneAggregate>,
    thresholds: &[(String, i64)],
    exit_code: i32,
    limit: usize,
) -> Result<()> {
    let visible = lanes
        .iter()
        .take(if limit > 0 { limit } else { lanes.len() });
    let repos_json: Vec<serde_json::Value> = visible
        .map(|l| {
            serde_json::json!({
                "repo_id": l.repo_id,
                "display": l.display,
                "identity": {
                    "status": l.identity_status,
                    "label_repository_count": l.label_repository_count,
                    "ambiguous_label": l.identity_status == "ambiguous-label",
                },
                "open": l.open_count,
                "severity_counts": severity_counts_json(&l.severity_counts),
                "actionable": l.actionable(),
                "actionable_severity_counts": severity_counts_json(&l.actionable_counts),
                "in_flight": l.in_flight(),
                "unreviewed": l.unreviewed,
                "oldest_open": l.oldest_open,
                "materiality": l.materiality,
                "crossed": l.crossed(thresholds),
            })
        })
        .collect();
    let mut envelope = serde_json::json!({
        "schema": "review_summary_v1",
        "thresholds": thresholds
            .iter()
            .map(|(s, c)| serde_json::json!({ "severity": s, "count": c }))
            .collect::<Vec<_>>(),
        "exit_code": exit_code,
        "repos": repos_json,
    });
    if let Some(u) = unowned {
        envelope["unowned"] = serde_json::json!({
            "open": u.open_count,
            "severity_counts": severity_counts_json(&u.severity_counts),
            "actionable": u.actionable(),
            "actionable_severity_counts": severity_counts_json(&u.actionable_counts),
            "in_flight": u.in_flight(),
            "unreviewed": u.unreviewed,
            "oldest_open": u.oldest_open,
            "materiality": u.materiality,
            "crossed": u.crossed(thresholds),
        });
    }
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    Ok(())
}

fn verdict_label(crossed: bool, thresholds_empty: bool) -> String {
    if thresholds_empty {
        "-".to_string()
    } else if crossed {
        "DISPATCH".to_string()
    } else {
        "watch".to_string()
    }
}

/// Render text for a terminal without allowing stored values to control it.
///
/// Human output is the only place this is applied: callers keep the original
/// string for persistence and JSON. Escaping every control character (rather
/// than trying to parse terminal grammars) also neutralizes OSC, ANSI, and C1
/// sequences, since their introducers cannot reach the terminal.
pub(crate) fn terminal_safe(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| {
            let code = ch as u32;
            let bidi = matches!(
                code,
                0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069
            );
            if ch.is_control() || bidi || matches!(ch, '\u{2028}' | '\u{2029}') {
                format!("\\u{{{code:04x}}}").chars().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}

/// Column alignment for [`render_table`].
#[derive(Clone, Copy)]
pub(crate) enum TableAlign {
    Left,
    Right,
}

/// Measured-width text table. Each column's width is the max of the header
/// and every row cell (computed from the sanitized data, never hardcoded).
pub(crate) fn render_table(headers: &[&str], align: &[TableAlign], rows: &[Vec<String>]) {
    debug_assert_eq!(headers.len(), align.len());
    let col_count = headers.len();
    let safe_headers: Vec<String> = headers.iter().map(|h| terminal_safe(h)).collect();
    let safe_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(|cell| terminal_safe(cell)).collect())
        .collect();
    let widths: Vec<usize> = (0..col_count)
        .map(|c| {
            safe_headers[c]
                .chars()
                .count()
                .max(
                    safe_rows
                        .iter()
                        .map(|r| r.get(c).map(|s| s.chars().count()).unwrap_or(0))
                        .max()
                        .unwrap_or(0),
                )
                .max(1)
        })
        .collect();
    let cell = |value: &str, w: usize, a: TableAlign| -> String {
        match a {
            TableAlign::Left => format!("{value:<w$}"),
            TableAlign::Right => format!("{value:>w$}"),
        }
    };
    let line = |values: &[String]| -> String {
        values
            .iter()
            .enumerate()
            .map(|(c, v)| cell(v, widths[c], align[c]))
            .collect::<Vec<_>>()
            .join("  ")
    };
    println!("{}", line(&safe_headers));
    for row in &safe_rows {
        println!("{}", line(row));
    }
}

/// Resolve all displayed owner names in one query. Confirmed aliases observed
/// on a checkout bound to the same canonical repository win; otherwise the
/// most recently seen confirmed alias wins. Explicit ids remain readable ids.
fn bulk_display_names(
    conn: &rusqlite::Connection,
    repo_ids: &[String],
) -> Result<std::collections::HashMap<String, String>> {
    let mut names = std::collections::HashMap::new();
    let generated_ids: Vec<String> = repo_ids
        .iter()
        .filter(|id| id.starts_with("repo_"))
        .cloned()
        .collect();
    for repo_id in repo_ids {
        if !repo_id.starts_with("repo_") {
            names.insert(repo_id.clone(), repo_id.clone());
        }
    }
    if generated_ids.is_empty() {
        return Ok(names);
    }

    let placeholders = (0..generated_ids.len())
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "WITH observed_aliases AS (
             SELECT DISTINCT c.repository_id, context_alias.value AS alias
             FROM observations o
             JOIN checkouts c
               ON c.checkout_id =
                  json_extract(o.context_json, '$.repository.checkout_id')
             JOIN json_each(
                  json_extract(o.context_json, '$.repository.git_remote_aliases')
             ) context_alias
         )
         SELECT ra.repository_id, ra.alias
         FROM repository_aliases ra
         LEFT JOIN observed_aliases oa
           ON oa.repository_id = ra.repository_id AND oa.alias = ra.alias
         WHERE ra.confirmed = 1
           AND ra.repository_id IN ({placeholders})
         ORDER BY ra.repository_id,
                  CASE WHEN oa.repository_id IS NOT NULL THEN 1 ELSE 0 END DESC,
                  ra.last_seen_at DESC, ra.alias DESC"
    );
    let params: Vec<Box<dyn rusqlite::ToSql>> = generated_ids
        .iter()
        .map(|id| Box::new(id.clone()) as Box<dyn rusqlite::ToSql>)
        .collect();
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let aliases = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (repo_id, alias) in aliases {
        names.entry(repo_id).or_insert(alias);
    }
    for repo_id in &generated_ids {
        names
            .entry(repo_id.clone())
            .or_insert_with(|| abbreviated_repo_id(repo_id));
    }
    Ok(names)
}

fn abbreviated_repo_id(repo_id: &str) -> String {
    if repo_id.len() > 16 {
        let head = &repo_id[..12];
        let tail = &repo_id[repo_id.len() - 4..];
        format!("{head}…{tail}")
    } else {
        repo_id.to_string()
    }
}

/// Attach provenance for the selected human label. Labels are intentionally
/// many-to-many and are never merge keys; this metadata makes ambiguity
/// visible while the lane remains keyed by its exact repository id.
fn apply_identity_evidence(conn: &rusqlite::Connection, lane: &mut LaneAggregate) {
    let Some(repo_id) = lane.repo_id.as_deref() else {
        return;
    };
    let evidence = conn.query_row(
        "SELECT COUNT(DISTINCT repository_id),
                COALESCE(MAX(CASE WHEN repository_id = ?2 THEN 1 ELSE 0 END), 0)
         FROM repository_aliases
         WHERE alias = ?1 AND confirmed = 1",
        rusqlite::params![&lane.display, repo_id],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    );
    let Ok((mapped_ids, includes_lane)) = evidence else {
        return;
    };
    if mapped_ids == 0 {
        return;
    }
    lane.label_repository_count = mapped_ids + i64::from(includes_lane == 0);
    lane.identity_status = if lane.label_repository_count > 1 {
        "ambiguous-label"
    } else {
        "alias-bound"
    }
    .to_string();
}

fn render_summary_text(
    lanes: &[LaneAggregate],
    unowned: &Option<LaneAggregate>,
    thresholds: &[(String, i64)],
    limit: usize,
) {
    // Text table, ranked by materiality desc; `--limit` truncates lanes but
    // the unowned bucket always shows. Column widths are measured from the
    // data, so any lane name / timestamp length aligns.
    let visible: Vec<&LaneAggregate> = if limit > 0 {
        lanes.iter().take(limit).collect()
    } else {
        lanes.iter().collect()
    };
    let mut rows: Vec<Vec<String>> =
        Vec::with_capacity(visible.len() + usize::from(unowned.is_some()));
    for lane in visible.iter().copied().chain(unowned.iter()) {
        rows.push(vec![
            lane.repo_id.as_deref().unwrap_or("-").to_string(),
            lane.display.clone(),
            match lane.identity_status.as_str() {
                "ambiguous-label" => format!("AMBIG:{}", lane.label_repository_count),
                "explicit-id" => "EXPLICIT".to_string(),
                "alias-bound" => "BOUND".to_string(),
                "id-only" => "ID-ONLY".to_string(),
                _ => "-".to_string(),
            },
            lane.open_count.to_string(),
            lane.actionable().to_string(),
            lane.in_flight().to_string(),
            lane.actionable_counts[0].to_string(),
            lane.actionable_counts[1].to_string(),
            lane.actionable_counts[2].to_string(),
            lane.actionable_counts[3].to_string(),
            lane.actionable_counts[4].to_string(),
            lane.actionable_counts[5].to_string(),
            lane.unreviewed.to_string(),
            lane.oldest_open.as_deref().unwrap_or("-").to_string(),
            format!("{:.1}", lane.materiality),
            verdict_label(lane.crossed(thresholds), thresholds.is_empty()),
        ]);
    }
    render_table(
        &[
            "OWNER_ID", "LABEL", "IDENTITY", "OPEN", "READY", "INFLT", "R:B", "R:M", "R:MED",
            "R:MIN", "R:LOW", "R:U", "UNREV", "OLDEST", "MAT", "VERDICT",
        ],
        &[
            TableAlign::Left,
            TableAlign::Left,
            TableAlign::Left,
            TableAlign::Right,
            TableAlign::Right,
            TableAlign::Right,
            TableAlign::Right,
            TableAlign::Right,
            TableAlign::Right,
            TableAlign::Right,
            TableAlign::Right,
            TableAlign::Right,
            TableAlign::Right,
            TableAlign::Left,
            TableAlign::Right,
            TableAlign::Left,
        ],
        &rows,
    );
    println!();
    println!(
        "READY=open and not in-flight; R:=READY by severity (B/M/MED/MIN/LOW/U); \
UNREV=not adjudicated; MAT=weighted READY."
    );
    println!("Work: snag review next --repo <OWNER_ID>; then snag review claim <OBSERVATION_ID>");
    if visible.iter().any(|lane| {
        matches!(
            lane.identity_status.as_str(),
            "ambiguous-label" | "explicit-id" | "id-only"
        )
    }) {
        println!(
            "Identity: counts are never merged by LABEL. AMBIG:N maps to N ids; \
EXPLICIT is a literal id; ID-ONLY has no alias evidence."
        );
        println!("Inspect: snag review list --repo <OWNER_ID> --unhandled");
    }
}

/// `snag review summary [--repo X] [--at-least severity=count]… [--limit N]
/// [--format text|json]`
///
/// Per-owner-lane open-observation materiality: a text table
/// ranked by materiality desc (severity mix, unreviewed, oldest, unowned
/// bucket) or a `review_summary_v1` JSON envelope. With `--at-least`
/// thresholds, exits 1 when ANY evaluated lane crosses one (actionable open
/// obs without a live claim), 0 otherwise; `--repo` narrows the evaluated set
/// to that lane.
fn summary(args: crate::cli::ReviewSummaryArgs) -> Result<()> {
    let store = Store::open_read_only()?;
    let repository_id = resolve_repo_filter_read(&store.conn, args.repo.as_deref())?;
    let thresholds = parse_thresholds(&args.at_least)?;

    let mut lanes = query_repo_lanes(&store.conn, repository_id.as_deref())?;
    lanes.sort_by(|a, b| {
        b.materiality
            .partial_cmp(&a.materiality)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.repo_id.cmp(&b.repo_id))
    });

    // Unowned bucket (open obs with no primary row) — only without `--repo`.
    let unowned = if repository_id.is_none() {
        query_unowned_lane(&store.conn)?
    } else {
        None
    };

    let exit_code = summary_exit_code(&lanes, &unowned, &thresholds);
    if args.format.as_deref() == Some("json") {
        render_summary_json(&lanes, &unowned, &thresholds, exit_code, args.limit)?;
    } else {
        render_summary_text(&lanes, &unowned, &thresholds, args.limit);
    }

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}
