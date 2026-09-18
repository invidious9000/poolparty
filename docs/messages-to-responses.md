# Claude Code through the Codex Responses route

`spikes/messages_responses/bridge.py` is a local, buffered protocol experiment.
It translates Claude Code's Messages requests into an existing Poolparty Codex
subscription binding. Provider credentials remain inside Poolparty. The daemon
itself still rejects Messages on Codex bindings.

Read the [translation contract](../design/messages-to-responses.md) for the state,
reasoning, cache and dispatch boundaries. This fixture supports a bounded text and
function-tool session with separate parent and subagent histories. The bounded live slice below passed; broader native
compatibility remains unqualified.

## Offline validation

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s spikes/messages_responses -p 'test_*.py'
```

These tests use isolated state and synthetic loopback servers. They cover tools,
reasoning replay, native history regrouping, cache/effort policy, binding/model
checks, independent child histories, concurrent request queuing, errors, redirects,
incomplete output and refusal to replay after failure across the whole binding.
They never launch installed Claude Code, read operator credentials or call a live
provider.

An additional explicit macOS probe runs installed Claude Code against synthetic
Responses through the real bridge:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 spikes/messages_responses/native_probe.py \
  --cli /path/to/claude
```

It pins Claude Code `2.1.267`, uses an isolated native home and loopback network
sandbox, and supplies only synthetic `Read` calls targeting temporary files. It
checks start and resume across a bridge restart. Output consists of counts and
boolean receipts; raw conversation data stays in temporary state and is removed.
This is not live provider evidence or a streaming-latency check.

The local 2026-09-10 run passed with Claude Code `2.1.267`: four synthetic
upstream requests, two successful native `Read` roundtrips, and three prior
reasoning items present in the final request. Bridge restart and native resume
preserved the session. The synthetic upstream does not run Poolparty's daemon or
establish Codex provider acceptance.

The additional subagent probe uses the normal CLI with only `Agent` and `Read`
tools, two identical custom workers and synthetic Responses. It checks separate
agent histories, native child tool execution and reasoning preservation on one
binding. Run both foreground and background forms:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 spikes/messages_responses/native_agent_probe.py \
  --cli /path/to/claude
PYTHONDONTWRITEBYTECODE=1 python3 spikes/messages_responses/native_agent_probe.py \
  --cli /path/to/claude --background
```

Both forms passed locally on 2026-09-11 with Claude Code `2.1.267`. Each completed
two child `Read` roundtrips. Background completion notifications also continued
the parent's history. The observed native session and agent headers select the
histories; every request still requires the same caller grant and bound route.

## Live validation evidence

A separately authorized 2026-09-10 run used Claude Code `2.1.267`, the local bridge,
an existing deployed Poolparty daemon and the Codex subscription backend. With
`gpt-6-astra` pinned at `low` effort, start and resume each executed one native
`Read` tool against a different synthetic nonce file and returned its exact contents.
All four inference attempts settled as succeeded on the original binding.

The bridge restarted between phases. Claude Code retained its session and native
state; the provider/account/model/effort binding and credential generation were
unchanged. No daemon restart, deployment update or credential rotation was needed.
The scoped caller grant and provider credential custody remained unchanged.

The backend carried completed tool/text items in `response.output_item.done` while
the terminal response had an empty output array. The decoder now assembles those
items, requires matching IDs and complete indices, and checks any populated
terminal array for agreement. Synthetic regression coverage exercises this form.

This low-effort live run returned no reasoning items. It establishes native tool
execution, durable attempt settlement and same-session resume, but live reasoning
preservation remains unproven. Synthetic reasoning replay coverage still passes.
The fixture buffers responses and restricts native tools to the exact synthetic
file through the existing Messages observer. Streaming latency, arbitrary tools,
compaction and ordinary interactive sessions remain separate gates. All raw
requests, responses, transcripts and binding receipts remain in private operator
state outside this checkout.

A subsequent local-launcher check pinned Astra at `high` effort and used Claude's
normal system prompt and full tool definitions. A simple request and same-session
continuation with a native `Read` tool passed, with three succeeded attempts on
one binding. This required preserving text system messages inside the conversation,
in addition to top-level instructions. That high-effort check also returned no
reasoning items, so it does not expand the live reasoning acceptance claim.

A 2026-09-11 subagent check at Astra `high` effort completed two native worker
agents, each reading the same synthetic nonce file once. All six inference
attempts succeeded on one unchanged binding and credential generation, with no
pending bridge operation. Parent and child histories remained separate. Two
reasoning items were returned on final child turns and retained in their journals;
no later child request exercised replay of those items. Background completion
turns are covered by the synthetic native probe above.

## Preparing a live experiment

No `--live` means help only. A live experiment requires an already-running router,
an existing open Codex binding, and `POOLPARTY_NATIVE_GRANT` supplied from the
operator's caller-grant mechanism. The bridge does not load a provider credential
or native authentication cache. Do not change deployment state to run it.

Use an empty owner-only directory outside git checkouts for bridge state. Keep
the native CLI home and transcript outside checkouts too. The model and effort
must be chosen when creating the binding, then used unchanged by Claude Code.

The following uses synthetic placeholders and starts the bridge only:

```sh
python3 spikes/messages_responses/bridge.py --live \
  --router https://router.example.com --binding binding-a \
  --state-dir /path/to/private/bridge-state --port 8765 \
  --codex-default-output-budget --codex-automatic-cache
```

The two profile flags are deliberate: `max_tokens` is not forwarded as a hard
output cap, and Messages cache-placement markers are replaced with Codex automatic
caching. Omit the cache flag to reject native cache annotations. Numeric thinking
budgets, disabled thinking, effort mismatches, context edits other than keep-all,
images, server tools, citations and unknown fields are rejected. A backend model
alias differing from the exact pin is rejected too.

Configure an isolated Claude Code invocation with the following values. Keep the
grant in the environment; never place it in an argument or URL:

| Setting | Value |
| --- | --- |
| `ANTHROPIC_BASE_URL` | `http://127.0.0.1:8765/routes/binding-a` |
| `ANTHROPIC_AUTH_TOKEN` | The same scoped Poolparty caller grant |
| `CLAUDE_CONFIG_DIR` | A private isolated native configuration directory |
| `ANTHROPIC_MODEL` and default Sonnet/Opus/Haiku model overrides | The exact bound Codex model |
| `CLAUDE_CODE_MAX_RETRIES` | `0` |
| `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`, `DISABLE_AUTOUPDATER` | `1` |

The native synthetic probe uses `--bare`, an explicit `--model` and matching
`--effort`, `--tools Read --allowedTools Read`, `--permission-mode dontAsk`,
`--strict-mcp-config`, empty `--setting-sources`, a replacement system prompt and
an explicit session UUID. It does not set `MAX_THINKING_TOKENS`. Start with that
narrow harness shape and a synthetic file. The live check qualified that bounded
shape; normal interactive defaults, helper models and compaction may require unsupported
features. Claude Code remains responsible for tool permissions and execution.

For a clean restart, stop the bridge after the native turn finishes, then use:

```sh
python3 spikes/messages_responses/bridge.py --live --resume \
  --router https://router.example.com --binding binding-a \
  --state-dir /path/to/private/bridge-state
```

Resume restores the bridge port and translation policy. Resume Claude Code with
its original session ID and native state. Missing bridge state, changed native
history, changed system instructions or a changed binding fail explicitly.

## Failures and private state

The bridge serializes dispatch, admits at most 16 active or queued requests, and
waits at most 180 seconds for a queue slot to reach dispatch. It retains at most
64 child histories and allows at most 64 upstream attempts across the binding. Request
and translated request bodies are limited to 2 MiB, buffered SSE to 4 MiB, individual
upstream frames to 256 KiB and the journal to 16 MiB. Transport waits are bounded.
All routes require the caller grant; only the exact bound Messages route forwards.
Proxies and redirects are disabled. No token-counting or model-discovery route is
implemented.

`messages-responses.json` contains the original binding, operation and attempt
IDs, parent and child conversation content, encrypted reasoning and original Responses items. It
is sensitive operator state, even though the checked-in fixture is synthetic.
Do not commit, share or print that file or native transcripts.

An upstream rejection preserves its HTTP status and leaves a pending marker. A
timeout, malformed/incomplete stream or delivery failure also fences the experiment.
Inspect the original binding and recorded router attempt through the existing
control API. Never delete a pending marker and blindly retry. The fixture does
not reconcile uncertain work, release capacity, migrate accounts or replay a
response. If the caller chooses a fresh conversation, create a new logical session
and separate private state explicitly.
