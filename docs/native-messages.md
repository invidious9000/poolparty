# Native Messages vertical check

`spikes/messages/vertical.py` runs an installed Claude Code process against the
Messages API of an already-running Poolparty router. This fixture targets an
explicit `glm_coding` binding. Claude Code is the caller harness; actual Anthropic
provider credentials, subscriptions and OAuth are outside this integration.

The version pin is `2.1.263 (Claude Code)`, carried forward from the earlier
synthetic [native client probe](../spikes/messages/native_cli_probe.py). The
[CLI reference](https://code.claude.com/docs/en/cli-usage) documents bare mode,
explicit tools, session IDs and resume. Another installed version is refused.
This document describes the fixture and its offline coverage; it does not claim a
live provider qualification until a separately authorized run has passed.

## Start and resume

No `--live` means help only, with no router request or native process. For a live
check, supply an existing scoped caller grant through `POOLPARTY_NATIVE_GRANT`.
The fixture never loads provider credentials. Use an empty owner-only directory
outside git checkouts for its isolated native home, settings and transcripts:

```sh
python3 spikes/messages/vertical.py --live --phase start \
  --router https://router.example.com \
  --state-dir /path/to/private/messages-check \
  --pool pool-a --account account-a --model example-model --timeout 300

# Restart the router through its deployment owner's existing converge procedure.

python3 spikes/messages/vertical.py --live --phase resume \
  --router https://router.example.com \
  --state-dir /path/to/private/messages-check --timeout 300
```

`--phase all` is the default and runs both native processes without an intervening
router restart. `--binding binding-a` selects an existing binding instead of
creating one; explicit pool/account/model arguments must agree with it. The
fixture requires `glm_coding`, an open binding and null effort, because the router
does not map Messages effort pins. Thinking is a native request setting, not an
assertion that a provider honored an exact reasoning effort.

Both phases use the actual installed CLI, limited to its `Read` tool. Each phase
reads a different synthetic nonce file and must report its exact contents. The
first process uses an explicit session UUID; the second uses `--resume` with that
same UUID and original native state. Resume restores the same model, router
binding, loopback port and provider route, and compares the entire control-API
binding record. A changed settings file, missing start state or completed resume
is refused before another native turn.

## Caller isolation and observer

This macOS fixture requires `sandbox-exec` and has no unrestricted fallback. The
native process can connect only to loopback. It receives an isolated `HOME`,
`CLAUDE_CONFIG_DIR`, XDG and scratch directories, with an explicit environment
that omits provider API keys, vault service accounts, proxy settings and inherited
native auth state. `--bare`, disabled settings discovery, strict MCP configuration
and a replacement system prompt constrain incidental harness behavior.

The configured `ANTHROPIC_AUTH_TOKEN` is only the Poolparty caller grant. The
native `ANTHROPIC_BASE_URL` names a loopback observer with the original bound
`/routes/<binding>` prefix. Only that observer can reach the configured router,
using HTTPS or a literal loopback HTTP origin. It refuses redirects and inherited
proxies, forwards only the exact `/routes/<binding>/v1/messages` route, and checks
bearer authentication, model and streaming pins. Both upstream and native output model labels must
match the pin exactly; aliases are not silently accepted. An API-key header is refused.
The observer forwards provider version/beta headers but no arbitrary upstream URL
or authorization supplied in the request body.

The observer buffers each response to a 4 MiB limit before handing unchanged SSE
bytes to the native process. It validates all returned tool calls as `Read` of the
exact phase's synthetic file before delivering any tool payload. This prevents a
model-produced call from reading unrelated host files; a prompt alone would not
enforce that boundary. Unknown tool names, wrong paths and reused content indices
fail the check. Requests are bounded to 2 MiB and six upstream calls per phase.

Buffering is deliberate fixture instrumentation. This check establishes native
content, tool execution and resume behavior, including the real upstream SSE
format. It does not qualify end-to-end streaming latency, backpressure or an
uninstrumented native client's behavior.

## Thinking, tools and attempts

The observer reconstructs thinking and signature deltas in memory and requires
the complete ordered reasoning-block sequence to return in subsequent request
history, including multiplicity. Cache-placement annotations are excluded from the reasoning fingerprint; this
fixture does not separately qualify native cache placement. The
resumed process must retain the previously observed reasoning sequence. No
reasoning text is printed or stored separately by the observer; private manifest
fingerprints support the second process's comparison. Native transcripts still
contain conversation material because resume requires them.

A successful phase requires a non-error native result, the exact native session
ID, the nonce read result, at least one successful tool-result roundtrip and
thinking echoed on the wire. Signatures are compared when supplied and counted
in the receipt; an upstream that supplies none is not reported as having passed
signature validation. The observer collects router attempt IDs and checks their
successful settlement against the original binding. Summary output contains
only counts, version and boolean checks.

## Retries and failure handling

The fixture sets documented `CLAUDE_CODE_MAX_RETRIES=0`, along with disabled
nonessential traffic and updater activity. It supplies output/thinking budgets
of 4096/1024 tokens and limits the native turn count to four. See the official
[environment-variable reference](https://code.claude.com/docs/en/env-vars).
These settings do not establish native exactly-once behavior.

The observer adds one unique operation ID per forwarded request and refuses a
repeated body before another upstream dispatch. Any observer or upstream failure
also latches a refusal, including later requests with a different body. Receipts
report `observed_repeated_requests`, `observer_replay_guard_enabled` and
`observer_response_buffering_enabled` explicitly. These protections must not be
mistaken for proof that an uninstrumented native caller never retries. The router
remains the production admission and one-attempt authority; equivalent native
retry coverage and caller operation-ID support are separate acceptance work.

A phase records its in-progress marker before inference. Timeout, native failure,
stream error or protocol mismatch never triggers an automatic fixture retry.
Inspect the retained binding and attempts before deciding on recovery. The
fixture does not clear uncertainty, change accounts, close the binding, restart
services or mint credentials. Each native process has a bounded deadline, default
180 seconds and maximum 600; transport and accepted-socket waits are bounded too.

Raw child output is bounded and excluded from terminal output. `--keep-output`
retains successful native stdout only in the private state directory. The native
history, manifest, nonce files, configured URLs and any retained output must stay
outside public artifacts. Do not publish provider reasoning or transcripts as
evidence.

## Offline validation

```sh
PYTHONDONTWRITEBYTECODE=1 python3 spikes/messages/test_vertical.py
```

The tests use a fake native executable and local synthetic control/stream server.
They exercise actual process boundaries and HTTP forwarding, thinking/signature
history, tool-result roundtrips, same-session resume, private configuration,
replay refusal, changed-history refusal and out-of-scope tool suppression. They
never run installed Claude Code, read native auth caches or call a live provider.
