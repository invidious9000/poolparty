# Concurrency and admission

Status: required accounting boundaries, proposed implementation. Provider limits
are configuration with provenance, not constants baked into the allocator.

## Limits and ownership

Respect published concurrency limits when their applicability is established.
Concurrency, requests per minute, tokens per minute, spending budgets, and
subscription windows are separate constraints. A low token utilization does not
make a saturated concurrent-request pool eligible.

Each limit records provider, product, owner scope, optional model or model group,
unit, ceiling, source, verification date, and any operator override. Multiple
credentials belonging to one quota owner share the same counters. Different model
caps can coexist with an account-wide cap; admission must satisfy every applicable
constraint. Requests through Messages and Chat Completions share counters when
they consume the same provider capacity.

The initial Chinese-provider accounts use coding plans. GLM's balance-consumption
API concurrency table does not apply to those accounts. Consult the matching
package benefits and [provider spike](../research/chinese-providers-spike.md);
do not import metered API ceilings as Coding Plan defaults. Conservatively
aggregate credential aliases at account scope until the actual scope is known.
Do not multiply capacity by issuing extra keys. Unknown scope remains visible
to operators.

Local safety caps can be stricter than published limits. Raising a local cap does
not override a verified provider cap. Unknown provider limits are not infinite:
use an explicit finite local cap, disclose incomplete coverage, and react to
provider throttling without inventing an authoritative numeric ceiling.

## Claim lifecycle

Count inference requests, not sessions, CLI processes, TCP connections, or tool
execution time. An idle WebSocket does not consume inference concurrency merely
because it is open. Each generation consumes the applicable claims; a provider
connection limit, if any, is a separate counter. Parallel child requests and
inference-based compaction count too. Usage reads have separate collection limits.

Within one short database transaction, authorize the binding, resolve all
applicable counters, check capacity, and persist the request claim. Dispatch only
after commit. No network operation takes place under a database transaction.
Two simultaneous admissions cannot both consume the final slot. Acquisition of
multiple constraints is all-or-nothing; there is no partial claim or lock-order
deadlock. Creation of an idle binding promises affinity, not reserved capacity.
A combined create-and-admit operation performs both atomically.

Claims move through reserved, dispatching, active, uncertain, and settled states.
Persist dispatch intent before network transmission. A crash after intent but
before confirmation is uncertain even if no bytes actually reached the provider.
Confirmed completion or a proven pre-dispatch failure releases capacity exactly
once. A local timeout, client disconnect, or cancellation request does not by
itself prove upstream computation stopped.

For uncertain requests, retain conservative pressure until provider reconciliation
or a provider-enforced execution bound established for that adapter. An arbitrary
local timeout is not that bound. Without such evidence, retain uncertainty;
any future operator override must explicitly record acceptance of residual risk.
Lease expiry triggers reconciliation; it is not proof of free provider capacity.
A restart must not admit over surviving upstream work merely because local
sockets disappeared.
Exhaustion and uncertain pressure preserve the session's account binding.

## Saturation behavior

Initially reject admission promptly with `session_concurrency_exhausted`, the
preserved binding, blocking constraint identifiers, and `not_dispatched` state.
Native error rendering must be validated against each client's retry behavior.
Do not guess a Retry-After from average request duration. Bounded queueing can be
introduced later as an explicit caller policy with a deadline and cancellation.

A new session may choose another eligible account. A bound session never moves to
escape a cap. Existing requests drain when an operator lowers a cap; subsequent
admissions wait until every constraint permits them. No forced cancellation is
implied by a configuration change.

Out-of-band calls make locally counted pressure a lower bound. Status must expose
routed/cooperating/unobserved coverage. Provider 429 responses can signal dynamic
load or another limit; preserve the reason when available, back off the correct
scope, and do not rewrite all account windows to exhausted.

## Implementation acceptance

- Race more admissions than available slots through two credential aliases and
  both protocol routes; accepted claims never exceed shared model/account caps.
- Saturate one model while another remains eligible under the aggregate cap.
- Reject a resumed saturated session while admitting a fresh session elsewhere.
- Crash after dispatch intent; verify restart retains uncertain pressure and
  refuses automatic replay. Expiring a lease alone must not free that pressure.
- Duplicate confirmed completion/cancellation events settle once; local cancel
  alone retains uncertain pressure. Idle bindings remain resumable.
- Lower a cap below current pressure; existing work drains and new work is refused.
- Preserve admission safety when observations arrive out of order or are absent.

These are required future implementation tests, not claims of existing daemon
behavior. The source/client spikes are tracked in the [DD review](dd-review.md).
