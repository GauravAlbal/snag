use rusqlite::{Connection, Result, Row};
use std::cmp::Ordering;

#[derive(Debug)]
struct OldRecord {
    id: String,
    typ: String,
    entity_id: String,
    captured_at: String,
    payload: String,
}

pub fn migrate_v1_to_v2(tx: &rusqlite::Transaction) -> anyhow::Result<()> {
    tx.execute_batch(
        "
        CREATE TABLE records (
            local_sequence INTEGER PRIMARY KEY,
            record_id TEXT UNIQUE NOT NULL,
            record_type TEXT NOT NULL,
            entity_id TEXT NOT NULL,
            captured_at TEXT NOT NULL,
            canonical_payload_json TEXT NOT NULL,
            previous_record_hash TEXT NOT NULL,
            record_hash TEXT NOT NULL
        );
        "
    )?;

    let mut old_records: Vec<OldRecord> = Vec::new();

    let mut stmt = tx.prepare("SELECT observation_id, captured_at, canonical_payload_json FROM observations")?;
    let obs_iter = stmt.query_map([], |row| {
        Ok(OldRecord {
            id: row.get(0)?,
            typ: "observation_created".to_string(),
            entity_id: row.get(0)?,
            captured_at: row.get(1)?,
            payload: row.get(2)?,
        })
    })?;
    for obs in obs_iter {
        old_records.push(obs?);
    }

    let mut stmt = tx.prepare("SELECT action_id, observation_id, action_type, created_at, action_payload_json FROM observation_actions")?;
    let act_iter = stmt.query_map([], |row| {
        Ok(OldRecord {
            id: row.get(0)?,
            typ: format!("observation_{}", row.get::<_, String>(2)?),
            entity_id: row.get(1)?,
            captured_at: row.get(3)?,
            payload: row.get(4)?,
        })
    })?;
    for act in act_iter {
        old_records.push(act?);
    }

    // Sort by captured_at
    old_records.sort_by(|a, b| a.captured_at.cmp(&b.captured_at));

    let store_id: String = tx.query_row("SELECT store_id FROM store_metadata LIMIT 1", [], |row| row.get(0)).unwrap_or_else(|_| "store_000".to_string());

    let mut previous_hash = "0000000000000000000000000000000000000000000000000000000000000000".to_string();

    let mut new_sequence = 1_u64;

    for rec in old_records {
        let record_payload: crate::record::RecordPayload = serde_json::from_str(&rec.payload).expect("Invalid legacy payload");
        let canonical_record = crate::record::CanonicalRecordV1 {
            local_sequence: new_sequence,
            record_id: rec.id.clone(),
            record_type: rec.typ.clone(),
            entity_id: rec.entity_id.clone(),
            captured_at: rec.captured_at.clone(),
            payload: record_payload,
        };
        let record_hash = canonical_record.compute_hash(&store_id, &previous_hash);

        tx.execute(
            "INSERT INTO records (local_sequence, record_id, record_type, entity_id, captured_at, canonical_payload_json, previous_record_hash, record_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                new_sequence as i64,
                rec.id,
                rec.typ,
                rec.entity_id,
                rec.captured_at,
                rec.payload,
                previous_hash,
                record_hash,
            ],
        )?;

        // Update the original tables to match the new sequence/hashes
        if rec.typ == "observation_created" {
            tx.execute(
                "UPDATE observations SET local_sequence = ?1, previous_record_hash = ?2, record_hash = ?3 WHERE observation_id = ?4",
                rusqlite::params![new_sequence as i64, previous_hash, record_hash, rec.id],
            )?;
        } else {
            tx.execute(
                "UPDATE observation_actions SET local_sequence = ?1, previous_record_hash = ?2, record_hash = ?3 WHERE action_id = ?4",
                rusqlite::params![new_sequence as i64, previous_hash, record_hash, rec.id],
            )?;
        }

        previous_hash = record_hash;
        new_sequence += 1;
    }

    Ok(())
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
