# tonin — developer + release justfile.
#
# Run `just` (or `just --list`) for the table of available recipes.
# Install just: https://github.com/casey/just

set shell := ["bash", "-euo", "pipefail", "-c"]

cargo := env_var_or_default("CARGO", "cargo")
rustdocflags := env_var_or_default("RUSTDOCFLAGS", "-D warnings")

# Show available recipes (default)
[private]
default:
    @just --list

# ---------------------------------------------------------------------------
# Local dev loop
# ---------------------------------------------------------------------------

# Compile every workspace member (debug profile)
build:
    {{ cargo }} build --workspace

# Type-check the workspace including tests/examples
check:
    {{ cargo }} check --workspace --all-targets

# Run unit and contract tests (no Docker required)
test:
    {{ cargo }} nextest run --workspace

# Run all tests including E2E (requires Docker/testcontainers)
e2e:
    {{ cargo }} nextest run --workspace --include-ignored

# Run all tests (unit, contract, E2E with testcontainers)
test-all:
    {{ cargo }} nextest run --workspace --include-ignored

# Format all crates with rustfmt
fmt:
    {{ cargo }} fmt --all

# Verify formatting without rewriting files
fmt-check:
    {{ cargo }} fmt --all -- --check

# Run clippy across the workspace, warnings denied
lint:
    {{ cargo }} clippy --workspace --all-targets -- -D warnings

# Build rustdoc for the workspace, warnings denied
doc:
    RUSTDOCFLAGS="{{ rustdocflags }}" {{ cargo }} doc --workspace --no-deps

# Run the same gate CI runs (fmt + lint + test + doc + version-sync)
ci: fmt-check lint test doc check-version
    @echo "ci: all checks passed"

# Wire up git hooks (run once after cloning)
install-hooks:
    cp scripts/pre-commit .git/hooks/pre-commit
    chmod +x .git/hooks/pre-commit
    cp scripts/commit-msg .git/hooks/commit-msg
    chmod +x .git/hooks/commit-msg
    @echo "hooks installed: pre-commit, commit-msg"

# Re-render examples/greeter Helm chart and confirm it builds
gen-example:
    {{ cargo }} build -p greeter
    {{ cargo }} run -p tonin -- helm generate --path examples/greeter
    @echo "gen-example: greeter builds and Helm chart renders"

# Remove cargo build artifacts
clean:
    {{ cargo }} clean

# Build and install the `tonin` CLI from source
install-cli:
    {{ cargo }} install --path crates/tonin

# ---------------------------------------------------------------------------
# Publishing
# ---------------------------------------------------------------------------

# Dry-run `cargo publish` for the leaf crates
publish-dry:
    {{ cargo }} publish --dry-run --allow-dirty -p tonin-client
    {{ cargo }} publish --dry-run --allow-dirty -p tonin-mcp-macros
    {{ cargo }} publish --dry-run --allow-dirty -p tonin-build
    {{ cargo }} publish --dry-run --allow-dirty -p tonin-plugin
    @echo "publish-dry: leaf crates packaged cleanly"

# Publish all six crates to crates.io in dep order
publish:
    #!/usr/bin/env bash
    set -euo pipefail
    test -n "${CARGO_REGISTRY_TOKEN}" || \
        { echo "CARGO_REGISTRY_TOKEN not set" >&2; exit 1; }

    _cargo_publish() {
        local pkg="$1"
        local out
        out=$({{ cargo }} publish -p "$pkg" 2>&1) && rc=0 || rc=$?
        printf '%s\n' "$out"
        if [ $rc -ne 0 ]; then
            echo "$out" | grep -qE "already uploaded|already exists on crates\.io" \
                && echo "$pkg: already on crates.io, skipping" \
                || exit $rc
        fi
    }

    echo "publish: tonin-client"
    _cargo_publish tonin-client
    sleep 30

    echo "publish: tonin-mcp-macros"
    _cargo_publish tonin-mcp-macros
    sleep 30

    echo "publish: tonin-build"
    _cargo_publish tonin-build
    sleep 30

    echo "publish: tonin-plugin"
    _cargo_publish tonin-plugin
    sleep 30

    echo "publish: tonin-sdk (larger crate, longer sleep after)"
    _cargo_publish tonin-sdk
    sleep 60

    echo "publish: tonin (CLI binary)"
    _cargo_publish tonin
    echo "publish: all six crates done"

# ---------------------------------------------------------------------------
# Versioning (single unified workspace version)
# ---------------------------------------------------------------------------

# Print the current unified workspace version (from /VERSION)
show-version:
    @cat VERSION

# Verify VERSION file and Cargo.toml [workspace.package].version are in sync
check-version:
    #!/usr/bin/env bash
    set -euo pipefail
    FILE="$(tr -d '[:space:]' < VERSION)"
    CARGO="$(awk '
      /^\[workspace\.package\]/ { s=1; next }
      /^\[/ && s { s=0 }
      s && /^version[[:space:]]*=/ { match($0, /"[^"]+"/) ; print substr($0, RSTART+1, RLENGTH-2) ; exit }
    ' Cargo.toml)"
    if [ "$FILE" != "$CARGO" ]; then
        echo "error: VERSION ($FILE) and Cargo.toml ($CARGO) are out of sync" >&2
        echo "fix:   scripts/bump-version.sh $CARGO" >&2
        exit 1
    fi
    echo "check-version: ok ($FILE)"

# Bump VERSION + Cargo.toml to an explicit X.Y.Z and commit locally
version VER:
    ./scripts/bump-version.sh "{{ VER }}"

# Bump unified PATCH version locally (does not tag/push)
bump-patch:
    ./scripts/bump-version.sh patch

# Bump unified MINOR version locally (does not tag/push)
bump-minor:
    ./scripts/bump-version.sh minor

# Bump unified MAJOR version locally (does not tag/push)
bump-major:
    ./scripts/bump-version.sh major
