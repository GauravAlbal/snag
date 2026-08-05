use crate::schema::{apply_migrations, initialize_reader_connection, initialize_writer_connection};
use crate::types::generate_id;
use anyhow::{Context, Result};
use directories::ProjectDirs;
use rusqlite::Connection;
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

pub struct Store {
    pub conn: Connection,
    pub store_id: String,
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
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
        fs::create_dir_all(data_dir)?;
        let mut conn = Connection::open(&db_path)?;
        initialize_writer_connection(&conn)?;

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
            fs::create_dir_all(&forensics)?;
            let ts = time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap()
                .replace(':', "");
            let copy = forensics.join(format!("pre-v2-migration-{}.sqlite", ts));
            let _ = fs::copy(&db_path, &copy);
        }

        apply_migrations(&mut conn)?;

        let store_id = Self::ensure_store_id(&conn)?;

        let store = Self {
            conn,
            store_id,
            data_dir: data_dir.to_path_buf(),
            db_path: db_path.clone(),
        };

        // G33: verify the full resulting store immediately after a migration.
        if pre_migration_version < 2 {
            let mut s = store;
            crate::verify::full_verify(&mut s)
                .context("migration produced an invalid store; original preserved in forensics/")?;
            return Ok(s);
        }

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
        })
    }

    pub fn open_read_only() -> Result<Self> {
        let (data_dir, _db_path) = Self::paths()?;
        Self::open_read_only_at(&data_dir)
    }

    pub fn open_for_maintenance() -> Result<Self> {
        Self::open_read_write()
    }

    fn ensure_store_id(conn: &Connection) -> Result<String> {
        let store_id: Option<String> = conn
            .query_row("SELECT store_id FROM store_metadata LIMIT 1", [], |row| {
                row.get(0)
            })
            .optional()?;

        let store_id = match store_id {
            Some(id) => id,
            None => {
                let new_id = generate_id("store");
                let now = time::OffsetDateTime::now_utc()
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap();
                conn.execute(
                    "INSERT INTO store_metadata (store_id, created_at) VALUES (?1, ?2)",
                    [&new_id, &now],
                )?;
                new_id
            }
        };

        Ok(store_id)
    }
}

// Added extension trait for Option
use rusqlite::OptionalExtension;
