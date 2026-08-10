#[derive(Debug)]
struct OldRecord {
    id: String,
    typ: String,
    entity_id: String,
    captured_at: String,
    payload: String,
    seq: i64,
    class_order: u32,
}

/// v1 -> v2: merge legacy observations/actions into a single global `records`
/// stream with a deterministic, collision-safe ordering.
///
/// Deterministic ordering tie-breakers (G33): captured_at, then record class
/// order (observations before actions), then the original sequence, then the
/// record ID. The legacy tables carry a UNIQUE constraint on local_sequence,
/// so before rewriting we first "park" every old sequence to a unique negative
/// value; this guarantees the new positive sequences can never collide with a
/// still-unmigrated old sequence, making the rewrite collision-safe without
/// rebuilding the tables.
pub fn migrate_v1_to_v2(tx: &rusqlite::Transaction) -> anyhow::Result<()> {
    tx.execute_batch(
        "
        CREATE TABLE records_new (
            local_sequence INTEGER PRIMARY KEY,
            record_id TEXT UNIQUE NOT NULL,
            record_type TEXT NOT NULL,
            entity_id TEXT NOT NULL,
            captured_at TEXT NOT NULL,
            canonical_payload_json TEXT NOT NULL,
            previous_record_hash TEXT NOT NULL,
            record_hash TEXT NOT NULL
        );
        ",
    )?;

    park_sequences(tx)?;

    let old_records = collect_legacy_records(tx)?;

    let store_id: String = tx
        .query_row("SELECT store_id FROM store_metadata LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap_or_else(|_| "store_000".to_string());

    let mut previous_hash =
        "0000000000000000000000000000000000000000000000000000000000000000".to_string();

    let mut new_sequence = 1_u64;
    let mut count = 0_i64;

    #[allow(clippy::explicit_counter_loop)]
    for rec in old_records {
        previous_hash = write_record(tx, &rec, new_sequence, &store_id, &previous_hash)?;
        new_sequence += 1;
        count += 1;
    }

    // Atomic swap of the new records table into place.
    tx.execute_batch(
        "
        ALTER TABLE records_new RENAME TO records;
        ",
    )?;

    let _ = count;
    Ok(())
}

/// Park every legacy sequence at a unique negative value so the new positive
/// global sequences can never collide with a still-unmigrated old row while the
/// rewrite is in flight (the legacy tables carry a UNIQUE on local_sequence).
fn park_sequences(tx: &rusqlite::Transaction) -> anyhow::Result<()> {
    tx.execute_batch(
        "
        UPDATE observations SET local_sequence = -(1000000000 + local_sequence);
        UPDATE observation_actions SET local_sequence = -(2000000000 + local_sequence);
        ",
    )?;
    Ok(())
}

/// Every legacy observation and action, in the G33 deterministic order.
fn collect_legacy_records(tx: &rusqlite::Transaction) -> anyhow::Result<Vec<OldRecord>> {
    let mut old_records: Vec<OldRecord> = Vec::new();
    collect_legacy_observations(tx, &mut old_records)?;
    collect_legacy_actions(tx, &mut old_records)?;
    sort_deterministic(&mut old_records);
    Ok(old_records)
}

fn collect_legacy_observations(
    tx: &rusqlite::Transaction,
    out: &mut Vec<OldRecord>,
) -> anyhow::Result<()> {
    let mut stmt = tx.prepare(
        "SELECT observation_id, captured_at, canonical_payload_json, local_sequence FROM observations")?;
    let obs_iter = stmt.query_map([], |row| {
        Ok(OldRecord {
            id: row.get(0)?,
            typ: "observation_created".to_string(),
            entity_id: row.get(0)?,
            captured_at: row.get(1)?,
            payload: row.get(2)?,
            seq: row.get(3)?,
            class_order: 0,
        })
    })?;
    for obs in obs_iter {
        out.push(obs?);
    }
    Ok(())
}

fn collect_legacy_actions(
    tx: &rusqlite::Transaction,
    out: &mut Vec<OldRecord>,
) -> anyhow::Result<()> {
    let mut stmt = tx.prepare(
        "SELECT action_id, observation_id, action_type, created_at, action_payload_json, local_sequence FROM observation_actions")?;
    let act_iter = stmt.query_map([], |row| {
        Ok(OldRecord {
            id: row.get(0)?,
            typ: format!("observation_{}", row.get::<_, String>(2)?),
            entity_id: row.get(1)?,
            captured_at: row.get(3)?,
            payload: row.get(4)?,
            seq: row.get(5)?,
            class_order: 1,
        })
    })?;
    for act in act_iter {
        out.push(act?);
    }
    Ok(())
}

/// G33 deterministic ordering: captured_at, class order, original seq, id.
fn sort_deterministic(old_records: &mut [OldRecord]) {
    old_records.sort_by(|a, b| {
        a.captured_at
            .cmp(&b.captured_at)
            .then_with(|| a.class_order.cmp(&b.class_order))
            .then_with(|| a.seq.cmp(&b.seq))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Rewrite one legacy record into `records_new` and re-stamp its origin table
/// with the new sequence and hash chain. Returns the record hash, which becomes
/// the next record's predecessor.
fn write_record(
    tx: &rusqlite::Transaction,
    rec: &OldRecord,
    new_sequence: u64,
    store_id: &str,
    previous_hash: &str,
) -> anyhow::Result<String> {
    let record_payload: crate::record::RecordPayload = serde_json::from_str(&rec.payload)
        .map_err(|e| anyhow::anyhow!("invalid legacy payload for {}: {}", rec.id, e))?;
    let canonical_record = crate::record::CanonicalRecordV1 {
        local_sequence: new_sequence,
        record_id: rec.id.clone(),
        record_type: rec.typ.clone(),
        entity_id: rec.entity_id.clone(),
        captured_at: rec.captured_at.clone(),
        payload: record_payload,
    };
    let record_hash = canonical_record.compute_hash(store_id, previous_hash);

    tx.execute(
        "INSERT INTO records_new (local_sequence, record_id, record_type, entity_id, captured_at, canonical_payload_json, previous_record_hash, record_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            new_sequence as i64,
            &rec.id,
            &rec.typ,
            &rec.entity_id,
            &rec.captured_at,
            &rec.payload,
            previous_hash,
            &record_hash,
        ],
    )?;

    // Update the original tables to match the new sequence/hashes.
    if rec.typ == "observation_created" {
        tx.execute(
            "UPDATE observations SET local_sequence = ?1, previous_record_hash = ?2, record_hash = ?3 WHERE observation_id = ?4",
            rusqlite::params![new_sequence as i64, previous_hash, &record_hash, &rec.id],
        )?;
    } else {
        tx.execute(
            "UPDATE observation_actions SET local_sequence = ?1, previous_record_hash = ?2, record_hash = ?3 WHERE action_id = ?4",
            rusqlite::params![new_sequence as i64, previous_hash, &record_hash, &rec.id],
        )?;
    }

    Ok(record_hash)
}

/// v2 -> v3: make alias ambiguity representable. `repository_aliases` becomes a
/// per-(alias, repository) mapping so one alias may legitimately map to several
/// repository candidates. A `confirmed` flag records when a binding is known to
/// be unique/correct; unresolved ambiguity must never silently pick the first.
pub fn migrate_v2_to_v3(tx: &rusqlite::Transaction) -> anyhow::Result<()> {
    tx.execute_batch(
        "
        CREATE TABLE repository_aliases_new (
            alias TEXT NOT NULL,
            repository_id TEXT NOT NULL REFERENCES repositories(repository_id),
            confirmed INTEGER NOT NULL DEFAULT 0,
            first_seen_at TEXT NOT NULL,
            last_seen_at TEXT NOT NULL,
            PRIMARY KEY (alias, repository_id)
        );
        INSERT INTO repository_aliases_new (alias, repository_id, confirmed, first_seen_at, last_seen_at)
            SELECT alias, repository_id, 1, first_seen_at, last_seen_at FROM repository_aliases;
        DROP TABLE repository_aliases;
        ALTER TABLE repository_aliases_new RENAME TO repository_aliases;

        CREATE TABLE observation_repositories_new (
            observation_id TEXT NOT NULL REFERENCES observations(observation_id),
            repository_id TEXT NOT NULL REFERENCES repositories(repository_id),
            role TEXT NOT NULL DEFAULT 'affected',
            PRIMARY KEY (observation_id, repository_id, role)
        );
        INSERT INTO observation_repositories_new (observation_id, repository_id, role)
            SELECT observation_id, repository_id, 'affected' FROM observation_repositories;
        DROP TABLE observation_repositories;
        ALTER TABLE observation_repositories_new RENAME TO observation_repositories;
        ",
    )?;
    Ok(())
}

/// v3 -> v4: store the stable semantic idempotency digest directly so the
/// same-key/same-semantics replay returns the original observation and a
/// same-key/different-semantics replay is a typed conflict.
pub fn migrate_v3_to_v4(tx: &rusqlite::Transaction) -> anyhow::Result<()> {
    tx.execute_batch(
        "
        ALTER TABLE observations ADD COLUMN semantic_digest TEXT;
        ",
    )?;
    Ok(())
}

/// v4 -> v5: remediation protocol substrate.
///
/// Adds the normalized remediation tables (claims, dispositions, relationships,
/// remediation links) and the materialized `observation_review_state`
/// projection. All four normalized tables are derived indexes over the global
/// record stream; `observation_review_state` is populated by the reducer and
/// backfilled here as `unreviewed` for every existing observation so existing
/// stores begin with a consistent, queryable queue. No existing observation
/// records or retractions are modified.
pub fn migrate_v4_to_v5(tx: &rusqlite::Transaction) -> anyhow::Result<()> {
    tx.execute_batch(
        "
        CREATE TABLE remediation_claims (
            claim_id TEXT PRIMARY KEY,
            observation_id TEXT NOT NULL REFERENCES observations(observation_id),
            claimed_by TEXT NOT NULL,
            claim_session_id TEXT NOT NULL,
            claimed_at TEXT NOT NULL,
            lease_expires_at TEXT NOT NULL,
            released_at TEXT,
            release_reason TEXT,
            source_record_sequence INTEGER NOT NULL UNIQUE,
            idempotency_key TEXT UNIQUE
        );
        CREATE INDEX idx_remediation_claims_observation ON remediation_claims(observation_id);
        CREATE INDEX idx_remediation_claims_active
            ON remediation_claims(observation_id, released_at, lease_expires_at);

        CREATE TABLE observation_dispositions (
            disposition_id TEXT PRIMARY KEY,
            observation_id TEXT NOT NULL REFERENCES observations(observation_id),
            disposition TEXT NOT NULL,
            target_observation_id TEXT REFERENCES observations(observation_id),
            rationale TEXT,
            evidence_json TEXT,
            reviewer TEXT NOT NULL,
            review_session_id TEXT NOT NULL,
            created_at TEXT NOT NULL,
            source_record_sequence INTEGER NOT NULL UNIQUE,
            retracted_by_record_sequence INTEGER,
            idempotency_key TEXT UNIQUE
        );
        CREATE INDEX idx_observation_dispositions_observation
            ON observation_dispositions(observation_id, source_record_sequence);
        CREATE INDEX idx_observation_dispositions_target
            ON observation_dispositions(target_observation_id);

        CREATE TABLE observation_relationships (
            relationship_id TEXT PRIMARY KEY,
            left_observation_id TEXT NOT NULL REFERENCES observations(observation_id),
            right_observation_id TEXT NOT NULL REFERENCES observations(observation_id),
            relation TEXT NOT NULL,
            rationale TEXT,
            evidence_json TEXT,
            reviewer TEXT NOT NULL,
            review_session_id TEXT NOT NULL,
            created_at TEXT NOT NULL,
            source_record_sequence INTEGER NOT NULL UNIQUE,
            retracted_by_record_sequence INTEGER,
            idempotency_key TEXT UNIQUE
        );
        CREATE INDEX idx_observation_relationships_endpoints
            ON observation_relationships(left_observation_id, right_observation_id);

        CREATE TABLE remediation_links (
            link_id TEXT PRIMARY KEY,
            observation_id TEXT NOT NULL REFERENCES observations(observation_id),
            link_type TEXT NOT NULL,
            target_id TEXT NOT NULL,
            repository_id TEXT,
            status TEXT,
            metadata_json TEXT,
            created_at TEXT NOT NULL,
            source_record_sequence INTEGER NOT NULL UNIQUE,
            retracted_by_record_sequence INTEGER,
            idempotency_key TEXT UNIQUE
        );
        CREATE INDEX idx_remediation_links_observation
            ON remediation_links(observation_id, link_type);

        CREATE TABLE observation_review_state (
            observation_id TEXT PRIMARY KEY REFERENCES observations(observation_id),
            state TEXT NOT NULL,
            disposition TEXT,
            handled INTEGER NOT NULL DEFAULT 0,
            active_claim_id TEXT,
            active_claim_expires_at TEXT,
            promoted_finding_id TEXT,
            task_ids_json TEXT NOT NULL DEFAULT '[]',
            commits_json TEXT NOT NULL DEFAULT '[]',
            verification_receipts_json TEXT NOT NULL DEFAULT '[]',
            latest_verification_status TEXT,
            updated_through_sequence INTEGER NOT NULL
        );

        CREATE VIEW active_claims AS
        SELECT claim_id, observation_id, claimed_by, claim_session_id, claimed_at, lease_expires_at
        FROM remediation_claims
        WHERE released_at IS NULL
          AND lease_expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now');
        ",
    )?;

    // Every existing observation begins as `unreviewed` with an empty lineage
    // so the queue and verify see a consistent projection immediately.
    tx.execute(
        "INSERT INTO observation_review_state (
            observation_id, state, handled, task_ids_json, commits_json,
            verification_receipts_json, updated_through_sequence
        )
        SELECT observation_id, 'unreviewed', 0, '[]', '[]', '[]', local_sequence
        FROM observations",
        [],
    )?;
    Ok(())
}

/// v6 -> v7: rename the ambiguous `role='primary'` attribution to
/// `role='reporter'` (the filing context — where the reporter was). The fix
/// owner is a separate actor (`role='owner'`, from `snag report --owner`);
/// `primary` conflated the two. Values-only rename: the column stays, no
/// structural change. `affected` is unchanged. Numbered v7 because v6 is a
/// lane-local migration in the internal lane (pearl_id chain repair) that
/// the shared store may already carry.
pub fn migrate_v6_to_v7(tx: &rusqlite::Transaction) -> anyhow::Result<()> {
    tx.execute(
        "UPDATE observation_repositories SET role = 'reporter' WHERE role = 'primary'",
        [],
    )?;
    Ok(())
}

/// v7 -> v8: rebuild the materialized review-state projection from the
/// append-only remediation event stream.
///
/// Reducer semantics evolved after the v5 projection was introduced. Stores
/// materialized by an older binary can therefore carry valid records but stale
/// lineage arrays. The record stream remains authoritative; replace only the
/// derived projection, seed observations without remediation events as
/// unreviewed, then replay every event-bearing observation.
pub fn migrate_v7_to_v8(tx: &rusqlite::Transaction) -> anyhow::Result<()> {
    tx.execute("DELETE FROM observation_review_state", [])?;
    tx.execute(
        "INSERT INTO observation_review_state (
            observation_id, state, handled, task_ids_json, commits_json,
            verification_receipts_json, updated_through_sequence
        )
        SELECT observation_id, 'unreviewed', 0, '[]', '[]', '[]', local_sequence
        FROM observations",
        [],
    )?;

    let reduced = crate::remediation::reducer::replay_all(tx)?;
    for state in reduced.values() {
        crate::remediation::reducer::upsert_review_state(tx, state)?;
    }
    Ok(())
}
/// v8 -> v9: index record event lookups by entity and type while preserving
/// stream order. `IF NOT EXISTS` keeps retries and forensic rebuilds safe.
pub fn migrate_v8_to_v9(tx: &rusqlite::Transaction) -> anyhow::Result<()> {
    tx.execute(
        "CREATE INDEX IF NOT EXISTS idx_records_entity_type_sequence
         ON records(entity_id, record_type, local_sequence)",
        [],
    )?;
    Ok(())
}

/// v10 -> v11: replay the authoritative event stream into the review-state
/// projection. Versions 8 and 9 were already in use by deployed lane variants,
/// so their migration markers cannot prove that the projection replay ran.
pub fn migrate_v10_to_v11(tx: &rusqlite::Transaction) -> anyhow::Result<()> {
    migrate_v7_to_v8(tx)
}

/// v12 -> v13: guarantee the record-lookup index regardless of lane history.
/// The index was introduced under v9 in the public lane and v10 in the
/// internal lane after lane markers had already collided, so stores migrated
/// by an earlier binary can carry the marker without the index. Same
/// idempotent statement in both lanes, so whichever binary runs next heals
/// the store. This version must stay semantically identical across lanes.
pub fn migrate_v12_to_v13(tx: &rusqlite::Transaction) -> anyhow::Result<()> {
    tx.execute(
        "CREATE INDEX IF NOT EXISTS idx_records_entity_type_sequence
         ON records(entity_id, record_type, local_sequence)",
        [],
    )?;
    Ok(())
}
