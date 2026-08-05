use assert_cmd::Command;
use predicates::prelude::*;
use std::env;
use tempfile::TempDir;

/// Provides an isolated environment for a snag test instance
pub struct TestContext {
    pub home_dir: TempDir,
    pub data_dir: std::path::PathBuf,
}

impl Default for TestContext {
    fn default() -> Self {
        Self::new()
    }
}

impl TestContext {
    pub fn new() -> Self {
        let home_dir = tempfile::tempdir().unwrap();
        // Since snag relies on ProjectDirs, we can mock XDG_DATA_HOME or standard paths
        unsafe {
            env::set_var("XDG_DATA_HOME", home_dir.path());
            env::set_var("HOME", home_dir.path());
        }

        let data_dir = home_dir.path().join("snag");

        Self { home_dir, data_dir }
    }

    pub fn cmd(&self) -> Command {
        let mut cmd = Command::cargo_bin("snag").unwrap();
        cmd.env("XDG_DATA_HOME", self.home_dir.path())
            .env("HOME", self.home_dir.path());
        cmd
    }
}

impl Drop for TestContext {
    fn drop(&mut self) {
        unsafe {
            env::remove_var("XDG_DATA_HOME");
            env::remove_var("HOME");
        }
    }
}

#[test]
fn test_bare_fast_path() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
        .arg("Bare fast path test")
        .assert()
        .success()
        .stdout(predicate::str::contains("Recorded obs_"));
    assert_eq!(
        obs_count(&ctx),
        1,
        "bare fast path must persist exactly one observation"
    );
}

#[test]
fn test_structured_cli_report() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
        .arg("Test structured CLI")
        .arg("--kind")
        .arg("bug")
        .arg("--severity")
        .arg("major")
        .arg("--expected")
        .arg("Works")
        .arg("--observed")
        .arg("Fails")
        .assert()
        .success()
        .stdout(predicate::str::contains("Recorded obs_"));
    assert_eq!(
        obs_count(&ctx),
        1,
        "structured CLI report must persist one observation"
    );
}

#[test]
fn test_list_filters_gap() {
    // Create a specific observation
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
        .arg("List filter test")
        .arg("--kind")
        .arg("security")
        .assert()
        .success();

    // It filters out correctly
    ctx.cmd()
        .arg("list")
        .arg("--kind")
        .arg("security")
        .assert()
        .success()
        .stdout(predicate::str::contains("List filter test"));

    ctx.cmd()
        .arg("list")
        .arg("--kind")
        .arg("bug")
        .assert()
        .success()
        .stdout(predicate::str::contains("List filter test").not());
    assert_eq!(
        obs_count(&ctx),
        1,
        "one observation must exist for list filtering"
    );
}

// ==========================================
// Failing tests for known gaps (G1 - G19)
// ==========================================

#[test]
fn test_json_intake_gap() {
    // Gap G1: JSON intake is not fully implemented
    let ctx = TestContext::new();
    let json_payload = r#"{
        "schema_version": 1,
        "title": "JSON Intake Test",
        "kind_assertion": "reliability"
    }"#;

    ctx.cmd()
        .arg("report")
        .arg("--json")
        .write_stdin(json_payload)
        .assert()
        .success();
    assert_eq!(
        obs_count(&ctx),
        1,
        "JSON intake must persist one observation"
    );
}

#[test]
fn test_idempotency_gap() {
    // Gap G3: Idempotency is not implemented (currently it just allows it)
    let ctx = TestContext::new();

    // First insertion
    ctx.cmd()
        .arg("report")
        .arg("Idempotency Test")
        .arg("--idempotency-key")
        .arg("key_123")
        .assert()
        .success();

    // Same payload should succeed returning created=false
    ctx.cmd()
        .arg("report")
        .arg("Idempotency Test")
        .arg("--idempotency-key")
        .arg("key_123")
        .assert()
        .success()
        .stdout(predicate::str::contains("Observation already exists"));

    // 3. Different payload with same key -> fails with IDEMPOTENCY_CONFLICT
    ctx.cmd()
        .arg("report")
        .arg("Idempotency Test DIFFERENT")
        .arg("--idempotency-key")
        .arg("key_123")
        .assert()
        .failure()
        .stderr(predicate::str::contains("different semantic payload"));
    assert_eq!(
        obs_count(&ctx),
        1,
        "idempotency replay must not create a second observation"
    );
}

#[test]
fn test_certification_mission() {
    let ctx = TestContext::new();

    // 1. Encounter a problem and record it in one command
    ctx.cmd()
        .arg("report")
        .arg("System crashed during start")
        .arg("--kind")
        .arg("reliability")
        .arg("--idempotency-key")
        .arg("cert_123")
        .assert()
        .success()
        .stdout(predicate::str::contains("Recorded obs_"));

    // 2. "crash immediately afterward" -> CLI naturally terminates.

    // 3. Later prove whether it was durably captured
    // using verify
    ctx.cmd()
        .arg("verify")
        .arg("--full")
        .assert()
        .success()
        .stdout(predicate::str::contains("Full verification passed"));

    // using list
    ctx.cmd()
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("System crashed during start"));

    // using backup
    ctx.cmd()
        .arg("backup")
        .assert()
        .success()
        .stdout(predicate::str::contains("Backup verified and saved"));
    assert_eq!(
        obs_count(&ctx),
        1,
        "certification mission must persist exactly one observation"
    );
}

#[test]
fn test_metadata_tamper() {
    let ctx = TestContext::new();

    // 1. Create a record
    ctx.cmd()
        .arg("report")
        .arg("Tamper Test")
        .assert()
        .success();

    // 2. Tamper with the sqlite DB directly
    let db_path = ctx.data_dir.join("snag.sqlite");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "UPDATE records SET captured_at = '2000-01-01T00:00:00Z' WHERE local_sequence = 1",
        [],
    )
    .unwrap();

    // 3. Verify should fail
    ctx.cmd()
        .arg("verify")
        .arg("--full")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Hash chain mismatch"));
    assert_eq!(
        obs_count(&ctx),
        1,
        "tampered record must still be one observation"
    );
}

#[test]
fn test_export_protocol() {
    let ctx = TestContext::new();

    ctx.cmd()
        .arg("report")
        .arg("Export Test 1")
        .assert()
        .success();

    let out_path = ctx.home_dir.path().join("export.jsonl");
    ctx.cmd()
        .arg("export")
        .arg("--output")
        .arg(&out_path)
        .assert()
        .success();

    let content = std::fs::read_to_string(&out_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert!(lines.len() >= 2);

    let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(header["export_kind"], "export_header");

    let record: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(record["export_kind"], "record");
}

#[test]
fn test_rebuild_protocol() {
    let ctx = TestContext::new();

    ctx.cmd()
        .arg("report")
        .arg("Rebuild Test 1")
        .assert()
        .success();

    let out_path = ctx.home_dir.path().join("export.jsonl");
    ctx.cmd()
        .arg("export")
        .arg("--output")
        .arg(&out_path)
        .assert()
        .success();

    let rebuild_dest = ctx.home_dir.path().join("rebuilt_snag");
    ctx.cmd()
        .arg("rebuild")
        .arg("--from-export")
        .arg(&out_path)
        .arg("--destination")
        .arg(&rebuild_dest)
        .assert()
        .success();

    // Verify the rebuilt DB
    let rebuilt_db = rebuild_dest.join("snag.sqlite");
    assert!(rebuilt_db.exists());
    let conn = rusqlite::Connection::open(&rebuilt_db).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn test_restore_protocol() {
    let ctx = TestContext::new();

    // Create record
    ctx.cmd()
        .arg("report")
        .arg("Restore Test")
        .assert()
        .success();

    // Backup
    ctx.cmd().arg("backup").assert().success();

    // Find the backup archive
    let backups_dir = ctx.data_dir.join("backups");
    let mut backup_archive = None;
    for entry in std::fs::read_dir(&backups_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "gz") {
            backup_archive = Some(path);
            break;
        }
    }
    let archive_path = backup_archive.expect("Backup archive not found");

    // Try restoring when active store is non-empty -> should fail
    ctx.cmd()
        .arg("restore")
        .arg(&archive_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("non-empty"));

    // Delete store file directly
    let _ = std::fs::remove_file(ctx.data_dir.join("snag.sqlite"));

    // Restore
    ctx.cmd()
        .arg("restore")
        .arg(&archive_path)
        .assert()
        .success();

    // Verify
    ctx.cmd().arg("verify").arg("--full").assert().success();
    assert_eq!(
        obs_count(&ctx),
        1,
        "restored store must contain the original observation"
    );
}

fn latest_backup(ctx: &TestContext) -> std::path::PathBuf {
    let backups_dir = ctx.data_dir.join("backups");
    let mut archive_path = None;
    for entry in std::fs::read_dir(&backups_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "gz")
            && (archive_path.is_none() || path > archive_path.clone().unwrap())
        {
            archive_path = Some(path);
        }
    }
    archive_path.expect("Backup archive not found")
}

fn obs_count(ctx: &TestContext) -> i64 {
    let conn = rusqlite::Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
    conn.query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
        .unwrap()
}

/// End-to-end P0 chain: report with an artifact, verify the live store,
/// backup, independently verify the backup archive, verify that a tampered
/// bundle is rejected, then restore the archive into a fresh store.
#[test]
fn test_e2e_backup_restore_roundtrip() {
    let ctx = TestContext::new();

    // A real artifact file in a git-worktree-like temp location.
    let art = ctx.home_dir.path().join("evidence.txt");
    std::fs::write(&art, b"THE QUICK BROWN FOX JUMPS OVER THE LAZY DOG\n").unwrap();

    ctx.cmd()
        .arg("report")
        .arg("E2E roundtrip")
        .arg("--artifact")
        .arg(&art)
        .arg("--idempotency-key")
        .arg("e2e_1")
        .assert()
        .success();

    // Live full verification.
    ctx.cmd().arg("verify").arg("--full").assert().success();

    // Backup.
    ctx.cmd().arg("backup").assert().success();
    let archive = latest_backup(&ctx);

    // Independent backup verification of the archive.
    ctx.cmd()
        .arg("verify")
        .arg("--backup")
        .arg(&archive)
        .assert()
        .success();

    // Tamper with the archive: verify must fail.
    {
        let t = ctx
            .home_dir
            .path()
            .join(format!("tampered_{}.tar.gz", ulid::Ulid::generate()));
        std::fs::copy(&archive, &t).unwrap();
        // Flip a byte in the middle of the archive to corrupt it.
        let bytes = std::fs::read(&t).unwrap();
        let mut b = bytes.clone();
        let idx = b.len() / 2;
        b[idx] ^= 0xFF;
        std::fs::write(&t, b).unwrap();
        ctx.cmd()
            .arg("verify")
            .arg("--backup")
            .arg(&t)
            .assert()
            .failure();
        std::fs::remove_file(&t).unwrap();
    }

    // Restore into a FRESH store (empty) — non-destructive path.
    let ctx2 = TestContext::new();
    ctx2.cmd().arg("restore").arg(&archive).assert().success();
    ctx2.cmd().arg("verify").arg("--full").assert().success();

    // The restored observation + artifact are present and correct.
    ctx2.cmd()
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("E2E roundtrip"));

    // Artifact bytes survive in the restored objects store.
    let restored_db = rusqlite::Connection::open(ctx2.data_dir.join("snag.sqlite")).unwrap();
    let digest: String = restored_db
        .query_row("SELECT digest FROM artifacts LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let hex = digest.strip_prefix("blake3:").unwrap();
    let obj = ctx2
        .data_dir
        .join("objects/blake3")
        .join(&hex[0..2])
        .join(hex);
    assert_eq!(
        std::fs::read(&obj).unwrap(),
        b"THE QUICK BROWN FOX JUMPS OVER THE LAZY DOG\n"
    );
}

/// G31: JSON intake persists all supported fields, and JSON idempotency replay
/// returns the original observation (G32).
#[test]
fn test_json_full_intake_and_replay() {
    let ctx = TestContext::new();

    let json_payload = r#"{
        "schema_version": 1,
        "title": "JSON Full Intake",
        "summary": "a summary",
        "kind_assertion": "reliability",
        "severity_assertion": "major",
        "expected_behavior": "expected works",
        "observed_behavior": "observed fails",
        "reproduction": "steps",
        "workaround": "none",
        "impact": "prod down",
        "confidence": 0.85,
        "sensitivity": "sensitive",
        "labels": { "area": "core", "tier": "2" },
        "idempotency_key": "json_key_1"
    }"#;

    ctx.cmd()
        .arg("report")
        .arg("--json")
        .write_stdin(json_payload)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"idempotent_replay\": true").not());

    let conn = rusqlite::Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
    let row: (String, String, f64, String, String) = conn.query_row(
        "SELECT title, kind_assertion, confidence, sensitivity, labels_json FROM observations WHERE idempotency_key='json_key_1'",
        [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4).unwrap_or_default())),
    ).unwrap();
    let labels: serde_json::Value = serde_json::from_str(&row.4).unwrap();
    assert_eq!(row.0, "JSON Full Intake");
    assert_eq!(row.1, "reliability");
    assert_eq!(row.2, 0.85);
    assert_eq!(row.3, "sensitive");
    assert_eq!(labels["area"], "core");

    // Replay with identical JSON -> idempotent_replay=true, no new row.
    ctx.cmd()
        .arg("report")
        .arg("--json")
        .write_stdin(json_payload)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"idempotent_replay\": true"));

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM observations WHERE idempotency_key='json_key_1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

/// G32: same key + different semantic payload -> typed IDEMPOTENCY_CONFLICT.
#[test]
fn test_idempotency_conflict_typed() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
        .arg("original")
        .arg("--idempotency-key")
        .arg("k2")
        .assert()
        .success();
    ctx.cmd()
        .arg("report")
        .arg("different payload")
        .arg("--idempotency-key")
        .arg("k2")
        .assert()
        .failure()
        .stderr(predicate::str::contains("different semantic payload"));
    assert_eq!(
        obs_count(&ctx),
        1,
        "conflicting replay must not create a second observation"
    );
}

/// G36: list --since, --format json (versioned envelope), --limit, retracted
/// state, and invalid --format failure.
#[test]
fn test_list_since_json_retracted() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
        .arg("recent thing")
        .assert()
        .success();

    let recent: String = ctx
        .cmd()
        .arg("list")
        .arg("--format")
        .arg("json")
        .output()
        .unwrap()
        .stdout
        .into_iter()
        .map(|b| b as char)
        .collect();
    let v: serde_json::Value = serde_json::from_str(&recent).unwrap();
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["count"], 1);
    assert_eq!(v["observations"][0]["title"], "recent thing");
    assert_eq!(v["observations"][0]["retracted"], false);

    // --since 7d includes it; --since 0s excludes it.
    ctx.cmd()
        .arg("list")
        .arg("--since")
        .arg("7d")
        .assert()
        .success()
        .stdout(predicate::str::contains("recent thing"));
    ctx.cmd()
        .arg("list")
        .arg("--since")
        .arg("0s")
        .assert()
        .success()
        .stdout(predicate::str::contains("recent thing").not());

    // Invalid duration -> typed failure.
    ctx.cmd()
        .arg("list")
        .arg("--since")
        .arg("bogus")
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid --since"));

    // Invalid format -> typed failure.
    ctx.cmd()
        .arg("list")
        .arg("--format")
        .arg("xml")
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid --format"));

    // Retracted state is surfaced.
    let id: String = {
        let conn = rusqlite::Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
        conn.query_row("SELECT observation_id FROM observations LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap()
    };
    ctx.cmd().arg("retract").arg(&id).assert().success();
    ctx.cmd()
        .arg("list")
        .arg("--format")
        .arg("json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"retracted\": true"));
}

/// G37: read-only commands (list/show/context/export/verify/doctor) must not
/// mutate sequence, migrations, store metadata, backup checkpoints, or the DB.
#[test]
fn test_read_purity() {
    let ctx = TestContext::new();
    ctx.cmd().arg("report").arg("purity").assert().success();

    let db_path = ctx.data_dir.join("snag.sqlite");
    let before = std::fs::read(&db_path).unwrap();
    let seq_before: i64 = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row(
            "SELECT COALESCE(MAX(local_sequence),0) FROM records",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };

    ctx.cmd().arg("list").assert().success();
    ctx.cmd().arg("show").arg("x").assert().failure(); // not found, but must not mutate
    let _ = ctx
        .cmd()
        .arg("context")
        .arg("--format")
        .arg("json")
        .assert()
        .success();
    let out_path = ctx.home_dir.path().join("purity-export.jsonl");
    ctx.cmd()
        .arg("export")
        .arg("--output")
        .arg(&out_path)
        .assert()
        .success();
    ctx.cmd().arg("verify").arg("--full").assert().success();
    ctx.cmd().arg("doctor").assert().success();

    let after = std::fs::read(&db_path).unwrap();
    let seq_after: i64 = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row(
            "SELECT COALESCE(MAX(local_sequence),0) FROM records",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        seq_before, seq_after,
        "read commands must not advance the sequence"
    );
    assert_eq!(
        before, after,
        "read commands must not mutate the database file"
    );
}

/// Dogfood finding (fixed): `report "<title>" --json` must treat the title
/// as the observation title and emit a JSON response, not misread the title
/// as a JSON intake file path.
#[test]
fn test_report_json_output_mode_with_title() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
        .arg("JSON output mode test")
        .arg("--json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"schema_version\": 1"))
        .stdout(predicate::str::contains("\"observation_id\""));
    assert_eq!(
        obs_count(&ctx),
        1,
        "--json with a bare title must persist exactly one observation"
    );
}

/// Backward compatibility: `report --json <file>` still reads a JSON
/// observation input document from an existing file.
#[test]
fn test_report_json_intake_from_file() {
    let ctx = TestContext::new();
    let payload = ctx.home_dir.path().join("input.json");
    std::fs::write(
        &payload,
        r#"{"schema_version": 1, "title": "File intake test", "kind_assertion": "bug"}"#,
    )
    .unwrap();
    ctx.cmd()
        .arg("report")
        .arg("--json")
        .arg(&payload)
        .assert()
        .success();
    ctx.cmd()
        .arg("list")
        .arg("--kind")
        .arg("bug")
        .assert()
        .success()
        .stdout(predicate::str::contains("File intake test"));
}

/// Dogfood finding (fixed): the bare fast path `snag "<title>"` must accept
/// the structured flags without requiring the `report` subcommand.
#[test]
fn test_bare_fast_path_structured_flags() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("bare flags test")
        .arg("--kind")
        .arg("bug")
        .arg("--severity")
        .arg("major")
        .assert()
        .success()
        .stdout(predicate::str::contains("Recorded obs_"));
    assert_eq!(
        obs_count(&ctx),
        1,
        "bare fast path must persist one observation"
    );
    ctx.cmd()
        .arg("list")
        .arg("--kind")
        .arg("bug")
        .assert()
        .success()
        .stdout(predicate::str::contains("bare flags test"));
    ctx.cmd()
        .arg("list")
        .arg("--kind")
        .arg("papercut")
        .assert()
        .success()
        .stdout(predicate::str::contains("bare flags test").not());
}

/// The context protocol is versioned: a SNAG_CONTEXT_FILE with an unsupported
/// schema_version is rejected with a typed error; version 1 is accepted.
#[test]
fn test_context_file_schema_version_validation() {
    let ctx = TestContext::new();
    let bad = ctx.home_dir.path().join("ctx-bad.json");
    std::fs::write(&bad, r#"{"schema_version": 2}"#).unwrap();
    ctx.cmd()
        .env("SNAG_CONTEXT_FILE", &bad)
        .arg("report")
        .arg("rejected context version")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Unsupported schema"));
    assert!(
        !ctx.data_dir.join("snag.sqlite").exists(),
        "rejected context must not create a store"
    );

    let ok = ctx.home_dir.path().join("ctx-ok.json");
    std::fs::write(
        &ok,
        r#"{"schema_version": 1, "source": {"kind": "agent_explicit", "agent_runtime": "claude-code"}}"#,
    )
    .unwrap();
    ctx.cmd()
        .env("SNAG_CONTEXT_FILE", &ok)
        .arg("report")
        .arg("accepted context version")
        .assert()
        .success();
}

/// `snag context --format json` emits a versioned envelope.
#[test]
fn test_context_json_has_schema_version() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("context")
        .arg("--format")
        .arg("json")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"schema_version\": 1"));
}

/// Piping output into a consumer that closes early (`snag list | head -1`)
/// must exit quietly — not panic on a broken pipe.
#[test]
fn test_closed_pipe_exits_cleanly() {
    let ctx = TestContext::new();
    ctx.cmd().arg("report").arg("pipe test").assert().success();
    let bin = assert_cmd::cargo::cargo_bin("snag");
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("'{}' list | head -1", bin.display()))
        .env("XDG_DATA_HOME", ctx.home_dir.path())
        .env("HOME", ctx.home_dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "pipeline must exit 0, got {:?}",
        out.status
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked"),
        "closed pipe must not panic: {stderr}"
    );
}

/// `snag init` writes the capture-and-move-on instruction block to AGENTS.md
/// in the current directory (creating it when absent).
#[test]
fn test_init_writes_instructions() {
    let ctx = TestContext::new();
    let dir = ctx.home_dir.path();
    ctx.cmd()
        .current_dir(dir)
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("Configured"));
    let content = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert!(
        content.contains("record it with `snag`"),
        "block must be installed"
    );
    assert!(content.contains("<!-- snag:instructions -->"));
}

/// `snag init` is idempotent: a second run reports already-configured and
/// leaves the file byte-identical.
#[test]
fn test_init_idempotent() {
    let ctx = TestContext::new();
    let dir = ctx.home_dir.path();
    ctx.cmd().current_dir(dir).arg("init").assert().success();
    let before = std::fs::read(dir.join("AGENTS.md")).unwrap();
    ctx.cmd()
        .current_dir(dir)
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("Already configured"));
    let after = std::fs::read(dir.join("AGENTS.md")).unwrap();
    assert_eq!(before, after, "second init must not modify the file");
}

/// `snag init --dry-run` prints the section without writing anything.
#[test]
fn test_init_dry_run_writes_nothing() {
    let ctx = TestContext::new();
    let dir = ctx.home_dir.path();
    ctx.cmd()
        .current_dir(dir)
        .arg("init")
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(predicate::str::contains("Would write"));
    assert!(
        !dir.join("AGENTS.md").exists(),
        "dry-run must not create the file"
    );
}

/// `snag init` preserves existing file content and appends the section.
#[test]
fn test_init_preserves_existing_file() {
    let ctx = TestContext::new();
    let dir = ctx.home_dir.path();
    std::fs::write(dir.join("AGENTS.md"), "# existing instructions\n").unwrap();
    ctx.cmd().current_dir(dir).arg("init").assert().success();
    let content = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert!(
        content.starts_with("# existing instructions"),
        "existing content must survive"
    );
    assert!(content.contains("record it with `snag`"));
}

/// `snag init --file <path>` targets a custom file.
#[test]
fn test_init_custom_file() {
    let ctx = TestContext::new();
    let dir = ctx.home_dir.path();
    ctx.cmd()
        .current_dir(dir)
        .arg("init")
        .arg("--file")
        .arg("CLAUDE.md")
        .assert()
        .success();
    assert!(dir.join("CLAUDE.md").exists());
    assert!(
        !dir.join("AGENTS.md").exists(),
        "default file must not be created"
    );
}

/// `snag init --agent` adds the agent setup note for known agents.
#[test]
fn test_init_agent_note() {
    let ctx = TestContext::new();
    let dir = ctx.home_dir.path();
    ctx.cmd()
        .current_dir(dir)
        .arg("init")
        .arg("--agent")
        .arg("claude-code")
        .assert()
        .success();
    let content = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert!(content.contains("SNAG_SOURCE_KIND=agent_report"));
}

/// `snag doctor` reports the exact store paths, effective context source, and
/// version so users never have to infer where data lives.
#[test]
fn test_doctor_reports_paths_and_version() {
    let ctx = TestContext::new();
    let expected_db = ctx.data_dir.join("snag.sqlite");
    ctx.cmd()
        .env_remove("SNAG_CONTEXT_FILE")
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "snag {} (doctor)",
            env!("CARGO_PKG_VERSION")
        )))
        .stdout(predicate::str::contains("Database:"))
        .stdout(predicate::str::contains(expected_db.display().to_string()))
        .stdout(predicate::str::contains("Objects:"))
        .stdout(predicate::str::contains("Backups:"))
        .stdout(predicate::str::contains("Context file:"))
        .stdout(predicate::str::contains("(not set)"));
}

/// repro_key: every report carries a deterministic localization label that
/// survives idempotent replays (stable across same-key replays, distinct
/// across distinct content) and is excluded from the semantic digest.
#[test]
fn repro_key_is_labeled_deterministic_and_distinct() {
    let home = tempfile::tempdir().unwrap();
    let run = |title: &str, ik: Option<&str>| {
        let mut c = assert_cmd::Command::cargo_bin("snag").unwrap();
        c.env("XDG_DATA_HOME", home.path()).env("HOME", home.path());
        c.arg("report")
            .arg(title)
            .arg("--kind")
            .arg("bug")
            .arg("--severity")
            .arg("minor");
        if let Some(k) = ik {
            c.arg("--idempotency-key").arg(k);
        }
        c.output().unwrap()
    };
    let out1 = run("repro key test", Some("ik_rk"));
    assert!(out1.status.success());
    let text1 = String::from_utf8(out1.stdout).unwrap();
    let key1 = text1
        .lines()
        .find_map(|l| l.strip_prefix("repro key: "))
        .expect("repro key printed")
        .to_string();
    assert_eq!(key1.len(), 24);

    // Idempotent replay: no duplicate, same key.
    let out2 = run("repro key test", Some("ik_rk"));
    assert!(
        String::from_utf8(out2.stdout)
            .unwrap()
            .contains("already exists")
    );
    let conn = rusqlite::Connection::open(home.path().join("snag/snag.sqlite")).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM observations WHERE title = 'repro key test'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    let stored: String = conn
        .query_row("SELECT json_extract(labels_json, '$.repro_key') FROM observations WHERE title = 'repro key test'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, key1);

    // Distinct content gets a distinct key.
    let out3 = run("different repro content", None);
    let key3 = String::from_utf8(out3.stdout)
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("repro key: "))
        .expect("repro key printed")
        .to_string();
    assert_ne!(key1, key3);
}

/// Severity microcopy: a high-severity assertion with a thin body prints the
/// inflation nudge; a full-bodied report does not.
#[test]
fn thin_high_severity_report_nudges() {
    let home = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let mut c = assert_cmd::Command::cargo_bin("snag").unwrap();
        c.env("XDG_DATA_HOME", home.path()).env("HOME", home.path());
        c.arg("report");
        for a in args {
            c.arg(a);
        }
        c.output().unwrap()
    };
    let out = run(&["thin blocker", "--kind", "bug", "--severity", "blocker"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains("severity is a prior"),
        "nudge expected: {text}"
    );

    let out = run(&[
        "full major",
        "--kind",
        "bug",
        "--severity",
        "major",
        "--expected",
        "x",
        "--observed",
        "y",
    ]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        !text.contains("severity is a prior"),
        "no nudge for a full body: {text}"
    );
}

/// Observation ids resolve by unique prefix (GitHub-style) across show and
/// retract; ambiguity and misses are typed errors.
#[test]
fn prefix_observation_ids_resolve() {
    let home = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let mut c = assert_cmd::Command::cargo_bin("snag").unwrap();
        c.env("XDG_DATA_HOME", home.path()).env("HOME", home.path());
        for a in args {
            c.arg(a);
        }
        c.output().unwrap()
    };
    let out = run(&["report", "prefix-a", "--kind", "bug", "--severity", "minor"]);
    let id = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("Recorded "))
        .and_then(|l| l.split_whitespace().next())
        .expect("obs id")
        .to_string();
    run(&["report", "prefix-b", "--kind", "bug", "--severity", "minor"]);

    // Unique prefix resolves for show.
    let out = run(&["show", &id[..14]]);
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains(&id));

    // Missing prefix is a typed not-found.
    let out = run(&["show", "obs_zzz"]);
    assert!(!out.status.success());
    assert!(String::from_utf8(out.stderr).unwrap().contains("Not found"));

    // Retract by prefix works.
    let out = run(&["retract", &id[..14]]);
    assert!(out.status.success());
}
