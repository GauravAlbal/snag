use crate::schema::{apply_migrations, initialize_reader_connection, initialize_writer_connection};
use crate::types::generate_id;
use anyhow::{Context, Result};
use directories::ProjectDirs;
use rusqlite::Connection;
use std::env;
use std::fs::{self, File};
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::path::PathBuf;

pub struct Store {
    pub conn: Connection,
    pub store_id: String,
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub(crate) _lease: Option<File>,
}

impl Store {
    pub fn paths() -> Result<(PathBuf, PathBuf)> {
        let project_dirs = ProjectDirs::from("", "", "snag-cli")
            .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
        let data_dir = if let Ok(data_home) = env::var("XDG_DATA_HOME") {
            PathBuf::from(data_home).join("snag")
        } else {
            project_dirs.data_dir().to_path_buf()
        };
        let db_path = data_dir.join("snag.sqlite");
        Ok((data_dir, db_path))
    }

    pub fn open_at(data_dir: &Path) -> Result<Self> {
        let db_path = data_dir.join("snag.sqlite");
        ensure_private_dir(data_dir)?;
        let lease = Self::acquire_shared(data_dir)?;
        repair_private_file(&db_path)?;
        let mut conn = Connection::open(&db_path)?;
        initialize_writer_connection(&conn)?;
        repair_store_permissions(data_dir, &db_path)?;

        let tx = conn.transaction()?;
        tx.commit()?;

        // G33: preserve a forensic copy before any v1->v2 migration so the old
        // database remains recoverable if the migration fails.
        let pre_migration_version: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version),0) FROM schema_migrations",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if pre_migration_version < 2 {
            let forensics = data_dir.join("forensics");
            ensure_private_dir(&forensics)?;
            let ts = time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap()
                .replace(':', "");
            let copy = forensics.join(format!("pre-v2-migration-{}.sqlite", ts));
            fs::copy(&db_path, &copy)?;
            ensure_private_file(&copy)?;
        }

        apply_migrations(&mut conn)?;

        let store_id = Self::ensure_store_id(&mut conn)?;

        let store = Self {
            conn,
            store_id,
            data_dir: data_dir.to_path_buf(),
            db_path: db_path.clone(),
            _lease: Some(lease),
        };

        // G33: verify the migration produced a structurally sound store. The
        // migration is transactional and its correctness for data-bearing
        // legacy stores is proven by the migration fixture tests (verified via
        // explicit `snag verify --full`). We do NOT walk the live record chain
        // here: a concurrent writer may legitimately advance the chain between
        // our migration and this read, which would false-fail a chain check.
        // Structural checks are safe under WAL concurrency and still catch a
        // broken migration.
        if pre_migration_version < 2 {
            let integrity: String = store
                .conn
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .context("migration produced an invalid store; original preserved in forensics/")?;
            if integrity != "ok" {
                anyhow::bail!(
                    "migration produced an invalid store ({integrity}); original preserved in forensics/"
                );
            }
        }
        repair_store_permissions(&store.data_dir, &store.db_path)?;

        Ok(store)
    }

    pub fn open_read_write() -> Result<Self> {
        let (data_dir, _db_path) = Self::paths()?;
        Self::open_at(&data_dir)
    }

    pub fn open_read_only_at(data_dir: &Path) -> Result<Self> {
        let db_path = data_dir.join("snag.sqlite");
        if !db_path.exists() {
            return Err(anyhow::anyhow!(
                "Store not found (has snag report been run?)"
            ));
        }
        let lease = Self::acquire_shared(data_dir)?;
        Self::open_read_only_at_with_lease(data_dir, Some(lease))
    }

    pub(crate) fn open_read_only_unlocked_at(data_dir: &Path) -> Result<Self> {
        Self::open_read_only_at_with_lease(data_dir, None)
    }

    fn open_read_only_at_with_lease(data_dir: &Path, lease: Option<File>) -> Result<Self> {
        let db_path = data_dir.join("snag.sqlite");
        if !db_path.exists() {
            return Err(anyhow::anyhow!(
                "Store not found (has snag report been run?)"
            ));
        }
        validate_store_permissions(data_dir, &db_path)?;

        let conn =
            Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        initialize_reader_connection(&conn)?;

        let store_id: String =
            conn.query_row("SELECT store_id FROM store_metadata LIMIT 1", [], |row| {
                row.get(0)
            })?;

        Ok(Self {
            conn,
            store_id,
            data_dir: data_dir.to_path_buf(),
            db_path,
            _lease: lease,
        })
    }

    pub fn open_read_only() -> Result<Self> {
        let (data_dir, _db_path) = Self::paths()?;
        Self::open_read_only_at(&data_dir)
    }

    pub fn open_for_maintenance() -> Result<Self> {
        Self::open_read_write()
    }

    pub(crate) fn acquire_shared(data_dir: &Path) -> Result<File> {
        acquire_lock(data_dir, libc::LOCK_SH)
    }

    pub(crate) fn acquire_exclusive(data_dir: &Path) -> Result<StoreLease> {
        acquire_lock(data_dir, libc::LOCK_EX).map(|file| StoreLease { _file: file })
    }

    fn ensure_store_id(conn: &mut Connection) -> Result<String> {
        // Atomic under an IMMEDIATE transaction so that when several processes
        // first open a fresh store concurrently, exactly one creates the store
        // id and every other process reads the SAME committed value. Otherwise
        // a report could hash its records against a store id that loses the
        // creation race, producing a chain that fails verification.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let store_id: String = tx
            .query_row("SELECT store_id FROM store_metadata LIMIT 1", [], |row| {
                row.get(0)
            })
            .optional()?
            .unwrap_or_else(|| {
                let new_id = generate_id("store");
                let now = time::OffsetDateTime::now_utc()
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap();
                tx.execute(
                    "INSERT INTO store_metadata (store_id, created_at) VALUES (?1, ?2)",
                    [&new_id, &now],
                )
                .expect("store_id insert");
                new_id
            });
        tx.commit()?;
        Ok(store_id)
    }
}
pub(crate) struct StoreLease {
    _file: File,
}

fn acquire_lock(data_dir: &Path, operation: libc::c_int) -> Result<File> {
    let file = File::open(data_dir)
        .with_context(|| format!("opening store lock directory {}", data_dir.display()))?;
    #[cfg(unix)]
    {
        let result = unsafe { libc::flock(file.as_raw_fd(), operation) };
        if result != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("locking store directory {}", data_dir.display()));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = operation;
        anyhow::bail!("store locking is unsupported on this platform");
    }
    Ok(file)
}
pub(crate) fn ensure_private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    #[cfg(not(unix))]
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        anyhow::bail!("managed directory is not a directory: {}", path.display());
    }
    set_private_mode(path, 0o700)
}
pub(crate) fn ensure_private_child_dir(path: &Path) -> Result<bool> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    match builder.create(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            ensure_private_dir(path)?;
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn ensure_private_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        anyhow::bail!("managed file is not a regular file: {}", path.display());
    }
    set_private_mode(path, 0o600)
}

fn repair_private_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => ensure_private_file(path),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn repair_private_tree(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() {
        anyhow::bail!("managed directory is not a directory: {}", path.display());
    }
    set_private_mode(path, 0o700)?;
    for entry in fs::read_dir(path)? {
        let child = entry?.path();
        let child_metadata = fs::symlink_metadata(&child)?;
        if child_metadata.is_dir() {
            repair_private_tree(&child)?;
        } else {
            ensure_private_file(&child)?;
        }
    }
    Ok(())
}

fn repair_store_permissions(data_dir: &Path, db_path: &Path) -> Result<()> {
    ensure_private_dir(data_dir)?;
    ensure_private_file(db_path)?;
    for suffix in ["-wal", "-shm"] {
        repair_private_file(&db_path.with_extension(format!(
            "{}{}",
            db_path.extension().and_then(|ext| ext.to_str()).unwrap_or(""),
            suffix
        )))?;
    }
    for name in [
        "objects",
        "backups",
        "forensics",
        "restore-receipts",
        "sessions",
    ] {
        repair_private_tree(&data_dir.join(name))?;
    }
    repair_private_file(&data_dir.join("review_session.json"))?;
    Ok(())
}

fn validate_private_mode(path: &Path, expected: u32, directory: bool) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!("managed path must not be a symlink: {}", path.display());
    }
    if metadata.is_dir() != directory {
        anyhow::bail!("managed path has unexpected type: {}", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o7777;
        if mode != expected {
            anyhow::bail!(
                "managed path has unsafe mode {:04o}: {} (expected {:04o})",
                mode,
                path.display(),
                expected
            );
        }
    }
    Ok(())
}

fn validate_private_tree(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    validate_private_mode(path, 0o700, true)?;
    for entry in fs::read_dir(path)? {
        let child = entry?.path();
        let metadata = fs::symlink_metadata(&child)?;
        if metadata.is_dir() {
            validate_private_tree(&child)?;
        } else {
            validate_private_mode(&child, 0o600, false)?;
        }
    }
    Ok(())
}

fn validate_store_permissions(data_dir: &Path, db_path: &Path) -> Result<()> {
    validate_private_mode(data_dir, 0o700, true)?;
    validate_private_mode(db_path, 0o600, false)?;
    for suffix in ["-wal", "-shm"] {
        let path = db_path.with_extension(format!(
            "{}{}",
            db_path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or(""),
            suffix
        ));
        match fs::symlink_metadata(&path) {
            Ok(_) => validate_private_mode(&path, 0o600, false)?,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    for name in [
        "objects",
        "backups",
        "forensics",
        "restore-receipts",
        "sessions",
    ] {
        validate_private_tree(&data_dir.join(name))?;
    }
    let path = data_dir.join("review_session.json");
    match fs::symlink_metadata(&path) {
        Ok(_) => validate_private_mode(&path, 0o600, false)?,
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!("managed path must not be a symlink: {}", path.display());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("failed to set private mode on {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

// Added extension trait for Option
use rusqlite::OptionalExtension;
