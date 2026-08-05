# Changelog

All notable changes to snag are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning follows
[SemVer](https://semver.org/). See [docs/RELEASING.md](docs/RELEASING.md) for
the release process.

> **Version state on `main`:** development for v0.2.0; `snag --version` reports
> `0.2.0-dev` on unreleased builds. The current release is `v0.1.1`
> (Apache-2.0); the prior MIT release `v0.1.0` is retired (release and tag
> removed). All versions from v0.1.1 onward are Apache-2.0.

## [0.1.1] - 2026-08-05

### Changed
- **Apache-2.0 re-release**: v0.1.1 is licensed Apache-2.0 (explicit
  contributor patent grant, notice preservation, trademark exclusion) and
  supersedes the MIT `v0.1.0` release, which is retired (release + tag
  removed). The LICENSE and `Cargo.toml` carry Apache-2.0 from this version
  onward; all later versions stay Apache-2.0.
- **Public release hardening**: standalone README, public AGENTS.md,
  trust-and-safety docs (SECURITY, CONTRIBUTING, CODE_OF_CONDUCT, SUPPORT),
  stability guarantees, narrow roadmap, anonymized dogfood case study.
- **Context protocol as a public API**: `SNAG_CONTEXT_FILE` documents are now
  versioned (`schema_version: 1` enforced, future versions rejected with a
  typed error); `snag context --format json` emits a versioned envelope;
  published JSON Schemas in `schemas/` with a compatibility test suite
  (`tests/schema_compat.rs`) that validates the binary's real output.
- **Generic agent integrations** under `examples/agents/` (Claude Code, Codex,
  Gemini CLI, OpenCode, generic shell agents) with no private tooling.
- **Downstream consumer example** under `examples/export-consumer/`
  (stdlib-only Python: header validation, sequence contiguity, checkpointing).
- **One-line installer** (`install.sh`): downloads the platform release binary
  from GitHub Releases and verifies its SHA-256 checksum.
- **CI matrix** over macOS arm64 and Linux x86_64/aarch64, plus
  `cargo deny` and `cargo audit` dependency gates.
- **Observation pipeline positioning**: `docs/PIPELINE.md` and a README
  pull-quote place Snag as the stage before the task tracker.

### Added
- **Trademark policy**: `TRADEMARKS.md` governs use of the Snag name and logo.
- **DCO**: contributions require signed-off commits (`git commit -s`) under
  the Developer Certificate of Origin 1.1; no CLA.
- **Release hygiene**: distinct install contracts in the README (released
  `--tag v0.1.1` vs current main).

## [Unreleased]
- **Release workflow**: four-platform binaries, SHA-256 checksums, source-SHA
  provenance, version output, best-effort CycloneDX SBOM.
- **Issue templates** for bugs, installation, agent integration, context
  adapters, export/schema compatibility, and features.

### Fixed
- `snag report "<title>" --json` now treats a bare title as the observation
  title with JSON output (previously it misread the title as a JSON intake
  file path and failed). File intake (`--json <file>`) and stdin intake are
  unchanged.
- Structured flags now work on the bare fast path:
  `snag "<title>" --kind bug --severity minor`.
- `snag doctor` now prints the exact store paths (database, objects, backups),
  the effective context source, and the Snag version, even when no store
  exists yet.

## [0.1.0] - 2026-08-04

Certified v0.1.0 — 59-test surface, three hard gates, tag `v0.1.0`.

### Added
- **Canonical record kernel**: hash-chained, globally-sequenced records
  (`previous_record_hash` binding); deterministic canonical encoding; tamper
  tests for every bound field.
- **Capture**: `report` (fast path + structured), `list` (filters:
  `--repo --since --source --kind --limit --format`), `show`, `context`,
  `retract`.
- **Context**: git repo/checkout/worktree identity (real git common dir,
  linked worktrees), `SNAG_CONTEXT_FILE` overlay with explicit
  CLI > context-file > env > git precedence, affected-repo resolution with
  typed failures.
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
- **Robustness**: crash injection, 32-writer concurrency, git process-kill
  with bounded budget, artifact constraints (symlink rejection, per-file and
  total limits), 30s `busy_timeout`.
- **Testing**: 59 tests across cli, git_identity, migration, concurrency,
  robustness suites; GitHub Actions CI.

[Unreleased]: https://github.com/GauravAlbal/snag/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/GauravAlbal/snag/releases/tag/v0.1.1
