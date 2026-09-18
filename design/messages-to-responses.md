# Messages clients on Codex Responses

Status: bounded local experiment implemented, with live Codex text/tool and resume
acceptance. Daemon integration and broader native qualification remain proposals.
See the [experiment instructions](../docs/messages-to-responses.md).

## Scope and placement

Claude Code can act as the caller harness for a Codex subscription session. This
uses the Anthropic Messages wire format without introducing an Anthropic provider,
subscription credential, OAuth integration or account machinery.

The experiment places an explicit translator before the existing bound Responses
route:

```text
Claude Code
  POST /routes/{binding}/v1/messages
Local translation fixture
  POST /routes/{same_binding}/codex/responses
Poolparty admission, credential lifecycle and one-attempt transport
  Codex subscription backend
```

Poolparty remains the only account allocator. The bridge accepts an existing open
`codex_subscription` binding and a scoped Poolparty caller grant. It cannot create
bindings, change accounts, refresh credentials or dispatch directly to a provider.
The daemon's protocol/product checks remain unchanged.

## Translation contract

The fixture supports a parent conversation and native subagents with text,
client-defined function tools and text tool results. Each has a separate history
on the same binding. The request and returned model labels must exactly
match the binding. An explicit native effort must match the bound effort; numeric
thinking budgets and disabled thinking are rejected. Adaptive thinking uses the
bound Codex effort, which is a declared translation policy, not a claim that the
two providers have equivalent reasoning controls.

| Messages representation | Responses representation |
| --- | --- |
| Ordered text system blocks | `instructions`, separated by two newlines |
| User text | Ordered `input_text` content |
| Text system messages within the conversation | Ordered `input_text` with the `system` role preserved |
| Custom tool with `input_schema` | Function tool with unchanged parameters and explicit `strict: false` |
| Tool use ID | Exact `function_call.call_id`, separate from output item ID |
| Tool result | `function_call_output`, with a JSON envelope retaining text blocks and `is_error` |
| Reasoning summary | Thinking block, with a fixture reference in its signature field |
| Completed text or tool output | Messages content blocks and `end_turn` or `tool_use` |

The fixture never fabricates an Anthropic signature. Its `poolparty-responses-v1:`
reference identifies an exact Responses reasoning item in the private journal.
It is not a provider signature or authorization token. The bridge checks every
returned reasoning block against that journal. Original output items, including
encrypted reasoning, tool arguments, IDs and assistant phase metadata, are replayed
in their original Responses order with `store: false`. Unknown content, citations,
remote references and unsupported built-in tools fail explicitly.

Claude Code can regroup assistant blocks and user tool results on resume. The
fixture checks assistant blocks, user text and system text in their respective original order,
including duplicates. Consumed tool results must match by their unique call IDs;
duplicate or missing results are rejected. Only new user content or pending tool
results may be added, alongside appended system guidance. System guidance alone
does not authorize another inference. It retains the original Responses ordering instead of
reconstructing that ordering from regrouped native history. Compacted, truncated
or edited histories are rejected. The only accepted context-edit setting is the
explicit keep-all-thinking setting.

## Explicit differences from a native provider

This development experiment requires an explicit choice to use the Codex backend's
output budget instead of Messages `max_tokens`. It does not claim to enforce that
native ceiling. It also offers an explicit automatic-cache profile, which removes
Messages cache placement markers and supplies a stable conversation-specific cache key.
Without that profile, cache annotations are rejected. Provider cached-input usage
is reported separately from uncached input; no Anthropic cache-write quantity or
authoritative account quota is invented. These differences are not defaults for a
future production adapter.

Response buffering is bounded and intentional. The fixture requires a successful
Responses terminal event and supported output before emitting Messages SSE.
Complete `response.output_item.done` events supply output when the subscription
backend's terminal array is empty. Every started item must finish with the same
ID and contiguous output indices; a populated terminal array must agree.
This qualifies content translation, not native stream timing, cancellation or
backpressure. Failed/incomplete output never becomes a successful `message_stop`.

## State and dispatch

Native state and the private bridge journal are both required for resume. The
journal pins router origin, full binding record, translation policy and listening
port. Restart refuses a changed binding or missing state. A process lock protects
the operator entry point against concurrent writers. Overlapping requests queue
for serialized dispatch on the same binding, with bounded bodies, responses,
queue size, journal size and aggregate attempt count.

The installed Claude Code `2.1.267` probe sends `x-claude-code-session-id` for both
parent and child requests, and `x-claude-code-agent-id` for a child. The fixture
pins the native session header and uses the agent header to select a separate
durable child history, instructions and pending tool IDs in the same journal.
Identical prompts from different agents remain separate. These caller-supplied
headers do not grant access or select a different account; the scoped bearer and
bound route still authorize every request. Missing child continuation state fails
history validation. It is never reconstructed from a matching system prompt.

Each conversation retains its original Responses items and stable cache key.
System instructions and history remain strict within that conversation. Parent
and child attempts share the binding's uncertainty fence, so a failed child
dispatch cannot be bypassed by sending the next request under another agent ID.
The header behavior is an installed-client observation, not a general Messages
protocol guarantee. Forked histories and native context compaction remain unsupported.

Before forwarding, the fixture durably records a pending operation ID and supplies
that ID to Poolparty. It records the router attempt ID when available. Terminal
translation and journal state are written before downstream delivery; a pending
marker remains until delivery finishes. Crashes, ambiguous failures, upstream
rejections and failed delivery require inspection. The fixture neither retries
nor clears that fence automatically. A repeated native request is rejected by
history validation, not treated as a new operation. This conservative instrumentation
does not establish native exactly-once semantics.

The journal contains conversation content. It belongs outside every checkout in
an owner-only directory, alongside the caller's private native state. No provider
secret is written there by the bridge. Router diagnostics continue to carry
canonical settlement; the bridge never changes the ledger's outcome.

## Promotion gates

1. Live Codex start, native tool execution, bridge restart and native resume on
   one unchanged binding passed for the bounded low-effort text/tool slice.
   Live reasoning replay, credential rotation during this bridge check and a
   daemon restart during this bridge check remain acceptance work.
2. Incremental bounded SSE translation with terminal settlement before completion
   delivery, disconnect tests and partial argument/reasoning handling.
3. A reviewed durable continuation contract that handles native history edits and
   preserves ownership without turning the daemon into a general transcript store.
4. Explicit model capability, output budget, thinking and cache policies, with
   honest error/usage mappings and native helper/compaction behavior.
5. Broader native errors, parallel tools, cancellation and retry qualification.

## Primary protocol references

The [Claude Code gateway guide](https://code.claude.com/docs/en/llm-gateway-protocol)
describes Messages endpoint expectations and native capability handling.
[Messages streaming](https://platform.claude.com/docs/en/build-with-claude/streaming)
defines content-block, tool argument and thinking events.
[Responses function calling](https://developers.openai.com/api/docs/guides/function-calling)
defines call IDs, tool results and strict schema behavior.
[Responses reasoning](https://developers.openai.com/api/docs/guides/reasoning)
describes preserving output and encrypted reasoning for stateless continuation.
These are living protocol references, consulted 2026-09-10. They do not establish
the Codex subscription backend's conformance to the metered API. No third-party
implementation code is incorporated.
