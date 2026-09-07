# Poolparty

A planned self-hosted account allocator and protocol router for Codex subscription
accounts and providers such as Kimi, GLM, and MiniMax.

Independent clients share account capacity and usage visibility through one
service. Sessions keep their provider/account binding; an exhausted resumed
session returns an error so its caller can wait or explicitly start a new session.

**Status: design only.** There is no runnable daemon, dashboard, CLI, or deployment
yet. The proposed implementation is a Rust daemon with an embedded web UI, a
versioned control API, and native Responses and Anthropic Messages routing.
Anthropic protocol compatibility does not include actual Anthropic provider
integration.

- [Project scope and decisions](PROJECT.md)
- [Design index](design/README.md)
- [codex-lb reference assessment](research/codex-lb.md)
- [Contributor and public/private instructions](AGENTS.md)

Deployment-specific configuration belongs in a separate private overlay. Public
examples use synthetic identities and reserved domains.

Licensed under [MIT](LICENSE).
