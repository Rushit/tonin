# Changelog

## [0.7.9](https://github.com/Rushit/tonin/compare/v0.7.8...v0.7.9) (2026-06-25)


### Features

* **proxy:** outbound gRPC proxy sidecar for non-Rust services ([#66](https://github.com/Rushit/tonin/issues/66)) ([3248895](https://github.com/Rushit/tonin/commit/3248895a067d0817b71621ab94569d0b96dee09f))

## [0.7.7](https://github.com/Rushit/tonin/compare/v0.7.6...v0.7.7) (2026-06-24)


### Bug Fixes

* **helm:** apply containerSecurityContext to migrate and mcp containers ([#62](https://github.com/Rushit/tonin/issues/62)) ([cdda13a](https://github.com/Rushit/tonin/commit/cdda13ac74621b7a37895c9c067dcf86688a3552))

## [0.7.6](https://github.com/Rushit/tonin/compare/v0.7.5...v0.7.6) (2026-06-24)


### Bug Fixes

* **ci:** rustfmt assert! expansions and portable commit-msg hook ([#60](https://github.com/Rushit/tonin/issues/60)) ([4b43943](https://github.com/Rushit/tonin/commit/4b439436da87cb99de0a8e756052eea8dd26e694))
* **helm:** guard against grpc:false + empty health path ([#58](https://github.com/Rushit/tonin/issues/58)) ([e42f642](https://github.com/Rushit/tonin/commit/e42f64248b043dc161bd6b661a781c9bf44b9f68))

## [0.7.5](https://github.com/Rushit/tonin/compare/v0.7.4...v0.7.5) (2026-06-24)


### Features

* **scaffold:** Python Makefile template, streaming RPCs, --template-repo support ([#53](https://github.com/Rushit/tonin/issues/53)) ([eb2862e](https://github.com/Rushit/tonin/commit/eb2862e60aef0af6238166df44ae492783f4c597))


### Bug Fixes

* **ci:** broaden idempotent publish check to catch both crates.io error messages ([#55](https://github.com/Rushit/tonin/issues/55)) ([6eb185c](https://github.com/Rushit/tonin/commit/6eb185c58751855b80bcfb8c20f1a6e0f35d4989))

## [0.7.4](https://github.com/Rushit/tonin/compare/v0.7.3...v0.7.4) (2026-06-24)


### Bug Fixes

* **ci:** broaden idempotent publish check to catch both crates.io error messages ([#51](https://github.com/Rushit/tonin/issues/51)) ([0e5b137](https://github.com/Rushit/tonin/commit/0e5b137b9e1260316a06550568e97e8fe9251df4))

## [0.7.3](https://github.com/Rushit/tonin/compare/v0.7.2...v0.7.3) (2026-06-24)


### Bug Fixes

* **ci:** make crates.io publish idempotent (skip already-uploaded versions) ([#49](https://github.com/Rushit/tonin/issues/49)) ([46dee23](https://github.com/Rushit/tonin/commit/46dee237ae2f79e966bd4fbad32ad2f6fabecd80))

## [0.7.2](https://github.com/Rushit/tonin/compare/v0.7.1...v0.7.2) (2026-06-24)


### Bug Fixes

* **ci:** use --auto on merge so Copilot review gate is honoured ([#44](https://github.com/Rushit/tonin/issues/44)) ([7886491](https://github.com/Rushit/tonin/commit/788649135ab9b9caf54ef24ae21447eab2fbf977))

## [0.7.1](https://github.com/Rushit/tonin/compare/v0.7.0...v0.7.1) (2026-06-24)


### Bug Fixes

* **ci:** fix automerge bypass, dispatch publish on release, name Release PRs ([#41](https://github.com/Rushit/tonin/issues/41)) ([e72c0dc](https://github.com/Rushit/tonin/commit/e72c0dc45967a2c762aa540aa07b29dff6d5b730))
* **ci:** fix automerge for separate Release PRs; add workflow_dispatch ([#39](https://github.com/Rushit/tonin/issues/39)) ([d929014](https://github.com/Rushit/tonin/commit/d92901440a8c997446b895e1839b12feb63ca9e8))
* **ci:** remove invalid 'administration:write' permission; robust dispatch ([#42](https://github.com/Rushit/tonin/issues/42)) ([ae8bb29](https://github.com/Rushit/tonin/commit/ae8bb29cc32df7231f923e483ed78db2720b16c3))

## [0.7.0](https://github.com/Rushit/tonin/compare/v0.6.9...v0.7.0) (2026-06-24)


### Features

* add template loading system and service scaffolding ([#3](https://github.com/Rushit/tonin/issues/3)) ([a3c864c](https://github.com/Rushit/tonin/commit/a3c864c2bb4c0bab2d941cb53ea4cc9ab002c733))
* built-in helm, release-please versioning, GHCR cleanup ([#28](https://github.com/Rushit/tonin/issues/28)) ([ce9de59](https://github.com/Rushit/tonin/commit/ce9de599eaf86d5e5f6e9427cb4482e22791db24))
* **client:** request coalescing + optional ttl cache ([#1](https://github.com/Rushit/tonin/issues/1)) ([7ecc5e9](https://github.com/Rushit/tonin/commit/7ecc5e9609006c187b987bd99b3f54c488659411))
* drop tonin k8s, make tonin-helm the only deploy path ([#27](https://github.com/Rushit/tonin/issues/27)) ([b13624f](https://github.com/Rushit/tonin/commit/b13624f54ab6fc74fe295891ef69ad162691f3f6))
* **k8s:** support http services and grpc+http on one service ([#12](https://github.com/Rushit/tonin/issues/12)) ([c4b607d](https://github.com/Rushit/tonin/commit/c4b607d503857343272ba4aea7f24baa13c1ffe0))
* monorepo phase 1 — absorb tonin-helm into the workspace ([#26](https://github.com/Rushit/tonin/issues/26)) ([5563690](https://github.com/Rushit/tonin/commit/55636904765a429362d2e4c4fa9d8534fabd675b))
* native gRPC health probes (runtime + k8s generation) ([#22](https://github.com/Rushit/tonin/issues/22)) ([a7311fe](https://github.com/Rushit/tonin/commit/a7311fe1e35709fb95e8a2feb62817f750d8f392))
* per-environment namespaces and dependencies in tonin.toml ([#15](https://github.com/Rushit/tonin/issues/15)) ([b51683b](https://github.com/Rushit/tonin/commit/b51683bcabd3b2dc93ddce54aab22449af187f4f))
* plugin architecture — tonin-plugin, tonin-sdk, plugin dispatch, version contract (v0.5.0) ([#5](https://github.com/Rushit/tonin/issues/5)) ([88b7014](https://github.com/Rushit/tonin/commit/88b70149a6aeb45bdc9a4586542e9fb88d7638de))
* **plugin:** {env} interpolation in [callers] namespace values ([#21](https://github.com/Rushit/tonin/issues/21)) ([0fc217d](https://github.com/Rushit/tonin/commit/0fc217dd0958ad1e0a1c06a96c5e1febbbb75768))
* **plugin:** native [image] registry and [security] context (0.6.4) ([#23](https://github.com/Rushit/tonin/issues/23)) ([be7d8a6](https://github.com/Rushit/tonin/commit/be7d8a6edcdba88b5feae803f55737c73022f959))
* tonin — opinionated Rust microservice framework for Kubernetes ([c52b0fb](https://github.com/Rushit/tonin/commit/c52b0fb6a9f42a5a91d83df7bb8f3843e58e6d80))
* tonin upgrade + doctor for plugin version sync ([#14](https://github.com/Rushit/tonin/issues/14)) ([62a863a](https://github.com/Rushit/tonin/commit/62a863a439dcb12cc926c19e507f60b28a575f80))


### Bug Fixes

* build macOS Intel binary on macos-15-intel (macos-13 removed) ([2e1ed71](https://github.com/Rushit/tonin/commit/2e1ed7167cedd50f0929f5934a778b53a58b0778))
* bump RECOMMENDED_CLI_MIN to 0.6.0 for per-env depends_on ([#18](https://github.com/Rushit/tonin/issues/18)) ([4ae3823](https://github.com/Rushit/tonin/commit/4ae382389c1c47550b42b6298a31598ea76bc1b3))
* **ci:** fix automerge for separate Release PRs; add workflow_dispatch ([#39](https://github.com/Rushit/tonin/issues/39)) ([d929014](https://github.com/Rushit/tonin/commit/d92901440a8c997446b895e1839b12feb63ca9e8))
* **ci:** install cargo-nextest in release test job ([#4](https://github.com/Rushit/tonin/issues/4)) ([bbdeb99](https://github.com/Rushit/tonin/commit/bbdeb998300bf3a7313b4c1d2ccc1e195e1263bb))
* **ci:** set manifest to 0.6.9 for v0.7.0 release; add tonin-sdk VERSION marker ([#36](https://github.com/Rushit/tonin/issues/36)) ([fafc538](https://github.com/Rushit/tonin/commit/fafc538708e95405ffa190c308db6da6fe56f7f8))
* **ci:** update publish order — tonin-core -&gt; tonin-sdk, add tonin-plugin ([6948bc4](https://github.com/Rushit/tonin/commit/6948bc45038cb3fc32a3fc34c58160b528f7ea8c))
* **ci:** use generic extra-file type for tonin-sdk VERSION and correct path ([#31](https://github.com/Rushit/tonin/issues/31)) ([8f62d92](https://github.com/Rushit/tonin/commit/8f62d92e7edb8c2a398ae4980b3caa8f2262939e))
* **ci:** use GITHUB_TOKEN for release-please (no PAT needed) ([#33](https://github.com/Rushit/tonin/issues/33)) ([4de6d92](https://github.com/Rushit/tonin/commit/4de6d928e949688063cda6f1371f9a586f149609))
* **ci:** use RELEASE_PAT for release-please and fix tonin-sdk VERSION path ([#32](https://github.com/Rushit/tonin/issues/32)) ([62ebb73](https://github.com/Rushit/tonin/commit/62ebb731db9f45034582dab8c6b26fb14ac64c41))
* **install:** add --helm-version flag, fix quick-start snippet ([1b4f3c9](https://github.com/Rushit/tonin/commit/1b4f3c94b7b417928305a7b46f302162c53608d1))
* **install:** self-upgrade without ETXTBSY + Windows PowerShell installer ([#20](https://github.com/Rushit/tonin/issues/20)) ([81db0dd](https://github.com/Rushit/tonin/commit/81db0dd16e9e48a45dcff5a2686ead56e9a23f13))
* **plugin:** use k8s $(VAR) syntax for DB password expansion ([490526d](https://github.com/Rushit/tonin/commit/490526d817ab8251647919e8aa2166af1836e0d3))
* remove auto-tag.yml, exclude tonin-sdk from workspace dep bumps, cap auto-bumps at minor ([#30](https://github.com/Rushit/tonin/issues/30)) ([ae1de15](https://github.com/Rushit/tonin/commit/ae1de1583ce8f21fe9dca8b70b0a6751c9d4ab75))
* **security:** upgrade vite to ^6.4.2, add package-lock.json (CVE-2026-39365) ([#6](https://github.com/Rushit/tonin/issues/6)) ([987ca00](https://github.com/Rushit/tonin/commit/987ca0030496fa34d12d2fa6d7976a8683abbd21))
* set default-run = tonin (cli) to disambiguate from library ([756f9fe](https://github.com/Rushit/tonin/commit/756f9fe8391963ac53b0b44fe8f5ffb1dadfea28))
