//! Remediation queue (T1) and claim-lease (T2) suites.
//!
//! CLI-driven against a scratch store, subprocess-based for concurrency and
//! crash injection, matching the repo's integration-test conventions.

use assert_cmd::Command;
use rusqlite::Connection;
use std::path::PathBuf;
use std::process::Command as Proc;

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
    fn cmd_as(&self, session: &str) -> Command {
        let mut c = self.cmd();
        c.env("SNAG_REVIEWER_ID", format!("rev_{session}"))
            .env("SNAG_REVIEW_SESSION_ID", format!("sess_{session}"));
        c
    }
    fn bin(&self) -> PathBuf {
        assert_cmd::cargo::cargo_bin("snag")
    }
    fn conn(&self) -> Connection {
        Connection::open(self.data_dir.join("snag.sqlite")).unwrap()
    }
}

/// File an observation; return its id.
fn report(ctx: &TestContext, title: &str, kind: &str, severity: &str) -> String {
    report_in(ctx, title, kind, severity, None)
}

/// File an observation, optionally pinned to a repository; return its id.
fn report_in(
    ctx: &TestContext,
    title: &str,
    kind: &str,
    severity: &str,
    repo: Option<&str>,
) -> String {
    let mut cmd = ctx.cmd();
    cmd.arg("report")
        .arg(title)
        .arg("--kind")
        .arg(kind)
        .arg("--severity")
        .arg(severity);
    if let Some(r) = repo {
        cmd.arg("--repo-id").arg(r);
    }
    cmd.arg("--unowned");
    cmd.assert().success();
    let conn = ctx.conn();
    conn.query_row(
        "SELECT observation_id FROM observations WHERE title = ?1 ORDER BY local_sequence DESC LIMIT 1",
        [title],
        |r| r.get(0),
    )
    .unwrap()
}
fn report_owned(ctx: &TestContext, title: &str, kind: &str, severity: &str, owner: &str) -> String {
    ctx.cmd()
        .arg("report")
        .arg(title)
        .arg("--kind")
        .arg(kind)
        .arg("--severity")
        .arg(severity)
        .arg("--owner")
        .arg(owner)
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

fn record_count(ctx: &TestContext) -> i64 {
    let conn = ctx.conn();
    conn.query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0))
        .unwrap()
}

fn claim_count(ctx: &TestContext) -> i64 {
    let conn = ctx.conn();
    conn.query_row("SELECT COUNT(*) FROM remediation_claims", [], |r| r.get(0))
        .unwrap()
}

fn active_claim_for(ctx: &TestContext, obs: &str) -> Option<String> {
    let conn = ctx.conn();
    conn.query_row(
        "SELECT claim_id FROM remediation_claims
         WHERE observation_id = ?1 AND released_at IS NULL ORDER BY claimed_at DESC LIMIT 1",
        [obs],
        |r| r.get(0),
    )
    .ok()
}

// ---------------------------------------------------------------------------
// T1: queue retrieval.
// ---------------------------------------------------------------------------

#[test]
fn t1_deterministic_next_oldest_highest_severity() {
    let ctx = TestContext::new();
    let minor_old = report(&ctx, "minor old", "bug", "minor");
    std::thread::sleep(std::time::Duration::from_millis(10));
    // "major old" is filed first among the majors: it must be the oldest.
    let major_old = report(&ctx, "major old", "bug", "major");
    std::thread::sleep(std::time::Duration::from_millis(10));
    report(&ctx, "major new", "bug", "major");

    // Highest severity first; within severity, oldest first.
    let out = ctx.cmd().arg("review").arg("next").output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains(&major_old), "expected major_old, got: {text}");

    let out = ctx
        .cmd()
        .arg("review")
        .arg("next")
        .arg("--severity")
        .arg("minor")
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains(&minor_old), "expected minor_old, got: {text}");
}

#[test]
fn t1_filters_kind_and_severity() {
    let ctx = TestContext::new();
    let bug = report(&ctx, "a bug", "bug", "major");
    report(&ctx, "a papercut", "papercut", "major");

    let out = ctx
        .cmd()
        .arg("review")
        .arg("next")
        .arg("--kind")
        .arg("bug")
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains(&bug));
    assert!(!text.contains("a papercut"));
}

#[test]
fn t1_empty_queue_is_typed_not_error() {
    let ctx = TestContext::new();
    let out = ctx.cmd().arg("review").arg("next").output().unwrap();
    assert!(out.status.success(), "empty queue must not be an error");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("empty queue"));

    let out = ctx
        .cmd()
        .arg("review")
        .arg("next")
        .arg("--format")
        .arg("agent")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["queue"], "empty");
    // The empty envelope names the active store: a wrong-store (leaked
    // XDG_DATA_HOME) is one-glance obvious instead of a baffling empty queue.
    assert_eq!(
        parsed["store"]["db_path"],
        ctx.data_dir.join("snag.sqlite").display().to_string()
    );
    assert_eq!(parsed["store"]["observations"], 0);
}

#[test]
fn t1_retracted_observations_are_excluded() {
    let ctx = TestContext::new();
    let obs = report(&ctx, "doomed", "bug", "major");
    ctx.cmd().arg("retract").arg(&obs).assert().success();

    let out = ctx.cmd().arg("review").arg("next").output().unwrap();
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("empty queue")
    );
}

#[test]
fn t1_active_claims_by_other_sessions_are_excluded() {
    let ctx = TestContext::new();
    let obs = report(&ctx, "claimed elsewhere", "bug", "major");
    ctx.cmd_as("alice")
        .arg("review")
        .arg("claim")
        .arg(&obs)
        .assert()
        .success();

    // Bob's queue is empty; Alice's queue still surfaces her own claim.
    let out = ctx
        .cmd_as("bob")
        .arg("review")
        .arg("next")
        .output()
        .unwrap();
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("empty queue")
    );
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("next")
        .output()
        .unwrap();
    assert!(String::from_utf8(out.stdout).unwrap().contains(&obs));
}

// ---------------------------------------------------------------------------
// T1b: review list filters + pagination.
// ---------------------------------------------------------------------------

/// `review list --format json` parsed as a row array.
fn list_json(ctx: &TestContext, args: &[&str]) -> Vec<serde_json::Value> {
    let out = ctx
        .cmd()
        .arg("review")
        .arg("list")
        .arg("--format")
        .arg("json")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "review list failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str::<Vec<serde_json::Value>>(&String::from_utf8(out.stdout).unwrap()).unwrap()
}

fn assign_owner(ctx: &TestContext, observation_id: &str, repository_id: &str) {
    ctx.cmd()
        .arg("review")
        .arg("assign-owner")
        .arg(observation_id)
        .arg(repository_id)
        .assert()
        .success();
}

#[test]
fn t1b_list_filters_by_repository() {
    let ctx = TestContext::new();
    let in_r1 = report_in(&ctx, "r1 bug", "bug", "major", Some("repo_alpha"));
    let in_r2 = report_in(&ctx, "r2 bug", "bug", "minor", Some("repo_beta"));
    assign_owner(&ctx, &in_r1, "repo_alpha");
    assign_owner(&ctx, &in_r2, "repo_beta");

    let rows = list_json(&ctx, &["--repo", "repo_alpha"]);
    assert_eq!(rows.len(), 1, "repo filter must scope to the lane");

    assert_eq!(rows[0]["observation_id"], in_r1);
    assert_ne!(rows[0]["observation_id"], in_r2);

    // Unknown repo is a typed error, not an empty list.
    let out = ctx
        .cmd()
        .arg("review")
        .arg("list")
        .arg("--repo")
        .arg("no_such_repo")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("no_such_repo"),
        "unknown repo must name the missing id"
    );
}
#[test]
fn t1b_owner_filter_uses_latest_owner_for_list_and_next() {
    let ctx = TestContext::new();
    let moved = report_in(&ctx, "moves to beta", "bug", "major", Some("repo_alpha"));
    let alpha_stays = report_in(&ctx, "stays with alpha", "bug", "minor", Some("repo_alpha"));
    assign_owner(&ctx, &moved, "repo_alpha");
    assign_owner(&ctx, &alpha_stays, "repo_alpha");
    assign_owner(&ctx, &moved, "repo_beta");

    let alpha = list_json(&ctx, &["--repo", "repo_alpha"]);
    assert!(alpha.iter().all(|row| row["observation_id"] != moved));
    assert!(alpha.iter().any(|row| row["observation_id"] == alpha_stays));
    let beta = list_json(&ctx, &["--repo", "repo_beta"]);
    assert!(beta.iter().any(|row| row["observation_id"] == moved));

    let alpha_next = ctx
        .cmd()
        .arg("review")
        .arg("next")
        .arg("--repo")
        .arg("repo_alpha")
        .output()
        .unwrap();
    assert!(
        String::from_utf8(alpha_next.stdout)
            .unwrap()
            .contains(&alpha_stays)
    );
    let beta_next = ctx
        .cmd()
        .arg("review")
        .arg("next")
        .arg("--repo")
        .arg("repo_beta")
        .arg("--format")
        .arg("agent")
        .output()
        .unwrap();
    let packet: serde_json::Value = serde_json::from_slice(&beta_next.stdout).unwrap();
    assert_eq!(packet["observation"]["observation_id"], moved);
    assert_eq!(packet["current_state"]["owner_repository_id"], "repo_beta");
}

#[test]
fn t1b_agent_packet_includes_fresh_report_owner() {
    let ctx = TestContext::new();
    let observation = report_owned(&ctx, "fresh owner", "bug", "major", "repo_fresh");
    let out = ctx
        .cmd()
        .arg("review")
        .arg("next")
        .arg("--repo")
        .arg("repo_fresh")
        .arg("--format")
        .arg("agent")
        .output()
        .unwrap();
    assert!(out.status.success());
    let packet: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(packet["observation"]["observation_id"], observation);
    assert_eq!(packet["current_state"]["owner_repository_id"], "repo_fresh");
}

#[test]
fn t1b_next_unknown_current_repo_is_read_only() {
    let ctx = TestContext::new();
    let unknown_repo = ctx.home_dir.path().join("fresh-unknown");
    std::fs::create_dir_all(&unknown_repo).unwrap();
    assert!(
        Proc::new("git")
            .args(["init", "-q"])
            .current_dir(&unknown_repo)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Proc::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "git@github.com:acme/never-recorded-read-purity.git",
            ])
            .current_dir(&unknown_repo)
            .status()
            .unwrap()
            .success()
    );
    let _ = report(&ctx, "read-only repo lookup", "bug", "major");
    let before_records = record_count(&ctx);
    let before_claims = claim_count(&ctx);
    let before_observations = ctx
        .conn()
        .query_row("SELECT COUNT(*) FROM observations", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    let before_bytes = std::fs::metadata(ctx.data_dir.join("snag.sqlite"))
        .unwrap()
        .len();
    let out = ctx
        .cmd()
        .arg("review")
        .arg("next")
        .arg("--repo")
        .arg("current")
        .current_dir(&unknown_repo)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(record_count(&ctx), before_records);
    assert_eq!(claim_count(&ctx), before_claims);
    assert_eq!(
        ctx.conn()
            .query_row("SELECT COUNT(*) FROM observations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        before_observations
    );
    assert_eq!(
        std::fs::metadata(ctx.data_dir.join("snag.sqlite"))
            .unwrap()
            .len(),
        before_bytes
    );
}

#[test]
fn t1b_review_repo_help_names_fix_owner_lane() {
    let ctx = TestContext::new();
    for subcommand in ["next", "list", "summary"] {
        let out = ctx
            .cmd()
            .args(["review", subcommand, "--help"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let help = String::from_utf8_lossy(&out.stdout);
        assert!(help.contains("fix-owner"), "{subcommand} help: {help}");
        assert!(
            help.contains("top-level `snag list --repo`"),
            "{subcommand} help: {help}"
        );
    }
}

#[test]
fn t1b_review_remediation_help_requires_confirmed_disposition() {
    let ctx = TestContext::new();
    for subcommand in [
        "attach-task",
        "promote",
        "attach-fix",
        "attach-verification",
    ] {
        let out = ctx
            .cmd()
            .args(["review", subcommand, "--help"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let help = String::from_utf8_lossy(&out.stdout);
        assert!(
            help.contains("Requires the observation to have disposition `confirmed`."),
            "{subcommand} help: {help}"
        );
    }
}

#[test]
fn t1b_list_filters_kind_and_severity() {
    let ctx = TestContext::new();
    let bug = report(&ctx, "a bug", "bug", "major");
    let papercut = report(&ctx, "a papercut", "papercut", "minor");

    let rows = list_json(&ctx, &["--kind", "papercut"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["observation_id"], papercut);
    assert_ne!(rows[0]["observation_id"], bug);

    let rows = list_json(&ctx, &["--severity", "major"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["observation_id"], bug);
}

#[test]
fn t1b_list_unreviewed_and_unhandled_with_deferred() {
    let ctx = TestContext::new();
    let a = report(&ctx, "unreviewed a", "bug", "major");
    let b = report(&ctx, "deferred b", "bug", "minor");
    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&b)
        .arg("deferred")
        .assert()
        .success();

    // --unreviewed: only the untouched observation.
    let rows = list_json(&ctx, &["--unreviewed"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["observation_id"], a);

    // --unhandled: deferred marks handled=true in the reducer, so b is hidden.
    let rows = list_json(&ctx, &["--unhandled"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["observation_id"], a);

    // --unhandled --include-deferred: the lane owner still owns deferred work.
    let rows = list_json(&ctx, &["--unhandled", "--include-deferred"]);
    let ids: Vec<&str> = rows
        .iter()
        .map(|r| r["observation_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&a.as_str()));
    assert!(ids.contains(&b.as_str()));
}

#[test]
fn t1b_list_paginates() {
    let ctx = TestContext::new();
    let mut expected = Vec::new();
    for i in 0..5 {
        expected.push(report(&ctx, &format!("page {i}"), "bug", "minor"));
    }

    let page1 = list_json(&ctx, &["--limit", "2"]);
    assert_eq!(page1.len(), 2);
    assert_eq!(page1[0]["observation_id"], expected[0]);
    assert_eq!(page1[1]["observation_id"], expected[1]);

    let page2 = list_json(&ctx, &["--limit", "2", "--offset", "2"]);
    assert_eq!(page2.len(), 2);
    assert_eq!(page2[0]["observation_id"], expected[2]);
    assert_eq!(page2[1]["observation_id"], expected[3]);

    // Offset past the end: empty page, not an error.
    let tail = list_json(&ctx, &["--limit", "2", "--offset", "10"]);
    assert!(tail.is_empty());

    // Default (limit 0) is unbounded — the text-parse consumer's contract.
    let all = list_json(&ctx, &[]);
    assert_eq!(all.len(), 5);
}

// ---------------------------------------------------------------------------
// T2: claim leases.
// ---------------------------------------------------------------------------

#[test]
fn t2_claim_acquire_and_same_session_replay_is_idempotent() {
    let ctx = TestContext::new();
    let obs = report(&ctx, "claim me", "bug", "major");
    let before = record_count(&ctx);

    ctx.cmd_as("alice")
        .arg("review")
        .arg("claim")
        .arg(&obs)
        .assert()
        .success();
    let first_claim = active_claim_for(&ctx, &obs).expect("claim must exist");
    assert_eq!(claim_count(&ctx), 1);
    assert_eq!(record_count(&ctx), before + 1, "one claimed event");

    // Same session, same observation: idempotent replay, no new record.
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("claim")
        .arg(&obs)
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("already held by this session"));
    assert_eq!(active_claim_for(&ctx, &obs).unwrap(), first_claim);
    assert_eq!(record_count(&ctx), before + 1, "replay must not append");
}

#[test]
fn t2_claim_conflict_for_another_session() {
    let ctx = TestContext::new();
    let obs = report(&ctx, "conflict", "bug", "major");
    ctx.cmd_as("alice")
        .arg("review")
        .arg("claim")
        .arg(&obs)
        .assert()
        .success();

    let out = ctx
        .cmd_as("bob")
        .arg("review")
        .arg("claim")
        .arg(&obs)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.contains("Claim conflict"),
        "expected CLAIM_CONFLICT, got: {err}"
    );
}

#[test]
fn t2_heartbeat_extends_only_the_callers_claim() {
    let ctx = TestContext::new();
    let obs = report(&ctx, "heartbeat", "bug", "major");
    ctx.cmd_as("alice")
        .arg("review")
        .arg("claim")
        .arg(&obs)
        .arg("--lease")
        .arg("30m")
        .assert()
        .success();
    let conn = ctx.conn();
    let base: String = conn
        .query_row(
            "SELECT lease_expires_at FROM remediation_claims WHERE observation_id = ?1 AND released_at IS NULL",
            [&obs],
            |r| r.get(0),
        )
        .unwrap();

    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("heartbeat")
        .arg(&obs)
        .arg("--lease")
        .arg("2h")
        .output()
        .unwrap();
    assert!(out.status.success());
    let conn = ctx.conn();
    let expiry: String = conn
        .query_row(
            "SELECT lease_expires_at FROM remediation_claims WHERE observation_id = ?1 AND released_at IS NULL",
            [&obs],
            |r| r.get(0),
        )
        .unwrap();
    // The 2h extension must strictly extend the 30m lease.
    assert!(
        expiry > base,
        "heartbeat must extend the lease: {expiry} vs {base}"
    );
    assert!(base.as_str() > "2026-08-05T00:00:00Z");

    // Another session cannot heartbeat.
    let out = ctx
        .cmd_as("bob")
        .arg("review")
        .arg("heartbeat")
        .arg(&obs)
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn t2_release_removes_lease_and_release_by_other_fails() {
    let ctx = TestContext::new();
    let obs = report(&ctx, "release", "bug", "major");
    ctx.cmd_as("alice")
        .arg("review")
        .arg("claim")
        .arg(&obs)
        .assert()
        .success();

    let out = ctx
        .cmd_as("bob")
        .arg("review")
        .arg("release")
        .arg(&obs)
        .output()
        .unwrap();
    assert!(!out.status.success(), "another session must not release");

    ctx.cmd_as("alice")
        .arg("review")
        .arg("release")
        .arg(&obs)
        .assert()
        .success();
    assert_eq!(active_claim_for(&ctx, &obs), None);
    // Releasing again fails (no active claim).
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("release")
        .arg(&obs)
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn t2_expired_lease_is_reacquirable_and_records_expiry() {
    let ctx = TestContext::new();
    let obs = report(&ctx, "expiry", "bug", "major");
    ctx.cmd_as("alice")
        .arg("review")
        .arg("claim")
        .arg(&obs)
        .arg("--lease")
        .arg("1s")
        .assert()
        .success();
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Bob acquires after the lease lapses.
    ctx.cmd_as("bob")
        .arg("review")
        .arg("claim")
        .arg(&obs)
        .assert()
        .success();
    let conn = ctx.conn();
    let expired_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE record_type = 'observation_claim_expired'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(expired_events, 1, "old lease expiry must be recorded");
    let active: String = conn
        .query_row(
            "SELECT claim_id FROM remediation_claims WHERE observation_id = ?1 AND released_at IS NULL",
            [&obs],
            |r| r.get(0),
        )
        .unwrap();
    assert!(active.starts_with("claim_"), "bob's claim must be active");
}

#[test]
fn t2_claim_with_task_links_owned_work() {
    let ctx = TestContext::new();
    let obs = report(&ctx, "task-linked", "bug", "major");
    ctx.cmd_as("alice")
        .arg("review")
        .arg("claim")
        .arg(&obs)
        .arg("--task")
        .arg("pearl_xyz")
        .assert()
        .success();
    let conn = ctx.conn();
    let links: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM remediation_links WHERE observation_id = ?1 AND link_type = 'task' AND target_id = 'pearl_xyz'",
            [&obs],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(links, 1);
    // The claim event and the task event both exist in the stream.
    let events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE entity_id = ?1 AND record_type IN ('observation_claimed','remediation_task_attached')",
            [&obs],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(events, 2);
}

#[test]
fn t2_process_crash_leaves_no_visible_claim() {
    // Crash before commit: the claim event and row must not exist (T7's
    // zero-visible-event outcome, exercised through the CLI).
    let ctx = TestContext::new();
    let obs = report(&ctx, "crash", "bug", "major");
    let before = record_count(&ctx);

    let mut child = Proc::new(ctx.bin());
    let out = child
        .args(["review", "claim", &obs])
        .env("XDG_DATA_HOME", ctx.home_dir.path())
        .env("HOME", ctx.home_dir.path())
        .env("SNAG_REVIEWER_ID", "rev_alice")
        .env("SNAG_REVIEW_SESSION_ID", "sess_alice")
        .env("SNAG_FAILPOINT", "remediation_before_commit")
        .output()
        .unwrap();
    assert!(!out.status.success(), "failpoint must abort the process");
    assert_eq!(record_count(&ctx), before, "no event may be visible");
    assert_eq!(claim_count(&ctx), 0, "no claim row may be visible");

    // The observation is still fully claimable afterwards.
    ctx.cmd_as("bob")
        .arg("review")
        .arg("claim")
        .arg(&obs)
        .assert()
        .success();
}

#[test]
fn t2_concurrent_acquisition_yields_exactly_one_winner() {
    let ctx = TestContext::new();
    let obs = report(&ctx, "race", "bug", "major");
    let bin = ctx.bin();

    let mut children = Vec::new();
    for i in 0..8 {
        let mut c = Proc::new(&bin);
        c.args(["review", "claim", &obs])
            .env("XDG_DATA_HOME", ctx.home_dir.path())
            .env("HOME", ctx.home_dir.path())
            .env("SNAG_REVIEWER_ID", format!("rev_{i}"))
            .env("SNAG_REVIEW_SESSION_ID", format!("sess_{i}"))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        children.push(c.spawn().unwrap());
    }

    let mut winners = 0;
    for child in children {
        let out = child.wait_with_output().unwrap();
        if out.status.success() {
            winners += 1;
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(
                err.contains("Claim conflict"),
                "loser must see CLAIM_CONFLICT: {err}"
            );
        }
    }
    assert_eq!(winners, 1, "exactly one session may hold the claim");
    assert_eq!(claim_count(&ctx), 1);
}

#[test]
fn t2_concurrent_owner_assignment_preserves_projection_and_stream() {
    let ctx = TestContext::new();
    let observation = report_owned(&ctx, "owner race", "bug", "major", "repo_alpha");
    let unrelated = report(&ctx, "unrelated survives", "bug", "minor");
    let before_records = record_count(&ctx);
    let bin = ctx.bin();
    let mut children = Vec::new();
    for (i, owner) in ["repo_alpha", "repo_beta"].into_iter().enumerate() {
        let mut child = Proc::new(&bin);
        child
            .args(["review", "assign-owner", &observation, owner])
            .env("XDG_DATA_HOME", ctx.home_dir.path())
            .env("HOME", ctx.home_dir.path())
            .env("SNAG_REVIEWER_ID", format!("rev_owner_{i}"))
            .env("SNAG_REVIEW_SESSION_ID", format!("sess_owner_{i}"))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        children.push(child.spawn().unwrap());
    }
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "concurrent owner assignment failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let conn = ctx.conn();
    let mut owner_stmt = conn
        .prepare(
            "SELECT repository_id FROM observation_repositories
             WHERE observation_id = ?1 AND role = 'owner'",
        )
        .unwrap();
    let owner_rows: Vec<String> = owner_stmt
        .query_map([&observation], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(owner_rows.len(), 1, "owner projection must remain singular");
    let stream_owner: String = conn
        .query_row(
            "SELECT json_extract(canonical_payload_json, '$.owner_repository_id')
             FROM records
             WHERE entity_id = ?1 AND record_type = 'observation_owner_assigned'
             ORDER BY local_sequence DESC LIMIT 1",
            [&observation],
            |row| row.get(0),
        )
        .unwrap();
    let show = ctx
        .cmd()
        .args(["review", "show", &observation, "--format", "agent"])
        .output()
        .unwrap();
    assert!(show.status.success());
    let packet: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(packet["current_state"]["owner_repository_id"], stream_owner);
    assert_eq!(owner_rows[0], stream_owner);
    assert_eq!(record_count(&ctx), before_records + 2);
    assert!(
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM observations WHERE observation_id = ?1)",
            [&unrelated],
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
    );
}

#[test]
fn t2_owner_assignment_failpoints_preserve_identity_and_projection() {
    let ctx = TestContext::new();
    let observation = report_owned(&ctx, "owner failpoint", "bug", "major", "repo_initial");
    let before_records = record_count(&ctx);

    assert!(!run_with_failpoint(
        &ctx,
        "remediation_before_commit",
        &["review", "assign-owner", &observation, "repo_before"],
    ));
    assert_eq!(record_count(&ctx), before_records);
    let conn = ctx.conn();
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM repositories WHERE repository_id = 'repo_before'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );

    assert!(!run_with_failpoint(
        &ctx,
        "remediation_after_commit",
        &["review", "assign-owner", &observation, "repo_after"],
    ));
    assert_eq!(record_count(&ctx), before_records + 1);
    assert_eq!(
        conn.query_row(
            "SELECT repository_id FROM observation_repositories
             WHERE observation_id = ?1 AND role = 'owner'",
            [&observation],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        "repo_after"
    );
}

// ---------------------------------------------------------------------------
// T3: dispositions.
// ---------------------------------------------------------------------------

fn current_disposition(ctx: &TestContext, obs: &str) -> Option<String> {
    let conn = ctx.conn();
    conn.query_row(
        "SELECT disposition FROM observation_dispositions
         WHERE observation_id = ?1 AND retracted_by_record_sequence IS NULL
         ORDER BY source_record_sequence DESC LIMIT 1",
        [obs],
        |r| r.get(0),
    )
    .ok()
}

fn review_state(ctx: &TestContext, obs: &str) -> (String, bool) {
    let conn = ctx.conn();
    conn.query_row(
        "SELECT state, handled FROM observation_review_state WHERE observation_id = ?1",
        [obs],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap()
}

#[test]
fn t3_every_disposition_transitions_state() {
    let ctx = TestContext::new();
    let a = report(&ctx, "disp-a", "bug", "major");
    let b = report(&ctx, "disp-b", "bug", "major");
    let c = report(&ctx, "disp-c", "bug", "major");

    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("confirmed")
        .assert()
        .success();
    assert_eq!(current_disposition(&ctx, &a).unwrap(), "confirmed");
    assert_eq!(review_state(&ctx, &a).0, "confirmed");
    assert!(!review_state(&ctx, &a).1, "confirmed alone is not handled");

    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("duplicate")
        .arg("--of")
        .arg(&b)
        .assert()
        .success();
    assert_eq!(review_state(&ctx, &a).0, "negative_disposition");
    assert!(review_state(&ctx, &a).1);

    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&b)
        .arg("environmental")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&c)
        .arg("expected-behavior")
        .assert()
        .success();
    assert_eq!(current_disposition(&ctx, &c).unwrap(), "expected_behavior");
    assert!(review_state(&ctx, &c).1);
}

#[test]
fn t3_target_required_and_validated() {
    let ctx = TestContext::new();
    let a = report(&ctx, "tgt-a", "bug", "major");
    let b = report(&ctx, "tgt-b", "bug", "major");

    // duplicate requires --of; superseded requires --by.
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("duplicate")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("superseded")
        .output()
        .unwrap();
    assert!(!out.status.success());

    // Target must exist.
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("duplicate")
        .arg("--of")
        .arg("obs_nonexistent")
        .output()
        .unwrap();
    assert!(!out.status.success());

    // Self-target rejected.
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("duplicate")
        .arg("--of")
        .arg(&a)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("own target"), "{err}");

    // --of is invalid for non-target dispositions.
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("confirmed")
        .arg("--of")
        .arg(&b)
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn t3_disposition_cycles_rejected_but_chains_allowed() {
    let ctx = TestContext::new();
    let a = report(&ctx, "cyc-a", "bug", "major");
    let b = report(&ctx, "cyc-b", "bug", "major");
    let c = report(&ctx, "cyc-c", "bug", "major");

    // Chain: a dup-of b, c dup-of a — both valid.
    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("duplicate")
        .arg("--of")
        .arg(&b)
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&c)
        .arg("duplicate")
        .arg("--of")
        .arg(&a)
        .assert()
        .success();

    // b dup-of a closes the a->b->a cycle: rejected.
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&b)
        .arg("duplicate")
        .arg("--of")
        .arg(&a)
        .output()
        .unwrap();
    assert!(!out.status.success(), "b dup-of a must close a cycle");
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("cycle"), "{err}");
}

#[test]
fn t3_idempotency_replays_and_conflicts() {
    let ctx = TestContext::new();
    let a = report(&ctx, "idem-a", "bug", "major");
    let before = record_count(&ctx);

    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("confirmed")
        .arg("--idempotency-key")
        .arg("ik1")
        .assert()
        .success();
    // Identical replay: no new records.
    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("confirmed")
        .arg("--idempotency-key")
        .arg("ik1")
        .assert()
        .success();
    assert_eq!(
        record_count(&ctx),
        before + 2,
        "reviewed + disposition_set, no replay dupes"
    );

    // Conflicting reuse of the key fails.
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("deferred")
        .arg("--idempotency-key")
        .arg("ik1")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("Idempotency conflict"), "{err}");

    // Later adjudication is auditable: the earlier disposition remains in the
    // stream, the current one is the latest.
    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("deferred")
        .assert()
        .success();
    assert_eq!(current_disposition(&ctx, &a).unwrap(), "deferred");
    let conn = ctx.conn();
    let events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE record_type = 'observation_disposition_set' AND entity_id = ?1",
            [&a],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(events, 2, "both dispositions stay in the stream");
}

#[test]
fn t3_reopen_returns_observation_to_queue() {
    let ctx = TestContext::new();
    let a = report(&ctx, "reopen-a", "bug", "major");
    let b = report(&ctx, "reopen-b", "bug", "major");
    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("duplicate")
        .arg("--of")
        .arg(&b)
        .assert()
        .success();
    assert!(review_state(&ctx, &a).1);

    ctx.cmd_as("alice")
        .arg("review")
        .arg("reopen")
        .arg(&a)
        .arg("--rationale")
        .arg("rechecked")
        .assert()
        .success();
    assert_eq!(review_state(&ctx, &a).0, "reopened");
    assert!(!review_state(&ctx, &a).1);
    // Back in the queue.
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("next")
        .output()
        .unwrap();
    assert!(String::from_utf8(out.stdout).unwrap().contains(&a));

    // Reopening with no disposition fails.
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("reopen")
        .arg(&a)
        .output()
        .unwrap();
    assert!(!out.status.success());
}

// ---------------------------------------------------------------------------
// T4: relationships.
// ---------------------------------------------------------------------------

#[test]
fn t4_symmetric_canonical_ordering_and_idempotency() {
    let ctx = TestContext::new();
    let a = report(&ctx, "rel-a", "bug", "major");
    let b = report(&ctx, "rel-b", "bug", "major");

    ctx.cmd_as("alice")
        .arg("review")
        .arg("relate")
        .arg(&a)
        .arg(&b)
        .arg("--relation")
        .arg("same-finding")
        .assert()
        .success();
    // Reverse assertion is the same canonical relationship; idempotent.
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("relate")
        .arg(&b)
        .arg(&a)
        .arg("--relation")
        .arg("same_finding")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("idempotent")
    );

    let conn = ctx.conn();
    let (left, right): (String, String) = conn
        .query_row(
            "SELECT left_observation_id, right_observation_id FROM observation_relationships",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(left, a);
    assert_eq!(right, b);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM observation_relationships", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1, "one canonical row for both assertions");
}

#[test]
fn t4_directional_preservation_and_cycle_rejection() {
    let ctx = TestContext::new();
    let a = report(&ctx, "dir-a", "bug", "major");
    let b = report(&ctx, "dir-b", "bug", "major");
    let c = report(&ctx, "dir-c", "bug", "major");

    // Direction preserved for upstream-cause.
    ctx.cmd_as("alice")
        .arg("review")
        .arg("relate")
        .arg(&a)
        .arg(&b)
        .arg("--relation")
        .arg("upstream-cause")
        .assert()
        .success();
    let conn = ctx.conn();
    let (left, right): (String, String) = conn
        .query_row(
            "SELECT left_observation_id, right_observation_id FROM observation_relationships WHERE relation='upstream_cause'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (left, right),
        (a.clone(), b.clone()),
        "direction must be preserved"
    );

    // duplicate_of chain: c->b, then a->c.
    ctx.cmd_as("alice")
        .arg("review")
        .arg("relate")
        .arg(&c)
        .arg(&b)
        .arg("--relation")
        .arg("duplicate-of")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("relate")
        .arg(&a)
        .arg(&c)
        .arg("--relation")
        .arg("duplicate-of")
        .assert()
        .success();
    // b->a would close c->b->a->c: rejected.
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("relate")
        .arg(&b)
        .arg(&a)
        .arg("--relation")
        .arg("duplicate-of")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "b->a with a->c and c->b closes a duplicate_of cycle"
    );
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("cycle"), "{err}");
}

#[test]
fn t4_retraction_is_append_only() {
    let ctx = TestContext::new();
    let a = report(&ctx, "ret-a", "bug", "major");
    let b = report(&ctx, "ret-b", "bug", "major");
    ctx.cmd_as("alice")
        .arg("review")
        .arg("relate")
        .arg(&a)
        .arg(&b)
        .arg("--relation")
        .arg("related")
        .assert()
        .success();
    let conn = ctx.conn();
    let rel: String = conn
        .query_row(
            "SELECT relationship_id FROM observation_relationships",
            [],
            |r| r.get(0),
        )
        .unwrap();

    ctx.cmd_as("alice")
        .arg("review")
        .arg("unrelate")
        .arg(&rel)
        .arg("--rationale")
        .arg("wrong")
        .assert()
        .success();
    let retracted: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM observation_relationships WHERE retracted_by_record_sequence IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(retracted, 1, "row is marked retracted, not deleted");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM observation_relationships", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1, "no hard delete");
    let events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE record_type = 'observation_relationship_retracted'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(events, 1);

    // Retracting again fails (no live relationship).
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("unrelate")
        .arg(&rel)
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn t4_invalid_endpoints_and_self_rejected() {
    let ctx = TestContext::new();
    let a = report(&ctx, "end-a", "bug", "major");
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("relate")
        .arg(&a)
        .arg("obs_nonexistent")
        .arg("--relation")
        .arg("related")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("relate")
        .arg(&a)
        .arg(&a)
        .arg("--relation")
        .arg("related")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let b = report(&ctx, "end-b", "bug", "major");
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("relate")
        .arg(&a)
        .arg(&b)
        .arg("--relation")
        .arg("nonsense")
        .output()
        .unwrap();
    assert!(!out.status.success());
}

// ---------------------------------------------------------------------------
// T5: remediation lineage.
// ---------------------------------------------------------------------------

#[test]
fn t5_promotion_requires_confirmed_and_links_finding() {
    let ctx = TestContext::new();
    let a = report(&ctx, "prom-a", "bug", "major");

    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("promote")
        .arg(&a)
        .arg("--finding-id")
        .arg("f1")
        .output()
        .unwrap();
    assert!(!out.status.success(), "promote before confirmed must fail");

    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("confirmed")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("promote")
        .arg(&a)
        .arg("--finding-id")
        .arg("f1")
        .assert()
        .success();
    let conn = ctx.conn();
    let finding: String = conn
        .query_row(
            "SELECT target_id FROM remediation_links WHERE observation_id = ?1 AND link_type = 'finding'",
            [&a],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(finding, "f1");
    let (state, _) = review_state(&ctx, &a);
    assert_eq!(state, "promoted");
}

#[test]
fn t5_multiple_tasks_and_commits_and_verification_statuses() {
    let ctx = TestContext::new();
    let a = report(&ctx, "multi", "bug", "major");
    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("confirmed")
        .assert()
        .success();

    ctx.cmd_as("alice")
        .arg("review")
        .arg("attach-task")
        .arg(&a)
        .arg("--task-id")
        .arg("t1")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("attach-task")
        .arg(&a)
        .arg("--task-id")
        .arg("t2")
        .assert()
        .success();
    let conn = ctx.conn();
    let tasks: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM remediation_links WHERE observation_id = ?1 AND link_type = 'task'",
            [&a],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tasks, 2, "multiple task links");
    let (state, _) = review_state(&ctx, &a);
    assert_eq!(state, "remediation_in_progress");

    ctx.cmd_as("alice")
        .arg("review")
        .arg("attach-fix")
        .arg(&a)
        .arg("--commit")
        .arg("sha1")
        .arg("--repo")
        .arg("r1")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("attach-fix")
        .arg(&a)
        .arg("--commit")
        .arg("sha2")
        .arg("--repo")
        .arg("r1")
        .assert()
        .success();
    let commits: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM remediation_links WHERE observation_id = ?1 AND link_type = 'commit'",
            [&a],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(commits, 2, "multiple commit links");
    // Commit alone never verifies: still candidate_fix, not handled.
    let (state, handled) = review_state(&ctx, &a);
    assert_eq!(state, "candidate_fix");
    assert!(!handled, "a commit alone must not imply success");
}

#[test]
fn t5_accepted_verification_verifies_rejected_does_not() {
    let ctx = TestContext::new();
    let a = report(&ctx, "verify-a", "bug", "major");
    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("confirmed")
        .assert()
        .success();

    // Invalid status rejected up front.
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("attach-verification")
        .arg(&a)
        .arg("--receipt")
        .arg("rx")
        .arg("--status")
        .arg("bogus")
        .output()
        .unwrap();
    assert!(!out.status.success());

    ctx.cmd_as("alice")
        .arg("review")
        .arg("attach-fix")
        .arg(&a)
        .arg("--commit")
        .arg("sha1")
        .arg("--repo")
        .arg("r1")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("attach-verification")
        .arg(&a)
        .arg("--receipt")
        .arg("r_rej")
        .arg("--status")
        .arg("rejected")
        .assert()
        .success();
    let (state, handled) = review_state(&ctx, &a);
    assert_eq!(
        state, "candidate_fix",
        "rejected verification keeps remediation open"
    );
    assert!(!handled);

    ctx.cmd_as("alice")
        .arg("review")
        .arg("attach-verification")
        .arg(&a)
        .arg("--receipt")
        .arg("r_acc")
        .arg("--status")
        .arg("accepted")
        .assert()
        .success();
    let (state, handled) = review_state(&ctx, &a);
    assert_eq!(state, "verified_fixed");
    assert!(handled);
}

#[test]
fn t5_reopen_after_verification_is_append_only() {
    let ctx = TestContext::new();
    let a = report(&ctx, "reopen-rem", "bug", "major");
    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("confirmed")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("attach-fix")
        .arg(&a)
        .arg("--commit")
        .arg("sha1")
        .arg("--repo")
        .arg("r1")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("attach-verification")
        .arg(&a)
        .arg("--receipt")
        .arg("r1")
        .arg("--status")
        .arg("accepted")
        .assert()
        .success();
    let (state, handled) = review_state(&ctx, &a);
    assert_eq!(state, "verified_fixed");
    assert!(handled);

    ctx.cmd_as("alice")
        .arg("review")
        .arg("reopen-remediation")
        .arg(&a)
        .arg("--rationale")
        .arg("regression")
        .assert()
        .success();
    let (state, handled) = review_state(&ctx, &a);
    assert_eq!(state, "reopened");
    assert!(!handled, "reopening un-handles");

    // A fresh accepted receipt re-verifies; all events remain in the stream.
    let conn = ctx.conn();
    let events_before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE entity_id = ?1",
            [&a],
            |r| r.get(0),
        )
        .unwrap();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("attach-verification")
        .arg(&a)
        .arg("--receipt")
        .arg("r2")
        .arg("--status")
        .arg("accepted")
        .assert()
        .success();
    let (state, handled) = review_state(&ctx, &a);
    assert_eq!(state, "verified_fixed");
    assert!(handled);
    let events_after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE entity_id = ?1",
            [&a],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(events_after, events_before + 1, "reopening is append-only");
}

#[test]
fn t5_full_verify_waits_for_remediation_failure() {
    let ctx = TestContext::new();
    let observation = report(&ctx, "verify-ordering", "bug", "major");
    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&observation)
        .arg("confirmed")
        .assert()
        .success();

    // Corrupt only the derived remediation projection. Store integrity and
    // the record chain remain valid, so the failure comes after full store
    // verification reaches the remediation replay.
    ctx.conn()
        .execute(
            "UPDATE observation_review_state
             SET commits_json = '[{\"commit_sha\":\"stale\",\"repository_id\":\"repo\"}]'
             WHERE observation_id = ?1",
            [&observation],
        )
        .unwrap();
    let output = ctx.cmd().arg("verify").arg("--full").output().unwrap();

    assert!(
        !output.status.success(),
        "stale remediation state must fail"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("commit lineage mismatch"),
        "failure must identify the remediation projection mismatch: {stderr}"
    );
    assert!(
        !stdout.contains("Full verification passed"),
        "a remediation failure must not emit a success claim: {stdout}"
    );
}

#[test]
fn t5_reopened_negative_disposition_clears_stale_lineage() {
    let ctx = TestContext::new();
    let observation = report(&ctx, "reopened-negative-lineage", "bug", "major");
    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&observation)
        .arg("confirmed")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("attach-fix")
        .arg(&observation)
        .arg("--commit")
        .arg("old-sha")
        .arg("--repo")
        .arg("repo")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("attach-verification")
        .arg(&observation)
        .arg("--receipt")
        .arg("old-receipt")
        .arg("--status")
        .arg("accepted")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("reopen-remediation")
        .arg(&observation)
        .arg("--rationale")
        .arg("recheck")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&observation)
        .arg("expected_behavior")
        .assert()
        .success();

    let (commits, receipts, latest): (String, String, Option<String>) = ctx
        .conn()
        .query_row(
            "SELECT commits_json, verification_receipts_json,
                    latest_verification_status
             FROM observation_review_state
             WHERE observation_id = ?1",
            [&observation],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(commits, "[]");
    assert_eq!(receipts, "[]");
    assert_eq!(latest, None);

    let output = ctx.cmd().arg("verify").arg("--full").output().unwrap();
    assert!(output.status.success(), "reopened sequence must verify");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout.matches("Full verification passed").count(),
        1,
        "healthy full verification emits one success line: {stdout}"
    );
}

#[test]
fn t5_mark_handled_rules() {
    let ctx = TestContext::new();
    let a = report(&ctx, "mh-a", "bug", "major");
    // No disposition, no evidence: refused.
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("mark-handled")
        .arg(&a)
        .output()
        .unwrap();
    assert!(!out.status.success());

    // Confirmed with no task/commit/verification: refused.
    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("confirmed")
        .assert()
        .success();
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("mark-handled")
        .arg(&a)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("task link"), "{err}");

    // With a task link: allowed.
    ctx.cmd_as("alice")
        .arg("review")
        .arg("attach-task")
        .arg(&a)
        .arg("--task-id")
        .arg("t1")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("mark-handled")
        .arg(&a)
        .assert()
        .success();
    assert!(review_state(&ctx, &a).1);

    // Reopen-remediation on a non-handled observation fails.
    let out = ctx
        .cmd_as("alice")
        .arg("review")
        .arg("reopen-remediation")
        .arg(report(&ctx, "mh-b", "bug", "major"))
        .output()
        .unwrap();
    assert!(!out.status.success());
}

// ---------------------------------------------------------------------------
// T6: negative disposition workflow (handled without patch).
// ---------------------------------------------------------------------------

#[test]
fn t6_negative_dispositions_handle_without_task_or_patch() {
    let ctx = TestContext::new();
    let mut obs = Vec::new();
    for (title, disp, flag, arg) in [
        ("n1", "duplicate", "--of", "n_target"),
        ("n2", "environmental", "", ""),
        ("n3", "expected-behavior", "", ""),
        ("n4", "insufficient-evidence", "", ""),
    ] {
        let a = report(&ctx, title, "bug", "major");
        let mut cmd = ctx.cmd_as("alice");
        cmd.arg("review").arg("disposition").arg(&a).arg(disp);
        if !flag.is_empty() {
            let target = report(&ctx, arg, "bug", "major");
            cmd.arg(flag).arg(&target);
            obs.push(target);
        }
        cmd.assert().success();
        let (state, handled) = review_state(&ctx, &a);
        assert_eq!(state, "negative_disposition", "{title}");
        assert!(handled, "{title}: negative disposition is handled");
        // mark-handled succeeds without any task or patch.
        ctx.cmd_as("alice")
            .arg("review")
            .arg("mark-handled")
            .arg(&a)
            .assert()
            .success();
        obs.push(a);
    }
    // The duplicate target is also adjudicated so nothing remains queued.
    for t in obs.iter().filter(|t| ctx.conn().query_row("SELECT COUNT(*) FROM observation_dispositions WHERE observation_id = ?1 AND retracted_by_record_sequence IS NULL", [t], |r| r.get::<_, i64>(0)).unwrap() == 0) {
        ctx.cmd_as("alice").arg("review").arg("disposition").arg(t).arg("environmental").assert().success();
    }
    // Every negative disposition leaves the default queue; none can be
    // pulled by next (even by another session).
    let out = ctx
        .cmd_as("bob")
        .arg("review")
        .arg("next")
        .output()
        .unwrap();
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("empty queue")
    );
    for a in obs {
        let (state, handled) = review_state(&ctx, &a);
        assert_eq!(state, "negative_disposition");
        assert!(handled);
    }
}

// ---------------------------------------------------------------------------
// T8: export/rebuild round-trip.
// ---------------------------------------------------------------------------

#[test]
fn t8_export_rebuild_preserves_all_remediation_state() {
    let ctx = TestContext::new();
    let a = report(&ctx, "exp-a", "bug", "major");
    let b = report(&ctx, "exp-b", "bug", "major");
    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("confirmed")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("attach-task")
        .arg(&a)
        .arg("--task-id")
        .arg("t1")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("attach-fix")
        .arg(&a)
        .arg("--commit")
        .arg("sha1")
        .arg("--repo")
        .arg("r1")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("attach-verification")
        .arg(&a)
        .arg("--receipt")
        .arg("rec1")
        .arg("--status")
        .arg("accepted")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("relate")
        .arg(&a)
        .arg(&b)
        .arg("--relation")
        .arg("upstream-cause")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("claim")
        .arg(&b)
        .arg("--task")
        .arg("t2")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("assign-owner")
        .arg(&b)
        .arg("repo_owner")
        .assert()
        .success();

    // Snapshot the materialized state before the round trip.
    let conn = ctx.conn();
    let state_before: Vec<(String, String, i64)> = {
        let mut stmt = conn
            .prepare("SELECT observation_id, state, handled FROM observation_review_state ORDER BY observation_id")
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        let mut v = Vec::new();
        while let Some(r) = rows.next().unwrap() {
            v.push((r.get(0).unwrap(), r.get(1).unwrap(), r.get(2).unwrap()));
        }
        v
    };
    let owners_before: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT observation_id, repository_id
                 FROM observation_repositories
                 WHERE role = 'owner'
                 ORDER BY observation_id, repository_id",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };

    let export_path = ctx.home_dir.path().join("export.jsonl");
    ctx.cmd()
        .arg("export")
        .arg("--output")
        .arg(&export_path)
        .assert()
        .success();
    let header: serde_json::Value = serde_json::from_str(
        std::fs::read_to_string(&export_path)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        header["minimum_reader_version"], 3,
        "owner assignment requires a reader that understands its event"
    );
    let rebuilt = ctx.home_dir.path().join("rebuilt");
    // Rebuild's destination is a data dir (the store lands at
    // <destination>/snag.sqlite), so pointing it at the XDG store dir makes
    // the rebuilt store addressable with XDG_DATA_HOME=rebuilt.
    let dest = rebuilt.join("snag");
    ctx.cmd()
        .arg("rebuild")
        .arg("--from-export")
        .arg(&export_path)
        .arg("--destination")
        .arg(&dest)
        .assert()
        .success();

    // The rebuilt store passes full verification (chain + remediation checks).
    ctx.cmd()
        .env("XDG_DATA_HOME", &rebuilt)
        .arg("verify")
        .arg("--full")
        .assert()
        .success();

    let conn2 = Connection::open(dest.join("snag.sqlite")).unwrap();
    let state_after: Vec<(String, String, i64)> = {
        let mut stmt = conn2
            .prepare("SELECT observation_id, state, handled FROM observation_review_state ORDER BY observation_id")
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        let mut v = Vec::new();
        while let Some(r) = rows.next().unwrap() {
            v.push((r.get(0).unwrap(), r.get(1).unwrap(), r.get(2).unwrap()));
        }
        v
    };
    assert_eq!(
        state_before, state_after,
        "review state must survive rebuild"
    );
    let owners_after: Vec<(String, String)> = {
        let mut stmt = conn2
            .prepare(
                "SELECT observation_id, repository_id
                 FROM observation_repositories
                 WHERE role = 'owner'
                 ORDER BY observation_id, repository_id",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(
        owners_before, owners_after,
        "owner projection must survive rebuild"
    );

    // Event history is complete.
    let events: i64 = conn2
        .query_row("SELECT COUNT(*) FROM records WHERE record_type LIKE 'observation_%' OR record_type LIKE 'remediation_%'", [], |r| r.get(0))
        .unwrap();
    assert!(
        events >= 8,
        "remediation history must round-trip, got {events}"
    );

    // Relationships and links survive.
    let rels: i64 = conn2
        .query_row("SELECT COUNT(*) FROM observation_relationships", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rels, 1);
    let links: i64 = conn2
        .query_row("SELECT COUNT(*) FROM remediation_links", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        links, 4,
        "finding? no — task + commit + verification + claim-time task"
    );
}

// ---------------------------------------------------------------------------
// T9: backup/restore.
// ---------------------------------------------------------------------------

#[test]
fn t9_backup_restore_preserves_remediation_state() {
    let ctx = TestContext::new();
    let a = report(&ctx, "bk-a", "bug", "major");
    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("confirmed")
        .assert()
        .success();
    ctx.cmd_as("alice")
        .arg("review")
        .arg("attach-verification")
        .arg(&a)
        .arg("--receipt")
        .arg("rec1")
        .arg("--status")
        .arg("accepted")
        .assert()
        .success();

    let out = ctx.cmd().arg("backup").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let archive = stdout
        .lines()
        .find_map(|l| l.split("saved to: ").nth(1))
        .map(|p| p.trim().to_string())
        .expect("backup path in stdout");

    // Destroy the store, then restore from the archive.
    let db = ctx.data_dir.join("snag.sqlite");
    std::fs::remove_file(&db).unwrap();
    ctx.cmd().arg("restore").arg(&archive).assert().success();

    ctx.cmd().arg("verify").arg("--full").assert().success();
    let conn = ctx.conn();
    let (state, handled): (String, i64) = conn
        .query_row(
            "SELECT state, handled FROM observation_review_state WHERE observation_id = ?1",
            [&a],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "verified_fixed");
    assert_eq!(handled, 1);
    let events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM records WHERE entity_id = ?1",
            [&a],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        events, 4,
        "created + reviewed + disposition_set + verification"
    );
}

// ---------------------------------------------------------------------------
// T7: crash injection (zero-visible-event | one-complete-event outcomes).
// ---------------------------------------------------------------------------

/// Run a remediation command with a failpoint; return whether it succeeded.
fn run_with_failpoint(ctx: &TestContext, failpoint: &str, args: &[&str]) -> bool {
    let mut child = Proc::new(ctx.bin());
    let out = child
        .args(args)
        .env("XDG_DATA_HOME", ctx.home_dir.path())
        .env("HOME", ctx.home_dir.path())
        .env("SNAG_REVIEWER_ID", "rev_alice")
        .env("SNAG_REVIEW_SESSION_ID", "sess_alice")
        .env("SNAG_FAILPOINT", failpoint)
        .output()
        .unwrap();
    out.status.success()
}

fn disposition_event_count(ctx: &TestContext, obs: &str) -> i64 {
    let conn = ctx.conn();
    conn.query_row(
        "SELECT COUNT(*) FROM records WHERE entity_id = ?1 AND record_type IN ('observation_reviewed','observation_disposition_set')",
        [obs],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn t7_crash_before_commit_leaves_no_visible_disposition() {
    let ctx = TestContext::new();
    let a = report(&ctx, "crash-disp", "bug", "major");
    let before = record_count(&ctx);

    assert!(!run_with_failpoint(
        &ctx,
        "remediation_before_commit",
        &["review", "disposition", &a, "confirmed"]
    ));
    assert_eq!(
        record_count(&ctx),
        before,
        "zero visible events before commit"
    );
    assert_eq!(disposition_event_count(&ctx, &a), 0);
    assert_eq!(
        ctx.conn()
            .query_row("SELECT COUNT(*) FROM observation_dispositions", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        0
    );

    // After the crash the same submission succeeds completely.
    ctx.cmd_as("alice")
        .arg("review")
        .arg("disposition")
        .arg(&a)
        .arg("confirmed")
        .assert()
        .success();
    assert_eq!(
        disposition_event_count(&ctx, &a),
        2,
        "reviewed + disposition_set"
    );
    assert_eq!(
        ctx.conn()
            .query_row("SELECT COUNT(*) FROM observation_dispositions", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        1
    );
}

#[test]
fn t7_crash_after_event_insert_rolls_back_completely() {
    let ctx = TestContext::new();
    let a = report(&ctx, "crash-insert", "bug", "major");
    let before = record_count(&ctx);

    assert!(!run_with_failpoint(
        &ctx,
        "remediation_after_event_insert",
        &["review", "disposition", &a, "confirmed"]
    ));
    // The event row was inserted but the tx rolled back: zero visible events.
    assert_eq!(
        record_count(&ctx),
        before,
        "event insert + rollback leaves nothing"
    );
    assert_eq!(disposition_event_count(&ctx, &a), 0);
}

#[test]
fn t7_crash_after_commit_leaves_one_complete_event() {
    let ctx = TestContext::new();
    let a = report(&ctx, "crash-commit", "bug", "major");

    // Abort after commit: the disposition is fully visible (one complete event
    // pair), not a half-state.
    assert!(!run_with_failpoint(
        &ctx,
        "remediation_after_commit",
        &["review", "disposition", &a, "confirmed"]
    ));
    assert_eq!(disposition_event_count(&ctx, &a), 2);
    let conn = ctx.conn();
    let state: String = conn
        .query_row(
            "SELECT state FROM observation_review_state WHERE observation_id = ?1",
            [&a],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "confirmed");
    // The store still verifies end to end.
    ctx.cmd().arg("verify").arg("--full").assert().success();
}

// ---------------------------------------------------------------------------

#[test]
fn verify_rejects_initial_owner_projection_drift() {
    let ctx = TestContext::new();
    let observation = report_owned(&ctx, "initial owner drift", "bug", "major", "repo_initial");
    report_owned(
        &ctx,
        "initial owner target",
        "bug",
        "minor",
        "repo_tampered",
    );
    let conn = ctx.conn();
    conn.execute(
        "UPDATE observation_repositories
         SET repository_id = 'repo_tampered'
         WHERE observation_id = ?1 AND role = 'owner'",
        [&observation],
    )
    .unwrap();
    drop(conn);

    let out = ctx.cmd().arg("verify").arg("--full").output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("owner mismatch"), "{err}");
}

#[test]
fn verify_rejects_projected_owner_drift() {
    let ctx = TestContext::new();
    let observation = report_owned(
        &ctx,
        "projected owner drift",
        "bug",
        "major",
        "repo_initial",
    );
    report_owned(
        &ctx,
        "projected owner target",
        "bug",
        "minor",
        "repo_second",
    );
    ctx.cmd_as("alice")
        .arg("review")
        .arg("assign-owner")
        .arg(&observation)
        .arg("repo_second")
        .assert()
        .success();

    let conn = ctx.conn();
    conn.execute(
        "UPDATE observation_repositories
         SET repository_id = 'repo_initial'
         WHERE observation_id = ?1 AND role = 'owner'",
        [&observation],
    )
    .unwrap();
    drop(conn);

    let out = ctx.cmd().arg("verify").arg("--full").output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("owner mismatch"), "{err}");
}
