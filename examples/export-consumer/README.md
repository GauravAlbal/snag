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

## GitHub issues adapter (simple, deliberately)

[`github-issues.py`](github-issues.py) files one issue per non-retracted
observation via the GitHub REST API:

```bash
snag export --output observations.jsonl
GITHUB_TOKEN=ghp_... python3 github-issues.py \
  --stream observations.jsonl --repo owner/name --label snag
python3 github-issues.py --stream observations.jsonl --repo owner/name --dry-run
```

**This is a deliberately simple one-observation-per-issue adapter, not the
canonical architecture.** Snag's model preserves raw observations first and
coalesces them into findings downstream; filing every observation directly is
right for a small repo with modest volume and wrong for a high-volume fleet
(issue inflation, duplicate symptoms, premature root-cause commitments). Use
it to get visibility quickly on a small repo; do not build your pipeline on
it. It does not look at existing issues — re-running without a checkpoint
files duplicates.

## Writing your own

The contract you must implement is small and stable:

1. Read the header; refuse streams with `export_schema_version != 1` or
   `minimum_reader_version > 1`.
2. Iterate records; enforce sequence contiguity and hash shape.
3. Persist `{last_sequence, last_record_hash}` after each successful pass.
4. Resume with `snag export --after-sequence N`.
