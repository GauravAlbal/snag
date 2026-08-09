use assert_cmd::Command;
use flate2::Compression;
use flate2::write::GzEncoder;
use predicates::prelude::*;
use rusqlite::Connection;
use std::fs::File;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
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
    fn conn(&self) -> Connection {
        Connection::open(self.data_dir.join("snag.sqlite")).unwrap()
    }
}

fn obs_count(ctx: &TestContext) -> i64 {
    let db = ctx.data_dir.join("snag.sqlite");
    if !db.exists() {
        return 0;
    }
    let conn = Connection::open(db).unwrap();
    conn.query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let st = Proc::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(st.success(), "git {:?} failed", args);
}
fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t.test"]);
    git(dir, &["config", "user.name", "T"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("f.txt"), b"one\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "one"]);
}

fn latest_archive(ctx: &TestContext) -> PathBuf {
    let dir = ctx.data_dir.join("backups");
    let mut best: Option<PathBuf> = None;
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "gz")
            && !p.file_name().unwrap().to_string_lossy().contains(".tmp.")
            && (best.is_none() || p > best.clone().unwrap())
        {
            best = Some(p);
        }
    }
    best.expect("no published backup archive")
}

fn object_path_for(ctx: &TestContext, digest: &str) -> PathBuf {
    let hex = digest.strip_prefix("blake3:").unwrap();
    ctx.data_dir
        .join("objects/blake3")
        .join(&hex[0..2])
        .join(hex)
}

fn object_tree(ctx: &TestContext) -> Vec<PathBuf> {
    let root = ctx.data_dir.join("objects/blake3");
    let Ok(prefixes) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for prefix in prefixes.filter_map(Result::ok) {
        let Ok(objects) = std::fs::read_dir(prefix.path()) else {
            continue;
        };
        entries.push(prefix.path());
        entries.extend(objects.filter_map(Result::ok).map(|object| object.path()));
    }
    entries.sort();
    entries
}

// =====================================================================
// 1. Idempotency under ambient drift (HEAD/branch change must not break replay)
// =====================================================================
#[test]
fn test_idempotency_survives_head_and_branch_drift() {
    let ctx = TestContext::new();
    let repo = ctx.home_dir.path().join("r");
    init_repo(&repo);

    let mut first = ctx.cmd();
    first.current_dir(&repo);
    first
        .arg("report")
        .arg("--unowned")
        .arg("stable report")
        .arg("--repo-id")
        .arg("repo_drift")
        .arg("--idempotency-key")
        .arg("drift_1");
    first
        .assert()
        .success()
        .stdout(predicate::str::contains("Recorded obs_"));

    // Change HEAD and switch branch.
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "new head"]);
    git(&repo, &["checkout", "-q", "-b", "other"]);

    let mut replay = ctx.cmd();
    replay.current_dir(&repo);
    replay
        .arg("report")
        .arg("--unowned")
        .arg("stable report")
        .arg("--repo-id")
        .arg("repo_drift")
        .arg("--idempotency-key")
        .arg("drift_1");
    let out = replay.output().unwrap();
    assert!(
        out.status.success(),
        "replay after ambient drift must succeed"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("already exists"),
        "expected a replay, got: {stdout}"
    );

    let conn = ctx.conn();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM observations WHERE title='stable report'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "ambient drift must not create a second observation");
}

// =====================================================================
// 2. Artifact corruption and lifecycle
// =====================================================================
#[test]
fn test_artifact_source_mutation_does_not_alter_object() {
    let ctx = TestContext::new();
    let src = ctx.home_dir.path().join("a.bin");
    std::fs::write(&src, b"ORIGINAL CONTENT").unwrap();
    ctx.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("art")
        .arg("--artifact")
        .arg(&src)
        .assert()
        .success();

    let digest: String = ctx
        .conn()
        .query_row("SELECT digest FROM artifacts LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let obj = object_path_for(&ctx, &digest);

    // Mutate the source after capture; the stored object is a content-addressed copy.
    std::fs::write(&src, b"DIFFERENT CONTENT NOW").unwrap();
    assert_eq!(std::fs::read(&obj).unwrap(), b"ORIGINAL CONTENT");
}

#[test]
fn test_missing_and_modified_artifact_fail_verify() {
    let ctx = TestContext::new();
    let src = ctx.home_dir.path().join("a.bin");
    std::fs::write(&src, b"hello").unwrap();
    ctx.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("art")
        .arg("--artifact")
        .arg(&src)
        .assert()
        .success();
    let digest: String = ctx
        .conn()
        .query_row("SELECT digest FROM artifacts LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let obj = object_path_for(&ctx, &digest);

    // Missing object.
    std::fs::remove_file(&obj).unwrap();
    ctx.cmd()
        .arg("verify")
        .arg("--full")
        .assert()
        .failure()
        .stderr(predicate::str::contains("missing"));

    // Restore and then corrupt.
    std::fs::write(&obj, b"tampered").unwrap();
    std::fs::set_permissions(&obj, std::fs::Permissions::from_mode(0o600)).unwrap();
    ctx.cmd()
        .arg("verify")
        .arg("--full")
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid"));
    assert_eq!(
        std::fs::read(&obj).unwrap(),
        b"tampered",
        "object must hold the corrupted bytes"
    );
}

#[test]
fn test_orphan_object_reported() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("no artifacts")
        .assert()
        .success();
    // Drop an unreferenced object file.
    let dir = ctx.data_dir.join("objects/blake3/ab");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let orphan = dir.join("a".repeat(64));
    std::fs::write(&orphan, b"orphan bytes").unwrap();
    std::fs::set_permissions(&orphan, std::fs::Permissions::from_mode(0o600)).unwrap();
    ctx.cmd()
        .arg("verify")
        .arg("--full")
        .assert()
        .success()
        .stdout(predicate::str::contains("orphan"));
    assert!(
        ctx.data_dir
            .join("objects/blake3/ab")
            .join("a".repeat(64))
            .exists(),
        "orphan object file must remain on disk"
    );
}

// =====================================================================
// 3. Export adversarial cases
// =====================================================================
fn header(store_id: &str, first: u64, through: u64, count: u64) -> String {
    format!(
        r#"{{"export_kind":"export_header","export_schema_version":1,"minimum_reader_version":1,"store_id":"{store_id}","first_sequence":{first},"through_sequence":{through},"previous_checkpoint_hash":"0000000000000000000000000000000000000000000000000000000000000000","head_record_hash":"0000000000000000000000000000000000000000000000000000000000000000","record_count":{count}}}"#
    )
}

fn obs_record(seq: u64, prev: &str, out: &str) -> String {
    format!(
        r#"{{"export_kind":"record","record_schema_version":1,"local_sequence":{seq},"record_id":"obs_{seq}","record_type":"observation_created","entity_id":"obs_{seq}","captured_at":"2026-01-01T00:00:00Z","canonical_payload":{{"schema_version":1,"observation_id":"obs_{seq}","store_id":"s","local_sequence":{seq},"created_at":"2026-01-01T00:00:00Z","source":{{"kind":"human_explicit"}},"title":"t{seq}","sensitivity":"normal","context":{{}}}},"previous_record_hash":"{prev}","record_hash":"{out}"}}"#
    )
}

fn rebuild_stream(ctx: &TestContext, stream: &str, label: &str) -> assert_cmd::assert::Assert {
    let file = ctx.home_dir.path().join(format!("stream-{label}.jsonl"));
    std::fs::write(&file, stream).unwrap();
    ctx.cmd()
        .arg("rebuild")
        .arg("--from-export")
        .arg(&file)
        .arg("--destination")
        .arg(ctx.home_dir.path().join(format!("dest-{label}")))
        .assert()
}

const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[test]
fn test_export_wrong_predecessor_rejected() {
    let ctx = TestContext::new();
    // Second record references a wrong predecessor hash.
    let stream = format!(
        "{h}\n{r1}\n{r2}",
        h = header("s", 1, 2, 2),
        r1 = obs_record(
            1,
            ZERO,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ),
        r2 = obs_record(
            2,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ZERO
        ),
    );
    rebuild_stream(&ctx, &stream, "wrongpred").failure();
    assert!(
        !ctx.home_dir.path().join("dest-wrongpred").exists(),
        "wrong-predecessor stream must never publish a destination"
    );
}

#[test]
fn test_export_duplicate_and_missing_sequence_rejected() {
    let ctx = TestContext::new();
    // Duplicate sequence (1 twice).
    let dup = format!(
        "{h}\n{r1a}\n{r1b}",
        h = header("s", 1, 2, 2),
        r1a = obs_record(
            1,
            ZERO,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ),
        r1b = obs_record(
            1,
            ZERO,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        ),
    );
    rebuild_stream(&ctx, &dup, "dupseq").failure();
    assert!(
        !ctx.home_dir.path().join("dest-dupseq").exists(),
        "duplicate-sequence stream must never publish a destination"
    );

    // Missing sequence: header claims 2 records through 2 but only seq 1 present.
    let miss = format!(
        "{h}\n{r1}",
        h = header("s", 1, 2, 2),
        r1 = obs_record(
            1,
            ZERO,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ),
    );
    rebuild_stream(&ctx, &miss, "missseq").failure();
    assert!(
        !ctx.home_dir.path().join("dest-missseq").exists(),
        "missing-sequence stream must never publish a destination"
    );
}

#[test]
fn test_export_unsupported_schema_rejected() {
    let ctx = TestContext::new();
    let bad_hdr = format!(
        r#"{{"export_kind":"export_header","export_schema_version":99,"minimum_reader_version":1,"store_id":"s","first_sequence":1,"through_sequence":0,"previous_checkpoint_hash":"{ZERO}","head_record_hash":"{ZERO}","record_count":0}}"#
    );
    rebuild_stream(&ctx, &bad_hdr, "hdr99").failure();
    assert!(
        !ctx.home_dir.path().join("dest-hdr99").exists(),
        "unsupported header schema must never publish a destination"
    );

    let bad_rec = obs_record(
        1,
        ZERO,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .replace(
        "\"record_schema_version\":1",
        "\"record_schema_version\":99",
    );
    let stream = format!("{h}\n{r}", h = header("s", 1, 1, 1), r = bad_rec);
    rebuild_stream(&ctx, &stream, "rec99").failure();
    assert!(
        !ctx.home_dir.path().join("dest-rec99").exists(),
        "unsupported record schema must never publish a destination"
    );
}

#[test]
fn test_export_failed_export_preserves_existing_output() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("keep me")
        .assert()
        .success();

    let out = ctx.home_dir.path().join("existing.jsonl");
    std::fs::write(&out, b"SENTINEL\n").unwrap();

    // Invalid bounds (after > through) must fail and leave the existing output untouched.
    ctx.cmd()
        .arg("export")
        .arg("--output")
        .arg(&out)
        .arg("--after-sequence")
        .arg("100")
        .arg("--through-sequence")
        .arg("1")
        .assert()
        .failure();
    assert_eq!(
        std::fs::read(&out).unwrap(),
        b"SENTINEL\n",
        "failed export must not touch existing destination"
    );
}

// =====================================================================
// 4. Backup component substitution
// =====================================================================
#[test]
fn test_backup_component_substitution_detected() {
    // Two different stores with different data.
    let a = TestContext::new();
    a.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("store A record")
        .assert()
        .success();
    a.cmd().arg("backup").assert().success();
    let arch_a = latest_archive(&a);

    let b = TestContext::new();
    b.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("store B DIFFERENT")
        .assert()
        .success();
    b.cmd().arg("backup").assert().success();

    // Mix the database from store B into store A's bundle; verification must fail.
    let mix_dir = a
        .home_dir
        .path()
        .join(format!("mix-{}", ulid::Ulid::generate()));
    extract(&arch_a, &mix_dir);
    let bundled_a = mix_dir.join("snag.sqlite");
    let db_b = b.data_dir.join("snag.sqlite");
    std::fs::copy(&db_b, &bundled_a).unwrap();

    a.cmd()
        .arg("verify")
        .arg("--backup")
        .arg(&mix_dir)
        .assert()
        .failure();
    b.cmd()
        .arg("verify")
        .arg("--backup")
        .arg(&mix_dir)
        .assert()
        .failure();
    assert!(
        mix_dir.join("snag.sqlite").exists() && mix_dir.join("manifest.json").exists(),
        "mixed bundle must still contain both components for the swap to be detectable"
    );
}

fn extract(archive: &Path, dest: &Path) {
    std::fs::create_dir_all(dest).unwrap();
    let f = std::fs::File::open(archive).unwrap();
    let dec = flate2::read::GzDecoder::new(f);
    let mut ar = tar::Archive::new(dec);
    ar.unpack(dest).unwrap();
}

// =====================================================================
// 5. Git timeout cleanup: bounded return + child killed + reaped
// =====================================================================
#[test]
fn test_git_timeout_kills_child_and_returns_bounded() {
    let ctx = TestContext::new();
    let repo = ctx.home_dir.path().join("g");
    init_repo(&repo);

    // A fake `git` that records its PID and hangs forever.
    let fakebin = ctx.home_dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    let pidfile = ctx.home_dir.path().join("git.pid");
    let script = format!("#!/bin/sh\necho $$ > {}\nsleep 100\n", pidfile.display());
    std::fs::write(fakebin.join("git"), script).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(fakebin.join("git"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }

    let bin = ctx.bin();
    let path = format!(
        "{}:{}",
        fakebin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let start = std::time::Instant::now();
    let status = Proc::new(&bin)
        .arg("report")
        .arg("--unowned")
        .arg("timeout bounded")
        .current_dir(&repo)
        .env("XDG_DATA_HOME", ctx.home_dir.path())
        .env("HOME", ctx.home_dir.path())
        .env("PATH", path)
        .status()
        .unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "git timeout must be bounded (hung child would sleep 100s), took {elapsed:?}"
    );
    assert!(
        status.success(),
        "bounded git timeout must not lose the report"
    );

    // The hung child must have been killed and reaped.
    let pid = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .to_string();
    let alive = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("kill -0 {pid} 2>/dev/null"))
        .status()
        .unwrap();
    assert!(
        !alive.success(),
        "timed-out git child {pid} must be killed, not left running"
    );
}

// =====================================================================
// 6. Recovery-operation crash matrix (backup / restore / rebuild)
// =====================================================================
fn run_abort(ctx: &TestContext, args: &[&str], failpoint: &str) -> std::process::ExitStatus {
    Proc::new(ctx.bin())
        .args(args)
        .env("XDG_DATA_HOME", ctx.home_dir.path())
        .env("HOME", ctx.home_dir.path())
        .env("SNAG_FAILPOINT", failpoint)
        .status()
        .unwrap()
}

#[test]
fn test_backup_crash_never_publishes_partial_bundle() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("backup crash")
        .assert()
        .success();

    let stages = [
        "backup_after_db_copy",
        "backup_during_object_copy",
        "backup_after_verification",
        "backup_after_manifest_write",
        "backup_before_publish",
    ];
    for stage in stages {
        let st = run_abort(&ctx, &["backup"], stage);
        assert!(!st.success(), "{stage} should abort");
        // No published final archive may exist with a .manifest-missing/partial bundle.
        let backups = ctx.data_dir.join("backups");
        let published = std::fs::read_dir(&backups)
            .map(|it| {
                it.filter_map(|e| e.ok())
                    .filter(|e| {
                        let n = e.file_name().to_string_lossy().into_owned();
                        n.ends_with(".tar.gz") && !n.contains(".tmp.")
                    })
                    .count()
            })
            .unwrap_or(0);
        assert_eq!(published, 0, "{stage}: no published bundle may exist");
        // No backup_checkpoints row before publication.
        let conn = ctx.conn();
        let ck: i64 = conn
            .query_row("SELECT COUNT(*) FROM backup_checkpoints", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ck, 0, "{stage}: no checkpoint before publish");
    }

    // after_publish: the bundle is complete and verifies. (The checkpoint
    // insert is bookkeeping that follows filesystem publish; the archive itself
    // carries the manifest/digests, so it is valid regardless.)
    let st = run_abort(&ctx, &["backup"], "backup_after_publish");
    assert!(!st.success(), "backup_after_publish should abort");
    let arch = latest_archive(&ctx);
    ctx.cmd()
        .arg("verify")
        .arg("--backup")
        .arg(&arch)
        .assert()
        .success();
}

#[test]
fn test_restore_crash_leaves_verified_store() {
    // Source store with a backup.
    let src = TestContext::new();
    src.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("restored data")
        .arg("--idempotency-key")
        .arg("rk")
        .assert()
        .success();
    src.cmd().arg("backup").assert().success();
    let arch = latest_archive(&src);

    let stages = [
        "restore_after_forensic_copy",
        "restore_after_candidate_creation",
        "restore_after_candidate_verification",
        "restore_before_active_switch",
        "restore_after_active_switch",
    ];
    for stage in stages {
        let ctx = TestContext::new(); // fresh, empty active store
        let st = run_abort(&ctx, &["restore", arch.to_str().unwrap()], stage);
        assert!(!st.success(), "{stage} should abort");
        // Active store must be either absent (old empty state) or fully verifiable.
        let db = ctx.data_dir.join("snag.sqlite");
        if db.exists() {
            ctx.cmd().arg("verify").arg("--full").assert().success();
        }
    }
}

#[test]
fn test_stale_lock_diagnostic_does_not_block_restore() {
    let src = TestContext::new();
    src.cmd()
        .arg("report")
        .arg("lock source")
        .arg("--unowned")
        .assert()
        .success();
    src.cmd().arg("backup").assert().success();
    let archive = latest_archive(&src);

    let dst = TestContext::new();
    std::fs::create_dir_all(&dst.data_dir).unwrap();
    std::fs::write(dst.data_dir.join(".maintenance.lock"), b"dead pid=1\n").unwrap();
    let restore = dst.cmd().arg("restore").arg(&archive).output().unwrap();
    assert!(
        restore.status.success(),
        "a stale diagnostic lock must not block restore: {}",
        String::from_utf8_lossy(&restore.stderr)
    );
    let verify = dst.cmd().arg("verify").arg("--full").output().unwrap();
    assert!(
        verify.status.success(),
        "the restored store must remain fully verifiable: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
}

#[test]
fn test_restore_lock_blocks_writer_until_cutover_then_releases() {
    let src = TestContext::new();
    src.cmd()
        .arg("report")
        .arg("barrier source")
        .arg("--unowned")
        .assert()
        .success();
    src.cmd().arg("backup").assert().success();
    let archive = latest_archive(&src);

    let dst = TestContext::new();
    dst.cmd()
        .arg("report")
        .arg("empty seed")
        .arg("--unowned")
        .assert()
        .success();
    dst.conn()
        .execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM records; DELETE FROM observations;")
        .unwrap();
    let mut restore = Proc::new(dst.bin())
        .args(["restore", archive.to_str().unwrap()])
        .env("XDG_DATA_HOME", dst.home_dir.path())
        .env("HOME", dst.home_dir.path())
        .env("SNAG_FAILPOINT_HOLD", "restore_before_active_switch")
        .env("SNAG_FAILPOINT_HOLD_MS", "3000")
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if dst.data_dir.join("forensics").exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::thread::sleep(std::time::Duration::from_millis(250));

    let mut writer = Proc::new(dst.bin())
        .args(["report", "barrier writer", "--unowned"])
        .env("XDG_DATA_HOME", dst.home_dir.path())
        .env("HOME", dst.home_dir.path())
        .spawn()
        .unwrap();
    let mut reader = Proc::new(dst.bin())
        .args(["verify", "--full"])
        .env("XDG_DATA_HOME", dst.home_dir.path())
        .env("HOME", dst.home_dir.path())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(250));
    assert!(writer.try_wait().unwrap().is_none());
    assert!(reader.try_wait().unwrap().is_none());
    assert!(dst.data_dir.join("snag.sqlite").exists());

    assert!(restore.wait().unwrap().success());
    assert!(writer.wait().unwrap().success());
    assert!(reader.wait().unwrap().success());
    dst.cmd().arg("verify").arg("--full").assert().success();
    assert_eq!(
        dst.conn()
            .query_row("SELECT COUNT(*) FROM records", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
}
#[test]
fn test_killed_restore_releases_lock_for_writer() {
    let src = TestContext::new();
    src.cmd()
        .arg("kill source")
        .arg("--unowned")
        .assert()
        .success();
    src.cmd().arg("backup").assert().success();
    let archive = latest_archive(&src);

    let dst = TestContext::new();
    let mut restore = Proc::new(dst.bin())
        .args(["restore", archive.to_str().unwrap()])
        .env("XDG_DATA_HOME", dst.home_dir.path())
        .env("HOME", dst.home_dir.path())
        .env("SNAG_FAILPOINT_HOLD", "restore_before_active_switch")
        .env("SNAG_FAILPOINT_HOLD_MS", "5000")
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if dst.data_dir.join("forensics").exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::thread::sleep(std::time::Duration::from_millis(250));

    let mut writer = Proc::new(dst.bin())
        .args(["report", "surviving writer", "--unowned"])
        .env("XDG_DATA_HOME", dst.home_dir.path())
        .env("HOME", dst.home_dir.path())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(250));
    assert!(writer.try_wait().unwrap().is_none());
    restore.kill().unwrap();
    let _ = restore.wait();
    assert!(writer.wait().unwrap().success());
    dst.cmd().arg("verify").arg("--full").assert().success();
}
#[test]
fn test_rebuild_crash_never_publishes_destination() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("rebuild crash")
        .assert()
        .success();

    let out = ctx.home_dir.path().join("export.jsonl");
    ctx.cmd()
        .arg("export")
        .arg("--output")
        .arg(&out)
        .assert()
        .success();

    let stages = [
        "rebuild_after_header_validation",
        "rebuild_mid_stream",
        "rebuild_after_verification",
        "rebuild_after_construction",
        "rebuild_before_publication",
    ];
    for stage in stages {
        let dest = ctx
            .home_dir
            .path()
            .join(format!("dest-{}", stage[8..].replace(['_'], "-")));
        let st = Proc::new(ctx.bin())
            .args([
                "rebuild",
                "--from-export",
                out.to_str().unwrap(),
                "--destination",
                dest.to_str().unwrap(),
            ])
            .env("XDG_DATA_HOME", ctx.home_dir.path())
            .env("HOME", ctx.home_dir.path())
            .env("SNAG_FAILPOINT", stage)
            .status()
            .unwrap();
        assert!(!st.success(), "{stage} should abort");
        // Destination must never appear as a valid-looking published directory.
        assert!(!dest.exists(), "{stage}: destination must not be published");
    }
}

// =====================================================================
// 7b. Hermetic consumer hardening (Darn harvest): export → rebuild into an
//     isolated data home → full verify → reopen read-only → verify again →
//     assert identity; mutation verbs fail against the read-only store.
// =====================================================================

#[test]
fn test_hermetic_export_rebuild_verify_identity() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("hermetic a")
        .arg("--kind")
        .arg("bug")
        .arg("--severity")
        .arg("major")
        .assert()
        .success();
    ctx.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("hermetic b")
        .arg("--kind")
        .arg("papercut")
        .arg("--severity")
        .arg("minor")
        .assert()
        .success();

    // Fingerprint the original store.
    let out = ctx
        .cmd()
        .arg("verify")
        .arg("--quick")
        .arg("--json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let original: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(original["record_count"], 2);
    assert!(!original["store_id"].as_str().unwrap().is_empty());
    assert!(!original["head_hash"].as_str().unwrap().is_empty());

    // Export the stream.
    let export = ctx.home_dir.path().join("hermetic.jsonl");
    ctx.cmd()
        .arg("export")
        .arg("--output")
        .arg(&export)
        .assert()
        .success();

    // Rebuild into an isolated data home. XDG_DATA_HOME resolves to
    // <home>/snag/snag.sqlite, so the destination must be a directory named
    // `snag` inside a fresh home for read-reopen to land on the rebuilt store.
    let new_home = ctx.home_dir.path().join("isolated");
    let rebuilt = new_home.join("snag");
    ctx.cmd()
        .arg("rebuild")
        .arg("--from-export")
        .arg(&export)
        .arg("--destination")
        .arg(&rebuilt)
        .assert()
        .success();
    assert!(rebuilt.join("snag.sqlite").exists());

    // Full verify against the rebuilt store (point XDG_DATA_HOME at the
    // isolated home).
    let mut verify = ctx.cmd();
    verify
        .env("XDG_DATA_HOME", &new_home)
        .arg("verify")
        .arg("--full")
        .arg("--json");
    let out = verify.output().unwrap();
    assert!(
        out.status.success(),
        "full verify failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rebuilt_fp: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();

    // Identity: same store_id, sequence, head hash, record count.
    assert_eq!(rebuilt_fp["store_id"], original["store_id"]);
    assert_eq!(rebuilt_fp["through_sequence"], original["through_sequence"]);
    assert_eq!(rebuilt_fp["head_hash"], original["head_hash"]);
    assert_eq!(rebuilt_fp["record_count"], original["record_count"]);

    // Reopen read-only and verify again: the reconstructed store must be
    // consumable without mutation.
    let mut verify2 = ctx.cmd();
    verify2
        .env("XDG_DATA_HOME", &new_home)
        .arg("verify")
        .arg("--full");
    let out = verify2.output().unwrap();
    assert!(
        out.status.success(),
        "second verify failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Read verbs work against the reconstructed store.
    let mut list = ctx.cmd();
    list.env("XDG_DATA_HOME", &new_home)
        .arg("review")
        .arg("list")
        .arg("--format")
        .arg("json");
    let out = list.output().unwrap();
    assert!(out.status.success());
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_rebuild_destination_rejects_database_path() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("reject me")
        .assert()
        .success();
    let export = ctx.home_dir.path().join("e.jsonl");
    ctx.cmd()
        .arg("export")
        .arg("--output")
        .arg(&export)
        .assert()
        .success();

    // A .sqlite destination is the classic Darn ambiguity: caller passes the
    // database path, rebuild silently creates <path>.sqlite/snag.sqlite.
    let out = ctx
        .cmd()
        .arg("rebuild")
        .arg("--from-export")
        .arg(&export)
        .arg("--destination")
        .arg(ctx.home_dir.path().join("dest.sqlite"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("DATA DIRECTORY"),
        "must name the directory-vs-file semantics"
    );
}

#[test]
fn test_rebuilt_readonly_store_rejects_mutations() {
    use std::os::unix::fs::PermissionsExt;

    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("ro store")
        .assert()
        .success();
    let export = ctx.home_dir.path().join("ro-export.jsonl");
    ctx.cmd()
        .arg("export")
        .arg("--output")
        .arg(&export)
        .assert()
        .success();

    let new_home = ctx.home_dir.path().join("ro-isolated");
    let rebuilt = new_home.join("snag");
    ctx.cmd()
        .arg("rebuild")
        .arg("--from-export")
        .arg(&export)
        .arg("--destination")
        .arg(&rebuilt)
        .assert()
        .success();

    // Read-only consumption must not require mutation: reads succeed first.
    let mut list = ctx.cmd();
    list.env("XDG_DATA_HOME", &new_home)
        .arg("review")
        .arg("list");
    assert!(list.output().unwrap().status.success());

    // Make the store read-only (files and dir).
    for entry in std::fs::read_dir(&rebuilt).unwrap() {
        let p = entry.unwrap().path();
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o444);
        std::fs::set_permissions(&p, perms).unwrap();
    }
    let mut perms = std::fs::metadata(&rebuilt).unwrap().permissions();
    perms.set_mode(0o555);
    std::fs::set_permissions(&rebuilt, perms).unwrap();

    // Mutation verb (report) must fail against the read-only store.
    let mut report = ctx.cmd();
    report
        .env("XDG_DATA_HOME", &new_home)
        .arg("report")
        .arg("--unowned")
        .arg("must fail");
    let out = report.output().unwrap();
    assert!(
        !out.status.success(),
        "mutation must fail against read-only store: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Read verb still works on the read-only store.
    let mut list2 = ctx.cmd();
    list2
        .env("XDG_DATA_HOME", &new_home)
        .arg("review")
        .arg("list");
    assert!(list2.output().unwrap().status.success());

    // Restore permissions so the tempdir can be cleaned up.
    for entry in std::fs::read_dir(&rebuilt).unwrap() {
        let p = entry.unwrap().path();
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&p, perms).unwrap();
    }
    let mut perms = std::fs::metadata(&rebuilt).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&rebuilt, perms).unwrap();
}

// =====================================================================
// 7. Remaining T-matrix sub-bullets (T1/T7): prose headings, outside-Git
//    capture, invalid schema, per-file artifact limit, concurrent same-object
// =====================================================================
#[test]
fn test_prose_headings_intake() {
    let ctx = TestContext::new();
    let prose = "Prose title here\n\nExpected:\nworks fine\n\nObserved:\nbroken\n\nReproduction:\nstep one\n";
    ctx.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("--stdin")
        .write_stdin(prose)
        .assert()
        .success();

    let conn = ctx.conn();
    let row: (String, String, String) = conn
        .query_row(
            "SELECT title, expected_behavior, observed_behavior FROM observations LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(row.0, "Prose title here");
    assert_eq!(row.1, "works fine");
    assert_eq!(row.2, "broken");
}

#[test]
fn test_outside_git_capture() {
    let ctx = TestContext::new();
    // Capture must succeed when run outside any git repository, and must NOT
    // invent a repository identity. The temp dir is sometimes created inside a
    // git checkout (e.g. a sandbox whose TMPDIR lives under a repo), so the
    // "no repository" assertion is gated on the environment actually being
    // outside a work tree.
    let plain = ctx.home_dir.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let inside_worktree = Proc::new("git")
        .current_dir(&plain)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false);

    let mut c = ctx.cmd();
    c.current_dir(&plain);
    c.arg("report").arg("--unowned").arg("no repo here");
    c.assert().success();

    let conn = ctx.conn();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    if !inside_worktree {
        let repos: i64 = conn
            .query_row("SELECT COUNT(*) FROM repositories", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            repos, 0,
            "outside-git capture must not invent a repository identity"
        );
    }
}

#[test]
fn test_invalid_json_schema_rejected() {
    let ctx = TestContext::new();
    let bad = r#"{"schema_version": 99, "title": "too new"}"#;
    ctx.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("--json")
        .write_stdin(bad)
        .assert()
        .failure()
        .stderr(predicate::str::contains("Unsupported schema"));
    assert_eq!(
        obs_count(&ctx),
        0,
        "unsupported schema must not create an observation"
    );
}

#[test]
fn test_artifact_per_file_limit() {
    let ctx = TestContext::new();
    let big = ctx.home_dir.path().join("big.bin");
    // 51 MiB exceeds the 50 MiB per-artifact limit.
    let blob = vec![0u8; 51 * 1024 * 1024];
    std::fs::write(&big, &blob).unwrap();
    ctx.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("too big")
        .arg("--artifact")
        .arg(&big)
        .assert()
        .failure()
        .stderr(predicate::str::contains("50 MiB"));
    assert_eq!(
        obs_count(&ctx),
        0,
        "oversized artifact must not create an observation"
    );
}

#[test]
fn test_aggregate_artifact_limit_preflight_leaves_no_objects() {
    let ctx = TestContext::new();
    let mut paths = Vec::new();
    for index in 0..6 {
        let path = ctx.home_dir.path().join(format!("sparse-{index}.bin"));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(42 * 1024 * 1024).unwrap();
        paths.push(path);
    }

    let mut cmd = ctx.cmd();
    cmd.arg("report").arg("--unowned").arg("aggregate too big");
    for path in &paths {
        cmd.arg("--artifact").arg(path);
    }
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("250 MiB"));
    assert_eq!(obs_count(&ctx), 0);
    assert!(object_tree(&ctx).is_empty());
}

#[test]
fn test_idempotency_conflict_removes_only_new_objects() {
    let ctx = TestContext::new();
    let first = ctx.home_dir.path().join("first.bin");
    let second = ctx.home_dir.path().join("second.bin");
    std::fs::write(&first, b"first artifact").unwrap();
    std::fs::write(&second, b"second artifact").unwrap();

    ctx.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("first artifact")
        .arg("--artifact")
        .arg(&first)
        .arg("--idempotency-key")
        .arg("artifact-conflict")
        .assert()
        .success();
    let before_tree = object_tree(&ctx);
    assert_eq!(before_tree.len(), 2);

    ctx.cmd()
        .arg("report")
        .arg("--unowned")
        .arg("different artifact")
        .arg("--artifact")
        .arg(&second)
        .arg("--idempotency-key")
        .arg("artifact-conflict")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Idempotency conflict"));
    assert_eq!(
        object_tree(&ctx),
        before_tree,
        "idempotency rejection must preserve the preexisting object tree"
    );
}

#[test]
fn test_concurrent_same_object_single_artifact() {
    let ctx = TestContext::new();
    let f = ctx.home_dir.path().join("shared.bin");
    std::fs::write(&f, b"same content dedup").unwrap();

    // Two concurrent processes ingest the identical object.
    let mut children = Vec::new();
    for _ in 0..2 {
        let mut c = Proc::new(ctx.bin());
        c.arg("report")
            .arg("--unowned")
            .arg("dup obj")
            .arg("--artifact")
            .arg(&f)
            .env("XDG_DATA_HOME", ctx.home_dir.path())
            .env("HOME", ctx.home_dir.path());
        children.push(c.spawn().unwrap());
    }
    for mut c in children {
        let s = c.wait().unwrap();
        assert!(s.success());
    }
    // Content-addressed store dedups to a single artifact row.
    let artifacts: i64 = ctx
        .conn()
        .query_row("SELECT COUNT(*) FROM artifacts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        artifacts, 1,
        "concurrent same-object ingestion must dedup to one artifact"
    );
    ctx.cmd().arg("verify").arg("--full").assert().success();
}

fn write_archive_entries(path: &Path, entries: &[(&str, tar::EntryType, &[u8])]) {
    let file = File::create(path).unwrap();
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (entry_path, entry_type, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_path(entry_path).unwrap();
        header.set_entry_type(*entry_type);
        header.set_size(data.len() as u64);
        header.set_cksum();
        builder.append(&header, *data).unwrap();
    }
    let encoder = builder.into_inner().unwrap();
    encoder.finish().unwrap();
}

fn write_rejected_archive(path: &Path, entry_path: &str, entry_type: tar::EntryType, size: u64) {
    let data = if size <= 4096 {
        vec![0u8; size as usize]
    } else {
        Vec::new()
    };
    write_archive_entries(path, &[(entry_path, entry_type, data.as_slice())]);
}

#[cfg(unix)]
#[test]
fn test_bundle_symlink_tree_rejected_without_touching_target() {
    let ctx = TestContext::new();
    let target = ctx.home_dir.path().join("outside");
    std::fs::write(&target, b"sentinel").unwrap();
    let bundle = ctx.home_dir.path().join("bundle");
    std::fs::create_dir(&bundle).unwrap();
    symlink(&target, bundle.join("snag.sqlite")).unwrap();
    ctx.cmd()
        .arg("verify")
        .arg("--backup")
        .arg(&bundle)
        .assert()
        .failure();
    assert_eq!(std::fs::read(&target).unwrap(), b"sentinel");
}

#[test]
fn test_archive_forbidden_entries_and_budgets_rejected_before_cutover() {
    let ctx = TestContext::new();
    let cases = [
        ("symlink", None, tar::EntryType::symlink(), "target", 0),
        ("special", None, tar::EntryType::new(b'6'), "special", 0),
        (
            "per-entry",
            Some(("SNAG_ARCHIVE_MAX_ENTRY_BYTES", "1")),
            tar::EntryType::file(),
            "large",
            2,
        ),
        (
            "depth",
            Some(("SNAG_ARCHIVE_MAX_DEPTH", "16")),
            tar::EntryType::file(),
            "a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q",
            0,
        ),
    ];
    for (name, limit, kind, entry, size) in cases {
        let archive = ctx.home_dir.path().join(format!("{name}.tar.gz"));
        write_rejected_archive(&archive, entry, kind, size);
        let mut command = ctx.cmd();
        if let Some((key, value)) = limit {
            command.env(key, value);
        }
        command
            .arg("verify")
            .arg("--backup")
            .arg(&archive)
            .assert()
            .failure();
        assert!(!ctx.data_dir.join("snag.sqlite").exists());
    }
    let aggregate = ctx.home_dir.path().join("aggregate.tar.gz");
    write_archive_entries(
        &aggregate,
        &[
            ("a", tar::EntryType::file(), b"x"),
            ("b", tar::EntryType::file(), b"x"),
        ],
    );
    ctx.cmd()
        .env("SNAG_ARCHIVE_MAX_TOTAL_BYTES", "1")
        .arg("verify")
        .arg("--backup")
        .arg(&aggregate)
        .assert()
        .failure();
    let count = ctx.home_dir.path().join("count.tar.gz");
    write_archive_entries(
        &count,
        &[
            ("a", tar::EntryType::file(), b""),
            ("b", tar::EntryType::file(), b""),
        ],
    );
    ctx.cmd()
        .env("SNAG_ARCHIVE_MAX_ENTRIES", "1")
        .arg("verify")
        .arg("--backup")
        .arg(&count)
        .assert()
        .failure();
}

#[test]
fn test_successful_verify_cleans_private_snapshot_and_managed_modes() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
        .arg("mode check")
        .arg("--unowned")
        .assert()
        .success();
    ctx.cmd().arg("backup").assert().success();
    let archive = latest_archive(&ctx);
    let scratch = tempfile::tempdir().unwrap();
    ctx.cmd()
        .env("TMPDIR", scratch.path())
        .arg("verify")
        .arg("--backup")
        .arg(&archive)
        .assert()
        .success();
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    #[cfg(unix)]
    {
        assert_eq!(std::fs::metadata(&archive).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            std::fs::metadata(ctx.data_dir.join("backups"))
                .unwrap()
                .mode()
                & 0o777,
            0o700
        );
    }
}
