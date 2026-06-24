#!/usr/bin/env bash
set -euo pipefail

# bump-version.sh — bump the tonin-helm version (source of truth:
# crates/tonin-helm/VERSION), mirror it into Cargo.toml [package].version,
# and create a clean commit.
#
# Operates only on crates/tonin-helm/ — does NOT touch the workspace
# [workspace.package].version (which is the tonin-* packages version).
#
# Usage (from repo root or anywhere):
#   crates/tonin-helm/scripts/bump-version.sh <X.Y.Z>
# Or via make:
#   make version-helm VERSION=X.Y.Z

if [[ $# -ne 1 ]]; then
    echo "error: expected exactly one argument (the new version)" >&2
    echo "usage: $0 <X.Y.Z>" >&2
    exit 1
fi

NEW="$1"

if [[ ! "$NEW" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.-]+)?$ ]]; then
    echo "error: '$NEW' is not a valid SemVer string" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$CRATE_DIR/../.." && pwd)"
cd "$REPO_ROOT"

if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "error: working tree has uncommitted changes; commit or stash them first" >&2
    git status --short >&2
    exit 1
fi

VERSION_FILE="$CRATE_DIR/VERSION"
CARGO_FILE="$CRATE_DIR/Cargo.toml"

if [[ ! -f "$VERSION_FILE" ]]; then
    echo "error: $VERSION_FILE not found" >&2
    exit 1
fi

CURRENT=$(tr -d '[:space:]' < "$VERSION_FILE")

CARGO_CURRENT=$(awk '
    /^\[package\]/ { in_section=1; next }
    /^\[/ && in_section { in_section=0 }
    in_section && /^version[[:space:]]*=/ {
        match($0, /"[^"]+"/)
        print substr($0, RSTART+1, RLENGTH-2)
        exit
    }
' "$CARGO_FILE")

if [[ "$CARGO_CURRENT" != "$CURRENT" ]]; then
    echo "error: VERSION ($CURRENT) and Cargo.toml ($CARGO_CURRENT) disagree" >&2
    echo "       Resync both to match, then re-run this script." >&2
    exit 1
fi

if [[ "$NEW" == "$CURRENT" ]]; then
    echo "error: new version equals current version ($CURRENT); nothing to do" >&2
    exit 1
fi

echo "Current tonin-helm version: $CURRENT"
echo "New tonin-helm version:     $NEW"

# Update VERSION
echo "$NEW" > "$VERSION_FILE"

# Update Cargo.toml [package].version
awk -v new="$NEW" '
    BEGIN { in_section=0; replaced=0 }
    /^\[package\]/ { in_section=1; print; next }
    /^\[/ && in_section { in_section=0 }
    in_section && !replaced && /^version[[:space:]]*=/ {
        print "version = \"" new "\""
        replaced=1
        next
    }
    { print }
' "$CARGO_FILE" > "$CARGO_FILE.bak" && mv "$CARGO_FILE.bak" "$CARGO_FILE"

VERIFY=$(awk '
    /^\[package\]/ { in_section=1; next }
    /^\[/ && in_section { in_section=0 }
    in_section && /^version[[:space:]]*=/ {
        match($0, /"[^"]+"/)
        print substr($0, RSTART+1, RLENGTH-2)
        exit
    }
' "$CARGO_FILE")

if [[ "$VERIFY" != "$NEW" ]]; then
    echo "error: Cargo.toml rewrite failed (still reads '$VERIFY')" >&2
    exit 1
fi

echo "Refreshing Cargo.lock via cargo check -p tonin-helm..."
cargo check -p tonin-helm

git add "$VERSION_FILE" "$CARGO_FILE" Cargo.lock
git commit -m "chore: release tonin-helm v$NEW"

echo
echo "Bumped tonin-helm $CURRENT → $NEW, committed."
echo "Next: make release-helm VERSION=$NEW to tag and push."
