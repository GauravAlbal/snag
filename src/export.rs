use crate::cli::ExportArgs;
use crate::store::Store;
use anyhow::Result;
use std::fs::File;
use std::io::{self, Write};
use serde_json::json;

pub fn handle(args: ExportArgs) -> Result<()> {
    let store = Store::open_read_only()?;
    
    let out_writer: Box<dyn Write> = if let Some(path) = &args.output {
        Box::new(File::create(path)?)
    } else {
        Box::new(io::stdout())
    };
    
    let mut writer = io::BufWriter::new(out_writer);
    
    let min_seq = args.after_sequence.map(|s| s + 1).unwrap_or(0) as i64;
    let max_seq = args.through_sequence.map(|s| s as i64).unwrap_or(i64::MAX);
    
    let predecessor_hash: String = if min_seq > 1 {
        store.conn.query_row(
            "SELECT record_hash FROM records WHERE local_sequence = ?1",
            rusqlite::params![min_seq - 1],
            |row| row.get(0),
        ).unwrap_or_else(|_| "0000000000000000000000000000000000000000000000000000000000000000".to_string())
    } else {
        "0000000000000000000000000000000000000000000000000000000000000000".to_string()
    };
    
    // Write header
    let header = json!({
        "export_kind": "stream_export",
        "store_id": store.store_id,
        "schema_version": 2,
        "start_sequence": min_seq,
        "end_sequence": max_seq,
        "predecessor_hash": predecessor_hash,
    });
    writeln!(writer, "{}", serde_json::to_string(&header)?)?;
    
    // Fetch records
    let mut stmt = store.conn.prepare("SELECT canonical_payload_json FROM records WHERE local_sequence >= ?1 AND local_sequence <= ?2 ORDER BY local_sequence ASC")?;
    let mut rows = stmt.query(rusqlite::params![min_seq, max_seq])?;
    
    let mut count = 0;
    while let Some(row) = rows.next()? {
        let payload: String = row.get(0)?;
        writeln!(writer, "{}", payload)?;
        count += 1;
    }
    
    writer.flush()?;
    
    // If output is not stdout, we might want to log progress to stderr
    if args.output.is_some() {
        eprintln!("Exported {} records.", count);
    }
    
    Ok(())
}
