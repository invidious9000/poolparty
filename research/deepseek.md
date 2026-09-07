# DeepSeek PAYG adapter and prior integration evidence

Reviewed 2026-09-06. Source/DD update only; no provider credential, live API call,
or new native-client execution. DeepSeek PAYG joins the initial provider scope.
Kimi/GLM remain coding-plan products; MiniMax remains a future option.

## Protocol and model contract

Use the native Messages route at
`https://api.deepseek.com/anthropic/v1/messages` initially. DeepSeek also documents
Chat Completions and Responses, which need their own conformance cases if exposed.
Do not translate protocols to share an implementation. Current model names include
`deepseek-v4-pro` and `deepseek-v4-flash`; stable names can receive upstream model
updates, so an exact model ID is not a guarantee of immutable weights.
[First API call](https://api-docs.deepseek.com/)

The compatibility guide says an unsupported model name maps to Flash. Validate
names before dispatch to preserve a hard pin. Thinking budgets and some control
fields are ignored; accepted JSON is not proof of honored intent. Preserve opaque
thinking, tool and server-tool blocks. Declare capability gaps explicitly.
[Messages compatibility](https://api-docs.deepseek.com/guides/anthropic_api/)

Current effort mappings are low -> low, medium/high/xhigh -> high, and max -> max.
Keep requested and effective effort separate and reject an impossible hard
requirement. Chat requests carrying tools must retain prior `reasoning_content`,
including preceding turns without a tool call. Do not generalize an older
display-only reasoning convention into current transport behavior.
[Thinking modes](https://api-docs.deepseek.com/guides/thinking_mode/)

Caching is automatic and best effort. The documented Chat usage fields distinguish
cache-hit and cache-miss tokens; cache creation and prefix matching have their own
rules. A successful request containing cache metadata does not prove a hit or an
explicit TTL guarantee. Preserve native usage dimensions before normalization.
[Context caching](https://api-docs.deepseek.com/guides/kv_cache/)

## PAYG observations and admission

`GET https://api.deepseek.com/user/balance` provides `is_available` and
`balance_infos[]`, with currency and decimal strings for total, granted, and
topped-up balance. Preserve each currency and amount using decimal arithmetic.
Do not assume the first row is USD or that missing/malformed fields mean zero.
An HTTP/application failure is a collection failure, not an exhausted balance.
[Balance reference](https://api-docs.deepseek.com/api/get-user-balance/)

Keep provider balance, estimated request cost, local budget remaining, and active
concurrency separate. A funded account can still be saturated or disallowed by a
local spending policy. Do not turn wallet balance into subscription utilization
or silently spend it to continue a session bound to another provider. An explicit
new-session policy may select PAYG accounts from an authorized pool.

When a spending ceiling is configured, admission must reserve a defensible upper
cost bound and reconcile actual usage. Unknown outcomes retain conservative
reservations. Pricing needs a dated model/product schedule, including cache,
reasoning and chargeable server-tool dimensions where applicable. A local
estimate is not an authoritative provider bill; external account traffic remains
outside a router-only budget guarantee.

## Published concurrency

DeepSeek documents per-account, per-model ceilings: Pro 500, Flash 2,500, and
Flash Vision experimental 2,500. All API keys share the account counter. Count
from request submission until response completion, including upstream waiting.
These are provider ceilings, not recommended local operating concurrency.

For ordinary accounts, `user_id` values share that ceiling. Expanded-capacity
accounts can have both aggregate and per-user constraints. Messages carries this
dimension in `metadata.user_id`; it also affects cache and scheduling isolation.
Use an authorized stable pseudonymous mapping, never a fresh ID to seek capacity.

The provider emits SSE keep-alive comments while waiting. Its documented
ten-minute cutoff concerns requests that have not started inference; it does not
prove already-started work has finished after ten minutes. Preserve uncertain
pressure after a local disconnect. [Rate limits and isolation](https://api-docs.deepseek.com/quick_start/rate_limit/)

## Existing priors to reuse

Read-only review of an existing integration found concrete prior work, beyond
vendor setup examples. The private source map retains exact implementation and
historical evidence references; no consumer code or configuration is copied here.

| Prior | Useful carry-forward | Boundary to preserve |
| --- | --- | --- |
| Messages and Chat transports with earlier live tool round-trips | Auth/path conventions, streamed tool arguments, continuation fixtures | Historical success is dated evidence, not current all-model parity |
| PAYG balance collector | Poll the balance endpoint without spending inference tokens | Keep currencies/decimals and collection errors; do not copy a normalized wallet score as quota truth |
| Shared Messages usage folding | Later full usage snapshots replace present fields without clearing absent values | Input, cached input and cache creation must not be double-counted |
| Thinking/signature and opaque server-tool preservation | Original regression cases for multi-turn bodies and extension blocks | A router does not reconstruct the client's conversation or execute its tools |
| Provider cache and model-name probes | Distinguish client notation from upstream IDs; test actual repeated-prefix usage | Tolerated fields and absent counters do not establish unsupported behavior |
| Client retry machinery | Identify hidden resubmission paths before integrating the consumer | No emitted text does not prove no upstream dispatch; Poolparty retains stricter uncertainty rules |

The existing transport is a harness component with conversation state, tool-loop
policy, retries and compaction. Use its evidence and isolated ideas; it is not a
drop-in transparent router library. This is the same distinction applied to the
[codex-lb egress review](egress-reuse-spike.md).

## Remaining bounded checks

1. Read one enrolled account's balance and model catalog; verify status, currency,
   quota ownership and any expanded concurrency contract without printing secrets.
2. Run a tiny Messages tool continuation with current thinking settings; preserve
   signatures, server-tool extensions and usage snapshots through the route.
3. Use a synthetic origin for balance failures, mixed currencies, ambiguous
   outcomes, 429 saturation, keep-alives and duplicate attempts. Do not exhaust a
   real wallet or saturate a provider to test local policy.
4. Revalidate historical provider claims only for changed or still-uncertain
   surfaces. The shared native Messages fixture already establishes the basic
   caller grant and bound URL shape; it does not establish DeepSeek behavior.

No new runtime claim is made by this report. Vendor sources were read for current
contracts; existing source and dated reports supplied integration priors.
