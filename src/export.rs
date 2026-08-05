use crate::cli::ExportArgs;
use crate::store::Store;
use anyhow::{Context, Result};
use serde_json::json;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

pub fn handle(args: ExportArgs) -> Result<()> {
    let store = Store::open_read_only()?;
    handle_with_store(args, store)
}

const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Sequence bounds and chain anchors for the requested export window.
struct ExportBounds {
    min_seq: i64,
    actual_through_seq: i64,
    record_count: i64,
    first_seq: i64,
    head_hash: String,
    predecessor_hash: String,
}

fn compute_bounds(args: &ExportArgs, store: &Store) -> Result<ExportBounds> {
    let min_seq = args.after_sequence.map(|s| s + 1).unwrap_or(1) as i64;

    let (actual_through_seq, record_count): (i64, i64) = if let Some(ts) = args.through_sequence {
        store.conn.query_row(
            "SELECT COALESCE(MAX(local_sequence), 0), COUNT(*) FROM records WHERE local_sequence >= ?1 AND local_sequence <= ?2",
            rusqlite::params![min_seq, ts as i64],
            |row| Ok((row.get(0)?, row.get(1)?))
        )?
    } else {
        store.conn.query_row(
            "SELECT COALESCE(MAX(local_sequence), 0), COUNT(*) FROM records WHERE local_sequence >= ?1",
            rusqlite::params![min_seq],
            |row| Ok((row.get(0)?, row.get(1)?))
        )?
    };

    let head_hash: String = if actual_through_seq > 0 {
        store
            .conn
            .query_row(
                "SELECT record_hash FROM records WHERE local_sequence = ?1",
                rusqlite::params![actual_through_seq],
                |row| row.get(0),
            )
            .unwrap_or_else(|_| ZERO_HASH.to_string())
    } else {
        ZERO_HASH.to_string()
    };

    let first_seq = if record_count > 0 { min_seq } else { 0 };

    let predecessor_hash: String = if min_seq > 1 {
        store
            .conn
            .query_row(
                "SELECT record_hash FROM records WHERE local_sequence = ?1",
                rusqlite::params![min_seq - 1],
                |row| row.get(0),
            )
            .context("Predecessor record not found for partial export bounds")?
    } else {
        ZERO_HASH.to_string()
    };

    Ok(ExportBounds {
        min_seq,
        actual_through_seq,
        record_count,
        first_seq,
        head_hash,
        predecessor_hash,
    })
}

fn build_header(store: &Store, bounds: &ExportBounds) -> serde_json::Value {
    json!({
        "export_kind": "export_header",
        "export_schema_version": 1,
        "minimum_reader_version": 1,
        "store_id": store.store_id,
        "first_sequence": bounds.first_seq,
        "through_sequence": bounds.actual_through_seq,
        "previous_checkpoint_hash": bounds.predecessor_hash,
        "head_record_hash": bounds.head_hash,
        "record_count": bounds.record_count
    })
}

/// Open the export sink: a sibling temp file when `--output` is given (published
/// by `atomic_finalize`), otherwise stdout.
fn open_writer(args: &ExportArgs) -> Result<(Box<dyn Write>, Option<PathBuf>)> {
    if let Some(path) = &args.output {
        let mut temp_path = path.clone();
        temp_path.set_extension(format!("tmp.{}", ulid::Ulid::generate()));
        Ok((
            Box::new(BufWriter::new(File::create(&temp_path)?)),
            Some(temp_path),
        ))
    } else {
        Ok((Box::new(BufWriter::new(io::stdout())), None))
    }
}

/// Stream every record in `[min_seq, actual_through_seq]` as a record envelope.
/// Returns the number of records written.
fn write_records(
    store: &Store,
    bounds: &ExportBounds,
    writer: &mut Box<dyn Write>,
) -> Result<usize> {
    let mut stmt = store.conn.prepare("SELECT local_sequence, record_id, record_type, entity_id, captured_at, canonical_payload_json, previous_record_hash, record_hash FROM records WHERE local_sequence >= ?1 AND local_sequence <= ?2 ORDER BY local_sequence ASC")?;
    let mut rows = stmt.query(rusqlite::params![bounds.min_seq, bounds.actual_through_seq])?;

    let mut actual_count = 0;
    while let Some(row) = rows.next()? {
        let local_sequence: i64 = row.get(0)?;
        let record_id: String = row.get(1)?;
        let record_type: String = row.get(2)?;
        let entity_id: String = row.get(3)?;
        let captured_at: String = row.get(4)?;
        let payload_json: String = row.get(5)?;
        let previous_record_hash: String = row.get(6)?;
        let record_hash: String = row.get(7)?;

        let payload: serde_json::Value = serde_json::from_str(&payload_json)?;

        let record_envelope = json!({
            "export_kind": "record",
            "record_schema_version": 1,
            "local_sequence": local_sequence,
            "record_id": record_id,
            "record_type": record_type,
            "entity_id": entity_id,
            "captured_at": captured_at,
            "canonical_payload": payload,
            "previous_record_hash": previous_record_hash,
            "record_hash": record_hash
        });

        writeln!(writer, "{}", serde_json::to_string(&record_envelope)?)?;
        actual_count += 1;
    }

    Ok(actual_count)
}

/// Durably publish the temp file at its final path: fsync, then rename.
fn atomic_finalize(temp: PathBuf, output: PathBuf, actual_count: usize) -> Result<()> {
    // Sync and rename
    let file = File::open(&temp)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temp, output)?;
    eprintln!("Exported {} records.", actual_count);
    Ok(())
}

pub fn handle_with_store(args: ExportArgs, store: Store) -> Result<()> {
    let bounds = compute_bounds(&args, &store)?;
    let header = build_header(&store, &bounds);

    let (mut writer, temp_path) = open_writer(&args)?;

    writeln!(writer, "{}", serde_json::to_string(&header)?)?;

    // Fetch records
    let actual_count = write_records(&store, &bounds, &mut writer)?;

    writer.flush()?;
    drop(writer);

    if let Some(temp) = temp_path {
        atomic_finalize(temp, args.output.unwrap(), actual_count)?;
    }

    Ok(())
}
