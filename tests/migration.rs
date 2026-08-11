use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;

struct TestContext {
    home_dir: tempfile::TempDir,
    data_dir: std::path::PathBuf,
}

impl TestContext {
    fn new() -> Self {
        let home_dir = tempfile::tempdir().unwrap();
        let data_dir = home_dir.path().join("snag");
        std::fs::create_dir_all(&data_dir).unwrap();
        Self { home_dir, data_dir }
    }
    fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("snag").unwrap();
        c.env("XDG_DATA_HOME", self.home_dir.path())
            .env("HOME", self.home_dir.path())
            // The agent harness injects SNAG_CONTEXT_FILE; it may point at a
            // purged /tmp file and must never leak into isolated CLI runs.
            .env_remove("SNAG_CONTEXT_FILE");
        c
    }
}

/// Build a genuine v1 database (schema_migrations at version 1, no `records`
/// table) with deliberately adversarial legacy data: observations and actions
/// whose OLD local_sequences overlap across tables and equal captured_at
/// values, exercising the G33 tie-breakers and collision-safety.
/// The full v1 base schema (mirrors the version-1 CREATE TABLE block in
/// src/schema.rs) so every later migration has the tables it references.
const V1_SCHEMA: &str = r#"
    PRAGMA foreign_keys = ON;
    CREATE TABLE store_metadata (store_id TEXT PRIMARY KEY, created_at TEXT NOT NULL);
    CREATE TABLE repositories (repository_id TEXT PRIMARY KEY, created_at TEXT NOT NULL);
    CREATE TABLE repository_aliases (
        alias TEXT PRIMARY KEY,
        repository_id TEXT NOT NULL REFERENCES repositories(repository_id),
        first_seen_at TEXT NOT NULL,
        last_seen_at TEXT NOT NULL
    );
    CREATE TABLE checkouts (
        checkout_id TEXT PRIMARY KEY,
        repository_id TEXT NOT NULL REFERENCES repositories(repository_id),
        git_common_dir TEXT NOT NULL UNIQUE,
        created_at TEXT NOT NULL
    );
    CREATE TABLE worktrees (
        worktree_id TEXT PRIMARY KEY,
        checkout_id TEXT NOT NULL REFERENCES checkouts(checkout_id),
        worktree_path TEXT NOT NULL UNIQUE,
        created_at TEXT NOT NULL
    );
    CREATE TABLE artifacts (
        digest TEXT PRIMARY KEY,
        byte_length INTEGER NOT NULL,
        media_type TEXT,
        original_name TEXT,
        created_at TEXT NOT NULL
    );
    CREATE TABLE observations (
        observation_id TEXT PRIMARY KEY,
        store_id TEXT NOT NULL REFERENCES store_metadata(store_id),
        local_sequence INTEGER NOT NULL UNIQUE,
        schema_version INTEGER NOT NULL,
        captured_at TEXT NOT NULL,
        source_kind TEXT NOT NULL,
        idempotency_key TEXT UNIQUE,
        title TEXT NOT NULL,
        summary TEXT,
        kind_assertion TEXT,
        severity_assertion TEXT,
        expected_behavior TEXT,
        observed_behavior TEXT,
        reproduction TEXT,
        workaround TEXT,
        impact TEXT,
        confidence REAL,
        sensitivity TEXT NOT NULL,
        labels_json TEXT,
        context_json TEXT,
        canonical_payload_json TEXT NOT NULL,
        previous_record_hash TEXT NOT NULL,
        record_hash TEXT NOT NULL
    );
    CREATE TABLE observation_repositories (
        observation_id TEXT NOT NULL REFERENCES observations(observation_id),
        repository_id TEXT NOT NULL REFERENCES repositories(repository_id),
        PRIMARY KEY (observation_id, repository_id)
    );
    CREATE TABLE observation_artifacts (
        observation_id TEXT NOT NULL REFERENCES observations(observation_id),
        digest TEXT NOT NULL REFERENCES artifacts(digest),
        PRIMARY KEY (observation_id, digest)
    );
    CREATE TABLE observation_actions (
        action_id TEXT PRIMARY KEY,
        observation_id TEXT NOT NULL REFERENCES observations(observation_id),
        action_type TEXT NOT NULL,
        action_payload_json TEXT NOT NULL,
        created_at TEXT NOT NULL,
        local_sequence INTEGER NOT NULL UNIQUE,
        previous_record_hash TEXT NOT NULL,
        record_hash TEXT NOT NULL
    );
    CREATE TABLE delivery_sinks (
        sink_id TEXT PRIMARY KEY,
        sink_type TEXT NOT NULL,
        configuration_json TEXT,
        created_at TEXT NOT NULL
    );
    CREATE TABLE delivery_state (
        sink_id TEXT NOT NULL REFERENCES delivery_sinks(sink_id),
        acknowledged_through INTEGER NOT NULL,
        head_record_hash TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        PRIMARY KEY (sink_id)
    );
    CREATE TABLE delivery_attempts (
        attempt_id TEXT PRIMARY KEY,
        sink_id TEXT NOT NULL REFERENCES delivery_sinks(sink_id),
        observation_id TEXT NOT NULL REFERENCES observations(observation_id),
        status TEXT NOT NULL,
        started_at TEXT NOT NULL,
        completed_at TEXT,
        error_message TEXT
    );
    CREATE TABLE backup_checkpoints (
        backup_id TEXT PRIMARY KEY,
        store_id TEXT NOT NULL REFERENCES store_metadata(store_id),
        created_at TEXT NOT NULL,
        through_sequence INTEGER NOT NULL,
        head_record_hash TEXT NOT NULL,
        database_digest TEXT NOT NULL,
        manifest_digest TEXT NOT NULL
    );
    CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);
    INSERT INTO schema_migrations (version, applied_at) VALUES (1, 'now');
    INSERT INTO store_metadata (store_id, created_at) VALUES ('store_fix', 'now');
"#;

fn build_v1_fixture(ctx: &TestContext) {
    let db = ctx.data_dir.join("snag.sqlite");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(V1_SCHEMA).unwrap();

    let obs_a = r#"{"schema_version":1,"observation_id":"obs_a","store_id":"store_fix","local_sequence":1,"idempotency_key":"kia","created_at":"2026-01-02T00:00:00Z","source":{"kind":"agent_explicit"},"title":"A","sensitivity":"normal","context":{}}"#;
    let obs_b = r#"{"schema_version":1,"observation_id":"obs_b","store_id":"store_fix","local_sequence":2,"idempotency_key":"kib","created_at":"2026-01-01T00:00:00Z","source":{"kind":"agent_explicit"},"title":"B","sensitivity":"normal","context":{}}"#;
    let retraction = r#"{"reason":"legacy retraction"}"#;

    // obs_a old seq 1; obs_b old seq 2; action on obs_a with OLD seq 1
    // (overlaps obs_a's seq, but in a different table) and EQUAL captured_at
    // to obs_b (2026-01-01) to exercise the tie-breaker.
    conn.execute(
        "INSERT INTO observations (observation_id, store_id, local_sequence, schema_version, captured_at, source_kind, idempotency_key, title, sensitivity, canonical_payload_json, previous_record_hash, record_hash)
         VALUES ('obs_a','store_fix',1,1,'2026-01-02T00:00:00Z','agent_explicit','kia','A','normal',?1,'p','h')",
        [obs_a]).unwrap();
    conn.execute(
        "INSERT INTO observations (observation_id, store_id, local_sequence, schema_version, captured_at, source_kind, idempotency_key, title, sensitivity, canonical_payload_json, previous_record_hash, record_hash)
         VALUES ('obs_b','store_fix',2,2,'2026-01-01T00:00:00Z','agent_explicit','kib','B','normal',?1,'p','h')",
        [obs_b]).unwrap();
    conn.execute(
        "INSERT INTO observation_actions (action_id, observation_id, action_type, action_payload_json, created_at, local_sequence, previous_record_hash, record_hash)
         VALUES ('act_a','obs_a','retracted',?1,'2026-01-01T00:00:00Z',1,'p','h')",
        [retraction]).unwrap();
}

/// v1 -> latest migration is deterministic, collision-safe, preserves all rows,
/// verifies the resulting store, and leaves a forensic pre-migration copy.
#[test]
fn test_v1_migration_fixture() {
    let ctx = TestContext::new();
    build_v1_fixture(&ctx);

    // A write-open triggers the migration chain (v1->v2->v3->v4), which runs
    // full verification immediately after migrating.
    ctx.cmd()
        .arg("report")
        .arg("post-migration")
        .arg("--unowned")
        .assert()
        .success();

    // Independent full verification of the migrated store.
    ctx.cmd().arg("verify").arg("--full").assert().success();

    let conn = Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
    // All legacy records survived in the global stream.
    let records: i64 = conn
        .query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0))
        .unwrap();
    let obs: i64 = conn
        .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
        .unwrap();
    let acts: i64 = conn
        .query_row("SELECT COUNT(*) FROM observation_actions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        obs, 3,
        "two legacy observations plus the post-migration report must survive"
    );
    assert_eq!(acts, 1, "legacy action must survive");
    // 2 obs + 1 action + 1 new report observation = 4 records.
    assert_eq!(records, 4);

    // Deterministic ordering: obs_b (01-01, class0) before action (01-01,
    // class1) before obs_a (01-02, class0).
    let order: Vec<(i64, String)> = {
        let mut stmt = conn
            .prepare("SELECT local_sequence, record_type FROM records ORDER BY local_sequence")
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        let mut v = Vec::new();
        while let Some(r) = rows.next().unwrap() {
            v.push((r.get(0).unwrap(), r.get(1).unwrap()));
        }
        v
    };
    assert_eq!(order[0].1, "observation_created");
    assert_eq!(order[1].1, "observation_retracted");
    assert_eq!(order[2].1, "observation_created");
    assert_eq!(order[3].1, "observation_created");

    // Forensic pre-migration copy exists.
    let forensics = ctx.data_dir.join("forensics");
    let has_copy = std::fs::read_dir(&forensics)
        .map(|mut it| {
            it.any(|e| {
                e.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("pre-v2-migration-")
            })
        })
        .unwrap_or(false);
    assert!(has_copy, "forensic pre-migration copy missing");
}

/// An irreconcilable legacy row (invalid canonical payload) must fail the
/// migration loudly rather than being silently discarded (G33).
#[test]
fn test_v1_migration_malformed_payload_fails() {
    let ctx = TestContext::new();
    let db = ctx.data_dir.join("snag.sqlite");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(V1_SCHEMA).unwrap();
    conn.execute_batch(
        "INSERT INTO observations (observation_id, store_id, local_sequence, schema_version, captured_at, source_kind, title, sensitivity, canonical_payload_json, previous_record_hash, record_hash)
        VALUES ('obs_bad','store_fix',2,1,'2026-01-01T00:00:00Z','agent_explicit','BAD','normal','{NOT VALID JSON','p','h');",
    )
    .unwrap();

    ctx.cmd()
        .arg("report")
        .arg("boom")
        .arg("--unowned")
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid legacy payload"));
    // The malformed legacy row must survive untouched (never silently discarded).
    let conn = Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
    let bad: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM observations WHERE observation_id='obs_bad'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bad, 1, "irreconcilable legacy row must be preserved");
}

// ---------------------------------------------------------------------------
// v4 -> v5 remediation migration fixture (T10).
//
// The v4 schema shape mirrors what the app produced after the v1->v4 chain
// (v2 global `records` stream, v3 alias/repo-role tables, v4 semantic_digest),
// with REAL canonical hashes so the post-migration `verify --full` is
// meaningful. The v5 migration must: touch no observation/record rows, backfill
// every observation as `unreviewed`, and leave the store fully verifiable.
// ---------------------------------------------------------------------------

const V4_SCHEMA: &str = r#"
    PRAGMA foreign_keys = ON;
    CREATE TABLE store_metadata (store_id TEXT PRIMARY KEY, created_at TEXT NOT NULL);
    CREATE TABLE repositories (repository_id TEXT PRIMARY KEY, created_at TEXT NOT NULL);
    CREATE TABLE repository_aliases (
        alias TEXT NOT NULL,
        repository_id TEXT NOT NULL REFERENCES repositories(repository_id),
        confirmed INTEGER NOT NULL DEFAULT 0,
        first_seen_at TEXT NOT NULL,
        last_seen_at TEXT NOT NULL,
        PRIMARY KEY (alias, repository_id)
    );
    CREATE TABLE checkouts (
        checkout_id TEXT PRIMARY KEY,
        repository_id TEXT NOT NULL REFERENCES repositories(repository_id),
        git_common_dir TEXT NOT NULL UNIQUE,
        created_at TEXT NOT NULL
    );
    CREATE TABLE worktrees (
        worktree_id TEXT PRIMARY KEY,
        checkout_id TEXT NOT NULL REFERENCES checkouts(checkout_id),
        worktree_path TEXT NOT NULL UNIQUE,
        created_at TEXT NOT NULL
    );
    CREATE TABLE artifacts (
        digest TEXT PRIMARY KEY,
        byte_length INTEGER NOT NULL,
        media_type TEXT,
        original_name TEXT,
        created_at TEXT NOT NULL
    );
    CREATE TABLE observations (
        observation_id TEXT PRIMARY KEY,
        store_id TEXT NOT NULL REFERENCES store_metadata(store_id),
        local_sequence INTEGER NOT NULL UNIQUE,
        schema_version INTEGER NOT NULL,
        captured_at TEXT NOT NULL,
        source_kind TEXT NOT NULL,
        idempotency_key TEXT UNIQUE,
        title TEXT NOT NULL,
        summary TEXT,
        kind_assertion TEXT,
        severity_assertion TEXT,
        expected_behavior TEXT,
        observed_behavior TEXT,
        reproduction TEXT,
        workaround TEXT,
        impact TEXT,
        confidence REAL,
        sensitivity TEXT NOT NULL,
        labels_json TEXT,
        context_json TEXT,
        canonical_payload_json TEXT NOT NULL,
        previous_record_hash TEXT NOT NULL,
        record_hash TEXT NOT NULL,
        semantic_digest TEXT
    );
    CREATE TABLE observation_repositories (
        observation_id TEXT NOT NULL REFERENCES observations(observation_id),
        repository_id TEXT NOT NULL REFERENCES repositories(repository_id),
        role TEXT NOT NULL DEFAULT 'affected',
        PRIMARY KEY (observation_id, repository_id, role)
    );
    CREATE TABLE observation_artifacts (
        observation_id TEXT NOT NULL REFERENCES observations(observation_id),
        digest TEXT NOT NULL REFERENCES artifacts(digest),
        PRIMARY KEY (observation_id, digest)
    );
    CREATE TABLE records (
        local_sequence INTEGER PRIMARY KEY,
        record_id TEXT UNIQUE NOT NULL,
        record_type TEXT NOT NULL,
        entity_id TEXT NOT NULL,
        captured_at TEXT NOT NULL,
        canonical_payload_json TEXT NOT NULL,
        previous_record_hash TEXT NOT NULL,
        record_hash TEXT NOT NULL
    );
    CREATE TABLE observation_actions (
        action_id TEXT PRIMARY KEY,
        observation_id TEXT NOT NULL REFERENCES observations(observation_id),
        action_type TEXT NOT NULL,
        action_payload_json TEXT NOT NULL,
        created_at TEXT NOT NULL,
        local_sequence INTEGER NOT NULL UNIQUE,
        previous_record_hash TEXT NOT NULL,
        record_hash TEXT NOT NULL
    );
    CREATE TABLE delivery_sinks (
        sink_id TEXT PRIMARY KEY,
        sink_type TEXT NOT NULL,
        configuration_json TEXT,
        created_at TEXT NOT NULL
    );
    CREATE TABLE delivery_state (
        sink_id TEXT NOT NULL REFERENCES delivery_sinks(sink_id),
        acknowledged_through INTEGER NOT NULL,
        head_record_hash TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        PRIMARY KEY (sink_id)
    );
    CREATE TABLE delivery_attempts (
        attempt_id TEXT PRIMARY KEY,
        sink_id TEXT NOT NULL REFERENCES delivery_sinks(sink_id),
        observation_id TEXT NOT NULL REFERENCES observations(observation_id),
        status TEXT NOT NULL,
        started_at TEXT NOT NULL,
        completed_at TEXT,
        error_message TEXT
    );
    CREATE TABLE backup_checkpoints (
        backup_id TEXT PRIMARY KEY,
        store_id TEXT NOT NULL REFERENCES store_metadata(store_id),
        created_at TEXT NOT NULL,
        through_sequence INTEGER NOT NULL,
        head_record_hash TEXT NOT NULL,
        database_digest TEXT NOT NULL,
        manifest_digest TEXT NOT NULL
    );
    CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);
    INSERT INTO schema_migrations (version, applied_at) VALUES (1,'now'),(2,'now'),(3,'now'),(4,'now');
    INSERT INTO store_metadata (store_id, created_at) VALUES ('store_fix', 'now');
"#;

/// Canonical record hash over the same kernel the app uses
/// (`CanonicalRecordV1::compute_hash`), so `verify --full` can validate the
/// fixture's chain.
#[allow(clippy::too_many_arguments)]
fn canonical_hash(
    store_id: &str,
    seq: u64,
    record_id: &str,
    record_type: &str,
    entity_id: &str,
    captured_at: &str,
    prev: &str,
    payload_json: &str,
) -> String {
    let mut h = blake3::Hasher::new();
    h.update(store_id.as_bytes());
    h.update(&1u32.to_le_bytes()); // CANONICAL_ENCODING_VERSION
    h.update(&seq.to_le_bytes());
    h.update(record_id.as_bytes());
    h.update(record_type.as_bytes());
    h.update(entity_id.as_bytes());
    h.update(captured_at.as_bytes());
    h.update(prev.as_bytes());
    h.update(payload_json.as_bytes());
    format!("blake3:{}", h.finalize().to_hex())
}

/// Build a genuine v4 store: two observations in the global records stream
/// with a real hash chain and the v3/v4 table shapes.
fn build_v4_fixture(ctx: &TestContext) {
    let db = ctx.data_dir.join("snag.sqlite");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(V4_SCHEMA).unwrap();

    let obs_a = r#"{"schema_version":1,"observation_id":"obs_a","store_id":"store_fix","local_sequence":1,"idempotency_key":"kia","created_at":"2026-01-02T00:00:00Z","source":{"kind":"agent_explicit"},"title":"A","sensitivity":"normal","context":{}}"#;
    let obs_b = r#"{"schema_version":1,"observation_id":"obs_b","store_id":"store_fix","local_sequence":2,"idempotency_key":"kib","created_at":"2026-01-01T00:00:00Z","source":{"kind":"agent_explicit"},"title":"B","sensitivity":"normal","context":{}}"#;
    let zero = "0000000000000000000000000000000000000000000000000000000000000000";
    let h1 = canonical_hash(
        "store_fix",
        1,
        "obs_a",
        "observation_created",
        "obs_a",
        "2026-01-02T00:00:00Z",
        zero,
        obs_a,
    );
    let h2 = canonical_hash(
        "store_fix",
        2,
        "obs_b",
        "observation_created",
        "obs_b",
        "2026-01-01T00:00:00Z",
        &h1,
        obs_b,
    );

    conn.execute(
        "INSERT INTO records (local_sequence, record_id, record_type, entity_id, captured_at, canonical_payload_json, previous_record_hash, record_hash)
         VALUES (1,'obs_a','observation_created','obs_a','2026-01-02T00:00:00Z',?1,?2,?3)",
        rusqlite::params![obs_a, zero, &h1],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO records (local_sequence, record_id, record_type, entity_id, captured_at, canonical_payload_json, previous_record_hash, record_hash)
         VALUES (2,'obs_b','observation_created','obs_b','2026-01-01T00:00:00Z',?1,?2,?3)",
        rusqlite::params![obs_b, &h1, &h2],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO observations (observation_id, store_id, local_sequence, schema_version, captured_at, source_kind, idempotency_key, title, sensitivity, canonical_payload_json, previous_record_hash, record_hash)
         VALUES ('obs_a','store_fix',1,1,'2026-01-02T00:00:00Z','agent_explicit','kia','A','normal',?1,?2,?3)",
        rusqlite::params![obs_a, zero, &h1],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO observations (observation_id, store_id, local_sequence, schema_version, captured_at, source_kind, idempotency_key, title, sensitivity, canonical_payload_json, previous_record_hash, record_hash)
         VALUES ('obs_b','store_fix',2,2,'2026-01-01T00:00:00Z','agent_explicit','kib','B','normal',?1,?2,?3)",
        rusqlite::params![obs_b, &h1, &h2],
    )
    .unwrap();
}

/// T10: a v4 store migrates to v5 with every observation initially
/// `unreviewed`, no observation history altered, and a fully verifiable store.
#[test]
fn test_v4_to_v5_remediation_migration() {
    let ctx = TestContext::new();
    build_v4_fixture(&ctx);

    // A write-open triggers the v5 migration.
    ctx.cmd()
        .arg("report")
        .arg("post-migration")
        .arg("--unowned")
        .assert()
        .success();

    // Full verification passes after migration (chain + normalized agreement).
    ctx.cmd().arg("verify").arg("--full").assert().success();

    let conn = Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
    // Every existing observation begins as unreviewed with an empty lineage.
    let unreviewed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM observation_review_state WHERE state='unreviewed' AND handled=0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unreviewed, 2, "both legacy observations start unreviewed");
    let state_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM observation_review_state", [], |r| {
            r.get(0)
        })
        .unwrap();
    // The migration backfills only pre-existing observations; observations
    // created after the migration receive their review-state row when the
    // remediation commands land (the queue treats a missing row as
    // `unreviewed`).
    assert_eq!(
        state_count, 2,
        "both legacy observations backfilled as unreviewed"
    );

    // Observation history is untouched by the migration.
    let titles: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT title FROM observations ORDER BY local_sequence")
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        let mut v = Vec::new();
        while let Some(r) = rows.next().unwrap() {
            v.push(r.get(0).unwrap());
        }
        v
    };
    assert_eq!(titles, vec!["A", "B", "post-migration"]);
    let records: i64 = conn
        .query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0))
        .unwrap();
    assert_eq!(records, 3, "two legacy records + the new report");
    let legacy_hashes: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT previous_record_hash, record_hash FROM records WHERE record_type='observation_created' AND entity_id IN ('obs_a','obs_b') ORDER BY local_sequence",
            )
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        let mut v = Vec::new();
        while let Some(r) = rows.next().unwrap() {
            v.push((r.get(0).unwrap(), r.get(1).unwrap()));
        }
        v
    };
    assert_eq!(legacy_hashes.len(), 2, "legacy records keep their hashes");
}

/// T11: v11 rebuilds the review-state projection from the authoritative event
/// stream. This repairs deployed stores whose lane-specific v8/v9 marker did
/// not prove that the replay ran, without changing append-only records.
#[test]
fn test_v11_rebuilds_stale_review_projection_after_legacy_markers() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
        .arg("projection-source")
        .arg("--unowned")
        .arg("--kind")
        .arg("bug")
        .arg("--severity")
        .arg("major")
        .assert()
        .success();
    let conn = Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
    let observation_id: String = conn
        .query_row(
            "SELECT observation_id FROM observations WHERE title = 'projection-source'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    drop(conn);

    ctx.cmd()
        .arg("review")
        .arg("disposition")
        .arg(&observation_id)
        .arg("confirmed")
        .assert()
        .success();
    ctx.cmd()
        .arg("review")
        .arg("attach-fix")
        .arg(&observation_id)
        .arg("--commit")
        .arg("abc123")
        .arg("--repo")
        .arg("repo_projection")
        .assert()
        .success();

    let conn = Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
    conn.execute(
        "UPDATE observation_review_state SET commits_json = '[]'
         WHERE observation_id = ?1",
        [&observation_id],
    )
    .unwrap();
    // Exercise the original v7 -> v8 replay path and continue through the
    // current migration head.
    conn.execute(
        "DELETE FROM schema_migrations WHERE version IN (8, 9, 10, 11, 12, 13)",
        [],
    )
    .unwrap();
    let record_snapshot: (i64, Option<String>) = conn
        .query_row(
            "SELECT COUNT(*),
                    MAX(CASE WHEN record_type = 'remediation_fix_attached'
                             THEN record_hash END)
             FROM records WHERE entity_id = ?1",
            [&observation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    drop(conn);

    // Any later write-open applies pending migrations before appending.
    ctx.cmd()
        .arg("report")
        .arg("migration-trigger")
        .arg("--unowned")
        .assert()
        .success();

    let conn = Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
    let (state, disposition, handled, commits_json): (String, Option<String>, i64, String) = conn
        .query_row(
            "SELECT state, disposition, handled, commits_json
             FROM observation_review_state WHERE observation_id = ?1",
            [&observation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(state, "candidate_fix");
    assert_eq!(disposition.as_deref(), Some("confirmed"));
    assert_eq!(handled, 0);
    assert_eq!(
        commits_json, r#"[{"commit_sha":"abc123","repository_id":"repo_projection"}]"#,
        "v11 must replay commit lineage from records"
    );
    let records_after: (i64, Option<String>) = conn
        .query_row(
            "SELECT COUNT(*),
                    MAX(CASE WHEN record_type = 'remediation_fix_attached'
                             THEN record_hash END)
             FROM records WHERE entity_id = ?1",
            [&observation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        records_after, record_snapshot,
        "projection rebuild must not alter append-only records"
    );
    drop(conn);

    ctx.cmd().arg("verify").arg("--full").assert().success();

    // A deployed store can already carry the old v8/v9 markers even though a
    // lane variant never replayed this projection. Only v11 closes that gap.
    let conn = Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
    conn.execute(
        "UPDATE observation_review_state SET commits_json = '[]'
         WHERE observation_id = ?1",
        [&observation_id],
    )
    .unwrap();
    conn.execute(
        "DELETE FROM schema_migrations WHERE version IN (11, 12, 13)",
        [],
    )
    .unwrap();
    drop(conn);
    ctx.cmd()
        .arg("report")
        .arg("deployed-marker-trigger")
        .arg("--unowned")
        .assert()
        .success();
    let conn = Connection::open(ctx.data_dir.join("snag.sqlite")).unwrap();
    let commits_json: String = conn
        .query_row(
            "SELECT commits_json FROM observation_review_state WHERE observation_id = ?1",
            [&observation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        commits_json,
        r#"[{"commit_sha":"abc123","repository_id":"repo_projection"}]"#
    );
    drop(conn);
    ctx.cmd().arg("verify").arg("--full").assert().success();
}

fn assert_record_lookup_index(conn: &Connection) {
    let index_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_records_entity_type_sequence'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        index_sql
            .to_ascii_lowercase()
            .contains("on records(entity_id, record_type, local_sequence)"),
        "record lookup index must preserve entity/type/sequence order: {index_sql}"
    );

    let lookup_queries = [
        "SELECT local_sequence FROM records
         WHERE entity_id = 'obs_a'
           AND record_type IN ('observation_claimed', 'observation_reviewed')
         ORDER BY local_sequence ASC",
        "SELECT local_sequence FROM records
         WHERE entity_id = 'obs_a' AND record_type = 'observation_retracted'
         ORDER BY local_sequence ASC",
        "SELECT local_sequence FROM records
         WHERE entity_id = 'obs_a' AND record_type = 'observation_owner_assigned'
         ORDER BY local_sequence ASC",
    ];
    for query in lookup_queries {
        let plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
            .unwrap()
            .query_map([], |row| row.get(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|detail| detail.contains("INDEX idx_records_entity_type_sequence")),
            "entity/type lookup must use the composite index: {query}; plan={plan:?}"
        );
        assert!(
            !plan.iter().any(|detail| detail.contains("SCAN records")),
            "entity/type lookup must not scan records: {query}; plan={plan:?}"
        );
    }
}

#[test]
fn test_record_lookup_index_fresh_upgraded_and_rerun_safe() {
    let fresh = TestContext::new();
    fresh
        .cmd()
        .arg("report")
        .arg("index-fresh")
        .arg("--unowned")
        .assert()
        .success();
    let conn = Connection::open(fresh.data_dir.join("snag.sqlite")).unwrap();
    assert_record_lookup_index(&conn);
    let index_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_records_entity_type_sequence'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index_count, 1);
    drop(conn);

    // Re-run the forward migration marker while the index already exists.
    let conn = Connection::open(fresh.data_dir.join("snag.sqlite")).unwrap();
    conn.execute(
        "DELETE FROM schema_migrations WHERE version IN (9, 10, 11, 12, 13)",
        [],
    )
    .unwrap();
    drop(conn);
    fresh
        .cmd()
        .arg("report")
        .arg("index-rerun")
        .arg("--unowned")
        .assert()
        .success();
    let conn = Connection::open(fresh.data_dir.join("snag.sqlite")).unwrap();
    assert_record_lookup_index(&conn);
    let index_count_after_rerun: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_records_entity_type_sequence'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index_count_after_rerun, 1);
    drop(conn);

    // A pre-index upgraded store receives the same index during the chain.
    let upgraded = TestContext::new();
    build_v4_fixture(&upgraded);
    upgraded
        .cmd()
        .arg("report")
        .arg("index-upgraded")
        .arg("--unowned")
        .assert()
        .success();
    let conn = Connection::open(upgraded.data_dir.join("snag.sqlite")).unwrap();
    assert_record_lookup_index(&conn);
}
