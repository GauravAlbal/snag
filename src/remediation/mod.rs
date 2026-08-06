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

use crate::cli::ReviewCommand;
use crate::error::SnagError;
use crate::record::RecordPayload;
use crate::remediation::events::*;
use crate::remediation::identity::{RemediationIdentity, lease_expiry, resolve_identity, utc_now};
use crate::remediation::queue::{NextFilters, agent_packet, render_next_text};
use crate::remediation::reducer::STATE_VERIFIED_FIXED;
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
        // Observation id first, then the state line and the body fields.
        let o = packet["observation"].clone();
        println!("{}", args.observation_id);
        println!(
            "title: {}  severity: {}  kind: {}",
            o["title"].as_str().unwrap_or(""),
            o["severity_assertion"].as_str().unwrap_or("-"),
            o["kind_assertion"].as_str().unwrap_or("-")
        );
        println!(
            "state: {}  disposition: {}  handled: {}",
            packet["current_state"]["remediation_status"]
                .as_str()
                .unwrap_or(""),
            packet["current_state"]["disposition"]
                .as_str()
                .unwrap_or("-"),
            packet["current_state"]["handled"]
        );
        if let Some(claim) = packet["current_state"]["active_claim"].as_object() {
            println!(
                "claim: {} by {} (session {}) until {}",
                claim["claim_id"].as_str().unwrap_or("?"),
                claim["claimed_by"].as_str().unwrap_or("?"),
                claim["claim_session_id"].as_str().unwrap_or("?"),
                claim["lease_expires_at"].as_str().unwrap_or("?")
            );
        }
        if let Some(eb) = o["expected_behavior"].as_str() {
            println!("expected: {eb}");
        }
        if let Some(ob) = o["observed_behavior"].as_str() {
            println!("observed: {ob}");
        }
        if let Some(r) = o["reproduction"].as_str() {
            println!("repro: {r}");
        }
        if packet["body_gap"].as_bool() == Some(true) {
            println!("warning: thin body (severity above minor, no expected/observed/repro)");
        }
        let lineage = &packet["lineage"];
        println!(
            "lineage: finding={} tasks={} commits={} receipts={}",
            lineage["finding_id"].as_str().unwrap_or("-"),
            lineage["task_ids"].as_array().map(|v| v.len()).unwrap_or(0),
            lineage["commits"].as_array().map(|v| v.len()).unwrap_or(0),
            lineage["verification_receipts"]
                .as_array()
                .map(|v| v.len())
                .unwrap_or(0)
        );
    }
    Ok(())
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
    for ev in &events {
        let seq = ev["local_sequence"].as_i64().unwrap_or(0);
        let typ = ev["record_type"].as_str().unwrap_or("?");
        println!("{}  {}", seq, typ);
    }
    if events.is_empty() {
        println!("no remediation events for {}", args.observation_id);
    }
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
fn resolve_repo_filter(store: &mut Store, repo: Option<&str>) -> Result<Option<String>> {
    let Some(repo) = repo else { return Ok(None) };
    if repo == "current" {
        let git_ctx = crate::git::collect_git_context(&std::env::current_dir()?)?;
        let res = crate::identity::resolve_repository(store, &git_ctx, None)?;
        if res.repository_id.is_empty() {
            anyhow::bail!("--repo current resolved no repository (not a git worktree?)");
        }
        return Ok(Some(res.repository_id));
    }
    // id-or-alias resolution: exact id first, then confirmed aliases.
    let by_id: Option<String> = store
        .conn
        .query_row(
            "SELECT repository_id FROM repositories WHERE repository_id = ?1",
            rusqlite::params![repo],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = by_id {
        return Ok(Some(id));
    }
    let by_alias: Option<String> = store
        .conn
        .query_row(
            "SELECT repository_id FROM repository_aliases
             WHERE alias = ?1 AND confirmed = 1
             GROUP BY repository_id HAVING COUNT(*) = 1
             ORDER BY repository_id LIMIT 1",
            rusqlite::params![crate::git::normalize_remote_alias(repo)],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = by_alias {
        return Ok(Some(id));
    }
    anyhow::bail!(SnagError::RepositoryNotFound(repo.to_string()));
}

/// `snag review next [filters] [--format agent] [--claim]`
fn next(args: crate::cli::ReviewNextArgs) -> Result<()> {
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let mut store = Store::open_read_write()?;
    let now = utc_now();
    let store_id = store.store_id.clone();
    let repository_id = resolve_repo_filter(&mut store, args.repo.as_deref())?;
    let filters = NextFilters {
        repository_id,
        kind: args.kind,
        severity: args.severity,
        unreviewed: args.unreviewed,
        include_deferred: args.include_deferred,
        my_session: identity.session_id.clone(),
        now: now.clone(),
    };

    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let selected = queue::select_next(&tx, &filters)?;

    let Some(observation_id) = selected else {
        // Typed empty-queue response, not an error. It names the active store
        // so a wrong-store (e.g. leaked XDG_DATA_HOME) is one-glance obvious
        // instead of a baffling empty queue.
        let observation_count: i64 = tx
            .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
            .unwrap_or(0);
        if args.format.as_deref() == Some("agent") {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema_version": 1,
                    "queue": "empty",
                    "store": {
                        "store_id": store.store_id,
                        "db_path": store.db_path.display().to_string(),
                        "observations": observation_count,
                    },
                    "message": "no unhandled observations match the filters",
                }))?
            );
        } else {
            println!("empty queue: no unhandled observations match the filters");
            println!(
                "store: {} ({observation_count} observations)",
                store.db_path.display()
            );
        }
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

/// `snag review list [filters] [--format json]`
fn list(args: crate::cli::ReviewListArgs) -> Result<()> {
    let store = Store::open_read_only()?;
    let mut sql = String::from(
        "SELECT o.observation_id, o.title, o.severity_assertion, o.kind_assertion,
                COALESCE(rs.state, 'unreviewed') AS state,
                rs.disposition, COALESCE(rs.handled, 0) AS handled, rs.active_claim_id,
                c.claim_session_id, c.claimed_by
         FROM observations o
         LEFT JOIN observation_review_state rs ON rs.observation_id = o.observation_id
         LEFT JOIN remediation_claims c ON c.claim_id = rs.active_claim_id
         WHERE 1=1",
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
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
    if args.handled {
        sql.push_str(" AND COALESCE(rs.handled, 0) = 1");
    }
    if args.unhandled {
        sql.push_str(" AND COALESCE(rs.handled, 0) = 0");
    }
    sql.push_str(" ORDER BY o.captured_at ASC, o.local_sequence ASC");

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
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (id, title, sev, kind, state, disp, handled, claim, claim_session, claimed_by) = r?;
        if args.format.as_deref() == Some("json") {
            out.push(serde_json::json!({
                "observation_id": id,
                "title": title,
                "severity": sev,
                "kind": kind,
                "state": state,
                "disposition": disp,
                "handled": handled == 1,
                "active_claim_id": claim,
                "active_claim_session_id": claim_session,
                "active_claim_claimed_by": claimed_by,
            }));
        } else {
            // Observation ids first: they became the cross-session language.
            println!("{}  {}", id, title);
            println!(
                "  state: {}  disposition: {}  severity: {}  handled: {}",
                state,
                disp.as_deref().unwrap_or("-"),
                sev.as_deref().unwrap_or("-"),
                handled == 1
            );
        }
    }
    if args.format.as_deref() == Some("json") {
        println!("{}", serde_json::to_string_pretty(&out)?);
    }
    Ok(())
}
