//! Rebuild reporter-role preservation (Pearl 1 of the summary intent)).
//!
//! The attribution projection (`observation_repositories.role='reporter'`) is
//! written at filing from the resolved repository identity. `snag rebuild`
//! must reconstruct it from the canonical payload's
//! `context.repository.repository_id`; inserting only `affected_repository_ids`
//! silently destroys owner attribution on every rebuild.
//!
//! This test FAILS on the pre-fix baseline (a regression test that cannot fail
//! on the old behavior is not evidence).

use assert_cmd::Command;
use rusqlite::Connection;
use std::path::PathBuf;

struct TestContext {
    home_dir: tempfile::TempDir,
    data_dir: PathBuf,
}

impl TestContext {
    fn new() -> Self {
        let home_dir = tempfile::tempdir().unwrap();
        let data_dir = home_dir.path().join("snag");
        Self { home_dir, data_dir }
    }
    fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("snag").unwrap();
        c.env("XDG_DATA_HOME", self.home_dir.path())
            .env("HOME", self.home_dir.path());
        c
    }
    fn conn(&self) -> Connection {
        Connection::open(self.data_dir.join("snag.sqlite")).unwrap()
    }
}

/// File an observation pinned to a repository (the filing reporter).
fn report_in(ctx: &TestContext, title: &str, repo_id: &str) -> String {
    ctx.cmd()
        .arg("report")
        .arg(title)
        .arg("--kind")
        .arg("bug")
        .arg("--severity")
        .arg("major")
        .arg("--repo-id")
        .arg(repo_id)
        .arg("--unowned")
        .assert()
        .success();
    ctx.conn()
        .query_row(
            "SELECT observation_id FROM observations WHERE title = ?1 ORDER BY local_sequence DESC LIMIT 1",
            [title],
            |r| r.get(0),
        )
        .unwrap()
}

/// Snapshot the reporter-role projection of a store: (observation, repository)
/// pairs with role='reporter'.
fn reporter_rows(db: &std::path::Path) -> Vec<(String, String)> {
    let conn = Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT observation_id, repository_id FROM observation_repositories WHERE role='reporter' ORDER BY observation_id",
        )
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    let mut v = Vec::new();
    while let Some(r) = rows.next().unwrap() {
        v.push((r.get(0).unwrap(), r.get(1).unwrap()));
    }
    v
}

#[test]
fn t1_rebuild_preserves_primary_role() {
    let ctx = TestContext::new();
    report_in(&ctx, "primary-a", "repo_alpha");
    report_in(&ctx, "primary-b", "repo_beta");

    // Snapshot the live projection before the round trip.
    let live = reporter_rows(&ctx.data_dir.join("snag.sqlite"));
    assert_eq!(
        live.len(),
        2,
        "both obs must carry a reporter role pre-rebuild"
    );

    let export_path = ctx.home_dir.path().join("export.jsonl");
    ctx.cmd()
        .arg("export")
        .arg("--output")
        .arg(&export_path)
        .assert()
        .success();

    let rebuilt = ctx.home_dir.path().join("rebuilt");
    let dest = rebuilt.join("snag");
    ctx.cmd()
        .arg("rebuild")
        .arg("--from-export")
        .arg(&export_path)
        .arg("--destination")
        .arg(&dest)
        .assert()
        .success();

    let after = reporter_rows(&dest.join("snag.sqlite"));
    assert_eq!(
        live, after,
        "reporter role must survive rebuild: live={live:?} rebuilt={after:?}"
    );
}

/// Snapshot the owner-role projection: (observation, repository) pairs.
fn owner_rows(db: &std::path::Path) -> Vec<(String, String)> {
    let conn = Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT observation_id, repository_id FROM observation_repositories WHERE role='owner' ORDER BY observation_id",
        )
        .unwrap();
    let mut rows = stmt.query([]).unwrap();
    let mut v = Vec::new();
    while let Some(r) = rows.next().unwrap() {
        v.push((r.get(0).unwrap(), r.get(1).unwrap()));
    }
    v
}

#[test]
fn t3_rebuild_preserves_owner_role() {
    let ctx = TestContext::new();
    // File from repo_alpha, owning the fix to repo_beta: reporter + owner
    // are distinct actors and must both survive rebuild.
    ctx.cmd()
        .arg("report")
        .arg("owner-obs")
        .arg("--kind")
        .arg("bug")
        .arg("--severity")
        .arg("major")
        .arg("--repo-id")
        .arg("repo_alpha")
        .arg("--owner")
        .arg("repo_beta")
        .assert()
        .success();

    let live_reporter = reporter_rows(&ctx.data_dir.join("snag.sqlite"));
    let live_owner = owner_rows(&ctx.data_dir.join("snag.sqlite"));
    assert_eq!(live_owner.len(), 1, "owner link recorded at filing");
    assert_eq!(live_owner[0].1, "repo_beta");

    let export_path = ctx.home_dir.path().join("export.jsonl");
    ctx.cmd()
        .arg("export")
        .arg("--output")
        .arg(&export_path)
        .assert()
        .success();

    let rebuilt = ctx.home_dir.path().join("rebuilt3");
    let dest = rebuilt.join("snag");
    ctx.cmd()
        .arg("rebuild")
        .arg("--from-export")
        .arg(&export_path)
        .arg("--destination")
        .arg(&dest)
        .assert()
        .success();

    assert_eq!(
        live_reporter,
        reporter_rows(&dest.join("snag.sqlite")),
        "reporter role survives rebuild with owner set"
    );
    assert_eq!(
        live_owner,
        owner_rows(&dest.join("snag.sqlite")),
        "owner role survives rebuild: {live_owner:?}"
    );
}

#[test]
fn t2_rebuild_unowned_obs_stays_unowned() {
    // An obs filed with no repository identity must not gain a phantom
    // primary after rebuild (no context.repository.repository_id in payload).
    // Run the report from a non-git cwd so git context cannot auto-resolve a
    // primary from the surrounding repository.
    let ctx = TestContext::new();
    let outside = tempfile::tempdir().unwrap();
    ctx.cmd()
        .current_dir(outside.path())
        .arg("report")
        .arg("no-repo obs")
        .arg("--kind")
        .arg("bug")
        .arg("--severity")
        .arg("minor")
        .arg("--unowned")
        .assert()
        .success();

    let export_path = ctx.home_dir.path().join("export.jsonl");
    ctx.cmd()
        .arg("export")
        .arg("--output")
        .arg(&export_path)
        .assert()
        .success();

    let rebuilt = ctx.home_dir.path().join("rebuilt2");
    let dest = rebuilt.join("snag");
    ctx.cmd()
        .arg("rebuild")
        .arg("--from-export")
        .arg(&export_path)
        .arg("--destination")
        .arg(&dest)
        .assert()
        .success();

    let after = reporter_rows(&dest.join("snag.sqlite"));
    assert!(
        after.is_empty(),
        "unowned obs must stay unowned through rebuild, got {after:?}"
    );
}
