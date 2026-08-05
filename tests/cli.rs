use assert_cmd::Command;
use predicates::prelude::*;
use std::env;
use std::fs;
use tempfile::TempDir;

/// Provides an isolated environment for a snag test instance
pub struct TestContext {
    pub home_dir: TempDir,
    pub data_dir: std::path::PathBuf,
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
        
    // 3. Different payload with same key -> fails
    ctx.cmd()
        .arg("report")
        .arg("Idempotency Test DIFFERENT")
        .arg("--idempotency-key")
        .arg("key_123")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Idempotency key collision"));
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
    conn.execute("UPDATE records SET captured_at = '2000-01-01T00:00:00Z' WHERE local_sequence = 1", []).unwrap();
    
    // 3. Verify should fail
    ctx.cmd()
        .arg("verify")
        .arg("--full")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Hash chain mismatch"));
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
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1);
}

#[test]
fn test_restore_protocol() {
    let ctx = TestContext::new();
    
    // Create record
    ctx.cmd().arg("report").arg("Restore Test").assert().success();
    
    // Backup
    ctx.cmd().arg("backup").assert().success();
    
    // Find the backup archive
    let backups_dir = ctx.data_dir.join("backups");
    let mut backup_archive = None;
    for entry in std::fs::read_dir(&backups_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().map_or(false, |e| e == "gz") {
            backup_archive = Some(path);
            break;
        }
    }
    let archive_path = backup_archive.expect("Backup archive not found");
    
    // Try restoring when active store is non-empty -> should fail
    ctx.cmd().arg("restore").arg(&archive_path).assert().failure().stderr(predicate::str::contains("non-empty"));
    
    // Delete store file directly
    let _ = std::fs::remove_file(ctx.data_dir.join("snag.sqlite"));
    
    // Restore
    ctx.cmd().arg("restore").arg(&archive_path).assert().success();
    
    // Verify
    ctx.cmd().arg("verify").arg("--full").assert().success();
}
