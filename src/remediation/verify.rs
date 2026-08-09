//! Remediation verification: the materialized projection must match a replay
//! of the record stream, and every remediation invariant must hold.
//!
//! The reducer is the authority; these checks prove the store's derived rows
//! agree with it (and that the stream itself is well-formed).

use crate::remediation::events::*;
use crate::remediation::reducer;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;

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
             'observation_promoted','observation_owner_assigned',
             'remediation_task_attached','remediation_fix_attached',
             'remediation_verification_attached','remediation_marked_handled','remediation_reopened'
         )
         AND NOT EXISTS (SELECT 1 FROM observations o WHERE o.observation_id = r.entity_id)",
        [],
        |r| r.get(0),
    )?;
    if count > 0 {
        anyhow::bail!("{count} remediation record(s) reference missing observations");
    }
    let missing_owner_repositories: i64 = conn.query_row(
        "SELECT COUNT(*) FROM records r
         WHERE r.record_type = 'observation_owner_assigned'
           AND NOT EXISTS (
               SELECT 1 FROM repositories repo
               WHERE repo.repository_id =
                     json_extract(r.canonical_payload_json, '$.owner_repository_id')
           )",
        [],
        |r| r.get(0),
    )?;
    if missing_owner_repositories > 0 {
        anyhow::bail!(
            "{missing_owner_repositories} owner assignment record(s) reference missing repositories"
        );
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
        let touched = load_touched_observation_ids(conn)?;
        let owner_projections = load_owner_projections(conn, Some(&touched))?;
        compare_observations(conn, &reduced, &touched, &owner_projections)?;
        return Ok(());
    }

    let observation_ids = load_observation_ids(conn)?;
    let owner_projections = load_owner_projections(conn, None)?;
    compare_observations(conn, &reduced, &observation_ids, &owner_projections)?;
    verify_unreviewed_rows(conn)
}

fn load_touched_observation_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT entity_id FROM (
             SELECT local_sequence, entity_id FROM records
             ORDER BY local_sequence DESC LIMIT 3
         )",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut touched = Vec::new();
    for row in rows {
        touched.push(row?);
    }
    Ok(touched)
}

fn load_observation_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT observation_id FROM observations")?;
    Ok(stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?)
}

fn compare_observations(
    conn: &Connection,
    reduced: &std::collections::BTreeMap<String, reducer::ReducedObservation>,
    observation_ids: &[String],
    owner_projections: &OwnerProjections,
) -> Result<()> {
    for observation_id in observation_ids {
        compare_observation(conn, reduced, observation_id, owner_projections)?;
    }
    Ok(())
}

fn compare_observation(
    conn: &Connection,
    reduced: &std::collections::BTreeMap<String, reducer::ReducedObservation>,
    observation_id: &str,
    owner_projections: &OwnerProjections,
) -> Result<()> {
    if let Some(expected) = reduced.get(observation_id) {
        compare_row(conn, expected, owner_projections)?;
    } else {
        let expected = reducer::reduce_events(observation_id, &[]);
        compare_row(conn, &expected, owner_projections)?;
    }
    Ok(())
}

fn verify_unreviewed_rows(conn: &Connection) -> Result<()> {
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

type OwnerProjection = (Option<String>, Vec<String>);
type OwnerProjections = HashMap<String, OwnerProjection>;

fn load_owner_projections(
    conn: &Connection,
    observation_ids: Option<&[String]>,
) -> Result<OwnerProjections> {
    let mut sql = String::from(
        "SELECT o.observation_id,
                json_extract(o.canonical_payload_json, '$.owner_repository_id'),
                r.repository_id
         FROM observations o
         LEFT JOIN observation_repositories r
           ON r.observation_id = o.observation_id AND r.role = 'owner'",
    );
    if let Some(observation_ids) = observation_ids {
        if observation_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = (1..=observation_ids.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!(" WHERE o.observation_id IN ({placeholders})"));
    }
    sql.push_str(" ORDER BY o.observation_id, r.repository_id");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = match observation_ids {
        Some(observation_ids) => stmt.query(rusqlite::params_from_iter(observation_ids.iter()))?,
        None => stmt.query([])?,
    };
    let mut projections = HashMap::new();
    while let Some(row) = rows.next()? {
        let observation_id = row.get::<_, String>(0)?;
        let initial_owner = row.get::<_, Option<String>>(1)?;
        let projected_owner = row.get::<_, Option<String>>(2)?;
        let entry = projections
            .entry(observation_id)
            .or_insert_with(|| (initial_owner, Vec::new()));
        if let Some(projected_owner) = projected_owner {
            entry.1.push(projected_owner);
        }
    }
    Ok(projections)
}

fn verify_owner_projection(
    expected: &reducer::ReducedObservation,
    owner_projections: &OwnerProjections,
) -> Result<()> {
    let (initial_owner, owner_rows) =
        owner_projections
            .get(&expected.observation_id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "observation {} owner mismatch: materialized [], replay {:?}",
                    expected.observation_id,
                    expected.owner_repository_id
                )
            })?;
    let expected_owner = expected
        .owner_repository_id
        .as_ref()
        .or(initial_owner.as_ref());
    let owner_matches = match expected_owner {
        Some(owner_repository_id) => owner_rows.len() == 1 && owner_rows[0] == *owner_repository_id,
        None => owner_rows.is_empty(),
    };
    if !owner_matches {
        anyhow::bail!(
            "observation {} owner mismatch: materialized {:?}, replay {:?}",
            expected.observation_id,
            owner_rows,
            expected_owner
        );
    }
    Ok(())
}

fn load_materialized_row(
    conn: &Connection,
    observation_id: &str,
) -> Result<Option<MaterializedRow>> {
    Ok(conn
        .query_row(
            "SELECT state, disposition, handled, active_claim_id, task_ids_json,
                    commits_json, verification_receipts_json, latest_verification_status,
                    updated_through_sequence
             FROM observation_review_state WHERE observation_id = ?1",
            rusqlite::params![observation_id],
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
        .optional()?)
}

/// Compare one reduced observation against its materialized row. The reducer's
/// `last_event_sequence == 0` means the observation has no remediation events:
/// the row must then be absent or the unreviewed migration backfill.
fn compare_row(
    conn: &Connection,
    expected: &reducer::ReducedObservation,
    owner_projections: &OwnerProjections,
) -> Result<()> {
    let row = load_materialized_row(conn, &expected.observation_id)?;
    verify_owner_projection(expected, owner_projections)?;

    if expected.last_event_sequence == 0 {
        verify_eventless_row(&expected.observation_id, row.as_ref())?;
        return Ok(());
    }

    let row = row.ok_or_else(|| {
        anyhow::anyhow!(
            "observation {} has remediation events but no materialized state row",
            expected.observation_id
        )
    })?;
    compare_materialized_fields(expected, row)
}

fn verify_eventless_row(observation_id: &str, row: Option<&MaterializedRow>) -> Result<()> {
    if let Some((state, disposition, handled, ..)) = row
        && (state != reducer::STATE_UNREVIEWED || disposition.is_some() || *handled != 0)
    {
        anyhow::bail!(
            "observation {} has no remediation events but a non-unreviewed state row",
            observation_id
        );
    }
    Ok(())
}

fn compare_materialized_fields(
    expected: &reducer::ReducedObservation,
    row: MaterializedRow,
) -> Result<()> {
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
    ) = row;
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
