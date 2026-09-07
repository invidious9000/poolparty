# Consumer integration, dashboard, and CLI

Status: proposed contracts. No consumer has been changed.

## Consumer shapes

These are independently expressed requirements, not copied private implementation.

| Consumer shape | Poolparty requirement | Caller retains |
| --- | --- | --- |
| Custom harness using HTTP/SSE or WebSockets | Native protocol endpoints, explicit intent and session binding, cancellation | Tool execution, transcript, context construction, workspaces |
| Supervisor launching a Messages CLI in stream-json mode | Configure a per-session endpoint/grant without changing the CLI event envelope | CLI process, local resume state, JSON event handling |
| Supervisor using Codex app-server over stdio | Configure provider routing while preserving logical thread identity and continuation | Initialize/thread/turn lifecycle, steer/interrupt, process ownership |
| Usage-only client with its own vault | Account observations and optional cooperative claims using stable account references | Credential custody, direct upstream calls, session pin enforcement |

Selection may begin as an ordered performer policy or an exact model/account pin.
Callers can use dry-run explanations to inspect eligibility, then create a binding
before starting a conversation. Freeze that binding alongside caller session state.
Launching an isolated CLI home must preserve the chosen configuration across resume.

Codex app-server is the caller's local process protocol. Poolparty does not become
an app-server implementation just because it proxies the underlying Responses
transport. Test both native CLI and custom harness paths before claiming parity.

CLI support for headers, base URLs, config reload, compaction, and auxiliary model
calls differs by client version. Native sessions may spawn subagents; the initial
safe policy keeps requests using a binding on its account. Separately allocatable
child sessions require explicit child bindings. The relationship between native
thread IDs and Poolparty logical sessions is documented for a pinned version in
the [Codex spike](../research/codex-spike.md). Do not generalize it to other versions.

## Proposed control API

```text
GET  /api/v1/status
GET  /api/v1/accounts
GET  /api/v1/usage
POST /api/v1/allocations/explain
POST /api/v1/sessions
GET  /api/v1/sessions/{binding_id}
POST /api/v1/sessions/{binding_id}/close
GET  /api/v1/requests/{request_id}
```

All account and usage results are restricted to the caller's authorized inventory.
List responses are bounded and paginated. Explain is read-only despite using POST
for structured intent. Diagnostics should include policy revision, observation
freshness, eligibility exclusions, and dispatch certainty, without prompt bodies
or credential values. Enrollment, configuration, refresh triggers, and account
administration use distinct privileged operations to be specified later.

Generate a versioned OpenAPI description when implementing these routes. JSON
schemas and error codes belong to the application contract; the web UI and CLI
must not reproduce the allocator's decisions independently.

## CLI direction

Start with documented curl/jq recipes as soon as the API exists. Add a thin Rust
`poolparty` executable for auth, consistent formatting, stable exit codes, and
machine-readable output. A shell wrapper can select a URL and credential file,
but should not own token refresh, quota policy, or session state.

Proposed commands, not currently executable:

```text
poolparty login --url https://poolparty.example.com
poolparty status --json
poolparty accounts list --json
poolparty usage --account account-a --json
poolparty explain --provider kimi --model model-a --json
poolparty session create --provider codex --model model-a --json
poolparty session inspect binding_example --json
poolparty request inspect request_example --json
```

Store CLI login state outside the source tree, using a protected OS credential
store where practical. Do not print tokens as part of status/config output. For
headless automation, accept a protected token file. Avoid literal secret command
arguments and verbose HTTP traces that leak authentication. JSON on stdout,
diagnostics on stderr; define stable failures for authentication, unavailable
capacity, missing session, and transport error when the client is implemented.

## Native client configuration

Configuration examples must be validated against a pinned client before they are
published as runnable setup. [Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
documents custom provider base URLs and header configuration, but a working
subscription proxy needs additional behavior verified for that client version.
Do not copy codex-lb's native-auth settings and assume Poolparty grants behave
identically. Model discovery, remote compaction, and WebSocket authentication
must also be tested.

Messages clients should receive a bound base URL and a Poolparty inference grant,
with the chosen Chinese provider/model explicit. The router substitutes its
upstream credential. Do not require users to sign into an actual Anthropic account
to use that protocol surface. The exact environment/header convention depends
on the native client and is part of the compatibility matrix.

## Web UI

The first dashboard should answer: which accounts are usable for this intent,
which windows block them, how fresh is the evidence, which account owns a session,
and why did a request fail? Show active request pressure separately from quotas.
Make unknown and stale visible. Display API-equivalent selection explanations.

Start with account/usage, session lookup, and correlated request diagnostics.
Privileged enrollment and administration can follow the read paths. Never display
raw credentials or conversation content by default. Fleet scheduling and harness
controls remain in consumers.
