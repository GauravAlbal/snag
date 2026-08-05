# Snag Runbook

Operational procedures for building, installing, running, and recovering a
snag store. For project context see [../README.md](../README.md); for
versioning see [RELEASING.md](RELEASING.md).

## Build

```sh
cargo build --release        # binary: target/release/snag
```

Requirements: Rust stable (edition 2024). SQLite is bundled; no system deps.

The same build runs in CI on every push/PR. The three gate commands below are
**the** gate contract — they appear byte-identical in `.moat.json` and
`.github/workflows/ci.yml`. If you change one, change both.

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features --no-fail-fast
```

## Install

```sh
cargo install --path .       # installs `snag` on PATH
# or copy the release binary:
cp target/release/snag ~/.local/bin/snag
```

### Context plumbing (agent fleets)

- `snag` auto-detects git repository identity from the current checkout.
- `SNAG_CONTEXT_FILE` (JSON) or `VX_*` / `ARQ_*` / `SNAG_*` env vars supply
  session context (pearl, attempt, model, tool, session). Precedence:
  explicit CLI > context file > environment > git auto-detect.
- `snag-ctx` (installed alongside) generates a context file from the
  environment and `runs/packet_registry.jsonl`; the `snag` wrapper
  self-injects it when unset.
- Verify what would be captured: `snag context`.

## Capture

Fast path:

```sh
snag "title"
```

Structured report:

```sh
snag report "<title>" --kind bug --severity minor \
  --observed "what happened" --expected "what should have happened" \
  --repro "minimal repro" --workaround "workaround" \
  --idempotency-key "attempt-local-key"
```

Reporting rubric (from the global agent instructions): report when
**unexpected + materially costly/risky + plausibly systematic**. Do not report
ordinary implementation failures, transient mistakes, or in-scope issues.
Capture, then continue the current task — do not broaden it to fix the snag.

## Operations

### List and inspect

```sh
snag list                                    # all observations
snag list --repo <id|alias> --kind bug --since 7d --limit 50 --format json
snag show <observation-id>                   # immutable payload + context
snag retract <observation-id>                # retract without deleting
snag doctor                                  # config, backup freshness, context
```

### Export (Panopticon-ready)

```sh
snag export --output observations.jsonl      # full stream
snag export --after-sequence 100 --through-sequence 200 --output partial.jsonl
```

Exports are deterministic: identical store state + bounds produce
byte-identical output.

### Backup and restore (recovery chain)

```sh
snag backup                                  # creates + verifies a point-in-time backup
snag verify --backup <backup-dir>            # independent backup verification
snag restore <backup-dir>                    # non-destructive; preserves a forensic copy
```

Restore is safe by construction: it verifies the backup fully before touching
active state, restores into a temp candidate, runs full verification on the
candidate, then atomically switches. On any failure the active store is
unchanged.

### Rebuild from export

```sh
snag rebuild --from-export observations.jsonl --destination /tmp/rebuilt
```

Rebuild never modifies the active store. It validates the header, refuses
unsupported schema versions, recomputes every hash, and runs full verification
before finalizing.

### Integrity

```sh
snag verify          # quick: bounded suffix + predecessor-hash equality
snag verify --full   # complete: whole chain, FK, artifacts, metadata
```

## Recovery

| Symptom | Procedure |
|---|---|
| Corrupt or missing store | `snag doctor` to assess; restore the latest backup (`snag backup` list, then `snag restore <dir>`) |
| No backup exists | Rebuild from the last export: `snag rebuild --from-export <stream> --destination <dir>` |
| Store exists but won't open | Keep it untouched; `snag restore` into a temp candidate first; never overwrite the only copy |
| Suspicious integrity | `snag verify --full`; if the chain breaks, the store is tampered/corrupt — restore from backup |

**Never** delete the only copy of a store before a verified backup exists.

## Store layout

- SQLite database under the platform data directory
  (`~/.local/share/snag/` on macOS/Linux, per the `directories` crate).
- Records are hash-chained (`previous_record_hash`) and globally sequenced;
  retractions append actions rather than deleting observations.
- Artifacts are content-addressed with size/digest caps; symlinks are rejected.

## Troubleshooting

| Problem | Check |
|---|---|
| Report fails with context-file error | `SNAG_CONTEXT_FILE` points at an unreadable/invalid file — unset it or fix the JSON (`snag context` shows the effective state) |
| 32-writer concurrency test flakiness | `busy_timeout` is 30s; transient lock contention is expected under parallel load — retry |
| Git context hangs | Snag spawns git with a deadline and kills timed-out processes (bounded budget) — a failed git lookup never loses the report |
| `--json` error path | Errors emit a typed JSON envelope when `--json` is active; parse that rather than stderr |
