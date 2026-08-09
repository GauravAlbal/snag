use crate::cli::BackupArgs;
use crate::failpoint::failpoint;
use crate::store::Store;
use crate::verify;
use anyhow::{Context, Result};
use flate2::Compression;
use flate2::write::GzEncoder;
use rusqlite::Connection;
use serde_json::json;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

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
        let ok = if let Ok(meta) = fs::symlink_metadata(&abs_path) {
            let len_ok = meta.file_type().is_file() && meta.len() as i64 == byte_length;
            let digest_ok = len_ok && file_digest(&abs_path).map(|d| d == digest).unwrap_or(false);
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

fn open_regular(path: &Path) -> Result<File> {
    let path_meta =
        fs::symlink_metadata(path).with_context(|| format!("cannot inspect {}", path.display()))?;
    if !path_meta.file_type().is_file() {
        anyhow::bail!(
            "expected regular file, found non-regular entry: {}",
            path.display()
        );
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options.open(path)?;
    let fd_meta = file.metadata()?;
    if !fd_meta.file_type().is_file() {
        anyhow::bail!(
            "expected regular file, found non-regular entry: {}",
            path.display()
        );
    }
    #[cfg(unix)]
    if fd_meta.dev() != path_meta.dev() || fd_meta.ino() != path_meta.ino() {
        anyhow::bail!("file changed while opening: {}", path.display());
    }
    Ok(file)
}

/// Copy a regular file without following a source symlink, checking that the
/// opened inode was not replaced while it was read.
pub fn copy_regular_file(src: &Path, dst: &Path) -> Result<()> {
    let before = fs::symlink_metadata(src)?;
    let mut input = open_regular(src)?;
    let input_meta = input.metadata()?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)
        .with_context(|| format!("cannot create {}", dst.display()))?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    crate::store::ensure_private_file(dst)?;
    let after = fs::symlink_metadata(src)?;
    let final_input = input.metadata()?;
    if before.len() != input_meta.len()
        || input_meta.len() != final_input.len()
        || after.len() != final_input.len()
    {
        anyhow::bail!("source file changed while copying: {}", src.display());
    }
    #[cfg(unix)]
    if after.dev() != input_meta.dev() || after.ino() != input_meta.ino() {
        anyhow::bail!("source file replaced while copying: {}", src.display());
    }
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if !meta.file_type().is_dir() {
        anyhow::bail!("snapshot source is not a directory: {}", src.display());
    }
    crate::store::ensure_private_dir(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let s = entry.path();
        let d = dst.join(entry.file_name());
        let entry_meta = fs::symlink_metadata(&s)?;
        if entry_meta.file_type().is_symlink() {
            anyhow::bail!("symlink is not allowed in bundle: {}", s.display());
        } else if entry_meta.file_type().is_dir() {
            copy_tree(&s, &d)?;
        } else if entry_meta.file_type().is_file() {
            copy_regular_file(&s, &d)?;
        } else {
            anyhow::bail!("non-regular bundle entry: {}", s.display());
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

/// Aggregated statistics read from the staged backup copy, used to build the
/// manifest and the backup_checkpoints row.
struct ManifestStats {
    through_sequence: i64,
    head_record_hash: String,
    observation_count: i64,
    action_count: i64,
    record_count: i64,
    artifact_count: i64,
    integrity_check: String,
    foreign_key_check: i64,
    store_id: String,
}

fn read_manifest_stats(conn: &Connection) -> Result<ManifestStats> {
    let head_record_hash: String = conn
        .query_row(
            "SELECT record_hash FROM records ORDER BY local_sequence DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| {
            "0000000000000000000000000000000000000000000000000000000000000000".to_string()
        });
    Ok(ManifestStats {
        through_sequence: conn.query_row(
            "SELECT COALESCE(MAX(local_sequence), 0) FROM records",
            [],
            |r| r.get(0),
        )?,
        head_record_hash,
        observation_count: conn.query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))?,
        action_count: conn
            .query_row("SELECT COUNT(*) FROM observation_actions", [], |r| r.get(0))?,
        record_count: conn.query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0))?,
        artifact_count: conn.query_row("SELECT COUNT(*) FROM artifacts", [], |r| r.get(0))?,
        integrity_check: conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?,
        foreign_key_check: conn.query_row(
            "SELECT count(*) FROM pragma_foreign_key_check",
            [],
            |r| r.get(0),
        )?,
        store_id: conn.query_row("SELECT store_id FROM store_metadata LIMIT 1", [], |r| {
            r.get(0)
        })?,
    })
}

/// Quiesced online copy of the live DB into the staging area.
fn stage_online_copy(store: &Store, stage_root: &Path) -> Result<PathBuf> {
    let staged_db = stage_root.join("snag.sqlite");
    {
        let mut dest = Connection::open(&staged_db)?;
        {
            let backup = rusqlite::backup::Backup::new(&store.conn, &mut dest)?;
            backup.step(-1)?;
        }
        dest.execute_batch("PRAGMA journal_mode = DELETE; PRAGMA wal_checkpoint(TRUNCATE);")?;
    }
    crate::store::ensure_private_file(&staged_db)?;
    Ok(staged_db)
}

/// Copy artifact objects so the bundle is self-contained (idempotent).
fn copy_objects_if_present(src: &Path, dst: &Path) -> Result<()> {
    failpoint("backup_during_object_copy");
    if src.exists() {
        copy_objects(src, dst)?;
    }
    Ok(())
}

/// Full verification of the staged copy BEFORE publication.
fn verify_staged(stage_root: &Path) -> Result<()> {
    let mut staged_store = Store::open_read_only_at(stage_root)?;
    verify::full_verify(&mut staged_store).context("backup copy failed full verification")?;
    failpoint("backup_after_verification");
    Ok(())
}

/// Write objects-manifest.json (verifies every artifact path/length/digest) and
/// return its path plus digest.
fn write_objects_manifest(conn: &Connection, stage_root: &Path) -> Result<(PathBuf, String)> {
    let (obj_errors, object_manifest_entries) = build_objects_manifest(conn, stage_root)?;
    if !obj_errors.is_empty() {
        anyhow::bail!("artifact verification failed: {}", obj_errors.join("; "));
    }
    let artifact_count: i64 = conn.query_row("SELECT COUNT(*) FROM artifacts", [], |r| r.get(0))?;
    let objects_manifest_path = stage_root.join("objects-manifest.json");
    fs::write(
        &objects_manifest_path,
        serde_json::to_string_pretty(&json!({
            "schema_version": 1,
            "artifact_count": artifact_count,
            "artifacts": object_manifest_entries,
        }))?,
    )?;
    crate::store::ensure_private_file(&objects_manifest_path)?;
    let artifact_manifest_digest = file_digest(&objects_manifest_path)?;
    Ok((objects_manifest_path, artifact_manifest_digest))
}

/// Write manifest.json with real, populated values; return path and timestamp.
fn write_manifest(
    stage_root: &Path,
    stats: &ManifestStats,
    database_digest: &str,
    artifact_manifest_digest: &str,
) -> Result<(PathBuf, String)> {
    let created_at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let manifest = json!({
        "schema_version": MANIFEST_SCHEMA_VERSION,
        "store_id": stats.store_id,
        "created_at": created_at,
        "through_sequence": stats.through_sequence,
        "head_record_hash": stats.head_record_hash,
        "database_digest": database_digest,
        "integrity_check": stats.integrity_check,
        "foreign_key_check": stats.foreign_key_check,
        "record_chain_check": true,
        "observation_count": stats.observation_count,
        "action_count": stats.action_count,
        "record_count": stats.record_count,
        "artifact_count": stats.artifact_count,
        "artifact_manifest_digest": artifact_manifest_digest,
        "self_contained": true,
    });
    let manifest_path = stage_root.join("manifest.json");
    fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;
    crate::store::ensure_private_file(&manifest_path)?;
    failpoint("backup_after_manifest_write");
    Ok((manifest_path, created_at))
}

/// Files that make up the published bundle.
struct StagedBundle {
    staged_db: PathBuf,
    manifest_path: PathBuf,
    objects_manifest_path: PathBuf,
    dst_objects: PathBuf,
}

/// Publish atomically: temp archive -> fsync -> rename. Returns final path.
fn durable_publish(
    backups_dir: &Path,
    bundle: &StagedBundle,
    head_record_hash: &str,
    created_at: &str,
) -> Result<PathBuf> {
    let short_hash = head_record_hash
        .strip_prefix("blake3:")
        .unwrap_or(head_record_hash)
        .chars()
        .take(8)
        .collect::<String>();
    let archive_name = format!(
        "snag-backup-{}-{}.tar.gz",
        created_at.replace(':', ""),
        short_hash
    );
    let final_archive = backups_dir.join(&archive_name);
    let tmp_archive = backups_dir.join(format!("{}.tmp.{}", archive_name, ulid::Ulid::generate()));
    failpoint("backup_before_publish");

    {
        let tar_gz = File::create(&tmp_archive)?;
        let enc = GzEncoder::new(tar_gz, Compression::default());
        let mut builder = tar::Builder::new(enc);
        builder.append_path_with_name(&bundle.staged_db, "snag.sqlite")?;
        builder.append_path_with_name(&bundle.manifest_path, "manifest.json")?;
        builder.append_path_with_name(&bundle.objects_manifest_path, "objects-manifest.json")?;
        if bundle.dst_objects.exists() {
            append_dir(&mut builder, &bundle.dst_objects, "objects")?;
        }
        let enc = builder.into_inner()?;
        let raw = enc.finish()?;
        raw.sync_all()?;
    }
    crate::store::ensure_private_file(&tmp_archive)?;
    File::open(&tmp_archive)?.sync_all()?;
    fs::rename(&tmp_archive, &final_archive)?;
    crate::store::ensure_private_file(&final_archive)?;
    sync_dir(backups_dir)?;
    failpoint("backup_after_publish");
    Ok(final_archive)
}

/// Record the backup checkpoint ONLY after publication succeeded.
fn record_checkpoint(
    store: &mut Store,
    stats: &ManifestStats,
    database_digest: &str,
    artifact_manifest_digest: &str,
    created_at: &str,
) -> Result<()> {
    let tx = store.conn.transaction()?;
    tx.execute(
        "INSERT INTO backup_checkpoints (backup_id, store_id, created_at, through_sequence, head_record_hash, database_digest, manifest_digest)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            ulid::Ulid::generate().to_string(),
            stats.store_id,
            created_at,
            stats.through_sequence,
            stats.head_record_hash,
            database_digest,
            artifact_manifest_digest,
        ],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn handle(_args: BackupArgs) -> Result<()> {
    let mut store = Store::open_read_write()?;
    let backups_dir = store.data_dir.join("backups");
    crate::store::ensure_private_dir(&backups_dir)?;

    // Stage the bundle in a temp dir so publication can be an atomic rename.
    let staging = tempfile::tempdir_in(&backups_dir).context("failed to create staging dir")?;
    crate::store::ensure_private_dir(staging.path())?;
    let stage_root = staging.path();
    let staged_db = stage_online_copy(&store, stage_root)?;
    failpoint("backup_after_db_copy");
    let src_objects = store.data_dir.join("objects");
    let dst_objects = stage_root.join("objects");
    copy_objects_if_present(&src_objects, &dst_objects)?;
    verify_staged(stage_root)?;

    let conn = Connection::open(&staged_db)?;
    let stats = read_manifest_stats(&conn)?;
    let (objects_manifest_path, artifact_manifest_digest) =
        write_objects_manifest(&conn, stage_root)?;
    let database_digest = file_digest(&staged_db)?;
    let (manifest_path, created_at) = write_manifest(
        stage_root,
        &stats,
        &database_digest,
        &artifact_manifest_digest,
    )?;

    // Durability: sync staged DB + manifests + objects before publishing.
    sync_tree(stage_root)?;

    let bundle = StagedBundle {
        staged_db: staged_db.clone(),
        manifest_path,
        objects_manifest_path,
        dst_objects,
    };
    let final_archive =
        durable_publish(&backups_dir, &bundle, &stats.head_record_hash, &created_at)?;

    record_checkpoint(
        &mut store,
        &stats,
        &database_digest,
        &artifact_manifest_digest,
        &created_at,
    )?;

    println!("Backup verified and saved to: {}", final_archive.display());
    Ok(())
}

/// Hard limits applied before any archive payload is written.
pub const MAX_ARCHIVE_EXPANDED_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_ARCHIVE_ENTRY_COUNT: usize = 100_000;
pub const MAX_ARCHIVE_ENTRY_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_ARCHIVE_PATH_DEPTH: usize = 16;

/// An owned, private bundle snapshot. The temporary directory is removed when
/// this value is dropped; callers must not retain paths after it is dropped.
pub struct BundleSnapshot {
    temp: tempfile::TempDir,
}

impl BundleSnapshot {
    pub fn path(&self) -> &Path {
        self.temp.path()
    }
}

fn validate_relative_bundle_path(path: &Path) -> Result<usize> {
    let mut depth = 0;
    for component in path.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                anyhow::bail!("unsafe archive path: {}", path.display())
            }
        }
    }
    if depth == 0
        || depth
            > configured_archive_limit("SNAG_ARCHIVE_MAX_DEPTH", MAX_ARCHIVE_PATH_DEPTH as u64)
                as usize
    {
        anyhow::bail!("archive path depth exceeds limit: {}", path.display());
    }
    Ok(depth)
}
fn configured_archive_limit(name: &str, hard: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(hard))
        .unwrap_or(hard)
}

fn validate_required_files(root: &Path) -> Result<()> {
    for required in BUNDLE_FILES {
        let path = root.join(required);
        let meta = fs::symlink_metadata(&path)
            .with_context(|| format!("backup bundle missing required file: {}", required))?;
        if !meta.file_type().is_file() {
            anyhow::bail!("required bundle entry is not regular: {}", required);
        }
    }
    Ok(())
}
fn validate_archive_entry<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    seen: &mut HashSet<PathBuf>,
    entries: &mut usize,
    max_entries: usize,
) -> Result<(PathBuf, bool)> {
    *entries += 1;
    if *entries > max_entries {
        anyhow::bail!("archive entry count exceeds limit");
    }
    let rel = entry.path()?.into_owned();
    validate_relative_bundle_path(&rel)?;
    if !seen.insert(rel.clone()) {
        anyhow::bail!("duplicate archive entry: {}", rel.display());
    }
    let kind = entry.header().entry_type();
    if kind.is_symlink() || kind.is_hard_link() || !kind.is_file() && !kind.is_dir() {
        anyhow::bail!("archive contains forbidden entry type: {}", rel.display());
    }
    Ok((rel, kind.is_dir()))
}

fn extract_archive_entry<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    root: &Path,
    rel: &Path,
    is_dir: bool,
    expanded: &mut u64,
    max_entry: u64,
    max_total: u64,
) -> Result<()> {
    let out = root.join(rel);
    if is_dir {
        fs::create_dir_all(&out)?;
        crate::store::ensure_private_dir(&out)?;
        return Ok(());
    }
    let size = entry.size();
    if size > max_entry {
        anyhow::bail!(
            "archive entry exceeds per-entry byte limit: {}",
            rel.display()
        );
    }
    *expanded = expanded
        .checked_add(size)
        .context("archive expanded byte limit overflow")?;
    if *expanded > max_total {
        anyhow::bail!("archive expanded byte limit exceeded");
    }
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
        crate::store::ensure_private_dir(parent)?;
    }
    let mut output = OpenOptions::new().write(true).create_new(true).open(&out)?;
    let copied = io::copy(entry, &mut output)?;
    if copied != size {
        anyhow::bail!(
            "archive entry size changed while extracting: {}",
            rel.display()
        );
    }
    output.sync_all()?;
    crate::store::ensure_private_file(&out)?;
    Ok(())
}

fn extract_archive(path: &Path, root: &Path) -> Result<()> {
    let f = open_regular(path)?;
    let dec = flate2::read::GzDecoder::new(f);
    let mut archive = tar::Archive::new(dec);
    let mut seen = HashSet::new();
    let mut entries = 0usize;
    let mut expanded = 0u64;
    let max_entries =
        configured_archive_limit("SNAG_ARCHIVE_MAX_ENTRIES", MAX_ARCHIVE_ENTRY_COUNT as u64)
            as usize;
    let max_total =
        configured_archive_limit("SNAG_ARCHIVE_MAX_TOTAL_BYTES", MAX_ARCHIVE_EXPANDED_BYTES);
    let max_entry =
        configured_archive_limit("SNAG_ARCHIVE_MAX_ENTRY_BYTES", MAX_ARCHIVE_ENTRY_BYTES);
    for item in archive.entries()? {
        let mut entry = item?;
        let (rel, is_dir) =
            validate_archive_entry(&mut entry, &mut seen, &mut entries, max_entries)?;
        extract_archive_entry(
            &mut entry,
            root,
            &rel,
            is_dir,
            &mut expanded,
            max_entry,
            max_total,
        )?;
    }
    validate_required_files(root)?;
    Ok(())
}

pub fn resolve_bundle(path: &Path) -> Result<BundleSnapshot> {
    let temp = tempfile::tempdir()?;
    let source_meta = fs::symlink_metadata(path)?;
    crate::store::ensure_private_dir(temp.path())?;
    if source_meta.file_type().is_symlink() {
        anyhow::bail!("bundle path may not be a symlink");
    }
    if source_meta.file_type().is_dir() {
        copy_tree(path, temp.path())?;
    } else if source_meta.file_type().is_file() {
        extract_archive(path, temp.path())?;
    } else {
        anyhow::bail!("bundle path is not a regular file or directory");
    }
    validate_required_files(temp.path())?;
    Ok(BundleSnapshot { temp })
}

fn copy_verified_object_entry(
    bundle_dir: &Path,
    dst: &Path,
    entry: &serde_json::Value,
    seen: &mut HashSet<String>,
) -> Result<()> {
    let digest = entry["digest"].as_str().context("entry missing digest")?;
    let rel = artifact_rel_path(digest).context("entry has malformed digest")?;
    let declared = entry["path"].as_str().context("entry missing path")?;
    if declared != rel || !seen.insert(rel.clone()) {
        anyhow::bail!("object manifest contains non-canonical or duplicate path");
    }
    let src = bundle_dir.join(&rel);
    let src_meta = fs::symlink_metadata(&src)?;
    if !src_meta.file_type().is_file() {
        anyhow::bail!("manifest object is not regular: {}", rel);
    }
    let out = dst.join(format!("blake3/{}/{}", &digest[7..9], &digest[7..]));
    if let Some(parent) = out.parent() {
        if let Ok(meta) = fs::symlink_metadata(parent) {
            if !meta.file_type().is_dir() {
                anyhow::bail!("active object parent is not a directory");
            }
            crate::store::ensure_private_dir(parent)?;
        } else {
            fs::create_dir_all(parent)?;
            crate::store::ensure_private_dir(parent)?;
        }
    }
    match copy_regular_file(&src, &out) {
        Ok(()) => {}
        Err(err) if out.exists() && fs::symlink_metadata(&out)?.file_type().is_file() => {
            crate::store::ensure_private_file(&out)?;
            let _ = err;
        }
        Err(err) => return Err(err),
    }
    Ok(())
}

/// Copy only canonical objects enumerated by objects-manifest.json.
pub fn copy_verified_objects(bundle_dir: &Path, dst: &Path) -> Result<()> {
    let raw = fs::read_to_string(bundle_dir.join("objects-manifest.json"))?;
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    let entries = value["artifacts"]
        .as_array()
        .context("objects-manifest missing artifacts")?;
    let dst_meta = fs::symlink_metadata(dst);
    if let Ok(meta) = dst_meta {
        if !meta.file_type().is_dir() {
            anyhow::bail!("active objects path is not a directory");
        }
        crate::store::ensure_private_dir(dst)?;
    }
    crate::store::ensure_private_dir(dst)?;
    crate::store::ensure_private_dir(&dst.join("blake3"))?;
    let mut seen = HashSet::new();
    for entry in entries {
        copy_verified_object_entry(bundle_dir, dst, entry, &mut seen)?;
    }
    Ok(())
}
