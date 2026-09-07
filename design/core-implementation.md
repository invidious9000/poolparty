# First core implementation

Status: first implementation exists. Provider conformance and deployment remain
gates; see [development](../docs/development.md) for commands and limits.

One Rust crate contains domain types and ports, SQLite persistence, an application
service, HTTP handlers and provider transports. `domain` and `ports` are the shared
contract. Changes to those modules require integration review before dependent
modules adopt them.

## Module ownership

- `domain.rs`, `ports.rs`: identities, immutable intent, quota ownership,
  dispatch certainty and object-safe async boundaries.
- `storage.rs`: `SqliteLedger::open(path)` implementing `Ledger`. Database work
  runs outside async executor threads; short transactions never span network I/O.
- `runtime.rs`: `Router`, coordinating admission, secret loading, durable dispatch,
  streaming, settlement and cancellation. `Router::new` takes four `Arc` values:
  `Ledger`, `Transport`, `CredentialStore`, and `Clock` implementations.
- `http.rs`: `app(Arc<Router>, BearerGrants) -> axum::Router`; native bound routes
  and create/inspect/close/attempt-inspect control routes. Grants own authenticated
  principals and pool authorization; bodies cannot supply a trusted principal.
- `providers.rs`: one-attempt HTTP/SSE adapters and synthetic transport. Endpoint
  and credential configuration comes from trusted operator configuration.
- `config.rs`, `main.rs`: local synthetic demo wiring and exclusive startup.

The application method is `Router::execute(&self, principal: &Principal,
binding: BindingId, operation: Option<OperationId>, protocol: Protocol,
body: Bytes) -> domain::Result<RoutedResponse>`. A routed response contains
`status: u16`, `headers: Vec<(String, String)>`, `attempt_id: AttemptId`, and
`stream: Pin<Box<dyn Stream<Item = domain::Result<Bytes>> + Send>>`.
`Router::ledger()` returns `&Arc<dyn Ledger>` and `Router::now()` returns the
current timestamp for authenticated control handlers.

## First acceptance boundary

Create an authenticated binding, reserve one shared quota-owner slot, record
dispatch intent, stream unchanged protocol bytes, settle after a native terminal
event, restart and resume the same account. Concurrency and credential aliases
share counters. Credential generations are recorded on attempts, not frozen into
the session binding. Missing, closed and foreign bindings never create new ones.

Storage owns atomic create/admit and transition checks. Duplicate operation IDs
are scoped to a binding and return conflict/already-exists; this implementation
does not cache or replay inference responses. Uncertain work blocks its binding
and retains quota-owner pressure. Startup under exclusive ownership releases
never-dispatched reservations and converts dispatching/streaming work to uncertain.
Requests without operation IDs also fence active work on that binding, covering
the interval before asynchronous disconnect cleanup persists uncertainty. Distinct
identified operations can share a binding concurrently. Duplicate/fenced errors
include the original attempt ID for inspection after lost response headers.

Native terminal bytes and completion evidence travel as one transport event. The
application commits settlement before yielding that final chunk. A completed
rejection body can instead provide a separate completion event at EOF. Bare socket
EOF never establishes completion of an inference stream.

Core admission initially enforces local account-owner concurrency and explicit
available/exhausted/unknown/auth observations. Window and decimal-balance types
preserve evidence; automatic PAYG reservation, model-specific concurrency and
provider limit derivation remain later acceptance gates. Do not advertise complete
PAYG accounting from the existence of a balance DTO.

HTTP/SSE only. Unsupported compaction, WebSockets, remote continuation references,
model-discovery and helper-model changes fail explicitly until their ownership and
compatibility gates are implemented. Full request bodies remain transient; only a
hash is retained for operation conflict detection. Streaming buffers are bounded.

## Verification and delegation

Implementers own their assigned module and tests. Do not modify shared manifests,
domain types, ports, or another module without coordinating the exact change.
Use synthetic fixtures and isolated temporary state; no live provider credentials,
vault references, private source excerpts, or deployment data belong in this repo.

The integrator runs pinned formatting, workspace checks/tests and clippy, reviews
cross-module behavior and public disclosure, and commits/pushes the integrated
result. Persistence tests cover races, restart uncertainty, identity and quota
ownership. HTTP tests exercise authenticated requests against a fake transport.
Provider tests use local HTTP origins and count upstream attempts.
