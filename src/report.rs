use crate::cli::ReportArgs;
use crate::context::gather_context;
use crate::store::Store;
use crate::artifacts::ArtifactStorage;
use crate::types::{Observation, ArtifactReference, Sensitivity, generate_id};
use anyhow::Result;
use serde_json::json;
use std::io::{self, Read};

pub fn handle(args: ReportArgs) -> Result<()> {
    // 1. Parse input
    let mut title = args.title.clone().unwrap_or_default();
    
    if args.stdin {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        // Simplistic parse: first line is title, rest is summary (if we wanted to parse it)
        // Since prose parsing is deliberately simple:
        if title.is_empty() {
            let mut lines = buffer.lines();
            if let Some(first) = lines.next() {
                title = first.to_string();
            }
        }
    }
    
    // JSON parsing would override everything
    if args.json {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer)?;
        // Try parsing JSON... for now, we just proceed.
    }
    
    if title.is_empty() {
        anyhow::bail!("Title is required");
    }

    // 2. Gather Context
    let (source, context) = gather_context(&args)?;

    // 3. Artifact Storage setup
    let mut store = Store::open()?;
    let artifact_storage = ArtifactStorage::new(&store.data_dir)?;
    
    let mut artifacts = Vec::new();
    for artifact_path in &args.artifacts {
        let (digest, size) = artifact_storage.ingest_file(artifact_path)?;
        let name = artifact_path.file_name().map(|n| n.to_string_lossy().into_owned());
        artifacts.push(ArtifactReference {
            digest,
            byte_length: size,
            media_type: None,
            original_name: name,
            created_at: time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap(),
        });
    }

    // 4. Begin Transaction and allocate
    let tx = store.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    
    let local_sequence: i64 = tx.query_row(
        "SELECT COALESCE(MAX(local_sequence), 0) + 1 FROM observations",
        [],
        |row| row.get(0),
    )?;
    
    let previous_record_hash: String = tx.query_row(
        "SELECT record_hash FROM observations ORDER BY local_sequence DESC LIMIT 1",
        [],
        |row| row.get(0),
    ).unwrap_or_else(|_| "0000000000000000000000000000000000000000000000000000000000000000".to_string());
    
    let obs_id = generate_id("obs");
    let now = time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap();
    
    let obs = Observation {
        schema_version: 1,
        observation_id: obs_id.clone(),
        store_id: store.store_id.clone(),
        local_sequence: local_sequence as u64,
        idempotency_key: args.idempotency_key.clone(),
        created_at: now.clone(),
        source,
        title,
        summary: None,
        kind_assertion: args.kind.clone(),
        severity_assertion: args.severity.clone(),
        expected_behavior: args.expected.clone(),
        observed_behavior: args.observed.clone(),
        reproduction: args.repro.clone(),
        workaround: args.workaround.clone(),
        impact: None,
        confidence: None,
        sensitivity: Sensitivity::Normal,
        labels: None,
        context,
        artifacts: artifacts.clone(),
    };
    
    let canonical_payload = serde_json::to_string(&obs)?;
    
    let mut hasher = blake3::Hasher::new();
    hasher.update(store.store_id.as_bytes());
    hasher.update(&local_sequence.to_le_bytes());
    hasher.update(previous_record_hash.as_bytes());
    hasher.update(canonical_payload.as_bytes());
    let record_hash = format!("blake3:{}", hasher.finalize().to_hex());

    tx.execute(
        "INSERT INTO observations (
            observation_id, store_id, local_sequence, schema_version, captured_at, source_kind,
            idempotency_key, title, kind_assertion, severity_assertion, expected_behavior,
            observed_behavior, reproduction, workaround, sensitivity, context_json,
            canonical_payload_json, previous_record_hash, record_hash
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19
        )",
        rusqlite::params![
            &obs.observation_id,
            &obs.store_id,
            obs.local_sequence as i64,
            obs.schema_version,
            &obs.created_at,
            &obs.source.kind,
            &obs.idempotency_key,
            &obs.title,
            &obs.kind_assertion,
            &obs.severity_assertion,
            &obs.expected_behavior,
            &obs.observed_behavior,
            &obs.reproduction,
            &obs.workaround,
            "normal",
            serde_json::to_string(&obs.context)?,
            &canonical_payload,
            &previous_record_hash,
            &record_hash,
        ],
    )?;

    // Insert artifacts
    for art in &obs.artifacts {
        tx.execute(
            "INSERT OR IGNORE INTO artifacts (digest, byte_length, media_type, original_name, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                &art.digest,
                art.byte_length as i64,
                &art.media_type,
                &art.original_name,
                &art.created_at,
            ],
        )?;
        tx.execute(
            "INSERT INTO observation_artifacts (observation_id, digest) VALUES (?1, ?2)",
            rusqlite::params![&obs.observation_id, &art.digest],
        )?;
    }

    tx.commit()?;

    if args.json {
        let result = json!({
            "schema_version": 1,
            "observation_id": obs.observation_id,
            "store_id": obs.store_id,
            "local_sequence": obs.local_sequence,
            "record_hash": record_hash,
            "created": true,
            "sync_state": "local",
            "context": {
                "repository": obs.context.repository.is_some(),
                "execution": obs.context.execution.is_some(),
            },
            "artifacts": obs.artifacts.len(),
            "warnings": []
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Recorded {}  [sequence {}]", obs.observation_id, obs.local_sequence);
        println!("artifacts: {}", obs.artifacts.len());
        println!("sync: local");
    }
    
    Ok(())
}

fn list_json(rows: &mut rusqlite::Rows) -> anyhow::Result<()> {
    let mut obs = Vec::new();
    while let Some(row) = rows.next()? {
        let observation_id: String = row.get(0)?;
        let local_sequence: i64 = row.get(1)?;
        let captured_at: String = row.get(2)?;
        let title: String = row.get(3)?;
        obs.push(json!({
            "observation_id": observation_id,
            "local_sequence": local_sequence,
            "captured_at": captured_at,
            "title": title
        }));
    }
    println!("{}", serde_json::to_string_pretty(&obs)?);
    Ok(())
}

fn list_table(rows: &mut rusqlite::Rows) -> anyhow::Result<()> {
    println!("{:<20} | {:<8} | {:<25} | {}", "ID", "Seq", "Date", "Title");
    println!("{:-<20}-+-{:-<8}-+-{:-<25}-+-{:-<30}", "", "", "", "");
    while let Some(row) = rows.next()? {
        let observation_id: String = row.get(0)?;
        let local_sequence: i64 = row.get(1)?;
        let captured_at: String = row.get(2)?;
        let title: String = row.get(3)?;
        println!("{:<20} | {:<8} | {:<25} | {}", observation_id, local_sequence, captured_at, title);
    }
    Ok(())
}

pub fn list(args: crate::cli::ListArgs) -> anyhow::Result<()> {
    let store = Store::open()?;
    let query = "SELECT observation_id, local_sequence, captured_at, title FROM observations".to_string();
    let mut stmt = store.conn.prepare(&query)?;
    let mut rows = stmt.query([])?;
    
    if args.format.as_deref() == Some("json") {
        list_json(&mut rows)?;
    } else {
        list_table(&mut rows)?;
    }
    Ok(())
}

pub fn show(args: crate::cli::ShowArgs) -> anyhow::Result<()> {
    let store = Store::open()?;
    let payload: String = store.conn.query_row(
        "SELECT canonical_payload_json FROM observations WHERE observation_id = ?1",
        rusqlite::params![&args.observation_id],
        |row| row.get(0),
    )?;
    
    println!("{}", payload);
    Ok(())
}

pub fn retract(args: crate::cli::RetractArgs) -> anyhow::Result<()> {
    let mut store = Store::open()?;
    let tx = store.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM observations WHERE observation_id = ?1)",
        rusqlite::params![&args.observation_id],
        |row| row.get(0),
    )?;
    
    if !exists {
        anyhow::bail!("Observation not found");
    }
    
    let local_sequence: i64 = tx.query_row(
        "SELECT COALESCE(MAX(local_sequence), 0) + 1 FROM observation_actions",
        [],
        |row| row.get(0),
    )?;
    
    let previous_record_hash: String = tx.query_row(
        "SELECT record_hash FROM observation_actions ORDER BY local_sequence DESC LIMIT 1",
        [],
        |row| row.get(0),
    ).unwrap_or_else(|_| "0000000000000000000000000000000000000000000000000000000000000000".to_string());
    
    let action_id = generate_id("act");
    let now = time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap();
    let action_type = "retracted";
    let action_payload_json = json!({"reason": "manual retraction"}).to_string();
    
    let mut hasher = blake3::Hasher::new();
    hasher.update(store.store_id.as_bytes());
    hasher.update(&local_sequence.to_le_bytes());
    hasher.update(previous_record_hash.as_bytes());
    hasher.update(action_payload_json.as_bytes());
    let record_hash = format!("blake3:{}", hasher.finalize().to_hex());
    
    tx.execute(
        "INSERT INTO observation_actions (action_id, observation_id, action_type, action_payload_json, created_at, local_sequence, previous_record_hash, record_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            &action_id,
            &args.observation_id,
            action_type,
            &action_payload_json,
            &now,
            local_sequence,
            &previous_record_hash,
            &record_hash,
        ],
    )?;
    
    tx.commit()?;
    
    println!("Retracted {}", args.observation_id);
    
    Ok(())
}
