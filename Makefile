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
        bump-patch bump-minor bump-major \
        show-version-sdk check-version-sdk version-sdk \
        bump-patch-sdk bump-minor-sdk bump-major-sdk \
        show-version-proxy check-version-proxy version-proxy \
        bump-patch-proxy bump-minor-proxy bump-major-proxy \
        bump release

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
# Versioning
# ---------------------------------------------------------------------------
#
# Releases are fully automated via release-please (.github/workflows/release-please.yml):
#   1. Merge a feature PR → release-please opens a Release PR with CHANGELOG +
#      version bump (driven by Conventional Commits since the last tag).
#   2. The Release PR is auto-squash-merged by the automerge job.
#   3. release-please pushes the semver tag + creates the GitHub Release.
#   4. release.yml / release-tonin-sdk.yml publish to crates.io and build binaries.
#
# The targets below are for LOCAL version management only (e.g. testing a
# specific version, or bumping without committing to main). They do NOT tag or
# push — release-please owns that.

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

version: ## Bump /VERSION + Cargo.toml to an explicit X.Y.Z and commit locally (VERSION=X.Y.Z)
	@test -n "$(VERSION)" || \
	  (echo "usage: make version VERSION=X.Y.Z" >&2; exit 1)
	./scripts/bump-version.sh "$(VERSION)"

bump-patch: ## Bump workspace PATCH version locally (does not tag/push)
	./scripts/bump-version.sh patch

bump-minor: ## Bump workspace MINOR version locally (does not tag/push)
	./scripts/bump-version.sh minor

bump-major: ## Bump workspace MAJOR version locally (does not tag/push)
	./scripts/bump-version.sh major

# ---------------------------------------------------------------------------
# tonin-sdk versioning (independent from the core workspace version)
# ---------------------------------------------------------------------------
#
# tonin-sdk lives in crates/tonin-sdk/ and carries its own version in
# crates/tonin-sdk/VERSION (mirrored into crates/tonin-sdk/Cargo.toml
# [package].version). Tags use the prefix "tonin-sdk-v" to distinguish them
# from core workspace releases. Tagging and pushing are handled by
# release-please; the targets below are for local version management only.

show-version-sdk: ## Print tonin-sdk's current version (from crates/tonin-sdk/VERSION)
	@awk '{print $$1}' crates/tonin-sdk/VERSION

check-version-sdk: ## Verify crates/tonin-sdk/VERSION and Cargo.toml [package].version are in sync
	@FILE="$$(awk '{print $$1}' crates/tonin-sdk/VERSION)"; \
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

version-sdk: ## Bump crates/tonin-sdk VERSION + Cargo.toml and commit locally (VERSION=X.Y.Z)
	@test -n "$(VERSION)" || \
	  (echo "usage: make version-sdk VERSION=X.Y.Z" >&2; exit 1)
	./crates/tonin-sdk/scripts/bump-version.sh "$(VERSION)"

bump-patch-sdk: ## Bump tonin-sdk PATCH version locally (does not tag/push)
	./crates/tonin-sdk/scripts/bump-version.sh patch

bump-minor-sdk: ## Bump tonin-sdk MINOR version locally (does not tag/push)
	./crates/tonin-sdk/scripts/bump-version.sh minor

bump-major-sdk: ## Bump tonin-sdk MAJOR version locally (does not tag/push)
	./crates/tonin-sdk/scripts/bump-version.sh major

# ---------------------------------------------------------------------------
# tonin-proxy versioning (independent from the core workspace version)
# ---------------------------------------------------------------------------

show-version-proxy: ## Print tonin-proxy's current version (from crates/tonin-proxy/VERSION)
	@awk '{print $$1}' crates/tonin-proxy/VERSION

check-version-proxy: ## Verify crates/tonin-proxy/VERSION and Cargo.toml [package].version are in sync
	@FILE="$$(awk '{print $$1}' crates/tonin-proxy/VERSION)"; \
	 CARGO="$$(awk ' \
	   /^\[package\]/ { s=1; next } \
	   /^\[/ && s { s=0 } \
	   s && /^version[[:space:]]*=/ { match($$0, /"[^"]+"/) ; print substr($$0, RSTART+1, RLENGTH-2) ; exit } \
	 ' crates/tonin-proxy/Cargo.toml)"; \
	 if [ "$$FILE" != "$$CARGO" ]; then \
	   echo "error: crates/tonin-proxy/VERSION ($$FILE) and crates/tonin-proxy/Cargo.toml ($$CARGO) are out of sync" >&2; \
	   echo "fix:   crates/tonin-proxy/scripts/bump-version.sh $$CARGO" >&2; \
	   exit 1; \
	 fi; \
	 echo "check-version-proxy: ok ($$FILE)"

version-proxy: ## Bump crates/tonin-proxy VERSION + Cargo.toml and commit locally (VERSION=X.Y.Z)
	@test -n "$(VERSION)" || \
	  (echo "usage: make version-proxy VERSION=X.Y.Z" >&2; exit 1)
	./crates/tonin-proxy/scripts/bump-version.sh "$(VERSION)"

bump-patch-proxy: ## Bump tonin-proxy PATCH version locally (does not tag/push)
	./crates/tonin-proxy/scripts/bump-version.sh patch

bump-minor-proxy: ## Bump tonin-proxy MINOR version locally (does not tag/push)
	./crates/tonin-proxy/scripts/bump-version.sh minor

bump-major-proxy: ## Bump tonin-proxy MAJOR version locally (does not tag/push)
	./crates/tonin-proxy/scripts/bump-version.sh major

# ---------------------------------------------------------------------------
# Unified version bump + release (all packages atomically)
# ---------------------------------------------------------------------------
#
# These targets are the primary entry-point for a manual release.  All three
# packages (workspace, tonin-sdk, tonin-proxy) are bumped to the same version
# in a single atomic commit whose message contains "[manual bump]" — the
# release-dispatch.yml workflow detects that marker and automatically creates
# the tags, GitHub Releases, and dispatches the publish workflows.
#
# Typical usage:
#
#   make bump VERSION=1.2.0        # bump + commit locally
#   make release VERSION=1.2.0     # bump + commit + push → triggers CI builds

bump: ## Bump ALL packages to VERSION=X.Y.Z and commit with [manual bump] marker
	@test -n "$(VERSION)" || \
	  (echo "usage: make bump VERSION=X.Y.Z" >&2; exit 1)
	@echo "==> Bumping workspace to $(VERSION)"
	./scripts/bump-version.sh "$(VERSION)"
	@echo "==> Bumping tonin-sdk to $(VERSION)"
	./crates/tonin-sdk/scripts/bump-version.sh "$(VERSION)"
	@echo "==> Bumping tonin-proxy to $(VERSION)"
	./crates/tonin-proxy/scripts/bump-version.sh "$(VERSION)"
	@# Amend the three individual commits into one clean bump commit.
	@COMMITS=$$(git log --oneline -3 --format='%H' | tr '\n' ' '); \
	 FIRST=$$(git log --oneline -3 --format='%H' | tail -1); \
	 git reset --soft "$$FIRST^" && \
	 git commit -m "chore: bump all packages to v$(VERSION) [manual bump]"
	@echo "==> Bumped all packages to $(VERSION) — commit ready to push."
	@echo "    Push with:  git push origin HEAD"
	@echo "    Or run:     make release VERSION=$(VERSION)"

release: ## Bump ALL packages to VERSION=X.Y.Z, commit, and push → triggers CI builds
	@test -n "$(VERSION)" || \
	  (echo "usage: make release VERSION=X.Y.Z" >&2; exit 1)
	$(MAKE) bump VERSION=$(VERSION)
	@echo "==> Pushing to origin/main — release-dispatch.yml will build artifacts."
	git push origin HEAD
