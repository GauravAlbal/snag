# Snag

<div align="center">

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)
![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux-lightgrey.svg)
![Version](https://img.shields.io/badge/version-0.2.0--dev-blue.svg)

</div>

**Snag is a durable observation outbox for coding agents.**

When an agent encounters an out-of-scope bug or recurring workaround, it records
the evidence and continues its assigned task. Reports remain local, survive
crashes, and can be exported to any downstream issue or analysis system.

**Supported agents:** [Claude Code](examples/agents/claude-code.md),
[Codex CLI](examples/agents/codex.md), [Gemini CLI](examples/agents/gemini-cli.md),
[OpenCode](examples/agents/opencode.md), and any
[shell-based agent](examples/agents/generic.md) — the integration is a short
instruction block plus an optional context file, nothing more.

---

## Quick install

**Release binaries** (recommended) — download the platform binary for the
latest release from the [releases page](https://github.com/GauravAlbal/snag/releases),
verify with `shasum -a 256 -c SHA256SUMS.txt`, or use the one-line installer:

```bash
curl -fsSL https://raw.githubusercontent.com/GauravAlbal/snag/main/install.sh | bash
```

The installer downloads the platform binary from the latest GitHub release
and verifies its SHA-256 checksum.

**From source** — two contracts, keep them distinct:

```bash
# Reproducible released version (v0.1.0)
cargo install --git https://github.com/GauravAlbal/snag --tag v0.1.0

# Current unreleased main (reports itself as 0.2.0-dev)
cargo install --git https://github.com/GauravAlbal/snag
```

Release binaries are published for macOS (arm64) and Linux (x86_64, aarch64),
each with a SHA256SUMS.txt and the source SHA of the build.

## Thirty-second example

```bash
snag report "build reports success but produces no artifact" \
  --kind bug \
  --observed "command exited 0; dist/app does not exist" \
  --expected "successful build creates dist/app" \
  --repro "run make release in a fresh clone"
```

```bash
snag list
snag show <observation-id>
snag verify --full
```

## How it works

```text
agent encounters an out-of-scope issue
→ records it with Snag
→ continues the current task
→ observations are reviewed or exported later
```

**Snag is evidence capture, not issue capture.** Some agent tools send repository
friction straight to a GitHub issue tracker. Snag preserves observations across
repositories and concurrent agents first — so separate manifestations of the
same underlying problem can be coalesced before anything becomes an owned
issue. Filing issues is an explicit, downstream, opt-in step; a deliberately
simple one-observation-per-issue adapter ships as an example
([github-issues.py](examples/export-consumer/github-issues.py)), not as the
canonical pipeline.

> "The bottleneck wasn't tracking work. It was deciding which repeated
> observations actually deserved to become work."
>
> — feedback from live agent use; see the [observation pipeline](docs/PIPELINE.md)
> for the full model (observation → coalescing → ranking → execution candidate)
> and the [dogfood case study](docs/CASE_STUDY.md).

Snag is deliberately boring. It does one job — durable local capture — and
stays out of the way while you work.

## Why use it

| Feature | What it does |
|---|---|
| Durable local capture | One command records a finding with full context; nothing is uploaded anywhere |
| Tamper-evident history | Records are hash-chained (`previous_record_hash`) and globally sequenced; `verify --full` recomputes the whole chain |
| Context auto-attached | Git repo/checkout/worktree identity, branch, and HEAD are captured from the current checkout |
| Crash-safe | Writes are transactional; a killed process leaves either nothing or one complete observation |
| Recovery chain | `backup` → `restore` (non-destructive, forensic copy preserved) → `rebuild` from an export stream |
| Deterministic export | Identical store state produces byte-identical JSONL — safe to diff, checksum, and checkpoint |
| Append-only retraction | `retract` appends a retraction record; the original observation is never deleted |
| Zero telemetry | No analytics, no phone-home, no account, no cloud |

## Data captured

Each observation stores: title, summary, kind and severity assertions, expected
vs observed behavior, reproduction, workaround, impact, confidence, sensitivity,
labels, and optional attached artifacts. Context is inherited automatically:

- Git repository identity (repo/checkout/worktree IDs, branch, HEAD) from the
  current checkout.
- Session context via the `SNAG_CONTEXT_FILE` environment variable — a
  versioned JSON document (see [docs/SCHEMAS.md](docs/SCHEMAS.md) and
  [schemas/](schemas/)).
- Set `SNAG_SOURCE_KIND=agent_report` and `SNAG_REPORTER_ID=<agent>` for agent
  captures.

Run `snag context` to see exactly what would be attached from the current
process.

## Privacy

- Snag is **local-only by default**. It sends no telemetry and never uploads
  observations automatically.
- It does **not** capture environment variables or shell history.
- Artifacts are copied into the store **only when explicitly attached** with
  `--artifact`.
- Git remotes, paths, branch names, model/tool IDs, and context may be stored —
  treat the store like a code review log.
- Retraction is append-only: it never deletes or rewrites the original
  observation.

See [SECURITY.md](SECURITY.md) for the full threat model.

## Store location

Snag keeps everything in one directory. `snag doctor` prints the exact paths on
your machine:

| Platform | Store directory |
|---|---|
| macOS | `~/Library/Application Support/snag-cli` |
| Linux (default) | `~/.local/share/snag-cli` |
| Linux (`XDG_DATA_HOME` set) | `$XDG_DATA_HOME/snag` |

Inside it: `snag.sqlite` (the database), `objects/blake3/` (content-addressed
artifacts), and `backups/` (point-in-time bundles).

## Commands

| Command | Purpose |
|---|---|
| `snag report "<title>"` | Durably record one observation (fast path; structured flags work without the subcommand) |
| `snag init` | Install the capture-and-move-on agent instructions into the current repo (idempotent, `--agent`/`--file`/`--dry-run`) |
| `snag report --json` | Record from a JSON document on stdin or a file (see [schemas/](schemas/observation-input-v1.schema.json)) |
| `snag list` | List observations (`--repo --since --source --kind --limit --format json`) |
| `snag show <id>` | Display the immutable payload, context, and artifacts |
| `snag context` | Show what context would be attached from the current process |
| `snag export` | Deterministic JSONL stream (full or `--after-sequence N` partial) |
| `snag backup` | Create and verify a point-in-time backup bundle |
| `snag restore <dir>` | Non-destructively restore from a backup (forensic copy preserved) |
| `snag rebuild --from-export <stream> --destination <dir>` | Rebuild a store from an export stream |
| `snag verify` | Verify SQLite integrity and the observation hash chain (`--full` recomputes everything) |
| `snag doctor` | Print store paths, effective context source, version, and health |
| `snag retract <id>` | Append a retraction (the original observation is never deleted) |

## Durability model

Observations are append-only and hash-chained: every record binds to its
predecessor, so tampering breaks the chain and `snag verify --full` reports it.
Retractions append actions rather than deleting. Backups are self-contained
bundles (database + manifest + objects) that `snag restore` verifies fully
before touching active state; `snag rebuild` reconstructs a store from an
export stream. Details: [docs/RUNBOOK.md](docs/RUNBOOK.md).

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

## Recovery

| Symptom | Procedure |
|---|---|
| Corrupt or missing store | `snag doctor` to assess; restore the latest backup (`snag backup` list, then `snag restore <dir>`) |
| No backup exists | Rebuild from the last export: `snag rebuild --from-export <stream> --destination <dir>` |
| Suspicious integrity | `snag verify --full`; if the chain breaks, the store is tampered or corrupt — restore from backup |

**Never delete the only copy of a store before a verified backup exists.**

## Stability

Snag is at v0.1. The CLI, observation JSON, context JSON, and export stream are
versioned contracts; the SQLite schema is internal. Downstream systems must
consume `snag export` — never open `snag.sqlite` directly. Guarantees:
[docs/STABILITY.md](docs/STABILITY.md).

## Non-goals

Snag is **not**:

- an issue tracker;
- an agent orchestrator;
- an analytics system;
- a tracing platform;
- a telemetry collector;
- a replacement for GitHub Issues;
- an automatic bug detector;
- an LLM-based deduplicator.

It captures observations durably. Everything downstream — triage, dedup,
routing, fixing — belongs to other tools that consume the export stream.

## Extending Snag

- **Context protocol** — [docs/SCHEMAS.md](docs/SCHEMAS.md) + [schemas/](schemas/)
- **Agent integrations** — [examples/agents/](examples/agents/)
- **Downstream consumers** — [examples/export-consumer/](examples/export-consumer/)
- **Roadmap** — [docs/ROADMAP.md](docs/ROADMAP.md)

## Case study

Snag was born from a two-hour dogfood run that captured 87 observations across
5 repositories — including cancellation, timeout, spend-control, stale-state,
recovery, and interface failures — two of which were bugs in Snag itself and
are fixed in this release. The lesson: agents were already finding these
issues; Snag changed whether the findings survived the session. Read the
anonymized account in [docs/CASE_STUDY.md](docs/CASE_STUDY.md).

## Demo

See [docs/DEMO.md](docs/DEMO.md) for a five-minute walkthrough: capture,
inspect, verify, and export — plus the reliability story.

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) first, file
bugs via the [issue templates](.github/ISSUE_TEMPLATE/), and report security
vulnerabilities privately per [SECURITY.md](SECURITY.md). This project follows
the [Contributor Covenant](CODE_OF_CONDUCT.md). Support: [SUPPORT.md](SUPPORT.md).

## License

Apache-2.0 — see [LICENSE](LICENSE). The name "Snag" is the OSS command and
repository identifier; see [naming and attribution](TRADEMARKS.md) for how
the name may be used. Contributions are accepted under the Developer
Certificate of Origin (signed-off commits, see
[CONTRIBUTING.md](CONTRIBUTING.md)).
