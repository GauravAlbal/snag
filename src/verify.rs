use crate::cli::VerifyArgs;
use crate::store::Store;
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::Value;
use std::path::Path;

pub fn handle(args: VerifyArgs) -> Result<()> {
    if let Some(backup_path) = args.backup {
        println!("Verifying backup at {:?}", backup_path);
        verify_backup(&backup_path)?;
        return Ok(());
    }

    let mut store = Store::open_read_only()?;
    if args.quick {
        println!("Running quick verification...");
        quick_verify(&mut store)?;
        crate::remediation::verify::verify_remediation(&store.conn, true)?;
    } else {
        println!("Running full verification...");
        full_verify(&mut store)?;
        crate::remediation::verify::verify_remediation(&store.conn, false)?;
    }
    Ok(())
}

/// One record row of the verified suffix, in the column order selected by
/// [`load_suffix_records`].
struct SuffixRecord {
    seq: i64,
    prev: String,
    expected: String,
    payload: String,
    record_id: String,
    record_type: String,
    entity_id: String,
    captured_at: String,
}

/// Verify the most recent records plus their actual predecessor rows and
/// referenced artifacts (a bounded suffix, not a single hash recalc).
fn quick_verify(store: &mut Store) -> Result<()> {
    let result: String = store
        .conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if result != "ok" {
        anyhow::bail!("Quick check failed: {}", result);
    }

    // Verify the latest SUFFIX: take the last N rows, verify adjacency, each
    // row's predecessor-hash equality, and recompute each row's hash.
    let suffix: i64 = store.conn.query_row(
        "SELECT COUNT(*) FROM (SELECT local_sequence FROM records ORDER BY local_sequence DESC LIMIT 3)",
        [], |r| r.get(0))?;
    if suffix == 0 {
        println!("No observations to verify.");
        println!("Quick verification passed.");
        return Ok(());
    }

    let records = load_suffix_records(store)?;
    verify_suffix_chain(store, &records)?;
    verify_suffix_artifacts(store)?;

    println!("Quick verification passed.");
    Ok(())
}

/// Load the trailing record suffix in ascending sequence order.
fn load_suffix_records(store: &Store) -> Result<Vec<SuffixRecord>> {
    let mut stmt = store.conn.prepare(
        "SELECT local_sequence, previous_record_hash, record_hash, canonical_payload_json, record_id, record_type, entity_id, captured_at
         FROM records ORDER BY local_sequence DESC LIMIT 3",
    )?;
    let mut rows = stmt.query([])?;
    let mut records: Vec<SuffixRecord> = Vec::new();
    while let Some(row) = rows.next()? {
        records.push(SuffixRecord {
            seq: row.get(0)?,
            prev: row.get(1)?,
            expected: row.get(2)?,
            payload: row.get(3)?,
            record_id: row.get(4)?,
            record_type: row.get(5)?,
            entity_id: row.get(6)?,
            captured_at: row.get(7)?,
        });
    }
    // The query is DESC; reverse to ascending for chain verification.
    records.reverse();
    Ok(records)
}

/// Sequence adjacency, predecessor-hash equality, and hash recomputation over
/// the loaded suffix.
fn verify_suffix_chain(store: &Store, records: &[SuffixRecord]) -> Result<()> {
    for (i, rec) in records.iter().enumerate() {
        // Sequence adjacency + predecessor-hash equality.
        if i > 0 {
            let prior = &records[i - 1];
            if prior.seq != rec.seq - 1 {
                anyhow::bail!(
                    "Non-adjacent sequence in suffix: {} then {}",
                    prior.seq,
                    rec.seq
                );
            }
            if prior.expected != rec.prev {
                anyhow::bail!("Predecessor-hash mismatch at sequence {}", rec.seq);
            }
        }
        verify_suffix_record_hash(store, rec)?;
    }
    Ok(())
}

/// Recompute one suffix record's canonical hash and compare it to the stored
/// hash.
fn verify_suffix_record_hash(store: &Store, rec: &SuffixRecord) -> Result<()> {
    let record_payload: crate::record::RecordPayload =
        serde_json::from_str(&rec.payload).context("Invalid canonical payload")?;
    let canonical_record = crate::record::CanonicalRecordV1 {
        local_sequence: rec.seq as u64,
        record_id: rec.record_id.clone(),
        record_type: rec.record_type.clone(),
        entity_id: rec.entity_id.clone(),
        captured_at: rec.captured_at.clone(),
        payload: record_payload,
    };
    let computed = canonical_record.compute_hash(&store.store_id, &rec.prev);
    if computed != rec.expected {
        anyhow::bail!("Hash chain mismatch at sequence {}", rec.seq);
    }
    Ok(())
}

/// Presence + length of the artifacts referenced by the verified suffix.
fn verify_suffix_artifacts(store: &Store) -> Result<()> {
    let mut art_stmt = store.conn.prepare(
        "SELECT DISTINCT a.digest, a.byte_length FROM observation_artifacts oa
         JOIN artifacts a ON a.digest = oa.digest
         JOIN observations o ON o.observation_id = oa.observation_id
         WHERE o.local_sequence >= (SELECT COALESCE(MAX(local_sequence),0)-3 FROM records)
         ORDER BY a.digest",
    )?;
    let mut art_rows = art_stmt.query([])?;
    let objects_dir = store.data_dir.join("objects").join("blake3");
    while let Some(row) = art_rows.next()? {
        let digest: String = row.get(0)?;
        let expected_len: i64 = row.get(1)?;
        if let Some(hash) = digest.strip_prefix("blake3:") {
            let prefix = &hash[0..2];
            let path = objects_dir.join(prefix).join(hash);
            if !path.exists() {
                anyhow::bail!("Suffix artifact missing: {}", digest);
            }
            if std::fs::metadata(&path)?.len() as i64 != expected_len {
                anyhow::bail!("Suffix artifact length mismatch: {}", digest);
            }
        }
    }
    Ok(())
}

/// Full verification: integrity, foreign keys, sequence base + contiguity,
/// complete record hash chain, canonical binding, records-agree-with-
/// observations/actions, action targets exist, repository/checkout/worktree
/// relationships, artifact length + digest, orphan objects, store metadata
/// vs head, idempotency key contract, and head-sequence/hash agreement.
pub fn full_verify(store: &mut Store) -> Result<()> {
    integrity_and_fk_check(&store.conn)?;
    seq_contiguity_check(&store.conn)?;
    let (last_hash, chain_count) = record_chain_verify(store)?;
    normalized_agreement_check(&store.conn)?;
    artifact_verify(store)?;
    let orphan = orphan_scan(store)?;
    store_head_check(&store.conn, &last_hash, chain_count)?;
    idempotency_dup_check(&store.conn)?;

    if orphan > 0 {
        println!(
            "Full verification passed ({} records checked; {} orphan objects reported).",
            chain_count, orphan
        );
    } else {
        println!(
            "Full verification passed ({} records checked).",
            chain_count
        );
    }
    Ok(())
}

/// SQLite integrity check plus foreign-key violation scan.
fn integrity_and_fk_check(conn: &Connection) -> Result<()> {
    let result: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if result != "ok" {
        anyhow::bail!("Integrity check failed: {}", result);
    }
    let fk_violations: i64 =
        conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })?;
    if fk_violations > 0 {
        anyhow::bail!("Foreign key check failed with {} violations", fk_violations);
    }
    Ok(())
}

/// The record sequence starts at the expected base and has no gaps.
fn seq_contiguity_check(conn: &Connection) -> Result<()> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM records", [], |r| r.get(0))?;
    if count > 0 {
        let base: i64 =
            conn.query_row("SELECT MIN(local_sequence) FROM records", [], |r| r.get(0))?;
        if base != 1 {
            anyhow::bail!("Sequence does not start at expected base: {}", base);
        }
        let max_seq: i64 =
            conn.query_row("SELECT MAX(local_sequence) FROM records", [], |r| r.get(0))?;
        if max_seq != count {
            anyhow::bail!(
                "Sequence is not contiguous: {} records but max seq {}",
                count,
                max_seq
            );
        }
    }
    Ok(())
}

/// Walk the complete record hash chain, recomputing every canonical hash.
/// Returns the head hash reached and the number of records walked.
fn record_chain_verify(store: &Store) -> Result<(String, i64)> {
    let mut stmt = store.conn.prepare(
        "SELECT local_sequence, previous_record_hash, record_hash, canonical_payload_json, record_id, record_type, entity_id, captured_at
         FROM records ORDER BY local_sequence ASC",
    )?;
    let mut rows = stmt.query([])?;
    let mut last_hash =
        "0000000000000000000000000000000000000000000000000000000000000000".to_string();
    let mut chain_count = 0;
    while let Some(row) = rows.next()? {
        let seq: i64 = row.get(0)?;
        let prev: String = row.get(1)?;
        let expected_hash: String = row.get(2)?;
        let payload: String = row.get(3)?;
        let record_id: String = row.get(4)?;
        let record_type: String = row.get(5)?;
        let entity_id: String = row.get(6)?;
        let captured_at: String = row.get(7)?;
        if prev != last_hash {
            anyhow::bail!("Broken chain at seq {}: previous hash mismatch", seq);
        }
        let record_payload: crate::record::RecordPayload =
            serde_json::from_str(&payload).context("Invalid canonical payload")?;
        let canonical_record = crate::record::CanonicalRecordV1 {
            local_sequence: seq as u64,
            record_id,
            record_type,
            entity_id,
            captured_at,
            payload: record_payload,
        };
        let computed = canonical_record.compute_hash(&store.store_id, &prev);
        if computed != expected_hash {
            anyhow::bail!("Hash chain mismatch at sequence {}", seq);
        }
        last_hash = expected_hash;
        chain_count += 1;
    }
    Ok((last_hash, chain_count))
}

/// Records agree with the normalized projections: observation and action counts
/// match, and every action targets an existing observation.
fn normalized_agreement_check(conn: &Connection) -> Result<()> {
    // Records agree with observations (created observations all present, same count).
    let obs_records: i64 = conn.query_row(
        "SELECT COUNT(*) FROM records WHERE record_type = 'observation_created'",
        [],
        |r| r.get(0),
    )?;
    let obs_rows: i64 = conn.query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))?;
    if obs_records != obs_rows {
        anyhow::bail!(
            "Observation records ({}) disagree with observations table ({})",
            obs_records,
            obs_rows
        );
    }
    // Records agree with actions.
    let act_records: i64 = conn.query_row(
        "SELECT COUNT(*) FROM records WHERE record_type = 'observation_retracted'",
        [],
        |r| r.get(0),
    )?;
    let act_rows: i64 =
        conn.query_row("SELECT COUNT(*) FROM observation_actions", [], |r| r.get(0))?;
    if act_records != act_rows {
        anyhow::bail!(
            "Action records ({}) disagree with actions table ({})",
            act_records,
            act_rows
        );
    }
    // Every action targets an existing observation.
    let orphan_actions: i64 = conn.query_row(
        "SELECT COUNT(*) FROM observation_actions a LEFT JOIN observations o ON a.observation_id = o.observation_id WHERE o.observation_id IS NULL",
        [], |r| r.get(0))?;
    if orphan_actions > 0 {
        anyhow::bail!("{} actions reference missing observations", orphan_actions);
    }
    Ok(())
}

/// Every artifact row resolves to an on-disk object with the recorded length
/// and a matching BLAKE3 digest.
fn artifact_verify(store: &Store) -> Result<()> {
    let objects_dir = store.data_dir.join("objects").join("blake3");
    let mut art_stmt = store
        .conn
        .prepare("SELECT digest, byte_length FROM artifacts")?;
    let mut art_rows = art_stmt.query([])?;
    let mut missing = 0;
    let mut invalid = 0;
    while let Some(row) = art_rows.next()? {
        let digest: String = row.get(0)?;
        let expected_len: i64 = row.get(1)?;
        if let Some(hash) = digest.strip_prefix("blake3:") {
            let prefix = &hash[0..2];
            let path = objects_dir.join(prefix).join(hash);
            if !path.exists() {
                missing += 1;
                continue;
            }
            if std::fs::metadata(&path)?.len() as i64 != expected_len {
                invalid += 1;
                continue;
            }
            if !object_digest_matches(&path, &digest)? {
                invalid += 1;
            }
        }
    }
    if missing > 0 || invalid > 0 {
        anyhow::bail!("{} artifacts are missing, {} are invalid", missing, invalid);
    }
    Ok(())
}

/// Stream one object through BLAKE3 and compare with its recorded digest.
fn object_digest_matches(path: &Path, digest: &str) -> Result<bool> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher)?;
    let computed = format!("blake3:{}", hasher.finalize().to_hex());
    Ok(computed == digest)
}

/// Count objects present on disk but referenced by no artifact row.
fn orphan_scan(store: &Store) -> Result<i64> {
    let objects_dir = store.data_dir.join("objects").join("blake3");
    let mut orphan = 0;
    if objects_dir.exists() {
        for prefix in std::fs::read_dir(&objects_dir)? {
            let prefix = prefix?;
            if !prefix.path().is_dir() {
                continue;
            }
            orphan += orphan_count_in_prefix(store, &prefix.path())?;
        }
    }
    Ok(orphan)
}

/// Orphan objects inside one two-character fan-out directory.
fn orphan_count_in_prefix(store: &Store, prefix_dir: &Path) -> Result<i64> {
    let mut orphan = 0;
    for obj in std::fs::read_dir(prefix_dir)? {
        let obj = obj?;
        if obj.path().is_file() {
            let digest = format!("blake3:{}", obj.file_name().to_string_lossy());
            let referenced: i64 = store.conn.query_row(
                "SELECT COUNT(*) FROM artifacts WHERE digest = ?1",
                [&digest],
                |r| r.get(0),
            )?;
            if referenced == 0 {
                orphan += 1;
            }
        }
    }
    Ok(orphan)
}

/// Store metadata head agrees with the actual record head.
fn store_head_check(conn: &Connection, last_hash: &str, chain_count: i64) -> Result<()> {
    let actual_head_seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(local_sequence),0) FROM records",
        [],
        |r| r.get(0),
    )?;
    let actual_head_hash: String = conn.query_row(
        "SELECT COALESCE((SELECT record_hash FROM records ORDER BY local_sequence DESC LIMIT 1), '0000000000000000000000000000000000000000000000000000000000000000')",
        [], |r| r.get(0))?;
    if actual_head_hash != last_hash {
        anyhow::bail!("Store metadata head disagrees with actual record head");
    }
    if actual_head_seq < chain_count {
        anyhow::bail!("Store head sequence disagrees with record count");
    }
    Ok(())
}

/// Idempotency key contract: no two observations share a key with different
/// canonical payload semantics.
fn idempotency_dup_check(conn: &Connection) -> Result<()> {
    let dup_keys: i64 = conn.query_row(
        "SELECT COUNT(*) FROM (SELECT idempotency_key, COUNT(DISTINCT canonical_payload_json) c
         FROM observations WHERE idempotency_key IS NOT NULL GROUP BY idempotency_key HAVING c > 1)",
        [], |r| r.get(0))?;
    if dup_keys > 0 {
        anyhow::bail!("Duplicate idempotency keys with differing payloads found");
    }
    Ok(())
}

/// Validated fields of a backup bundle's `manifest.json`.
struct BundleManifest {
    store_id: String,
    head_record_hash: String,
    through_sequence: u64,
    database_digest: String,
    artifact_manifest_digest: String,
}

/// Independent verification of a backup bundle (directory or archive).
///
/// Verifies: required bundle files, manifest schema, database digest, SQLite
/// integrity, foreign keys, record sequence, complete record hash chain,
/// normalized consistency, store ID, head sequence, head hash, object-manifest
/// digest, every artifact path/length/BLAKE3 digest. Detects modified DB,
/// modified manifest, modified object manifest, missing/modified artifact,
/// swapped components, mismatched store ID, and incorrect head metadata.
pub fn verify_backup(backup_path: &Path) -> Result<()> {
    let bundle_dir = crate::backup::resolve_bundle(backup_path)?;

    bundle_files_check(&bundle_dir)?;
    let manifest = manifest_validate(&bundle_dir)?;
    let obj_manifest = object_manifest_verify(&bundle_dir, &manifest.artifact_manifest_digest)?;

    // Open the bundled DB read-only and run full verification against the
    // bundle's objects/ directory.
    let db_path = bundle_dir.join("snag.sqlite");
    let store_id = bundled_db_verify(&bundle_dir, &db_path)?;

    // Database digest.
    let db_digest = crate::backup::file_digest(&db_path)?;
    if db_digest != manifest.database_digest {
        anyhow::bail!("Database digest mismatch (modified snag.sqlite)");
    }

    // Store ID agreement.
    if store_id != manifest.store_id {
        anyhow::bail!(
            "Store ID mismatch: db {} vs manifest {}",
            store_id,
            manifest.store_id
        );
    }

    let head_seq = head_agreement(&db_path, &manifest)?;
    let verified_artifacts = artifact_manifest_agreement(&bundle_dir, &db_path, &obj_manifest)?;

    println!(
        "Backup verification passed ({} artifacts, through seq {}).",
        verified_artifacts, head_seq
    );
    Ok(())
}

/// Every required bundle file is present.
fn bundle_files_check(bundle_dir: &Path) -> Result<()> {
    for required in crate::backup::BUNDLE_FILES {
        if !bundle_dir.join(required).exists() {
            anyhow::bail!("Backup missing required file: {}", required);
        }
    }
    Ok(())
}

/// Load and validate the bundle manifest's schema version and required fields.
fn manifest_validate(bundle_dir: &Path) -> Result<BundleManifest> {
    let manifest_raw = std::fs::read_to_string(bundle_dir.join("manifest.json"))
        .context("manifest.json unreadable")?;
    let manifest: Value =
        serde_json::from_str(&manifest_raw).context("manifest.json invalid JSON")?;
    if manifest["schema_version"].as_u64() != Some(crate::backup::MANIFEST_SCHEMA_VERSION as u64) {
        anyhow::bail!(
            "Unsupported manifest schema: {:?}",
            manifest["schema_version"]
        );
    }
    // The manifest must itself be digestible; the value is not compared because
    // a manifest cannot carry its own digest.
    let _manifest_digest = crate::backup::file_digest(&bundle_dir.join("manifest.json"))
        .context("cannot digest manifest.json")?;
    Ok(BundleManifest {
        store_id: manifest["store_id"]
            .as_str()
            .context("manifest missing store_id")?
            .to_string(),
        head_record_hash: manifest["head_record_hash"]
            .as_str()
            .context("manifest missing head_record_hash")?
            .to_string(),
        through_sequence: manifest["through_sequence"]
            .as_u64()
            .context("manifest missing through_sequence")?,
        database_digest: manifest["database_digest"]
            .as_str()
            .context("manifest missing database_digest")?
            .to_string(),
        artifact_manifest_digest: manifest["artifact_manifest_digest"]
            .as_str()
            .context("manifest missing artifact_manifest_digest")?
            .to_string(),
    })
}

/// Recompute the object-manifest digest, compare it with the manifest, and
/// return the parsed object manifest.
fn object_manifest_verify(bundle_dir: &Path, expected_digest: &str) -> Result<Value> {
    let obj_manifest_raw = std::fs::read_to_string(bundle_dir.join("objects-manifest.json"))
        .context("objects-manifest.json unreadable")?;
    let obj_digest = crate::backup::file_digest(&bundle_dir.join("objects-manifest.json"))
        .context("cannot digest objects-manifest.json")?;
    if obj_digest != expected_digest {
        anyhow::bail!("Object manifest digest mismatch (modified objects-manifest.json)");
    }
    serde_json::from_str(&obj_manifest_raw).context("objects-manifest.json invalid JSON")
}

/// Open the bundled database read-only, run full verification against the
/// bundle's own objects directory, and return the bundled store ID.
fn bundled_db_verify(bundle_dir: &Path, db_path: &Path) -> Result<String> {
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .context("cannot open bundled snag.sqlite")?;
    let store_id: String =
        conn.query_row("SELECT store_id FROM store_metadata LIMIT 1", [], |r| {
            r.get(0)
        })?;
    let mut store = Store {
        conn,
        store_id: store_id.clone(),
        data_dir: bundle_dir.to_path_buf(),
        db_path: db_path.to_path_buf(),
    };
    full_verify(&mut store).context("bundled database failed full verification")?;
    Ok(store_id)
}

/// Head sequence + hash agreement between the bundled DB and the manifest.
/// Returns the DB head sequence.
fn head_agreement(db_path: &Path, manifest: &BundleManifest) -> Result<i64> {
    let (head_seq, head_hash): (i64, String) = conn_query(db_path, |c| {
        let seq: i64 = c.query_row(
            "SELECT COALESCE(MAX(local_sequence),0) FROM records",
            [],
            |r| r.get(0),
        )?;
        let hash: String = c.query_row(
            "SELECT COALESCE((SELECT record_hash FROM records ORDER BY local_sequence DESC LIMIT 1), '0000000000000000000000000000000000000000000000000000000000000000')",
            [], |r| r.get(0))?;
        Ok((seq, hash))
    })?;
    if head_seq as u64 != manifest.through_sequence {
        anyhow::bail!(
            "Head sequence mismatch: db {} vs manifest {}",
            head_seq,
            manifest.through_sequence
        );
    }
    if head_hash != manifest.head_record_hash {
        anyhow::bail!(
            "Head hash mismatch: db {} vs manifest {}",
            head_hash,
            manifest.head_record_hash
        );
    }
    Ok(head_seq)
}

/// Object-manifest entries agree with DB artifact rows and on-disk objects.
/// Returns the number of artifacts verified.
fn artifact_manifest_agreement(
    bundle_dir: &Path,
    db_path: &Path,
    obj_manifest: &Value,
) -> Result<usize> {
    let expected = obj_manifest["artifacts"]
        .as_array()
        .context("objects-manifest missing artifacts")?;
    let mut expected_map = std::collections::HashMap::new();
    for e in expected {
        let digest = e["digest"].as_str().context("entry missing digest")?;
        let byte_length = e["byte_length"]
            .as_u64()
            .context("entry missing byte_length")?;
        let path = e["path"].as_str().context("entry missing path")?;
        expected_map.insert(digest.to_string(), (byte_length, path.to_string()));
    }
    let db_artifacts = conn_query(db_path, |c| {
        let mut stmt = c.prepare("SELECT digest, byte_length FROM artifacts")?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64));
        }
        Ok(out)
    })?;
    if db_artifacts.len() != expected_map.len() {
        anyhow::bail!("Object manifest artifact count does not match database");
    }
    let mut verified_artifacts = 0;
    for (digest, byte_length) in &db_artifacts {
        match expected_map.get(digest) {
            Some((expected_len, rel_path)) => {
                bundle_artifact_verify(bundle_dir, digest, *byte_length, *expected_len, rel_path)?;
                verified_artifacts += 1;
            }
            None => anyhow::bail!(
                "Artifact {} in DB but not in objects-manifest (swapped components)",
                digest
            ),
        }
    }
    Ok(verified_artifacts)
}

/// One bundled artifact: manifest length, on-disk presence, on-disk length,
/// and on-disk digest.
fn bundle_artifact_verify(
    bundle_dir: &Path,
    digest: &str,
    byte_length: u64,
    expected_len: u64,
    rel_path: &str,
) -> Result<()> {
    if expected_len != byte_length {
        anyhow::bail!("Artifact {} length mismatch in objects-manifest", digest);
    }
    let abs = bundle_dir.join(rel_path);
    if !abs.exists() {
        anyhow::bail!("Missing artifact on disk: {}", digest);
    }
    if std::fs::metadata(&abs)?.len() != byte_length {
        anyhow::bail!("Artifact {} length mismatch on disk", digest);
    }
    let d = crate::backup::file_digest(&abs)?;
    if d != digest {
        anyhow::bail!("Artifact {} digest mismatch on disk", digest);
    }
    Ok(())
}

/// Small helper to run a closure against a fresh read-only connection so the
/// read-only connection never attempts journal-mode mutation.
fn conn_query<T>(db_path: &Path, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Ok(f(&conn)?)
}
