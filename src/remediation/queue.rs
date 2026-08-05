//! Review queue retrieval: deterministic `next` selection and the versioned
//! agent packet.
//!
//! The queue is a projection over the reducer's materialized state; ordering
//! is deterministic (severity rank desc, captured_at asc, sequence asc) and
//! never learned (no ranking or materiality scoring lives here — that is a
//! downstream findings-layer concern).

use crate::record::RecordPayload;
use crate::remediation::events::*;
use crate::store::Store;
use anyhow::Result;
use rusqlite::OptionalExtension;
use serde_json::json;

/// Filters for `snag review next`.
#[derive(Debug, Default, Clone)]
pub struct NextFilters {
    pub repository_id: Option<String>,
    pub kind: Option<String>,
    pub severity: Option<String>,
    pub unreviewed: bool,
    pub include_deferred: bool,
    pub my_session: String,
    pub now: String,
}

/// The selected observation id, or None for an empty queue.
pub fn select_next(conn: &rusqlite::Connection, f: &NextFilters) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT o.observation_id
         FROM observations o
         LEFT JOIN observation_review_state rs ON rs.observation_id = o.observation_id
         WHERE NOT EXISTS (
             SELECT 1 FROM records r
             WHERE r.entity_id = o.observation_id AND r.record_type = 'observation_retracted'
         )
         AND (
             rs.observation_id IS NULL
             OR rs.handled = 0
             OR (?1 = 1 AND rs.state = 'deferred')
         )
         AND NOT EXISTS (
             SELECT 1 FROM remediation_claims c
             WHERE c.observation_id = o.observation_id
               AND c.released_at IS NULL
               AND c.lease_expires_at > ?2
               AND c.claim_session_id != ?3
         )
         AND (?4 IS NULL OR EXISTS (
             SELECT 1 FROM observation_repositories or2
             WHERE or2.observation_id = o.observation_id AND or2.repository_id = ?4
         ))
         AND (?5 IS NULL OR o.kind_assertion = ?5)
         AND (?6 IS NULL OR o.severity_assertion = ?6)
         AND (?7 = 0 OR rs.observation_id IS NULL OR rs.state = 'unreviewed')
         ORDER BY
           CASE o.severity_assertion
             WHEN 'blocker' THEN 5 WHEN 'major' THEN 4 WHEN 'medium' THEN 3
             WHEN 'minor' THEN 2 WHEN 'low' THEN 1 ELSE 0 END DESC,
           o.captured_at ASC,
           o.local_sequence ASC
         LIMIT 1",
    )?;
    let row = stmt
        .query_row(
            rusqlite::params![
                f.include_deferred as i64,
                f.now,
                f.my_session,
                f.repository_id,
                f.kind,
                f.severity,
                f.unreviewed as i64,
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(row)
}

/// Body-gap signal (receiver-side ergonomics fold-in): an observation whose
/// asserted severity is above minor but which carries none of the
/// reconstruction fields (expected/observed/repro). The remediator should
/// expect reconstruction to cost more; the reporter is never blocked at
/// filing time.
pub fn body_gap(o: &crate::types::Observation) -> bool {
    let above_minor = matches!(
        o.severity_assertion.as_deref(),
        Some("major" | "medium" | "blocker")
    );
    above_minor
        && o.expected_behavior.is_none()
        && o.observed_behavior.is_none()
        && o.reproduction.is_none()
}

/// Load the observation row + canonical payload for the agent packet.
fn load_observation(store: &Store, observation_id: &str) -> Result<crate::types::Observation> {
    let payload_json: String = store
        .conn
        .query_row(
            "SELECT canonical_payload_json FROM observations WHERE observation_id = ?1",
            rusqlite::params![observation_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("Observation not found: {observation_id}"))?;
    let payload: RecordPayload = serde_json::from_str(&payload_json)?;
    match payload {
        RecordPayload::Observation(o) => Ok(o),
        _ => anyhow::bail!("Observation {observation_id} has a non-observation payload"),
    }
}

/// Build the versioned agent packet for one observation.
///
/// Reporter assertions stay assertions (the packet never converts them into
/// canonical facts). `current_state` is the reducer's projection; lineage and
/// relationships come from the materialized remediation tables. The packet
/// identifies its store so a fresh agent can never mistake a scratch store's
/// empty queue for the real one.
pub fn agent_packet(store: &Store, observation_id: &str) -> Result<serde_json::Value> {
    let observation = load_observation(store, observation_id)?;
    let reduced = crate::remediation::reducer::reduce_observation(&store.conn, observation_id)?;
    let observation_count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
        .unwrap_or(0);

    let repositories: Vec<String> = {
        let mut stmt = store.conn.prepare(
            "SELECT repository_id FROM observation_repositories WHERE observation_id = ?1 ORDER BY role, repository_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![observation_id], |row| {
            row.get::<_, String>(0)
        })?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r?);
        }
        v
    };

    let history: Vec<serde_json::Value> = {
        let mut stmt = store.conn.prepare(
            "SELECT local_sequence, record_type, canonical_payload_json
             FROM records WHERE entity_id = ?1 ORDER BY local_sequence ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![observation_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut v = Vec::new();
        for row in rows {
            let (seq, typ, payload_json) = row?;
            if !REMEDIATION_RECORD_TYPES.contains(&typ.as_str()) {
                continue;
            }
            v.push(json!({
                "local_sequence": seq,
                "record_type": typ,
                "payload": serde_json::from_str::<serde_json::Value>(&payload_json)?,
            }));
        }
        v
    };

    Ok(json!({
        "schema_version": 1,
        "store": {
            "store_id": store.store_id,
            "db_path": store.db_path.display().to_string(),
            "observations": observation_count,
        },
        "observation": observation,
        "current_state": {
            "disposition": reduced.disposition,
            "handled": reduced.handled,
            "active_claim": reduced.active_claim.as_ref().map(|c| json!({
                "claim_id": c.claim_id,
                "claimed_by": c.claimed_by,
                "claim_session_id": c.claim_session_id,
                "lease_expires_at": c.lease_expires_at,
            })),
            "remediation_status": reduced.state,
        },
        "relationships": [],
        "remediation_history": history,
        "repositories": repositories,
        "artifacts": observation
            .artifacts
            .iter()
            .map(|a| json!({"digest": a.digest, "byte_length": a.byte_length, "media_type": a.media_type, "original_name": a.original_name}))
            .collect::<Vec<_>>(),
        "lineage": {
            "finding_id": reduced.promoted_finding_id,
            "task_ids": reduced.task_ids,
            "commits": reduced.commits,
            "verification_receipts": reduced.verification_receipts,
        },
        "body_gap": body_gap(&observation),
        "allowed_actions": [],
    }))
}

/// Human-readable next output: observation id first (the cross-session
/// conversation language from the dogfood report), then the state line.
pub fn render_next_text(
    observation_id: &str,
    reduced: &crate::remediation::reducer::ReducedObservation,
) {
    println!("{}", observation_id);
    println!(
        "state: {}  disposition: {}  handled: {}",
        reduced.state,
        reduced.disposition.as_deref().unwrap_or("-"),
        reduced.handled
    );
}
