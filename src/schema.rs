use rusqlite::{Connection, Result};

/// Writer connection: may mutate journal mode and schema. Called on r/w opens
/// and on migration paths.
pub fn initialize_writer_connection(conn: &Connection) -> Result<()> {
    // Configure the busy timeout BEFORE any lock-taking statement so every
    // concurrent writer waits instead of failing fast under contention.
    conn.execute_batch(
        "
        PRAGMA synchronous = FULL;
        PRAGMA foreign_keys = ON;
        PRAGMA busy_timeout = 30000;
        ",
    )?;
    // journal_mode=WAL is a persistent, one-time DB property. Only attempt to
    // set it on a store that is not already WAL; forcing it on every open makes
    // 32 concurrent first-opens hammer the same lock and surface SQLITE_BUSY.
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        // Best-effort: if a concurrent writer is mid-transition, the next open
        // will already find it WAL (set by the process that created it).
        let _ = conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()));
    }
    Ok(())
}

/// Reader connection: only connection-local options that are legal on a
/// `SQLITE_OPEN_READ_ONLY` connection. Never attempts to mutate journal mode
/// or schema (both would fail or write on a read-only database).
pub fn initialize_reader_connection(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        PRAGMA foreign_keys = ON;
        PRAGMA busy_timeout = 30000;
        ",
    )?;
    Ok(())
}

/// Maintenance connection: read-write but without a forced migration/schema
/// mutation unless an explicit maintenance action requests it.
pub fn initialize_maintenance_connection(conn: &Connection) -> Result<()> {
    initialize_writer_connection(conn)
}

/// Backwards-compatible alias kept for any remaining callers.
pub fn init_connection(conn: &Connection) -> Result<()> {
    initialize_writer_connection(conn)
}

pub fn apply_migrations(conn: &mut Connection) -> anyhow::Result<()> {
    // Run under an EXCLUSIVE transaction so that when a fresh store is first
    // opened by several processes concurrently, exactly one applies the
    // migration chain; the others block and then observe the already-committed
    // schema version instead of re-running (and conflicting over) the same
    // migrations.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Exclusive)?;

    // Create migrations table if not exists
    tx.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        )",
        [],
    )?;

    let current_version: i64 = tx.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;

    if current_version < 1 {
        tx.execute_batch(
            "
            CREATE TABLE store_metadata (
                store_id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL
            );

            CREATE TABLE repositories (
                repository_id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL
            );

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

            INSERT INTO schema_migrations (version, applied_at) 
            VALUES (1, datetime('now'));
            ",
        )?;
    }

    if current_version < 2 {
        crate::migrations::migrate_v1_to_v2(&tx)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (2, datetime('now'))",
            [],
        )?;
    }

    if current_version < 3 {
        crate::migrations::migrate_v2_to_v3(&tx)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (3, datetime('now'))",
            [],
        )?;
    }

    if current_version < 4 {
        crate::migrations::migrate_v3_to_v4(&tx)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (4, datetime('now'))",
            [],
        )?;
    }

    tx.commit()?;
    Ok(())
}
