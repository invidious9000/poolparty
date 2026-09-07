# Poolparty

A self-hosted account allocator and protocol router being built for Codex subscription
accounts, Kimi/GLM coding plans, and DeepSeek PAYG. MiniMax is a future adapter option.

Independent clients share account capacity and usage visibility through one
service. Sessions keep their provider/account binding; an exhausted resumed
session returns an error so its caller can wait or explicitly start a new session.

**Status: experimental Rust core and runnable synthetic demo.** Durable SQLite
bindings/admission, authenticated control and streaming routes, and one-attempt
HTTP transport foundations are implemented with local fixtures. The demo contacts
no providers. Live adapters, durable OAuth refresh, quota collection, dashboard,
control CLI and production deployment still have acceptance work outstanding.
Anthropic protocol compatibility does not include actual Anthropic provider
integration.

- [Build, test, run the demo and current limitations](docs/development.md)
- [Core module contracts and acceptance boundary](design/core-implementation.md)
- [Project scope and decisions](PROJECT.md)
- [Design index](design/README.md)
- [DD findings, executed spikes, and open gates](design/dd-review.md)
- [codex-lb reference assessment](research/codex-lb.md)
- [Contributor and public/private instructions](AGENTS.md)

Deployment-specific configuration belongs in a separate private overlay. Public
examples use synthetic identities and reserved domains.

Licensed under [MIT](LICENSE).
