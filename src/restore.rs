use crate::cli::RestoreArgs;
use crate::store::Store;
use anyhow::{Context, Result};
use std::fs::{self, File};
use flate2::read::GzDecoder;

pub fn handle(args: RestoreArgs) -> Result<()> {
    // 1. Refuse if active store is non-empty
    let active_store = Store::open_read_only();
    if let Ok(store) = active_store {
        let count: i64 = store.conn.query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0)).unwrap_or(0);
        if count > 0 {
            anyhow::bail!("Active store is non-empty. Refusing to restore.");
        }
    }
    
    // 2. Extract the archive
    let archive_file = File::open(&args.archive)?;
    let tar = GzDecoder::new(archive_file);
    let mut archive = tar::Archive::new(tar);
    
    let temp_dir = tempfile::tempdir()?;
    archive.unpack(temp_dir.path())?;
    
    let temp_db = temp_dir.path().join("snag.sqlite");
    if !temp_db.exists() {
        anyhow::bail!("Archive does not contain snag.sqlite");
    }
    
    // 3. Verify the temporary sqlite file
    let mut temp_store = Store::open_read_only_at(&temp_dir.path().to_path_buf())?;
    crate::verify::full_verify(&mut temp_store).context("Verification of restored database failed")?;
    drop(temp_store); // release file locks
    
    // 4. Replace the active store atomically
    let (data_dir, final_db) = Store::paths()?;
    fs::create_dir_all(&data_dir)?;
    
    let temp_dest = data_dir.join(format!("snag.sqlite.tmp.{}", ulid::Ulid::generate()));
    fs::rename(&temp_db, &temp_dest)?;
    fs::rename(&temp_dest, &final_db)?;
    
    println!("Database successfully restored and verified from: {:?}", args.archive);
    
    Ok(())
}
