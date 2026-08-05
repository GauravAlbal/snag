use crate::cli::{BackupArgs, ExportArgs};
use crate::store::Store;
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::fs::{self, File};
use std::path::PathBuf;
use flate2::write::GzEncoder;
use flate2::Compression;

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
    
    // 2. Export offline
    let temp_store = Store::open_read_only_at(&temp_dir.path().to_path_buf())?;
    let temp_export_path = temp_dir.path().join("snag.jsonl");
    crate::export::handle_with_store(ExportArgs {
        format: None,
        after_sequence: None,
        through_sequence: None,
        output: Some(temp_export_path.clone()),
    }, temp_store)?;
    
    // Verification of the backup copy
    let backup_conn = Connection::open(&temp_db_path)?;
    
    let integrity: String = backup_conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        anyhow::bail!("Backup integrity check failed: {}", integrity);
    }
    
    let fk_violations: i64 = backup_conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| row.get(0))?;
    if fk_violations > 0 {
        anyhow::bail!("Backup foreign key check failed");
    }
    
    // 3. Get head hash
    let mut stmt = backup_conn.prepare("SELECT record_hash FROM records ORDER BY local_sequence DESC LIMIT 1")?;
    let mut rows = stmt.query([])?;
    
    let head_hash = if let Some(row) = rows.next()? {
        row.get::<_, String>(0)?
    } else {
        "0000000000000000000000000000000000000000000000000000000000000000".to_string()
    };
    
    // 4. Create tar.gz archive
    let short_hash = if head_hash.starts_with("blake3:") {
        &head_hash[7..15]
    } else {
        &head_hash[..8]
    };
    let now = time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap();
    let ts_dir = now.replace(':', "");
    let archive_name = format!("snag-backup-{}-{}.tar.gz", ts_dir, short_hash);
    let archive_path = backups_dir.join(&archive_name);
    
    let tar_gz = File::create(&archive_path)?;
    let enc = GzEncoder::new(tar_gz, Compression::default());
    let mut tar = tar::Builder::new(enc);
    
    // Add snag.sqlite and snag.jsonl
    tar.append_path_with_name(&temp_db_path, "snag.sqlite")?;
    tar.append_path_with_name(&temp_export_path, "snag.jsonl")?;
    
    tar.into_inner()?.finish()?;
    
    println!("Backup verified and saved to: {:?}", archive_path);
    
    Ok(())
}
