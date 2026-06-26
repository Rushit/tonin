#!/usr/bin/env bash
set -euo pipefail

# bump-version.sh — bump the workspace version (source of truth: /VERSION),
# mirror it into [workspace.package].version in Cargo.toml, refresh
# Cargo.lock, and create a clean commit.
#
# /VERSION is the source of truth. Cargo.toml's `[workspace.package].version`
# is a mirror that this script keeps in sync — Cargo requires the version
# to live in Cargo.toml, but humans (and CI) edit /VERSION.
#
# Does NOT tag and does NOT push — those belong to the `release` make target.
#
# Usage: scripts/bump-version.sh <X.Y.Z[-pre]>
#        scripts/bump-version.sh <patch|minor|major>

if [[ $# -ne 1 ]]; then
    echo "error: expected exactly one argument (the new version)" >&2
    echo "usage: $0 <X.Y.Z[-pre]>" >&2
    exit 1
fi

NEW="$1"

# cd to repo root (script lives in <repo>/scripts).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# If a bump type was given, auto-calculate the new version from VERSION file.
case "$NEW" in
  patch|minor|major)
    [[ -f VERSION ]] || { echo "error: VERSION file not found" >&2; exit 1; }
    CURRENT_AUTO="$(tr -d '[:space:]' < VERSION)"
    C_MAJOR="${CURRENT_AUTO%%.*}"
    C_REST="${CURRENT_AUTO#*.}"
    C_MINOR="${C_REST%%.*}"
    C_PATCH="${C_REST#*.}"
    case "$NEW" in
      major) NEW="$((C_MAJOR + 1)).0.0" ;;
      minor) NEW="${C_MAJOR}.$((C_MINOR + 1)).0" ;;
      patch) NEW="${C_MAJOR}.${C_MINOR}.$((C_PATCH + 1))" ;;
    esac
    echo "Auto-calculated bump → ${NEW}"
    ;;
esac

# Validate SemVer (with optional pre-release suffix).
if [[ ! "$NEW" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.-]+)?$ ]]; then
    echo "error: '$NEW' is not a valid SemVer string (expected X.Y.Z or X.Y.Z-prerelease)" >&2
    exit 1
fi

# Refuse if the working tree is dirty — a clean bump-commit shouldn't
# slurp up unrelated work.
if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "error: working tree has uncommitted changes; commit or stash them first" >&2
    git status --short >&2
    exit 1
fi

# Refuse if /VERSION is missing — it should always exist on main.
if [[ ! -f VERSION ]]; then
    echo "error: VERSION file not found at repo root" >&2
    echo "       /VERSION is the source of truth for the workspace version." >&2
    exit 1
fi

CURRENT=$(tr -d '[:space:]' < VERSION)
if [[ -z "$CURRENT" ]]; then
    echo "error: VERSION file is empty" >&2
    exit 1
fi

# Read what's currently in Cargo.toml so we can assert they were in sync
# BEFORE the bump. If they aren't, something is wrong (manual edit, merge
# resolution, etc.) — fix the divergence first, then re-run.
#
# Single awk pass — no pipeline — because:
# - `awk '/start/,/end/'` is ambiguous when the start line also matches
#   the end pattern, which `[workspace.package]` does (both contain `[`),
#   so it returns just the header line.
# - `awk | head -1` under `set -o pipefail` SIGPIPE-kills the upstream
#   and the whole script before the [[ -z ]] guard can fire.
CARGO_CURRENT=$(awk '
    /^\[workspace\.package\]/ { in_block = 1; next }
    in_block && /^\[/         { in_block = 0 }
    in_block && /^version[[:space:]]*=/ {
        match($0, /"[^"]+"/)
        print substr($0, RSTART+1, RLENGTH-2)
        exit
    }
' Cargo.toml)

if [[ -z "$CARGO_CURRENT" ]]; then
    echo "error: could not find version in [workspace.package] block of Cargo.toml" >&2
    exit 1
fi

if [[ "$CARGO_CURRENT" != "$CURRENT" ]]; then
    echo "error: VERSION ($CURRENT) and Cargo.toml ($CARGO_CURRENT) disagree" >&2
    echo "       Resync by editing both to match, then re-run this script." >&2
    exit 1
fi

echo "Current version: $CURRENT"
echo "New version:     $NEW"

if [[ "$NEW" == "$CURRENT" ]]; then
    echo "Notice: New version equals current version ($CURRENT). Skipping version bump."
    exit 0
fi

# 1. Write the new version to VERSION (source of truth).
echo "$NEW" > VERSION

# 2. Mirror it into Cargo.toml's [workspace.package].version.
# State machine: enter the block at [workspace.package], leave at the next
# [section] header, rewrite the first `version = ...` line inside.
awk -v new="$NEW" '
    BEGIN { in_block = 0; replaced = 0 }
    /^\[workspace\.package\]/ {
        in_block = 1
        print
        next
    }
    /^\[/ && in_block {
        in_block = 0
    }
    in_block && !replaced && /^version[[:space:]]*=/ {
        comment = ""
        if (match($0, /#[^"]*$/)) {
            comment = " " substr($0, RSTART, RLENGTH)
        }
        print "version = \"" new "\"" comment
        replaced = 1
        next
    }
    { print }
' Cargo.toml > Cargo.toml.bak
mv Cargo.toml.bak Cargo.toml

# Sanity check the rewrite — same state-machine awk, same reasons.
VERIFY=$(awk '
    /^\[workspace\.package\]/ { in_block = 1; next }
    in_block && /^\[/         { in_block = 0 }
    in_block && /^version[[:space:]]*=/ {
        match($0, /"[^"]+"/)
        print substr($0, RSTART+1, RLENGTH-2)
        exit
    }
' Cargo.toml)

# 3. Update intra-workspace dep pins in [workspace.dependencies].
# Every workspace-versioned tonin* entry has the shape:
#   tonin-plugin = { path = "...", version = "X.Y.Z" }
# When [workspace.package].version bumps, those pins must bump too —
# otherwise downstream crates fail to resolve because the path crate now
# has the new version but the dep declaration still asks for the old one.
#
# tonin-sdk and tonin-proxy are EXCLUDED: they carry independent versions
# and must NOT be bumped here.
#
# State machine: enter the block at [workspace.dependencies], leave at the
# next [section] header, rewrite every `version = "..."` field on lines
# whose key starts with `tonin` but is not `tonin-sdk` or `tonin-proxy`.
awk -v new="$NEW" '
    /^\[workspace\.dependencies\]/ { in_block = 1; print; next }
    /^\[/ && in_block              { in_block = 0 }
    in_block && /^tonin[A-Za-z0-9_-]*[[:space:]]*=.*version[[:space:]]*=/ \
             && !/^tonin-sdk[[:space:]]*=/ \
             && !/^tonin-proxy[[:space:]]*=/ {
        sub(/version[[:space:]]*=[[:space:]]*"[^"]+"/, "version = \"" new "\"")
    }
    { print }
' Cargo.toml > Cargo.toml.bak
mv Cargo.toml.bak Cargo.toml

# Sanity check: every workspace-versioned tonin* dep pin should now read
# the new version. tonin-sdk and tonin-proxy are excluded.
STALE=$(awk -v want="$NEW" '
    /^\[workspace\.dependencies\]/ { in_block = 1; next }
    /^\[/ && in_block              { in_block = 0 }
    in_block && /^tonin[A-Za-z0-9_-]*[[:space:]]*=/ \
             && !/^tonin-sdk[[:space:]]*=/ \
             && !/^tonin-proxy[[:space:]]*=/ {
        if (match($0, /version[[:space:]]*=[[:space:]]*"[^"]+"/)) {
            v = substr($0, RSTART, RLENGTH)
            sub(/^version[[:space:]]*=[[:space:]]*"/, "", v)
            sub(/"$/, "", v)
            if (v != want) print $0
        }
    }
' Cargo.toml)
if [[ -n "$STALE" ]]; then
    echo "error: some intra-workspace dep pins did not bump to $NEW:" >&2
    echo "$STALE" >&2
    exit 1
fi
if [[ "$VERIFY" != "$NEW" ]]; then
    echo "error: Cargo.toml rewrite failed (still reads '$VERIFY')" >&2
    exit 1
fi

# 3. Refresh Cargo.lock. Plain `cargo check` — we want hard build errors to
# surface, but lint warnings are fine.
echo "Refreshing Cargo.lock via cargo check..."
cargo check --workspace

# Update release-please manifest so it stays in sync with manual bumps.
# This avoids the need for release-dispatch to push directly to main (blocked
# by branch protection) — the manifest update travels in the bump commit itself.
if [[ -f ".release-please-manifest.json" ]]; then
    jq --arg v "$NEW" '."." = $v' .release-please-manifest.json \
      > .release-please-manifest.json.tmp \
      && mv .release-please-manifest.json.tmp .release-please-manifest.json
fi

git add VERSION Cargo.toml Cargo.lock .release-please-manifest.json
git commit -m "chore: release v$NEW"

echo
echo "Bumped $CURRENT → $NEW (VERSION + Cargo.toml in sync), committed."
echo "Next: \`make release VERSION=$NEW\` to tag, or push main directly."
