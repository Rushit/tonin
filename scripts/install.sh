#!/usr/bin/env bash
# install.sh — download and install the tonin CLI binary
#
# Usage:
#   # Install latest to ~/.local/bin (if on PATH) or /usr/local/bin
#   curl -sSfL https://raw.githubusercontent.com/Rushit/tonin/main/scripts/install.sh | bash
#
#   # Install a specific version
#   curl -sSfL ... | bash -s -- --version v0.5.4
#
#   # Install to a custom directory
#   curl -sSfL ... | bash -s -- --dir /usr/local/bin
#
#   # Install tonin plus one or more plugins (repeatable, any tonin-* plugin)
#   curl -sSfL ... | bash -s -- --plugin Rushit/tonin-helm
#   curl -sSfL ... | bash -s -- --plugin Rushit/tonin-helm@v0.2.6
#
#   # Back-compat alias for --plugin Rushit/tonin-helm
#   curl -sSfL ... | bash -s -- --with-tonin-helm
#
# Or run locally:
#   ./scripts/install.sh [--version v0.5.4] [--dir ~/.local/bin] \
#       [--plugin <owner/repo>[@vX.Y.Z]]... [--with-tonin-helm [--helm-version v0.1.1]]

set -euo pipefail

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------
TONIN_VERSION=""          # empty = fetch latest from GitHub
HELM_VERSION=""           # empty = fetch latest from GitHub
INSTALL_DIR=""            # empty = auto-detect
WITH_HELM=0
REPO_TONIN="Rushit/tonin"
REPO_HELM="Rushit/tonin-helm"
PLUGIN_SPECS=()           # repeatable --plugin <owner/repo>[@vX.Y.Z]

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)
            TONIN_VERSION="$2"; shift 2 ;;
        --version=*)
            TONIN_VERSION="${1#--version=}"; shift ;;
        --helm-version)
            HELM_VERSION="$2"; shift 2 ;;
        --helm-version=*)
            HELM_VERSION="${1#--helm-version=}"; shift ;;
        --dir)
            INSTALL_DIR="$2"; shift 2 ;;
        --dir=*)
            INSTALL_DIR="${1#--dir=}"; shift ;;
        --with-tonin-helm)
            WITH_HELM=1; shift ;;
        --plugin)
            PLUGIN_SPECS+=("$2"); shift 2 ;;
        --plugin=*)
            PLUGIN_SPECS+=("${1#--plugin=}"); shift ;;
        -h|--help)
            sed -n '/^# Usage:/,/^[^#]/p' "$0" | grep '^#' | sed 's/^# \{0,2\}//'
            exit 0 ;;
        *)
            echo "error: unknown argument '$1'" >&2
            echo "       Run with --help for usage." >&2
            exit 1 ;;
    esac
done

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
say()  { printf '\033[1m%s\033[0m\n' "$*"; }
ok()   { printf '\033[32m✓\033[0m %s\n' "$*"; }
err()  { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" &>/dev/null || err "'$1' is required but not found on PATH."; }

need curl
need tar

# ---------------------------------------------------------------------------
# Detect OS + architecture → Rust target triple
# ---------------------------------------------------------------------------
detect_target() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)
            case "$arch" in
                x86_64)  echo "x86_64-unknown-linux-gnu" ;;
                aarch64|arm64) echo "aarch64-unknown-linux-gnu" ;;
                *) err "Unsupported Linux architecture: $arch (pre-built binary not available; use 'cargo install tonin')" ;;
            esac ;;
        Darwin)
            case "$arch" in
                x86_64)  echo "x86_64-apple-darwin" ;;
                arm64)   echo "aarch64-apple-darwin" ;;
                *) err "Unsupported macOS architecture: $arch" ;;
            esac ;;
        MINGW*|MSYS*|CYGWIN*)
            # Windows via Git Bash / MSYS2 — produces a .zip, handled below
            echo "x86_64-pc-windows-msvc" ;;
        *)
            err "Unsupported OS: $os (pre-built binary not available; use 'cargo install tonin')" ;;
    esac
}

# ---------------------------------------------------------------------------
# Resolve install directory
# ---------------------------------------------------------------------------
resolve_install_dir() {
    if [[ -n "$INSTALL_DIR" ]]; then
        echo "$INSTALL_DIR"
        return
    fi

    # Default: ~/.cargo/bin — same place cargo install puts binaries,
    # already on PATH for anyone with a Rust toolchain installed.
    echo "$HOME/.cargo/bin"
}

# ---------------------------------------------------------------------------
# Fetch latest release tag from GitHub API
# ---------------------------------------------------------------------------
latest_tag() {
    local repo="$1"
    curl -sSf "https://api.github.com/repos/${repo}/releases/latest" \
        | grep '"tag_name"' \
        | head -1 \
        | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/'
}

# ---------------------------------------------------------------------------
# Current installed version of a binary (empty if not installed / unreadable)
# ---------------------------------------------------------------------------
installed_version() {
    local bin="$1" dest_dir="$2"
    local dest="${dest_dir}/${bin}"
    [[ -x "$dest" ]] || { echo ""; return; }
    # clap emits "<bin> X.Y.Z" — strip the binary name prefix
    "$dest" --version 2>/dev/null | awk '{print $NF}' || echo ""
}

# ---------------------------------------------------------------------------
# Download + install one binary
# ---------------------------------------------------------------------------
install_binary() {
    local repo="$1"      # e.g. Rushit/tonin
    local bin="$2"       # e.g. tonin
    local version="$3"   # e.g. v0.5.4
    local target="$4"    # e.g. x86_64-unknown-linux-gnu
    local dest_dir="$5"  # e.g. /usr/local/bin

    local dest="${dest_dir}/${bin}"
    [[ "$target" == *windows* ]] && dest="${dest}.exe"

    # Check currently installed version and skip if already up to date
    local current
    current="$(installed_version "$bin" "$dest_dir")"
    local want="${version#v}"   # strip leading 'v' for comparison
    if [[ -n "$current" && "$current" == "$want" ]]; then
        ok "${bin} ${current} is already up to date."
        return
    fi

    if [[ -n "$current" ]]; then
        say "Upgrading ${bin}: v${current} → ${version}..."
    else
        say "Installing ${bin} ${version}..."
    fi

    local archive_base="${bin}-${target}"
    local ext="tar.gz"
    # Windows releases are .zip
    [[ "$target" == *windows* ]] && ext="zip"

    local url="https://github.com/${repo}/releases/download/${version}/${archive_base}.${ext}"
    local tmp
    tmp="$(mktemp -d)"
    # shellcheck disable=SC2064
    trap "rm -rf '$tmp'" EXIT

    curl -sSfL --proto '=https' --tlsv1.2 -o "${tmp}/archive.${ext}" "$url" \
        || err "Download failed. Check that ${version} exists at https://github.com/${repo}/releases"

    if [[ "$ext" == "tar.gz" ]]; then
        tar -xzf "${tmp}/archive.${ext}" -C "$tmp"
    else
        need unzip
        unzip -q "${tmp}/archive.${ext}" -d "$tmp"
    fi

    local bin_path
    bin_path="$(find "$tmp" -type f -name "${bin}" -o -name "${bin}.exe" | head -1)"
    [[ -n "$bin_path" ]] || err "Binary '${bin}' not found in archive."
    chmod +x "$bin_path"

    # Install via stage-then-atomic-rename. `cp` opens the target with
    # O_TRUNC, which the kernel refuses for a binary that is currently being
    # executed (ETXTBSY) — exactly the case when `tonin upgrade` replaces its
    # own running binary. rename(2) instead swaps the directory entry, leaving
    # the running process on the old inode, so self-upgrade works. The staged
    # file must live in dest_dir so the rename stays on one filesystem
    # (a cross-filesystem `mv` falls back to copy and hits ETXTBSY again).
    local staged="${dest}.new.$$"
    if [[ -w "$dest_dir" ]]; then
        cp "$bin_path" "$staged"
        chmod +x "$staged"
        mv -f "$staged" "$dest"
    else
        say "Installing to ${dest_dir} requires elevated permissions."
        sudo cp "$bin_path" "$staged"
        sudo chmod +x "$staged"
        sudo mv -f "$staged" "$dest"
    fi

    if [[ -n "$current" ]]; then
        ok "Upgraded ${bin}: v${current} → ${version} (${dest})"
    else
        ok "Installed ${bin} ${version} → ${dest}"
    fi
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
TARGET="$(detect_target)"
DEST="$(resolve_install_dir)"

# Ensure install dir exists
mkdir -p "$DEST" 2>/dev/null || true

say "Target:      ${TARGET}"
say "Install dir: ${DEST}"
echo

# Resolve tonin version
if [[ -z "$TONIN_VERSION" ]]; then
    say "Fetching latest tonin version..."
    TONIN_VERSION="$(latest_tag "$REPO_TONIN")"
    [[ -n "$TONIN_VERSION" ]] || err "Could not determine latest version. Pass --version vX.Y.Z explicitly."
fi

install_binary "$REPO_TONIN" "tonin" "$TONIN_VERSION" "$TARGET" "$DEST"

# ---------------------------------------------------------------------------
# Plugins
# ---------------------------------------------------------------------------
# Back-compat: --with-tonin-helm [--helm-version vX] == --plugin <helm repo>[@vX].
if [[ "$WITH_HELM" -eq 1 ]]; then
    if [[ -n "$HELM_VERSION" ]]; then
        PLUGIN_SPECS+=("${REPO_HELM}@${HELM_VERSION}")
    else
        PLUGIN_SPECS+=("$REPO_HELM")
    fi
fi

# Each spec is `owner/repo` or `owner/repo@vX.Y.Z`. The binary name is the
# repo's last path segment (e.g. Rushit/tonin-helm -> tonin-helm), which is
# exactly the `tonin-<name>` plugin convention.
INSTALLED_PLUGINS=()
for spec in ${PLUGIN_SPECS+"${PLUGIN_SPECS[@]}"}; do
    repo="${spec%@*}"
    version=""
    [[ "$spec" == *@* ]] && version="${spec#*@}"
    bin="${repo##*/}"

    echo
    if [[ -z "$version" ]]; then
        say "Fetching latest ${bin} version..."
        version="$(latest_tag "$repo")"
        [[ -n "$version" ]] || err "Could not determine latest ${bin} version. Pass ${repo}@vX.Y.Z explicitly."
    fi
    install_binary "$repo" "$bin" "$version" "$TARGET" "$DEST"
    INSTALLED_PLUGINS+=("$bin")
done

# ---------------------------------------------------------------------------
# PATH reminder
# ---------------------------------------------------------------------------
echo
if ! echo ":$PATH:" | grep -q ":${DEST}:"; then
    printf '\033[33mNote:\033[0m %s is not on your PATH.\n' "$DEST"
    echo "      Add this to your shell profile:"
    echo
    echo "        export PATH=\"${DEST}:\$PATH\""
    echo
fi

ok "Done! Run 'tonin --version' to verify."
for bin in ${INSTALLED_PLUGINS+"${INSTALLED_PLUGINS[@]}"}; do
    name="${bin#tonin-}"
    ok "Run 'tonin ${name} --tonin-describe' to verify ${bin}."
done
