# AGENTS.md — Snag

Guidelines for AI coding agents working in this repository. If you are an agent
being asked to USE Snag (not work on it), see [examples/agents/](examples/agents/).

## What this repo is

Snag is a durable observation outbox for coding agents. It records out-of-scope
bugs, regressions, misleading interfaces, recurring workarounds, and tooling
failures into a local, verifiable, append-only store, then lets the agent
continue its assigned task. Observations can be exported as a deterministic
JSONL stream for any downstream issue or analysis system.

## Build and test

Rust stable, edition 2024. SQLite is bundled; there are no system dependencies.

```sh
cargo build --release
```

The three gates are the contract — CI runs exactly these, and they must pass
before any change lands:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features --no-fail-fast
```

## Working in this repo

- **Scope discipline.** Make the smallest change that repairs the root cause.
  Do not bundle adjacent refactors; a change that removes a side-change must
  leave the named fix working (revert the side-change and re-run the test).
- **Tests describe the contract.** For any behavior change, write the test from
  the expected contract first — never by pasting current output. Integration
  tests drive the real binary against an isolated store (`tests/*.rs` use a
  temp `XDG_DATA_HOME`). A test that cannot fail on the old behavior is not
  evidence.
- **No hollow implementations.** No `todo!()`, `unimplemented!()`, bare
  `FIXME`/`HACK`, or placeholder branches in delivered work.
- **Read purity.** `list`, `show`, `context`, `export`, `verify`, and `doctor`
  must never mutate the store. If you touch a read path, the
  `test_read_purity` suite must stay green.
- **Stability.** The observation JSON, context JSON, and export stream are
  versioned public contracts (see [docs/STABILITY.md](docs/STABILITY.md)). The
  SQLite schema is internal — never rely on it from outside the binary.

## Capture-and-move-on

When you encounter a material bug, regression, misleading interface, recurring
workaround, or tooling failure **outside the current task**, record it with
`snag` while the evidence is fresh, then continue the assigned task.

Do not broaden the current task to fix the snag unless explicitly asked.
Do not report ordinary implementation errors or your own transient mistakes.

Every capture MUST declare exactly one fix owner on the report command —
either `--owner <repo>` (id, alias, path, or `current`) when the owner is
known, or `--unowned` when the observation is genuinely ambiguous /
environmental. Reporter location is NOT ownership; guessing `current` to mean
"my checkout" recreates the misrouting the explicit flag exists to prevent.
Empty owner and `unowned: false` do not satisfy the requirement; one of the
two flags is always required. JSON intake (`--json`) uses schema v2 with
exactly one of `"owner": "..."` or `"unowned": true`.

```sh
snag report "<specific symptom>" \
  --owner <owner/repo> \
  --kind <bug|tooling|papercut|friction|usability|probe|feature> \
  --severity <blocker|major|medium|minor|low> \
  --observed "<what happened>" \
  --expected "<what should have happened>" \
  --repro "<minimal reproduction, when known>"
```

For genuinely environmental observations:

```sh
snag report "<specific symptom>" \
  --unowned \
  --kind <...> --severity <...> \
  --observed "<...>" --expected "<...>" --repro "<...>"
```

Run `snag doctor` to see where the store lives; `snag verify --full` checks
store health before you finish.
