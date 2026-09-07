# Developing the core

The current executable is a loopback-only synthetic demo. It exercises the real
SQLite ledger, caller authentication, control API and stream lifecycle without
contacting any provider. The HTTP provider transport is a library component tested
against local origins; the executable does not load real provider credentials.

## Build and verify

Use the exact toolchain in `rust-toolchain.toml` and committed `Cargo.lock`:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
python3 spikes/core/smoke.py
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

## Implemented boundaries

- Caller/pool isolation, immutable session intent, explicit close and tombstones.
- Shared quota-owner concurrency across credential aliases and protocols.
- Explicit usage freshness and unknown-capacity policy; stale success cannot
  clear a newer exhaustion/auth failure. Window and balance evidence are preserved.
- Durable request claims and credential generation references. Startup recovers
  never-dispatched reservations and fences possibly sent work as uncertain.
- No automatic transport retries, redirects, provider fallback or rebinding.
- Terminal SSE bytes carry completion evidence; the ledger settles before those
  bytes reach the caller. Partial EOF and disconnect preserve uncertain pressure.
- Bounded bodies/SSE frames, redacted secret types and response-header allowlists.

Without operation IDs, only one active inference may use a binding. An active
native attempt also blocks a new identified operation on that binding. Distinct
identified operations may run concurrently under the shared quota-owner cap.
Uncertain attempts block all inference on their binding. Retrying a completed
native operation without an ID cannot be distinguished from a new turn; native
retry configuration/conformance remains a gate, not an exactly-once guarantee.

## Remaining acceptance gates

Production credential persistence/refresh, quota collectors, supported-client
live conformance, OIDC and workload grant lifecycle, model-specific limits, PAYG
spend reservations, usage-driven selection scoring, explicit reconciliation/admin
recovery, backup/restore tooling and graceful-drain deadlines remain incomplete.
Uncertain work deliberately retains capacity; the initial HTTP surface offers
inspection but no operator override to release it without evidence.

WebSockets, compaction, remote continuation/file references, helper-model changes,
remote model discovery and nonstreaming inference return unsupported. No dashboard,
production deployment or consumer integration is included. Cerebras/OpenRouter
and MiniMax remain future options.
