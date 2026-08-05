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

Pushing the `vX.Y.Z` tag triggers `.github/workflows/release.yml`, which:

1. Builds release binaries for macOS (arm64) and Linux (x86_64,
   aarch64).
2. Captures `snag --version` output for each build.
3. Generates `SHA256SUMS.txt` over all artifacts, a `source-sha.txt` with the
   triggering commit SHA, and (best-effort) a CycloneDX SBOM.
4. Creates the GitHub Release for the tag with every artifact attached.

### 4. Verify

Check the Actions run on the tag and confirm the Release page has:

- `snag-<target-triple>` binaries for all four platforms;
- `SHA256SUMS.txt` (verify with `shasum -a 256 -c SHA256SUMS.txt`);
- `source-sha.txt` (the exact commit the binaries were built from);
- `version.txt` and the SBOM, if generated.

### 5. Announce

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
