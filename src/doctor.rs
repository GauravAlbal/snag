use crate::cli::DoctorArgs;
use crate::store::Store;
use anyhow::Result;
use std::fs;

pub fn handle(_args: DoctorArgs) -> Result<()> {
    println!("snag {} (doctor)", env!("CARGO_PKG_VERSION"));
    println!();

    // Effective context source.
    let ctx_file = std::env::var("SNAG_CONTEXT_FILE").ok();
    println!(
        "Context file:  {}",
        ctx_file.as_deref().unwrap_or("(not set)")
    );
    let source_kind = std::env::var("SNAG_SOURCE_KIND").unwrap_or_else(|_| "human_explicit".into());
    match std::env::var("SNAG_REPORTER_ID").ok() {
        Some(reporter) => {
            println!("Context env:   SNAG_SOURCE_KIND={source_kind}  SNAG_REPORTER_ID={reporter}")
        }
        None => println!("Context env:   SNAG_SOURCE_KIND={source_kind}"),
    }

    // Store paths. Reported even when no store exists yet, so users never have
    // to guess where data would live.
    let (data_dir, db_path) = Store::paths()?;
    let objects_dir = data_dir.join("objects").join("blake3");
    let backups_dir = data_dir.join("backups");
    println!("Database:      {}", db_path.display());
    println!("Objects:       {}", objects_dir.display());
    println!("Backups:       {}", backups_dir.display());
    println!();

    // Store access
    match Store::open_read_only() {
        Ok(store) => {
            println!("✅ Store access: OK");

            // Check backups
            if backups_dir.exists() {
                let count = fs::read_dir(&backups_dir)?.count();
                println!("✅ Backups directory: OK ({} backups found)", count);
            } else {
                println!("⚠️  Backups directory: Missing (run `snag backup` to initialize)");
            }

            // SQLite integrity check
            match store
                .conn
                .query_row::<String, _, _>("PRAGMA integrity_check", [], |row| row.get(0))
            {
                Ok(res) if res == "ok" => println!("✅ SQLite integrity: OK"),
                Ok(res) => println!("❌ SQLite integrity: FAILED ({})", res),
                Err(e) => println!("❌ SQLite integrity: ERROR ({})", e),
            }
        }
        Err(e) => {
            println!("❌ Store access: FAILED ({})", e);
        }
    }

    // Git Check
    if let Ok(cwd) = std::env::current_dir() {
        match crate::git::collect_git_context(&cwd) {
            Ok(ctx) => {
                if ctx.repository_root.is_some() {
                    println!("✅ Git context collection: OK (in repository)");
                } else {
                    println!("✅ Git context collection: OK (not in a repository)");
                }
            }
            Err(e) => {
                println!("⚠️  Git context collection: FAILED ({})", e);
            }
        }
    }

    println!("\nDiagnostics complete.");
    Ok(())
}
