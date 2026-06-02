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

.PHONY: help build check test fmt fmt-check lint doc ci clean gen-example \
        install-cli publish-dry publish version release show-version

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

test: ## Run the full workspace test suite
	$(CARGO) test --workspace

fmt: ## Format all crates with rustfmt
	$(CARGO) fmt --all

fmt-check: ## Verify formatting without rewriting files
	$(CARGO) fmt --all -- --check

lint: ## Run clippy across the workspace, warnings denied
	$(CARGO) clippy --workspace --all-targets -- -D warnings

doc: ## Build rustdoc for the workspace, warnings denied
	RUSTDOCFLAGS="$(RUSTDOCFLAGS)" $(CARGO) doc --workspace --no-deps

ci: fmt-check lint test doc ## Run the same gate CI runs (fmt + lint + test + doc)
	@echo "ci: all checks passed"

# Proves the templates + generated code still hold together: `cargo build`
# runs the example's build.rs codegen (tonic-build) and compiles the result;
# `k8s generate` re-renders its manifests from tonin.toml. Run after touching
# any template under crates/tonin/templates/ or the codegen module.
gen-example: ## Re-render examples/greeter manifests and confirm it builds
	$(CARGO) build -p greeter
	$(CARGO) run -p tonin -- k8s generate --path examples/greeter
	@echo "gen-example: greeter builds and manifests render"

clean: ## Remove cargo build artifacts
	$(CARGO) clean

install-cli: ## Build and install the `tonin` CLI from source
	$(CARGO) install --path crates/tonin

# ---------------------------------------------------------------------------
# Publishing
# ---------------------------------------------------------------------------

# Dry-run publish for the three leaf crates only. The dependent crates
# (tonin-core and tonin) can't dry-run cleanly until their upstream
# deps actually exist on crates.io, so we skip them here.
publish-dry: ## Dry-run `cargo publish` for the leaf crates
	$(CARGO) publish --dry-run --allow-dirty -p tonin-client
	$(CARGO) publish --dry-run --allow-dirty -p tonin-mcp-macros
	$(CARGO) publish --dry-run --allow-dirty -p tonin-build
	@echo "publish-dry: leaf crates packaged cleanly"

# Real publish: walk crates in dependency order, sleeping between each
# step so the crates.io index has time to propagate before the next crate
# (which depends on the previous one) is uploaded. `tonin` is published
# last because it's dual-purpose (library + the `tonin` CLI binary).
publish: ## Publish all five crates to crates.io in dep order
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
	@echo "publish: tonin-core (larger crate, longer sleep after)"
	$(CARGO) publish -p tonin-core
	sleep 60
	@echo "publish: tonin (umbrella library + CLI binary)"
	$(CARGO) publish -p tonin
	@echo "publish: all five crates uploaded"

# ---------------------------------------------------------------------------
# Versioning + release
# ---------------------------------------------------------------------------
#
# /VERSION is the source of truth. `scripts/bump-version.sh` mirrors it
# into Cargo.toml's [workspace.package].version and refreshes Cargo.lock,
# all in a single bump-commit. The release workflow reads /VERSION directly.

show-version: ## Print the current workspace version (from /VERSION)
	@cat VERSION

version: ## Bump /VERSION + Cargo.toml mirror and commit (VERSION=X.Y.Z)
	@test -n "$(VERSION)" || \
	  (echo "usage: make version VERSION=X.Y.Z" >&2; exit 1)
	./scripts/bump-version.sh "$(VERSION)"

release: version ## Bump, tag, and push vX.Y.Z to origin (fires the publish workflow)
	@echo "→ tagging v$(VERSION)"
	git tag -a "v$(VERSION)" -m "Release v$(VERSION)"
	@echo "→ pushing main"
	git push origin main
	@echo "→ pushing tag v$(VERSION)"
	git push origin "v$(VERSION)"
	@echo
	@echo "✓ v$(VERSION) released. .github/workflows/release.yml is running:"
	@echo "  https://github.com/Rushit/tonin/actions"
	@echo "  (verify → make ci → publish 5 crates → cross-platform binaries → GitHub Release)"
