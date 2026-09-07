# codex-lb reference assessment

Reviewed: 2026-09-06. Source revision:
[`5ad638b6a4c9c094bcc8866b1d7487173fe3b54e`](https://github.com/Soju06/codex-lb/tree/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e).

Method: read-only source and documentation inspection. No upstream build, live
account enrollment, inference probe, or performance benchmark was performed.
No source code has been copied into Poolparty.

## Observed structure

The application combines a Python/FastAPI control plane, a React frontend, and a
Rust workspace for transport. It is not currently a complete Rust daemon. Its
[Rust architecture](https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/docs/rust-architecture.md)
retains policy and persistence in Python while separating reusable Rust egress
from its worker executable and IPC types. Further Rust migration is intended.

The pinned [manifest](https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/Cargo.toml)
contains `codex-lb-protocol`, `codex-lb-egress`, and `codex-lb-egress-worker`.
The [egress library](https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/crates/codex-lb-egress/README.md)
handles HTTP/TLS/WebSocket transport and cancellation boundaries with pinned
transport dependencies. Its process wrapper is not the account allocator.

## Useful reference surfaces

| Topic | Pinned source | Poolparty use |
| --- | --- | --- |
| OAuth/device enrollment | [OAuth client](https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/app/core/clients/oauth.py) | Learn account enrollment and refresh edge cases; validate against native Codex |
| Quota reads | [Usage client](https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/app/core/clients/usage.py) | Locate backend usage behavior and distinguish collection from inference |
| Observation refresh | [Usage updater](https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/app/modules/usage/updater.py) | Freshness, additional windows, refresh coordination, failure handling |
| Continuation ownership | [Affinity policy](https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/app/modules/proxy/affinity.py) | Distinguish process, thread, turn-state, response, and file ownership |
| Native transport | [Rust egress source](https://github.com/Soju06/codex-lb/tree/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/crates/codex-lb-egress) | Candidate selective library reuse after a compatibility spike |

The usage client includes a backend quota-read path, but that does not turn the
backend endpoint into a vendor-supported public API contract. Version it and test
failure behavior. Do not adopt unrelated quota-reset/credit-consuming operations
merely because the same module implements them.

## Routing difference that matters

codex-lb distinguishes soft session/thread locality from hard continuation
ownership. Soft locality can move when account availability changes. Owner-bound
continuations receive stronger protection, with some explicitly proven replay or
migration paths. See the pinned
[routing guide](https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/docs/routing.md)
and affinity policy above.

Poolparty's requirement is stricter: an established logical session retains its
provider/account binding even when its payload could technically be replayed
elsewhere. Exhaustion returns an error. Only the caller decides to create a new
session. Reusing the selection policy unchanged would violate that requirement.

## Options

| Approach | Benefit | Cost or mismatch |
| --- | --- | --- |
| Deploy codex-lb unchanged | Existing Codex account UI and proxy | Does not establish Poolparty's multi-provider and strict-session contract |
| Fork and extend it | Reuses the complete product and compatibility work | Inherits Python/React/Rust migration, account-centric schema, and policy divergence |
| Put it behind Poolparty | Fast way to experiment with Codex transport | Two selection authorities unless its account choice is explicitly constrained |
| Independent Rust application, selective reuse | Own the required policy and small deployment boundary | Must earn protocol compatibility through tests; more initial work |

Recommendation: independent application with codex-lb as a source-grounded
reference. Evaluate its egress library before rebuilding difficult transport
details. If adopted, pin an immutable revision, preserve required notices, and
ensure Poolparty remains the sole account selection and admission authority.
An independently deployed second proxy is not the initial architecture.

## License and evidence limits

The inspected revision carries an
[MIT license](https://github.com/Soju06/codex-lb/blob/5ad638b6a4c9c094bcc8866b1d7487173fe3b54e/LICENSE).
Any code adoption must retain the required upstream copyright and license notice
and record the exact source/revision. Audit transitive dependencies separately.
Poolparty's own MIT license is not a replacement for third-party notices.

This assessment establishes architectural fit, not production readiness or a
maintenance commitment from upstream. Recheck the chosen dependency boundary when
implementation begins; do not base a release on mutable `main`.
