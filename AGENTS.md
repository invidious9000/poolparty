# Poolparty contributor instructions

Read [PROJECT.md](PROJECT.md) and the relevant design documents before changing
behavior. This is a standalone public project. Design proposals describe intended
behavior, not implemented features.

## Public repository boundary

Every committed byte, commit message, issue, pull request, screenshot, fixture,
and CI log must be suitable for unrestricted public disclosure.

- Public: Poolparty source and contracts, published provider documentation,
  public upstream references with attribution, generic deployment templates,
  synthetic examples, and independently described integration requirements.
- Private: customer/company identifiers, private repository names and paths,
  consumer implementation symbols, internal domains and addresses, cluster
  topology and inventory, identity-provider realm/client/group registrations,
  account identities, vault item references, credentials, authentication caches,
  transcripts, prompts, production logs, and production configuration.
- Use neutral examples such as `consumer-a`, `account-a`, `example.com`, and
  `poolparty.svc.cluster.local`. Replace the surrounding context too; changing
  only a company name does not sanitize a private excerpt.
- Keep deployment-specific values and consumer mappings in a separate private
  overlay repository or operator-local files outside this checkout. Ignored files
  are not a security boundary and must never be force-added.
- Before staging, inspect the exact files. Before committing, inspect the staged
  diff and filenames for both secrets and identifying details. Before pushing,
  inspect all outgoing commits, including their messages. Secret scanners cannot
  recognize every private identifier. Do not publish an unsanitized blob and rely
  on a later deletion to repair history.
- If disclosure status is uncertain, omit or generalize the material and keep
  making progress. Ask the owner only when the actual private detail is necessary.
- Reading a private consumer for requirements does not authorize copying its code
  or documentation here. Express requirements independently with synthetic cases.

## Product invariants

- Support Codex subscription accounts and providers such as Kimi, GLM, and MiniMax.
  Anthropic Messages is a supported protocol direction; actual Anthropic/Claude
  provider integration, subscription OAuth, Fable tracking, and its probes are
  excluded from this project scope.
- A session's provider/account binding is durable and strict. Exhaustion returns
  an error while preserving the binding. Only the caller chooses to wait or start
  a new session. No implicit account migration or replay after ambiguous dispatch.
- Session affinity outlives request admission leases. Credential refresh preserves
  account identity. Missing resume state fails explicitly.
- Preserve native streaming, tool and reasoning content, continuation references,
  and cache behavior. Do not silently downgrade a hard model/capability/effort pin.
- Unknown, stale, exhausted, unauthorized, and unsupported are different states.
  Local request counters do not establish authoritative provider quota remaining.
- Provider secrets stay separate from caller authentication. Internal network
  location alone never grants access.

## Working practices

- Change only this task's files. Never blanket-stage, stash, reset, or discard
  another contributor's work. Inspect git status before scoped mutations.
- Do not mutate shared services, deployment state, or production credentials
  without explicit authorization for the named operation.
- Keep root instructions conceptual. Put deep contracts in `design/`, current
  operating instructions in `docs/` when implementation exists, and public
  reference analysis in `research/` with immutable source revisions.
- Mark requirements, proposals, observed upstream behavior, and open questions
  distinctly. Verify provider-specific claims against primary sources.
- Track provenance and licenses before adopting third-party code. Preserve
  required notices. Referencing an upstream does not mean its code is included.
- Never add AI attribution or use em dashes in project prose or commit messages.
- During the design-only phase, validate links, examples, privacy, and git diffs.
  Do not invent build/test success for a daemon that does not yet exist.

`AGENTS.md` is canonical; `CLAUDE.md` is only a pointer to it.
