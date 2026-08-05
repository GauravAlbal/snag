# Stability guarantees (v0.x)

What external systems may rely on — and what they may not.

## The rule in one line

**Consume `snag export`. Never open `snag.sqlite` directly.** The SQLite schema
is an internal implementation detail and can change in any release; the export
stream is the public API.

## Guarantees by surface

| Surface | Guarantee in v0.x |
|---|---|
| CLI commands | **Best-effort stability.** Flags are additive within a minor; removing or renaming a flag is a MAJOR. Between minors, help text and output wording may change. `--format json` outputs are versioned envelopes. |
| Observation JSON | **Versioned.** Every observation carries `schema_version` (currently 1). New optional fields may be added in a minor; removing or renaming a field is a MAJOR. |
| Context JSON | **Versioned.** `SNAG_CONTEXT_FILE` documents carry `schema_version` (currently 1). A document with an unsupported version is rejected with a typed error, never misparsed. Unknown fields are ignored (documented compatibility rule). |
| Export stream | **Versioned and backward-readable.** `export_schema_version` / `minimum_reader_version` header fields govern compatibility; `snag rebuild` refuses unsupported versions rather than guessing. Identical store state + bounds produce byte-identical output. |
| SQLite schema | **Internal, not a public API.** Can change in any release. Downstream systems must not read it, write it, or depend on its table layout. |
| Hash encoding | **Frozen per encoding version.** Record hashes are `blake3:<64-hex>` over the canonical encoding (versioned `canonical_record_v1`). Within encoding version 1 the encoding is stable; a change to the encoding is a MAJOR and introduces a new encoding version. |

## What a MAJOR bump means

A MAJOR version bump signals that at least one of the versioned surfaces above
changed incompatibly (or the store/export/CLI contract broke). Downstream
consumers should treat the export stream's `export_schema_version` and
`minimum_reader_version` as the compatibility oracle: read them, and refuse to
process a stream you do not understand.

## Practical rules for downstream consumers

1. Read the export header first; validate `export_schema_version == 1` and
   `minimum_reader_version <= 1`.
2. Validate sequence contiguity and the hash chain as you iterate.
3. Checkpoint the last `local_sequence` and `record_hash`, and resume with
   `snag export --after-sequence N`.
4. Treat the checkpointed hash as an integrity anchor, not as a database
   cursor into `snag.sqlite`.

A ready-made consumer is in [examples/export-consumer/](../examples/export-consumer/).
