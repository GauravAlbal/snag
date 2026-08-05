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
    fn bin(&self) -> PathBuf {
        assert_cmd::cargo::cargo_bin("snag")
    }
}

fn obs_count(ctx: &TestContext) -> i64 {
    let conn = Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
    conn.query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
        .unwrap()
}
fn max_seq(ctx: &TestContext) -> i64 {
    let conn = Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
    conn.query_row(
        "SELECT COALESCE(MAX(local_sequence),0) FROM records",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

/// T5: 32 concurrent subprocess writers. Every successful report exists exactly
/// once, sequences are contiguous, the chain verifies, and no corruption.
#[test]
fn test_32_concurrent_writers() {
    let ctx = TestContext::new();
    let bin = ctx.bin();

    let mut children = Vec::new();
    for i in 0..32 {
        let mut c = Proc::new(&bin);
        c.arg("report")
            .arg(format!("concurrent-{}", i))
            .env("XDG_DATA_HOME", ctx.home_dir.path())
            .env("HOME", ctx.home_dir.path());
        children.push(c.spawn().unwrap());
    }
    for mut c in children {
        let status = c.wait().unwrap();
        assert!(status.success(), "a concurrent writer failed");
    }

    assert_eq!(
        obs_count(&ctx),
        32,
        "every successful report must exist exactly once"
    );
    assert_eq!(
        max_seq(&ctx),
        32,
        "global sequence must be exactly contiguous"
    );
    ctx.cmd().arg("verify").arg("--full").assert().success();
}

/// T5 (idempotency): concurrent same-key writers create exactly ONE observation.
#[test]
fn test_concurrent_same_key_single_observation() {
    let ctx = TestContext::new();
    let bin = ctx.bin();

    let mut children = Vec::new();
    for _ in 0..16 {
        let mut c = Proc::new(&bin);
        c.arg("report")
            .arg("same idempotent report")
            .arg("--idempotency-key")
            .arg("concurrent_shared")
            .env("XDG_DATA_HOME", ctx.home_dir.path())
            .env("HOME", ctx.home_dir.path());
        children.push(c.spawn().unwrap());
    }
    for mut c in children {
        let _ = c.wait();
    }
    assert_eq!(
        obs_count(&ctx),
        1,
        "concurrent same-key must yield one observation"
    );
    let conn = Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
    let records: i64 = conn
        .query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0))
        .unwrap();
    assert_eq!(records, 1, "concurrent same-key must yield one record");
    ctx.cmd().arg("verify").arg("--full").assert().success();
}

/// T7: artifacts are content-addressed (dedup by content) and symlinks are
/// rejected.
#[test]
fn test_artifact_dedup_and_symlink_rejection() {
    let ctx = TestContext::new();

    let f1 = ctx.home_dir.path().join("a.txt");
    let f2 = ctx.home_dir.path().join("b.txt");
    std::fs::write(&f1, b"identical bytes here").unwrap();
    std::fs::write(&f2, b"identical bytes here").unwrap();

    ctx.cmd()
        .arg("report")
        .arg("two same artifacts")
        .arg("--artifact")
        .arg(&f1)
        .arg("--artifact")
        .arg(&f2)
        .assert()
        .success();

    let conn = Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
    let objects: i64 = conn
        .query_row("SELECT COUNT(*) FROM artifacts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        objects, 1,
        "identical content must deduplicate to one object"
    );

    // Symlink rejection.
    let target = ctx.home_dir.path().join("real.txt");
    std::fs::write(&target, b"data").unwrap();
    let link = ctx.home_dir.path().join("link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    ctx.cmd()
        .arg("report")
        .arg("symlink")
        .arg("--artifact")
        .arg(&link)
        .assert()
        .failure()
        .stderr(predicates::str::contains("symlink"));
}

/// T6: crash injection via SNAG_FAILPOINT. A report that aborts anywhere inside
/// the transaction leaves either NO observation or the one complete, committed
/// observation — never a partial record. After abort, the store must verify.
#[test]
fn test_crash_injection_report_failpoints() {
    let ctx = TestContext::new();
    let bin = ctx.bin();

    let stages = [
        "before_tx",
        "after_seq",
        "after_record_insert",
        "after_obs_insert",
        "after_artifacts",
        "after_commit",
    ];
    for stage in stages {
        let mut c = Proc::new(&bin);
        c.arg("report")
            .arg(format!("crash-{}", stage))
            .env("XDG_DATA_HOME", ctx.home_dir.path())
            .env("HOME", ctx.home_dir.path())
            .env("SNAG_FAILPOINT", stage);
        let status = c.status().unwrap();
        assert!(
            !status.success(),
            "failpoint {stage} should abort the process"
        );

        let conn = Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
        let n_records: i64 = conn
            .query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0))
            .unwrap();
        // Only an after_commit crash persists exactly one complete record.
        assert!(
            (stage == "after_commit" && n_records == 1)
                || (stage != "after_commit" && n_records == 0),
            "stage {stage}: expected {} records, got {n_records}",
            if stage == "after_commit" { 1 } else { 0 }
        );
        if n_records == 1 {
            // The committed observation must be complete and verifiable.
            ctx.cmd().arg("verify").arg("--full").assert().success();
        }
    }
}
