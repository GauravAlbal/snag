# Export consumer example

A minimal, stdlib-only Python consumer for the Snag export stream — the
reference for how a downstream system should read Snag observations.

## The rule

**Never open `snag.sqlite` directly.** The SQLite schema is internal and can
change in any release. The export stream is the public API:

```text
snag export  →  JSONL stream (header + hash-chained records)  →  your system
```

See [docs/STABILITY.md](../../docs/STABILITY.md) for the guarantees and
[schemas/export-stream-v1.schema.json](../../schemas/export-stream-v1.schema.json)
for the exact line shapes.

## One-shot validation

```bash
snag export --output observations.jsonl
python3 consumer.py --stream observations.jsonl
```

The consumer parses the header (validating `export_schema_version` and
`minimum_reader_version`), then iterates every record, checking:

- `local_sequence` is strictly contiguous (+1 per record);
- `record_id` is non-empty;
- `record_hash` / `previous_record_hash` match `blake3:<64 hex>` (or 64 zeros);

and exits `1` on the first violation. A valid stream prints a summary line.

## Poll loop with checkpoints

For a periodic sync, checkpoint the last validated sequence + hash and resume
incrementally:

```bash
# first pass
snag export --output observations.jsonl
python3 consumer.py --stream observations.jsonl --checkpoint state.json

# later passes — only new records
snag export --after-sequence "$(python3 -c 'import json;print(json.load(open("state.json"))["last_sequence"])')" \
  --output new.jsonl
python3 consumer.py --stream new.jsonl --checkpoint state.json
```

The checkpoint is the integrity anchor: resume only from the sequence the
checkpoint records, and treat the hash as a verification fingerprint, not as a
cursor into the database.

## Via pipe

```bash
snag export | python3 consumer.py --stream -
```

## Writing your own

The contract you must implement is small and stable:

1. Read the header; refuse streams with `export_schema_version != 1` or
   `minimum_reader_version > 1`.
2. Iterate records; enforce sequence contiguity and hash shape.
3. Persist `{last_sequence, last_record_hash}` after each successful pass.
4. Resume with `snag export --after-sequence N`.
