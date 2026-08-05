# Snag Runbook

Operational procedures for building, installing, running, and recovering a snag
store. For project context see [../README.md](../README.md); for versioning see
[RELEASING.md](RELEASING.md).

## Build

```sh
cargo build --release        # binary: target/release/snag
```

Requirements: Rust stable (edition 2024). SQLite is bundled; no system deps.

The same build runs in CI on every push/PR. The three gate commands below are
**the** gate contract — they appear byte-identical in
`.github/workflows/ci.yml`. If you change one, change both.

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features --no-fail-fast
```

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/GauravAlbal/snag/main/install.sh | bash
# or
cargo install --git https://github.com/GauravAlbal/snag
# or build from this checkout
cargo install --path .
```

The installer downloads the platform binary from the latest GitHub release and
verifies its SHA-256 checksum before installing to `~/.local/bin` (override
with `--dest`). See `install.sh --help`.

### Uninstall

```sh
rm "$(command -v snag)"            # remove the binary
rm -rf ~/.local/share/snag-cli     # Linux (or $XDG_DATA_HOME/snag if set)
rm -rf ~/Library/Application\ Support/snag-cli   # macOS
```

Deleting the store directory is permanent — back it up first
(`snag backup`, then copy `backups/` somewhere safe) if the observations
matter.

## Context plumbing

- `snag` auto-detects git repository identity from the current checkout.
- `SNAG_CONTEXT_FILE` (JSON) supplies session context (session, task, tool,
  model, repository overrides). Precedence: explicit CLI > context file >
  environment > git auto-detect.
- `SNAG_SOURCE_KIND=agent_report` and `SNAG_REPORTER_ID=<agent>` mark a capture
  as agent-produced.
- Verify what would be captured: `snag context`.

The context document is a versioned public contract — see
[SCHEMAS.md](SCHEMAS.md) and the schemas in [../schemas/](../schemas/).

## Capture

Initialize an agent-aware repo (installs the capture-and-move-on instruction
block into `AGENTS.md`, idempotent, `--dry-run` to preview):

```sh
snag init
snag init --agent claude-code --file CLAUDE.md
```

Fast path:

```sh
snag "title"
```

Structured report (flags also work on the fast path):

```sh
snag report "<title>" --kind bug --severity minor \
  --observed "what happened" --expected "what should have happened" \
  --repro "minimal repro" --workaround "workaround" \
  --idempotency-key "attempt-local-key"
```

JSON intake:

```sh
snag report --json < input.json          # stdin
snag report --json ./input.json          # file
```

Reporting rubric: report when **unexpected + materially costly/risky +
plausibly systematic**. Do not report ordinary implementation failures,
transient mistakes, or in-scope issues. Capture, then continue the current
task — do not broaden it to fix the snag.

## Operations

### List and inspect

```sh
snag list                                    # all observations
snag list --repo <id|alias> --kind bug --since 7d --limit 50 --format json
snag show <observation-id>                   # immutable payload + context
snag retract <observation-id>                # retract without deleting
snag doctor                                  # paths, context source, health
```

### Export (the downstream API)

```sh
snag export --output observations.jsonl      # full stream
snag export --after-sequence 100 --through-sequence 200 --output partial.jsonl
```

Exports are deterministic: identical store state + bounds produce byte-identical
output. **Downstream systems must consume `snag export` — never open
`snag.sqlite` directly.** See the consumer example in
[../examples/export-consumer/](../examples/export-consumer/).

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

- SQLite database under the platform data directory: macOS
  `~/Library/Application Support/snag-cli`; Linux `~/.local/share/snag-cli`
  (or `$XDG_DATA_HOME/snag` when `XDG_DATA_HOME` is set). `snag doctor` prints
  the exact paths.
- Records are hash-chained (`previous_record_hash`) and globally sequenced;
  retractions append actions rather than deleting observations.
- Artifacts are content-addressed under `objects/blake3/<prefix>/<digest>` with
  size/digest caps; symlinks are rejected.
- Backups live in `backups/` as self-contained bundles
  (`snag.sqlite` + `manifest.json` + `objects-manifest.json` + `objects/`).

## Troubleshooting

| Problem | Check |
|---|---|
| Report fails with context-file error | `SNAG_CONTEXT_FILE` points at an unreadable/invalid file — unset it or fix the JSON (`snag context` shows the effective state) |
| `report --json "title"` seems to read a file | A title that is not an existing file is treated as the observation title with JSON output; only existing files are read as JSON intake |
| 32-writer concurrency test flakiness | `busy_timeout` is 30s; transient lock contention is expected under parallel load — retry |
| Git context hangs | Snag spawns git with a deadline and kills timed-out processes (bounded budget) — a failed git lookup never loses the report |
| `--json` error path | Errors emit a typed JSON envelope when `--json` is active; parse that rather than stderr |
