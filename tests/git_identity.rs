use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use std::process::Command as Proc;

// Isolated per-command environment (no global env racing — T12).
struct TestContext {
    home_dir: tempfile::TempDir,
    data_dir: std::path::PathBuf,
}

impl TestContext {
    fn new() -> Self {
        let home_dir = tempfile::tempdir().unwrap();
        let data_dir = home_dir.path().join("snag");
        Self { home_dir, data_dir }
    }

    fn cmd(&self) -> Command {
        let mut cmd = Command::cargo_bin("snag").unwrap();
        cmd.env("XDG_DATA_HOME", self.home_dir.path())
            .env("HOME", self.home_dir.path());
        cmd
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Proc::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {:?} failed", args);
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t.test"]);
    git(dir, &["config", "user.name", "T"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("README.md"), "# repo\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

fn store_rows(ctx: &TestContext, sql: &str) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
    let mut stmt = conn.prepare(sql).unwrap();
    let mut rows = stmt.query([]).unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        out.push((row.get(0).unwrap(), row.get(1).unwrap()));
    }
    out
}

/// Linked worktrees of one repository resolve to ONE logical repository, ONE
/// checkout, and DISTINCT worktree IDs — proving the real git common dir (G26)
/// is used rather than per-worktree absolute git dirs.
#[test]
fn test_linked_worktrees_share_repository() {
    let ctx = TestContext::new();
    let main = ctx.home_dir.path().join("main");
    init_repo(&main);
    git(
        &main,
        &["remote", "add", "origin", "git@github.com:acme/widgets.git"],
    );
    git(&main, &["worktree", "add", "-q", "../wt1", "-b", "feature"]);
    let wt1 = ctx.home_dir.path().join("wt1");

    ctx.cmd()
        .current_dir(&main)
        .arg("report")
        .arg("in main")
        .assert()
        .success();
    ctx.cmd()
        .current_dir(&wt1)
        .arg("report")
        .arg("in wt1")
        .assert()
        .success();

    let repos = store_rows(&ctx, "SELECT repository_id, created_at FROM repositories");
    let checkouts = store_rows(&ctx, "SELECT checkout_id, repository_id FROM checkouts");
    let worktrees = store_rows(&ctx, "SELECT worktree_id, checkout_id FROM worktrees");

    assert_eq!(repos.len(), 1, "expected one repository, got {repos:?}");
    assert_eq!(
        checkouts.len(),
        1,
        "expected one checkout, got {checkouts:?}"
    );
    assert_eq!(
        worktrees.len(),
        2,
        "expected two worktrees, got {worktrees:?}"
    );
    assert_ne!(
        worktrees[0].0, worktrees[1].0,
        "worktree ids must be distinct"
    );
    assert_eq!(
        worktrees[0].1, worktrees[1].1,
        "worktrees must share one checkout"
    );
}

/// Ambiguous remote aliases must not silently pick the first candidate (G30).
#[test]
fn test_ambiguous_remote_aliases() {
    let ctx = TestContext::new();
    let a = ctx.home_dir.path().join("a");
    let b = ctx.home_dir.path().join("b");
    let c = ctx.home_dir.path().join("c");
    init_repo(&a);
    init_repo(&b);
    init_repo(&c);

    // Two DISTINCT repositories both already confirmed against the SAME
    // normalized alias (SSH form on A, HTTPS form on B).
    git(
        &a,
        &["remote", "add", "origin", "git@github.com:acme/widgets.git"],
    );
    git(
        &b,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widgets.git",
        ],
    );
    ctx.cmd()
        .current_dir(&a)
        .arg("report")
        .arg("in a")
        .arg("--repo-id")
        .arg("repoA")
        .assert()
        .success();
    ctx.cmd()
        .current_dir(&b)
        .arg("report")
        .arg("in b")
        .arg("--repo-id")
        .arg("repoB")
        .assert()
        .success();

    // A third fresh checkout with the same alias, no checkout binding and no
    // explicit identity, must be reported AMBIGUOUS rather than silently bound
    // to the first candidate.
    git(
        &c,
        &["remote", "add", "origin", "git@github.com:acme/widgets.git"],
    );
    ctx.cmd()
        .current_dir(&c)
        .arg("report")
        .arg("in c")
        .assert()
        .failure()
        .stderr(predicate::str::contains("ambiguous"));
}

/// An explicit --repo-id is honored and linked (G28).
#[test]
fn test_explicit_repo_id() {
    let ctx = TestContext::new();
    let repo = ctx.home_dir.path().join("repo");
    init_repo(&repo);

    ctx.cmd()
        .current_dir(&repo)
        .arg("report")
        .arg("explicit")
        .arg("--repo-id")
        .arg("repo_corp_backend")
        .assert()
        .success();

    let repos = store_rows(&ctx, "SELECT repository_id, created_at FROM repositories");
    assert!(
        repos.iter().any(|(id, _)| id == "repo_corp_backend"),
        "explicit repository id not present: {repos:?}"
    );
}

/// `--affected-repo` resolves by ID and persists the relationship with a role
/// (G29).
#[test]
fn test_affected_repo_by_id() {
    let ctx = TestContext::new();
    let main = ctx.home_dir.path().join("main");
    init_repo(&main);
    let other = ctx.home_dir.path().join("other");
    init_repo(&other);
    ctx.cmd()
        .current_dir(&other)
        .arg("report")
        .arg("bind other")
        .assert()
        .success();

    let ids = store_rows(
        &ctx,
        "SELECT repository_id, created_at FROM repositories ORDER BY created_at DESC",
    );
    let other_id = ids[0].0.clone();

    ctx.cmd()
        .current_dir(&main)
        .arg("report")
        .arg("affects other")
        .arg("--repo-id")
        .arg("repo_main")
        .arg("--affected-repo")
        .arg(&other_id)
        .assert()
        .success();

    let conn = rusqlite::Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
    let rows: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT o.title, r.role FROM observation_repositories r JOIN observations o ON o.observation_id = r.observation_id WHERE r.repository_id = ?1",
        ).unwrap();
        let it = stmt
            .query_map(rusqlite::params![&other_id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        it.map(|r| r.unwrap()).collect()
    };
    assert!(
        rows.iter()
            .any(|(t, role)| t == "affects other" && role == "affected"),
        "expected affected role persisted, got {rows:?}"
    );
}
