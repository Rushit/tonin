# Contributing to tonin

Thanks for thinking about contributing. This is a small project — there's no review process to game, no merge queue. The fastest path to a merged change:

1. Read [`docs/01-principles.md`](docs/01-principles.md). If your change cuts against one of the four principles, name the trade-off in the PR description.
2. Open an issue first for anything bigger than a bug fix or doc tweak. Five minutes of "is this the right shape?" beats a week of churn in review.
3. Keep the change focused. Refactor + feature in the same PR is two PRs.

## Dev setup

```bash
# Toolchain is pinned in rust-toolchain.toml — rustup will install it on first cargo invocation.
rustup show

# Required system tools
# - protoc (for tonic-build; brew install protobuf | apt install protobuf-compiler)
# - kubectl (only for the `tonin k8s validate|diff|apply` paths)
# - a local k8s cluster (Rancher Desktop / OrbStack / Docker Desktop / kind / k3d) — see docs/12-kubernetes-deploy.md
```

## Before you push

The same gates CI enforces:

```bash
just ci    # runs fmt-check + lint + test + doc — same gate as CI
```

If that fails locally, CI will fail too. Don't merge over a red CI.

### Useful recipes

Run `just` (or `just --list`) to list every recipe with a one-line description.
The most common ones during development are `just build`, `just test`,
`just fmt`, and `just lint`; see the [justfile](justfile) for the full set
including the publish-side recipes.

## What kinds of changes are welcome

- Bug fixes — anywhere. PR with a regression test.
- Doc fixes — anywhere. Single-line typo PRs are fine.
- New capability impls — e.g. a `tonin-nats` crate implementing `EventBus`. Discuss the trait surface in an issue first.
- New `tonin.toml` sections — open an issue with the shape; we'll discuss against the existing capability docs (`docs/03-*` through `docs/15-*`) before any code.
- Mesh overlays for additional meshes — bundle the templates under `crates/tonin/templates/k8s/mesh/<engine>/` with a matching planner branch.

## What's out of scope (today)

- Re-introducing mesh-delegated concerns (mTLS, retries, circuit-breaking, cross-cluster routing) into the framework crates. See [`docs/01-principles.md`](docs/01-principles.md) and [`docs/13-service-mesh.md`](docs/13-service-mesh.md).
- New languages beyond Rust / Python / TypeScript. See [`docs/15-multi-language.md`](docs/15-multi-language.md) for the existing path.
- Replacing the `tonin.toml` config with a higher-level abstraction (CRD, operator). The CRD path is in the long-term roadmap; raise an issue before doing the work.

## Style

- Rust: rustfmt enforces formatting. Clippy enforces idioms. No further style guide.
- Markdown: no trailing whitespace, sentence-per-line is fine but not required.
- Commits: prefer one logical change per commit; we squash on merge if the PR has noise.

## Reporting security issues

See [SECURITY.md](SECURITY.md). Don't open a regular issue for vulnerabilities.

## License

By contributing, you agree your contributions are licensed under [Apache-2.0](LICENSE) — same as the rest of the project. No separate CLA.
