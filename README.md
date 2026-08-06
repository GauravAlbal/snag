# Snag

<div align="center">

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)
![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux-lightgrey.svg)

</div>

**Record the problem. Keep working. Hand it off with evidence.**

Snag is a local command-line tool for coding agents and developers.

When an agent finds a bug, broken command, misleading success message, recurring workaround, or tool failure outside its current task, it records what happened and continues the assigned work.

Later, another person or agent with access to the same Snag store can inspect the report, classify it, claim it, link a fix, and record verification. Nobody has to reconstruct the original session from chat logs.

![Snag demo](docs/assets/demo.gif)

## Install

The installer downloads the latest release, verifies its SHA-256 checksum, and installs `snag` to `~/.local/bin`.

```bash
curl -fsSL https://raw.githubusercontent.com/GauravAlbal/snag/main/install.sh | bash
```

Confirm the installation:

```bash
snag --version
snag doctor
```

The installer supports:

* macOS on Apple Silicon
* Linux on x86_64
* Linux on ARM64

See [Build from source](#build-from-source) for other platforms.

## Add Snag to your coding agent

Run `snag init` inside the repository where the agent works.

### Claude Code

```bash
snag init --agent claude-code --file CLAUDE.md
```

### Codex

```bash
snag init --agent codex --file AGENTS.md
```

### Gemini CLI

```bash
snag init --agent gemini-cli --file AGENTS.md
```

### OpenCode

```bash
snag init --agent opencode --file AGENTS.md
```

### Any shell-based agent

```bash
snag init --file AGENTS.md
```

`snag init` adds an idempotent instruction block. It tells the agent to record an unrelated problem while the evidence is fresh, then return to its assigned task.

Snag automatically records the current Git repository, branch, commit, checkout, and worktree. To identify the reporting agent, set:

```bash
export SNAG_SOURCE_KIND=agent_report
export SNAG_REPORTER_ID="codex"
```

A wrapper can provide richer session context through `SNAG_CONTEXT_FILE`. See [Agent integrations](examples/agents/) and [Context schemas](docs/SCHEMAS.md).

## Record a problem

```bash
snag report "build reports success but creates no artifact" \
  --kind bug \
  --observed "make release exited 0, but dist/app does not exist" \
  --expected "a successful release build creates dist/app" \
  --repro "run make release in a fresh clone"
```

The shorter form is equivalent:

```bash
snag "build reports success but creates no artifact" \
  --kind bug \
  --observed "make release exited 0, but dist/app does not exist" \
  --expected "a successful release build creates dist/app" \
  --repro "run make release in a fresh clone"
```

Snag stores the report as an **observation** and returns an observation ID.

A useful observation answers four questions:

1. What happened?
2. What should have happened?
3. How can someone reproduce it?
4. Is there a workaround?

Optional artifacts can be attached explicitly:

```bash
snag report "compiler crashes on generated schema" \
  --observed "compiler exited with signal 11" \
  --expected "compiler emits generated.rs" \
  --repro "run ./scripts/generate-schema.sh" \
  --artifact compiler.log
```

## Inspect observations

```bash
snag list
snag list --since 1d
snag show <observation-id>
```

Observation IDs can be shortened to any unique prefix.

Use `snag context` before reporting to see what repository and session context Snag will attach:

```bash
snag context
snag context --format json
```

## Pick up an observation later

A reviewer can claim the next unreviewed observation:

```bash
snag review next \
  --unreviewed \
  --claim \
  --reviewer repair-agent
```

The claim is a lease. It expires instead of leaving the observation permanently assigned when the reviewer disappears.

Inspect the complete evidence packet:

```bash
snag review show <observation-id>
snag review history <observation-id>
```

Classify the observation:

```bash
snag review disposition <observation-id> confirmed \
  --rationale "reproduced in a fresh checkout"
```

Other dispositions include:

* `duplicate`
* `expected-behavior`
* `environmental`
* `insufficient-evidence`
* `deferred`
* `superseded`

Related observations can be linked without merging or deleting their original records:

```bash
snag review relate <left-id> <right-id> \
  --relation same-finding \
  --rationale "both fail after the same stale-session transition"
```

## Link the fix and verification

Attach the owned task:

```bash
snag review attach-task <observation-id> \
  --task-id TASK-123
```

Attach the candidate fixing commit:

```bash
snag review attach-fix <observation-id> \
  --repo owner/repository \
  --commit <commit-sha>
```

Attach verification evidence:

```bash
snag review attach-verification <observation-id> \
  --receipt <verification-receipt> \
  --status accepted
```

After the recorded evidence shows that the observation has been handled:

```bash
snag review mark-handled <observation-id> \
  --rationale "fix landed and verification was accepted"
```

Every review action is append-only. Snag keeps the original observation and the complete decision history.

## Why not file every observation as an issue?

An issue tracker starts after someone has decided that a problem deserves owned work.

Snag starts earlier.

An agent may have found:

* a real defect;
* a duplicate symptom;
* expected behavior;
* an environmental failure;
* incomplete evidence;
* a workaround that matters only when it recurs.

Snag preserves the evidence first. Review determines what the observation means and whether it should become work.

Nothing is sent to GitHub automatically. When useful, export the local record stream and send it to another system:

```bash
snag export --output observations.jsonl
```

A simple GitHub Issues consumer is included at [`examples/export-consumer/github-issues.py`](examples/export-consumer/github-issues.py).

## What Snag records

Each observation can contain:

* title;
* observed behavior;
* expected behavior;
* reproduction steps;
* workaround;
* asserted kind and severity;
* labels;
* explicitly attached files;
* affected repositories;
* task, session, and attempt identifiers.

Snag automatically adds available Git context:

* repository identity;
* checkout and worktree identity;
* branch;
* current commit.

Review records can add:

* claims and releases;
* dispositions;
* relationships between observations;
* finding identifiers;
* task identifiers;
* fixing commits;
* verification receipts;
* handled and reopened states.

## Local by default

Snag has no account, cloud service, telemetry, or automatic upload.

It does not capture:

* environment-variable contents;
* shell history;
* arbitrary files;
* artifacts that were not explicitly attached.

Git remotes, paths, branch names, agent identifiers, and supplied session context may be stored. Treat the Snag store like a local code-review log.

Run `snag doctor` to print the exact store paths on the current machine.

| Platform                   | Default store                            |
| -------------------------- | ---------------------------------------- |
| macOS                      | `~/Library/Application Support/snag-cli` |
| Linux                      | `~/.local/share/snag-cli`                |
| Linux with `XDG_DATA_HOME` | `$XDG_DATA_HOME/snag`                    |

See [SECURITY.md](SECURITY.md) for the threat model and handling guidance.

## Integrity and recovery

Observations and review actions are globally sequenced and hash-chained. Changing or removing a record breaks the chain.

Verify the complete store:

```bash
snag verify --full
```

Create a verified backup:

```bash
snag backup
```

Restore a backup without deleting the current store:

```bash
snag restore <backup-archive>
```

Rebuild a store from an export:

```bash
snag rebuild \
  --from-export observations.jsonl \
  --destination <new-store-directory>
```

Never delete the only copy of a store before creating and verifying a backup.

See [Recovery runbook](docs/RUNBOOK.md) for detailed procedures.

## Command map

| Task                              | Commands                                                                  |
| --------------------------------- | ------------------------------------------------------------------------- |
| Configure an agent                | `snag init`                                                               |
| Record a problem                  | `snag report`, or `snag "<title>"`                                        |
| Inspect captured context          | `snag context`                                                            |
| Find and inspect observations     | `snag list`, `snag show`                                                  |
| Review the queue                  | `snag review next`, `list`, `show`, `history`                             |
| Claim work                        | `snag review claim`, `release`, `heartbeat`                               |
| Classify and relate observations  | `snag review disposition`, `reopen`, `relate`, `unrelate`                 |
| Link remediation                  | `snag review promote`, `attach-task`, `attach-fix`, `attach-verification` |
| Close or reopen remediation       | `snag review mark-handled`, `reopen-remediation`                          |
| Validate a completion report      | `snag review verify-report`                                               |
| Export records                    | `snag export`                                                             |
| Check integrity and configuration | `snag verify`, `snag doctor`                                              |
| Recover data                      | `snag backup`, `restore`, `rebuild`                                       |
| Retract an incorrect report       | `snag retract`                                                            |

Run `snag <command> --help` for complete arguments.

## Stable interfaces

The command-line interface, observation JSON, context JSON, review records, and export stream are versioned interfaces.

The SQLite schema is internal. Downstream tools should consume `snag export` rather than opening `snag.sqlite`.

See [Stability guarantees](docs/STABILITY.md).

## Build from source

Install the current release with Rust:

```bash
cargo install \
  --git https://github.com/GauravAlbal/snag \
  --tag v0.2.0 \
  --locked
```

Install unreleased `main` only when you intentionally want the development version:

```bash
cargo install \
  --git https://github.com/GauravAlbal/snag \
  --locked
```

## What Snag does not do

Snag is not:

* an automatic bug detector;
* a cloud telemetry service;
* a full project or dependency tracker;
* an agent orchestrator;
* an automatic deduplication or ranking system;
* a replacement for testing or verification.

Snag records problems, preserves their evidence, and carries their review and remediation history. Other tools can consume the export when broader planning or execution is needed.

## Documentation

* [Agent integrations](examples/agents/)
* [Context and observation schemas](docs/SCHEMAS.md)
* [Observation and review pipeline](docs/PIPELINE.md)
* [Five-minute demo](docs/DEMO.md)
* [Dogfood case study](docs/CASE_STUDY.md)
* [Recovery runbook](docs/RUNBOOK.md)
* [Stability guarantees](docs/STABILITY.md)
* [Roadmap](docs/ROADMAP.md)
* [Security policy](SECURITY.md)

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) before submitting a change.

Use the issue templates for bugs and feature requests. Report security vulnerabilities privately according to [SECURITY.md](SECURITY.md).

Contributions require a Developer Certificate of Origin sign-off.

## License

Apache-2.0. See [LICENSE](LICENSE).

The repository and command are named “Snag.” See [TRADEMARKS.md](TRADEMARKS.md) for naming and attribution rules.

