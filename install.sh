#!/usr/bin/env bash
#
# snag installer
#
# One-liner:
#   curl -fsSL https://raw.githubusercontent.com/GauravAlbal/snag/master/install.sh | bash
#
# Options:
#   --version vX.Y.Z   Install a specific version (default: latest release)
#   --dest DIR         Install to DIR (default: ~/.local/bin)
#   --system           Install to /usr/local/bin (requires sudo)
#   --verify           Run a self-test after install (snag --version + snag doctor)
#   --from-source      Build from source with cargo instead of downloading a binary
#   --quiet            Suppress non-error output
#   --no-checksum      Skip SHA-256 verification (testing only)
#   --artifact-url URL Override the binary download URL (testing)
#   --checksum-url URL Override the SHA256SUMS.txt URL (testing)
#
# The binary is downloaded from the GitHub release for the platform triple,
# its SHA-256 checksum is verified against SHA256SUMS.txt from the same
# release, then it is installed and made executable.
#
set -euo pipefail
umask 022

OWNER="${SNAG_OWNER:-GauravAlbal}"
REPO="${SNAG_REPO:-snag}"
BRANCH="${SNAG_BRANCH:-master}"
VERSION="${VERSION:-}"
DEST="${DEST:-$HOME/.local/bin}"
SYSTEM=0
VERIFY=0
FROM_SOURCE=0
QUIET=0
NO_CHECKSUM=0
ARTIFACT_URL="${ARTIFACT_URL:-}"
CHECKSUM_URL="${CHECKSUM_URL:-}"

log() { [ "$QUIET" -eq 1 ] && return 0; echo -e "$@"; }
err() { echo -e "\033[0;31m✗ $*\033[0m" >&2; }

# Parse arguments.
while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --dest) DEST="$2"; shift 2 ;;
    --system) SYSTEM=1; shift ;;
    --verify) VERIFY=1; shift ;;
    --from-source) FROM_SOURCE=1; shift ;;
    --quiet) QUIET=1; shift ;;
    --no-checksum) NO_CHECKSUM=1; shift ;;
    --artifact-url) ARTIFACT_URL="$2"; shift 2 ;;
    --checksum-url) CHECKSUM_URL="$2"; shift 2 ;;
    -h|--help)
      grep '^#' "$0" | sed 's/^# \{0,1\}//' | grep -v '^$' || true
      exit 0 ;;
    *) err "unknown option: $1"; exit 2 ;;
  esac
done

# Detect platform triple.
OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS:$ARCH" in
  Darwin:arm64) TRIPLE="aarch64-apple-darwin" ;;
  Darwin:x86_64)
    err "macOS x86_64 binaries are not published (Apple Silicon or --from-source required)"
    exit 2 ;;
  Linux:x86_64) TRIPLE="x86_64-unknown-linux-gnu" ;;
  Linux:aarch64|Linux:arm64) TRIPLE="aarch64-unknown-linux-gnu" ;;
  *)
    err "unsupported platform: $OS $ARCH (supported: macOS arm64, Linux x86_64/aarch64)"
    exit 2 ;;
esac

ASSET="snag-$TRIPLE"

if [ "$SYSTEM" -eq 1 ]; then
  DEST="/usr/local/bin"
fi

if [ "$FROM_SOURCE" -eq 1 ]; then
  log "→ Building snag from source (cargo)..."
  if ! command -v cargo >/dev/null 2>&1; then
    err "cargo not found; install Rust first (https://rustup.rs)"
    exit 2
  fi
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  git clone --depth 1 "https://github.com/$OWNER/$REPO.git" "$TMP/src"
  (cd "$TMP/src" && cargo build --release)
  BIN="$TMP/src/target/release/snag"
else
  RELEASE_URL="https://github.com/$OWNER/$REPO/releases"
  if [ -n "$VERSION" ]; then
    BASE="$RELEASE_URL/download/$VERSION"
  else
    BASE="$RELEASE_URL/latest/download"
  fi
  [ -n "$ARTIFACT_URL" ] && BASE_ARTIFACT="$ARTIFACT_URL" || BASE_ARTIFACT="$BASE"
  [ -n "$CHECKSUM_URL" ] && CHECKSUMS="$CHECKSUM_URL" || CHECKSUMS="$BASE/SHA256SUMS.txt"

  log "→ Downloading $ASSET..."
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  curl -fsSL "$BASE_ARTIFACT/$ASSET" -o "$TMP/snag"

  if [ "$NO_CHECKSUM" -eq 1 ]; then
    log "⚠  Skipping checksum verification (--no-checksum)"
  else
    log "→ Verifying SHA-256 checksum..."
    curl -fsSL "$CHECKSUMS" -o "$TMP/SHA256SUMS.txt"
    EXPECTED="$(awk -v a="$ASSET" '$2 == a { print $1 }' "$TMP/SHA256SUMS.txt")"
    if [ -z "$EXPECTED" ]; then
      err "checksum file has no entry for $ASSET; refusing to install"
      exit 2
    fi
    ACTUAL="$(shasum -a 256 "$TMP/snag" | awk '{ print $1 }')"
    if [ "$ACTUAL" != "$EXPECTED" ]; then
      err "checksum mismatch for $ASSET (expected $EXPECTED, got $ACTUAL); refusing to install"
      exit 2
    fi
    log "✓ Checksum verified"
  fi

  BIN="$TMP/snag"
fi

# Install.
mkdir -p "$DEST"
install -m 0755 "$BIN" "$DEST/snag"
log "✓ Installed $DEST/snag"

# PATH hint.
case ":$PATH:" in
  *":$DEST:"*) ;;
  *)
    if [ "$SYSTEM" -eq 1 ]; then :; else
      log "ℹ  $DEST is not on your PATH. Add it, e.g.:"
      log "   echo 'export PATH=\"$DEST:\$PATH\"' >> ~/.bashrc"
    fi
    ;;
esac

# Self-test.
if [ "$VERIFY" -eq 1 ]; then
  log "→ Running self-test..."
  "$DEST/snag" --version
  "$DEST/snag" doctor
  log "✓ Self-test passed"
fi

log "✓ Done. Run 'snag report \"title\"' to capture your first observation."
