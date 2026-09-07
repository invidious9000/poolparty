# Credential maintenance and explicit provider checks

These commands are experimental, one-shot operator operations. They use the real
credential store and configured provider endpoints, then exit. The separate
[`--serve` daemon](daemon.md) maintains enrolled inventory and routes authenticated
HTTP/SSE requests continuously. The `--demo` server remains synthetic.

## Commands and side effects

Build `poolpartyd` using [the development commands](development.md). Supply
`POOLPARTY_OP_SERVICE_ACCOUNT_TOKEN` through the operator's secret-delivery
mechanism. It must grant the enrolled vault operations; its value does not belong
in the configuration file or command arguments.

```sh
./target/debug/poolpartyd --check /path/to/operator.json
./target/debug/poolpartyd --probe /path/to/operator.json
./target/debug/poolpartyd --refresh /path/to/operator.json credential-a
```

| Mode | Behavior |
| --- | --- |
| `--check` | Resolve enrolled credentials, refresh Codex when expiry is unknown or within five minutes, then collect configured usage. No inference request. |
| `--probe` | Perform the same maintenance and usage checks, then attempt each explicitly configured probe once through binding/admission and the HTTP transport. |
| `--refresh` | Force one enrolled Codex credential's refresh, then collect usage for its configured account aliases. No inference request. |

All modes acquire exclusive state-directory ownership, run ledger recovery and
update credential generation/account/policy/observation state. **`--check` is not a
read-only or dry-run command:** refreshing Codex can change the vaulted bundle.
An API key has no OAuth refresh operation. Credential aliases share one resolution
result within an invocation, including failures; an alias does not retry issuance.
One-shot commands use fresh vault reads. The persistent service instead shares a
bounded in-memory credential cache and store-wide failure backoff, described in
[1Password storage](onepassword.md). Neither mode uses a disk credential cache.

Reports contain configured account labels, products, generations, capacity states,
window counts and error codes. Probe reports add binding/attempt IDs, HTTP status,
content type, byte count and durable attempt state. They do not include provider response text,
tokens, upstream account IDs or wallet amounts. Configured labels can themselves
be private, so keep reports with the operator's private state. A reported credential,
collection or probe failure, including authentication-required capacity, produces
a nonzero exit status.

## Operator configuration

Keep the JSON outside the public checkout. Unknown configuration fields are
rejected. The example below uses synthetic identities and reserved endpoint names;
replace them with explicitly enrolled IDs and verified product endpoints before
running any command.

```json
{
  "state_dir": "/home/operator/.local/state/poolparty",
  "op_executable": "/usr/local/bin/op",
  "credentials": [
    {
      "credential": "credential-a",
      "vault": "example-vault-id",
      "item": "example-item-id",
      "field": "example-field-id"
    }
  ],
  "accounts": [
    {
      "id": "account-a",
      "product": "codex_subscription",
      "quota_owner": "owner-a",
      "pool": "pool-a",
      "credential": "credential-a",
      "model": "example-codex-model",
      "expected_account_id": "example-upstream-account-id",
      "max_concurrency": 1,
      "unknown_capacity": "reject",
      "usage_url": "https://usage.example.com/account",
      "inference_url": "https://inference.example.com/responses",
      "probe": {
        "model": "example-codex-model",
        "stream": true,
        "store": false,
        "instructions": "Reply briefly.",
        "input": "Reply with OK."
      }
    }
  ],
  "oauth": {
    "endpoint": "https://auth.example.com/token",
    "client_id": "example-enrolled-client-id"
  }
}
```

The `op_executable` must be an absolute trusted executable path. Credential
references are stable vault/item/field IDs, with one mapped credential per item;
see [1Password storage](onepassword.md). Codex fields contain an auth JSON bundle;
API-key fields contain the key string. `expected_account_id` is required for
Codex and must match the enrolled bundle. It is unrelated to the internal `id` or
`quota_owner` label.

Accounts sharing a credential must agree on product, upstream account identity
and quota owner. Accounts sharing a quota owner must agree on concurrency and
unknown-capacity policy. `max_concurrency` is a local cap, not a discovered provider
limit. `unknown_capacity` accepts `reject` or `allow_under_local_cap`; the latter
does not establish budget or entitlement.

Usage and inference URLs are full endpoints, not base URLs. They must use HTTPS;
redirects, automatic retries and inherited proxies are disabled. Accounts using
the same product must configure matching endpoints. The current schema requires
an `oauth` object even for an API-key-only inventory. Omit `probe` or set it to
`null` to avoid inference for that account in `--probe`; other maintenance still
runs. A missing usage endpoint produces an unsupported collection result.

Only small synthetic inference requests belong in `probe`. Its model must match
the enrolled model and its native protocol must be supported. Admission may reject
it before dispatch. The probe bounds streamed output to 256 KiB and draining to
60 seconds; ambiguity retains pressure rather than authorizing replay. Re-running
`--probe` creates a new session/operation, not an idempotent retry of a prior probe.

## Refresh custody and recovery

Poolparty's direct OAuth refresher is an experimental implementation of an
observed protocol. It is not a documented vendor-supported generic OAuth API.
OpenAI's [CI/CD account-auth guide](https://learn.chatgpt.com/docs/auth/ci-cd-auth)
describes running Codex and persisting the auth cache that Codex refreshes.
Its [authentication guide](https://learn.chatgpt.com/docs/auth) documents native
enrollment/cache behavior. Those documented workflows do not establish support
for Poolparty's independent refresh client or account pooling.

Keep the private state directory, SQLite/WAL files, lock file and
`refresh-<credential-id>.pending.json` markers together. The 1Password item holds
the credential; SQLite holds a durable generation watermark. That watermark
rejects older observed generations across process restarts. Restoring old state
can lose newer watermarks or fences, so backup/restore reconciliation remains a
deployment gate.

Poolparty serializes each credential's maintenance and checks the explicitly
enrolled account identity. A pending marker is created and synced before issuing
an OAuth refresh. The marker contains references/digests, not token values. The
new bundle is checked, written to 1Password and read back; its generation is
recorded durably before the marker is removed. A rotated refresh token can be
preserved even when the returned access token is unusable, while the fence remains.

**Any existing pending marker blocks later use.** A newer vault generation does
not automatically reconcile the pending exchange. An interrupted request, failed
writeback/readback or identity conflict can require an operator to establish which
bundle is current or re-enroll the account. This slice provides no automatic
reconciliation or marker-clearing command. Do not delete a marker merely to make
a retry proceed. Preserve the state and reconcile the current bundle, identity
and generation before releasing custody through a reviewed recovery procedure.

The backend requires one writer across all processes, including direct CLI use
and manual vault edits. A state-directory lock coordinates processes using that
directory; it does not prevent another directory or independent vault client from
writing the same item. 1Password CLI prechecks/readback are conflict detection,
not distributed compare-and-set.

## Usage interpretation and remaining gates

Codex collection preserves base and auxiliary windows, exact used percentages and
reset timestamps. Auxiliary feature exhaustion does not establish exhausted base
generation capacity. Model restrictions that this admission model cannot express
leave capacity unknown. GLM preserves five-hour and weekly generation rows plus
tool/other rows; tool exhaustion does not exhaust generation capacity. Unknown
fields and scopes remain unknown.

DeepSeek preserves each currency's total, granted and topped-up decimal amounts
and the provider's `is_available` signal. A funded wallet remains unknown for
admission purposes; provider-reported insufficient funds can establish exhaustion.
These observations do not implement spending ceilings, cost reservations or
model-specific concurrency. Successful observations expire after 60 seconds.
HTTP 401/403 produces an authentication-required observation; 429 is a collection
failure and never invented quota exhaustion.

Kimi currently returns unsupported for account-usage collection. The existing
transport foundation does not remove that collection gate. The daemon now runs
periodic maintenance and prepares credentials/usage before request admission.
Native Codex HTTP start/tools/resume passed across restart and credential rotation;
see [current validation](development.md#current-validation-evidence).

WebSockets, compaction and broader native/provider conformance remain separate
gates, alongside Kimi collection, complete PAYG admission, OIDC/workload grants,
deployment, explicit reconciliation tooling and backup recovery. The successful
native slice does not authorize automatic retry or clearing unresolved state.
