use crate::schema::{apply_migrations, init_connection};
use crate::types::{generate_id, Observation};
use anyhow::{Context, Result};
use directories::ProjectDirs;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};

pub struct Store {
    pub conn: Connection,
    pub store_id: String,
    pub data_dir: PathBuf,
}

impl Store {
    pub fn open() -> Result<Self> {
        let proj_dirs = ProjectDirs::from("", "", "snag").context("Failed to determine project directories")?;
        let data_dir = proj_dirs.data_dir().to_path_buf();
        
        if !data_dir.exists() {
            fs::create_dir_all(&data_dir)?;
            
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o700))?;
            }
        }
        
        let db_path = data_dir.join("snag.sqlite");
        let mut conn = Connection::open(&db_path)?;
        init_connection(&conn)?;
        apply_migrations(&mut conn)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = fs::metadata(&db_path) {
                fs::set_permissions(&db_path, fs::Permissions::from_mode(0o600)).ok();
            }
        }
        
        // Ensure store_id exists
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

        Ok(Self {
            conn,
            store_id,
            data_dir,
        })
    }

    pub fn insert_observation(&mut self, _obs: &Observation) -> Result<()> {
        // ... to be implemented for the report logic ...
        Ok(())
    }
}

// Added extension trait for Option
use rusqlite::OptionalExtension;
