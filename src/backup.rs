use crate::cli::BackupArgs;
use crate::store::Store;
use crate::verify;
use anyhow::{Context, Result};
use flate2::Compression;
use flate2::write::GzEncoder;
use rusqlite::Connection;
use serde_json::json;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Manifest schema version for the self-contained v0 backup bundle.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Files that must be present at the root of a valid backup bundle.
pub const BUNDLE_FILES: [&str; 3] = ["snag.sqlite", "manifest.json", "objects-manifest.json"];

/// Compute a BLAKE3 digest over the raw bytes of a file.
pub fn file_digest(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

/// Map a `blake3:<hex>` digest to its canonical object path relative to the
/// bundle's `objects/` root, i.e. `objects/blake3/<prefix>/<hex>`.
pub fn artifact_rel_path(digest: &str) -> Option<String> {
    let hex = digest.strip_prefix("blake3:")?;
    if hex.len() != 64 {
        return None;
    }
    let prefix = &hex[0..2];
    Some(format!("objects/blake3/{}/{}", prefix, hex))
}

/// Build the `objects-manifest.json` content for a store: one entry per
/// artifact row with its canonical path, expected byte length, and digest.
/// Any artifact that is missing, wrong length, or wrong digest is reported as
/// an error (so a backup is never published with broken artifact recoverability).
fn build_objects_manifest(
    conn: &Connection,
    bundle_root: &Path,
) -> Result<(Vec<String>, Vec<serde_json::Value>)> {
    let mut stmt = conn.prepare("SELECT digest, byte_length FROM artifacts ORDER BY digest")?;
    let mut rows = stmt.query([])?;

    let mut entries = Vec::new();
    let mut errors = Vec::new();
    while let Some(row) = rows.next()? {
        let digest: String = row.get(0)?;
        let byte_length: i64 = row.get(1)?;
        let rel_path = match artifact_rel_path(&digest) {
            Some(p) => p,
            None => {
                errors.push(format!("malformed artifact digest: {}", digest));
                continue;
            }
        };
        let abs_path = bundle_root.join(&rel_path);
        let ok = if let Ok(meta) = fs::metadata(&abs_path) {
            let len_ok = meta.len() as i64 == byte_length;
            let digest_ok = file_digest(&abs_path).map(|d| d == digest).unwrap_or(false);
            len_ok && digest_ok
        } else {
            false
        };
        if !ok {
            errors.push(format!(
                "artifact {} missing/invalid (expected len {}, digest {})",
                rel_path, byte_length, digest
            ));
        }
        entries.push(json!({
            "digest": digest,
            "byte_length": byte_length,
            "path": rel_path,
        }));
    }

    Ok((errors, entries))
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let s = entry.path();
        let d = dst.join(entry.file_name());
        if s.is_dir() {
            copy_tree(&s, &d)?;
        } else if s.is_file() {
            fs::copy(&s, &d)?;
            File::open(&d)?.sync_all()?;
        }
    }
    Ok(())
}

fn copy_objects(src: &Path, dst: &Path) -> Result<()> {
    copy_tree(src, dst)
}

fn append_dir(
    tar: &mut tar::Builder<flate2::write::GzEncoder<File>>,
    dir: &Path,
    prefix: &str,
) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let rel = format!("{}/{}", prefix, entry.file_name().to_string_lossy());
        if path.is_dir() {
            append_dir(tar, &path, &rel)?;
        } else if path.is_file() {
            tar.append_path_with_name(&path, &rel)?;
        }
    }
    Ok(())
}

fn sync_tree(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            File::open(&path)?.sync_all()?;
        } else if path.is_dir() {
            sync_tree(&path)?;
        }
    }
    sync_dir(root)?;
    Ok(())
}

fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let fd = File::open(dir)?;
        fd.sync_all()?;
    }
    Ok(())
}

pub fn handle(_args: BackupArgs) -> Result<()> {
    let store = Store::open_read_only()?;
    let backups_dir = store.data_dir.join("backups");
    fs::create_dir_all(&backups_dir)?;

    // Stage the bundle in a temp dir so publication can be an atomic rename.
    let staging = tempfile::tempdir_in(&backups_dir).context("failed to create staging dir")?;
    let stage_root = staging.path();

    // 1. Quiesced online backup of the live DB into the staging area.
    let staged_db = stage_root.join("snag.sqlite");
    {
        let mut dest = Connection::open(&staged_db)?;
        {
            let backup = rusqlite::backup::Backup::new(&store.conn, &mut dest)?;
            backup.step(-1)?;
        }
        dest.execute_batch("PRAGMA journal_mode = DELETE; PRAGMA wal_checkpoint(TRUNCATE);")?;
    }

    // 2. Copy artifact objects so the bundle is self-contained.
    let src_objects = store.data_dir.join("objects");
    let dst_objects = stage_root.join("objects");
    if src_objects.exists() {
        copy_objects(&src_objects, &dst_objects)?;
    }

    // 3. Full verification of the staged copy BEFORE publication.
    let mut staged_store = Store::open_read_only_at(stage_root)?;
    verify::full_verify(&mut staged_store).context("backup copy failed full verification")?;

    let conn = Connection::open(&staged_db)?;
    let through_sequence: i64 = conn.query_row(
        "SELECT COALESCE(MAX(local_sequence), 0) FROM records",
        [],
        |r| r.get(0),
    )?;
    let head_record_hash: String = conn
        .query_row(
            "SELECT record_hash FROM records ORDER BY local_sequence DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| {
            "0000000000000000000000000000000000000000000000000000000000000000".to_string()
        });
    let observation_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))?;
    let action_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM observation_actions", [], |r| r.get(0))?;
    let record_count: i64 = conn.query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0))?;
    let artifact_count: i64 = conn.query_row("SELECT COUNT(*) FROM artifacts", [], |r| r.get(0))?;
    let integrity_check: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    let foreign_key_check: i64 =
        conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })?;
    let store_id: String =
        conn.query_row("SELECT store_id FROM store_metadata LIMIT 1", [], |r| {
            r.get(0)
        })?;

    // 4. Objects manifest (verifies every artifact path/length/digest).
    let (obj_errors, object_manifest_entries) = build_objects_manifest(&conn, stage_root)?;
    if !obj_errors.is_empty() {
        anyhow::bail!("artifact verification failed: {}", obj_errors.join("; "));
    }
    let objects_manifest_path = stage_root.join("objects-manifest.json");
    fs::write(
        &objects_manifest_path,
        serde_json::to_string_pretty(&json!({
            "schema_version": 1,
            "artifact_count": artifact_count,
            "artifacts": object_manifest_entries,
        }))?,
    )?;
    let artifact_manifest_digest = file_digest(&objects_manifest_path)?;

    // 5. Database digest after verification (represents the published bytes).
    let database_digest = file_digest(&staged_db)?;

    // 6. Manifest with real, populated values (no placeholders).
    let created_at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let manifest = json!({
        "schema_version": MANIFEST_SCHEMA_VERSION,
        "store_id": store_id,
        "created_at": created_at,
        "through_sequence": through_sequence,
        "head_record_hash": head_record_hash,
        "database_digest": database_digest,
        "integrity_check": integrity_check,
        "foreign_key_check": foreign_key_check,
        "record_chain_check": true,
        "observation_count": observation_count,
        "action_count": action_count,
        "record_count": record_count,
        "artifact_count": artifact_count,
        "artifact_manifest_digest": artifact_manifest_digest,
        "self_contained": true,
    });
    let manifest_path = stage_root.join("manifest.json");
    fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;

    // Durability: sync staged DB + manifests + objects before publishing.
    sync_tree(stage_root)?;

    // 7. Publish atomically: temp name -> fsync -> rename.
    let short_hash = head_record_hash
        .strip_prefix("blake3:")
        .unwrap_or(&head_record_hash)
        .chars()
        .take(8)
        .collect::<String>();
    let ts = created_at.replace(':', "");
    let archive_name = format!("snag-backup-{}-{}.tar.gz", ts, short_hash);
    let final_archive = backups_dir.join(&archive_name);
    let tmp_archive = backups_dir.join(format!("{}.tmp.{}", archive_name, ulid::Ulid::generate()));

    {
        let tar_gz = File::create(&tmp_archive)?;
        let enc = GzEncoder::new(tar_gz, Compression::default());
        let mut builder = tar::Builder::new(enc);
        builder.append_path_with_name(&staged_db, "snag.sqlite")?;
        builder.append_path_with_name(&manifest_path, "manifest.json")?;
        builder.append_path_with_name(&objects_manifest_path, "objects-manifest.json")?;
        if dst_objects.exists() {
            append_dir(&mut builder, &dst_objects, "objects")?;
        }
        let enc = builder.into_inner()?;
        let raw = enc.finish()?;
        raw.sync_all()?;
    }
    File::open(&tmp_archive)?.sync_all()?;
    fs::rename(&tmp_archive, &final_archive)?;
    sync_dir(&backups_dir)?;

    // 8. Record the backup checkpoint ONLY after publication succeeded.
    {
        let mut rw = Store::open_read_write()?;
        let tx = rw.conn.transaction()?;
        tx.execute(
            "INSERT INTO backup_checkpoints (backup_id, store_id, created_at, through_sequence, head_record_hash, database_digest, manifest_digest)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                ulid::Ulid::generate().to_string(),
                store_id,
                created_at,
                through_sequence,
                head_record_hash,
                database_digest,
                artifact_manifest_digest,
            ],
        )?;
        tx.commit()?;
    }

    println!("Backup verified and saved to: {}", final_archive.display());
    Ok(())
}

/// Low-level helpers needed by restore/verify. A backup is a directory (or an
/// extracted archive) whose layout is {snag.sqlite, manifest.json,
/// objects-manifest.json, objects/}. `prepare_backup_dir` resolves an on-disk
/// archive or directory to a verified-on-disk directory containing these.
pub fn resolve_bundle(path: &Path) -> Result<PathBuf> {
    if path.is_dir() {
        return Ok(path.to_path_buf());
    }
    // Assume a tar.gz archive; extract into a temp dir and validate layout.
    let tmp = tempfile::tempdir()?;
    let f = File::open(path)?;
    let dec = flate2::read::GzDecoder::new(f);
    let mut archive = tar::Archive::new(dec);
    archive.unpack(tmp.path())?;
    for required in BUNDLE_FILES {
        if !tmp.path().join(required).exists() {
            anyhow::bail!("backup bundle missing required file: {}", required);
        }
    }
    Ok(tmp.keep().to_path_buf())
}
