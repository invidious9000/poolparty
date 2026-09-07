# Running the authenticated daemon

`poolpartyd --serve CONFIG.json` runs the persistent HTTP/SSE router against an
explicitly enrolled inventory. Startup acquires exclusive state ownership, fences
interrupted inference, checks credentials and collects usage before accepting
requests. Background maintenance and request preparation keep credentials and
usage current. Actual provider entitlement and native-client compatibility still
require the relevant adapter checks.

## Configuration and grants

The `inventory` object uses the same schema as the one-shot
[credential maintenance commands](credential-maintenance.md). Keep the entire
configuration outside this public checkout. These values are synthetic examples:

```json
{
  "inventory": {
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
        "inference_url": "https://inference.example.com/responses"
      }
    ],
    "oauth": {
      "endpoint": "https://auth.example.com/token",
      "client_id": "example-enrolled-client-id"
    }
  },
  "listen": "127.0.0.1:8080",
  "grants": [
    {
      "principal": "consumer-a",
      "pools": ["pool-a"],
      "token_env": "POOLPARTY_GRANT_CONSUMER_A"
    }
  ],
  "maintenance_interval_seconds": 30
}
```

Supply `POOLPARTY_OP_SERVICE_ACCOUNT_TOKEN` through the operator's secret delivery
mechanism. Each caller grant comes from its named `POOLPARTY_GRANT_*` variable and
must contain at least 32 visible ASCII characters. Provider tokens remain in the
vault; neither a grant nor the configuration carries them to callers. Caller and
vault service tokens must be different. Multiple caller tokens may represent the
same principal, but each has its own explicit pool scope.

The listener defaults to `127.0.0.1:8080`. Binding a non-loopback address requires
`"trusted_ingress": true`, which records the operator's trusted ingress setup.
The daemon serves plain HTTP, so configure HTTPS at that ingress and restrict
direct socket access accordingly. Bearer authentication remains mandatory on all
control and inference routes. There is no OIDC implementation in this static-grant
surface. Grant changes require restarting the process.

```sh
poolpartyd --serve /path/to/private-server.json
```

The daemon does not run configured `probe` requests. Only explicit `--probe`
invocations send those requests. Maintenance reads usage and may refresh OAuth;
starting the daemon can therefore update the credential vault.

## Bind, call and resume

The Python standard-library helper reads `POOLPARTY_URL` (default
`http://127.0.0.1:8080`) and the caller's `POOLPARTY_GRANT` from the environment.
It accepts no token argument, disables redirects and ambient proxies, performs
one request and prints JSON. It requires Python 3 and can be installed on PATH
as `poolparty`.

```sh
bin/poolparty accounts
bin/poolparty session create --session session-a --pool pool-a \
  --product codex_subscription --model example-codex-model
bin/poolparty session inspect BINDING_ID
bin/poolparty attempt inspect ATTEMPT_ID
bin/poolparty session close BINDING_ID
```

`GET /api/v1/accounts` reports accounts visible through the caller's pools,
including disabled accounts. Its response includes models, enabled state,
logical quota owner, credential generation and the last usage observation.
Pool memberships are restricted to those authorized for the caller. The response
omits credential IDs, vault references and secret values. A null usage observation
means no evidence is available; callers should inspect observation timestamps
before treating older evidence as current.

Create a binding with the caller grant, for example:

```sh
curl --fail-with-body http://127.0.0.1:8080/api/v1/sessions \
  -H "Authorization: Bearer $POOLPARTY_GRANT_CONSUMER_A" \
  -H 'Content-Type: application/json' \
  --data '{"session":"session-a","pool":"pool-a","product":"codex_subscription","model":"example-codex-model"}'
```

Save the returned binding ID in the caller's own durable session mapping. Native
Codex's configured base URL is then
`http://127.0.0.1:8080/routes/BINDING_ID/codex`, with the caller grant as its bearer.
The caller supplies the same binding after a process or daemon restart. Use the
HTTP-only native configuration and retry restrictions from the
[Codex compatibility spike](../research/codex-spike.md); default native retries do
not establish safe replay behavior.

The same binding exposes:

| Operation | Route |
| --- | --- |
| Account/usage status | `GET /api/v1/accounts` |
| Inspect | `GET /api/v1/sessions/BINDING_ID` |
| Close | `POST /api/v1/sessions/BINDING_ID/close` |
| Responses stream | `POST /routes/BINDING_ID/codex/responses` |
| Messages stream | `POST /routes/BINDING_ID/v1/messages` |
| Attempt outcome | `GET /api/v1/attempts/ATTEMPT_ID` |

The protocol must match the bound product. Custom callers should provide a unique
`x-poolparty-operation-id` for each intended inference operation. A repeated ID
returns the stored conflict/outcome information and never replays inference.
The response's `x-poolparty-attempt-id` correlates its durable state. Binding
possession never replaces principal and pool authorization.

`GET /healthz` is an unauthenticated, nonsecret process-liveness response. Account
eligibility is established by admission and usage observations, not by liveness.
Exhaustion, credential faults and uncertain attempts preserve the binding; the
caller chooses whether to wait or create a new logical session.

## Maintenance and shutdown

The default background interval is 30 seconds; configuration accepts 5 through
3600 seconds. Request preparation also checks the enrolled account before
admission, using the manager's bounded freshness cache. Refresh aliases share one
credential authority. Startup synchronization visits the enrolled accounts before
serving. Upstream credential or collection failures are reported as sanitized
codes while healthy enrollment remains available and affected credentials stay
fenced. Invalid configuration or storage failures stop startup. Later maintenance
failures likewise leave request preparation and admission responsible for health.

The state directory must remain private and durable. The same directory is
exclusive across `--serve`, `--check`, `--probe` and `--refresh`; those commands
cannot concurrently operate against a running daemon's state. Never use another
directory to bypass the owner lock for the same provider credentials.

SIGINT and SIGTERM stop new HTTP acceptance and request a graceful drain. HTTP
requests and background maintenance share a 30-second drain deadline. Exceeding
that bound exits with an error; startup recovery preserves uncertainty for
possibly dispatched work. Runtime and refresh tasks retain state ownership until
they finish or the process exits. Keep the same persistent directory on restart.

A pending credential refresh marker requires explicit reconciliation. Neither a
restart nor a newer vault item version clears an ambiguous issuance or failed
preservation check automatically. Follow [credential custody and recovery](credential-maintenance.md)
before changing such state. Closing a session never releases uncertain upstream
pressure by itself.
