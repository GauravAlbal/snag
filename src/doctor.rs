use crate::cli::DoctorArgs;
use crate::store::Store;
use anyhow::Result;
use std::fs;

pub fn handle(_args: DoctorArgs) -> Result<()> {
    println!("Running snag doctor...\n");
    
    // Store access
    match Store::open() {
        Ok(store) => {
            println!("✅ Store access: OK ({})", store.data_dir.display());
            
            // Check backups
            let backups_dir = store.data_dir.join("backups");
            if backups_dir.exists() {
                let count = fs::read_dir(&backups_dir)?.count();
                println!("✅ Backups directory: OK ({} backups found)", count);
            } else {
                println!("⚠️  Backups directory: Missing (run `snag backup` to initialize)");
            }
            
            // SQLite integrity check
            match store.conn.query_row::<String, _, _>("PRAGMA integrity_check", [], |row| row.get(0)) {
                Ok(res) if res == "ok" => println!("✅ SQLite integrity: OK"),
                Ok(res) => println!("❌ SQLite integrity: FAILED ({})", res),
                Err(e) => println!("❌ SQLite integrity: ERROR ({})", e),
            }
        },
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
            },
            Err(e) => {
                println!("⚠️  Git context collection: FAILED ({})", e);
            }
        }
    }
    
    println!("\nDiagnostics complete.");
    Ok(())
}
