use crate::cli::DoctorArgs;
use crate::store::Store;
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn handle(_args: DoctorArgs) -> Result<()> {
    println!("snag {} (doctor)", crate::cli::BUILD_VERSION);
    println!();
    warn_if_stale_binary();
    print_context_source();

    let mut failures = Vec::new();
    let paths = report_store_paths(&mut failures);
    check_store_access(paths, &mut failures);
    check_git_context();

    println!("\nDiagnostics complete.");
    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("doctor found failed checks: {}", failures.join("; "))
    }
}

fn warn_if_stale_binary() {
    // Stale-binary guard: when the embedded source repository matches the
    // repo doctor is run from, compare the embedded revision against HEAD and
    // warn on drift. Dogfood findings: (a) a fix can sit committed in the tree
    // while the installed binary still runs older code (rev mismatch); (b) a
    // fix can sit UNcommitted in the tree with the installed binary built
    // from a dirty workspace (the `-dirty` marker on the built rev).
    let built_rev = env!("SNAG_BUILD_REV");
    let built_repo = env!("SNAG_BUILD_REPO_URL");
    if built_repo.is_empty() || built_rev.starts_with("unknown") {
        return;
    }
    let here = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .output();
    let head = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output();
    let (Ok(url_out), Ok(head_out)) = (here, head) else {
        return;
    };
    let url = String::from_utf8_lossy(&url_out.stdout).trim().to_string();
    let head = String::from_utf8_lossy(&head_out.stdout).trim().to_string();
    let clean_rev = built_rev.trim_end_matches("-dirty");
    if url == built_repo && !head.is_empty() && clean_rev != head {
        println!("⚠️  Installed binary is STALE: built from rev {clean_rev}, repo HEAD is {head}");
        println!("    rebuild and reinstall before trusting version-specific behavior.");
        println!();
    }
}

fn print_context_source() {
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
}

fn report_store_paths(failures: &mut Vec<String>) -> Option<(PathBuf, PathBuf)> {
    // Store paths. Reported even when no store exists yet, so users never have
    // to guess where data would live.
    match Store::paths() {
        Ok((data_dir, db_path)) => {
            let objects_dir = data_dir.join("objects").join("blake3");
            let backups_dir = data_dir.join("backups");
            println!("Database:      {}", db_path.display());
            println!("Objects:       {}", objects_dir.display());
            println!("Backups:       {}", backups_dir.display());
            println!();
            Some((db_path, backups_dir))
        }
        Err(e) => {
            println!("Database:      unavailable");
            println!("Objects:       unavailable");
            println!("Backups:       unavailable");
            println!();
            println!("❌ Store paths: FAILED ({e})");
            failures.push(format!("store paths: {e}"));
            None
        }
    }
}

fn check_store_access(paths: Option<(PathBuf, PathBuf)>, failures: &mut Vec<String>) {
    // A missing store is a first-run warning, not a failed check: `snag doctor`
    // is the installer self-test before any report.
    let Some((db_path, backups_dir)) = paths else {
        return;
    };
    if !db_path.exists() {
        println!("⚠️  Store access: no store yet (run `snag report` to initialize)");
        return;
    }
    match Store::open_read_only() {
        Ok(store) => {
            println!("✅ Store access: OK");
            check_backups(&backups_dir, failures);
            check_integrity(&store, failures);
        }
        Err(e) => {
            println!("❌ Store access: FAILED ({e})");
            failures.push(format!("store access: {e}"));
        }
    }
}

fn check_backups(backups_dir: &Path, failures: &mut Vec<String>) {
    if !backups_dir.exists() {
        println!("⚠️  Backups directory: Missing (run `snag backup` to initialize)");
        return;
    }
    match fs::read_dir(backups_dir) {
        Ok(entries) => {
            println!(
                "✅ Backups directory: OK ({} backups found)",
                entries.count()
            );
        }
        Err(e) => {
            println!("❌ Backups directory: FAILED ({e})");
            failures.push(format!("backups directory: {e}"));
        }
    }
}

fn check_integrity(store: &Store, failures: &mut Vec<String>) {
    match store
        .conn
        .query_row::<String, _, _>("PRAGMA integrity_check", [], |row| row.get(0))
    {
        Ok(res) if res == "ok" => println!("✅ SQLite integrity: OK"),
        Ok(res) => {
            println!("❌ SQLite integrity: FAILED ({res})");
            failures.push(format!("SQLite integrity: {res}"));
        }
        Err(e) => {
            println!("❌ SQLite integrity: ERROR ({e})");
            failures.push(format!("SQLite integrity: {e}"));
        }
    }
}

fn check_git_context() {
    match std::env::current_dir() {
        Ok(cwd) => match crate::git::collect_git_context(&cwd) {
            Ok(ctx) => {
                if ctx.repository_root.is_some() {
                    println!("✅ Git context collection: OK (in repository)");
                } else {
                    println!("✅ Git context collection: OK (not in a repository)");
                }
            }
            Err(e) => {
                println!("⚠️  Git context collection: FAILED ({e})");
            }
        },
        Err(e) => {
            println!("⚠️  Git context collection: FAILED ({e})");
        }
    }
}
