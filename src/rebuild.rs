use crate::cli::RebuildArgs;
use crate::failpoint::failpoint;
use crate::record::{CanonicalRecordV1, RecordPayload};
use crate::remediation::events::RemediationEvent;
use crate::store::Store;
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};

/// Parse and validate the export header; return (store_id, first_sequence,
/// predecessor hash, head hash, record_count).
fn parse_and_validate_header(header_line: &str) -> Result<(String, u64, String, String, u64)> {
    let header: Value = serde_json::from_str(header_line)?;
    if header.get("export_kind").and_then(|v| v.as_str()) != Some("export_header") {
        anyhow::bail!("Invalid header kind");
    }
    if header.get("export_schema_version").and_then(|v| v.as_i64()) != Some(1) {
        anyhow::bail!("Unsupported export schema version");
    }
    let store_id = header
        .get("store_id")
        .and_then(|v| v.as_str())
        .context("Missing store_id")?
        .to_string();
    let first_sequence = header
        .get("first_sequence")
        .and_then(|v| v.as_u64())
        .context("Missing first_sequence")?;
    let predecessor_hash = header
        .get("previous_checkpoint_hash")
        .and_then(|v| v.as_str())
        .context("Missing previous_checkpoint_hash")?
        .to_string();
    let head_hash = header
        .get("head_record_hash")
        .and_then(|v| v.as_str())
        .context("Missing head_record_hash")?
        .to_string();
    let count = header
        .get("record_count")
        .and_then(|v| v.as_u64())
        .context("Missing record_count")?;
    // The reader understands export protocol 1 with remediation records
    // (minimum_reader_version 2); anything newer is rejected cleanly.
    if let Some(min_v) = header
        .get("minimum_reader_version")
        .and_then(|v| v.as_i64())
        && min_v > 2
    {
        anyhow::bail!("Unsupported minimum reader version: {min_v}");
    }
    Ok((store_id, first_sequence, predecessor_hash, head_hash, count))
}

/// Insert the normalized observation projection for a replayed record.
fn reconstruct_observation(
    tx: &rusqlite::Transaction,
    obs: &crate::types::Observation,
    local_sequence: u64,
    canonical_payload_json: &str,
    previous_record_hash: &str,
    computed_hash: &str,
    captured_at: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO observations (
            observation_id, store_id, local_sequence, schema_version, captured_at, source_kind,
            idempotency_key, title, kind_assertion, severity_assertion, expected_behavior,
            observed_behavior, reproduction, workaround, impact, confidence, sensitivity, context_json,
            canonical_payload_json, previous_record_hash, record_hash, semantic_digest, labels_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
        rusqlite::params![
            &obs.observation_id,
            &obs.store_id,
            local_sequence as i64,
            obs.schema_version,
            &obs.created_at,
            &obs.source.kind,
            &obs.idempotency_key,
            &obs.title,
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
            &canonical_payload_json,
            &previous_record_hash,
            &computed_hash,
            &crate::idempotency::observation_semantic_digest(obs),
            serde_json::to_string(&obs.labels).unwrap_or_else(|_| "null".to_string()),
        ],
    )?;

    // The primary repository (the owner lane) is persisted in the canonical
    // payload's context.repository.repository_id by the reporter
    // (report.rs resolve_identity). Rebuild must re-insert it with
    // role='primary' — inserting only `affected_repository_ids` as
    // 'affected' silently destroys owner attribution on every rebuild.
    if let Some(primary) = obs
        .context
        .repository
        .as_ref()
        .and_then(|r| r.repository_id.clone())
        .filter(|id| !id.is_empty())
    {
        tx.execute(
            "INSERT OR REPLACE INTO repositories (repository_id, created_at)
             VALUES (?1, COALESCE((SELECT created_at FROM repositories WHERE repository_id = ?1), ?2))",
            rusqlite::params![&primary, &captured_at],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO observation_repositories (observation_id, repository_id, role) VALUES (?1, ?2, 'primary')",
            rusqlite::params![&obs.observation_id, &primary],
        )?;
    }

    for repo_id in &obs.affected_repository_ids {
        tx.execute(
            "INSERT OR REPLACE INTO repositories (repository_id, created_at)
             VALUES (?1, COALESCE((SELECT created_at FROM repositories WHERE repository_id = ?1), ?2))",
            rusqlite::params![repo_id, &captured_at],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO observation_repositories (observation_id, repository_id, role) VALUES (?1, ?2, 'affected')",
            rusqlite::params![&obs.observation_id, repo_id],
        )?;
    }

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
    Ok(())
}

/// Validate one record envelope, insert it into the records stream without
/// REPLACE, and reconstruct its normalized state. Returns (next_sequence,
/// next_hash).
fn verify_and_insert_record(
    tx: &rusqlite::Transaction,
    line: &str,
    store_id: &str,
    current_expected_sequence: u64,
    current_previous_hash: &str,
) -> Result<(u64, String)> {
    let envelope: Value = serde_json::from_str(line)?;
    if envelope.get("export_kind").and_then(|v| v.as_str()) != Some("record") {
        anyhow::bail!("Invalid record kind");
    }
    if envelope
        .get("record_schema_version")
        .and_then(|v| v.as_i64())
        != Some(1)
    {
        anyhow::bail!("Unsupported record schema version");
    }
    let local_sequence = envelope
        .get("local_sequence")
        .and_then(|v| v.as_u64())
        .context("Missing local_sequence")?;
    let record_id = envelope
        .get("record_id")
        .and_then(|v| v.as_str())
        .context("Missing record_id")?
        .to_string();
    let record_type = envelope
        .get("record_type")
        .and_then(|v| v.as_str())
        .context("Missing record_type")?
        .to_string();
    let entity_id = envelope
        .get("entity_id")
        .and_then(|v| v.as_str())
        .context("Missing entity_id")?
        .to_string();
    let captured_at = envelope
        .get("captured_at")
        .and_then(|v| v.as_str())
        .context("Missing captured_at")?
        .to_string();
    let previous_record_hash = envelope
        .get("previous_record_hash")
        .and_then(|v| v.as_str())
        .context("Missing previous_record_hash")?
        .to_string();
    let envelope_hash = envelope
        .get("record_hash")
        .and_then(|v| v.as_str())
        .context("Missing record_hash")?
        .to_string();
    let payload_val = envelope
        .get("canonical_payload")
        .context("Missing canonical_payload")?;
    let payload: crate::record::RecordPayload = serde_json::from_value(payload_val.clone())?;

    if local_sequence != current_expected_sequence {
        anyhow::bail!(
            "Non-contiguous sequence: expected {}, got {}",
            current_expected_sequence,
            local_sequence
        );
    }
    if previous_record_hash != current_previous_hash {
        anyhow::bail!(
            "Predecessor hash mismatch at sequence {}: expected {}, got {}",
            local_sequence,
            current_previous_hash,
            previous_record_hash
        );
    }
    let canonical_record = CanonicalRecordV1 {
        local_sequence,
        record_id: record_id.clone(),
        record_type: record_type.clone(),
        entity_id: entity_id.clone(),
        captured_at: captured_at.clone(),
        payload: payload.clone(),
    };
    let computed_hash = canonical_record.compute_hash(store_id, &previous_record_hash);
    if computed_hash != envelope_hash {
        anyhow::bail!("Record hash mismatch at sequence {}", local_sequence);
    }
    let canonical_payload_json = serde_json::to_string(&payload)?;

    // Insert without REPLACE.
    tx.execute(
        "INSERT INTO records (local_sequence, record_id, record_type, entity_id, captured_at, canonical_payload_json, previous_record_hash, record_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            local_sequence as i64,
            &record_id,
            &record_type,
            &entity_id,
            &captured_at,
            &canonical_payload_json,
            &previous_record_hash,
            &computed_hash,
        ],
    )?;

    // Reconstruct normalized state.
    match payload {
        crate::record::RecordPayload::Observation(obs) => {
            reconstruct_observation(
                tx,
                &obs,
                local_sequence,
                &canonical_payload_json,
                &previous_record_hash,
                &computed_hash,
                &captured_at,
            )?;
        }
        crate::record::RecordPayload::Retraction(ret) => {
            tx.execute(
                "INSERT INTO observation_actions (action_id, observation_id, action_type, action_payload_json, created_at, local_sequence, previous_record_hash, record_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    &record_id,
                    &entity_id,
                    "retracted",
                    &serde_json::to_string(&ret)?,
                    &captured_at,
                    local_sequence as i64,
                    &previous_record_hash,
                    &computed_hash,
                ],
            )?;
        }
        // Remediation events reconstruct their normalized rows (the
        // materialized `observation_review_state` projection is rebuilt by the
        // reducer replay in `handle`).
        RecordPayload::Remediation(ev) => {
            reconstruct_remediation_event(tx, &record_id, &entity_id, local_sequence, ev)?;
        }
    }
    Ok((current_expected_sequence + 1, computed_hash))
}

/// Reconstruct the normalized rows for one remediation event during rebuild.
///
/// Claims, dispositions, relationships, and remediation links map one-to-one
/// onto their tables; state-only events (reviewed, marked handled, remediation
/// reopened) contribute nothing here because the materialized review-state
/// projection is rebuilt by the reducer replay in `handle`.
fn reconstruct_remediation_event(
    tx: &rusqlite::Transaction,
    record_id: &str,
    entity_id: &str,
    local_sequence: u64,
    ev: RemediationEvent,
) -> Result<()> {
    match ev {
        RemediationEvent::Claimed(p) => {
            tx.execute(
                "INSERT INTO remediation_claims (
                    claim_id, observation_id, claimed_by, claim_session_id, claimed_at,
                    lease_expires_at, source_record_sequence, idempotency_key
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    &p.claim_id,
                    entity_id,
                    &p.claimed_by,
                    &p.claim_session_id,
                    &p.created_at,
                    &p.lease_expires_at,
                    local_sequence as i64,
                    &p.idempotency_key,
                ],
            )?;
        }
        RemediationEvent::ClaimHeartbeat(p) => {
            tx.execute(
                "UPDATE remediation_claims SET lease_expires_at = ?1
                 WHERE claim_id = ?2 AND released_at IS NULL",
                rusqlite::params![&p.lease_expires_at, &p.claim_id],
            )?;
        }
        RemediationEvent::ClaimReleased(p) => {
            tx.execute(
                "UPDATE remediation_claims SET released_at = ?1, release_reason = ?2
                 WHERE claim_id = ?3",
                rusqlite::params![&p.released_at, &p.release_reason, &p.claim_id],
            )?;
        }
        RemediationEvent::ClaimExpired(p) => {
            tx.execute(
                "UPDATE remediation_claims SET released_at = ?1, release_reason = 'lease expired'
                 WHERE claim_id = ?2 AND released_at IS NULL",
                rusqlite::params![&p.expired_at, &p.claim_id],
            )?;
        }
        RemediationEvent::Reviewed(_) => {
            // History/audit surface; the disposition row follows from the
            // disposition-set event.
        }
        RemediationEvent::DispositionSet(p) => {
            tx.execute(
                "INSERT INTO observation_dispositions (
                    disposition_id, observation_id, disposition, target_observation_id,
                    rationale, evidence_json, reviewer, review_session_id, created_at,
                    source_record_sequence, idempotency_key
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    &p.disposition_id,
                    entity_id,
                    &p.disposition,
                    &p.target_observation_id,
                    &p.rationale,
                    &p.evidence_json,
                    &p.reviewer,
                    &p.review_session_id,
                    &p.created_at,
                    local_sequence as i64,
                    &p.idempotency_key,
                ],
            )?;
        }
        RemediationEvent::Reopened(_) => {
            tx.execute(
                "UPDATE observation_dispositions SET retracted_by_record_sequence = ?1
                 WHERE observation_id = ?2 AND retracted_by_record_sequence IS NULL",
                rusqlite::params![local_sequence as i64, &entity_id],
            )?;
        }
        RemediationEvent::RelationshipAdded(p) => {
            tx.execute(
                "INSERT INTO observation_relationships (
                    relationship_id, left_observation_id, right_observation_id, relation,
                    rationale, evidence_json, reviewer, review_session_id, created_at,
                    source_record_sequence, idempotency_key
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    &p.relationship_id,
                    &p.left_observation_id,
                    &p.right_observation_id,
                    &p.relation,
                    &p.rationale,
                    &p.evidence_json,
                    &p.reviewer,
                    &p.review_session_id,
                    &p.created_at,
                    local_sequence as i64,
                    &p.idempotency_key,
                ],
            )?;
        }
        RemediationEvent::RelationshipRetracted(p) => {
            tx.execute(
                "UPDATE observation_relationships SET retracted_by_record_sequence = ?1
                 WHERE relationship_id = ?2 AND retracted_by_record_sequence IS NULL",
                rusqlite::params![local_sequence as i64, &p.relationship_id],
            )?;
        }
        RemediationEvent::Promoted(p) => {
            tx.execute(
                "INSERT INTO remediation_links (
                    link_id, observation_id, link_type, target_id, created_at,
                    source_record_sequence, idempotency_key
                ) VALUES (?1, ?2, 'finding', ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    &record_id,
                    entity_id,
                    &p.finding_id,
                    &p.created_at,
                    local_sequence as i64,
                    &p.idempotency_key,
                ],
            )?;
        }
        RemediationEvent::TaskAttached(p) => {
            tx.execute(
                "INSERT INTO remediation_links (
                    link_id, observation_id, link_type, target_id, created_at,
                    source_record_sequence, idempotency_key
                ) VALUES (?1, ?2, 'task', ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    &record_id,
                    entity_id,
                    &p.task_id,
                    &p.created_at,
                    local_sequence as i64,
                    &p.idempotency_key,
                ],
            )?;
        }
        RemediationEvent::FixAttached(p) => {
            tx.execute(
                "INSERT INTO remediation_links (
                    link_id, observation_id, link_type, target_id, repository_id, created_at,
                    source_record_sequence, idempotency_key
                ) VALUES (?1, ?2, 'commit', ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    &record_id,
                    entity_id,
                    &p.commit_sha,
                    &p.repository_id,
                    &p.created_at,
                    local_sequence as i64,
                    &p.idempotency_key,
                ],
            )?;
        }
        RemediationEvent::VerificationAttached(p) => {
            tx.execute(
                "INSERT INTO remediation_links (
                    link_id, observation_id, link_type, target_id, status, created_at,
                    source_record_sequence, idempotency_key
                ) VALUES (?1, ?2, 'verification', ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    &record_id,
                    entity_id,
                    &p.receipt_ref,
                    &p.status,
                    &p.created_at,
                    local_sequence as i64,
                    &p.idempotency_key,
                ],
            )?;
        }
        RemediationEvent::MarkedHandled(_) | RemediationEvent::RemediationReopened(_) => {
            // State-only events; the reducer replay reconstructs the
            // materialized projection.
        }
    }
    Ok(())
}

/// Atomically finalize the rebuilt destination after dropping open connections.
fn finalize_destination(
    store: Store,
    temp_dest: &std::path::PathBuf,
    destination: &std::path::Path,
) -> Result<()> {
    failpoint("rebuild_before_publication");
    drop(store);
    std::fs::rename(temp_dest, destination)?;
    Ok(())
}

pub fn handle(args: RebuildArgs) -> Result<()> {
    let file = File::open(&args.from_export)?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    // 1. Parse and validate the header.
    let header_line = lines.next().transpose()?.context("Stream is empty")?;
    let (
        store_id,
        expected_first_sequence,
        expected_predecessor_hash,
        expected_head_hash,
        expected_count,
    ) = parse_and_validate_header(&header_line)?;

    // 3. Create a fresh temporary destination.
    let mut temp_dest = args.destination.clone();
    temp_dest.set_extension(format!("tmp.{}", ulid::Ulid::generate()));
    let mut store = Store::open_at(&temp_dest)?;

    // 4. Preserve the exported store_id.
    store.conn.execute(
        "UPDATE store_metadata SET store_id = ?1",
        rusqlite::params![store_id],
    )?;
    store.store_id = store_id.clone();
    failpoint("rebuild_after_header_validation");

    let tx = store.conn.transaction()?;
    let mut count = 0;
    let mut current_expected_sequence = expected_first_sequence;
    let mut current_previous_hash = expected_predecessor_hash.clone();

    for line_result in lines {
        let line = line_result?;
        if line.trim().is_empty() {
            continue;
        }
        let (next_sequence, next_hash) = verify_and_insert_record(
            &tx,
            &line,
            &store_id,
            current_expected_sequence,
            &current_previous_hash,
        )?;
        current_expected_sequence = next_sequence;
        current_previous_hash = next_hash;
        count += 1;
        failpoint("rebuild_mid_stream");
    }

    // 11. Verify final sequence and head hash.
    if count != expected_count {
        anyhow::bail!(
            "Record count mismatch: expected {}, got {}",
            expected_count,
            count
        );
    }
    if count > 0 && current_previous_hash != expected_head_hash {
        anyhow::bail!("Head hash mismatch");
    }

    // Reconstruct the materialized review-state projection purely from the
    // replayed stream (the reducer is a pure function of the records).
    let reduced = crate::remediation::reducer::replay_all(&tx)?;
    for obs in reduced.values() {
        crate::remediation::reducer::upsert_review_state(&tx, obs)?;
    }
    tx.commit()?;

    // Run full verification.
    crate::verify::full_verify(&mut store).context("Failed verification after rebuild")?;
    failpoint("rebuild_after_verification");
    failpoint("rebuild_after_construction");

    // 12. Atomically finalize destination.
    finalize_destination(store, &temp_dest, &args.destination)?;

    println!("Rebuilt {} records.", count);
    Ok(())
}
