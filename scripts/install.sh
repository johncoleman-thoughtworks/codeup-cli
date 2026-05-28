#!/usr/bin/env sh
# codeup-cli installer.
#
# Usage:
#   curl -fsSL https://github.com/johncoleman-thoughtworks/codeup-cli/releases/latest/download/codeup-installer.sh | sh
#   curl -fsSL .../codeup-installer.sh | sh -s -- --version v0.2.0
#   curl -fsSL .../codeup-installer.sh | sh -s -- --from-source
#
# Options:
#   --version <tag>       Install a specific release tag (default: latest).
#   --prefix <dir>        Install into <dir>/bin (default: $HOME/.local).
#                         Also honours $CODEUP_INSTALL_DIR / $CODEUP_PREFIX.
#   --from-source         Skip the prebuilt binary path; clone and cargo build.
#   --yes, -y             Non-interactive: modify shell rc files without asking.
#   --no-modify-path      Never touch shell rc files; just print the export line.
#   --uninstall           Remove the installed binary and PATH line, then exit.
#   --help, -h            Show this help.
#
# Environment:
#   CODEUP_VERSION        Same as --version.
#   CODEUP_INSTALL_DIR    Install dir for the binary (e.g. $HOME/.local/bin).
#   CODEUP_PREFIX         Prefix; $CODEUP_PREFIX/bin is the install dir.
#
# Exit codes: 0 on success, non-zero on any failure.

set -eu

REPO="johncoleman-thoughtworks/codeup-cli"
PATH_MARKER="# >>> codeup-cli (added by installer) >>>"
PATH_MARKER_END="# <<< codeup-cli <<<"

VERSION="${CODEUP_VERSION:-latest}"
FROM_SOURCE=0
ASSUME_YES=0
MODIFY_PATH=auto   # auto | yes | no
DO_UNINSTALL=0

if [ -n "${CODEUP_INSTALL_DIR:-}" ]; then
  INSTALL_DIR="$CODEUP_INSTALL_DIR"
elif [ -n "${CODEUP_PREFIX:-}" ]; then
  INSTALL_DIR="$CODEUP_PREFIX/bin"
else
  INSTALL_DIR="$HOME/.local/bin"
fi

log() { printf '%s\n' "$*"; }
err() { printf 'error: %s\n' "$*" >&2; }
die() { err "$*"; exit 1; }

usage() {
  # Print the leading comment block (everything from line 2 up to the
  # first non-comment line) with the `# ` prefix stripped.
  awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "$0"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version)         shift; [ $# -gt 0 ] || die "--version needs an argument"; VERSION="$1" ;;
    --version=*)       VERSION="${1#*=}" ;;
    --prefix)          shift; [ $# -gt 0 ] || die "--prefix needs an argument"; INSTALL_DIR="$1/bin" ;;
    --prefix=*)        INSTALL_DIR="${1#*=}/bin" ;;
    --install-dir)     shift; [ $# -gt 0 ] || die "--install-dir needs an argument"; INSTALL_DIR="$1" ;;
    --install-dir=*)   INSTALL_DIR="${1#*=}" ;;
    --from-source)     FROM_SOURCE=1 ;;
    -y|--yes)          ASSUME_YES=1; MODIFY_PATH=yes ;;
    --no-modify-path)  MODIFY_PATH=no ;;
    --uninstall)       DO_UNINSTALL=1 ;;
    -h|--help)         usage; exit 0 ;;
    *)                 die "unknown option: $1 (try --help)" ;;
  esac
  shift
done

# ---- shell rc detection -----------------------------------------------------

detect_rc() {
  shell_name=$(basename "${SHELL:-}")
  case "$shell_name" in
    zsh)  printf '%s\n' "$HOME/.zshrc" ;;
    bash)
      # macOS bash loads .bash_profile for login shells (Terminal.app default);
      # Linux bash typically uses .bashrc for interactive shells.
      if [ "$(uname -s)" = "Darwin" ] && [ -f "$HOME/.bash_profile" ]; then
        printf '%s\n' "$HOME/.bash_profile"
      else
        printf '%s\n' "$HOME/.bashrc"
      fi ;;
    fish) printf '%s\n' "$HOME/.config/fish/config.fish" ;;
    *)    printf '%s\n' "$HOME/.profile" ;;
  esac
}

path_already_has() {
  case ":$PATH:" in
    *":$1:"*) return 0 ;;
    *)        return 1 ;;
  esac
}

append_path_block() {
  rc="$1"; dir="$2"
  mkdir -p "$(dirname "$rc")"
  [ -f "$rc" ] || : > "$rc"
  if grep -Fq "$PATH_MARKER" "$rc" 2>/dev/null; then
    log "PATH entry already present in $rc — skipping."
    return 0
  fi
  case "$(basename "$rc")" in
    config.fish) line="set -gx PATH \"$dir\" \$PATH" ;;
    *)           line="export PATH=\"$dir:\$PATH\"" ;;
  esac
  {
    printf '\n%s\n' "$PATH_MARKER"
    printf '%s\n'    "$line"
    printf '%s\n'    "$PATH_MARKER_END"
  } >> "$rc"
  log "Added $dir to PATH in $rc."
  log "Open a new shell, or run: . \"$rc\""
}

remove_path_block() {
  rc="$1"
  [ -f "$rc" ] || return 0
  grep -Fq "$PATH_MARKER" "$rc" 2>/dev/null || return 0
  tmp=$(mktemp)
  awk -v start="$PATH_MARKER" -v end="$PATH_MARKER_END" '
    $0 == start { skip=1; next }
    skip && $0 == end { skip=0; next }
    !skip { print }
  ' "$rc" > "$tmp"
  mv "$tmp" "$rc"
  log "Removed codeup PATH entry from $rc."
}

# ---- uninstall --------------------------------------------------------------

if [ "$DO_UNINSTALL" -eq 1 ]; then
  bin="$INSTALL_DIR/codeup"
  if [ -e "$bin" ]; then
    rm -f "$bin"
    log "Removed $bin."
  else
    log "No binary at $bin."
  fi
  remove_path_block "$(detect_rc)"
  exit 0
fi

# ---- platform detection -----------------------------------------------------

OS=$(uname -s)
ARCH=$(uname -m)
case "$OS-$ARCH" in
  Linux-x86_64)            TARGET=x86_64-unknown-linux-gnu ;;
  Linux-aarch64|Linux-arm64) TARGET=aarch64-unknown-linux-gnu ;;
  Darwin-arm64)            TARGET=aarch64-apple-darwin ;;
  Darwin-x86_64)           TARGET=x86_64-apple-darwin ;;
  *)
    err "No prebuilt for $OS-$ARCH — falling back to source build."
    FROM_SOURCE=1
    TARGET=""
    ;;
esac

have() { command -v "$1" >/dev/null 2>&1; }

downloader=""
if have curl;   then downloader=curl
elif have wget; then downloader=wget
else die "need curl or wget on PATH"; fi

fetch() {
  url="$1"; out="$2"
  case "$downloader" in
    curl) curl -fsSL "$url" -o "$out" ;;
    wget) wget -qO "$out" "$url" ;;
  esac
}

# ---- workdir ----------------------------------------------------------------

TMP=$(mktemp -d "${TMPDIR:-/tmp}/codeup-install.XXXXXX")
trap 'rm -rf "$TMP"' EXIT INT TERM

mkdir -p "$INSTALL_DIR"

# ---- install: prebuilt ------------------------------------------------------

install_prebuilt() {
  asset="codeup-$TARGET.tar.gz"
  if [ "$VERSION" = "latest" ]; then
    base="https://github.com/$REPO/releases/latest/download"
  else
    base="https://github.com/$REPO/releases/download/$VERSION"
  fi
  log "Downloading $asset from $base"
  fetch "$base/$asset"        "$TMP/$asset"        || return 1
  fetch "$base/$asset.sha256" "$TMP/$asset.sha256" || log "(no checksum sidecar; skipping verification)"

  if [ -s "$TMP/$asset.sha256" ]; then
    expected=$(awk '{print $1}' "$TMP/$asset.sha256")
    if have shasum;     then actual=$(shasum -a 256 "$TMP/$asset" | awk '{print $1}')
    elif have sha256sum; then actual=$(sha256sum "$TMP/$asset" | awk '{print $1}')
    else log "no shasum/sha256sum tool — skipping verification"; actual="$expected"
    fi
    [ "$expected" = "$actual" ] || die "checksum mismatch for $asset (expected $expected, got $actual)"
    log "Checksum OK."
  fi

  tar -xzf "$TMP/$asset" -C "$TMP" || die "failed to extract $asset"
  mv "$TMP/codeup" "$INSTALL_DIR/codeup"
  chmod +x "$INSTALL_DIR/codeup"

  # Strip the macOS quarantine bit so the binary runs without a Gatekeeper
  # prompt. Harmless if the attribute isn't present.
  if [ "$OS" = "Darwin" ] && have xattr; then
    xattr -d com.apple.quarantine "$INSTALL_DIR/codeup" 2>/dev/null || true
  fi
}

# ---- install: source --------------------------------------------------------

install_from_source() {
  have cargo || die "cargo not found — install Rust from https://rustup.rs and re-run"
  have git   || die "git not found — required for source build"

  if [ "$VERSION" = "latest" ]; then
    git clone --depth 1 "https://github.com/$REPO.git" "$TMP/src" || die "git clone failed"
  else
    git clone --depth 1 --branch "$VERSION" "https://github.com/$REPO.git" "$TMP/src" \
      || die "git clone failed for tag $VERSION"
  fi
  log "Building codeup (this takes ~1-3 min)..."
  ( cd "$TMP/src" && cargo build --release --locked --bin codeup ) || die "cargo build failed"
  cp "$TMP/src/target/release/codeup" "$INSTALL_DIR/codeup"
  chmod +x "$INSTALL_DIR/codeup"
}

if [ "$FROM_SOURCE" -eq 1 ]; then
  install_from_source
else
  install_prebuilt || {
    log "Prebuilt install failed — falling back to source build."
    install_from_source
  }
fi

# ---- PATH handling ----------------------------------------------------------

if path_already_has "$INSTALL_DIR"; then
  log "PATH already contains $INSTALL_DIR — no shell rc changes needed."
else
  rc=$(detect_rc)
  case "$MODIFY_PATH" in
    no)
      log ""
      log "Add this to your shell rc to put codeup on PATH:"
      log "  export PATH=\"$INSTALL_DIR:\$PATH\""
      ;;
    yes)
      append_path_block "$rc" "$INSTALL_DIR"
      ;;
    auto)
      if [ -t 0 ] && [ -t 1 ]; then
        printf 'Add %s to PATH in %s? [Y/n] ' "$INSTALL_DIR" "$rc"
        read -r reply || reply=""
        case "$reply" in
          ''|y|Y|yes|YES) append_path_block "$rc" "$INSTALL_DIR" ;;
          *) log "Skipping PATH update. To add manually:"; log "  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
        esac
      else
        # Non-interactive (piped from curl). Don't silently edit rc files.
        log ""
        log "$INSTALL_DIR is not on PATH. Add this line to your shell rc:"
        log "  export PATH=\"$INSTALL_DIR:\$PATH\""
        log "Or re-run with --yes to let the installer append it for you."
      fi
      ;;
  esac
fi

# ---- verify -----------------------------------------------------------------

log ""
if "$INSTALL_DIR/codeup" --version >/dev/null 2>&1; then
  log "Installed: $("$INSTALL_DIR/codeup" --version) → $INSTALL_DIR/codeup"
else
  log "Installed: $INSTALL_DIR/codeup (couldn't run --version; check that the binary is executable on this platform)"
fi
log "Try: codeup --help"
