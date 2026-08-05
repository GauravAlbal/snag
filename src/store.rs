use crate::schema::{apply_migrations, init_connection};
use crate::types::{generate_id, Observation};
use anyhow::{Context, Result};
use directories::ProjectDirs;
use rusqlite::Connection;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

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

    pub fn open_at(data_dir: &PathBuf) -> Result<Self> {
        let db_path = data_dir.join("snag.sqlite");
        fs::create_dir_all(&data_dir)?;
        let mut conn = Connection::open(&db_path)?;
        init_connection(&conn)?;
        
        let tx = conn.transaction()?;
        tx.commit()?;
        
        apply_migrations(&mut conn)?;

        let store_id = Self::ensure_store_id(&conn)?;

        Ok(Self {
            conn,
            store_id,
            data_dir: data_dir.clone(),
            db_path,
        })
    }

    pub fn open_read_write() -> Result<Self> {
        let (data_dir, _db_path) = Self::paths()?;
        Self::open_at(&data_dir)
    }
    
    pub fn open_read_only_at(data_dir: &PathBuf) -> Result<Self> {
        let db_path = data_dir.join("snag.sqlite");
        if !db_path.exists() {
            return Err(anyhow::anyhow!("Store not found (has snag report been run?)"));
        }
        
        let conn = Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        init_connection(&conn)?;
        
        let store_id: String = conn.query_row("SELECT store_id FROM store_metadata LIMIT 1", [], |row| row.get(0))?;

        Ok(Self {
            conn,
            store_id,
            data_dir: data_dir.clone(),
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
        let store_id: Option<String> = conn.query_row(
            "SELECT store_id FROM store_metadata LIMIT 1",
            [],
            |row| row.get(0),
        ).optional()?;

        let store_id = match store_id {
            Some(id) => id,
            None => {
                let new_id = generate_id("store");
                let now = time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap();
                conn.execute(
                    "INSERT INTO store_metadata (store_id, created_at) VALUES (?1, ?2)",
                    [&new_id, &now],
                )?;
                new_id
            }
        };

        Ok(store_id)
    }

    pub fn insert_observation(&mut self, _obs: &Observation) -> Result<()> {
        // ... to be implemented for the report logic ...
        Ok(())
    }
}

// Added extension trait for Option
use rusqlite::OptionalExtension;
