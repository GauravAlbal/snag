use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Github-style remote normalization: keeps the `owner/name` pair for SSH and
/// HTTPS github.com forms so both spellings collapse to one alias token.
pub fn normalize_remote_alias(raw: &str) -> String {
    let trimmed = raw.trim();
    // git@github.com:owner/repo.git
    if let Some(rest) = trimmed.strip_prefix("git@") {
        if let Some(slash) = rest.find(':') {
            let host = &rest[..slash];
            let path = &rest[slash + 1..];
            return host_path_to_alias(host, path);
        }
    }
    // https://github.com/owner/repo.git , https://git@github.com/owner/repo
    if let Some(rest) = trimmed.strip_prefix("https://") {
        let rest = rest.strip_prefix("git@").unwrap_or(rest);
        if let Some(slash) = rest.find('/') {
            let host = &rest[..slash];
            let path = &rest[slash + 1..];
            return host_path_to_alias(host, path);
        }
    }
    trimmed.to_string()
}

fn host_path_to_alias(host: &str, path: &str) -> String {
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/');
    // github.com + github.com: owner/name -> owner/name regardless of host casing
    let host = host.to_ascii_lowercase();
    if host == "github.com" {
        path.to_string()
    } else {
        format!("{}/{}", host, path)
    }
}

#[derive(Debug, Default)]
pub struct GitContext {
    pub repository_root: Option<String>,
    pub git_common_dir: Option<String>,
    pub git_head: Option<String>,
    pub git_branch: Option<String>,
    pub git_remote_aliases: Vec<String>,
    pub relative_cwd: Option<String>,
    pub warnings: Vec<String>,
}

/// Run one git command with a hard global deadline. The child is explicitly
/// spawned, polled with `try_wait`, and KILLED + reaped when the deadline
/// passes, so no child process or thread is leaked. On timeout or spawn
/// failure a warning is recorded and `None` is returned (optional context must
/// never lose a report).
fn run_git(
    args: &[&str],
    cwd: &Path,
    deadline: Instant,
    warnings: &mut Vec<String>,
) -> Option<std::process::Output> {
    let mut child = match Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            warnings.push(format!("could not spawn git {:?}: {}", args, e));
            return None;
        }
    };

    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    // Kill the child process and reap it so nothing leaks.
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                warnings.push(format!("git {:?} errored: {}", args, e));
                return None;
            }
        }
    }

    if timed_out {
        warnings.push(format!("git {:?} timed out and was terminated", args));
        return None;
    }

    match child.wait_with_output() {
        Ok(output) => Some(output),
        Err(e) => {
            warnings.push(format!("git {:?} failed to collect output: {}", args, e));
            None
        }
    }
}

fn stdout_str(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Collect git context under a single bounded overall budget. The default is
/// 1200ms for the whole batch; passing an explicit deadline makes the budget
/// shareable across callers.
pub fn collect_git_context(cwd: &Path) -> Result<GitContext> {
    collect_git_context_with_budget(cwd, Duration::from_millis(1200))
}

pub fn collect_git_context_with_budget(cwd: &Path, budget: Duration) -> Result<GitContext> {
    let deadline = Instant::now() + budget;
    let mut ctx = GitContext::default();

    let inside = run_git(&["rev-parse", "--is-inside-work-tree"], cwd, deadline, &mut ctx.warnings);
    let is_inside = inside
        .filter(|o| o.status.success() && stdout_str(o) == "true")
        .is_some();
    if !is_inside {
        return Ok(ctx);
    }

    if let Some(out) = run_git(&["rev-parse", "--show-toplevel"], cwd, deadline, &mut ctx.warnings) {
        if out.status.success() {
            ctx.repository_root = Some(stdout_str(&out));
        }
    }

    // G26: use the real common dir so linked worktrees resolve to one logical
    // repository, and canonicalize to an absolute path.
    if let Some(out) = run_git(&["rev-parse", "--git-common-dir"], cwd, deadline, &mut ctx.warnings) {
        if out.status.success() {
            let raw = stdout_str(&out);
            let path = PathBuf::from(&raw);
            let abs = if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            };
            if let Ok(canon) = abs.canonicalize() {
                ctx.git_common_dir = Some(canon.to_string_lossy().into_owned());
            } else {
                ctx.git_common_dir = Some(abs.to_string_lossy().into_owned());
            }
        }
    }

    if let Some(out) = run_git(&["rev-parse", "HEAD"], cwd, deadline, &mut ctx.warnings) {
        if out.status.success() {
            ctx.git_head = Some(stdout_str(&out));
        }
    }

    if let Some(out) = run_git(&["symbolic-ref", "--short", "HEAD"], cwd, deadline, &mut ctx.warnings) {
        if out.status.success() {
            ctx.git_branch = Some(stdout_str(&out));
        }
    }

    if let Some(out) = run_git(&["remote", "-v"], cwd, deadline, &mut ctx.warnings) {
        if out.status.success() {
            let mut remotes = Vec::new();
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 && parts.get(2) == Some(&"(fetch)") {
                    remotes.push(normalize_remote_alias(parts[1]));
                }
            }
            remotes.sort();
            remotes.dedup();
            ctx.git_remote_aliases = remotes;
        }
    }

    if let Some(out) = run_git(&["rev-parse", "--show-prefix"], cwd, deadline, &mut ctx.warnings) {
        if out.status.success() {
            let prefix = stdout_str(&out);
            if !prefix.is_empty() {
                ctx.relative_cwd = Some(prefix);
            }
        }
    }

    Ok(ctx)
}
