# tonin — developer + release Makefile.
#
# Run `make` (or `make help`) for the table of available targets.
# Convention: each target is documented with `## <description>` on the
# same line as the rule header; `help` parses those out with awk.

CARGO ?= cargo
VERSION ?=
RUSTDOCFLAGS ?= -D warnings

# Default to printing the help table when invoked with no args.
.DEFAULT_GOAL := help

.PHONY: help build check test e2e test-all fmt fmt-check lint doc ci clean gen-example \
        install-cli install-hooks publish-dry publish show-version check-version version \
        bump-patch bump-minor bump-major

help: ## Show this help table
	@awk 'BEGIN {FS = ":.*##"; printf "Available targets:\n\n"} \
	      /^[a-z][a-z0-9-]*:.*##/ { printf "  make %-15s — %s\n", $$1, $$2 }' \
	      $(MAKEFILE_LIST)

# ---------------------------------------------------------------------------
# Local dev loop
# ---------------------------------------------------------------------------

build: ## Compile every workspace member (debug profile)
	$(CARGO) build --workspace

check: ## Type-check the workspace including tests/examples
	$(CARGO) check --workspace --all-targets

test: ## Run unit and contract tests (no Docker required)
	$(CARGO) nextest run --workspace

e2e: ## Run all tests including E2E (requires Docker/testcontainers)
	$(CARGO) nextest run --workspace --include-ignored

test-all: ## Run all tests (unit, contract, E2E with testcontainers)
	$(CARGO) nextest run --workspace --include-ignored

fmt: ## Format all crates with rustfmt
	$(CARGO) fmt --all

fmt-check: ## Verify formatting without rewriting files
	$(CARGO) fmt --all -- --check

lint: ## Run clippy across the workspace, warnings denied
	$(CARGO) clippy --workspace --all-targets -- -D warnings

doc: ## Build rustdoc for the workspace, warnings denied
	RUSTDOCFLAGS="$(RUSTDOCFLAGS)" $(CARGO) doc --workspace --no-deps

ci: fmt-check lint test doc check-version check-version-sdk check-version-proxy ## Run the same gate CI runs (fmt + lint + test + doc + version-sync)
	@echo "ci: all checks passed"

install-hooks: ## Wire up git hooks (run once after cloning)
	cp scripts/pre-commit .git/hooks/pre-commit
	chmod +x .git/hooks/pre-commit
	cp scripts/commit-msg .git/hooks/commit-msg
	chmod +x .git/hooks/commit-msg
	@echo "hooks installed: pre-commit, commit-msg"

gen-example: ## Re-render examples/greeter Helm chart and confirm it builds
	$(CARGO) build -p greeter
	$(CARGO) run -p tonin -- helm generate --path examples/greeter
	@echo "gen-example: greeter builds and Helm chart renders"

clean: ## Remove cargo build artifacts
	$(CARGO) clean

install-cli: ## Build and install the `tonin` CLI from source
	$(CARGO) install --path crates/tonin

# ---------------------------------------------------------------------------
# Publishing
# ---------------------------------------------------------------------------

# Dry-run publish for the four leaf crates. The dependent crates
# (tonin-sdk and tonin) can't dry-run cleanly until their upstream
# deps actually exist on crates.io, so we skip them here.
publish-dry: ## Dry-run `cargo publish` for the leaf crates
	$(CARGO) publish --dry-run --allow-dirty -p tonin-client
	$(CARGO) publish --dry-run --allow-dirty -p tonin-mcp-macros
	$(CARGO) publish --dry-run --allow-dirty -p tonin-build
	$(CARGO) publish --dry-run --allow-dirty -p tonin-plugin
	@echo "publish-dry: leaf crates packaged cleanly"

# Real publish: walk crates in dependency order, sleeping between each
# step so the crates.io index has time to propagate before the next crate
# (which depends on the previous one) is uploaded.
#
# Idempotent: if a crate version is already on crates.io ("already uploaded"),
# the step prints a notice and continues rather than failing. This allows
# re-running a release workflow after a partial failure without needing a new
# version bump.
#
# Publish order (must respect the internal dep graph):
#   1. tonin-client      — leaf, no internal deps
#   2. tonin-mcp-macros  — leaf, no internal deps
#   3. tonin-build       — leaf, no internal deps
#   4. tonin-plugin      — leaf, no internal deps
#   5. tonin-sdk         — depends on tonin-client + tonin-mcp-macros
#   6. tonin             — CLI binary, depends on tonin-plugin
define cargo_publish
	@out=$$($(CARGO) publish -p $(1) 2>&1); rc=$$?; \
	printf '%s\n' "$$out"; \
	if [ $$rc -ne 0 ]; then \
	  echo "$$out" | grep -qE "already uploaded|already exists on crates\.io" \
	    && echo "$(1): already on crates.io, skipping" \
	    || exit $$rc; \
	fi
endef

publish: ## Publish all six crates to crates.io in dep order
	@test -n "$${CARGO_REGISTRY_TOKEN}" || \
	  (echo "CARGO_REGISTRY_TOKEN not set" >&2; exit 1)
	@echo "publish: tonin-client"
	$(call cargo_publish,tonin-client)
	sleep 30
	@echo "publish: tonin-mcp-macros"
	$(call cargo_publish,tonin-mcp-macros)
	sleep 30
	@echo "publish: tonin-build"
	$(call cargo_publish,tonin-build)
	sleep 30
	@echo "publish: tonin-plugin"
	$(call cargo_publish,tonin-plugin)
	sleep 30
	@echo "publish: tonin-sdk (larger crate, longer sleep after)"
	$(call cargo_publish,tonin-sdk)
	sleep 60
	@echo "publish: tonin (CLI binary)"
	$(call cargo_publish,tonin)
	@echo "publish: all six crates done"

# ---------------------------------------------------------------------------
# Versioning (single unified workspace version)
# ---------------------------------------------------------------------------
#
# All three published packages share ONE version, the workspace version in
# /VERSION (mirrored into [workspace.package].version in Cargo.toml). tonin-sdk
# and tonin-proxy inherit it via `version.workspace = true`.
#
# Releases are fully automated by .github/workflows/auto-release.yml:
#   1. Merge a PR to main. The workflow reads Conventional Commits since the
#      last v* tag (feat → minor, fix → patch, breaking → minor pre-1.0).
#   2. It bumps /VERSION + Cargo.toml + Cargo.lock, commits to main, tags v*,
#      creates the GitHub Release, and dispatches release.yml to publish.
#
# So you normally DON'T bump by hand — just land Conventional-Commit PRs. The
# targets below are for LOCAL version management (inspecting, or forcing a
# specific version inside a PR). They do NOT tag or push.

show-version: ## Print the current unified workspace version (from /VERSION)
	@cat VERSION

check-version: ## Verify VERSION file and Cargo.toml [workspace.package].version are in sync
	@FILE="$$(tr -d '[:space:]' < VERSION)"; \
	 CARGO="$$(awk ' \
	   /^\[workspace\.package\]/ { s=1; next } \
	   /^\[/ && s { s=0 } \
	   s && /^version[[:space:]]*=/ { match($$0, /"[^"]+"/) ; print substr($$0, RSTART+1, RLENGTH-2) ; exit } \
	 ' Cargo.toml)"; \
	 if [ "$$FILE" != "$$CARGO" ]; then \
	   echo "error: VERSION ($$FILE) and Cargo.toml ($$CARGO) are out of sync" >&2; \
	   echo "fix:   scripts/bump-version.sh $$CARGO" >&2; \
	   exit 1; \
	 fi; \
	 echo "check-version: ok ($$FILE)"

version: ## Bump VERSION + Cargo.toml to an explicit X.Y.Z and commit locally (VERSION=X.Y.Z)
	@test -n "$(VERSION)" || \
	  (echo "usage: make version VERSION=X.Y.Z" >&2; exit 1)
	./scripts/bump-version.sh "$(VERSION)"

bump-patch: ## Bump unified PATCH version locally (does not tag/push)
	./scripts/bump-version.sh patch

bump-minor: ## Bump unified MINOR version locally (does not tag/push)
	./scripts/bump-version.sh minor

bump-major: ## Bump unified MAJOR version locally (does not tag/push)
	./scripts/bump-version.sh major
