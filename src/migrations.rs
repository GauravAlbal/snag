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
        let mut hasher = blake3::Hasher::new();
        hasher.update(store_id.as_bytes());
        hasher.update(&new_sequence.to_le_bytes());
        hasher.update(previous_hash.as_bytes());
        hasher.update(rec.payload.as_bytes());
        let record_hash = format!("blake3:{}", hasher.finalize().to_hex());

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
