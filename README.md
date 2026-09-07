# Poolparty

A self-hosted account allocator and protocol router being built for Codex subscription
accounts, Kimi/GLM coding plans, and DeepSeek PAYG. MiniMax is a future adapter option.

Independent clients share account capacity and usage visibility through one
service. Sessions keep their provider/account binding; an exhausted resumed
session returns an error so its caller can wait or explicitly start a new session.

**Status: experimental persistent daemon with a verified Codex HTTP slice.**
`poolpartyd --serve` provides durable SQLite bindings/admission, authenticated
control and streaming routes, account/usage visibility, managed 1Password
credentials, fenced Codex refresh and periodic usage collection. Native Codex
start, tools and resume have passed through a daemon restart and credential
rotation with the same binding. A small Python control helper accompanies the
HTTP API. The synthetic demo and explicit one-shot maintenance/probe commands
remain available.

WebSockets, compaction, complete provider conformance, OIDC, dashboard and
production deployment remain separate acceptance gates.
Anthropic protocol compatibility does not include actual Anthropic provider
integration.

- [Build, test, run the demo and current limitations](docs/development.md)
- [Run the authenticated daemon and control helper](docs/daemon.md)
- [Container build and runtime contract](docs/container.md)
- [Native Codex start/resume validation](docs/native-codex.md)
- [Credential maintenance, usage checks and explicit probes](docs/credential-maintenance.md)
- [1Password storage and exclusive writer requirements](docs/onepassword.md)
- [Core module contracts and acceptance boundary](design/core-implementation.md)
- [Project scope and decisions](PROJECT.md)
- [Design index](design/README.md)
- [DD findings, executed spikes, and open gates](design/dd-review.md)
- [codex-lb reference assessment](research/codex-lb.md)
- [Contributor and public/private instructions](AGENTS.md)

Deployment-specific configuration belongs in a separate private overlay. Public
examples use synthetic identities and reserved domains.

Licensed under [MIT](LICENSE).
