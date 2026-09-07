# Design index

These documents capture a proposed implementation. Statements marked as
requirements come from the product scope; proposed endpoint names and component
choices are not a released API. Provider evidence was reviewed on 2026-09-06.

Start with the [DD review](dd-review.md) for current findings and open gates.
Then read in this order:

1. [Architecture](architecture.md): runtime, ownership, storage, alternatives.
2. [Contracts](contracts.md): allocation, hard session binding, protocol behavior.
3. [Providers and usage](providers-and-usage.md): account adapters and quota model.
4. [Deployment and access](deployment-and-access.md): services, ingress, auth.
5. [Shared platform integration](platform-integration.md): LGTM telemetry,
   Homepage annotations, dashboard provisioning, and network admission.
6. [Consumers and CLI](consumers-and-cli.md): integration shapes and diagnostics.
7. [Delivery plan](delivery-plan.md): acceptance cases and unresolved choices.
8. [Admission](admission.md): concurrency scopes, atomic claims and uncertainty.

[codex-lb assessment](../research/codex-lb.md) records the reference revision and
reuse recommendation. Public examples are synthetic. Estate-specific configuration
and private consumer mappings are intentionally maintained outside this repository.
