//! Remediation verification: the materialized projection must match a replay
//! of the record stream, and every remediation invariant must hold.
//!
//! The reducer is the authority; these checks prove the store's derived rows
//! agree with it (and that the stream itself is well-formed).

use crate::remediation::events::*;
use crate::remediation::reducer;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

/// Verify every remediation invariant. `quick` restricts the materialized-vs-
/// replay comparison to observations touched by the bounded record suffix
/// (the full chain walk is always part of `snag verify --full`).
pub fn verify_remediation(conn: &Connection, quick: bool) -> Result<()> {
    verify_record_references(conn)?;
    verify_claim_leases(conn)?;
    verify_disposition_and_relationship_references(conn)?;
    verify_relationship_acyclicity(conn)?;
    verify_links(conn)?;
    verify_materialized_state(conn, quick)?;
    Ok(())
}

/// Every remediation record must reference an existing observation.
fn verify_record_references(conn: &Connection) -> Result<()> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM records r
         WHERE r.record_type IN (
             'observation_claimed','observation_claim_heartbeat','observation_claim_released',
             'observation_claim_expired','observation_reviewed','observation_disposition_set',
             'observation_reopened','observation_relationship_added','observation_relationship_retracted',
             'observation_promoted','remediation_task_attached','remediation_fix_attached',
             'remediation_verification_attached','remediation_marked_handled','remediation_reopened'
         )
         AND NOT EXISTS (SELECT 1 FROM observations o WHERE o.observation_id = r.entity_id)",
        [],
        |r| r.get(0),
    )?;
    if count > 0 {
        anyhow::bail!("{count} remediation record(s) reference missing observations");
    }
    Ok(())
}

/// Claim records: lease intervals are valid and no observation holds more than
/// one active unexpired claim.
fn verify_claim_leases(conn: &Connection) -> Result<()> {
    let bad_interval: i64 = conn.query_row(
        "SELECT COUNT(*) FROM remediation_claims WHERE lease_expires_at <= claimed_at",
        [],
        |r| r.get(0),
    )?;
    if bad_interval > 0 {
        anyhow::bail!("{bad_interval} claim(s) have invalid lease intervals");
    }
    let released_before_claimed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM remediation_claims WHERE released_at IS NOT NULL AND released_at < claimed_at",
        [],
        |r| r.get(0),
    )?;
    if released_before_claimed > 0 {
        anyhow::bail!("{released_before_claimed} claim(s) released before claimed");
    }
    // No more than one active unexpired claim per observation (live now).
    let too_many: i64 = conn.query_row(
        "SELECT COUNT(*) FROM (
            SELECT observation_id FROM remediation_claims
            WHERE released_at IS NULL
              AND lease_expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
            GROUP BY observation_id HAVING COUNT(*) > 1
        )",
        [],
        |r| r.get(0),
    )?;
    if too_many > 0 {
        anyhow::bail!("{too_many} observation(s) hold more than one active claim");
    }
    Ok(())
}

/// Disposition targets and relationship endpoints must exist (the normalized
/// tables carry FKs; this checks the records-derived rows explicitly).
fn verify_disposition_and_relationship_references(conn: &Connection) -> Result<()> {
    let bad_targets: i64 = conn.query_row(
        "SELECT COUNT(*) FROM observation_dispositions d
         WHERE d.target_observation_id IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM observations o WHERE o.observation_id = d.target_observation_id)",
        [],
        |r| r.get(0),
    )?;
    if bad_targets > 0 {
        anyhow::bail!("{bad_targets} disposition(s) reference missing targets");
    }
    let bad_endpoints: i64 = conn.query_row(
        "SELECT COUNT(*) FROM observation_relationships r
         WHERE NOT EXISTS (SELECT 1 FROM observations o WHERE o.observation_id = r.left_observation_id)
            OR NOT EXISTS (SELECT 1 FROM observations o WHERE o.observation_id = r.right_observation_id)",
        [],
        |r| r.get(0),
    )?;
    if bad_endpoints > 0 {
        anyhow::bail!("{bad_endpoints} relationship(s) reference missing endpoints");
    }
    Ok(())
}

/// Directional relationship types must be acyclic (per type, following live
/// edges only). A node that can reach itself closes a cycle.
fn verify_relationship_acyclicity(conn: &Connection) -> Result<()> {
    for relation in DIRECTIONAL_RELATIONSHIPS {
        let cycle: i64 = conn.query_row(
            "WITH RECURSIVE walk(node, start) AS (
                 SELECT right_observation_id, left_observation_id FROM observation_relationships
                 WHERE relation = ?1 AND retracted_by_record_sequence IS NULL
                 UNION
                 SELECT r.right_observation_id, w.start FROM observation_relationships r
                 JOIN walk w ON r.left_observation_id = w.node
                 WHERE r.relation = ?1 AND r.retracted_by_record_sequence IS NULL
             )
             SELECT COUNT(*) FROM walk WHERE node = start",
            rusqlite::params![relation],
            |r| r.get(0),
        )?;
        if cycle > 0 {
            anyhow::bail!("{relation} relationship graph contains a cycle");
        }
    }
    Ok(())
}

/// Remediation links are structurally valid.
fn verify_links(conn: &Connection) -> Result<()> {
    let bad: i64 = conn.query_row(
        "SELECT COUNT(*) FROM remediation_links
         WHERE link_type NOT IN ('finding', 'task', 'commit', 'verification')
            OR target_id IS NULL OR target_id = ''
            OR (link_type = 'commit' AND (repository_id IS NULL OR repository_id = ''))
            OR (link_type = 'verification' AND status NOT IN ('accepted','rejected','abstained','invalid','unknown'))",
        [],
        |r| r.get(0),
    )?;
    if bad > 0 {
        anyhow::bail!("{bad} remediation link(s) are structurally invalid");
    }
    Ok(())
}

/// The materialized review-state projection must match a pure replay of the
/// stream. In quick mode only observations touched by the trailing suffix are
/// compared (plus the same structural checks above).
fn verify_materialized_state(conn: &Connection, quick: bool) -> Result<()> {
    let reduced = reducer::replay_all(conn)?;

    if quick {
        // Bounded scope: observations touched by the last 3 records.
        let touched: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT entity_id FROM (
                     SELECT local_sequence, entity_id FROM records
                     ORDER BY local_sequence DESC LIMIT 3
                 )",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        };
        for obs in touched {
            if let Some(expected) = reduced.get(&obs) {
                compare_row(conn, expected)?;
            }
        }
        return Ok(());
    }

    for expected in reduced.values() {
        compare_row(conn, expected)?;
    }
    // Observations with no remediation events must not carry a stale row
    // (the migration backfill is the only allowed non-empty shape: unreviewed,
    // unhandled, no disposition). Anything else requires backing events.
    let unreviewed_with_events: i64 = conn.query_row(
        "SELECT COUNT(*) FROM observation_review_state rs
         WHERE (rs.state != 'unreviewed' OR rs.handled != 0 OR rs.disposition IS NOT NULL)
           AND NOT EXISTS (
               SELECT 1 FROM records r WHERE r.entity_id = rs.observation_id
               AND r.record_type IN (
                   'observation_claimed','observation_claim_heartbeat','observation_claim_released',
                   'observation_claim_expired','observation_reviewed','observation_disposition_set',
                   'observation_reopened','observation_relationship_added','observation_relationship_retracted',
                   'observation_promoted','remediation_task_attached','remediation_fix_attached',
                   'remediation_verification_attached','remediation_marked_handled','remediation_reopened'
               )
           )",
        [],
        |r| r.get(0),
    )?;
    if unreviewed_with_events > 0 {
        anyhow::bail!(
            "{unreviewed_with_events} review-state row(s) with no backing remediation events"
        );
    }
    Ok(())
}

/// The materialized review-state row, in SELECT order.
type MaterializedRow = (
    String,
    Option<String>,
    i64,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
    i64,
);

/// Compare one reduced observation against its materialized row. The reducer's
/// `last_event_sequence == 0` means the observation has no remediation events:
/// the row must then be absent or the unreviewed migration backfill.
fn compare_row(conn: &Connection, expected: &reducer::ReducedObservation) -> Result<()> {
    let row: Option<MaterializedRow> = conn
        .query_row(
            "SELECT state, disposition, handled, active_claim_id, task_ids_json,
                    commits_json, verification_receipts_json, latest_verification_status,
                    updated_through_sequence
             FROM observation_review_state WHERE observation_id = ?1",
            rusqlite::params![expected.observation_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
        .optional()?;

    if expected.last_event_sequence == 0 {
        if let Some((state, disposition, handled, ..)) = row
            && (state != reducer::STATE_UNREVIEWED || disposition.is_some() || handled != 0)
        {
            anyhow::bail!(
                "observation {} has no remediation events but a non-unreviewed state row",
                expected.observation_id
            );
        }
        return Ok(());
    }

    let (
        state,
        disposition,
        handled,
        active_claim_id,
        task_json,
        commits_json,
        receipts_json,
        latest,
        seq,
    ) = row.ok_or_else(|| {
        anyhow::anyhow!(
            "observation {} has remediation events but no materialized state row",
            expected.observation_id
        )
    })?;
    if state != expected.state {
        anyhow::bail!(
            "observation {} state mismatch: materialized {state}, replay {}",
            expected.observation_id,
            expected.state
        );
    }
    if disposition != expected.disposition {
        anyhow::bail!(
            "observation {} disposition mismatch: materialized {:?}, replay {:?}",
            expected.observation_id,
            disposition,
            expected.disposition
        );
    }
    if handled != expected.handled as i64 {
        anyhow::bail!(
            "observation {} handled mismatch: materialized {handled}, replay {}",
            expected.observation_id,
            expected.handled
        );
    }
    if active_claim_id != expected.active_claim.as_ref().map(|c| c.claim_id.clone()) {
        anyhow::bail!(
            "observation {} active-claim mismatch: materialized {:?}, replay {:?}",
            expected.observation_id,
            active_claim_id,
            expected.active_claim.as_ref().map(|c| c.claim_id.clone())
        );
    }
    if serde_json::from_str::<Vec<String>>(&task_json)? != expected.task_ids {
        anyhow::bail!(
            "observation {} task lineage mismatch",
            expected.observation_id
        );
    }
    if serde_json::from_str::<Vec<reducer::CommitLink>>(&commits_json)? != expected.commits {
        anyhow::bail!(
            "observation {} commit lineage mismatch",
            expected.observation_id
        );
    }
    if serde_json::from_str::<Vec<reducer::VerificationReceipt>>(&receipts_json)?
        != expected.verification_receipts
    {
        anyhow::bail!(
            "observation {} verification lineage mismatch",
            expected.observation_id
        );
    }
    if latest != expected.latest_verification_status {
        anyhow::bail!(
            "observation {} latest verification mismatch",
            expected.observation_id
        );
    }
    if seq != expected.last_event_sequence {
        anyhow::bail!(
            "observation {} updated-through mismatch: materialized {seq}, replay {}",
            expected.observation_id,
            expected.last_event_sequence
        );
    }
    // verified_fixed requires accepted verification evidence (direct check).
    if state == reducer::STATE_VERIFIED_FIXED && latest.as_deref() != Some(VERIFY_ACCEPTED) {
        anyhow::bail!(
            "observation {} is verified_fixed without an accepted receipt",
            expected.observation_id
        );
    }
    Ok(())
}
