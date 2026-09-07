# Existing provider integration evidence

Reviewed 2026-09-06. Read-only source and historical evidence review. No new live
calls, credential reads, builds, or client changes. Exact consumer implementation
references and provenance remain in the private overlay. This document states
independently described integration requirements, not copied source.

The earlier DD established native-client routing against synthetic origins and
current vendor/reference behavior. Existing integrations supply another evidence
layer. A source implementation, a recorded historical live result, and a newly
executed test are different things; retain that distinction in every adapter gate.

| Area | Existing evidence | What remains for Poolparty |
| --- | --- | --- |
| GLM quota reads | Raw-key monitoring request; parser distinguishes five-hour and seven-day token-limit rows from other limit types | Preserve all windows/reset fields and unknown types; revalidate current credit-plan schema and missing-value behavior |
| GLM Messages | Full usage snapshots can appear late in SSE; noncanonical server-search result blocks; prior live tool and summarization checks | Preserve present/absent field semantics, cache dimensions and opaque blocks; validate changed model controls |
| Codex authentication | Refresh near expiry and once after 401, sidecar locking, field-preserving token merge and atomic file replacement; recorded historical live rotation success | One router refresh authority, encrypted durable generations, cross-process lock participation, account invariants and crash/restore behavior |
| Codex quota | Existing backend usage read with bearer and account context; primary/secondary windows and explicit exhaustion | Extensible windows/feature buckets, freshness, reset metadata and quota-owner identity |
| Codex Responses | HTTP/SSE and WebSocket transports, session/cache identity, turn state, previous-response continuation and encrypted items | Distinguish consumer-defined IDs from native CLI lifetimes; enforce Poolparty binding/attempt ownership |
| Codex compaction | Unary remote compaction implementation and recorded live replay of encrypted summary output | Preserve opaque output; validate the selected native client's capability gate and newer streaming compaction separately |
| DeepSeek | Messages and Chat tool-call priors, balance collector, automatic caching | Currency-aware PAYG observations, spend reservations and published account/model admission; see [DeepSeek](deepseek.md) |
| Kimi | Recorded coding-endpoint model-notation and repeated-prefix caching checks | Revalidate changed models and package data; a zero cache-creation counter does not prove no caching |

## Reuse the evidence without inheriting a second harness

The existing transports own conversation buffers, tool continuation, compaction
and retry policy. Poolparty owns transparent routing and admission. Carry forward
the protocol cases and narrowly useful code after a license/privacy review;
do not embed the whole harness or silently rewrite consumer history.

Specific adaptation risks found during review:

- A GLM parser recognizes specific window-number/unit combinations and defaults
  a missing percentage to zero. Retain the schema knowledge; keep absent or
  malformed percentages unknown in Poolparty.
- A Codex usage mapper reduces the response to two windows. That is a useful
  initial observation shape, not evidence that extra windows or reset metadata
  are irrelevant.
- An advisory file lock coordinates only participants using that same lock.
  Existing comments about cooperative native refresh do not prove that every
  current native client participates. Router custody remains a deliberate cutover.
- Atomic rename alone does not establish power-loss durability, crash-after-token-
  rotation recovery, or secret protection throughout temporary-file creation.
  Review those properties before adopting persistence code.
- A client's lack of emitted output is weaker than proof that the provider never
  received a request. Existing status, stream and WebSocket retries need explicit
  integration controls or durable uncertainty fencing.
- Older prose describes thinking as display-only, while newer source preserves
  thinking/signatures. Use the current protocol and implementation evidence;
  never erase required reasoning on a tool continuation.

## Narrowed verification gates

Codex refresh, unary compaction, GLM monitoring and Messages continuation are
existing implemented patterns with recorded prior validation. They are not
unknown feasibility questions. Remaining tests should target custody changes,
schema/version drift and Poolparty's stricter guarantees. Historical success does
not establish today's account entitlement or new router behavior.

The [Codex spike](codex-spike.md) remains authoritative for the newly executed
native CLI fixture and its WebSocket retry counterexample. The
[coding-provider spike](chinese-providers-spike.md) remains the current vendor
matrix. Combine those with these priors when writing original conformance cases;
avoid redoing broad discovery before checking the existing evidence.
