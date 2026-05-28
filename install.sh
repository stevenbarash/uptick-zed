#!/usr/bin/env bash
#
# Uptick installer for macOS and Linux.
#
# Downloads the latest released `uptick-lsp` binary from GitHub, verifies its
# .sha256 sidecar, installs it under $PREFIX/bin (default ~/.local/bin), and
# warns if that directory is not on PATH. Optionally clones the repo so you
# can install the Zed dev extension that launches the binary.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/stevenbarash/uptick-zed/main/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/stevenbarash/uptick-zed/main/install.sh | bash -s -- --clone
#
# Flags:
#   --prefix DIR   Install directory base. Binary lands in <DIR>/bin.
#                  Default: $HOME/.local
#   --version VER  Tag without leading v (e.g. 0.4.0). Default: latest non-prerelease.
#   --clone        Also clone the repo to ~/.local/share/uptick-zed for the
#                  Zed extension. Skipped if the directory already exists.
#   --no-verify    Skip sha256 verification. Discouraged.
#   --help         Show this help and exit.

set -euo pipefail

REPO="stevenbarash/uptick-zed"
PREFIX="${PREFIX:-$HOME/.local}"
VERSION=""
CLONE=0
VERIFY=1

usage() {
  cat <<'EOF'
Uptick installer for macOS and Linux.

Downloads the latest released uptick-lsp binary from GitHub, verifies its
.sha256 sidecar, installs it under $PREFIX/bin (default ~/.local/bin), and
warns if that directory is not on PATH. Optionally clones the repo so you
can install the Zed dev extension that launches the binary.

Usage:
  curl -fsSL https://raw.githubusercontent.com/stevenbarash/uptick-zed/main/install.sh | bash
  curl -fsSL https://raw.githubusercontent.com/stevenbarash/uptick-zed/main/install.sh | bash -s -- --clone

Flags:
  --prefix DIR   Install directory base. Binary lands in <DIR>/bin.
                 Default: $HOME/.local
  --version VER  Tag without leading v (e.g. 0.4.0). Default: latest non-prerelease.
  --clone        Also clone the repo to ~/.local/share/uptick-zed for the
                 Zed extension. Skipped if the directory already exists.
  --no-verify    Skip sha256 verification. Discouraged.
  --help         Show this help and exit.
EOF
  exit "${1:-0}"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix)    PREFIX="$2"; shift 2 ;;
    --version)   VERSION="$2"; shift 2 ;;
    --clone)     CLONE=1; shift ;;
    --no-verify) VERIFY=0; shift ;;
    --help|-h)   usage 0 ;;
    *) printf 'Unknown flag: %s\n' "$1" >&2; usage 1 ;;
  esac
done

err() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }
note() { printf '==> %s\n' "$*"; }

# --- Detect OS + arch -> Rust target triple ----------------------------------
uname_s="$(uname -s)"
uname_m="$(uname -m)"

case "$uname_s" in
  Darwin)
    case "$uname_m" in
      arm64|aarch64) target="aarch64-apple-darwin" ;;
      x86_64)        target="x86_64-apple-darwin" ;;
      *) err "Unsupported macOS architecture: $uname_m" ;;
    esac
    ;;
  Linux)
    case "$uname_m" in
      x86_64|amd64) target="x86_64-unknown-linux-gnu" ;;
      *) err "Unsupported Linux architecture: $uname_m (only x86_64 prebuilts are published today)" ;;
    esac
    ;;
  *) err "Unsupported OS: $uname_s. On Windows, use install.ps1." ;;
esac
note "Detected target: $target"

# --- Resolve version ---------------------------------------------------------
if [ -z "$VERSION" ]; then
  note "Resolving latest release tag..."
  api_url="https://api.github.com/repos/$REPO/releases/latest"
  tag="$(curl -fsSL "$api_url" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
  [ -n "$tag" ] || err "Could not resolve latest release tag from $api_url"
  VERSION="${tag#v}"
fi
note "Using version: $VERSION"

# --- Required tools ----------------------------------------------------------
need() { command -v "$1" >/dev/null 2>&1 || err "Required tool not found on PATH: $1"; }
need curl
need tar
if [ "$VERIFY" -eq 1 ]; then
  if command -v shasum >/dev/null 2>&1; then
    sha_cmd=(shasum -a 256)
  elif command -v sha256sum >/dev/null 2>&1; then
    sha_cmd=(sha256sum)
  else
    err "Need shasum or sha256sum for checksum verification (or pass --no-verify)."
  fi
fi

# --- Download + verify -------------------------------------------------------
archive="uptick-lsp-${VERSION}-${target}.tar.gz"
base_url="https://github.com/$REPO/releases/download/v${VERSION}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

note "Downloading $archive"
curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp/$archive" "$base_url/$archive" \
  || err "Download failed: $base_url/$archive"

if [ "$VERIFY" -eq 1 ]; then
  note "Verifying sha256"
  curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp/$archive.sha256" "$base_url/$archive.sha256" \
    || err "Could not download checksum sidecar: $base_url/$archive.sha256"
  ( cd "$tmp" && "${sha_cmd[@]}" -c "$archive.sha256" >/dev/null ) \
    || err "Checksum verification failed for $archive"
fi

# --- Extract + install -------------------------------------------------------
note "Extracting"
tar -xzf "$tmp/$archive" -C "$tmp"
[ -f "$tmp/uptick-lsp" ] || err "Archive did not contain expected 'uptick-lsp' binary"

bin_dir="$PREFIX/bin"
mkdir -p "$bin_dir"
install -m 0755 "$tmp/uptick-lsp" "$bin_dir/uptick-lsp"
note "Installed: $bin_dir/uptick-lsp"

# --- PATH check --------------------------------------------------------------
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *)
    # Surface a shell-specific suggestion when we recognise $SHELL — saves
    # the user from guessing which rc file to edit. Unknown shells fall
    # through to the generic list.
    shell_name="$(basename "${SHELL:-}")"
    case "$shell_name" in
      zsh)
        rc="${ZDOTDIR:-$HOME}/.zshrc"
        suggestion="echo 'export PATH=\"$bin_dir:\$PATH\"' >> $rc"
        ;;
      bash)
        # macOS bash sources ~/.bash_profile for login shells; Linux bash
        # uses ~/.bashrc. Pick the one that exists, default to ~/.bashrc.
        if [ "$uname_s" = "Darwin" ] && [ -f "$HOME/.bash_profile" ]; then
          rc="$HOME/.bash_profile"
        else
          rc="$HOME/.bashrc"
        fi
        suggestion="echo 'export PATH=\"$bin_dir:\$PATH\"' >> $rc"
        ;;
      fish)
        suggestion="fish_add_path '$bin_dir'"
        ;;
      *)
        suggestion=""
        ;;
    esac

    printf '\nNote: %s is not on your PATH.\n' "$bin_dir"
    if [ -n "$suggestion" ]; then
      printf '  Run:  %s\n' "$suggestion"
      printf '  Then restart your shell.\n'
    else
      printf '  Add %s to PATH in your shell rc, then restart the shell.\n' "$bin_dir"
    fi
    printf 'Zed itself does not need PATH — it launches the binary by absolute\n'
    printf 'path. This only matters if you also want to run uptick-lsp manually.\n'
    ;;
esac

# --- Optional repo clone for Zed dev extension -------------------------------
clone_dir=""
if [ "$CLONE" -eq 1 ]; then
  clone_dir="$HOME/.local/share/uptick-zed"
  if [ -d "$clone_dir/.git" ]; then
    note "Repo already cloned at $clone_dir; skipping."
  else
    need git
    note "Cloning extension repo to $clone_dir"
    mkdir -p "$(dirname "$clone_dir")"
    git clone --depth 1 "https://github.com/$REPO.git" "$clone_dir"
  fi
fi

# --- Next steps + smoke test -------------------------------------------------
cat <<EOF

==> Next steps

  1. Install the Zed extension:
EOF
if [ -n "$clone_dir" ]; then
  printf "       Open Zed → command palette → 'zed: install dev extension'\n"
  printf "       Select: %s\n" "$clone_dir"
else
  printf "       git clone https://github.com/%s ~/.local/share/uptick-zed\n" "$REPO"
  printf "       Open Zed → command palette → 'zed: install dev extension'\n"
  printf "       Select: ~/.local/share/uptick-zed\n"
fi
cat <<'EOF'

  2. Smoke test — should flag lodash 4.17.15 as critically vulnerable:
       echo '{"dependencies":{"lodash":"4.17.15"}}' > /tmp/uptick-smoke.json
       zed /tmp/uptick-smoke.json
       # → red diagnostic on the literal + '⛔' code lens above the line

EOF

note "Done."
