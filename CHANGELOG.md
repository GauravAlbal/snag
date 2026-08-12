# Changelog

All notable changes to snag are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning follows
[SemVer](https://semver.org/). See [docs/RELEASING.md](docs/RELEASING.md) for
the release process.

> **Version state on `main`:** development for v0.4.0; `snag --version` reports
> `0.4.0-dev` on unreleased builds. The current release is `v0.3.0`
> (Apache-2.0); the prior MIT release `v0.1.0` is retired (release and tag
> removed). All versions from v0.1.1 onward are Apache-2.0.

## [Unreleased] — 0.4.0-dev

## [0.3.0] — 2026-08-12 (release publication hardening)

### Changed
- Replace tag-triggered GitHub Actions release publication with the explicit, checked, in-house release command.

- **Capture ownership is mandatory on every report.** Every `snag report` must
  declare exactly one fix owner: `--owner <id|alias|path|current>` when the
  lane is known, or `--unowned` when the observation is genuinely ambiguous
  or purely environmental. Reporter location (cwd checkout) is no longer
  treated as ownership — `--owner current` means the repository bound to the
  cwd, not "wherever I happen to be filing from," and guessing `current`
  recreates the misrouting the explicit flag exists to prevent. Empty
  `--owner ""` and JSON/prose `unowned: false` are rejected with a typed error that names
  the escape hatch. CLI flags override JSON/prose declarations as one
  complete choice. JSON intake accepts a new schema v2 with exactly one of
  `"owner": "..."` or `"unowned": true`; v1 is still accepted only when the
  CLI supplies `--owner` or `--unowned`. The v2 schema is published as
  `schemas/observation-input-v2.schema.json`. Persisted explicit-unowned
  observations keep `owner_repository_id = None` and can be reassigned later
  via the append-only `snag review assign-owner` event without rewriting the
  original capture. Documentation (`README.md`, `AGENTS.md`, `docs/SCHEMAS.md`,
  `docs/STABILITY.md`, `docs/RUNBOOK.md`, `docs/DEMO.md`,
  `examples/agents/*.md`) and the `INSTRUCTION_BLOCK` installed by `snag init`
  now require the explicit ownership choice.
- Ambiguous `--owner` aliases now fail with `REPOSITORY_AMBIGUOUS` instead of
  being materialized as duplicate literal repository identities.
- Make `review summary` dispatch signals self-explaining: text now separates
  ready and in-flight work and shows the full actionable severity mix,
  including low; JSON adds `actionable`, `actionable_severity_counts`, and
  `in_flight` while retaining the existing open counts. `--limit` now applies
  to JSON owner lanes without narrowing threshold evaluation, and equal-score
  lanes sort deterministically by canonical repository ID.

- Prevent a conflicting explicit repository ID from acquiring the current
  checkout's worktree and remote-alias identity while preserving explicit
  reporter attribution.
- Make `review summary` lanes owner-only: filing reporters no longer create
  lanes, ownerless observations aggregate under `(unowned)`, and the text
  column is labeled `OWNER`.
- Prefer checkout-backed aliases for opaque owner IDs and render readable
  explicit IDs verbatim, preventing stale foreign aliases from making one
  repository appear as another.
- Keep `review list` and `review summary` read-pure under `--repo current`:
  repository filters now resolve from existing checkout and confirmed-alias
  state without creating repository, alias, checkout, or worktree rows.
- **Report flag documentation**: every `report`/fast-path flag now carries
  help text, including the `--json` dual role (a TITLE that names an existing
  file, or `-`, is JSON intake; otherwise `--json` selects JSON output) and
  `--stdin` intake ownership.
- **Intake vocabulary enforcement**: `--kind` and `--severity` are validated
  at report intake against the canonical sets
  (`bug|tooling|papercut|friction|usability|probe|feature` and
  `blocker|major|medium|minor|low`); unknown values are rejected with the
  allowed set named. The `list`/`review` filters stay permissive so legacy
  drift values remain queryable.
- **JSON intake error remedy**: a failed JSON-file read now names the escape
  hatch (`--stdin` for stdin intake; a non-file title for JSON output) instead
  of a bare read error.
### Added
- Add append-only `review assign-owner` for moving ownerless observations into
  fix-owner lanes without rewriting report provenance. Repository resolution,
  event append, and projection update commit atomically; exact canonical IDs
  beat aliases; list/show expose the singular owner; the export schema and
  rebuild projection recognize the event; and `verify --full` detects missing,
  stale, or conflicting owner projections.
- **`review list` lane filters + pagination**: `snag review list` now accepts
  the full `next` filter surface — `--repo` (id, alias, or `current`),
  `--kind`, `--severity`, `--unreviewed` — plus `--include-deferred` (deferred
  marks handled=true in the reducer, so `--unhandled` alone hides punted
  work an owner lane still owns) and explicit `--limit N` / `--offset N`
  paging with an unbounded default, so `--repo my-lane --unhandled` is a
  one-flag "my lane's open observations" query and text-parse consumers keep
  the full dump. `--repo current` resolves from the cwd git context
  (recording checkout bindings, same as `next`).
- **Build provenance**: `snag --version` and the `snag doctor` header now
  report the source revision (`rev <sha>`), build date, and a `-dirty` marker
  for builds from an uncommitted tree; internal-lane builds add a flavor
  suffix. `snag doctor` compares the embedded revision against the repo HEAD
  when run from a matching checkout and warns when the installed binary is
  stale — so a fix sitting in the tree while the installed binary still runs
  older code is one command away from diagnosable.


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

## [0.2.0] - 2026-08-05

### Added
- **Release workflow**: four-platform binaries, SHA-256 checksums, source-SHA
  provenance, version output, best-effort CycloneDX SBOM.
- **Issue templates** for bugs, installation, agent integration, context
  adapters, export/schema compatibility, and features.
- **repro_key localization label**: every report carries a deterministic
  `repro_key` (`blake3(store | semantic_digest)[:24]`), stored as a
  `labels.repro_key` and printed at filing so the reporter can echo it into
  the session — the line a session-search tool indexes verbatim. The digest
  strips the key so idempotent replays stay stable.
- **Prefix observation ids**: `snag show` and `snag retract` resolve a unique
  prefix of an observation id (GitHub-style); ambiguity and misses are typed
  errors.
- **Remediation protocol**: the normalized review surface lands in six
  incremental units — the event schema v5 substrate (claims, dispositions,
  relationships, remediation links + the materialized review-state
  projection) with the v4→v5 migration; the review queue with transactional
  claim leases; dispositions (verified-fixed, expected-behavior, negative)
  and relationships (same-finding, duplicate-of, upstream-cause); lineage
  (promotion, task/commit attachment, mark-handled, reopen); inspection
  (`snag review show/history` with unique-prefix id resolution) and
  remediation verification checks; persisted per-store review sessions and
  `snag review verify-report` completion validation. Adds the `serde_yaml`
  dependency.

### Changed
- **Severity microcopy**: `--severity` help frames the assertion as a prior
  (reviewers re-rank on disposition; reserve blocker/major for
  fleet-blocking classes), and a high-severity report with a thin body
  prints an inflation nudge at filing time.
- **Context-file source merge**: a partial `source` object overlays fields
  over the environment base instead of replacing the whole struct
  (`SNAG_SOURCE_KIND` survives).

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

[Unreleased]: https://github.com/GauravAlbal/snag/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/GauravAlbal/snag/compare/v0.2.0...v0.3.0
