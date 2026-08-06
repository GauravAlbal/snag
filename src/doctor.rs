use crate::cli::DoctorArgs;
use crate::store::Store;
use anyhow::Result;
use std::fs;
use std::process::Command;

pub fn handle(_args: DoctorArgs) -> Result<()> {
    println!("snag {} (doctor)", crate::cli::BUILD_VERSION);
    println!();

    // Stale-binary guard: when the embedded source repository matches the
    // repo doctor is run from, compare the embedded revision against HEAD and
    // warn on drift. Dogfood findings: (a) a fix can sit committed in the tree
    // while the installed binary still runs older code (rev mismatch); (b) a
    // fix can sit UNcommitted in the tree with the installed binary built
    // from a dirty workspace (the `-dirty` marker on the built rev).
    let built_rev = env!("SNAG_BUILD_REV");
    let built_repo = env!("SNAG_BUILD_REPO_URL");
    if !built_repo.is_empty() && !built_rev.starts_with("unknown") {
        let here = Command::new("git")
            .args(["config", "--get", "remote.origin.url"])
            .output();
        let head = Command::new("git")
            .args(["rev-parse", "--short=7", "HEAD"])
            .output();
        if let (Ok(url_out), Ok(head_out)) = (here, head) {
            let url = String::from_utf8_lossy(&url_out.stdout).trim().to_string();
            let head = String::from_utf8_lossy(&head_out.stdout).trim().to_string();
            let clean_rev = built_rev.trim_end_matches("-dirty");
            if url == built_repo && !head.is_empty() && clean_rev != head {
                println!(
                    "⚠️  Installed binary is STALE: built from rev {clean_rev}, repo HEAD is {head}"
                );
                println!("    rebuild and reinstall before trusting version-specific behavior.");
                println!();
            }
        }
    }

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
