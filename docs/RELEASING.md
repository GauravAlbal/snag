# Releasing

How snag is versioned and released.

## Versioning

- **SemVer**: `MAJOR.MINOR.PATCH`. The single source of truth is the `version`
  field in `Cargo.toml`.
- `MAJOR` — breaking change to the store, export protocol, or CLI contract.
- `MINOR` — new backward-compatible feature.
- `PATCH` — backward-compatible bug fix.
- Pre-release suffixes (`-rc.1`) are allowed for release candidates.

For the v0.x stability guarantees (which surfaces are versioned and which are
internal), see [STABILITY.md](STABILITY.md).

## The release process

### 1. Prepare

```sh
git checkout main && git pull
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features --no-fail-fast
```

### 2. Bump version

Edit `Cargo.toml` `version` per SemVer. Update `CHANGELOG.md`:

- Move `[Unreleased]` entries into a new `## [X.Y.Z] - <date>` section.
- Add a fresh `[Unreleased]` section on top.
- Update the version compare links at the bottom.

### 3. Commit, tag, push

```sh
git add -A && git commit -m "chore: release vX.Y.Z"
git tag -a vX.Y.Z -m "snag vX.Y.Z"
git push origin main --tags
```

### 4. Build and publish from the tagged commit

Run this explicitly from the repository checkout after pushing the tag:

```sh
scripts/release.sh --lane public vX.Y.Z
```

The command is the release authority; GitHub Actions is not involved. It
requires a clean checkout whose `HEAD` exactly matches the annotated tag's
resolved commit, a running Docker daemon, `cross`, and the Rust macOS arm64
target. Both Linux targets are built with `cross`; macOS arm64 is built with
Cargo. The command verifies all three binaries, records the package version in
`version.txt`, writes `source-sha.txt`, generates `SHA256SUMS.txt` (including
the SBOM when generated), and uploads the artifacts to a draft GitHub Release.
It verifies the complete asset set before publishing and removes an incomplete
draft on failure without overwriting an existing release. Use `--dry-run` to
build and verify artifacts without GitHub writes.

### 5. Verify

Confirm the GitHub Release page has:

- `snag-<target-triple>` binaries for all three targets;
- `SHA256SUMS.txt` (verify with `shasum -a 256 -c SHA256SUMS.txt`);
- `source-sha.txt` (the exact commit the binaries were built from);
- `version.txt` and the SBOM, if generated.

### 6. Announce

Note the release in the project's discussion channel; the CHANGELOG entry is
the source of truth for what changed.

## Version compatibility policy

- **Store schema**: a store written by X.Y may be read by X.Y+1 via the
  deterministic migration path. Backward reads of older stores are supported
  through migrations; forward reads (newer store, older binary) are not.
- **Export protocol**: `export_schema_version` / `minimum_reader_version`
  headers govern compatibility; rebuild refuses unsupported schema versions
  rather than guessing. The export stream is the public API for downstream
  systems.
- **Context protocol**: `SNAG_CONTEXT_FILE` documents are versioned by
  `schema_version`; a future major version is rejected with a typed error
  rather than misparsed.
- **CLI**: flags are additive within a MINOR; removing/renaming a flag is a
  MAJOR. In v0.x, CLI ergonomics may change between minors (best-effort
  stability).
