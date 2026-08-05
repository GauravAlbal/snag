# Snag

Snag is a durable observation outbox for coding agents: when you encounter an
out-of-scope bug, regression, misleading interface, recurring workaround, or
tooling failure, record it with `snag` while the evidence is fresh, then
continue the assigned task. See [README.md](README.md) and
[AGENTS.md](AGENTS.md).

## Build and test

Requires Rust stable (edition 2024). SQLite is bundled — no system dependency.

```sh
cargo build --release        # binary: target/release/snag
```

The three gates below are the contract. CI runs exactly these; they must pass
before any change is merged.

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features --no-fail-fast
```

## Contribution rules

- Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a PR.
- Keep changes scoped to the named ask. No bundled refactors, no drive-by
  edits. If a change touches behavior, it ships with a test that fails on the
  old behavior.
- Security vulnerabilities: report privately per [SECURITY.md](SECURITY.md),
  never in a public issue.
- Reporting bugs: use the issue templates (include `snag --version`,
  OS/architecture, install method, and redacted `snag doctor` output).

## Snag reporting (capture-and-move-on)

When you encounter a material bug, regression, misleading interface, recurring
workaround, or tooling failure outside the current task, record it with `snag`
while the evidence is fresh, then continue the assigned task.

Do not broaden the current task to fix the snag unless explicitly asked.
Do not report ordinary implementation errors or your own transient mistakes.

```sh
snag report "<specific symptom>" \
  --kind <bug|papercut|friction|usability> \
  --severity <minor|major|blocker> \
  --observed "<what happened>" \
  --expected "<what should have happened>" \
  --repro "<minimal reproduction, when known>" \
  --workaround "<workaround used, when any>"
```

Before completing a task, review whether any qualifying snag occurred and
report the observation ids. Run `snag doctor` to see where data lives and
`snag verify --full` to confirm store health.
