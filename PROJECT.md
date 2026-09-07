# Project context

Poolparty is an independent service for sharing provider account capacity across
multiple agent clients. It owns allocation, session affinity, provider credential
lifecycle for routed accounts, and usage observations. Callers own their harnesses,
workspaces, tool execution, conversations, and recovery decisions.

## Settled requirements

- Separate public repository, independent of any one consumer or private estate.
- Target Codex subscriptions, Kimi/GLM coding plans, and DeepSeek PAYG.
  MiniMax is a named future adapter option. Cerebras and OpenRouter PAYG are
  future proxy/routing options, outside initial implementation and live DD.
- Keep subscription windows and PAYG balances/budgets distinct. DeepSeek is an
  explicitly selectable product, not automatic overflow for a bound subscription
  session. Kimi/GLM metered products remain separate future adapters.
- Retain Anthropic Messages compatibility for those providers; omit actual
  Anthropic/Claude integration and its account-specific machinery.
- Preserve provider/account pins across session resume and quota exhaustion.
  Return a useful error; the caller chooses retry later or a new session.
- Support both custom harnesses and native CLI-shaped consumers.
- Keep private deployment and consumer details out of all public artifacts.

## Technology and remaining defaults

The runtime and SQLite choices underpin the experimental daemon. Deployment, UI
and a packaged Rust control client remain proposals; a Python standard-library
control helper is implemented.

| Area | Proposal | Reason |
| --- | --- | --- |
| Runtime | Rust, Tokio, Axum, reqwest/rustls | One long-lived asynchronous service with explicit stream ownership |
| UI | TypeScript/React/Vite, compiled assets served by the daemon | Browser dashboard without a second production application server |
| State | SQLite WAL on persistent storage, one active daemon | Durable bindings and atomic admission without an initial distributed coordinator |
| Control client | Python JSON helper now; packaged Rust CLI remains an option | Stable API commands with environment-based caller grants |
| Hosting | Container, Kubernetes Service, authenticated HTTPS ingress | Internal service discovery plus ordinary remote debugging |
| Platform integration | Existing LGTM and Homepage, through standard telemetry and ingress metadata | Reuse shared observability and portal discovery |
| Upstream reuse | Reference codex-lb; evaluate its Rust egress library selectively | Keep one allocation authority and Poolparty's stricter session contract |

Revisit storage before introducing multiple active replicas. Do not run multiple
independent allocators against the same accounts and call that shared admission.

## Document map

- [Architecture and technology](design/architecture.md)
- [Allocation, sessions, protocols, and errors](design/contracts.md)
- [Providers and usage](design/providers-and-usage.md)
- [Deployment and authentication](design/deployment-and-access.md)
- [Shared observability and portal integration](design/platform-integration.md)
- [Consumer integration and CLI](design/consumers-and-cli.md)
- [Delivery sequence and unresolved decisions](design/delivery-plan.md)
- [Due diligence review and remaining gates](design/dd-review.md)
- [Concurrency and admission](design/admission.md)
- [Reference assessment](research/codex-lb.md)
- [DeepSeek PAYG and prior integration evidence](research/deepseek.md)

## Current implementation state

`poolpartyd --serve` runs the authenticated HTTP/SSE daemon with typed adapter
contracts, durable SQLite bindings, conservative admission/recovery, managed
inventory, periodic maintenance and checks before request admission. Caller grants
are scoped to principals and pools. Account status exposes authorized usage and
credential generations; the control helper supports account queries and session
operations. See [daemon operations](docs/daemon.md).

The credential boundary integrates a 1Password CLI store, serialized Codex refresh
with durable pending fences and generation watermarks, and bounded Codex/GLM/
DeepSeek usage readers. One-shot checks, refreshes and explicit probes remain
available. A loopback-only `--demo` exercises the same ledger and HTTP lifecycle
without provider access. Pinned tooling and isolated fixtures are included.

Native Codex HTTP start, tool execution and resume passed across daemon restart
and credential rotation while retaining the original thread/account binding.
This establishes the bounded native slice described in
[validation evidence](docs/development.md), not complete native parity.

WebSockets/compaction, Kimi account-usage collection, broader provider conformance,
complete PAYG accounting, OIDC/workload identity lifecycle, reconciliation tooling,
dashboard and deployment remain distinct gates. No existing consumer has changed.
Module boundaries are recorded in [core implementation](design/core-implementation.md).
