# Rust egress reuse due diligence

Reviewed: 2026-09-06. Public upstream revision:
[`5ad638b6a4c9c094bcc8866b1d7487173fe3b54e`](https://github.com/Soju06/codex-lb/tree/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e).
Status: source/API spike complete; transport implementation and runtime validation
remain open. This refines [the initial reference assessment](codex-lb.md).

## Recommendation

**Reference-only for the current `codex-lb-egress` crate. Build a narrow
Poolparty-owned transport boundary; selectively adapt the WebSocket connector and
compatibility profile if the first synthetic compatibility tests justify it.**
Do not adopt the whole application or deploy another allocator.

The decisive API finding is that the pinned library publicly exports only
`RequestError` and `run_stdio()`. Its HTTP client pool, HTTP execution function,
WebSocket connector, connection type, and configuration are private. `run_stdio`
hardcodes process stdin/stdout, owns a multiplexing loop, and installs the global
rustls crypto provider. The worker is a separate executable, but the library does
not yet expose a usable typed in-process egress API. Its documentation describes a
future reuse boundary more completely than the current exports implement it.
See [library exports][lib] and [runtime][runtime].

The valuable work is concentrated in WebSocket deflate/proxy support, liveness
handling, TLS profile choices, and synthetic failure tests. Ordinary HTTP
forwarding is small enough that an original reqwest implementation with explicit
Poolparty constraints is preferable to inheriting the stdio runtime. This is an
engineering recommendation, not a claim that upstream transport is defective for
its existing Python-owned deployment.

## Method and evidence limits

Read the pinned Rust manifests, lockfile, all four egress source files, protocol
types, worker entry point, Rust transport tests, and relevant Python test cases.
Confirmed the checkout revision and inspected source-level API, dependency, and
control-flow boundaries. Checked Cargo's patch inheritance rules and the pinned
reqwest documentation. No Rust compilation, dependency-resolution probe,
benchmark, packet capture, provider traffic, account enrollment, credentials, or
service mutations occurred. No upstream code or test payload was copied here.

The findings below are observed source behavior unless explicitly identified as
an inference or proposed change. They do not establish native Codex wire parity,
current provider acceptance, cancellation latency under load, or release readiness.

## Actual API and ownership boundary

| Surface | Observed implementation | Consequence for Poolparty |
| --- | --- | --- |
| Library entry | `run_stdio() -> Result<(), RequestError>`; networking modules are private | A Git dependency does not provide `send_http` or `connect_ws`; a normal wrapper cannot call the private internals |
| Output | `Arc<Mutex<BufWriter<tokio::io::Stdout>>>`; events serialized to JSON, newline, flush | Cannot inject a per-request byte stream; all requests share output serialization |
| Input | Process stdin lines parsed as `NativeCommand` after hello negotiation | IPC framing remains inside the library; no supplied reader or typed request receiver |
| Request/event types | `codex-lb-protocol` with string URLs, string headers, base64 HTTP bodies/chunks and binary WS data | Transport code depends directly on wire DTOs; avoid those copies and IPC names in a new in-process API |
| Policy | One URL/proxy per request; no account selection or durable state in these crates | Good allocation separation; reuse need not inherit upstream's softer affinity policy |
| Startup | Installs the aws-lc global provider and errors if installation fails | A daemon that already installed a provider cannot safely call it as a reusable initialization step |
| Build | Worker depends on egress, which depends on protocol; no Python or frontend dependency in Cargo graph | A Rust-only build boundary exists even though a typed application API does not |

Sources: [exports][lib], [runtime][runtime], [wire types][protocol],
[workspace manifest][manifest], [worker entry point][worker].

Do not pass a pool or list of accounts to the transport. Poolparty must supply one
admitted attempt containing the already bound account's credential generation and
approved endpoint. Transport must return facts about that attempt. Failure,
exhaustion, reconnection, credential rotation, and process restart never authorize
a new account selection or creation of missing resume state.

## Cancellation, backpressure, and replay certainty

HTTP tasks select between execution and a oneshot cancellation signal. WS
cancellation aborts its owning task. EOF drains active requests and waits for
tasks. WS commands use a channel with capacity 32; a full channel returns a setup
error through `try_send`. Request IDs are validated against active entries, but
there is no maximum active request count, client-cache cardinality, stdin line
length, or HTTP body size enforced by this runtime. These limits must be imposed
by its caller or an adaptation. [Runtime][runtime]

Output writes and flushes hold one global mutex. HTTP waits for each emitted
chunk, providing backpressure through that shared sink. This is not independent
per-request backpressure. A blocked sink can delay other requests and cancellation
events; control-loop paths also await emission. WS sends and event emission await
inside select branches, so their waits prevent that task from polling its pong
deadline. These are source-derived interference risks, not measured stalls.
Use bounded per-attempt byte queues, bounded concurrency and body limits, an
out-of-band cancellation path, and a deadline covering blocked writes. Slow
telemetry must not own the data stream. [HTTP][http], [WS][ws], [runtime][runtime]

`WebsocketSent` follows a successful sink `send`. It indicates local send
completion, not upstream receipt, inference admission, or execution. Send failure,
lost acknowledgement, peer close, and cancellation after sending can leave
dispatch uncertain. `retryable_same_contract` is a coarse transport flag, not a
durable replay proof. HTTP errors report phases but expose no precise request
write progress. Cancelling local I/O does not establish that remote computation
stopped. [WS][ws], [HTTP][http], [wire types][protocol]

The Rust code does not implement an account retry loop. However, its reqwest
builder leaves library retry and redirect defaults in force. reqwest 0.12.28
documents protocol-NACK retries and a default limit of ten redirects. This must
be reviewed separately from application replay; do not infer that every NACK retry
is ambiguous or that absence of a local loop proves a single wire attempt.
Poolparty should explicitly disable automatic inference retries and redirects in
the first slice, then test the actual lower-level behavior. [Pinned reqwest
builder documentation](https://docs.rs/reqwest/0.12.28/reqwest/struct.ClientBuilder.html#method.retry)

Maintain `not_dispatched`, `dispatched`, and `unknown` in Poolparty attempt state.
After crossing a possible-write boundary, default to `unknown` unless stronger
evidence exists. Persist the session binding independently of attempt settlement.
No automatic replay or resumed-account movement follows a transport error flag.

## WebSocket, TLS, and pool behavior

| Concern | Observed behavior | Required adaptation or proof |
| --- | --- | --- |
| Compression | Uses the pinned OpenAI forks and enables default `permessage-deflate`; limits received message size | Verify extension negotiation, fragmented compressed messages, expansion limits, binary/text fidelity, and refusal behavior with synthetic peers |
| Pings | Answers peer ping; optional periodic sequenced ping; only matching pong clears watchdog; interval/timeout cannot be zero | Supply both finite values; test mismatched pong and stalled writers. Optional fields alone do not guarantee a watchdog is active |
| WS close | Local close emits sent and close after sending; peer close/EOF produce close events | Treat this as transport close, not model completion; verify close handshake and cancellation races |
| TLS | rustls/aws-lc; native roots; WS reloads native certificates and builds config per connection, with no client certificate | Inject application-owned TLS config and provider choice; test invalid roots, host mismatch, custom CA needs, and root rotation |
| TLS profile | Manifest enables `prefer-post-quantum`; upstream describes Codex-family profile matching | Retain only intentionally supported profile choices; source flags are not proof of measured wire parity in Poolparty |
| Proxies | HTTP uses reqwest proxy support; WS connects directly or via parsed proxy, with an extra TLS hop for HTTPS proxy | Allow only configured endpoints; verify CONNECT/SOCKS DNS/auth semantics separately. HTTP without explicit proxy can inherit system proxy defaults; WS `None` is direct |
| HTTP pool | Clients keyed by exact proxy URL, optional connect timeout, and response-decode boolean; cloned handles reuse the same client | Bound key space and cache lifetime; credentials in proxy URLs remain in keys. Do not let caller-controlled timeout values create unbounded clients |
| Pool isolation | Key does not include account or origin; auth headers are per request; idle connections capped at 8 per host, idle timeout 120 seconds | Sharing a connector is not account admission. Decide if connection-bound identities, mTLS, or differing TLS policy require additional partitioning |
| HTTP/2 | Sets initial stream window 2 MiB, connection window 5 MiB, max frame and header list 16 KiB | Treat as an upstream compatibility profile to validate, not universal multi-provider tuning |

Sources: [WS implementation][ws], [HTTP implementation][http],
[runtime client-key construction][runtime], [manifest][manifest],
[upstream profile notes][readme]. The pinned reqwest documentation distinguishes
explicit proxy configuration from its automatic system-proxy behavior.
[ClientBuilder](https://docs.rs/reqwest/0.12.28/reqwest/struct.ClientBuilder.html#method.no_proxy)

## Transparency, observation, and secret handling

HTTP accepts arbitrary parseable methods and URLs, an optional complete base64
body, and repeated string header pairs. It does not interpret model JSON. Body
bytes are preserved after base64 decoding, but uploads are fully buffered and
there is no streaming request-body API. Responses expose status, HTTP version,
headers, chunks, and end. SSE chunks are arbitrary byte chunks, not parsed events.
There is no trailer surface. Header names pass through `HeaderMap` normalization;
response header values use lossy UTF-8 conversion, so this is not a byte-exact
header proxy. [HTTP][http], [wire types][protocol]

The presence of any `Accept-Encoding` request header enables response decoding
for all four compiled codecs. Without that header, decoding is disabled and no
encoding advertisement is added by those codecs. Decoded responses change the
entity encoding and related headers; the Rust gzip test expressly verifies this.
Preserve semantic content deliberately and test downstream header consistency.
Do not claim encoded-byte transparency. [HTTP][http], [runtime][runtime],
[HTTP integration tests][http-tests]

WS forwards opaque text/binary messages and arbitrary supplied handshake headers,
but reconstructs the handshake, negotiates compression, and terminates ping/pong
locally. It relays messages, not original compressed frames or fragmentation.
Neither transport restricts upstream origin, strips caller credentials, filters
hop-by-hop headers, or exposes an application policy callback. Poolparty must
approve endpoints, own auth-header construction, restrict reserved WS headers,
and reject redirects before adding credentials. [WS][ws], [HTTP][http]

There is no tracing/metrics observer API or usage parser. A stdio consumer can
observe events only after serialization. An adapted typed response stream should
tee bounded metadata to Poolparty's parser without changing payload ordering.
Count bytes and timings, parse bounded SSE/WS usage observations, and record
attempt certainty separately from billing or authoritative quota. Never retain
prompt/body fragments merely to make dashboard charts.

Error event messages are mostly fixed, redacted phrases. Invalid proxy parsing
substitutes a redacted placeholder, and certificate classification follows typed
causes. Those are useful patterns to reproduce. They are not blanket redaction:
WS handshake failures include full response headers/body, close reasons are
forwarded, HTTP heads/chunks carry upstream data, and the internal WS error
`Display` delegates to underlying errors. Protocol structs store credentials and
proxy URLs in ordinary strings. Add sensitive header marking, sanitized structured
telemetry and error views, bounded handshake error bodies, and tests for secret
sentinels in headers, URLs, proxy passwords, and upstream error echoes. Preserve
native client error semantics without copying private content into logs.
[WS][ws], [HTTP][http], [wire types][protocol]

## Options and concrete change footprint

Counts are from this immutable checkout and include tests/comments. They describe
review scope, not estimates of final code size or implementation time.

| Approach | Concrete work | Assessment |
| --- | --- | --- |
| Pin current crate as a Git dependency | Add egress dependency and repeat both root WS fork patches; resolve and commit Poolparty's lockfile; still only gain `run_stdio` | Reject for in-process use. Pinning cannot expose private functions, inject I/O, or remove global startup side effects |
| Adapted upstream fork exposing an API | Change `lib.rs`, `http.rs`, `websocket.rs`, `runtime.rs`, manifests, and tests; create typed requests/streams, replace `Output`/`emit`, separate stdio adapter, inject TLS/client policy | Feasible bounded fork, but crosses all 4 egress source files (1,323 lines) plus IPC types (166 lines) and worker tests (458 lines). Useful if upstream collaboration becomes desirable |
| Selective extraction | Review HTTP (201 lines) and WS (540 lines); carry only justified helpers/profile, rewrite ownership and errors, add notices and fork pins | Best code-reuse option after tests establish value. WS currently imports HTTP certificate classification and runtime output, so copying one file unchanged is insufficient |
| Own narrow transport using maintained dependencies | Original reqwest adapter, application-owned WS task and bounded streams, reproduce useful profile/behavior tests; choose the pinned WS forks only if compatibility requires them | Recommended initial implementation direction. Own the small API and policy seam while retaining upstream as evidence |
| Whole application fork | Retain Python control plane, database/account model, frontend, Rust workspace and packaging; revise allocation/resume semantics and add multi-provider adapters | Disproportionate scope; creates a second product migration and greater policy divergence than the transport reuse problem |

No option should import account selection, soft affinity, transparent failover,
health-driven rebinding, or replay policy from the surrounding application.

## Dependency, version, license, and release risks

The workspace is version `0.1.0`, `publish = false`, Rust edition 2024, minimum
Rust 1.96 with toolchain 1.96.0. It exactly pins reqwest 0.12.28, Tokio 1.49.0,
rustls 0.23.36, aws-lc-rs 1.16.2 and several HTTP/TLS dependencies. Other ranges
remain compatible-version requirements. Its lockfile fixes upstream application
resolution, not Poolparty's future resolution. There is no stable typed egress
API or separate published-crate compatibility promise evidenced here.
[Manifest][manifest], [lockfile][lock], [toolchain][toolchain]

The workspace patches `tokio-tungstenite` to
`0e5b2d73aa18dd9f0a50ee9ff199d5aef7594186` and `tungstenite` to
`4fffad30fe373adbdcffab9545e9e9bf4f2fc19f`. The source uses their deflate and proxy
surfaces. Cargo ignores dependency-owned patch tables, so a consuming workspace
must repeat the applicable patches or use explicit compatible Git dependencies.
This is a concrete integration trap, not an optional optimization.
[Cargo patch rules](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html#the-patch-section),
[manifest][manifest]

Keep fork revisions immutable, audit changes before updating, and test resolution
alongside Axum's WebSocket dependencies to detect duplicate versions or unwanted
feature unification. Distinct tungstenite versions can coexist but expose
different Rust message types; the proposed API should hide those types. Existing
upstream pins and `deny.toml` are useful evidence of policy, not a completed
Poolparty vulnerability/license audit or an upstream maintenance commitment.
Native aws-lc build requirements and target compatibility need verification in
the first actual Rust CI build.

Upstream codex-lb is MIT, copyright 2025 Soju06. Any copied or adapted substantial
portion needs its copyright and permission notice plus exact revision provenance.
The pinned tokio-tungstenite fork declares MIT; pinned tungstenite declares
MIT OR Apache-2.0. Record the selected license path and audit all transitive
notices before distribution. No copied code is present in this spike, so these
are adoption requirements rather than a claim that third-party notices are
already installed. [Upstream license][license],
[tokio-tungstenite manifest](https://github.com/openai-oss-forks/tokio-tungstenite/blob/0e5b2d73aa18dd9f0a50ee9ff199d5aef7594186/Cargo.toml),
[tungstenite manifest](https://github.com/openai-oss-forks/tungstenite-rs/blob/4fffad30fe373adbdcffab9545e9e9bf4f2fc19f/Cargo.toml)

## First implementation seam and acceptance tests

Proposed transport boundary, not implemented API: submit one `PreparedAttempt`
with approved endpoint, sensitive headers, body stream, deadlines, connector
profile, and cancellation handle. Return typed response metadata plus an owned
bounded byte stream, or a WS handle with bounded send/receive channels and local
send receipts. The domain layer supplies an attempt ID and records certainty;
the transport never receives allocation authority. Dropping/cancelling a handle
settles only its attempt, with no account change or inferred remote cancellation.

Reproduce these upstream ideas with original synthetic fixtures rather than
copying their source or captured provider traffic:

| Upstream evidence | Original Poolparty acceptance case |
| --- | --- |
| `codex_websocket_fork_negotiates_compression_and_relays_frames` in [runtime tests][runtime] | Local peers negotiate deflate and a subprotocol; exchange original text/binary payloads, fragmentation, and oversized decompressed messages |
| `missing_pong_emits_liveness_timeout` in [WS worker tests][ws-tests] | Silent and mismatched-pong peers produce a bounded liveness failure; matching pong keeps the connection alive |
| `explicit_cancel_aborts_websocket_and_emits_one_cancelled_event` in [WS worker tests][ws-tests] | Cancel a single live attempt, assert peer teardown and exactly-once local settlement while a second attempt continues |
| gzip relay and missing `Accept-Encoding` in [HTTP worker tests][http-tests] | Synthetic compressed origin verifies body/header consistency and no unintended encoding advertisement |
| pool partition and certificate classification in [runtime tests][runtime] | Reuse equivalent connector profiles; isolate decode/proxy/TLS changes; classify a synthetic untrusted certificate without string matching |
| helper death during pending send and full queue during close in [Python adapter tests][python-tests] | Close/cancel with saturated bounded queues; kill an isolated transport task after possible send; surface unknown certainty without replay |
| no fallback after submission/output in [fallback tests][fallback-tests] | Simulate headers written, partial body, local WS receipt, and partial output; assert one upstream attempt and unchanged durable binding |

Add Poolparty-specific tests absent from those selected references: timeout under
blocked write, independent backpressure for concurrent streams, HTTP cancellation,
queue/body/cache byte limits, non-UTF-8 and duplicate header handling, forbidden
redirects, caller-auth stripping, redaction sentinels, credential refresh on the
same bound account, restart/resume lookup, and exhaustion preserving that binding.
Count wire attempts at a local synthetic origin to verify hidden client retries.
Provider acceptance and native CLI compatibility remain separate opt-in gates.

The next decision point is a small original HTTP adapter plus WS fixture harness,
not a full fork. Adopt selected upstream code only if those fixtures show a
specific compatibility benefit that outweighs its extra dependency and maintenance
surface. Revisit this assessment if upstream publishes a typed transport API.

[lib]: https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/crates/codex-lb-egress/src/lib.rs
[runtime]: https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/crates/codex-lb-egress/src/runtime.rs
[http]: https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/crates/codex-lb-egress/src/http.rs
[ws]: https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/crates/codex-lb-egress/src/websocket.rs
[protocol]: https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/crates/codex-lb-protocol/src/lib.rs
[manifest]: https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/Cargo.toml
[lock]: https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/Cargo.lock
[toolchain]: https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/rust-toolchain.toml
[worker]: https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/crates/codex-lb-egress-worker/src/main.rs
[readme]: https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/crates/codex-lb-egress/README.md
[http-tests]: https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/crates/codex-lb-egress-worker/tests/http_protocol.rs
[ws-tests]: https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/crates/codex-lb-egress-worker/tests/websocket_protocol.rs
[python-tests]: https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/tests/unit/test_native_egress.py
[fallback-tests]: https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/tests/unit/test_websocket_transport_fallback.py
[license]: https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/LICENSE
