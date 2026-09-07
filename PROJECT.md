# Project context

Poolparty is an independent service for sharing provider account capacity across
multiple agent clients. It owns allocation, session affinity, provider credential
lifecycle for routed accounts, and usage observations. Callers own their harnesses,
workspaces, tool execution, conversations, and recovery decisions.

## Settled requirements

- Separate public repository, independent of any one consumer or private estate.
- Target Codex subscriptions, Kimi/GLM coding plans, and DeepSeek PAYG.
  MiniMax is a named future adapter option.
- Keep subscription windows and PAYG balances/budgets distinct. DeepSeek is an
  explicitly selectable product, not automatic overflow for a bound subscription
  session. Kimi/GLM metered products remain separate future adapters.
- Retain Anthropic Messages compatibility for those providers; omit actual
  Anthropic/Claude integration and its account-specific machinery.
- Preserve provider/account pins across session resume and quota exhaustion.
  Return a useful error; the caller chooses retry later or a new session.
- Support both custom harnesses and native CLI-shaped consumers.
- Keep private deployment and consumer details out of all public artifacts.

## Proposed defaults

These choices are the initial design recommendation, not implemented behavior.

| Area | Proposal | Reason |
| --- | --- | --- |
| Runtime | Rust, Tokio, Axum, reqwest/rustls | One long-lived asynchronous service with explicit stream ownership |
| UI | TypeScript/React/Vite, compiled assets served by the daemon | Browser dashboard without a second production application server |
| State | SQLite WAL on persistent storage, one active daemon | Durable bindings and atomic admission without an initial distributed coordinator |
| Control client | Thin Rust `poolparty` CLI; curl examples first | Stable JSON output and shared API contracts without policy in shell scripts |
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

Design documents and isolated compatibility fixtures exist. Native-client probes
use synthetic loopback origins and no provider credentials. There is no daemon,
live model call, deployed service, or changed consumer. Validation and remaining
gates are recorded in the DD reports. Application implementation will introduce
pinned toolchains, lockfiles, domain tests, and build gates.
