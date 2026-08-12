#!/usr/bin/env bash
# Build and publish one immutable release from an annotated tag.
# The command is intentionally explicit: GitHub Actions is not part of the
# release build or publication path.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/release.sh --lane public [--dry-run] TAG

Build and, unless --dry-run is supplied, publish TAG from this checkout.
TAG must be an existing annotated tag whose resolved commit is exactly HEAD.
The release is published to the repository's origin GitHub repository.

Prerequisites: cargo, cross, docker (running), gh, python3, shasum, rustup.
Both Linux targets are built with cross; macOS arm64 is built with cargo.
Existing GitHub Releases are never overwritten.
EOF
}

DRY_RUN=0
LANE=""
while (($#)); do
  case "$1" in
    --lane) LANE="${2:?release: --lane requires public}"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    --) shift; break ;;
    -*) echo "release: unknown option: $1" >&2; usage >&2; exit 2 ;;
    *) break ;;
  esac
done

case "$LANE" in
  public) ;;
  *) echo "release: --lane must be public" >&2; usage >&2; exit 2 ;;
esac

[[ $# -eq 1 ]] || { usage >&2; exit 2; }
TAG="$1"
case "$TAG" in
  v[0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "release: tag must look like vX.Y.Z: $TAG" >&2; exit 2 ;;
esac

command -v cargo >/dev/null || { echo "release: cargo is required" >&2; exit 2; }
command -v cross >/dev/null || { echo "release: cross is required for both Linux targets" >&2; exit 2; }
command -v docker >/dev/null || { echo "release: docker is required for both Linux targets" >&2; exit 2; }
if [[ "$DRY_RUN" -eq 0 ]]; then
  command -v gh >/dev/null || { echo "release: gh is required for publication" >&2; exit 2; }
fi
command -v python3 >/dev/null || { echo "release: python3 is required" >&2; exit 2; }
command -v shasum >/dev/null || { echo "release: shasum is required" >&2; exit 2; }
command -v rustup >/dev/null || { echo "release: rustup is required for the macOS target check" >&2; exit 2; }

git diff --quiet && git diff --cached --quiet || {
  echo "release: worktree has unstaged or staged changes" >&2
  exit 2
}
[[ -z "$(git status --porcelain=v1 --untracked-files=all)" ]] || {
  echo "release: worktree has untracked files" >&2
  exit 2
}

[[ "$(git cat-file -t "$TAG" 2>/dev/null || true)" == tag ]] || {
  echo "release: $TAG must be an annotated tag" >&2
  exit 2
}
TAG_COMMIT="$(git rev-parse "$TAG^{}")"
HEAD_COMMIT="$(git rev-parse HEAD)"
[[ "$TAG_COMMIT" == "$HEAD_COMMIT" ]] || {
  echo "release: HEAD ($HEAD_COMMIT) is not the resolved commit for $TAG ($TAG_COMMIT)" >&2
  exit 2
}

REMOTE_URL="$(git remote get-url origin)"
REPO="${REMOTE_URL#https://github.com/}"
REPO="${REPO#http://github.com/}"
REPO="${REPO#git@github.com:}"
REPO="${REPO%.git}"
[[ "$REPO" == GauravAlbal/snag ]] || {
  echo "release: origin must be GauravAlbal/snag" >&2
  exit 2
}

PACKAGE_VERSION="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json, sys; d=json.load(sys.stdin); print(next(p["version"] for p in d["packages"] if p["name"] == "snag"))')"
[[ "v$PACKAGE_VERSION" == "$TAG" ]] || {
  echo "release: Cargo package version $PACKAGE_VERSION does not match $TAG" >&2
  exit 2
}

if ! docker info >/dev/null 2>&1; then
  echo "release: Docker daemon is unavailable; both Linux targets require cross" >&2
  exit 2
fi
if ! rustup target list --installed | python3 -c 'import sys; sys.exit(0 if any(line.split()[0:1] == ["aarch64-apple-darwin"] for line in sys.stdin) else 1)'; then
  echo "release: rustup target aarch64-apple-darwin is not installed" >&2
  exit 2
fi
if [[ "$DRY_RUN" -eq 0 ]]; then
  REMOTE_TAG_COMMIT="$(git ls-remote "$REMOTE_URL" "refs/tags/$TAG^{}" | awk 'NR == 1 { print $1 }')"
  [[ "$REMOTE_TAG_COMMIT" == "$TAG_COMMIT" ]] || {
    echo "release: remote tag $TAG does not resolve to HEAD" >&2
    exit 2
  }
  if gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
    echo "release: GitHub Release $REPO/$TAG already exists; refusing overwrite" >&2
    exit 2
  fi
fi

BUILD_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/snag-release.XXXXXX")"
DRAFT_CREATED=0
RELEASE_MARKER="snag-release-marker-$TAG-$HEAD_COMMIT-$(python3 -c 'import secrets; print(secrets.token_hex(16))')"
cleanup_release() {
  if [[ "$DRAFT_CREATED" -eq 1 ]]; then
    draft_status="$(gh release view "$TAG" --repo "$REPO" --json isDraft --jq '.isDraft' 2>/dev/null || true)"
    draft_body="$(gh release view "$TAG" --repo "$REPO" --json body --jq '.body' 2>/dev/null || true)"
    if [[ "$draft_status" == "true" && "$draft_body" == *"$RELEASE_MARKER"* ]]; then
      echo "release: removing incomplete draft $REPO/$TAG" >&2
      gh release delete "$TAG" --repo "$REPO" --yes >/dev/null 2>&1 || true
    fi
  fi
  if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "release: dry-run artifacts retained at $BUILD_ROOT"
  else
    rm -rf "$BUILD_ROOT"
  fi
}
trap cleanup_release EXIT
TARGET_DIR="$BUILD_ROOT/target"
ARTIFACT_DIR="$BUILD_ROOT/release-binaries"
mkdir -p "$ARTIFACT_DIR"
export CARGO_TARGET_DIR="$TARGET_DIR"
for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
  echo "release: cross build $target"
  cross build --release --locked --target "$target"
done
echo "release: cargo build aarch64-apple-darwin"
cargo build --release --locked --target aarch64-apple-darwin

for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu aarch64-apple-darwin; do
  binary="$TARGET_DIR/$target/release/snag"
  [[ -x "$binary" ]] || { echo "release: missing binary $binary" >&2; exit 1; }
  cp "$binary" "$ARTIFACT_DIR/snag-$target"
done
chmod 0755 "$ARTIFACT_DIR"/snag-*
printf '%s\n' "$PACKAGE_VERSION" > "$ARTIFACT_DIR/version.txt"
printf '%s\n' "$TAG_COMMIT" > "$ARTIFACT_DIR/source-sha.txt"

if cargo cyclonedx --version >/dev/null 2>&1; then
  cargo cyclonedx --all-features --output "$BUILD_ROOT/sbom.xml" >/dev/null
  cp "$BUILD_ROOT/sbom.xml" "$ARTIFACT_DIR/sbom-cyclonedx.xml"
else
  echo "release: cargo-cyclonedx unavailable; omitting optional SBOM"
fi

(
  cd "$ARTIFACT_DIR"
  if [[ -f sbom-cyclonedx.xml ]]; then
    shasum -a 256 snag-* version.txt source-sha.txt sbom-cyclonedx.xml > SHA256SUMS.txt
  else
    shasum -a 256 snag-* version.txt source-sha.txt > SHA256SUMS.txt
  fi
  shasum -a 256 -c SHA256SUMS.txt
)

cat > "$BUILD_ROOT/release-notes.md" <<EOF
See [CHANGELOG.md](https://github.com/$REPO/blob/$TAG/CHANGELOG.md) for this release.

Built from source commit \`$TAG_COMMIT\` by the in-house release command.

Release marker: \`$RELEASE_MARKER\`.

Verify checksums with \`shasum -a 256 -c SHA256SUMS.txt\`.
EOF

if [[ "$DRY_RUN" -eq 1 ]]; then
  echo "release: dry-run passed for $REPO/$TAG"
  find "$ARTIFACT_DIR" -maxdepth 1 -type f -print | sort
  exit 0
fi

gh release create "$TAG" --repo "$REPO" --verify-tag --draft --title "snag $TAG" \
  --notes-file "$BUILD_ROOT/release-notes.md"
DRAFT_CREATED=1
gh release upload "$TAG" --repo "$REPO" "$ARTIFACT_DIR"/*
EXPECTED_ASSETS="$(printf '%s\n' "$ARTIFACT_DIR"/* | sed 's#.*/##' | sort)"
ACTUAL_ASSETS="$(gh release view "$TAG" --repo "$REPO" --json assets \
  --jq '.assets[].name' | sort)"
[[ "$ACTUAL_ASSETS" == "$EXPECTED_ASSETS" ]] || {
  echo "release: draft asset set mismatch; refusing to publish" >&2
  echo "expected:" >&2
  printf '%s\n' "$EXPECTED_ASSETS" >&2
  echo "actual:" >&2
  printf '%s\n' "$ACTUAL_ASSETS" >&2
  exit 1
}
gh release edit "$TAG" --repo "$REPO" --draft=false
DRAFT_CREATED=0
echo "release: published $REPO/$TAG"
