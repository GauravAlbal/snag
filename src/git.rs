use anyhow::{Context, Result};
use std::process::Command;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct GitContext {
    pub repository_root: Option<String>,
    pub git_common_dir: Option<String>,
    pub git_head: Option<String>,
    pub git_branch: Option<String>,
    pub git_remote_aliases: Vec<String>,
    pub relative_cwd: Option<String>,
}

pub fn collect_git_context(cwd: &Path) -> Result<GitContext> {
    let mut ctx = GitContext::default();
    
    // Check if inside work tree
    let output = Command::new("git")
        .args(&["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()?;
        
    if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim() != "true" {
        return Ok(ctx);
    }
    
    // Repository root
    if let Ok(out) = Command::new("git").args(&["rev-parse", "--show-toplevel"]).current_dir(cwd).output() {
        if out.status.success() {
            ctx.repository_root = Some(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
    }
    
    // Git common dir
    if let Ok(out) = Command::new("git").args(&["rev-parse", "--absolute-git-dir"]).current_dir(cwd).output() {
        if out.status.success() {
            ctx.git_common_dir = Some(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
    }
    
    // Git HEAD
    if let Ok(out) = Command::new("git").args(&["rev-parse", "HEAD"]).current_dir(cwd).output() {
        if out.status.success() {
            ctx.git_head = Some(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
    }
    
    // Git branch (symbolic ref)
    if let Ok(out) = Command::new("git").args(&["symbolic-ref", "--short", "HEAD"]).current_dir(cwd).output() {
        if out.status.success() {
            ctx.git_branch = Some(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
    }
    
    // Git remotes
    if let Ok(out) = Command::new("git").args(&["remote", "-v"]).current_dir(cwd).output() {
        if out.status.success() {
            let output_str = String::from_utf8_lossy(&out.stdout);
            let mut remotes = Vec::new();
            for line in output_str.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 && parts[2] == "(fetch)" {
                    remotes.push(parts[1].to_string());
                }
            }
            remotes.sort();
            remotes.dedup();
            ctx.git_remote_aliases = remotes;
        }
    }
    
    // Relative CWD
    if let Ok(out) = Command::new("git").args(&["rev-parse", "--show-prefix"]).current_dir(cwd).output() {
        if out.status.success() {
            let prefix = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !prefix.is_empty() {
                ctx.relative_cwd = Some(prefix);
            }
        }
    }

    Ok(ctx)
}
