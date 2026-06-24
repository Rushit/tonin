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
        install-cli install-hooks publish-dry publish version release show-version check-version \
        bump-patch bump-minor bump-major \
        check-version-sdk show-version-sdk version-sdk release-sdk \
        bump-patch-sdk bump-minor-sdk bump-major-sdk

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

ci: fmt-check lint test doc check-version check-version-sdk ## Run the same gate CI runs (fmt + lint + test + doc + version-sync)
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
# Publish order (must respect the internal dep graph):
#   1. tonin-client      — leaf, no internal deps
#   2. tonin-mcp-macros  — leaf, no internal deps
#   3. tonin-build       — leaf, no internal deps
#   4. tonin-plugin      — leaf, no internal deps
#   5. tonin-sdk         — depends on tonin-client + tonin-mcp-macros
#   6. tonin             — CLI binary, depends on tonin-plugin
publish: ## Publish all six crates to crates.io in dep order
	@test -n "$${CARGO_REGISTRY_TOKEN}" || \
	  (echo "CARGO_REGISTRY_TOKEN not set" >&2; exit 1)
	@echo "publish: tonin-client"
	$(CARGO) publish -p tonin-client
	sleep 30
	@echo "publish: tonin-mcp-macros"
	$(CARGO) publish -p tonin-mcp-macros
	sleep 30
	@echo "publish: tonin-build"
	$(CARGO) publish -p tonin-build
	sleep 30
	@echo "publish: tonin-plugin"
	$(CARGO) publish -p tonin-plugin
	sleep 30
	@echo "publish: tonin-sdk (larger crate, longer sleep after)"
	$(CARGO) publish -p tonin-sdk
	sleep 60
	@echo "publish: tonin (CLI binary)"
	$(CARGO) publish -p tonin
	@echo "publish: all six crates uploaded"

# ---------------------------------------------------------------------------
# Versioning + release
# ---------------------------------------------------------------------------
#
# /VERSION is the source of truth. `scripts/bump-version.sh` mirrors it
# into Cargo.toml's [workspace.package].version and refreshes Cargo.lock,
# all in a single bump-commit. The release workflow reads /VERSION directly.

show-version: ## Print the current workspace version (from /VERSION)
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

version: ## Bump /VERSION + Cargo.toml to an explicit X.Y.Z and commit (VERSION=X.Y.Z)
	@test -n "$(VERSION)" || \
	  (echo "usage: make version VERSION=X.Y.Z" >&2; exit 1)
	./scripts/bump-version.sh "$(VERSION)"

bump-patch: ## Bump workspace PATCH version locally (fix: commits)
	./scripts/bump-version.sh patch

bump-minor: ## Bump workspace MINOR version locally (feat: commits)
	./scripts/bump-version.sh minor

bump-major: ## Bump workspace MAJOR version locally (BREAKING CHANGE)
	./scripts/bump-version.sh major

release: ## Tag and push to origin; pass BUMP=patch|minor|major or VERSION=X.Y.Z to bump first
ifdef VERSION
	./scripts/bump-version.sh "$(VERSION)"
else ifdef BUMP
	./scripts/bump-version.sh "$(BUMP)"
endif
	@TAG="v$$(tr -d '[:space:]' < VERSION)"; \
	echo "→ tagging $$TAG"; \
	git tag -a "$$TAG" -m "Release $$TAG"; \
	echo "→ pushing main"; \
	git push origin main; \
	echo "→ pushing tag $$TAG"; \
	git push origin "$$TAG"; \
	echo; \
	echo "✓ $$TAG released. .github/workflows/release.yml is running:"; \
	echo "  https://github.com/Rushit/tonin/actions"

# ---------------------------------------------------------------------------
# tonin-sdk versioning + release (independent from the core workspace version)
# ---------------------------------------------------------------------------
#
# tonin-sdk lives in crates/tonin-sdk/ and carries its own version in
# crates/tonin-sdk/VERSION (mirrored into crates/tonin-sdk/Cargo.toml
# [package].version). Tags use the prefix "tonin-sdk-v" to distinguish them
# from core workspace releases (which use plain "v") and tonin-helm releases.

show-version-sdk: ## Print tonin-sdk's current version (from crates/tonin-sdk/VERSION)
	@cat crates/tonin-sdk/VERSION

check-version-sdk: ## Verify crates/tonin-sdk/VERSION and Cargo.toml [package].version are in sync
	@FILE="$$(tr -d '[:space:]' < crates/tonin-sdk/VERSION)"; \
	 CARGO="$$(awk ' \
	   /^\[package\]/ { s=1; next } \
	   /^\[/ && s { s=0 } \
	   s && /^version[[:space:]]*=/ { match($$0, /"[^"]+"/) ; print substr($$0, RSTART+1, RLENGTH-2) ; exit } \
	 ' crates/tonin-sdk/Cargo.toml)"; \
	 if [ "$$FILE" != "$$CARGO" ]; then \
	   echo "error: crates/tonin-sdk/VERSION ($$FILE) and crates/tonin-sdk/Cargo.toml ($$CARGO) are out of sync" >&2; \
	   echo "fix:   crates/tonin-sdk/scripts/bump-version.sh $$CARGO" >&2; \
	   exit 1; \
	 fi; \
	 echo "check-version-sdk: ok ($$FILE)"

version-sdk: ## Bump crates/tonin-sdk VERSION + Cargo.toml and commit (VERSION=X.Y.Z)
	@test -n "$(VERSION)" || \
	  (echo "usage: make version-sdk VERSION=X.Y.Z" >&2; exit 1)
	./crates/tonin-sdk/scripts/bump-version.sh "$(VERSION)"

bump-patch-sdk: ## Bump tonin-sdk PATCH version locally (fix: commits)
	./crates/tonin-sdk/scripts/bump-version.sh patch

bump-minor-sdk: ## Bump tonin-sdk MINOR version locally (feat: commits)
	./crates/tonin-sdk/scripts/bump-version.sh minor

bump-major-sdk: ## Bump tonin-sdk MAJOR version locally (BREAKING CHANGE)
	./crates/tonin-sdk/scripts/bump-version.sh major

release-sdk: ## Tag and push tonin-sdk; pass BUMP=patch|minor|major or VERSION=X.Y.Z to bump first
ifdef VERSION
	./crates/tonin-sdk/scripts/bump-version.sh "$(VERSION)"
else ifdef BUMP
	./crates/tonin-sdk/scripts/bump-version.sh "$(BUMP)"
endif
	@TAG="tonin-sdk-v$$(tr -d '[:space:]' < crates/tonin-sdk/VERSION)"; \
	echo "→ tagging $$TAG"; \
	git tag -a "$$TAG" -m "Release tonin-sdk $$TAG"; \
	echo "→ pushing main"; \
	git push origin main; \
	echo "→ pushing tag $$TAG"; \
	git push origin "$$TAG"; \
	echo; \
	echo "✓ $$TAG released. .github/workflows/release-tonin-sdk.yml is running:"; \
	echo "  https://github.com/Rushit/tonin/actions"
