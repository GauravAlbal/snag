# snag

Durable observation capture for agent fleets. `snag` records atomic bug/friction
reports — with repository, worktree, pearl, attempt, model, and tool context —
into a local, verifiable, backup/restore-capable store.

Snag is the **capture layer** of the constellation:

```text
Snag        → durable observation capture
Panopticon  → coalescing, taxonomy, materiality, issue synchronization
VX          → promoted work and execution
Moat        → acceptance evidence
```

Report issues while evidence is fresh; **do not broaden the current task to fix
them** unless explicitly instructed.

## Build

```sh
cargo build --release        # binary: target/release/snag
```

Requires Rust stable (edition 2024). SQLite is bundled (`rusqlite` `bundled`
feature) — no system dependency.

## Install

```sh
cargo install --path .       # or copy target/release/snag onto PATH
```

Snag stores observations in a local SQLite database under your platform data
directory (`~/.local/share/snag` on macOS/Linux). The first run creates the
store and a repository identity.

## Usage

Fast path:

```sh
snag "flake: vendored build script breaks on fresh clone"
```

Full report:

```sh
snag report "<specific symptom>" \
  --kind <kind> \
  --severity <blocker|major|minor> \
  --observed "<what happened>" \
  --expected "<what should have happened>" \
  --repro "<minimal reproduction, when known>" \
  --workaround "<workaround used, when any>" \
  --idempotency-key "<stable attempt-local key>"
```

### Commands

| Command | Purpose |
|---|---|
| `snag report` | Durably record one observation |
| `snag list` | List captured observations (filters: `--repo --since --source --kind --limit --format`) |
| `snag show <id>` | Display the immutable payload, context, and artifacts |
| `snag context` | Show what context would be attached from the current process |
| `snag export` | Produce deterministic Panopticon-ready records (JSONL) |
| `snag backup` | Create and verify a point-in-time backup + manifest |
| `snag restore <dir>` | Non-destructively restore from a backup (forensic copy preserved) |
| `snag rebuild --from-export <stream> --destination <dir>` | Rebuild a store from an export stream |
| `snag verify` | Verify SQLite integrity and the observation hash chain |
| `snag doctor` | Check configuration, backup freshness, and system context |
| `snag retract <id>` | Add a retraction action (original observation is never deleted) |

### Context

Context is inherited automatically:

- Git repository identity (repo/checkout/worktree IDs, branch, HEAD) from the
  current checkout.
- Session context via `SNAG_CONTEXT_FILE` (JSON) or `VX_*` / `ARQ_*` /
  `SNAG_*` environment variables — see `src/context.rs` and the installed
  `snag-ctx` helper.
- Set `SNAG_SOURCE_KIND=agent_report` and `SNAG_REPORTER_ID=<agent>` for agent
  captures.

Run `snag context` to inspect what would be attached.

## Architecture

```text
src/
  main.rs        CLI entry + command dispatch
  report.rs      capture pipeline (parse, merge, persist)
  context.rs     context-file + env overlay, precedence rules
  identity.rs    git repo/checkout/worktree identity resolution
  record.rs      canonical record encoding (hash-chained)
  schema.rs      SQLite schema + versioning
  store.rs       read/write/maintenance connection modes
  export.rs      deterministic JSONL export protocol
  backup.rs      point-in-time backup + manifest
  restore.rs     non-destructive restore with forensic copy
  rebuild.rs     rebuild store from export stream
  verify.rs      full/quick integrity verification
  migrations.rs  deterministic, collision-safe schema migrations
  parser.rs      prose/JSON input parsing
  idempotency.rs stable semantic idempotency keys
```

Records are hash-chained (`previous_record_hash`), giving tamper-evident,
globally-sequenced history. `verify --full` recomputes the whole chain.

## Documentation

- [Runbook](docs/RUNBOOK.md) — build, install, operations, recovery
- [Releasing](docs/RELEASING.md) — versioning and release process
- [CHANGELOG](CHANGELOG.md) — version history
- [Certification requirement digest](.cert-reqs-digest.md) — the v0 certification contract
- [AUDIT.md](AUDIT.md) — post-certification audit state

## License

MIT — see [LICENSE](LICENSE).
