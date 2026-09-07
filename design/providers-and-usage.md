# Providers, credentials, and usage

Status: Codex subscriptions plus Kimi and GLM coding plans are settled;
adapter details require validation. Metered API products are deferred and cannot
serve as automatic paid overflow when a coding-plan window is exhausted.

## Provider boundary

| Adapter | Initial direction | Evidence and remaining work |
| --- | --- | --- |
| Codex subscriptions | Native Codex backend Responses, including streaming and continuation | Validate account enrollment, refresh, usage, HTTP/SSE, WebSocket, compaction, and native CLI configuration |
| Kimi | Anthropic Messages using the chosen product's endpoint and credentials | Published Kimi Code compatibility; distinguish membership from platform products |
| GLM / Z.AI | Anthropic Messages; Chat Completions when a consumer requires it | Published coding-plan protocol endpoints; retain endpoint/product distinction |
| MiniMax | Future option, Anthropic Messages | Retain research; not a first-release integration gate |
| Actual Anthropic | Excluded | No Claude subscription OAuth, Fable quota logic, or Claude monitoring probes |

The public vendor evidence supports compatible interfaces, not identical feature
sets. [Kimi Code](https://www.kimi.com/code/docs/en/) lists separate protocol
endpoints. [Z.AI](https://docs.z.ai/devpack/tool/others) distinguishes Messages and
Chat Completions endpoints for coding tools. [MiniMax](https://platform.minimax.io/docs/api-reference/text-anthropic-api)
documents both supported and ignored parameters. Adapter conformance tests must
cover the exact selected product and model. Do not silently send unsupported
capability requests just because the JSON shape is accepted.

Store protocol, upstream provider, account product/plan, and concrete model as
different dimensions. A Messages client calling MiniMax is not an Anthropic
account. No universal model-name alias or effort mapping is assumed.

## Credential ownership

Routed accounts have one refresh authority in Poolparty. Use a secret-store
adapter or encrypted durable credential store; the encryption key is separately
injected. Public configuration refers to logical secret handles only. Token
enrollment/import is privileged and never returns existing secrets through normal
status, UI, or CLI output. Do not embed credentials in image layers.

Codex device authentication is a human enrollment flow; the resulting credential
lifecycle includes refresh and possible reauthentication. It is not a permanently
valid static bearer token. Official guidance documents
[`codex login --device-auth`](https://learn.chatgpt.com/docs/auth) for remote
environments. The Poolparty enrollment UI and native-account import contract still
need a spike. In particular, avoid two independent processes refreshing the same
rotating credential cache.

The Codex subscription backend is a distinct adapter from the public OpenAI API.
Community backend behavior is compatibility evidence, not a stable public API
promise. Track the tested client and upstream reference revisions. Keep changes
localized to this adapter and return unsupported errors for unimplemented routes.

Consumer authentication uses Poolparty grants, never a provider OAuth token as a
general administrator password. For the optional observation-only mode, callers
keep their own credentials and contribute/use account observations; they remain
responsible for their direct session binding and cooperative admission.

## Observation model

An observation records:

- Stable quota-owner/account reference and provider/product identity.
- Provider window key, scope (account, model family, feature), duration if known,
  reset time if known, and reported utilization or absolute remaining units.
- Unit and denominator semantics. Keep percent used and percent remaining clear.
- Observation time, provider timestamp if present, source, and freshness deadline.
- Collection result and error separately from the last successful observation.

Use an extensible collection of windows rather than hard-coding five-hour and
weekly columns. A provider may expose monthly or additional scoped windows.
Missing fields can mean unsupported, absent, or unavailable; distinguish these.

Collection preference: provider quota/status reads, telemetry from routed
responses, then explicitly configured probes where necessary. Poll with jitter,
bounded concurrency, freshness caching, and backoff. Successful inference proves
only the tested route's health. No Claude CLI is needed in this service image.

Maintain three separate views: authoritative provider observations, locally
observed token/request usage, and active admission pressure. Token totals and
notional API costs cannot be converted into subscription remaining capacity without
a valid provider-specific basis. Two credentials sharing a quota owner do not
create two quotas. Out-of-order observations must not overwrite newer evidence.

Quota reset is a change of window, not proof that credentials or the model work.
Do not clear a newer exhaustion signal with an older poll. Unknown/stale capacity
policies should be explicit per pool, bounded, and visible in selection reasons.

## Allocation and accounting

Respect applicable package concurrency limits separately from subscription quota
windows. Keys sharing capacity share admission counters; metered API rate tables
are not evidence of a coding-plan ceiling. See the [admission contract](admission.md)
and [provider spike](../research/chinese-providers-spike.md).

Selection filters hard requirements first and then scores eligible candidates
using quota headroom and active pressure. Record exclusion reasons and the policy
revision for diagnostics. Do not sum unrelated provider percentages into a fake
global token budget. Show per-account windows and eligible-account counts instead.

A shared control API can coordinate admission among cooperating direct consumers,
but cannot prevent out-of-band provider calls. The UI should distinguish routed,
reported direct, and unobserved usage coverage. Transcript ingestion is deferred;
it can contribute local activity observations later without becoming quota truth.
