# Poolparty

A planned self-hosted account allocator and protocol router for Codex subscription
accounts, Kimi/GLM coding plans, and DeepSeek PAYG. MiniMax is a future adapter option.

Independent clients share account capacity and usage visibility through one
service. Sessions keep their provider/account binding; an exhausted resumed
session returns an error so its caller can wait or explicitly start a new session.

**Status: design and compatibility spikes.** There is no runnable daemon,
dashboard, control CLI, or deployment yet. The proposed implementation is a Rust
daemon with an embedded web UI, a versioned control API, and native Responses
and Anthropic Messages routing.
Anthropic protocol compatibility does not include actual Anthropic provider
integration.

- [Project scope and decisions](PROJECT.md)
- [Design index](design/README.md)
- [DD findings, executed spikes, and open gates](design/dd-review.md)
- [codex-lb reference assessment](research/codex-lb.md)
- [Contributor and public/private instructions](AGENTS.md)

Deployment-specific configuration belongs in a separate private overlay. Public
examples use synthetic identities and reserved domains.

Licensed under [MIT](LICENSE).
