use crate::cli::VerifyArgs;
use crate::store::Store;
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::PathBuf;

pub fn handle(args: VerifyArgs) -> Result<()> {
    let mut store = Store::open_read_only()?;
    
    if let Some(backup_path) = args.backup {
        println!("Verifying backup at {:?}", backup_path);
        verify_backup(&backup_path)?;
        return Ok(());
    }
    
    if args.quick {
        println!("Running quick verification...");
        quick_verify(&mut store)?;
    } else {
        println!("Running full verification...");
        full_verify(&mut store)?;
    }
    
    Ok(())
}

fn quick_verify(store: &mut Store) -> Result<()> {
    let result: String = store.conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if result != "ok" {
        anyhow::bail!("Quick check failed: {}", result);
    }
    
    // Check latest chain segment
    let mut stmt = store.conn.prepare("SELECT local_sequence, previous_record_hash, record_hash, canonical_payload_json, record_id, record_type, entity_id, captured_at FROM records ORDER BY local_sequence DESC LIMIT 1")?;
    let mut rows = stmt.query([])?;
    
    if let Some(row) = rows.next()? {
        let seq: i64 = row.get(0)?;
        let prev: String = row.get(1)?;
        let expected_hash: String = row.get(2)?;
        let payload: String = row.get(3)?;
        let record_id: String = row.get(4)?;
        let record_type: String = row.get(5)?;
        let entity_id: String = row.get(6)?;
        let captured_at: String = row.get(7)?;
        
        let record_payload: crate::record::RecordPayload = serde_json::from_str(&payload).context("Invalid canonical payload")?;
        let canonical_record = crate::record::CanonicalRecordV1 {
            local_sequence: seq as u64,
            record_id,
            record_type,
            entity_id,
            captured_at,
            payload: record_payload,
        };
        let computed = canonical_record.compute_hash(&store.store_id, &prev);
        
        if computed != expected_hash {
            anyhow::bail!("Hash chain mismatch at sequence {}", seq);
        }
        
        println!("Latest hash segment verified (seq {}).", seq);
    } else {
        println!("No observations to verify.");
    }
    
    println!("Quick verification passed.");
    Ok(())
}

pub fn full_verify(store: &mut Store) -> Result<()> {
    let result: String = store.conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if result != "ok" {
        anyhow::bail!("Integrity check failed: {}", result);
    }
    
    let fk_violations: i64 = store.conn.query_row(
        "SELECT count(*) FROM pragma_foreign_key_check", 
        [], 
        |row| row.get(0)
    )?;
    if fk_violations > 0 {
        anyhow::bail!("Foreign key check failed with {} violations", fk_violations);
    }
    
    // Complete hash chain verify
    let mut stmt = store.conn.prepare("SELECT local_sequence, previous_record_hash, record_hash, canonical_payload_json, record_id, record_type, entity_id, captured_at FROM records ORDER BY local_sequence ASC")?;
    let mut rows = stmt.query([])?;
    
    let mut last_hash = "0000000000000000000000000000000000000000000000000000000000000000".to_string();
    let mut count = 0;
    while let Some(row) = rows.next()? {
        let seq: i64 = row.get(0)?;
        let prev: String = row.get(1)?;
        let expected_hash: String = row.get(2)?;
        let payload: String = row.get(3)?;
        let record_id: String = row.get(4)?;
        let record_type: String = row.get(5)?;
        let entity_id: String = row.get(6)?;
        let captured_at: String = row.get(7)?;
        
        if prev != last_hash {
            anyhow::bail!("Broken chain at seq {}: previous hash mismatch", seq);
        }
        
        let record_payload: crate::record::RecordPayload = serde_json::from_str(&payload).context("Invalid canonical payload")?;
        let canonical_record = crate::record::CanonicalRecordV1 {
            local_sequence: seq as u64,
            record_id,
            record_type,
            entity_id,
            captured_at,
            payload: record_payload,
        };
        let computed = canonical_record.compute_hash(&store.store_id, &prev);
        
        if computed != expected_hash {
            anyhow::bail!("Hash chain mismatch at sequence {}", seq);
        }
        
        last_hash = expected_hash;
        count += 1;
    }
    
    // Check artifact existence, length, and hash
    let objects_dir = store.data_dir.join("objects").join("blake3");
    let mut art_stmt = store.conn.prepare("SELECT digest, byte_length FROM artifacts")?;
    let mut art_rows = art_stmt.query([])?;
    
    let mut missing = 0;
    let mut invalid = 0;
    while let Some(row) = art_rows.next()? {
        let digest: String = row.get(0)?;
        let expected_len: i64 = row.get(1)?;
        
        if let Some(hash) = digest.strip_prefix("blake3:") {
            let prefix = &hash[0..2];
            let path = objects_dir.join(prefix).join(hash);
            if !path.exists() {
                missing += 1;
            } else {
                let meta = std::fs::metadata(&path)?;
                if meta.len() as i64 != expected_len {
                    invalid += 1;
                } else {
                    let mut file = std::fs::File::open(&path)?;
                    let mut hasher = blake3::Hasher::new();
                    std::io::copy(&mut file, &mut hasher)?;
                    let computed = format!("blake3:{}", hasher.finalize().to_hex());
                    if computed != digest {
                        invalid += 1;
                    }
                }
            }
        }
    }
    
    if missing > 0 || invalid > 0 {
        anyhow::bail!("{} artifacts are missing, {} are invalid", missing, invalid);
    }
    
    println!("Full verification passed ({} records checked).", count);
    Ok(())
}

fn verify_backup(backup_path: &PathBuf) -> Result<()> {
    // Just a placeholder to open DB and run integrity check
    let conn = Connection::open(backup_path.join("snag.sqlite"))?;
    let result: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if result != "ok" {
        anyhow::bail!("Backup integrity check failed: {}", result);
    }
    println!("Backup verification passed.");
    Ok(())
}
