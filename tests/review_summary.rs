//! `snag review summary` — per-repo open-observation materiality (Pearl 2 of
//! the summary intent).
//!
//! A repository lane exists only for an explicit fix owner (role='owner').
//! Threshold exit code: exit 1 when ANY evaluated lane has >= count open
//! ACTIONABLE obs at the given severity (actionable = open AND state NOT IN
//! candidate_fix / remediation_in_progress AND without a live claim);
//! `--repo` narrows the evaluated owner lane. Observations without an owner
//! form the unowned bucket and participate under the same rules.

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

/// File an observation assigned to a fix-owner lane.
fn report_in(ctx: &TestContext, title: &str, repo_id: &str, severity: &str) -> String {
    ctx.cmd()
        .arg("report")
        .arg(title)
        .arg("--kind")
        .arg("bug")
        .arg("--severity")
        .arg(severity)
        .arg("--owner")
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
fn report_unowned(ctx: &TestContext, title: &str, severity: &str) -> String {
    let outside = tempfile::tempdir().unwrap();
    ctx.cmd()
        .current_dir(outside.path())
        .arg("report")
        .arg(title)
        .arg("--kind")
        .arg("bug")
        .arg("--severity")
        .arg(severity)
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
    assert!(
        text.lines()
            .next()
            .is_some_and(|line| line.starts_with("OWNER")),
        "text summary names the grouping authority: {text}"
    );

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

// ---------------------------------------------------------------------------
// Capstone: rebuild -> summary grouping parity (end-to-end hermetic).
// ---------------------------------------------------------------------------

/// Snapshot the summary's per-lane open counts (repo_id -> open) from JSON.
fn summary_open_by_lane(ctx: &TestContext) -> std::collections::BTreeMap<String, i64> {
    let out = summary_cmd(ctx)
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(out.status.success(), "summary must run cleanly");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut map = std::collections::BTreeMap::new();
    for repo in v["repos"].as_array().unwrap() {
        let rid = repo["repo_id"].as_str().unwrap().to_string();
        map.insert(rid, repo["open"].as_i64().unwrap());
    }
    map
}

#[test]
fn t7_rebuild_preserves_summary_grouping() {
    let ctx = TestContext::new();
    report_in(&ctx, "c1", "repo_alpha", "major");
    report_in(&ctx, "c2", "repo_alpha", "minor");
    report_in(&ctx, "c3", "repo_beta", "medium");

    // Snapshot grouping before the round trip.
    let before = summary_open_by_lane(&ctx);
    assert_eq!(before.len(), 2, "two primary lanes pre-rebuild");
    assert_eq!(before["repo_alpha"], 2);
    assert_eq!(before["repo_beta"], 1);

    // Export -> rebuild -> verify -> summary again.
    let export_path = ctx.home_dir.path().join("export.jsonl");
    ctx.cmd()
        .arg("export")
        .arg("--output")
        .arg(&export_path)
        .assert()
        .success();
    let rebuilt = ctx.home_dir.path().join("rebuilt-cap");
    let dest = rebuilt.join("snag");
    ctx.cmd()
        .arg("rebuild")
        .arg("--from-export")
        .arg(&export_path)
        .arg("--destination")
        .arg(&dest)
        .assert()
        .success();

    // Point the summary at the rebuilt store via XDG_DATA_HOME.
    let rebuilt_ctx_cmd = || {
        let mut c = Command::cargo_bin("snag").unwrap();
        c.env("XDG_DATA_HOME", &rebuilt)
            .env("HOME", ctx.home_dir.path());
        c
    };
    let out = rebuilt_ctx_cmd()
        .arg("review")
        .arg("summary")
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(out.status.success(), "summary on rebuilt store");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut after = std::collections::BTreeMap::new();
    for repo in v["repos"].as_array().unwrap() {
        let rid = repo["repo_id"].as_str().unwrap().to_string();
        after.insert(rid, repo["open"].as_i64().unwrap());
    }

    assert_eq!(
        before, after,
        "summary grouping must survive rebuild: before={before:?} after={after:?}"
    );
}

// ---------------------------------------------------------------------------
// t8: table alignment regression — a long lane name and a full RFC3339
// timestamp must not break column alignment (the fixed-width renderer this
// replaces silently misaligned when a cell exceeded its hardcoded width).
// ---------------------------------------------------------------------------

#[test]
fn t8_long_lane_names_keep_columns_aligned() {
    let ctx = TestContext::new();
    // A lane whose display name (abbreviated opaque id) and full RFC3339
    // timestamp are both longer than the old fixed widths (12 / 20). Filed
    // from a non-git cwd so no git remotes attach aliases — the display
    // falls back to the abbreviated id, exercising that path too.
    let long_id = "repo_0123456789abcdefghijklmnopqrstuvwxyz";
    let outside = tempfile::tempdir().unwrap();
    ctx.cmd()
        .current_dir(outside.path())
        .arg("report")
        .arg("long-lane")
        .arg("--kind")
        .arg("bug")
        .arg("--severity")
        .arg("major")
        .arg("--owner")
        .arg(long_id)
        .assert()
        .success();

    let out = summary_cmd(&ctx).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let mut lines = text.lines();

    let header = lines.next().expect("header line");
    // Locate each column's start from the header as CHAR offsets (Rust pads
    // by chars; the abbreviated-id display contains the 3-byte ellipsis, so
    // byte offsets would drift — char offsets are exact). The header is pure
    // ASCII, so byte find == char index there.
    let open_at = header.find("OPEN").expect("OPEN header");
    let oldest_at = header.find("OLDEST").expect("OLDEST header");
    let mat_at = header.find("MAT").expect("MAT header");

    let mut data_rows = 0;
    for line in lines {
        if line.trim().is_empty() {
            break;
        }
        data_rows += 1;
        let chars: Vec<char> = line.chars().collect();
        assert!(
            chars.len() > mat_at,
            "row long enough for MAT column: {line}"
        );
        // The lane display is the abbreviated opaque id (no aliases were
        // recorded from a non-git cwd).
        assert!(
            line.trim_start().starts_with("repo_01"),
            "abbreviated id display: {line}"
        );
        // Numeric columns are right-aligned, so the first non-space char
        // at/after the header offset is the value; a misaligned renderer
        // would land on a space or the wrong column's content.
        let first_non_space = |from: usize| -> Option<char> {
            chars
                .get(from..)
                .and_then(|s| s.iter().copied().find(|c| *c != ' '))
        };
        // OPEN column: a digit.
        assert!(
            first_non_space(open_at).is_some_and(|c| c.is_ascii_digit()),
            "OPEN aligned at char {open_at}: {line}"
        );
        // OLDEST column: a timestamp year digit or the empty `-` placeholder.
        assert!(
            first_non_space(oldest_at).is_some_and(|c| c == '2' || c == '-'),
            "OLDEST aligned at char {oldest_at}: {line}"
        );
        // MAT column: a decimal digit.
        assert!(
            first_non_space(mat_at).is_some_and(|c| c.is_ascii_digit()),
            "MAT aligned at char {mat_at}: {line}"
        );
    }
    assert!(data_rows >= 1, "at least one data row: {text}");
}

// ---------------------------------------------------------------------------
// t9: owner attribution — only the fix owner defines a lane. A filing
// reporter without an owner belongs to the unowned bucket.
// ---------------------------------------------------------------------------

#[test]
fn t9_only_owner_defines_lane_reporter_falls_unowned() {
    let ctx = TestContext::new();
    // Filed from repo_alpha (reporter), fix owned by repo_beta.
    ctx.cmd()
        .arg("report")
        .arg("owned-by-beta")
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
    // A plain observation has a reporter but no fix owner.
    ctx.cmd()
        .arg("report")
        .arg("alpha-plain")
        .arg("--kind")
        .arg("bug")
        .arg("--severity")
        .arg("minor")
        .arg("--unowned")
        .arg("--repo-id")
        .arg("repo_alpha")
        .assert()
        .success();

    let out = summary_cmd(&ctx)
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let repos = v["repos"].as_array().unwrap();

    let beta = repos
        .iter()
        .find(|r| r["repo_id"] == "repo_beta")
        .expect("owner lane repo_beta present");
    assert_eq!(
        beta["open"], 1,
        "the owned obs groups under its fix owner: {v}"
    );
    assert_eq!(
        beta["severity_counts"]["major"], 1,
        "the major owned obs counts in beta"
    );
    assert!(
        repos.iter().all(|r| r["repo_id"] != "repo_alpha"),
        "a reporter without an owner must not become a lane: {v}"
    );

    let unowned = &v["unowned"];
    assert_eq!(
        unowned["open"], 1,
        "the reporter-only observation belongs to unowned: {v}"
    );
    assert_eq!(
        unowned["severity_counts"]["minor"], 1,
        "the reporter-only minor counts in unowned"
    );
}

// ---------------------------------------------------------------------------
// t10: distinct repository identities that share an alias remain distinct and
// receive unambiguous text labels. Alias ambiguity is valid identity state.
// ---------------------------------------------------------------------------

#[test]
fn t10_shared_alias_lanes_are_disambiguated_without_merging() {
    let ctx = TestContext::new();
    report_in(&ctx, "alpha-shared-alias", "repo_alpha", "major");
    report_in(&ctx, "beta-shared-alias", "repo_beta", "minor");

    let conn = ctx.conn();
    for repo_id in ["repo_alpha", "repo_beta"] {
        conn.execute(
            "INSERT INTO repository_aliases
             (alias, repository_id, confirmed, first_seen_at, last_seen_at)
             VALUES ('example/shared', ?1, 1, '9999-01-01T00:00:00Z', '9999-01-01T00:00:00Z')
             ON CONFLICT(alias, repository_id) DO UPDATE SET
                confirmed = 1,
                last_seen_at = excluded.last_seen_at",
            [repo_id],
        )
        .unwrap();
    }
    drop(conn);

    let out = summary_cmd(&ctx).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let shared_labels: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("example/shared"))
        .collect();
    assert_eq!(
        shared_labels.len(),
        2,
        "both repository identities remain visible: {text}"
    );
    assert!(
        text.lines().any(|line| {
            line.starts_with("repo_alpha")
                && line.contains("example/shared")
                && line.contains("AMBIG:2")
        }) && text.lines().any(|line| {
            line.starts_with("repo_beta")
                && line.contains("example/shared")
                && line.contains("AMBIG:2")
        }),
        "each ambiguous label exposes its exact owner id and ambiguity: {text}"
    );
    assert!(
        text.contains("counts are never merged by LABEL")
            && text.contains("snag review list --repo <OWNER_ID> --unhandled"),
        "identity ambiguity must explain the safe next action: {text}"
    );

    let out = summary_cmd(&ctx)
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let repo_ids: std::collections::BTreeSet<&str> = value["repos"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|lane| lane["repo_id"].as_str())
        .collect();
    assert_eq!(
        repo_ids,
        std::collections::BTreeSet::from(["repo_alpha", "repo_beta"]),
        "JSON preserves one lane per canonical repository id"
    );
    for lane in value["repos"].as_array().unwrap() {
        assert_eq!(lane["identity"]["status"], "ambiguous-label");
        assert_eq!(lane["identity"]["label_repository_count"], 2);
        assert_eq!(lane["identity"]["ambiguous_label"], true);
    }
}

// ---------------------------------------------------------------------------
// t11: display aliases must be backed by a checkout bound to the same
// repository. A newer unsupported alias is historical identity contamination,
// not a better owner label.
// ---------------------------------------------------------------------------

#[test]
fn t11_display_prefers_alias_supported_by_bound_checkout() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
        .arg("supported-owner-alias")
        .arg("--kind")
        .arg("bug")
        .arg("--severity")
        .arg("major")
        .arg("--repo-id")
        .arg("repo_owner")
        .arg("--owner")
        .arg("repo_owner")
        .assert()
        .success();

    let conn = ctx.conn();
    let supported_alias: String = conn
        .query_row(
            "SELECT alias FROM repository_aliases
             WHERE repository_id = 'repo_owner'
             ORDER BY last_seen_at DESC, alias DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO repository_aliases
         (alias, repository_id, confirmed, first_seen_at, last_seen_at)
         VALUES ('zzz/unsupported', 'repo_owner', 1,
                 '9999-01-01T00:00:00Z', '9999-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    drop(conn);

    let out = summary_cmd(&ctx)
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let owner = value["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|lane| lane["repo_id"] == "repo_owner")
        .expect("repo_owner lane present");
    assert_eq!(
        owner["display"], supported_alias,
        "unsupported historical alias must not replace a checkout-backed owner label"
    );
}

// ---------------------------------------------------------------------------
// t12: a readable explicit owner id is authoritative. Historical aliases from
// a different checkout must never relabel that owner as the reporter repository.
// ---------------------------------------------------------------------------

#[test]
fn t12_explicit_owner_id_is_not_replaced_by_unrelated_alias() {
    let ctx = TestContext::new();
    report_in(&ctx, "foreign-owned", "foreign-owner", "major");

    let conn = ctx.conn();
    conn.execute(
        "INSERT INTO repository_aliases
         (alias, repository_id, confirmed, first_seen_at, last_seen_at)
         VALUES ('example/reporter', 'foreign-owner', 1,
                 '9999-01-01T00:00:00Z', '9999-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    drop(conn);

    let out = summary_cmd(&ctx)
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let foreign = value["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|lane| lane["repo_id"] == "foreign-owner")
        .expect("explicit owner lane present");
    assert_eq!(
        foreign["display"], "foreign-owner",
        "unrelated alias must not relabel the explicit owner"
    );
}

// ---------------------------------------------------------------------------
// t13: owner assignment is an append-only transition from the unowned bucket
// into exactly one owner lane. Replaying the same command is idempotent.
// ---------------------------------------------------------------------------

#[test]
fn t13_assign_owner_moves_unowned_observation_once() {
    let ctx = TestContext::new();
    let observation_id = report_unowned(&ctx, "assign-me", "major");

    for _ in 0..2 {
        ctx.cmd()
            .arg("review")
            .arg("assign-owner")
            .arg(&observation_id)
            .arg("repo_owner")
            .arg("--reviewer")
            .arg("reviewer-a")
            .arg("--session-id")
            .arg("session-a")
            .arg("--idempotency-key")
            .arg("assign-owner-once")
            .assert()
            .success();
    }

    let out = summary_cmd(&ctx)
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let owner = value["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|lane| lane["repo_id"] == "repo_owner")
        .expect("assigned owner lane present");
    assert_eq!(owner["open"], 1);
    assert!(
        value["unowned"].is_null(),
        "assigned observation must leave the unowned bucket: {value}"
    );

    let list = ctx
        .cmd()
        .arg("review")
        .arg("list")
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(list.status.success());
    let list: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let listed = list
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["observation_id"] == observation_id)
        .unwrap();
    assert_eq!(listed["owner_repository_id"], "repo_owner");

    let show = ctx
        .cmd()
        .arg("review")
        .arg("show")
        .arg(&observation_id)
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(show.status.success());
    let show: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show["current_state"]["owner_repository_id"], "repo_owner");

    let conn = ctx.conn();
    let owner_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM observation_repositories
             WHERE observation_id = ?1 AND role = 'owner' AND repository_id = 'repo_owner'",
            [&observation_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(owner_rows, 1, "owner projection is singular");
    let owner_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records
             WHERE entity_id = ?1 AND record_type = 'observation_owner_assigned'",
            [&observation_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(owner_events, 1, "idempotent replay appends no second event");
}

#[test]
fn t14_assign_owner_rejects_ambiguous_alias() {
    let ctx = TestContext::new();
    let observation_id = report_unowned(&ctx, "ambiguous-owner", "major");
    let conn = ctx.conn();
    for repo_id in ["repo_alpha", "repo_beta"] {
        conn.execute(
            "INSERT INTO repositories (repository_id, created_at)
             VALUES (?1, '2026-08-08T00:00:00Z')",
            [repo_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO repository_aliases
             (alias, repository_id, confirmed, first_seen_at, last_seen_at)
             VALUES ('example/shared', ?1, 1,
                     '2026-08-08T00:00:00Z', '2026-08-08T00:00:00Z')",
            [repo_id],
        )
        .unwrap();
    }
    drop(conn);

    ctx.cmd()
        .arg("review")
        .arg("assign-owner")
        .arg(&observation_id)
        .arg("example/shared")
        .assert()
        .failure()
        .stderr(predicates::str::contains("Repository ambiguous"));
    summary_cmd(&ctx)
        .arg("--repo")
        .arg("example/shared")
        .assert()
        .failure()
        .stderr(predicates::str::contains("Repository ambiguous"));

    let owner_rows: i64 = ctx
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM observation_repositories
             WHERE observation_id = ?1 AND role = 'owner'",
            [&observation_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(owner_rows, 0, "failed assignment must not mutate ownership");
}

#[test]
fn t14_report_rejects_ambiguous_owner_alias_without_writes() {
    let ctx = TestContext::new();
    report_unowned(&ctx, "existing observation", "minor");
    let conn = ctx.conn();
    for repo_id in ["repo_alpha", "repo_beta"] {
        conn.execute(
            "INSERT INTO repositories (repository_id, created_at)
             VALUES (?1, '2026-08-08T00:00:00Z')",
            [repo_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO repository_aliases
             (alias, repository_id, confirmed, first_seen_at, last_seen_at)
             VALUES ('example/shared', ?1, 1,
                     '2026-08-08T00:00:00Z', '2026-08-08T00:00:00Z')",
            [repo_id],
        )
        .unwrap();
    }
    drop(conn);

    ctx.cmd()
        .arg("report")
        .arg("must not create a duplicate owner")
        .arg("--owner")
        .arg("example/shared")
        .assert()
        .failure()
        .stderr(predicates::str::contains("Repository ambiguous"));

    let conn = ctx.conn();
    let observations: i64 = conn
        .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        observations, 1,
        "failed report must not append an observation"
    );
    let literal_owner: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM repositories WHERE repository_id = 'example/shared'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        literal_owner, 0,
        "ambiguous alias must not become a literal repository id"
    );
}

#[test]
fn t15_verify_detects_owner_projection_drift() {
    let ctx = TestContext::new();
    let observation_id = report_unowned(&ctx, "owner-drift", "major");
    ctx.cmd()
        .arg("review")
        .arg("assign-owner")
        .arg(&observation_id)
        .arg("repo_owner")
        .assert()
        .success();

    ctx.conn()
        .execute(
            "DELETE FROM observation_repositories
             WHERE observation_id = ?1 AND role = 'owner'",
            [&observation_id],
        )
        .unwrap();

    let verify = ctx.cmd().arg("verify").arg("--full").output().unwrap();
    assert!(
        !verify.status.success(),
        "owner projection drift must fail full verification"
    );
    assert!(
        String::from_utf8_lossy(&verify.stderr).contains("owner mismatch"),
        "verification must diagnose owner drift: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
}

#[test]
fn t16_replaying_old_assignment_does_not_undo_newer_owner() {
    let ctx = TestContext::new();
    let observation_id = report_unowned(&ctx, "reassign-me", "major");
    for (owner, key) in [("repo_a", "assign-a"), ("repo_b", "assign-b")] {
        ctx.cmd()
            .arg("review")
            .arg("assign-owner")
            .arg(&observation_id)
            .arg(owner)
            .arg("--idempotency-key")
            .arg(key)
            .assert()
            .success();
    }

    ctx.cmd()
        .arg("review")
        .arg("assign-owner")
        .arg(&observation_id)
        .arg("repo_a")
        .arg("--idempotency-key")
        .arg("assign-a")
        .assert()
        .success();

    let owners: Vec<String> = {
        let conn = ctx.conn();
        let mut stmt = conn
            .prepare(
                "SELECT repository_id FROM observation_repositories
                 WHERE observation_id = ?1 AND role = 'owner'
                 ORDER BY repository_id",
            )
            .unwrap();
        stmt.query_map([&observation_id], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(
        owners,
        vec!["repo_b"],
        "replaying an older idempotency key must preserve the latest event"
    );
}

#[test]
fn t17_exact_owner_id_wins_over_colliding_alias() {
    let ctx = TestContext::new();
    let observation_id = report_unowned(&ctx, "canonical-owner", "major");
    let conn = ctx.conn();
    for repo_id in ["repo_a", "repo_b"] {
        conn.execute(
            "INSERT INTO repositories (repository_id, created_at)
             VALUES (?1, '2026-08-08T00:00:00Z')",
            [repo_id],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO repository_aliases
         (alias, repository_id, confirmed, first_seen_at, last_seen_at)
         VALUES ('repo_a', 'repo_b', 1,
                 '2026-08-08T00:00:00Z', '2026-08-08T00:00:00Z')",
        [],
    )
    .unwrap();
    drop(conn);

    ctx.cmd()
        .arg("review")
        .arg("assign-owner")
        .arg(&observation_id)
        .arg("repo_a")
        .assert()
        .success();
    let owner: String = ctx
        .conn()
        .query_row(
            "SELECT repository_id FROM observation_repositories
             WHERE observation_id = ?1 AND role = 'owner'",
            [&observation_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(owner, "repo_a");
}

#[test]
fn t18_verify_checks_report_time_and_absent_owners() {
    let initial_ctx = TestContext::new();
    let initial_id = report_in(&initial_ctx, "initial-owner", "repo_initial", "major");
    initial_ctx
        .conn()
        .execute(
            "DELETE FROM observation_repositories
             WHERE observation_id = ?1 AND role = 'owner'",
            [&initial_id],
        )
        .unwrap();
    let initial_verify = initial_ctx
        .cmd()
        .arg("verify")
        .arg("--full")
        .output()
        .unwrap();
    assert!(
        !initial_verify.status.success(),
        "missing report-time owner must fail full verification"
    );
    assert!(
        String::from_utf8_lossy(&initial_verify.stderr).contains("owner mismatch"),
        "verification must diagnose the missing report-time owner: {}",
        String::from_utf8_lossy(&initial_verify.stderr)
    );

    let absent_ctx = TestContext::new();
    let absent_id = report_unowned(&absent_ctx, "absent-owner", "major");
    let conn = absent_ctx.conn();
    conn.execute(
        "INSERT INTO repositories (repository_id, created_at)
         VALUES ('repo_stale', '2026-08-08T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO observation_repositories
         (observation_id, repository_id, role)
         VALUES (?1, 'repo_stale', 'owner')",
        [&absent_id],
    )
    .unwrap();
    drop(conn);
    let absent_verify = absent_ctx
        .cmd()
        .arg("verify")
        .arg("--full")
        .output()
        .unwrap();
    assert!(
        !absent_verify.status.success(),
        "unexpected projected owner must fail full verification"
    );
    assert!(
        String::from_utf8_lossy(&absent_verify.stderr).contains("owner mismatch"),
        "verification must diagnose the unexpected projected owner: {}",
        String::from_utf8_lossy(&absent_verify.stderr)
    );
}

#[test]
fn t19_assignment_identity_writes_roll_back_with_event() {
    let ctx = TestContext::new();
    let observation_id = report_unowned(&ctx, "atomic-owner", "major");
    let repository = tempfile::tempdir().unwrap();
    assert!(
        std::process::Command::new("git")
            .arg("init")
            .current_dir(repository.path())
            .status()
            .unwrap()
            .success()
    );
    let before: (i64, i64, i64, i64) = ctx
        .conn()
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM repositories),
                (SELECT COUNT(*) FROM repository_aliases),
                (SELECT COUNT(*) FROM checkouts),
                (SELECT COUNT(*) FROM worktrees)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();

    ctx.cmd()
        .arg("review")
        .arg("assign-owner")
        .arg(&observation_id)
        .arg(repository.path())
        .env("SNAG_FAILPOINT", "remediation_before_tx")
        .assert()
        .failure();

    let after: (i64, i64, i64, i64) = ctx
        .conn()
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM repositories),
                (SELECT COUNT(*) FROM repository_aliases),
                (SELECT COUNT(*) FROM checkouts),
                (SELECT COUNT(*) FROM worktrees)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        after, before,
        "failed assignment must leave no identity rows"
    );
}
// ---------------------------------------------------------------------------
// t20: JSON keeps the v1 envelope additive while exposing actionable and
// in-flight projections. An in-flight major remains open but is not ready.
// ---------------------------------------------------------------------------

#[test]
fn t20_json_exposes_additive_actionable_projection() {
    let ctx = TestContext::new();
    let in_flight = report_in(&ctx, "in-flight-major", "repo_actionable", "major");
    report_in(&ctx, "ready-blocker", "repo_actionable", "blocker");
    report_in(&ctx, "ready-medium", "repo_actionable", "medium");
    report_in(&ctx, "ready-minor", "repo_actionable", "minor");
    report_in(&ctx, "ready-low", "repo_actionable", "low");

    ctx.cmd()
        .arg("review")
        .arg("disposition")
        .arg(&in_flight)
        .arg("confirmed")
        .assert()
        .success();
    ctx.cmd()
        .arg("review")
        .arg("attach-fix")
        .arg(&in_flight)
        .arg("--commit")
        .arg("sha-in-flight")
        .arg("--repo")
        .arg("repo_actionable")
        .assert()
        .success();

    let out = summary_cmd(&ctx)
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["schema"], "review_summary_v1");
    let lane = value["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|lane| lane["repo_id"] == "repo_actionable")
        .expect("actionable lane present");

    // Existing fields retain their meanings.
    assert_eq!(lane["open"], 5);
    assert_eq!(lane["severity_counts"]["major"], 1);
    assert_eq!(lane["severity_counts"]["blocker"], 1);
    // New fields reconcile: 4 ready observations, one in flight.
    assert_eq!(lane["actionable"], 4);
    assert_eq!(lane["in_flight"], 1);
    assert_eq!(lane["actionable_severity_counts"]["blocker"], 1);
    assert_eq!(lane["actionable_severity_counts"]["major"], 0);
    assert_eq!(lane["actionable_severity_counts"]["medium"], 1);
    assert_eq!(lane["actionable_severity_counts"]["minor"], 1);
    assert_eq!(lane["actionable_severity_counts"]["low"], 1);
}

// ---------------------------------------------------------------------------
// t21: text labels actionable readiness explicitly, including all five
// severities, while retaining the existing dispatch columns.
// ---------------------------------------------------------------------------

#[test]
fn t21_text_exposes_ready_inflight_and_actionable_severity_mix() {
    let ctx = TestContext::new();
    let in_flight = report_in(&ctx, "text-in-flight-major", "repo_text", "major");
    report_in(&ctx, "text-ready-blocker", "repo_text", "blocker");
    report_in(&ctx, "text-ready-medium", "repo_text", "medium");
    report_in(&ctx, "text-ready-minor", "repo_text", "minor");
    report_in(&ctx, "text-ready-low", "repo_text", "low");

    ctx.cmd()
        .arg("review")
        .arg("disposition")
        .arg(&in_flight)
        .arg("confirmed")
        .assert()
        .success();
    ctx.cmd()
        .arg("review")
        .arg("attach-fix")
        .arg(&in_flight)
        .arg("--commit")
        .arg("sha-text")
        .arg("--repo")
        .arg("repo_text")
        .assert()
        .success();

    let out = summary_cmd(&ctx).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let header = text.lines().next().unwrap();
    for column in [
        "OPEN", "READY", "INFLT", "R:B", "R:M", "R:MED", "R:MIN", "R:LOW", "UNREV", "OLDEST",
        "MAT", "VERDICT",
    ] {
        assert!(header.contains(column), "missing {column} header: {text}");
    }
    assert!(
        !header.contains("INFLIGHT") && !header.contains("READY_"),
        "summary header regressed to the wide form: {header}"
    );
    let row = text
        .lines()
        .find(|line| line.starts_with("repo_text"))
        .expect("repo_text row present");
    let cells: Vec<&str> = row.split_whitespace().collect();
    assert_eq!(cells[0], "repo_text");
    assert_eq!(
        cells[1], "repo_text",
        "LABEL remains visible beside OWNER_ID"
    );
    assert_eq!(cells[2], "ID-ONLY");
    assert_eq!(cells[3], "5", "OPEN includes the in-flight major");
    assert_eq!(cells[4], "4", "READY excludes the in-flight major");
    assert_eq!(cells[5], "1", "INFLT is open minus ready");
    assert_eq!(&cells[6..11], ["1", "0", "1", "1", "1"]);
    assert!(text.contains("UNREV") && text.contains("OLDEST"));
    assert!(text.contains("MAT") && text.contains("VERDICT"));
}

// ---------------------------------------------------------------------------
// t22: JSON --limit controls rendered owner lanes only; threshold evaluation
// still sees a hidden lane, and unowned remains a separate bucket.
// ---------------------------------------------------------------------------

#[test]
fn t22_json_limit_hides_lanes_without_hiding_thresholds_or_unowned() {
    let ctx = TestContext::new();
    report_in(&ctx, "visible-blocker-1", "repo_visible", "blocker");
    report_in(&ctx, "visible-blocker-2", "repo_visible", "blocker");
    report_in(&ctx, "hidden-major-1", "repo_hidden", "major");
    report_in(&ctx, "hidden-major-2", "repo_hidden", "major");
    report_unowned(&ctx, "still-unowned", "low");

    let out = summary_cmd(&ctx)
        .arg("--format")
        .arg("json")
        .arg("--limit")
        .arg("1")
        .arg("--at-least")
        .arg("major=2")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "hidden repo_hidden lane still drives threshold exit"
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let repos = value["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 1, "--limit applies to rendered owner lanes");
    assert_eq!(repos[0]["repo_id"], "repo_visible");
    assert!(
        repos.iter().all(|lane| lane["repo_id"] != "repo_hidden"),
        "hidden lane must not be rendered: {value}"
    );
    assert_eq!(value["unowned"]["open"], 1);
    assert_eq!(value["unowned"]["actionable"], 1);
    assert_eq!(value["unowned"]["actionable_severity_counts"]["low"], 1);
}

// ---------------------------------------------------------------------------
// t23: equal materiality uses canonical repository id as a stable tie-break.
// ---------------------------------------------------------------------------

#[test]
fn t23_equal_materiality_sorts_by_canonical_repo_id() {
    let ctx = TestContext::new();
    report_in(&ctx, "tie-beta", "repo_beta", "major");
    report_in(&ctx, "tie-alpha", "repo_alpha", "major");

    let out = summary_cmd(&ctx)
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let ids: Vec<&str> = value["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|lane| lane["repo_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["repo_alpha", "repo_beta"]);
}

// ---------------------------------------------------------------------------
// t24: retracted observations are absent from every summary lane.
// ---------------------------------------------------------------------------

#[test]
fn t24_retracted_observation_is_excluded() {
    let ctx = TestContext::new();
    let observation_id = report_in(&ctx, "retract-me", "repo_retracted", "major");
    ctx.cmd()
        .arg("retract")
        .arg(&observation_id)
        .assert()
        .success();

    let out = summary_cmd(&ctx)
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        value["repos"]
            .as_array()
            .unwrap()
            .iter()
            .all(|lane| lane["repo_id"] != "repo_retracted"),
        "retracted observations must not populate owner lanes: {value}"
    );
    assert!(
        value["unowned"].is_null(),
        "retracted observations must not populate unowned: {value}"
    );
}

// ---------------------------------------------------------------------------
// t25: a live active_claims row is in-flight even while the reduced state is
// otherwise actionable.
// ---------------------------------------------------------------------------

#[test]
fn t25_live_claim_is_in_flight() {
    let ctx = TestContext::new();
    let observation_id = report_in(&ctx, "claimed-now", "repo_claimed", "major");
    ctx.cmd()
        .arg("review")
        .arg("claim")
        .arg(&observation_id)
        .assert()
        .success();

    let out = summary_cmd(&ctx)
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let lane = value["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|lane| lane["repo_id"] == "repo_claimed")
        .expect("claimed lane present");
    assert_eq!(lane["open"], 1);
    assert_eq!(lane["actionable"], 0);
    assert_eq!(lane["in_flight"], 1);
    assert_eq!(lane["severity_counts"]["major"], 1);
}
// ---------------------------------------------------------------------------
// t26: summary attribution follows the latest canonical owner projection.
// ---------------------------------------------------------------------------

#[test]
fn t26_reassignment_appears_only_in_latest_owner_lane() {
    let ctx = TestContext::new();
    let observation_id = report_in(&ctx, "reassigned", "repo_owner_old", "major");
    ctx.cmd()
        .arg("review")
        .arg("assign-owner")
        .arg(&observation_id)
        .arg("repo_owner_new")
        .assert()
        .success();

    let out = summary_cmd(&ctx)
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let repos = value["repos"].as_array().unwrap();
    assert!(
        repos.iter().all(|lane| lane["repo_id"] != "repo_owner_old"),
        "historical owner must not retain the observation: {value}"
    );
    let new_lane = repos
        .iter()
        .find(|lane| lane["repo_id"] == "repo_owner_new")
        .expect("latest owner lane present");
    assert_eq!(new_lane["open"], 1);
}

// ---------------------------------------------------------------------------
// t27: unknown severities reconcile across OPEN, READY, INFLT, materiality,
// compact text, and additive JSON fields without changing threshold parsing.
// ---------------------------------------------------------------------------

#[test]
fn t27_unknown_severity_is_an_additive_ready_bucket() {
    let ctx = TestContext::new();
    report_in(&ctx, "unknown-severity", "repo_unknown", "major");
    ctx.conn()
        .execute(
            "UPDATE observations SET severity_assertion = 'new-severity'
             WHERE title = 'unknown-severity'",
            [],
        )
        .unwrap();

    let out = summary_cmd(&ctx)
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let lane = value["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|lane| lane["repo_id"] == "repo_unknown")
        .expect("unknown severity lane present");
    assert_eq!(lane["open"], 1);
    assert_eq!(lane["actionable"], 1);
    assert_eq!(lane["in_flight"], 0);
    assert_eq!(lane["severity_counts"]["unknown"], 1);
    assert_eq!(lane["actionable_severity_counts"]["unknown"], 1);
    assert_eq!(lane["materiality"], 0.0);

    let text = String::from_utf8(summary_cmd(&ctx).output().unwrap().stdout).unwrap();
    assert!(text.lines().next().unwrap().contains("R:U"));
}
