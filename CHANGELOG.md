# Changelog

All notable changes to snag are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning follows
[SemVer](https://semver.org/). See [docs/RELEASING.md](docs/RELEASING.md) for
the release process.

## [Unreleased]

### Added
- Project identity: Cargo.toml metadata (description, license, repository),
  `LICENSE` (MIT), root `README.md`.
- Operational documentation: `docs/RUNBOOK.md`, `docs/RELEASING.md`,
  `CHANGELOG.md` (this file).
- Release automation: tag-triggered `.github/workflows/release.yml` building
  release artifacts; CI now runs a release-profile build smoke.
- Rewrote `AUDIT.md` to post-certification state (was the pre-certification
  gap list).

## [0.1.0] - 2026-08-04

Certified v0.1.0 — accepted through the moat acceptance loop and merged via
PR #1 (merge `b8dda31`). 59/59 tests, moat ACCEPTED, tag `v0.1.0`.

### Added
- **Canonical record kernel**: hash-chained, globally-sequenced records
  (`previous_record_hash` binding); deterministic canonical encoding; tamper
  tests for every bound field.
- **Capture**: `report` (fast path + structured), `list` (filters:
  `--repo --since --source --kind --limit --format`), `show`, `context`,
  `retract`.
- **Context**: git repo/checkout/worktree identity (real git common dir,
  linked worktrees), `SNAG_CONTEXT_FILE` overlay with explicit
  CLI > context-file > env > git precedence, `VX_*`/`ARQ_*`/`SNAG_*` env
  variables, affected-repo resolution with typed failures.
- **Idempotency**: stable semantic digest; same key + same digest replays,
  same key + different digest conflicts.
- **Recovery chain**: `backup` (self-contained bundle + manifest), `restore`
  (non-destructive, forensic copy preserved), `rebuild --from-export`,
  `verify --quick` / `--full` / `--backup`.
- **Export protocol**: deterministic JSONL export (byte-identical for
  identical state), partial exports with exact predecessor hashes.
- **Migrations**: v1→v2 deterministic and collision-safe, transactional,
  forensic copy on failure.
- **Read purity**: distinct reader/writer/maintenance connection modes;
  list/show/context/export/verify/doctor proven non-mutating.
- **Robustness**: crash injection (T6), 32-writer concurrency (T5), git
  process-kill with bounded budget, artifact constraints (symlink rejection,
  per-file and total limits), 30s `busy_timeout`.
- **Testing**: 59 tests across cli, git_identity, migration, concurrency,
  robustness suites; GitHub Actions CI.

[Unreleased]: https://github.com/13banditos/snag/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/13banditos/snag/releases/tag/v0.1.0
