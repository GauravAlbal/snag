use crate::cli::RebuildArgs;
use crate::failpoint::failpoint;
use crate::record::CanonicalRecordV1;
use crate::store::Store;
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};

pub fn handle(args: RebuildArgs) -> Result<()> {
    let file = File::open(&args.from_export)?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    // 1. Parse and validate the header.
    let header_line = lines.next().transpose()?.context("Stream is empty")?;
    let header: Value = serde_json::from_str(&header_line)?;

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
    let expected_first_sequence = header
        .get("first_sequence")
        .and_then(|v| v.as_u64())
        .context("Missing first_sequence")?;
    let _expected_through_sequence = header
        .get("through_sequence")
        .and_then(|v| v.as_u64())
        .context("Missing through_sequence")?;
    let expected_predecessor_hash = header
        .get("previous_checkpoint_hash")
        .and_then(|v| v.as_str())
        .context("Missing previous_checkpoint_hash")?
        .to_string();
    let expected_head_hash = header
        .get("head_record_hash")
        .and_then(|v| v.as_str())
        .context("Missing head_record_hash")?
        .to_string();
    let expected_count = header
        .get("record_count")
        .and_then(|v| v.as_u64())
        .context("Missing record_count")?;

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

        let envelope: Value = serde_json::from_str(&line)?;
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

        // 5. Verify contiguous sequence
        if local_sequence != current_expected_sequence {
            anyhow::bail!(
                "Non-contiguous sequence: expected {}, got {}",
                current_expected_sequence,
                local_sequence
            );
        }

        // 6. Verify predecessor hash
        if previous_record_hash != current_previous_hash {
            anyhow::bail!(
                "Predecessor hash mismatch at sequence {}: expected {}, got {}",
                local_sequence,
                current_previous_hash,
                previous_record_hash
            );
        }

        // 7. Recompute hash
        let canonical_record = CanonicalRecordV1 {
            local_sequence,
            record_id: record_id.clone(),
            record_type: record_type.clone(),
            entity_id: entity_id.clone(),
            captured_at: captured_at.clone(),
            payload: payload.clone(),
        };
        let computed_hash = canonical_record.compute_hash(&store_id, &previous_record_hash);

        if computed_hash != envelope_hash {
            anyhow::bail!("Record hash mismatch at sequence {}", local_sequence);
        }

        let canonical_payload_json = serde_json::to_string(&payload)?;

        // 8. Insert without REPLACE
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

        // 9. Reconstruct normalized state
        match payload {
            crate::record::RecordPayload::Observation(obs) => {
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
                        &crate::idempotency::observation_semantic_digest(&obs),
                        serde_json::to_string(&obs.labels).unwrap_or_else(|_| "null".to_string()),
                    ],
                )?;

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
        }

        current_expected_sequence += 1;
        current_previous_hash = computed_hash;
        count += 1;
        failpoint("rebuild_mid_stream");
    }

    // 11. Verify final sequence and head hash
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

    tx.commit()?;

    // Run full verification
    crate::verify::full_verify(&mut store).context("Failed verification after rebuild")?;
    failpoint("rebuild_after_verification");
    failpoint("rebuild_after_construction");
    // 12. Atomically finalize destination
    // Since Store has open connections, we drop it to ensure files are closed
    failpoint("rebuild_before_publication");
    drop(store);
    std::fs::rename(&temp_dest, &args.destination)?;

    println!("Rebuilt {} records.", count);
    Ok(())
}
