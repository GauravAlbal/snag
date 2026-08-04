use crate::cli::RestoreArgs;
use anyhow::{Context, Result};
use std::fs;

pub fn handle(args: RestoreArgs) -> Result<()> {
    let backup_dir = args.backup_dir;
    let backup_db = backup_dir.join("snag.sqlite");
    
    if !backup_db.exists() {
        anyhow::bail!("Backup database not found: {:?}", backup_db);
    }
    
    let proj_dirs = directories::ProjectDirs::from("", "", "snag")
        .context("Could not determine project directories")?;
    let data_dir = proj_dirs.data_dir();
    fs::create_dir_all(data_dir)?;
    
    let target_db = data_dir.join("snag.sqlite");
    fs::copy(&backup_db, &target_db)?;
    
    println!("Database restored from backup: {:?}", backup_dir);
    
    Ok(())
}
