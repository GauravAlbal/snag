# Contributing to Snag

Thanks for contributing. This project is small and intends to stay that way —
the bar for changes is: does it make durable local observation capture better,
without turning Snag into an "agent observability platform"?

## Scope discipline

- **Capture-and-move-on.** When you hit a material out-of-scope bug, friction,
  or tooling failure while working here, record it with `snag` and continue.
  Do not broaden your current task to fix it unless asked.
- **Stay within the named ask.** No bundled refactors, no drive-by edits. If
  removing a side-change you made leaves the named fix working, the
  side-change was unsolicited — revert it.
- **No hollow implementations.** No `todo!()`, `unimplemented!()`, bare
  `FIXME`/`HACK`, or placeholder branches in delivered work.

## Build and test

Rust stable, edition 2024. SQLite is bundled — no system dependencies.

```sh
cargo build --release
```

The three gates are the contract (CI runs exactly these):

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features --no-fail-fast
```

## Code style

- `cargo fmt` formatting, clippy clean with `-D warnings`.
- No `unsafe` unless unavoidable and justified in the PR.
- Small, single-purpose changes. Prefer revising existing files over new ones.

## Tests

- **Tests describe the contract, not current behavior.** Write the test from
  the expected contract; it must fail on the old behavior. Never paste current
  output as an expectation.
- **Integration scale.** The evidence-bearing tests in `tests/` drive the real
  binary against an isolated store (temp `XDG_DATA_HOME`/`HOME`). Unit tests
  are documentation; integration tests are the promotion gate.
- Keep tests deterministic, isolated, and parallel-safe. `test_read_purity`
  proves the read commands never mutate the store — if you touch a read path,
  it must stay green.
- Stability: observation JSON, context JSON, and the export stream are
  versioned public contracts ([docs/STABILITY.md](docs/STABILITY.md)). Changes
  to those surfaces require versioning decisions, not silent edits.

## Reporting bugs

Use the issue templates (they enforce the required fields):

- `snag --version`, OS/architecture, and install method (installer / cargo /
  release binary / built from source);
- `snag doctor` output with sensitive fields redacted;
- reproduction steps;
- whether `snag verify --full` passes.

**Never upload your database or full store to an issue.** Export the relevant
records (`snag export --after-sequence N`) and redact them instead.

## Security vulnerabilities

Report privately per [SECURITY.md](SECURITY.md). Never file a public issue for
a vulnerability.

## Dependency and advisory policy

CI runs `cargo audit` and `cargo deny` (licenses, bans, advisories). A new
dependency must be MIT/Apache-2.0-compatible per `deny.toml`. An advisory
exception requires a documented justification in the PR referencing the
advisory ID — exceptions are for "not exploitable in Snag's local, single-user
threat model", never for silence.

## Commit conventions

- Imperative subject, scoped prefix: `fix:`, `feat:`, `docs:`, `ci:`,
  `chore:`, `test:`, `refactor:`.
- One logical change per commit. No whitespace-only commits.
- Reference the issue/observation when one exists.

## License

By contributing you agree that your contributions are licensed under the MIT
License — see [LICENSE](LICENSE).
