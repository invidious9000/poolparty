# Codex subscription compatibility spike

Status: bounded due diligence, 2026-09-06. No provider credentials or live model
calls were used. This is evidence for an implementation decision, not a claim
that Poolparty or a supported subscription gateway already exists.

## Decision

Proceed with the durable binding/control domain and an isolated HTTP/SSE adapter.
The native CLI and app-server can carry an opaque binding in a provider base URL
and authenticate using a Poolparty bearer. Keep native WebSocket routing disabled
for the first strict integration until ambiguous-dispatch protection is proven.
The native client demonstrably resubmits an interrupted WebSocket turn over HTTP
even with both retry settings zero. A transparent proxy is insufficient.

Do not claim full native parity yet: subagent lifecycle coverage, remote
compaction capability negotiation, command-auth refresh, account enrollment,
quota accuracy, and live backend entitlement remain gates. None justifies
migrating an exhausted session. Bindings survive all these failures; callers
choose to wait or explicitly create a new session.

## Evidence and reproducibility

Evidence labels used below:

- **Vendor contract**: current official documentation for a named client surface.
  This does not make an internal subscription endpoint a public platform API.
- **Pinned source observation**: behavior read in public native Codex source;
  upstream tests are source evidence unless explicitly executed here.
- **Executed fixture**: the released native binary talking to a synthetic local
  server, with outbound network access restricted to loopback.
- **Unknown/live check**: not established by those sources or this fixture.
- **Recommendation**: Poolparty design inference, not observed implementation.

Native source: [OpenAI Codex release rust-v0.153.4][release], commit
`3d2ee51ca2d5db578f328aa75e20aa22c0197c9a`, released 2026-09-04. This was the latest
stable release returned by the public GitHub release API during the review.
The downloaded `codex-aarch64-apple-darwin` reports `codex-cli 0.153.4`.
The native checkout was read without building it. Source changes on the main
branch were not used to infer this release's behavior.

| Artifact | SHA-256 |
| --- | --- |
| Release `codex-aarch64-apple-darwin.tar.gz` | `8cf911ea676523bfb2121ec561848d2aba564890ad536db4d8a3353f2b9850b1` |
| Extracted binary | `b973d440acac501fd2594a43e7ca9ce41e0a65b9dfb28d0d7a7837c99e1261e3` |

The [fixture](../spikes/codex/probe.py) uses Python's standard library and a
temporary `HOME`, `CODEX_HOME`, working directory, and minimal environment. It
uses macOS `sandbox-exec` to deny non-loopback outbound connections. It neither
reads native credentials nor alters installed binaries/configuration. Run:

```sh
python3 spikes/codex/probe.py /path/to/codex-0.153.4
```

**Executed fixture** results:

| Case | Result |
| --- | --- |
| CLI HTTP/SSE, then `exec resume` in a new process | Same binding URL, `session-id`, and `thread-id`; synthetic grant arrives as bearer |
| Two app-server threads, alternating turns | Per-thread base URL overrides remain isolated |
| App-server restart, explicit `thread/resume` with restored URL | Same native thread/session identity; authenticated WebSocket works |
| HTTP 429 with `error.type=usage_limit_reached` | Failed turn after one request |
| HTTP 401 with env bearer | Failed turn after one request with retries disabled |
| HTTP SSE with assistant output but no `response.completed` | Failed turn after one request with retries disabled |
| WebSocket accepted request, disconnect before completion | Native client resubmits the same nonempty turn ID over HTTP and completes |

The final observed run recorded 13 local requests, including WebSocket prewarm.
App-server control RPC uses stdio in this fixture; WebSocket results refer to its
outbound model transport, not the separate app-server control WebSocket listener.
The fixture filters prewarm's empty turn ID when testing the resubmission. It
does not assert a fixed global request count because prewarming is asynchronous.
It does not implement allocation, durable storage, quota ownership, OAuth,
backpressure, tool execution, or real upstream continuation semantics.

## Enrollment, account ownership, and refresh

**Vendor contract**: Codex supports ChatGPT sign-in, including device-code login
when enabled by the account/workspace. It refreshes managed tokens automatically
and stores auth in a file or credential store. These are human enrollment and
client credential-lifecycle features. [Authentication documentation][auth-doc]

**Pinned source observation**: device enrollment requests a user code at
`/api/accounts/deviceauth/usercode`, polls `/api/accounts/deviceauth/token`, and
exchanges the resulting authorization code/PKCE material at `/oauth/token`.
The resulting ID/access/refresh tokens are persisted. The built-in OAuth client
identity is native Codex's, not a registered Poolparty client. Refresh uses
`https://auth.openai.com/oauth/token`; returned replacement refresh tokens are
persisted. Reused, expired, and invalidated refresh tokens have distinct failure
classification. The auth manager's semaphore protects that manager, not separate
processes or independent copies of a credential cache. It also guards reload
against account changes. [Device flow][device-source], [auth manager][auth-source]

**Vendor contract**: app-server offers managed ChatGPT auth and experimental
`chatgptAuthTokens`; in the latter the host supplies access token/account ID and
answers refresh requests. That is useful for a host already owning provider
credentials, but does not turn a Poolparty caller grant into ChatGPT auth.
[App-server authentication][app-doc]

**Recommendation**: one refresh authority per routed account, with serialized
refresh, compare-and-swap credential generations, account identity validation,
and durable encrypted persistence before further use. Enrollment/import is a
privileged transfer of custody, not ongoing synchronization with a caller's
active auth cache. Keep caller grants separate. Expired or uncertain refresh
state returns reauthentication-required while retaining existing bindings.

**Unknown/live check**: whether the intended account products and deployment are
authorized/supported for a separate pooling service, how Poolparty should obtain
its enrollment authority, exact workspace/user quota ownership, safe recovery
after a refresh succeeds upstream but persistence fails, and actual rotation/
revocation behavior. Native source availability does not answer these questions.

## Quota observations and concurrency

**Pinned source observation**: the native backend client reads
`GET https://chatgpt.com/backend-api/wham/usage`; its alternative non-backend path
is `/api/codex/usage`. It sends provider auth and selected account context.
The payload can contain account/user IDs, a main rate-limit bucket, additional
metered-feature buckets, credits, spend controls, and reset-credit information.
This is an observed internal backend read, not a documented public OpenAI
Platform quota API. [Usage request][usage-source], [payload mapping][backend-source]

**Vendor contract**: app-server exposes `account/rateLimits/read` and update
notifications, including a legacy single bucket and a multi-bucket view, used
percent, window duration, and Unix reset timestamps. [App-server limits][app-doc]

**Recommendation**: preserve raw optional-field meaning in the adapter. Keep the
last successful observation separate from collection failure; attach source,
account/product, observed time, freshness, limit ID, window, and units. The native
mapper can produce a bucket with absent windows; that is not zero utilization.
Additional buckets may have a feature name rather than a model name. Do not turn
credits, percent-used, local token totals, or reset availability into equivalent
capacity. Never automatically redeem a reset credit as part of observation.

For published per-model concurrency ceilings, store provenance, product/model
scope, effective date, and the operator's potentially lower admission ceiling.
No authoritative subscription per-model concurrent-request number was established
in this review. Public Platform model pages' RPM/TPM tables describe API usage
tiers, not subscription concurrent-request limits. Native subagent thread-count
settings are harness limits, not provider quotas. Treat a missing subscription
ceiling as unknown, then use explicit conservative local policy. A request claim
must count the actual model and account, including auxiliary work; a WebSocket
connection is not itself an active inference request.

**Unknown/live check**: quota freshness/lag, header-versus-poll ordering, percent
denominators, shared versus per-user workspace limits, per-model ceilings,
out-of-band consumption, and whether quota read authorization matches inference.
The fixture cannot establish any of them.

## Provider grants, models, and auxiliary routes

**Vendor contract**: custom providers support a base URL, Responses protocol,
env bearer, static/env headers, WebSocket capability, and command-backed bearer
auth. Command auth supports timeout and refresh interval. Use user-level config
or explicit overrides; project-local provider routing config is ignored.
[Configuration reference][config-doc]

**Executed fixture**: `requires_openai_auth=false`, `env_key=POOLPARTY_GRANT`, and
`name=Poolparty` deliver the synthetic grant to the bound HTTP and WebSocket
routes. No provider token reaches the caller. Binding possession is not auth.

**Recommendation**: inject a scoped caller grant with an env key initially. For
renewable grants, spike command auth independently. Do not combine it with env
auth or native ChatGPT auth. The auth docs say `requires_openai_auth=true` ignores
`env_key`, while this pin's bearer resolver checks the env key first; avoid this
combination rather than relying on conflicting precedence descriptions.
[Auth resolution][provider-auth-source], [authentication docs][auth-doc]

**Pinned source observation**: command-backed auth installs a provider-scoped
external auth manager; a 401 can trigger refresh and retry independently of
ordinary transport retry counts. An env-only grant has no grant-refresh command.
Do not let upstream provider 401 mean caller reauthentication: Poolparty should
handle bound-account refresh itself or return the account-health error through
its control API. Caller-grant 401 belongs before upstream dispatch. A refresh
retry is safe only when that first request is known not to have dispatched.
[Auth resolution][provider-auth-source], [401 recovery][client-source]

**Pinned source observation**: native `/models` discovery uses
`client_version` and returns Codex `ModelInfo` metadata, not merely OpenAI
Platform's `{data:[{id:...}]}` model listing. The models manager fetches remotely
when auth uses the Codex backend or the provider has command auth. Env-bearer-only
custom providers do not automatically fetch; they can use bundled/cache/static
catalog data. Thus the successful env-bearer fixture does not prove discovery or
entitlement. Use a versioned explicit catalog or prove command-auth discovery,
with account-specific entitlement checked separately. [Models manager][models-source],
[models endpoint][models-endpoint-source]

**Pinned source observation**: `/responses/compact` appends to the same provider
base URL and carries session/thread/turn headers. However, the configured
provider capability grants remote compaction V2 to OpenAI/Azure-shaped providers;
a provider named Poolparty reports unsupported, causing native Codex to select
local summarization through ordinary inference. That is a behavior change from
native remote compaction and blocks a full-parity claim. V2 can compact through streaming
Responses using `compaction_trigger` items, so unary compaction support alone is
insufficient. Do not rename Poolparty to OpenAI merely to enable name-dependent
behavior. [Provider capabilities][provider-source], [client][client-source],
[remote compaction V2][compact-source]

Primary model/effort pins need an explicit auxiliary policy. Subagents can have
role-specific model/effort settings; this pin's bounded role overrides preserve
the parent's provider configuration. Compaction around a model switch may first
use the previous model and then retry with the current model, including after a
usage-limit failure. Internal helper work can use different models. An exact
primary model must not silently become permission for arbitrary auxiliary models.
Either admit only that model and return unsupported on incompatible auxiliary
work, or require an explicit allowed auxiliary model/effort set, all on the same
provider/account binding. Record request purpose and account/model concurrency
separately. [Agent spawn/resume][spawn-source], [bounded role overrides][role-source],
[compaction fallback][fallback-source]

## Identity lifetimes and carrier recommendation

**Pinned source observation**, with fixture confirmation where marked:

| Identity | Lifetime and implication |
| --- | --- |
| OS process / app-server connection | Can host multiple threads; changes on restart. Never the durable binding key. |
| Native root `session_id` | This release uses the root thread ID, restoring persisted metadata on resume. Cross-process CLI/app-server resume confirmed. Do not inherit older assumptions that it is a fresh process UUID. |
| Native `thread_id` | Durable conversation ID. Resume restores it; a root fork creates a different thread. Independent app-server threads have different IDs. |
| Subagent session/thread | A child has its own thread ID but inherits the root session ID; persisted child session lineage is restored on resume. Legacy child metadata has compatibility handling. Source/tests only, not executed here. |
| Turn ID | Identifies one native turn, carried in request metadata. Many inference/tool continuations can share it. Prewarm can carry an empty ID. |
| Response ID | Identifies an upstream response, not the logical conversation. Native WebSocket continuation may send `previous_response_id` plus incremental input. Preserve its bound account ownership. |
| `x-codex-turn-state` | Server sticky-routing state held for one turn; reset for the next. It is not a durable session handle. |
| `prompt_cache_key` | Normally derived from root session ID, but overridable and different for internal helpers. Cache affinity is not authority. |
| Installation/window/request IDs | Telemetry, compaction-window, and request-related identities have separate lifetimes; none replaces the explicit binding. |

Sources: [session initialization][session-source], [resume tests][session-tests],
[metadata][metadata-source], [session headers][headers-source], [client][client-source].

**Recommendation**: allocate the binding explicitly, then configure
`https://example.com/routes/binding-a/codex`. Native path composition preserves
that prefix for Responses HTTP, WebSocket, and unary compaction. Keep a durable
caller mapping from native root/thread IDs to binding and resolved provider
configuration. Restore that mapping before each resume; missing mapping fails.
The fixture deliberately restores the URL on app-server restart and does not
claim native rollout state persists the entire provider definition. A fixed
global URL is unsafe for independently allocated threads in one process.

Children may share the parent's binding family if explicitly defined that way,
with per-child request claims. Native role overrides preserve provider authority
at this pin; separately requested provider changes and persisted-child provider
resolution still need a binding conflict check. Inheritance is not a substitute
for Poolparty authorization.
The root session header is useful evidence for a pinned native adapter, but is
client-supplied and not an authenticated account selector. Do not auto-create on
an unknown header or use native subagent resume to reconstruct missing ownership.

## Exhaustion and no replay

**Pinned source observation**:

| Failure shape | Native behavior at this pin |
| --- | --- |
| HTTP 429, `error.type=usage_limit_reached` | Maps to non-retryable `UsageLimitReached`; caller gets a failed turn. |
| Other HTTP 429 | Maps to non-retryable `RetryLimit`; loses the useful quota-specific explanation. |
| SSE `response.failed`, `error.code=rate_limit_exceeded` | Retryable stream error, distinct from HTTP subscription exhaustion. |
| Unknown SSE failure code | Generally retryable; an invented Poolparty code is not automatically terminal. |
| HTTP 401 | Can invoke auth recovery; otherwise unexpected-status handling is retryable according to stream policy. |
| HTTP 5xx / transport interruption | Retry policies apply; defaults are not strict no-replay. |
| Connection failure | A default-enabled unbounded connection-retry feature can wait/retry outside the configured finite stream count. |
| WebSocket stream failure | Can switch to HTTP and retry even when the stream retry count is zero. |

Sources: [HTTP mapping][error-source], [SSE mapping][sse-source],
[retryability][protocol-error-source], [retry/fallback loop][retry-source].

**Recommendation** for the first native HTTP integration:

```toml
[model_providers.poolparty]
name = "Poolparty"
base_url = "https://example.com/routes/binding-a/codex"
env_key = "POOLPARTY_GRANT"
requires_openai_auth = false
wire_api = "responses"
supports_websockets = false
request_max_retries = 0
stream_max_retries = 0

[features]
unbounded_connection_retries = false
```

Perform admission before starting the upstream request or committing an SSE/WS
success response. Return native HTTP 429 usage-limit shape for known exhausted
capacity, while preserving the canonical binding/error record in Poolparty.
Do not forge quota exhaustion to represent unrelated errors. Already-started
streams need a separately tested terminal envelope or correlated failure; neither
this note nor the fixture proves a general terminal SSE/WS extension.

Poolparty itself can prohibit upstream retries and persist uncertain attempts.
To prevent a native resubmission from becoming a second upstream dispatch, a
pinned native adapter can latch ambiguity for `(binding, thread, turn)` and
reject subsequent requests for that turn until explicit reconciliation. It must
handle concurrent requests, process/daemon restart, empty-ID prewarm, tool
continuations, and auxiliary requests. A turn ID is not a request idempotency key:
successful turns may legitimately require multiple calls. A caller could also
resubmit as a new turn, so a binding-level recovery-required latch or stronger
attempt protocol may be necessary. These are proposed protections, not proven
fixture behavior. Unsupported WebSocket integration is a smaller safe first cut;
strict WebSocket can become viable after the protection is demonstrated.

## Exact next checks

1. Confirm account products, pooling authorization/enrollment authority, stable
   quota owner, and any published per-model concurrent-request limits.
2. Prove refresh rotation with one authority, including crash-after-refresh and
   restore; account identity must not change and old credentials must not race.
3. Compare quota polling with response headers and controlled exhaustion/reset,
   preserving unknown and stale states. Do not infer balances from fixture tokens.
4. Execute child spawn, child resume across native process restart, role overrides,
   and root fork with the chosen binding-family policy. No silent new allocation.
5. Prove command-grant refresh and native model discovery; settle the remote
   compaction capability gap and auxiliary-model set without identity spoofing.
6. Exercise stream cancellation, partial tool/reasoning events, HTTP status/SSE
   errors, and router restart with durable ambiguous-attempt protection. Then
   retest WebSocket continuation, prewarm, fallback, and duplicate admission.

The community codex-lb reference remains pinned separately at
`5ad638b6a4c9c094bcc8866b1d7487173fe3b54e`; see its
[assessment](codex-lb.md). Its affinity aliases and recovery machinery do not
establish native identity lifetimes or Poolparty's strict session contract.
No upstream implementation code was copied into the fixture.

[release]: https://github.com/openai/codex/releases/tag/rust-v0.153.4
[auth-doc]: https://learn.chatgpt.com/docs/auth
[app-doc]: https://learn.chatgpt.com/docs/app-server
[config-doc]: https://learn.chatgpt.com/docs/config-file/config-reference
[device-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/login/src/device_code_auth.rs
[auth-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/login/src/auth/manager.rs
[usage-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/backend-client/src/client/rate_limit_resets.rs
[backend-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/backend-client/src/client.rs
[provider-auth-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/model-provider/src/auth.rs
[provider-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/model-provider/src/provider.rs
[models-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/models-manager/src/manager.rs
[models-endpoint-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/model-provider/src/models_endpoint.rs
[compact-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/compact_remote_v2.rs
[spawn-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/agent/control/spawn.rs
[role-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/agent/role.rs
[fallback-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/compact_model_fallback.rs
[session-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/session/session.rs
[session-tests]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/session/tests.rs
[metadata-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/responses_metadata.rs
[headers-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/codex-api/src/requests/headers.rs
[client-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/client.rs
[error-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/codex-api/src/api_bridge.rs
[sse-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/codex-api/src/sse/responses.rs
[protocol-error-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/protocol/src/error.rs
[retry-source]: https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/responses_retry.rs
