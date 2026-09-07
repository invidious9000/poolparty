# Delivery plan

Status: proposed sequence. This repository currently contains design only.

## 1. Resolve compatibility before broad implementation

- Pin reference revisions and native client versions.
- Validate Codex subscription enrollment, refresh ownership, quota collection,
  Responses HTTP/SSE, WebSocket continuation, model discovery, and compaction.
- Determine the actual lifetime of native process/session/thread/turn identifiers
  and prove a binding carrier works with the CLI and app-server consumer shapes.
- Exercise one Messages provider with streamed thinking, tool calls/results,
  interruption, and a second turn. Then build the per-provider conformance matrix.
- Evaluate codex-lb's Rust egress library in isolation for API fit, transitive
  dependencies, release stability, and license/notice obligations.

Result: a small compatibility report and synthetic fixtures, with runtime secrets
and real conversations kept outside the public repo. Live account probes occur
only as explicitly scoped verification work, not as part of this docs bootstrap.

## 2. Implement the domain and control API

Introduce pinned Rust tooling and lockfiles. Implement account references, quota
windows, authorization, intent resolution, durable binding creation, request
admission/settlement, and error codes. Use deterministic synthetic clocks and
fake providers to test the domain before adding credential-bearing transport.

Required acceptance cases:

1. Concurrent creates for one consumer/session bind once; conflicting intents fail.
2. Different consumer namespaces cannot read or reuse each other's binding IDs.
3. Exhaustion on resume errors with the original binding preserved.
4. A fresh session can choose another eligible account without touching the old one.
5. Lease expiry, idle periods, and daemon restarts preserve affinity.
6. Missing/closed resume handles fail; there is no implicit create fallback.
7. Token rotation preserves quota ownership and does not double-count accounts.
8. Stale or missing windows never silently become zero usage or unlimited capacity.
9. Older observations cannot clear newer quota/auth failures.
10. Lost dispatch acknowledgements never cause automatic replay.

## 3. Add protocol routes and provider adapters

Implement native Responses and Messages incrementally, sharing the same admission
authority. Add HTTP/SSE and the tested WebSocket path. Validate cancellation,
backpressure, partial errors, unknown event extensions, thinking/tool content,
cache directives, and account-bound continuation references. Reject unsupported
routes explicitly. Add Chat Completions only when a selected consumer needs it.

## 4. Add operational surfaces

Ship the small web dashboard and CLI over the control API. Add redacted metrics,
request correlation, readiness, graceful drain, backup/restore, and credential
enrollment/rotation paths. Keep provider usage reads bounded and distinguish
request totals from quota observations.

## 5. Package, deploy, and integrate consumers separately

Publish a reproducible container and portable deployment contract. In the separate
private overlay, create the workload, storage, service, ingress, auth registrations,
and secret mappings. Verify immutable image provenance and preview before activation.
Test normal workstation HTTPS access and internal service access, including negative
authorization and streaming tests. Integration changes belong in each consumer's
own repository after the service contract is proven.

## Open decisions

| Decision | Current proposal | Evidence needed |
| --- | --- | --- |
| Extend codex-lb or build independently | Independent Rust application; selective reference/reuse | Transport spike and maintenance-cost assessment |
| Exact first provider products | Codex subscriptions plus Kimi/GLM/MiniMax | Actual account products, entitlement, endpoint and quota behavior |
| Binding carrier for each native client | Explicit control allocation and bound base URL | Native CLI/app-server tests, including subagents and resume |
| Dashboard tooling | TypeScript/React/Vite embedded assets | First UI slice; no SSR requirement identified |
| Credential persistence backend | Secret-store interface with one refresh authority | Vault write/rotation semantics and restore procedure |
| Machine authentication | Scoped service grants; bearer compatibility where needed | Estate identity-provider and native-client capabilities |
| State topology | One daemon and SQLite WAL | Actual load/recovery needs before any HA expansion |
| Binding retention | Explicit close plus tombstones; no lease-based eviction | Longest expected resume lifetime and restore requirements |
| Direct-consumer claims | Optional cooperative admission | A concrete consumer that needs direct credential custody |

No deployment, subscription purchase, credential migration, or consumer change is
implied by accepting these design documents.
