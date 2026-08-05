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

use crate::cli::ReviewCommand;
use crate::error::SnagError;
use crate::record::RecordPayload;
use crate::remediation::events::*;
use crate::remediation::identity::{RemediationIdentity, lease_expiry, resolve_identity, utc_now};
use crate::remediation::queue::{NextFilters, agent_packet, render_next_text};
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
fn claim(args: crate::cli::ReviewClaimArgs) -> Result<()> {
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let lease_seconds = match &args.lease {
        Some(raw) => identity::parse_duration(raw).map_err(SnagError::Validation)?,
        None => identity::default_lease_seconds(),
    };
    let mut store = Store::open_read_write()?;
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
fn release(args: crate::cli::ReviewReleaseArgs) -> Result<()> {
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let mut store = Store::open_read_write()?;
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
fn heartbeat(args: crate::cli::ReviewHeartbeatArgs) -> Result<()> {
    let identity = resolve_identity(args.reviewer.as_deref(), args.session_id.as_deref());
    let lease_seconds = match &args.lease {
        Some(raw) => identity::parse_duration(raw).map_err(SnagError::Validation)?,
        None => identity::default_lease_seconds(),
    };
    let mut store = Store::open_read_write()?;
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
        // Typed empty-queue response, not an error.
        if args.format.as_deref() == Some("agent") {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema_version": 1,
                    "queue": "empty",
                    "message": "no unhandled observations match the filters",
                }))?
            );
        } else {
            println!("empty queue: no unhandled observations match the filters");
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
                rs.disposition, COALESCE(rs.handled, 0) AS handled, rs.active_claim_id
         FROM observations o
         LEFT JOIN observation_review_state rs ON rs.observation_id = o.observation_id
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
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (id, title, sev, kind, state, disp, handled, claim) = r?;
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
