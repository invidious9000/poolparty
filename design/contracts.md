# Allocation, session, and protocol contracts

Status: required semantics with proposed API spelling.

## Selection intent

Callers can request an exact provider/model with an account chosen from an
authorized pool, or a policy tier with capability requirements. Exact account,
model, and effort pins are hard constraints. Preferences are separately named.
Tier resolution produces a concrete provider, account, model, effort, and policy
revision; it is not an instruction to downgrade an existing session later.

Eligibility combines caller authorization, model capabilities, quota observations,
local admission pressure, account health, and explicit pins. Unknown observations
are explicit inputs to policy. A dry-run explanation is advisory and reserves
nothing; create/admit operations are atomic.

## Session identity

A binding is scoped by an authenticated consumer namespace and logical session
ID, and points to a stable provider/account identity. It also records the resolved
route and any upstream continuation ownership needed by the adapter. Credential
rotation changes a credential generation, not the account binding.

Proposed lifecycle:

1. Create: validate intent and atomically select and persist a binding. Concurrent
   creates with the same idempotency key and payload return the same result;
   conflicting intent returns a conflict.
2. Admit: check the existing binding's eligibility and acquire request capacity.
3. Complete/cancel/fail: record the outcome exactly once. Release capacity on
   confirmed completion/cancellation or proven pre-dispatch failure; retain
   conservative pressure when upstream execution remains uncertain.
4. Resume: retrieve the same binding and repeat admission against its account.
5. Close: retire the session explicitly. Retain a tombstone according to a stated
   retention policy so closed sessions cannot accidentally be recreated.

Resumed sessions never change accounts because of exhaustion, cooldown, provider
failure, model unavailability, or a cheaper candidate. Return an error and retain
the binding. The caller may retry later or create a new logical session. Even a
self-contained transcript does not authorize implicit rebinding.

Binding lookup failure is not new-session creation. A missing, expired, or closed
resume handle fails explicitly. Replica replacement and request-lease expiry do
not expire the binding. Retention must be visible to callers and backed by a
defined export/restore policy.

An explicit different primary model or provider on resume returns a conflict;
callers start a new session. A binding may declare a narrow auxiliary-model set
for native helper operations, all on the same account. Those routes must be
validated explicitly and cannot weaken a hard request model/effort constraint.
Future primary-model changes need a separate explicit operation.

## Carrying the binding through native clients

The canonical control API should allocate an opaque binding handle. A proposed
route-specific base URL encodes the handle without adding fields to model JSON:

```text
POST /api/v1/sessions
GET  /api/v1/sessions/{binding_id}
POST /api/v1/sessions/{binding_id}/close

POST /routes/{binding_id}/v1/messages
POST /routes/{binding_id}/codex/responses
GET  /routes/{binding_id}/codex/responses   (WebSocket upgrade)
POST /routes/{binding_id}/codex/responses/compact
```

A binding ID is not authentication. Every request authorizes access to that
binding and pool. The URL option is useful for a CLI that accepts a base URL but
cannot add a stable per-conversation header. Headers may be an alternate binding
carrier once tested. Do not put bearer tokens in URLs.

An unbound compatibility route needs a validated native identity adapter or an
explicit create operation. Never infer a logical session from a TCP connection,
API key, prompt cache key, or undocumented interpretation of a process ID.
Codex process, session, thread, turn, and response IDs have distinct meanings.
Their mappings are pinned in the [Codex spike](../research/codex-spike.md), with
executed root resume cases and unexecuted child cases clearly distinguished.

Native response IDs, file IDs, and conversation references must agree with the
binding's upstream owner. Reject unknown or conflicting ownership rather than
trying those identifiers on every account. Auxiliary routes such as compaction
must share the same binding. Capability gaps return explicit unsupported errors.

## Exhaustion and failure

Proposed canonical error (synthetic, returned through the control API):

```json
{
  "error": {
    "code": "session_quota_exhausted",
    "message": "The bound account cannot admit this request.",
    "binding_id": "binding_example",
    "binding_preserved": true,
    "blocking_windows": ["weekly"],
    "retry_at": null,
    "request_state": "not_dispatched",
    "request_id": "request_example"
  }
}
```

Use HTTP 429 for known quota exhaustion, with `Retry-After` only when justified.
Return concurrency saturation separately as `session_concurrency_exhausted`;
see [admission](admission.md). Native envelopes and client retry settings are
versioned conformance requirements, not just an HTTP status mapping.
Use a conflict for mismatched session intent, 404/410 for missing/retired bindings
where appropriate, and 503 for temporary service/account unavailability. Distinct
codes cover stale/unknown capacity, reauthentication, and unsupported capability.
Never expose unauthorized accounts through error details.

Model routes render errors in the client's native error envelope. Once an SSE or
WebSocket stream has started, a status code can no longer carry a new failure:
emit a compatible error event or close with a correlated diagnostic. The control
API exposes the canonical record. Do not pretend an interrupted stream completed.

Track dispatch certainty separately: `not_dispatched`, `dispatched`, or `unknown`.
No automatic replay after partial output, tool effects, or ambiguous transmission.
Any future same-account retry policy must prove replay safety, bound attempts, and
preserve the binding. Infrastructure proxies must not retry inference POSTs.

Session-creation idempotency does not deduplicate inference. Custom consumers
should supply a scoped operation ID, distinct from a session ID and a transport
attempt ID. Persist its dispatch outcome and reject conflicting reuse. Native
operation carriers require validation before claiming equivalent deduplication;
equal request bodies do not establish operation identity. When a native operation
cannot be distinguished and its dispatch is uncertain, fail closed for new
inference on that binding until reconciled or explicitly recovered by the caller.
This restriction can temporarily block unrelated child work sharing the binding.
Retry across transports still targets the same uncertainty record, never a new
allocation. See the [Codex spike](../research/codex-spike.md) for a reproduced
native WebSocket-to-HTTP fallback counterexample.

`retry_at` is a best-known eligibility time, not a guarantee of future capacity.
If multiple windows block admission, include all of them. Unknown reset times stay
unknown; never manufacture a retry promise from a default duration.
