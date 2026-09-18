# Developing the core

The executable provides an authenticated persistent daemon, a loopback-only
synthetic demo and one-shot credential maintenance modes. `--serve` maintains
enrolled credentials and usage, then routes requests through durable binding and
admission. See [daemon operations](daemon.md) for configuration and the control
helper. The demo exercises the same ledger and HTTP lifecycle without providers.
Explicit `--check`, `--probe` and `--refresh` modes operate once and exit. Read
[credential maintenance](credential-maintenance.md) before using real inventory;
both daemon startup and `--check` can rotate an expiring Codex credential.

## Build and verify

Use the exact toolchain in `rust-toolchain.toml` and committed `Cargo.lock`:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
python3 spikes/core/smoke.py
python3 -m unittest discover -s spikes/codex -p 'test_*.py'
python3 -m unittest discover -s spikes/messages -p 'test_*.py'
python3 -m unittest discover -s spikes/messages_responses -p 'test_*.py'
```

The smoke check starts its own daemon on loopback, generates a temporary caller
grant, verifies streamed completion and operation deduplication, restarts against
the same temporary database, resumes the original binding, then closes it. It
removes its own process/state on exit. Tests use isolated temporary directories;
they never read an operator's auth cache or vault.

## Run the demo

```sh
export POOLPARTY_DEMO_TOKEN="$(openssl rand -hex 32)"
cargo run --locked --bin poolpartyd -- --demo
```

The default address is `127.0.0.1:8080`; `POOLPARTY_LISTEN` can select another
loopback socket. `POOLPARTY_STATE_DIR` defaults to `./state` and must be private to
the process owner. Keep its lock file and SQLite/WAL files together. The daemon
holds an exclusive directory lock before opening/recovering the ledger.

All API requests except `GET /healthz` require `Authorization: Bearer` with the
demo token. Create a binding at `POST /api/v1/sessions`:

```json
{
  "session": "example-session",
  "pool": "demo",
  "product": "codex_subscription",
  "model": "synthetic-model",
  "account": null,
  "effort": null
}
```

Use its `id` in `POST /routes/{id}/codex/responses` with a body such as
`{"model":"synthetic-model","input":"hello","stream":true}`. Set
`x-poolparty-operation-id` to a new stable ID for each logical inference. A repeated
operation never dispatches again; its error identifies the existing attempt for
`GET /api/v1/attempts/{id}`. Session creation and inference have separate identity.
Resume uses `GET /api/v1/sessions/{id}` and the same bound route.

### Drop-in surface

Native clients need one static base URL and a caller grant, nothing else:
`POST /v1/responses` for Codex and `POST /v1/messages` for Messages clients.
The router derives the session from the client's own thread identity: the
`thread-id` header, then the `session-id` header, then for Messages the
`metadata.user_id` field (its trailing `session_<id>` segment when present).
The first request on a thread creates an affinity-only binding from the request
body and the caller's authorized pools; every later request on that thread,
including a resumed one, lands on the same binding and account. Model, effort
and any other body field such as a service tier pass through per request. A
model the bound account does not serve is refused on that binding as
`no_eligible_account`, never moved to another account. Successful and failed
responses after binding carry `x-poolparty-binding` and `x-poolparty-account`
headers. When several authorized pools serve the model,
the caller sets `x-poolparty-pool`; `x-poolparty-account` pins one account.
Requests without any session identity are rejected before dispatch. A thread
carrying `x-codex-parent-thread-id` binds to its parent's account and pool when
that parent is already bound, so one task stays on one account. The explicit
session API below remains for callers that choose or inspect bindings.

On the explicit session API, `model` and `effort` are optional hard pins: when
present, every request on that binding must match them; when absent, requests
pass through as on the drop-in surface. Session creation selects the first
eligible account in the pool by account ID, or the pinned `account` alone. When nothing is eligible, the error carries an
`exclusions` array with each pool member's `account`, `code` and `message`. A
pinned account reports its own code. Without a pin, when every intent-matching
member is blocked for the same reason, that reason is the error code, so a pool
whose members are all quota-exhausted returns `session_quota_exhausted` with
HTTP 429. Mixed or purely structural reasons (disabled, wrong product or model)
return `no_eligible_account`. A pin to an account outside the pool returns
`no_eligible_account` without exclusions. Accounts outside the requested pool
are never listed.

## Implemented boundaries

- Caller/pool isolation, immutable session intent, explicit close and tombstones.
- Persistent authenticated service, authorized account/usage inspection and an
  environment-authenticated Python control helper.
- Initial and periodic managed inventory synchronization, checks before request
  admission, credential-alias fencing and bounded graceful shutdown.
- Shared quota-owner concurrency across credential aliases and protocols.
- Explicit usage freshness and unknown-capacity policy; stale success cannot
  clear a newer exhaustion/auth failure. Window and balance evidence are preserved.
- Durable request claims and credential generation references. Startup recovers
  never-dispatched reservations and fences possibly sent work as uncertain.
- No automatic transport retries, redirects, provider fallback or rebinding.
- Terminal SSE bytes carry completion evidence; the ledger settles before those
  bytes reach the caller. A disconnected client does not stop the drain: the
  upstream stream is read to its terminal event and settled on that evidence.
  Partial EOF preserves uncertain pressure, and admin grants can list and
  resolve uncertain attempts with a recorded rationale.
- Bounded bodies/SSE frames, redacted secret types and response-header allowlists.
- Codex-only handling for successful SSE responses missing `Content-Type`:
  downstream metadata becomes `text/event-stream`, while strict frame/terminal
  validation remains required. Explicitly incompatible MIME types still fail.
- Explicit 1Password field mappings and versioned writeback under one writer;
  serialized Codex refresh with a durable pending fence and SQLite generation
  watermark. An unresolved refresh requires manual reconciliation.
- Bounded Codex/GLM usage and DeepSeek balance readers with exact decimal evidence,
  finite freshness and explicit unsupported collection for Kimi. Auxiliary feature
  exhaustion does not become account-wide generation exhaustion.

Without operation IDs, only one active inference may use a binding. An active
native attempt also blocks a new identified operation on that binding. Distinct
identified operations may run concurrently under the shared quota-owner cap.
Uncertain attempts block all inference on their binding. Retrying a completed
native operation without an ID cannot be distinguished from a new turn; native
retry configuration/conformance remains a gate, not an exactly-once guarantee.

## Remaining acceptance gates

Broader native-client/provider conformance, Kimi account-usage collection, OIDC and
workload grant lifecycle, model-specific limits, PAYG spend reservations,
usage-driven selection scoring, explicit reconciliation/admin recovery and
backup/restore tooling remain incomplete. The qualified native HTTP slice below
does not establish these independent capabilities.
Uncertain work deliberately retains capacity; the initial HTTP surface offers
inspection but no operator override to release it without evidence. Maintenance
pending markers likewise remain fenced after restart; a newer vault generation
alone does not clear them.

WebSockets, compaction, remote continuation/file references, helper-model changes,
remote model discovery and nonstreaming inference return unsupported. No dashboard,
production deployment or consumer integration is included. Cerebras/OpenRouter
and MiniMax remain future options.

## Container packaging

The pinned amd64 Dockerfile and runtime filesystem contract are described in
[container operations](container.md). The daemon exposes a separate local readiness
check and disables accounts removed from enrollment without deleting their bindings.
Container builds and deployed ingress checks are separate from the local test suite.

## Current validation evidence

The automated gates cover the Rust workspace, both native Python fixtures,
formatting, workspace check/build, clippy with warnings denied and the synthetic
process restart smoke check. These automated fixtures use isolated state and synthetic
origins; they do not contact real providers or a vault.

Separate explicitly authorized live validation exercised installed 1Password CLI
reads/writeback, Codex refresh and usage, GLM usage and a completed GLM Messages
stream. Native Codex 0.153.4 then completed start and resume phases through the
authenticated daemon: eight successful upstream attempts, four before a daemon
restart and credential rotation and four afterward. Both phases executed native
shell tools. The native thread, Poolparty binding and bound account stayed fixed
while the credential generation advanced. See the
[native validation fixture](native-codex.md) for its assertions and opt-in controls.

The [native Messages fixture](native-messages.md) provides bounded GLM start/tool
and resume checks with synthetic offline coverage. Its existence does not
establish successful live native acceptance. Direct GLM SSE completion and usage
collection have been observed separately.

The live Codex backend omitted `Content-Type` on valid SSE; the narrow adapter
handling above enabled the stream without relaxing completion checks. Previously
uncertain attempts stayed fenced and were never silently replayed or released.
Final daemon checks also exercised account queries and binding inspection through
the control helper, rejection of unauthenticated access and clean shutdown.

Private account identities, vault references, prompts, responses, logs and state
remain outside this repository. This bounded validation does not establish full
native parity, real crash-during-refresh recovery, backup restoration or deployed
service acceptance.
