use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
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
        .arg("no artifacts")
        .assert()
        .success();
    // Drop an unreferenced object file.
    let dir = ctx.data_dir.join("objects/blake3/ab");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a".repeat(64)), b"orphan bytes").unwrap();
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
    ctx.cmd().arg("report").arg("keep me").assert().success();

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
        .arg("store A record")
        .assert()
        .success();
    a.cmd().arg("backup").assert().success();
    let arch_a = latest_archive(&a);

    let b = TestContext::new();
    b.cmd()
        .arg("report")
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
fn test_rebuild_crash_never_publishes_destination() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
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
// 7. Remaining T-matrix sub-bullets (T1/T7): prose headings, outside-Git
//    capture, invalid schema, per-file artifact limit, concurrent same-object
// =====================================================================
#[test]
fn test_prose_headings_intake() {
    let ctx = TestContext::new();
    let prose = "Prose title here\n\nExpected:\nworks fine\n\nObserved:\nbroken\n\nReproduction:\nstep one\n";
    ctx.cmd()
        .arg("report")
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
    c.arg("report").arg("no repo here");
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
fn test_concurrent_same_object_single_artifact() {
    let ctx = TestContext::new();
    let f = ctx.home_dir.path().join("shared.bin");
    std::fs::write(&f, b"same content dedup").unwrap();

    // Two concurrent processes ingest the identical object.
    let mut children = Vec::new();
    for _ in 0..2 {
        let mut c = Proc::new(ctx.bin());
        c.arg("report")
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
