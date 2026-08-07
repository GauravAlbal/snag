//! `snag review summary` — per-repo open-observation materiality (Pearl 2 of
//! the summary intent).
//!
//! Owner lane = observation_repositories.role='primary'. Threshold exit code:
//! exit 1 when ANY evaluated lane has >= count open ACTIONABLE obs at the
//! given severity (actionable = open AND state NOT IN candidate_fix /
//! remediation_in_progress); `--repo` narrows the evaluated set. Unowned obs
//! (no primary row) form their own bucket and participate under the same
//! rules. Rebuild-primary preservation is covered by tests/rebuild_primary_test.rs.

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

/// File an observation pinned to a repository (the primary/owner lane).
fn report_in(ctx: &TestContext, title: &str, repo_id: &str, severity: &str) -> String {
    ctx.cmd()
        .arg("report")
        .arg(title)
        .arg("--kind")
        .arg("bug")
        .arg("--severity")
        .arg(severity)
        .arg("--repo-id")
        .arg(repo_id)
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

/// File an unowned observation (non-git cwd so no primary auto-resolves).
fn report_unowned(ctx: &TestContext, title: &str, severity: &str) {
    let outside = tempfile::tempdir().unwrap();
    ctx.cmd()
        .current_dir(outside.path())
        .arg("report")
        .arg(title)
        .arg("--kind")
        .arg("bug")
        .arg("--severity")
        .arg(severity)
        .assert()
        .success();
}

fn summary_cmd(ctx: &TestContext) -> Command {
    let mut c = ctx.cmd();
    c.arg("review").arg("summary");
    c
}

// ---------------------------------------------------------------------------
// t1: text table groups by primary repo, ranked by materiality desc, with an
// (unowned) row when unowned open obs exist.
// ---------------------------------------------------------------------------

#[test]
fn t1_summary_groups_by_primary_ranked_by_materiality() {
    let ctx = TestContext::new();
    // repo_alpha: 2 majors + 1 minor (materiality 3+3+1 = 7)
    report_in(&ctx, "a1", "repo_alpha", "major");
    report_in(&ctx, "a2", "repo_alpha", "major");
    report_in(&ctx, "a3", "repo_alpha", "minor");
    // repo_beta: 1 major (materiality 3)
    report_in(&ctx, "b1", "repo_beta", "major");
    // unowned: 1 medium
    report_unowned(&ctx, "u1", "medium");

    let out = summary_cmd(&ctx).output().unwrap();
    assert!(
        out.status.success(),
        "summary must exit 0 without thresholds"
    );
    let text = String::from_utf8(out.stdout).unwrap();

    // Both repo lanes present; the higher-materiality lane (alpha: 2 majors +
    // 1 minor = 7.0) must rank above beta (1 major = 3.0). Lane display names
    // resolve to aliases (the test runs inside the snag checkout, so aliases
    // inherit its remotes) — assert via materiality ordering by locating the
    // rows' data rather than the id.
    let alpha_line = text
        .lines()
        .find(|l| l.contains("7.0"))
        .expect("alpha lane (materiality 7.0) row present");
    let beta_line = text
        .lines()
        .find(|l| l.contains("3.0") && !l.contains("7.0"))
        .expect("beta lane (materiality 3.0) row present");
    let alpha_pos = text.find(alpha_line).unwrap();
    let beta_pos = text.find(beta_line).unwrap();
    assert!(
        alpha_pos < beta_pos,
        "higher materiality ranks first: {text}"
    );

    // Unowned bucket is a first-class row.
    assert!(
        text.contains("(unowned)"),
        "unowned row must appear: {text}"
    );

    // Severity columns visible (major count 2 for alpha).
    assert!(
        alpha_line.contains("2"),
        "alpha major count visible: {alpha_line}"
    );
}

// ---------------------------------------------------------------------------
// t2: threshold exit code — any lane crossing trips 1; --repo narrows.
// ---------------------------------------------------------------------------

#[test]
fn t2_threshold_exit_code() {
    let ctx = TestContext::new();
    report_in(&ctx, "a1", "repo_alpha", "major");
    report_in(&ctx, "a2", "repo_alpha", "major");
    // repo_beta has a major too but should not flip a --repo alpha check.
    report_in(&ctx, "b1", "repo_beta", "major");

    // Any lane with >=2 majors -> exit 1.
    let out = summary_cmd(&ctx)
        .arg("--at-least")
        .arg("major=2")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "any lane crossing trips 1");

    // Threshold above every lane -> exit 0.
    let out = summary_cmd(&ctx)
        .arg("--at-least")
        .arg("major=3")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "above every lane -> 0");

    // --repo alpha still crosses (alpha has 2); --repo beta does not.
    let out = summary_cmd(&ctx)
        .arg("--repo")
        .arg("repo_alpha")
        .arg("--at-least")
        .arg("major=2")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "alpha lane crosses");
    let out = summary_cmd(&ctx)
        .arg("--repo")
        .arg("repo_beta")
        .arg("--at-least")
        .arg("major=2")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "--repo beta narrows the evaluated set"
    );
}

// ---------------------------------------------------------------------------
// t3: in-flight obs (candidate_fix / remediation_in_progress) are excluded
// from threshold counts but still shown in open.
// ---------------------------------------------------------------------------

#[test]
fn t3_inflight_excluded_from_threshold() {
    let ctx = TestContext::new();
    let a1 = report_in(&ctx, "a1", "repo_alpha", "major");
    report_in(&ctx, "a2", "repo_alpha", "major");

    // Put a1 in candidate_fix (confirmed disposition + attach a commit): it
    // becomes in-flight.
    ctx.cmd()
        .arg("review")
        .arg("disposition")
        .arg(&a1)
        .arg("confirmed")
        .assert()
        .success();
    ctx.cmd()
        .arg("review")
        .arg("attach-fix")
        .arg(&a1)
        .arg("--commit")
        .arg("sha1")
        .arg("--repo")
        .arg("r1")
        .assert()
        .success();

    // Only 1 actionable major remains in alpha -> major=2 no longer trips.
    summary_cmd(&ctx)
        .arg("--at-least")
        .arg("major=2")
        .assert()
        .code(0);

    // But the open column still counts 2 (in-flight is open, not handled).
    // Assert via JSON to avoid alias-name dependence.
    let out = summary_cmd(&ctx)
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let alpha = v["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["repo_id"] == "repo_alpha")
        .expect("alpha lane present");
    assert_eq!(alpha["open"], 2, "in-flight obs still counts in open");
    assert_eq!(
        alpha["severity_counts"]["major"], 2,
        "severity mix shows both majors"
    );
}

// ---------------------------------------------------------------------------
// t4: --format json emits the review_summary_v1 envelope; exit_code matches.
// ---------------------------------------------------------------------------

#[test]
fn t4_json_envelope() {
    let ctx = TestContext::new();
    report_in(&ctx, "a1", "repo_alpha", "major");
    report_in(&ctx, "a2", "repo_alpha", "major");

    let out = summary_cmd(&ctx)
        .arg("--format")
        .arg("json")
        .arg("--at-least")
        .arg("major=2")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "crossed threshold -> exit 1");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema"], "review_summary_v1");
    assert_eq!(v["exit_code"], 1);
    let repos = v["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["repo_id"], "repo_alpha");
    assert_eq!(repos[0]["severity_counts"]["major"], 2);
    assert_eq!(repos[0]["crossed"], true);
    assert!(v["unowned"].is_null() || v["unowned"]["open"].as_i64().unwrap_or(0) == 0);

    // Below threshold: exit 0 and envelope says so.
    let out2 = summary_cmd(&ctx)
        .arg("--format")
        .arg("json")
        .arg("--at-least")
        .arg("major=5")
        .output()
        .unwrap();
    assert_eq!(out2.status.code(), Some(0));
    let v2: serde_json::Value = serde_json::from_slice(&out2.stdout).unwrap();
    assert_eq!(v2["exit_code"], 0);
    assert_eq!(v2["repos"][0]["crossed"], false);
}

// ---------------------------------------------------------------------------
// t5: unowned obs participate in thresholds under the same rules.
// ---------------------------------------------------------------------------

#[test]
fn t5_unowned_participates_in_threshold() {
    let ctx = TestContext::new();
    report_in(&ctx, "a1", "repo_alpha", "minor");
    // One unowned major.
    report_unowned(&ctx, "u1", "major");

    // The unowned bucket has 1 major -> major=1 trips exit 1 even though no
    // repo lane has any major.
    let out = summary_cmd(&ctx)
        .arg("--at-least")
        .arg("major=1")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "unowned major must trip the threshold"
    );
}

// ---------------------------------------------------------------------------
// t6: severity vocabulary validation on --at-least.
// ---------------------------------------------------------------------------

#[test]
fn t6_invalid_threshold_rejected() {
    let ctx = TestContext::new();
    report_in(&ctx, "a1", "repo_alpha", "major");
    let out = summary_cmd(&ctx)
        .arg("--at-least")
        .arg("catastrophic=1")
        .output()
        .unwrap();
    assert!(!out.status.success(), "unknown severity rejected");
    let out = summary_cmd(&ctx)
        .arg("--at-least")
        .arg("major")
        .output()
        .unwrap();
    assert!(!out.status.success(), "missing =count rejected");
    let out = summary_cmd(&ctx)
        .arg("--at-least")
        .arg("major=0")
        .output()
        .unwrap();
    assert!(!out.status.success(), "zero count rejected");
}
