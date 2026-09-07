# Kimi, GLM, and MiniMax adapter spike

Status: bounded due diligence and synthetic contract exercise, 2026-09-06.
No credentials, provider inference calls, account changes, or consumer changes
were used. This is implementation input, not a production conformance claim.
Actual Anthropic provider integration remains excluded; Messages is the protocol.

Follow-up: [existing integration priors](integration-priors.md) include GLM quota
parsing and live usage/cache/continuation findings, plus earlier Kimi coding cache
tests. Treat unverified items below as limits of this source/client spike; dated
prior checks narrow the new validation work. [DeepSeek PAYG](deepseek.md) is now
an additional initial product, independently scoped from these coding plans.

## Evidence and recommendation

Evidence labels:

- **Vendor**: current official documentation, retrieved on the date above. These
  pages are mutable and sometimes disagree across product-specific guides.
- **Source**: inspected public vendor code at the immutable revisions below.
  Client behavior is evidence of an integration, not an upstream API guarantee.
- **Fixture**: independently written synthetic cases exercised locally. These
  establish the proposed parser contract only.
- **Unverified**: not established by the inspected sources or a live probe.

The initial adapters are **Kimi Code and GLM Coding Plan**. MiniMax is a named
future adapter, not part of initial enrollment. Metered products below are
comparison boundaries, not fallback destinations.

**Recommendation:** implement separate product adapters on a common transparent
Messages transport. Keep provider, region, product, quota owner, credential
generation, concrete model, and supported effort independent. Start with enrolled
API keys. Native-client OAuth import and renewal need separate contracts.
Do not translate all traffic through Chat Completions or Responses merely to
share transport code. Their reasoning, caching, usage, and auxiliary routes differ.

Multiple consumers belonging to one subscriber are distinct from multiple people
sharing a subscription. The intended same-owner deployment does not establish
multi-user sharing. Its remaining product question is whether the chosen coding
plan supports the actual custom client and its intermediary. Published native
client setup is evidence for that setup, not a blanket pooling entitlement.

## Product and endpoint matrix

URLs are upstream configuration, never inferred from caller input. The initial
region below is international except where a separate mainland endpoint is noted.

| Product | Messages base URL | Authentication and alternate protocol | Product boundary |
| --- | --- | --- | --- |
| Kimi Code membership | `https://api.kimi.com/coding/` | Code-console API key; official native configuration uses `ANTHROPIC_AUTH_TOKEN`. Chat Completions and documented Codex Responses use `https://api.kimi.com/coding/v1` | Membership keys and platform keys are separate. Retain truthful client identity; the overview explicitly prohibits changing User-Agent to impersonate another client. [Vendor: overview](https://www.kimi.com/code/docs/en/), [native setup](https://www.kimi.com/code/docs/en/third-party-tools/claude-code.html) |
| Kimi Open Platform | `https://api.moonshot.ai/anthropic` | Platform key with Bearer auth; ordinary API base `https://api.moonshot.ai/v1`. Mainland `api.moonshot.cn` is a separate region configuration | Metered product, distinct from membership. Do not redirect a Code request here when exhausted. [Vendor: Messages](https://platform.kimi.ai/docs/api/messages), [product comparison](https://www.kimi.com/code/docs/en/kimi-code/faq.html) |
| GLM Coding Plan, individual | `https://api.z.ai/api/anthropic` | Z.AI API key; standard inference auth is Bearer. Coding Chat Completions base `https://api.z.ai/api/coding/paas/v4`; Responses base `https://api.z.ai/api/v1` | Plan benefits are confined to listed tools. Same subscriber may run concurrent projects; multi-user account sharing is prohibited. [Vendor: integration](https://docs.z.ai/devpack/tool/others), [usage policy](https://docs.z.ai/devpack/usage-policy) |
| GLM Coding Team Plan | Same coding endpoints, subject to entitlement validation | Dedicated member Team Plan Key, separate from other platform API keys | One member per seat; quota is per seat, with optional configured overage. A team administrator is not automatically a seat. [Vendor: Team Plan](https://docs.z.ai/devpack/teamplan) |
| GLM metered platform | Messages eligibility requires account/product confirmation | Documented general Chat Completions base `https://api.z.ai/api/paas/v4`, Bearer platform key | The current model guide restricts model API access for present or past Coding Plan subscribers to Chat Completions. Do not assume buying a plan leaves all metered protocols available. [Vendor: API introduction](https://docs.z.ai/api-reference/introduction), [GLM-5.3 model guide](https://docs.z.ai/guides/llm/glm-5.3) |
| MiniMax Token Plan | `https://api.minimax.io/anthropic` | Subscription Key; Messages reference uses `X-Api-Key`; native tools also document Bearer auth. Chat Completions/Responses base `https://api.minimax.io/v1` | Subscription Key consumes included quota and eligible purchased Credits. A standard platform key consumes metered balance, despite using the same inference host. [Vendor: other tools](https://platform.minimax.io/docs/token-plan/other-tools), [Messages](https://platform.minimax.io/docs/api-reference/text-chat-anthropic), [FAQ](https://platform.minimax.io/docs/token-plan/faq) |
| MiniMax metered platform | Same Messages base as Token Plan | Standard platform API key; enroll product explicitly | Wallet balance is separate from Token Plan/Credits. Existing Teams have member seats and shared Credits; new Team Plan purchases are suspended from 2026-09-05. [Vendor: Teams](https://platform.minimax.io/docs/guides/pricing-token-plan-team) |

Each Messages base above appends `v1/messages` with exactly one separator.
Do not concatenate the Chat Completions base with another `/v1/messages`.
Strip caller provider-credential headers and inject the enrolled credential using
the product adapter's tested auth form. Never forward credentials to a redirect
or derive monitoring hosts from arbitrary user-supplied URLs.

## Model identity, capabilities, and effort

| Route | Documented model/capability facts | Required adapter handling |
| --- | --- | --- |
| Kimi Code | `k3`, `k3-256k`, `kimi-for-coding`, `kimi-for-coding-highspeed`. K3 access/context and high-speed access depend on membership. K3 has `low/high/max`; K2.7 Code is thinking-on. K3-256k excludes video. [Vendor: models](https://www.kimi.com/code/docs/en/kimi-code/models.html) | Store model ID and observed entitlement/context together. `kimi-for-coding` is a product alias, not an immutable model build. A permanent upstream weight/version pin is unverified. |
| Kimi native Messages | Documented effort mapping is low -> low, medium/high -> high, xhigh/max -> max. **Disabling thinking routes K3 and K2.7 Code to K2.6.** [Vendor: native setup](https://www.kimi.com/code/docs/en/third-party-tools/claude-code.html) | Reject thinking-off for an exact K3/K2.7 pin before dispatch. Expose provider effort names; do not claim medium and high are distinct upstream levels. Verify exact wire fields with the selected client version. |
| Kimi Open Platform | Discovery uses `GET /v1/models`, Bearer key, and exposes IDs and capability fields; the example ID is `kimi-k3`. [Vendor: list models](https://platform.kimi.ai/docs/api/list-models) | Do not reuse membership model IDs automatically. Discovery establishes advertised capability, not successful inference or quota headroom. |
| GLM Coding Plan | Current models are `glm-5.3`, `glm-5.3-flash`. Requests using GLM-5.2/5.1 are automatically routed to 5.3; GLM-4.7 routes to 5.3-Flash. [Vendor: overview](https://docs.z.ai/devpack/overview) | Reject retired IDs for exact historical-model pins. A provider echo of the requested alias is insufficient proof of the model actually served. No independently verified account model-discovery endpoint was found in this spike. |
| GLM effort | Coding guide normalizes minimal/light/low -> low, medium/high -> high, xhigh/max/ultra -> max. Unknown strings default to max. Thinking-off becomes low. Standard model guide instead says disabling reasoning fails. [Vendor: model switching](https://docs.z.ai/devpack/latest-model), [model guide](https://docs.z.ai/guides/llm/glm-5.3) | Validate effort locally and reject unknown values. Never represent low as reasoning-disabled. Keep protocol/product behavior separate until the disagreement is tested. GLM-5.3 is text-only; visual MCP tooling is not direct image-input support. |
| MiniMax Messages | M3 and the listed M2.x family are supported. M3 accepts image/video and adaptive thinking; M2.x excludes image/video. M3 thinking defaults off; M2.x accepts disabled thinking but still thinks. [Vendor: compatibility](https://platform.minimax.io/docs/api-reference/text-anthropic-api) | Reject an M2.x thinking-off hard requirement. M3 thinking-on/off is not proof of low/medium/high effort support; exact effort levels remain unverified. |
| MiniMax discovery | `GET /anthropic/v1/models` with `X-Api-Key`; supports pagination and returns model IDs. [Vendor: list models](https://platform.minimax.io/docs/api-reference/models/anthropic/list-models) | Discover with the enrolled product key. Do not equate catalogue visibility with plan access or successful execution. |

## Messages conformance boundary

| Feature | Kimi | GLM / Z.AI | MiniMax |
| --- | --- | --- | --- |
| Streaming and tools | Platform Messages documents streaming, tools, ordered content and thinking signatures. Code has published native integrations, but exact SSE behavior remains unverified. [Vendor: Messages](https://platform.kimi.ai/docs/api/messages) | Streaming and function calling are documented; preserved thinking defaults differ between Coding Plan and standard API. Exact Messages event/usage coverage needs a probe. [Vendor: thinking modes](https://docs.z.ai/guides/capabilities/thinking-mode) | Streaming, tool choice and tool blocks documented. Return the entire assistant content, including thinking, in subsequent tool turns. [Vendor: compatibility](https://platform.minimax.io/docs/api-reference/text-anthropic-api) |
| Cache | Platform response schema includes cache-read/cache-creation usage. Code cache exists, but breakpoint/TTL/usage equivalence to platform is unverified. | Automatic caching exposes `usage.prompt_tokens_details.cached_tokens` on Chat Completions. This does not establish Messages `cache_control` or cache-write semantics. [Vendor: caching](https://docs.z.ai/guides/capabilities/cache) | Explicit ephemeral cache has a five-minute lifetime refreshed on hit, hierarchy tools -> system -> messages, separate read/write usage. The explicit-cache page lists M2.x; do not assume its full matrix extends to M3. [Vendor: explicit cache](https://platform.minimax.io/docs/api-reference/anthropic-api-compatible-cache) |
| Count tokens | No verified Code Messages count route. Platform has `POST /v1/tokenizers/estimate-token-count`, a different Chat-shaped API. [Vendor: estimator](https://platform.kimi.ai/docs/api/estimate) | No verified Messages count route. `POST /api/paas/v4/tokenizer` documents older model enums; neither current-model nor Coding Plan eligibility follows. [Vendor: tokenizer](https://docs.z.ai/api-reference/tools/tokenizer) | `POST /anthropic/v1/messages/count_tokens` explicitly documented for M3. M2.x support unverified. [Vendor: compatibility](https://platform.minimax.io/docs/api-reference/text-anthropic-api) |
| Ignored or incomplete controls | Full Code parameter matrix unverified. Do not infer parity from the platform schema. | Full Messages parameter matrix unverified. A Chat Completions capability is not automatically a Messages capability. | `top_k`, `stop_sequences`, `mcp_servers`, `context_management`, and `container` ignored. Reject hard requirements for these. `service_tier` is admission priority with separate pricing, not reasoning effort. [Vendor: compatibility](https://platform.minimax.io/docs/api-reference/text-anthropic-api) |

Proposed transport boundary: forward accepted Messages bodies and SSE bytes
without rebuilding content blocks. Observe a bounded stream copy for usage and
terminal state. Preserve tool IDs, signature blocks, unknown extension fields,
partial argument JSON, cache-control placement, and ordering. Never stringify
tool input fragments independently or remove thinking on a tool continuation.
Unsupported hard requirements fail before dispatch. Unknown optional extensions
may pass through only under an explicit capability policy, not silent claims of
support.

EOF is not completion. Record a stream error or missing terminal event separately
from success. Local cancellation requests upstream closure but may retain uncertain
admission pressure until upstream completion is established; it does not expire
the session. Confirmed upstream completion or pre-dispatch failure releases the
claim exactly once. A parser failure may make accounting
unknown but cannot trigger replay. All failures preserve provider/account binding.

## Capacity and observation matrix

| Product | Quota/window semantics | Read surface and remaining gaps |
| --- | --- | --- |
| Kimi Code | Weekly refresh anchored to subscription, rolling five-hour limit, and membership monthly exhaustion can freeze Code quota. All devices and API keys share quota. Optional Extra Usage can automatically cover overflow. [Vendor: membership](https://www.kimi.com/code/docs/en/kimi-code/membership.html) | Console and `/usage` are documented. Legacy official CLI reads `GET https://api.kimi.com/coding/v1/usages` with Bearer credential and parses `usage` plus `limits[].detail/window`. **Source**, not a current stable monitoring specification. Monthly freeze, Extra Usage and present OAuth/key coverage are unverified for this reader. |
| Kimi Open Platform | Metered balance plus account-tier RPM/TPM/TPD constraints. [Vendor: limits](https://platform.kimi.ai/docs/pricing/limits) | `GET /v1/users/me/balance` exposes available/cash/voucher balances. Global endpoint documents USD. This is balance, not subscription percentage or rate headroom. [Vendor: balance](https://platform.kimi.ai/docs/api/balance) |
| GLM Coding Plan | Five-hour and weekly credit windows; cached/input/output tokens and tool calls have different weights, with off-peak discounts. Dynamic concurrency depends on plan/load. [Vendor: overview](https://docs.z.ai/devpack/overview), [usage policy](https://docs.z.ai/devpack/usage-policy) | Official plugin reads `/api/monitor/usage/quota/limit`, `/model-usage`, and `/tool-usage` under the same monitoring prefix. **Source** reader sends the API key directly as `Authorization`, unlike inference Bearer auth. Current credit schema, percent direction, owner scope and reset fields require validation. |
| GLM individual/team distinction | Individual coding FAQ says no balance fallback. Team Plan may enable per-member metered overage. [Vendor: FAQ](https://docs.z.ai/devpack/faq), [Team Plan](https://docs.z.ai/devpack/teamplan) | Model/tool usage history is activity, not remaining quota. A team member key, personal key and team organization are not interchangeable quota owners. Machine-readable team overage/read scope remains unverified. |
| MiniMax Token Plan | Included usage is bounded by rolling five-hour and weekly windows. Purchased Credits may cover overflow automatically; all supported tools share quota. It is intended for interactive individual developer use, with dynamic traffic limits. [Vendor: FAQ](https://platform.minimax.io/docs/token-plan/faq) | Documented `GET https://www.minimax.io/v1/token_plan/remains`, Bearer key. Source CLI also constructs `/v1/token_plan/remains` relative to its configured base. Host equivalence must be probed. Preserve per-row scope, percentages, window timestamps, status and boost fields. |
| MiniMax metered | Separate wallet and standard API key; Teams can have a shared wallet. [Vendor: Teams](https://platform.minimax.io/docs/guides/pricing-token-plan-team) | Official CLI selects `/account/query_balance` for one platform-key form. **Source** evidence only; do not classify enrolled product solely by key prefix. Source schema includes available amount and cash/voucher/credit/owed fields. Exact currency/read scope requires validation. |

Kimi error handling needs body-level distinctions: documented quota exhaustion
and concurrency errors may use HTTP 403; model entitlement failures may use 401;
429 can mean load rather than exhausted subscription. Preserve upstream detail in
the private diagnostic record and normalize caller errors without treating every
401 as key expiry or every 429 as quota exhaustion. [Vendor: errors](https://www.kimi.com/code/docs/en/kimi-code/error-reference.html)

### GLM coding concurrency

The platform's static model-concurrency table applies to users consuming API
balance, according to an authenticated operator inspection. Those limits are
excluded from coding-plan configuration. This scope is operator-confirmed;
the unauthenticated source inspection could not inspect the table. The official
[rate-limit reference](https://docs.z.ai/api-reference/rate-limit) redirected to
[the account console](https://z.ai/manage-apikey/rate-limits) and exposed no table
to this unauthenticated inspection. Public model pages did not establish
the balance-consumption table's account/key/model ownership scope.

Use the enrolled package's verified benefits where available. Until an exact
coding concurrency limit is known, require a finite local cap, explicitly marked
as Poolparty policy, and adjust to provider observations. Do not multiply
capacity by the number of credentials. Admission needs quota-owner/model claims
and any enclosing owner-wide claim; the conservative scope is a local policy,
not a provider fact. A stream occupies its claim until terminal settlement,
including thinking and tool-input streaming. The published dynamic Coding Plan
policy remains separate from any static metered-model limit.

Resolve product/model identity first, then apply the appropriate concurrency key.

### Public package benefits

The current GLM individual-plan overview publishes **Max: 28,000 five-hour
credits and 140,000 weekly credits**. It documents five-hour refresh after
consumption and weekly refresh from activation. These are package allowances,
not fixed numbers of prompts or simultaneous requests. The usage policy gives
Max a recommendation of two or more concurrent projects and describes dynamic
concurrency; it supplies no numeric inference-request maximum. Confirm that the
enrolled package generation matches the current published package before using
these allowances. [Vendor: plan overview](https://docs.z.ai/devpack/overview),
[usage policy](https://docs.z.ai/devpack/usage-policy)

The inspected Kimi Code pages publish membership-dependent model access and
relative consumption, but do not establish a stable numeric Max concurrency or
absolute token allowance. The membership pricing page did not expose usable text
in this inspection. Do not translate a marketed plan name or relative multiplier
into an absolute allowance, or map Max to a differently named entitlement tier
without account evidence. The read-only membership/usage result must establish
the account's actual package limits and model access. [Vendor: membership](https://www.kimi.com/code/docs/en/kimi-code/membership.html),
[model entitlements](https://www.kimi.com/code/docs/en/kimi-code/models.html)

### Parser decisions

These are proposed Poolparty rules, supported by the source and fixture evidence:

1. Preserve raw provider fields alongside a versioned normalization result.
   Absent limits, missing totals, zero denominators, unknown units and parse errors
   do not mean unlimited or zero used.
2. MiniMax's source explicitly documents that `*_usage_count` has meant either
   remaining or consumed. Prefer explicit percent/status evidence. Without a
   verified schema or a percentage that resolves the ambiguity, expose unknown
   counts. Do not copy the CLI's legacy remaining-count fallback into admission.
3. MiniMax source separates exhausted/unlimited statuses and a weekly boost in
   permille. Retain those fields; do not clamp a boosted display to 100%, count
   every model row as an independent pool, or infer unlimited from a zero total.
4. The GLM plugin's older flattening labels TOKENS_LIMIT as five-hour and TIME_LIMIT
   as monthly MCP usage, discarding fields for the former. New docs use credits
   across model/MCP usage. Parse raw limits, preserve all windows and unknown
   types, and validate units per product generation.
5. Keep source timestamp, fetched time, scope, reset time, last successful value,
   and collection failure separately. A newer exhaustion observation cannot be
   cleared by an older successful poll. Optional purchased overflow is a separate
   configured admission decision and never permits switching products/accounts.

## Native clients and adapter ownership

Published provider guides support Messages-shaped native clients through base
URL and key configuration. That does not require an Anthropic account adapter.
Kimi, GLM and MiniMax also document native Codex using their **own Responses
endpoints**, so native Codex support is a separate protocol conformance slice.
Do not bridge it through Messages and claim preservation of continuation,
compaction or WebSocket semantics. [Vendor: Kimi Codex](https://www.kimi.com/code/docs/en/third-party-tools/codex.html),
[GLM integration](https://docs.z.ai/devpack/tool/others),
[MiniMax Codex](https://platform.minimax.io/docs/token-plan/codex)

For the first Messages slice, allocate a binding before launching the native
client, supply its binding-scoped base URL, and retain the same handle on resume.
Poolparty owns upstream auth and request admission; the client owns conversation
history, tools and explicit recovery. Verify auxiliary requests and helper-model
calls use that binding and reject conflicts with the resolved model. Do not copy
setup scripts that overwrite global client settings or silently map every helper
alias to a different model.

## Live probe plan and unresolved product choices

No live provider probe below has been run. The native fixture below covers a
subset of the local failure/client cases. Use a dedicated enrolled account/product and minimal
synthetic content after the product choice is settled. Never deliberately exhaust
an account to validate handling.

| Probe | Exact action | Required evidence |
| --- | --- | --- |
| Enrollment and monitoring | One authenticated model-list read where documented, then the chosen quota/balance read; repeat once to check freshness | Auth form, response/application error envelope, quota owner, plan type, all window fields/units, clock/reset interpretation, key vs OAuth support |
| GLM concurrency scope | Read the enrolled coding package's benefits and scope/help metadata, excluding API-balance tables | Whether caps are per owner/model, shared across keys/protocols, and changed by subscription history; finite local policy until established |
| Minimal Messages | One non-streaming and one streaming short response with a current explicit model ID | Actual route, accepted model, thinking behavior, usage/cache fields, terminal sequence, beta/version headers and error IDs |
| Tool continuation | Request one synthetic `echo_value` tool, return its result with the exact assistant content, and make one follow-up | Thinking/signature/tool IDs and partial JSON survive; multiple blocks and final usage are intact |
| Hard effort/capability pin | Test documented provider levels; fixture-reject invalid effort, thinking-off model-switch cases and ignored hard controls before network | Requested, normalized and effective effort separated; no fallback or model replacement |
| Count/cache | Explicit unsupported response locally for unverified count routes. After cache support is confirmed, repeat a synthetic qualifying prefix twice. Future MiniMax adapter only: one M3 count request | Real count route availability; cache read/write semantics; no claim of a guaranteed cache hit or quota calculation from token count |
| Failure/resume | Local mock 401/403/429, JSON application error inside 200, incomplete SSE and stream cancellation; then resume through the persisted binding | One settlement; no success on EOF; binding unchanged; no ambiguous replay or alternate-account attempt |
| Native client | Pinned native CLI with isolated config points to binding route; one prompt, tool turn, quit/resume, and auxiliary-route capture | Same binding/account and exact model across process restart; no unbound helper requests; no credential escape |

Outstanding decisions are concrete: whether each chosen coding product accepts
the custom client through Poolparty, and the exact scope of package limits.
Initial admission stops at included coding quota and does not automatically use
metered balance or purchased overflow. Enrollment must establish whether upstream
automatic overflow is disabled or otherwise enforceable. Same-owner consumers remain in scope.
Do not infer permission for multiple human users, nor block same-owner design
work merely because team products have different contracts.

## Source revisions and validation

The following code was inspected from public vendor repositories. No upstream
code or provider payload was copied into Poolparty. Kimi and Z.AI repository
license files at these revisions are Apache-2.0; MiniMax license applicability was
not established in the inspected files, so no code adoption is proposed.

- Kimi legacy Python CLI, revision
  `86f136422a0aae6b217ea49e7ea1d2e8a1defcd2`:
  [quota reader](https://github.com/MoonshotAI/kimi-cli/blob/86f136422a0aae6b217ea49e7ea1d2e8a1defcd2/src/kimi_cli/ui/shell/usage.py),
  [model discovery and platform URLs](https://github.com/MoonshotAI/kimi-cli/blob/86f136422a0aae6b217ea49e7ea1d2e8a1defcd2/src/kimi_cli/auth/platforms.py).
  Current vendor docs identify a newer Node.js CLI; treat the Python reader as
  historical implementation evidence pending live confirmation.
- Z.AI coding plugins, revision
  `0446d0bb0bc537d97d3ab3664c4b8b9c4a0e1254`:
  [monitoring reader](https://github.com/zai-org/zai-coding-plugins/blob/0446d0bb0bc537d97d3ab3664c4b8b9c4a0e1254/plugins/glm-plan-usage/skills/usage-query-skill/scripts/query-usage.mjs).
- MiniMax CLI, revision `74e904a19fce4b0660fa66f67ba7ee55ad89ddab`:
  [endpoint selection](https://github.com/MiniMax-AI/cli/blob/74e904a19fce4b0660fa66f67ba7ee55ad89ddab/src/client/endpoints.ts),
  [quota types](https://github.com/MiniMax-AI/cli/blob/74e904a19fce4b0660fa66f67ba7ee55ad89ddab/src/types/api.ts),
  [ambiguous-count handling](https://github.com/MiniMax-AI/cli/blob/74e904a19fce4b0660fa66f67ba7ee55ad89ddab/src/utils/quota.ts).

The dependency-free [synthetic exercise](../spikes/messages/contract_cases.py)
checks missing/ambiguous quota data and fragmented SSE observation. Run
`python3 spikes/messages/contract_cases.py`. It is a bounded executable design
example, not the production parser, an upstream recording, or a complete SSE
implementation. No heavy builds or provider calls are needed.

### Executed native Messages fixture

**Fixture, passed:** installed Claude Code **2.1.263**, through
[native_cli_probe.py](../spikes/messages/native_cli_probe.py). The test uses a
fresh temporary HOME/config/work directory, `--bare`, a synthetic bearer grant,
and macOS `sandbox-exec` to permit outbound networking only to loopback. The local
origin requires the synthetic bearer. No API key, real credentials, live provider,
or production conversation is involved. Raw request/output payloads stay in memory;
the printed receipt contains only synthetic assertions.

Verified outcomes:

- Two success-path requests both used the binding-scoped Messages route and exact
  requested model, carrying `Authorization: Bearer <synthetic grant>` without an
  `X-Api-Key` header.
- The CLI accepted streamed thinking/signature blocks and fragmented tool input,
  executed Read against a synthetic temporary file, returned a successful
  `tool_result`, and preserved thinking/signature in the second model request.
- A separate synthetic HTTP 403 quota failure produced exactly one request on the
  same binding path and an error exit, with no successful result.
- Seven dependency-free parser/stream cases passed, including every single cut
  position in a synthetic UTF-8/CRLF SSE stream.

Run `python3 spikes/messages/native_cli_probe.py --cli /absolute/path/to/claude`.
The default expected version is pinned to the observed version and can only be
changed explicitly. This proves native grant/route compatibility and a tool
continuation. Cross-process conversation resume, HTTP 429 retry behavior, real
provider error mapping, daemon persistence/admission, and current provider
entitlement remain untested. The fixture is not a claim that Poolparty already
implements those surfaces.
