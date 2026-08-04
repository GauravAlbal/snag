use crate::cli::ReportArgs;
use crate::context::gather_context;
use crate::error::SnagError;
use crate::parser::{parse_prose, JsonInput};
use crate::store::Store;
use crate::artifacts::ArtifactStorage;
use crate::types::{Observation, ArtifactReference, Sensitivity, generate_id};
use anyhow::Result;
use serde_json::json;
use std::io::{self, Read};

pub fn handle(args: ReportArgs) -> Result<()> {
    // 1. Parse input
    let mut title = args.title.clone();
    let mut summary = None;
    let mut expected_behavior = args.expected.clone();
    let mut observed_behavior = args.observed.clone();
    let mut workaround = args.workaround.clone();
    let mut repro = args.repro.clone();
    let mut kind = args.kind.clone();
    let mut severity = args.severity.clone();
    let mut idempotency_key = args.idempotency_key.clone();
    let mut affected_repos = args.affected_repos.clone();
    let mut impact = None;
    
    if args.stdin {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer).map_err(|e| SnagError::Other(e.into()))?;
        let parsed = parse_prose(&buffer);
        if !parsed.title.is_empty() && title.is_none() {
            title = Some(parsed.title);
        }
        if parsed.summary.is_some() { summary = parsed.summary; }
        if parsed.expected.is_some() { expected_behavior = parsed.expected; }
        if parsed.observed.is_some() { observed_behavior = parsed.observed; }
        if parsed.repro.is_some() { repro = parsed.repro; }
        if parsed.workaround.is_some() { workaround = parsed.workaround; }
        if parsed.impact.is_some() { impact = parsed.impact; }
    }
    
    if args.json {
        let path = args.title.clone().unwrap_or_else(|| "-".to_string());
        let mut buffer = String::new();
        if path == "-" {
            io::stdin().read_to_string(&mut buffer).map_err(|e| SnagError::Other(e.into()))?;
        } else {
            buffer = std::fs::read_to_string(&path).map_err(|e| SnagError::Validation(format!("Could not read JSON file: {}", e)))?;
        }
        
        let json_input: JsonInput = serde_json::from_str(&buffer).map_err(|e| SnagError::Validation(format!("Invalid JSON: {}", e)))?;
        
        if let Some(sv) = json_input.schema_version {
            if sv != 1 {
                return Err(SnagError::UnsupportedSchema(sv.to_string()).into());
            }
        }
        
        if let Some(t) = json_input.title { title = Some(t); }
        if let Some(s) = json_input.summary { summary = Some(s); }
        if let Some(k) = json_input.kind_assertion { kind = Some(k); }
        if let Some(sev) = json_input.severity_assertion { severity = Some(sev); }
        if let Some(exp) = json_input.expected_behavior { expected_behavior = Some(exp); }
        if let Some(obs) = json_input.observed_behavior { observed_behavior = Some(obs); }
        if let Some(r) = json_input.reproduction { repro = Some(r); }
        if let Some(w) = json_input.workaround { workaround = Some(w); }
        if let Some(i) = json_input.impact { impact = Some(i); }
        if let Some(ik) = json_input.idempotency_key { idempotency_key = Some(ik); }
        if let Some(ar) = json_input.affected_repositories { affected_repos = ar; }
    }
    
    let title = title.unwrap_or_default();
    if title.is_empty() {
        return Err(SnagError::Validation("Title is required".to_string()).into());
    }

    // 2. Gather Context
    let (source, context, gathered_idempotency_key) = gather_context(&args)?;
    if idempotency_key.is_none() {
        idempotency_key = gathered_idempotency_key;
    }

    // 3. Artifact Storage setup
    let mut store = Store::open_read_write()?;
    let artifact_storage = ArtifactStorage::new(&store.data_dir)?;
    
    let mut artifacts = Vec::new();
    let mut total_artifact_bytes = 0_u64;
    for artifact_path in &args.artifacts {
        let (digest, size) = artifact_storage.ingest_file(artifact_path)?;
        total_artifact_bytes += size;
        if total_artifact_bytes > 250 * 1024 * 1024 {
            return Err(SnagError::Validation("Total artifacts size exceeds 250 MiB limit".to_string()).into());
        }
        
        let name = artifact_path.file_name().map(|n| n.to_string_lossy().into_owned());
        artifacts.push(ArtifactReference {
            digest,
            byte_length: size,
            media_type: None,
            original_name: name,
            created_at: time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap(),
        });
    }

    // 3.5. Identity resolution
    if let Some(repo_ctx) = &context.repository {
        let mut temp_git = crate::git::GitContext::default();
        temp_git.git_common_dir = repo_ctx.git_common_dir.clone();
        temp_git.git_remote_aliases = repo_ctx.git_remote_aliases.clone();
        temp_git.repository_root = repo_ctx.repository_root.clone();
        
        if let Ok(Some(repo_id)) = crate::identity::resolve_repository(&mut store, &temp_git) {
            if !affected_repos.contains(&repo_id) {
                affected_repos.push(repo_id);
            }
        }
    }

    // 4. Begin Transaction and allocate
    let tx = store.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    
    let local_sequence: i64 = tx.query_row(
        "SELECT COALESCE(MAX(local_sequence), 0) + 1 FROM records",
        [],
        |row| row.get(0),
    )?;
    
    let previous_record_hash: String = tx.query_row(
        "SELECT record_hash FROM records ORDER BY local_sequence DESC LIMIT 1",
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
        idempotency_key: idempotency_key.clone(),
        created_at: now.clone(),
        source,
        title,
        summary,
        kind_assertion: kind,
        severity_assertion: severity,
        expected_behavior,
        observed_behavior,
        reproduction: repro,
        workaround,
        impact,
        confidence: None,
        sensitivity: Sensitivity::Normal,
        labels: None,
        context,
        artifacts: artifacts.clone(),
    };
    
    let canonical_payload = serde_json::to_string(&obs)?;

    if let Some(ik) = &idempotency_key {
        let existing: rusqlite::Result<(String, String, i64, String)> = tx.query_row(
            "SELECT o.observation_id, r.canonical_payload_json, r.local_sequence, r.record_hash
             FROM observations o
             JOIN records r ON o.observation_id = r.record_id
             WHERE o.idempotency_key = ?1 AND r.record_type = 'observation_created'",
            rusqlite::params![ik],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        );
        if let Ok((old_id, old_payload, old_seq, old_hash)) = existing {
            // Compare semantic equality
            let mut old_val: serde_json::Value = serde_json::from_str(&old_payload)?;
            let mut new_val: serde_json::Value = serde_json::from_str(&canonical_payload)?;
            
            let strip = |v: &mut serde_json::Value| {
                if let Some(obj) = v.as_object_mut() {
                    obj.remove("observation_id");
                    obj.remove("store_id");
                    obj.remove("local_sequence");
                    obj.remove("created_at");
                    if let Some(arts) = obj.get_mut("artifacts").and_then(|a| a.as_array_mut()) {
                        for art in arts {
                            if let Some(a_obj) = art.as_object_mut() {
                                a_obj.remove("created_at");
                            }
                        }
                    }
                }
            };
            strip(&mut old_val);
            strip(&mut new_val);
            
            if old_val == new_val {
                if args.json {
                    let result = json!({
                        "schema_version": 1,
                        "observation_id": old_id,
                        "store_id": store.store_id,
                        "local_sequence": old_seq,
                        "record_hash": old_hash,
                        "created": false,
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
                    println!("Observation already exists: {}  [sequence {}]", old_id, old_seq);
                }
                return Ok(());
            } else {
                return Err(SnagError::Validation("Idempotency key collision with different payload".to_string()).into());
            }
        }
    }
    
    let mut hasher = blake3::Hasher::new();
    hasher.update(store.store_id.as_bytes());
    hasher.update(&local_sequence.to_le_bytes());
    hasher.update(previous_record_hash.as_bytes());
    hasher.update(canonical_payload.as_bytes());
    let record_hash = format!("blake3:{}", hasher.finalize().to_hex());

    tx.execute(
        "INSERT INTO records (local_sequence, record_id, record_type, entity_id, captured_at, canonical_payload_json, previous_record_hash, record_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            local_sequence,
            &obs.observation_id,
            "observation_created",
            &obs.observation_id,
            &obs.created_at,
            &canonical_payload,
            &previous_record_hash,
            &record_hash,
        ],
    )?;

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

    // Insert observation_repositories (affected repos)
    for repo_id in &affected_repos {
        tx.execute(
            "INSERT OR IGNORE INTO observation_repositories (observation_id, repository_id) VALUES (?1, ?2)",
            rusqlite::params![&obs.observation_id, repo_id],
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
    let store = Store::open_read_only()?;
    
    let mut query = String::from(
        "SELECT r.record_id, r.local_sequence, r.captured_at, o.title 
         FROM records r 
         JOIN observations o ON r.record_id = o.observation_id 
         WHERE r.record_type = 'observation_created'"
    );
    
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    
    if let Some(sk) = &args.source {
        query.push_str(" AND o.source_kind = ?");
        params.push(Box::new(sk.clone()));
    }
    
    if let Some(k) = &args.kind {
        query.push_str(" AND o.kind_assertion = ?");
        params.push(Box::new(k.clone()));
    }
    
    if let Some(repo) = &args.repo {
        query.push_str(" AND EXISTS (SELECT 1 FROM observation_repositories u JOIN repository_aliases a ON u.repository_id = a.repository_id WHERE u.observation_id = o.observation_id AND a.alias = ?)");
        params.push(Box::new(repo.clone()));
    }
    
    query.push_str(" ORDER BY r.local_sequence DESC");
    
    if let Some(limit) = args.limit {
        query.push_str(" LIMIT ?");
        params.push(Box::new(limit as i64));
    }
    
    let mut stmt = store.conn.prepare(&query)?;
    // Convert Vec<Box<dyn ToSql>> to a slice of references
    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut rows = stmt.query(param_refs.as_slice())?;
    
    if args.format.as_deref() == Some("json") {
        list_json(&mut rows)?;
    } else {
        list_table(&mut rows)?;
    }
    Ok(())
}

pub fn show(args: crate::cli::ShowArgs) -> anyhow::Result<()> {
    let store = Store::open_read_only()?;
    let payload: String = store.conn.query_row(
        "SELECT canonical_payload_json FROM records WHERE record_id = ?1 AND record_type = 'observation_created'",
        rusqlite::params![&args.observation_id],
        |row| row.get(0),
    )?;
    
    println!("{}", payload);
    Ok(())
}

pub fn retract(args: crate::cli::RetractArgs) -> anyhow::Result<()> {
    let mut store = Store::open_read_write()?;
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
        "SELECT COALESCE(MAX(local_sequence), 0) + 1 FROM records",
        [],
        |row| row.get(0),
    )?;
    
    let previous_record_hash: String = tx.query_row(
        "SELECT record_hash FROM records ORDER BY local_sequence DESC LIMIT 1",
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
        "INSERT INTO records (local_sequence, record_id, record_type, entity_id, captured_at, canonical_payload_json, previous_record_hash, record_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            local_sequence,
            &action_id,
            "observation_retracted",
            &args.observation_id,
            &now,
            &action_payload_json,
            &previous_record_hash,
            &record_hash,
        ],
    )?;

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
