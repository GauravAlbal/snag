# Releasing

How snag is versioned and released.

## Versioning

- **SemVer**: `MAJOR.MINOR.PATCH`. The single source of truth is the
  `version` field in `Cargo.toml`.
- `MAJOR` — breaking change to the store, export protocol, or CLI contract.
- `MINOR` — new backward-compatible feature.
- `PATCH` — backward-compatible bug fix.
- Pre-release suffixes (`-rc.1`) are allowed for release candidates.

## The release process

Every change ships through the moat acceptance loop first (see
[CLAUDE.md](../CLAUDE.md)). A release is a **tag on an accepted revision** —
never tag an unaccepted one.

### 1. Prepare

```sh
# On master, with all accepted work merged:
git checkout master && git pull
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features --no-fail-fast
```

### 2. Bump version

Edit `Cargo.toml` `version` per SemVer. Update `CHANGELOG.md`:

- Move `[Unreleased]` entries into a new `## [X.Y.Z] - <date>` section.
- Add a fresh `[Unreleased]` section on top.
- Update the version compare links at the bottom.

### 3. Ship the bump through moat

Contract the version-bump + changelog change (touch `Cargo.toml`,
`CHANGELOG.md`), submit, and land only on ACCEPTED — the same as any other
change.

### 4. Tag and release

```sh
git tag -a vX.Y.Z -m "snag vX.Y.Z"
git push origin master --tags
```

Pushing the `vX.Y.Z` tag triggers `.github/workflows/release.yml`, which:

1. Builds release binaries on the supported platforms (Linux x86_64, macOS
   arm64).
2. Uploads them as artifacts on the GitHub Release for the tag.
3. The Release body should summarize the changelog highlights.

Verify the run on the Actions tab and check the Release page has binaries.

### 5. Announce

Note the release in the operator channel; the CHANGELOG entry is the source
of truth for what changed.

## Version compatibility policy

- **Store schema**: a store written by X.Y may be read by X.Y+1 via the
  deterministic migration path. Backward reads of older stores are supported
  through migrations; forward reads (newer store, older binary) are not.
- **Export protocol**: `export_schema_version` / `minimum_reader_version`
  headers govern compatibility; rebuild refuses unsupported schema versions
  rather than guessing.
- **CLI**: flags are additive within a MINOR; removing/renaming a flag is a
  MAJOR.
