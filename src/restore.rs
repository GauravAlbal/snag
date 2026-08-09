use crate::cli::RestoreArgs;
use crate::failpoint::failpoint;
use crate::store::Store;
use crate::verify;
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::json;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

/// Process-shared lock for all store writers and exclusive restore cutover.
/// The lock is held by the OS and is released automatically when this process
/// dies; the diagnostic path itself is never used as authority.
struct MaintenanceLock {
    _lease: crate::store::StoreLease,
}

impl MaintenanceLock {
    fn acquire(data_dir: &Path) -> Result<MaintenanceLock> {
        Store::acquire_exclusive(data_dir).map(|lease| MaintenanceLock { _lease: lease })
    }
}

fn forensic_copy(data_dir: &Path) -> Result<PathBuf> {
    let ts = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
        .replace(':', "");
    let forensics = data_dir.join("forensics");
    crate::store::ensure_private_dir(&forensics)?;
    let dir = forensics.join(format!("pre-restore-{}", ts));
    crate::store::ensure_private_dir(&dir)?;
    for suffix in ["snag.sqlite", "snag.sqlite-wal", "snag.sqlite-shm"] {
        let src = data_dir.join(suffix);
        match fs::symlink_metadata(&src) {
            Ok(meta) if meta.file_type().is_file() => {
                crate::backup::copy_regular_file(&src, &dir.join(suffix))?;
            }
            Ok(_) => anyhow::bail!("active {} is not a regular file", suffix),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
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
    let (data_dir, final_db) = Store::paths()?;
    fs::create_dir_all(&data_dir)?;
    let _lock = MaintenanceLock::acquire(&data_dir)?;
    crate::store::ensure_private_dir(&data_dir)?;
    refuse_nonempty_store(&final_db)?;
    let bundle_snapshot = crate::backup::resolve_bundle(&args.archive)
        .context("backup snapshot resolution failed; refusing restore")?;
    crate::verify::verify_bundle_dir(bundle_snapshot.path())
        .context("backup verification failed; refusing restore")?;
    let forensic_dir = forensic_copy(&data_dir)?;
    failpoint("restore_after_forensic_copy");
    let previous_head = store_head(&final_db, "none");
    let bundle_dir = bundle_snapshot.path();
    let candidate = stage_candidate(&data_dir, bundle_dir)?;
    verify_candidate(&candidate, bundle_dir)?;
    failpoint("restore_after_candidate_verification");
    activate_candidate(&data_dir, bundle_dir, &final_db, &candidate)?;
    let restored_head = store_head(&final_db, "");
    verify_active(&data_dir)?;
    emit_receipt(
        &data_dir,
        &args,
        &previous_head,
        &restored_head,
        &forensic_dir,
    )?;
    println!(
        "Database successfully restored and verified from: {}",
        args.archive.display()
    );
    Ok(())
}

fn refuse_nonempty_store(final_db: &Path) -> Result<()> {
    if final_db.exists() {
        let existing = Connection::open(final_db)?;
        let count: i64 = existing
            .query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0))
            .unwrap_or(0);
        if count > 0 {
            anyhow::bail!(crate::error::SnagError::RestoreRefused(
                "active store is non-empty; refusing to overwrite history".to_string()
            ));
        }
    }
    Ok(())
}

fn store_head(final_db: &Path, missing: &str) -> String {
    if !final_db.exists() {
        return missing.to_string();
    }
    Connection::open(final_db).and_then(|c| c.query_row(
        "SELECT COALESCE((SELECT record_hash FROM records ORDER BY local_sequence DESC LIMIT 1), '0000000000000000000000000000000000000000000000000000000000000000')", [], |r| r.get(0)))
        .unwrap_or_else(|_| "0000000000000000000000000000000000000000000000000000000000000000".to_string())
}

fn stage_candidate(data_dir: &Path, bundle_dir: &Path) -> Result<PathBuf> {
    let candidate = data_dir.join(format!("snag.sqlite.candidate.{}", ulid::Ulid::generate()));
    crate::backup::copy_regular_file(&bundle_dir.join("snag.sqlite"), &candidate)?;
    failpoint("restore_after_candidate_creation");
    let conn = Connection::open(&candidate)?;
    conn.execute_batch("PRAGMA journal_mode = DELETE; PRAGMA wal_checkpoint(TRUNCATE);")?;
    drop(conn);
    Ok(candidate)
}

fn verify_candidate(candidate: &Path, bundle_dir: &Path) -> Result<()> {
    let mut cand_store = Store {
        conn: Connection::open_with_flags(candidate, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?,
        store_id: String::new(),
        data_dir: bundle_dir.to_path_buf(),
        db_path: candidate.to_path_buf(),
        _lease: None,
    };
    cand_store.store_id =
        cand_store
            .conn
            .query_row("SELECT store_id FROM store_metadata LIMIT 1", [], |r| {
                r.get(0)
            })?;
    verify::full_verify(&mut cand_store).context("restored candidate failed full verification")
}

fn activate_candidate(
    data_dir: &Path,
    bundle_dir: &Path,
    final_db: &Path,
    candidate: &Path,
) -> Result<()> {
    crate::backup::copy_verified_objects(bundle_dir, &data_dir.join("objects"))?;
    File::open(candidate)?.sync_all()?;
    remove_active_journal_files(data_dir)?;
    hold_before_switch();
    failpoint("restore_before_active_switch");
    fs::rename(candidate, final_db)?;
    crate::store::ensure_private_file(final_db)?;
    sync_dir(data_dir)?;
    failpoint("restore_after_active_switch");
    Ok(())
}

fn remove_active_journal_files(data_dir: &Path) -> Result<()> {
    for suffix in ["snag.sqlite-wal", "snag.sqlite-shm"] {
        let p = data_dir.join(suffix);
        match fs::symlink_metadata(&p) {
            Ok(meta) if meta.file_type().is_file() => fs::remove_file(&p)?,
            Ok(_) => anyhow::bail!("active {} is not a regular file", suffix),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn hold_before_switch() {
    if std::env::var("SNAG_FAILPOINT_HOLD").as_deref() == Ok("restore_before_active_switch")
        && std::env::var("SNAG_FAILPOINT_HOLD_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .is_some()
    {
        let ms = std::env::var("SNAG_FAILPOINT_HOLD_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

fn verify_active(data_dir: &Path) -> Result<()> {
    let mut active = Store::open_read_only_unlocked_at(data_dir)?;
    verify::full_verify(&mut active).context("post-restore full verification failed")
}

fn emit_receipt(
    data_dir: &Path,
    args: &RestoreArgs,
    previous_head: &str,
    restored_head: &str,
    forensic_dir: &Path,
) -> Result<()> {
    let ts = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let receipt = json!({"restore_receipt_schema": 1, "source_backup": args.archive.to_string_lossy(), "previous_store_head": previous_head, "restored_head": restored_head, "forensic_copy": forensic_dir.to_string_lossy(), "completed_at": ts});
    let receipts_dir = data_dir.join("restore-receipts");
    crate::store::ensure_private_dir(&receipts_dir)?;
    let receipt_path = receipts_dir.join(format!("restore-{}.json", ts.replace(':', "")));
    fs::write(&receipt_path, serde_json::to_string_pretty(&receipt)?)?;
    File::open(&receipt_path)?.sync_all()?;
    crate::store::ensure_private_file(&receipt_path)
}
