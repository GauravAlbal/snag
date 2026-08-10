//! Build-provenance capture: embeds the source revision, build date, and
//! (for internal lanes) the build flavor into the binary via rustc-env.
//!
//! The embedded revision is what makes a stale installed binary detectable:
//! `snag --version` shows it, and `snag doctor` compares it against the repo
//! HEAD when run from a matching checkout (dogfood finding: a fix can sit
//! uncommitted in the tree while the installed binary still runs old code).
//!
//! Falls back to `unknown` outside a git checkout (release tarballs,
//! vendored builds) — provenance is best-effort, never a build blocker.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn path_from_git_output(path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .or_else(|| env::current_dir().ok())
        .map_or_else(|| path.to_path_buf(), |root| root.join(path))
}

fn git_directory(flag: &str) -> Option<PathBuf> {
    git(&["rev-parse", flag]).map(|path| path_from_git_output(&path))
}

/// Return the git files whose changes can alter the revision embedded by this
/// build. `HEAD` lives in the per-worktree git directory, while refs and
/// packed-refs live in the common directory shared by linked worktrees.
fn git_watch_paths(git_dir: &Path, common_dir: &Path, active_ref: Option<&str>) -> Vec<PathBuf> {
    let mut paths = vec![git_dir.join("HEAD")];
    if let Some(active_ref) = active_ref.filter(|name| name.starts_with("refs/")) {
        paths.push(common_dir.join(active_ref));
    }
    paths.push(common_dir.join("packed-refs"));
    paths
}

/// Days-from-civil conversion (Howard Hinnant's algorithm), used to render
/// the build date without pulling a date crate into the build script.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The flavor is an env input: without tracking it, a later build without
    // SNAG_BUILD_FLAVOR would silently reuse the cached flavor (and vice
    // versa), shipping an unmarked internal binary.
    println!("cargo:rerun-if-env-changed=SNAG_BUILD_FLAVOR");
    if let Some(git_dir) = git_directory("--git-dir") {
        let common_dir = git_directory("--git-common-dir").unwrap_or_else(|| git_dir.clone());
        let active_ref = git(&["symbolic-ref", "--quiet", "HEAD"]);
        for path in git_watch_paths(&git_dir, &common_dir, active_ref.as_deref()) {
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
    // Re-derive the dirty marker whenever any tracked file changes.
    if let Some(files) = git(&["ls-files"]) {
        for f in files.lines() {
            println!("cargo:rerun-if-changed={f}");
        }
    }

    let rev = git(&["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let rev = if dirty { format!("{rev}-dirty") } else { rev };
    println!("cargo:rustc-env=SNAG_BUILD_REV={rev}");

    // The origin URL lets `snag doctor` decide whether the repo it is run
    // from is the same source as the binary (a meaningful rev comparison).
    let url = git(&["config", "--get", "remote.origin.url"]).unwrap_or_default();
    println!("cargo:rustc-env=SNAG_BUILD_REPO_URL={url}");

    // Internal lanes set SNAG_BUILD_FLAVOR (e.g. "internal") at build time so
    // the public release binary never carries the marker.
    println!("cargo:rustc-check-cfg=cfg(snag_internal)");
    let flavor = env::var("SNAG_BUILD_FLAVOR").unwrap_or_default();
    let flavor_suffix = if flavor.is_empty() {
        String::new()
    } else {
        format!(", {flavor}")
    };
    println!("cargo:rustc-env=SNAG_BUILD_FLAVOR={flavor}");
    println!("cargo:rustc-env=SNAG_BUILD_FLAVOR_SUFFIX={flavor_suffix}");

    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, m, d) = civil_from_days(secs / 86400);
    println!("cargo:rustc-env=SNAG_BUILD_DATE={y:04}-{m:02}-{d:02}");
}

#[cfg(test)]
mod tests {
    use super::git_watch_paths;
    use std::path::Path;

    #[test]
    fn git_watch_paths_track_worktree_head_common_ref_and_packed_refs() {
        let paths = git_watch_paths(
            Path::new("/checkout/.git/worktrees/feature"),
            Path::new("/checkout/.git"),
            Some("refs/heads/feature"),
        );

        assert_eq!(
            paths,
            vec![
                Path::new("/checkout/.git/worktrees/feature/HEAD").to_path_buf(),
                Path::new("/checkout/.git/refs/heads/feature").to_path_buf(),
                Path::new("/checkout/.git/packed-refs").to_path_buf(),
            ]
        );
    }
}
