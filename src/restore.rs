use crate::cli::RestoreArgs;
use crate::store::Store;
use crate::verify;
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::json;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

/// A simple advisory exclusive lock acquired for the duration of a backup or
/// restore so no second maintenance writer can interleave. Refuses if another
/// writer owns the store.
struct MaintenanceLock {
    path: PathBuf,
}

impl MaintenanceLock {
    fn acquire(data_dir: &Path) -> Result<MaintenanceLock> {
        let path = data_dir.join(".maintenance.lock");
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                let _ = writeln!(f, "pid={}", std::process::id());
                let _ = f.sync_all();
                Ok(MaintenanceLock { path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                anyhow::bail!(crate::error::SnagError::RestoreRefused(
                    "another writer owns the store (maintenance lock held)".to_string()
                ))
            }
            Err(e) => Err(e.into()),
        }
    }
}

impl Drop for MaintenanceLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn forensic_copy(data_dir: &Path) -> Result<PathBuf> {
    let ts = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
        .replace(':', "");
    let dir = data_dir
        .join("forensics")
        .join(format!("pre-restore-{}", ts));
    fs::create_dir_all(&dir)?;
    for suffix in ["snag.sqlite", "snag.sqlite-wal", "snag.sqlite-shm"] {
        let src = data_dir.join(suffix);
        if src.exists() {
            fs::copy(&src, dir.join(suffix))?;
            File::open(dir.join(suffix))?.sync_all()?;
        }
    }
    sync_dir(&dir)?;
    Ok(dir)
}

fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let fd = File::open(dir)?;
        fd.sync_all()?;
    }
    Ok(())
}

pub fn handle(args: RestoreArgs) -> Result<()> {
    // 1. Acquire exclusive restore/maintenance lock.
    let (data_dir, final_db) = Store::paths()?;
    fs::create_dir_all(&data_dir)?;
    let _lock = MaintenanceLock::acquire(&data_dir)?;

    // Refuse if the active store already has data (we never overwrite the only
    // copy of a non-empty database silently).
    if final_db.exists() {
        let existing = Connection::open(&final_db)?;
        let count: i64 = existing
            .query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0))
            .unwrap_or(0);
        if count > 0 {
            anyhow::bail!(crate::error::SnagError::RestoreRefused(
                "active store is non-empty; refusing to overwrite history".to_string()
            ));
        }
    }

    // 2. Fully verify the backup bundle BEFORE touching active state.
    crate::verify::verify_backup(&args.archive)
        .context("backup verification failed; refusing restore")?;

    // 3. Preserve the active DB + WAL/SHM + metadata as a timestamped forensic copy.
    let forensic_dir = forensic_copy(&data_dir)?;

    let previous_head: String = if final_db.exists() {
        Connection::open(&final_db)
            .and_then(|c| c.query_row(
                "SELECT COALESCE((SELECT record_hash FROM records ORDER BY local_sequence DESC LIMIT 1), '0000000000000000000000000000000000000000000000000000000000000000')",
                [],
                |r| r.get(0)))
            .unwrap_or_else(|_| "0000000000000000000000000000000000000000000000000000000000000000".to_string())
    } else {
        "none".to_string()
    };

    // 4. Resolve the bundle to a directory (extracts archive if needed).
    let bundle_dir = crate::backup::resolve_bundle(&args.archive)?;
    let restored_db = bundle_dir.join("snag.sqlite");

    // 5. Build the candidate active-store file: copy the restored DB into a
    //    temp name in the data dir (same filesystem for atomic rename).
    let candidate = data_dir.join(format!("snag.sqlite.candidate.{}", ulid::Ulid::generate()));
    fs::copy(&restored_db, &candidate)?;
    {
        let conn = Connection::open(&candidate)?;
        conn.execute_batch("PRAGMA journal_mode = DELETE; PRAGMA wal_checkpoint(TRUNCATE);")?;
        drop(conn);
    }

    // 6. Full verification of the candidate against the BUNDLE's objects dir,
    //    which is self-contained and was independently verified in step 2.
    {
        let mut cand_store = Store {
            conn: Connection::open_with_flags(
                &candidate,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
            )?,
            store_id: String::new(),
            data_dir: bundle_dir.clone(),
            db_path: candidate.clone(),
        };
        cand_store.store_id =
            cand_store
                .conn
                .query_row("SELECT store_id FROM store_metadata LIMIT 1", [], |r| {
                    r.get(0)
                })?;
        verify::full_verify(&mut cand_store)
            .context("restored candidate failed full verification")?;
    }

    // 7. Merge the bundle's objects into the active objects dir (idempotent,
    //    content-addressed). Copy only objects not already present.
    let bundle_objects = bundle_dir.join("objects");
    let active_objects = data_dir.join("objects");
    if bundle_objects.exists() {
        copy_tree(&bundle_objects, &active_objects)?;
    }

    // 8. Flush and sync candidate files/dirs.
    File::open(&candidate)?.sync_all()?;

    // 9. Atomically switch the active database. Remove pre-existing WAL/SHM of
    //    the old DB first (they belong to the old head and are preserved in the
    //    forensic copy); then rename candidate into place.
    for suffix in ["snag.sqlite-wal", "snag.sqlite-shm"] {
        let p = data_dir.join(suffix);
        if p.exists() {
            fs::remove_file(&p)?;
        }
    }
    fs::rename(&candidate, &final_db)?;
    sync_dir(&data_dir)?;

    // 10. Run full verification on the switched active store before success.
    {
        let mut active = Store::open_read_only()?;
        verify::full_verify(&mut active).context("post-restore full verification failed")?;
    }

    // 11. Emit a restore receipt.
    let restored_head: String = Connection::open(&final_db).and_then(|c| c.query_row(
        "SELECT COALESCE((SELECT record_hash FROM records ORDER BY local_sequence DESC LIMIT 1), '0000000000000000000000000000000000000000000000000000000000000000')",
        [], |r| r.get(0))).unwrap_or_default();
    let ts = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let receipt = json!({
        "restore_receipt_schema": 1,
        "source_backup": args.archive.to_string_lossy(),
        "previous_store_head": previous_head,
        "restored_head": restored_head,
        "forensic_copy": forensic_dir.to_string_lossy(),
        "completed_at": ts,
    });
    let receipts_dir = data_dir.join("restore-receipts");
    fs::create_dir_all(&receipts_dir)?;
    let receipt_path = receipts_dir.join(format!("restore-{}.json", ts.replace(':', "")));
    fs::write(&receipt_path, serde_json::to_string_pretty(&receipt)?)?;
    File::open(&receipt_path)?.sync_all()?;

    println!(
        "Database successfully restored and verified from: {}",
        args.archive.display()
    );
    println!("Restore receipt: {}", receipt_path.display());
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let s = entry.path();
        let d = dst.join(entry.file_name());
        if s.is_dir() {
            copy_tree(&s, &d)?;
        } else if s.is_file() && !d.exists() {
            fs::copy(&s, &d)?;
            File::open(&d)?.sync_all()?;
        }
    }
    Ok(())
}
