#!/usr/bin/env bash
# Bump crates/tonin-proxy to a new version.
# Usage: ./crates/tonin-proxy/scripts/bump-version.sh X.Y.Z
#        ./crates/tonin-proxy/scripts/bump-version.sh <patch|minor|major>
set -euo pipefail

CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(cd "${CRATE_DIR}/../.." && pwd)"
NEW="${1:?usage: $0 X.Y.Z}"
CURRENT="$(awk '{print $1}' "${CRATE_DIR}/VERSION")"

# If a bump type was given, auto-calculate the new version from the VERSION file.
case "$NEW" in
  patch|minor|major)
    C_MAJOR="${CURRENT%%.*}"
    C_REST="${CURRENT#*.}"
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

echo "Current version: ${CURRENT}"
echo "New version:     ${NEW}"

# Update VERSION file (preserve the # x-release-please-version marker)
echo "${NEW} # x-release-please-version" > "${CRATE_DIR}/VERSION"

# Update Cargo.toml [package].version (first occurrence only)
sed -i "0,/^version = \"${CURRENT}\"/s//version = \"${NEW}\"/" \
  "${CRATE_DIR}/Cargo.toml"

# Refresh Cargo.lock
echo "Refreshing Cargo.lock via cargo check..."
cd "${REPO_ROOT}"
cargo check -p tonin-proxy --quiet

# Commit
git add "${CRATE_DIR}/VERSION" "${CRATE_DIR}/Cargo.toml" Cargo.lock
git commit -m "chore: release tonin-proxy v${NEW} [skip ci]"

echo "Bumped ${CURRENT} → ${NEW} (VERSION + Cargo.toml in sync), committed."
echo "Next: \`make release-proxy VERSION=${NEW}\` to tag, or push main directly."
