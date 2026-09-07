# Developing the core

The executable provides a loopback-only synthetic demo and separate one-shot
credential maintenance modes. The demo exercises the real SQLite ledger, caller
authentication, control API and stream lifecycle without contacting any provider.
Explicit `--check`, `--probe` and `--refresh` modes can load operator-enrolled
credentials and contact configured HTTPS endpoints. They do not start a production
listener. See [credential maintenance](credential-maintenance.md) before using
these modes; even `--check` can rotate an expiring Codex credential.

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

Persistent production listener wiring, automatic refresh/collection scheduling,
supported-client live conformance, Kimi account-usage collection, OIDC and workload
grant lifecycle, model-specific limits, PAYG spend reservations, usage-driven
selection scoring, explicit reconciliation/admin recovery, backup/restore tooling
and graceful-drain deadlines remain incomplete.
Uncertain work deliberately retains capacity; the initial HTTP surface offers
inspection but no operator override to release it without evidence. Maintenance
pending markers likewise remain fenced after restart; a newer vault generation
alone does not clear them.

WebSockets, compaction, remote continuation/file references, helper-model changes,
remote model discovery and nonstreaming inference return unsupported. No dashboard,
production deployment or consumer integration is included. Cerebras/OpenRouter
and MiniMax remain future options.

## Current validation evidence

The credential/usage slice passes 91 synthetic tests, formatting, workspace check,
clippy with warnings denied, and the process restart smoke test. Automated tests
never contact providers or a real vault.

An explicit operator run separately verified installed 1Password CLI reads,
Codex/GLM usage collection and one completed GLM Messages stream. A Codex token
exchange reached the vault; a server-owned editor-metadata comparison caused the
initial writeback result to fail closed. The comparison now allows that audit
field to change, with synthetic regression coverage and an installed-CLI dry run.
The stored bundle was manually reconciled against enrollment and a successful
usage read; no second refresh exchange was issued. This does not establish an
uninterrupted live refresh success path with the corrected adapter.

Codex inference conformance remains open: one older-model request was explicitly
rejected, and a separate current-model request returned HTTP 200 but no accepted
stream bytes. Its attempt remains uncertain with capacity retained. Neither was
automatically replayed. Private diagnostics and operator state remain outside this
repository. This bounded validation is not full native-client, refresh crash,
backup recovery or production deployment acceptance.
