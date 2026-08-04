use crate::cli::BackupArgs;
use crate::store::Store;
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::fs;
use std::path::PathBuf;
use serde_json::json;

pub fn handle(_args: BackupArgs) -> Result<()> {
    let store = Store::open_read_only()?;
    let backups_dir = store.data_dir.join("backups");
    fs::create_dir_all(&backups_dir)?;
    
    // 1. Online backup to a temporary file
    let temp_dir = tempfile::tempdir_in(&backups_dir)?;
    let temp_db_path = temp_dir.path().join("snag.sqlite");
    
    {
        let mut dest = Connection::open(&temp_db_path)?;
        let backup = rusqlite::backup::Backup::new(&store.conn, &mut dest)?;
        backup.step(-1)?;
    }
    
    // 2. Verification of the backup copy
    let backup_conn = Connection::open(&temp_db_path)?;
    
    let integrity: String = backup_conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        anyhow::bail!("Backup integrity check failed");
    }
    
    let fk_violations: i64 = backup_conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| row.get(0))?;
    if fk_violations > 0 {
        anyhow::bail!("Backup foreign key check failed");
    }
    
    // 3. Get head hash
    let mut stmt = backup_conn.prepare("SELECT local_sequence, record_hash FROM records ORDER BY local_sequence DESC LIMIT 1")?;
    let mut rows = stmt.query([])?;
    
    let (head_sequence, head_hash) = if let Some(row) = rows.next()? {
        let seq: i64 = row.get(0)?;
        let hash: String = row.get(1)?;
        (seq, hash)
    } else {
        (0_i64, "0000000000000000000000000000000000000000000000000000000000000000".to_string())
    };
    
    // Generate objects manifest
    let mut art_stmt = backup_conn.prepare("SELECT digest, byte_length FROM artifacts")?;
    let mut art_rows = art_stmt.query([])?;
    let mut objects = Vec::new();
    while let Some(row) = art_rows.next()? {
        let digest: String = row.get(0)?;
        let byte_length: i64 = row.get(1)?;
        objects.push(json!({"digest": digest, "byte_length": byte_length}));
    }
    
    let objects_manifest = json!({"objects": objects});
    let objects_manifest_bytes = serde_json::to_vec_pretty(&objects_manifest)?;
    let objects_manifest_digest = format!("blake3:{}", blake3::hash(&objects_manifest_bytes).to_hex());
    let objects_manifest_path = temp_dir.path().join("objects-manifest.json");
    fs::write(&objects_manifest_path, &objects_manifest_bytes)?;
    
    // Calculate DB digest
    let db_bytes = fs::read(&temp_db_path)?;
    let db_digest = format!("blake3:{}", blake3::hash(&db_bytes).to_hex());
    
    // 4. Manifest
    let now = time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap();
    let manifest = json!({
        "schema_version": 1,
        "store_id": store.store_id,
        "created_at": now,
        "through_sequence": head_sequence,
        "head_record_hash": head_hash,
        "database_digest": db_digest,
        "integrity_check": "ok",
        "foreign_key_check": "ok",
        "artifact_manifest_digest": objects_manifest_digest
    });
    
    let manifest_path = temp_dir.path().join("manifest.json");
    fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;
    
    let ts_dir = now.replace(':', "");
    let final_dir_name = format!("{}-seq-{}", ts_dir, head_sequence);
    let final_dir_path = backups_dir.join(&final_dir_name);
    
    fs::rename(temp_dir.path(), &final_dir_path)?;
    
    println!("Backup verified and saved to: {:?}", final_dir_path);
    
    Ok(())
}
