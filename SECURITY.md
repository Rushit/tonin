# Security Policy

## Reporting a vulnerability

**Do not open a public issue describing the vulnerability.** Public details turn into an exploit window before a fix can ship.

Instead:

1. Open a GitHub issue at https://github.com/Rushit/tonin/issues with the title `[security]` and a one-line note like "potential security issue — happy to share details privately." That's all. No PoC, no affected versions, no description of the bug class.
2. A maintainer will reach out to you privately (via the GitHub email associated with the reporter, or by responding in the issue with a private contact channel) within a few business days.
3. Share the details privately once contact is established.

This flow is intentional: it gives us a public record that a report exists (so reporters can prove timely disclosure) while keeping the exploitable detail off the public internet until a fix is available.

## What counts as a security issue

Anything that lets an attacker do something the framework's design says they shouldn't be able to do. Some examples:

- A capability impl that leaks credentials into logs or traces
- The auth layer accepting tokens it should reject (signature, audience, issuer, expiry)
- The MCP sidecar exposing tools that should require auth without enforcing it
- A scaffolded service shipping with insecure defaults that aren't documented
- Privilege escalation in the rendered Kubernetes manifests (overly broad RBAC, host mounts, etc.)
- Code in `crates/tonin/templates/` that lands an exploitable pattern in every scaffolded service

If you're unsure whether something qualifies, open the `[security]` issue anyway — we'd rather triage one extra report than miss a real one.

## What's out of scope

- Vulnerabilities in transitive dependencies (tonic, tokio, sqlx, etc.). Report those upstream; we'll bump dep versions as patches land.
- Issues that require attacker-controlled `Cargo.toml` / `tonin.toml` files on the target system. Local file trust is a precondition.
- Denial of service via configuration the user controls (e.g. setting `replicas = 0`).
- Vulnerabilities in clusters, meshes, or backing services (k8s, Cilium, Postgres, Redis). Report upstream.

## Disclosure

Once a fix is merged and released, we'll add a note to the GitHub Release describing the issue at a level of detail that helps users assess their exposure. Reporters are credited in the release notes unless they ask to remain anonymous.

We aim to ship security fixes as point releases on the current minor version (e.g. 0.1.1, 0.1.2) rather than rolling them into the next feature release.
