# Due diligence review

Reviewed 2026-09-06. Status: source review and isolated native-client spikes;
no production router, provider-account probe, credential migration, or deployment.

## Outcome

Proceed with an independent Rust daemon and one allocation authority. Use
codex-lb as a reference, with selective transport adaptation only when justified
by a concrete compatibility test. Its current Rust library exposes a stdio runtime,
not a typed transport API suitable for direct embedding.

The first account products are Codex subscriptions and Kimi/GLM coding plans.
MiniMax is a named future option. Metered API products and automatic paid overflow
are outside the initial slice. Preserve native Responses and Messages protocols;
actual Anthropic account integration remains excluded.

Start implementing the domain and synthetic transports once these reports are
reviewed. Native integration and live provider acceptance remain separate gates.
The spikes establish useful compatibility facts and counterexamples, not a
production-readiness claim.

## Evidence

| Work | Evidence obtained | Remaining boundary |
| --- | --- | --- |
| [Codex](../research/codex-spike.md) | Pinned native source, isolated CLI/app-server against a synthetic origin; bound URLs, bearer auth and resume | Live subscription backend, refresh rotation, model discovery/compaction choice, interruption safety |
| [Chinese providers](../research/chinese-providers-spike.md) | Primary-source product and protocol matrix; native Messages tool-continuation and quota-error fixture | Account-visible limits, native cross-process resume, real upstream feature/usage behavior |
| [Rust egress](../research/egress-reuse-spike.md) | Pinned exports, transport control flow, dependencies and license inspection | Original Rust transport implementation and runtime conformance |
| Consumer/admission review | Independently expressed consumer requirements and [admission contract](admission.md); private mappings retained separately | Later consumer changes, credential custody cutover and deployment validation |

Consult the individual reports for versions, commands, source links, executed
results, and explicit untested cases. No fixture contains a real provider token,
account identity, or transcript.

Review validation: the native Codex fixture and native Messages fixture were
independently rerun successfully, including the intentional WebSocket replay
counterexample. Seven synthetic quota/SSE cases passed. Local document links,
Python syntax, whitespace, and public/private content boundaries were checked.
There is no Rust daemon build or provider-backend conformance result yet.

## Recommendations supported by DD

1. **Own the application and transport interface.** A whole codex-lb fork inherits
   a mixed-language product and different affinity policy. The current Rust crate
   also requires adaptation: stdio I/O, shared output serialization, global TLS
   initialization, and workspace-level dependency patches are substantive seams.
2. **Allocate before launching or resuming a native session.** Pass the bound base
   URL and a Poolparty grant, then persist and restore them beside caller state.
   A binding ID grants no authority. Native thread/session identifiers are useful
   correlated evidence, not a substitute for authenticated ownership checks.
3. **Keep the account fixed and reject unsupported intent.** Provider acceptance
   of a model or effort field does not prove it was honored. Detect documented
   silent model substitutions and ignored controls. Permit only explicitly
   declared auxiliary models on the same account; no implicit broad wildcard.
4. **Treat concurrency as admission, separately from quota.** Shared account keys
   share pressure. Coding-plan benefits, not metered API tables, determine known
   provider limits. Unknown caps require a finite configured safety policy and
   visible uncertainty. Idle sessions retain affinity without holding a slot.
5. **Start with explicit HTTP/SSE support.** The pinned Codex client can retry an
   interrupted WebSocket via HTTP even when retry counts are zero. Keep its
   WebSocket option disabled in the first supported configuration. Production WS
   support requires a proven admission/attempt fence or client control that
   prevents ambiguous fallback from triggering another inference.
6. **Separate refresh custody from deployment secret injection.** Read-only vault
   delivery is not an atomic rotating-token store. One router authority updates
   credential generations; consumer native auth caches must not keep refreshing
   that same account independently after migration.

## Open questions and working defaults

| Question | Working default / disposition | Closure evidence |
| --- | --- | --- |
| Coding plans or metered API? | Resolved: Kimi/GLM coding plans first; MiniMax future | Product direction confirmed; actual package inventory belongs in the private overlay |
| Fork, dependency, or independent implementation? | Recommend independent Rust; current egress crate reference-only | API mismatch established; adoption requires original transport conformance tests |
| Bound URLs or implicit native session lookup? | Explicit bound URL first | Pinned CLI/app-server fixture; each resume restores configuration |
| Strict single model versus native helper requests? | Primary hard pin plus an explicit auxiliary-model allowlist, all on the same account | Validate each client helper/compaction route; unsupported models fail visibly |
| Codex remote discovery and compaction? | Baseline static verified model catalog; explicitly report unsupported remote operations | Pinned client provider-auth/name behavior and a focused compatibility choice before enabling remote paths |
| Native automatic replay? | HTTP/SSE baseline with bounded/disabled client retries; WS gated | Synthetic interruption tests must count upstream attempts, including fallback across transports |
| Unknown provider concurrency? | Finite local cap; no invented provider ceiling | Matching package benefits or account-visible metadata, shared owner scope verified |
| Credential store? | Encrypted durable records with separately injected key is the simplest initial candidate | Atomic rotation, crash recovery, backup/restore, and existing secret-store write semantics |
| Session retention? | No automatic deletion of active bindings; explicit close and retained tombstones initially | Retention/export contract before any cleanup feature; storage growth observable |
| Machine auth? | Pool-scoped Poolparty inference grants distinct from provider credentials | Native bearer fixture plus later JWT/audience/expiry and authorization tests |
| HA, direct-client claims, transcript collection? | Deferred; single writer and routed inference first | A concrete need and separate coordination contract |

Defaults are recommendations for implementation planning, not an assertion that
all provider packages permit every consumer shape or that all compatibility gates
passed. Do not disguise a custom client as another tool to satisfy an entitlement
check. Resolve actual product restrictions against the intended single-owner
deployment and supported clients; a protocol-compatible endpoint alone is not
that evidence.

## Next bounded implementation slice

Build the durable binding and admission domain with synthetic accounts, a SQLite
store, and fake-clock/fake-origin tests. Expose create/inspect/admit/settle through
the control API, plus a minimal HTTP/SSE route receiving a scoped caller grant.
Keep one prepared upstream attempt and explicit dispatch certainty in the
transport interface. Implement the acceptance cases in [delivery plan](delivery-plan.md)
and [admission](admission.md) before adding credential-bearing adapters.

Then exercise one Codex account and one Messages coding-plan account with a
bounded live checklist: status/usage read, tiny streamed response, tool round-trip,
second turn/resume, cancellation, and quota/auth failure handling. Enrollment and
refresh need their own isolated custody procedure. Synthetic error injection
covers exhaustion without deliberately burning a real subscription window.

The public repo owns executable source and portable evidence. Private integration,
package inventory, credential references, platform auth registrations and real
deployment names remain in the private overlay. Consumers are unchanged during DD.
