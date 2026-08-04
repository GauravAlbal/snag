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

fn run_git_with_timeout(args: &[&str], cwd: &Path) -> Result<std::process::Output> {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let cwd = cwd.to_path_buf();
    
    let (tx, rx) = mpsc::channel();
    
    let args_clone = args.clone();
    let _handle = thread::spawn(move || {
        let result = Command::new("git")
            .args(&args)
            .current_dir(&cwd)
            .output();
        let _ = tx.send(result);
    });
    
    match rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(e.into()),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            anyhow::bail!("Git command timed out: git {:?}", args_clone);
        },
        Err(_) => anyhow::bail!("Git thread panicked"),
    }
}

pub fn collect_git_context(cwd: &Path) -> Result<GitContext> {
    let mut ctx = GitContext::default();
    
    // Check if inside work tree
    let output = run_git_with_timeout(&["rev-parse", "--is-inside-work-tree"], cwd)?;
        
    if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim() != "true" {
        return Ok(ctx);
    }
    
    // Repository root
    if let Ok(out) = run_git_with_timeout(&["rev-parse", "--show-toplevel"], cwd) {
        if out.status.success() {
            ctx.repository_root = Some(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
    }
    
    // Git common dir
    if let Ok(out) = run_git_with_timeout(&["rev-parse", "--absolute-git-dir"], cwd) {
        if out.status.success() {
            ctx.git_common_dir = Some(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
    }
    
    // Git HEAD
    if let Ok(out) = run_git_with_timeout(&["rev-parse", "HEAD"], cwd) {
        if out.status.success() {
            ctx.git_head = Some(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
    }
    
    // Git branch (symbolic ref)
    if let Ok(out) = run_git_with_timeout(&["symbolic-ref", "--short", "HEAD"], cwd) {
        if out.status.success() {
            ctx.git_branch = Some(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
    }
    
    // Git remotes
    if let Ok(out) = run_git_with_timeout(&["remote", "-v"], cwd) {
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
    if let Ok(out) = run_git_with_timeout(&["rev-parse", "--show-prefix"], cwd) {
        if out.status.success() {
            let prefix = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !prefix.is_empty() {
                ctx.relative_cwd = Some(prefix);
            }
        }
    }

    Ok(ctx)
}
