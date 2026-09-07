# Native Codex vertical check

`spikes/codex/vertical.py` exercises an installed native Codex process against an
already-running Poolparty router. It requires explicit `--live` and a scoped
`POOLPARTY_NATIVE_GRANT`. With no `--live`, it prints help and performs no network
or native inference. The fixture never obtains provider OAuth credentials; those
remain with the router.

The currently qualified CLI shape is `codex-cli 0.153.4`. The installed CLI's
`exec`/`exec resume` help and its [pinned event definitions](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/exec/src/exec_events.rs)
establish the commands and assertions. A different version fails before inference
until its fixture compatibility is reviewed. A separately authorized live run has
now passed start, tool execution and resume through daemon restart and credential
rotation with the same thread and binding. Eight upstream attempts succeeded,
four in each phase. This qualifies the bounded HTTP fixture, while broader native
and provider parity remain independent gates. See
[current validation](development.md#current-validation-evidence).

## Start and resume

Supply the caller grant through the operator's secret-delivery mechanism. Use an
empty owner-only state directory outside git checkouts. Substitute an enrolled
pool/account/model and the running router origin in this synthetic example:

```sh
python3 spikes/codex/vertical.py --live \
  --router https://router.example.com \
  --state-dir /path/to/private/native-check \
  --pool pool-a --account account-a --model example-model --effort low
```

This creates a binding explicitly selecting `codex_subscription` and the supplied
account/model/effort. It runs a native shell-tool turn, exits that process, then
resumes the native thread in a second process using the same router binding.
`--binding binding-a` can select an existing open binding instead; its model and
effort must match the explicit arguments. A binding with no effort pin does not
match the fixture's default `low` pin.

To insert a router restart, run the two phases separately:

```sh
python3 spikes/codex/vertical.py --live --phase start \
  --router https://router.example.com \
  --state-dir /path/to/private/native-check \
  --pool pool-a --account account-a --model example-model --effort low

# The operator restarts the router against its original durable state here.

python3 spikes/codex/vertical.py --live --phase resume \
  --router https://router.example.com \
  --state-dir /path/to/private/native-check
```

The script does not restart services or execute an external restart command.
The resume phase reloads the original native home, thread and bound provider
configuration. It compares the router's entire binding record with the initial
snapshot, including account, intent and creation identity. The caller grant must
still authorize that binding after the restart.

## Native provider configuration

The fixture writes a protected configuration inside its own `CODEX_HOME` using
the bound Responses base URL and an environment-based caller grant:

```toml
model = "example-model"
model_provider = "poolparty"
model_reasoning_effort = "low"
cli_auth_credentials_store = "file"
approval_policy = "never"
sandbox_mode = "workspace-write"
web_search = "disabled"
check_for_update_on_startup = false

[model_providers.poolparty]
name = "Poolparty"
base_url = "https://router.example.com/routes/binding-a/codex"
env_key = "POOLPARTY_GRANT"
requires_openai_auth = false
wire_api = "responses"
supports_websockets = false
request_max_retries = 0
stream_max_retries = 0

[features]
unbounded_connection_retries = false
```

The additional generated sandbox/shell policy disables shell network access and
inherited shell environment. The native process receives the scoped caller grant;
its shell tools receive only a small configured executable search path. `HOME`,
`CODEX_HOME` and XDG paths are isolated; provider API keys, OAuth caches, service
account tokens, proxies and shell startup options are not inherited. The
[configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
documents these native provider and environment controls.

WebSockets remain disabled because the earlier [native carrier spike](../research/codex-spike.md)
observed WebSocket-to-HTTP resubmission despite zero retry counts. This fixture
does not claim native exactly-once behavior. Native tools require multiple valid
inference requests; lack of an operation ID does not identify which calls are
retries. Router uncertainty fencing and its one-attempt transport remain necessary.

## Assertions, state and failures

The first turn reads a synthetic nonce from `seed.txt` using a shell tool and
creates `proof.txt` with that nonce and a `started` marker. The resumed turn reads
the artifact and appends `resumed`. Both phases require a successful native shell
event, exactly one completed native turn and exact artifact contents. Resume also
requires the same native thread ID and unchanged router binding. The script never
executes the model's requested commands itself.

Success output contains version, counts and boolean checks. Raw native output is
bounded and omitted from stdout; errors use fixed diagnostic codes. `--keep-output`
retains bounded raw stdout/stderr files only inside the private state directory
for operator debugging. Native Codex's own conversation/history files also remain
there because process resume requires them. Do not commit or publish those files,
the manifest, the configured URLs or any operator logs.

Each native process has a bounded deadline (`--timeout`, default 180 seconds,
maximum 600). A timeout or failed phase is not automatically retried. The manifest
marks an in-progress phase before dispatch and rejects blind re-execution; inspect
the router's durable attempt/session state before choosing recovery. A completed
resume also cannot be rerun in the same fixture directory. The script leaves its
binding and private state available for inspection and does not clear uncertainty.

Offline tests use a fake native executable and local synthetic control server:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 spikes/codex/test_vertical.py
```

They validate fixture wiring, isolation, artifacts, binding/thread comparisons,
safe defaults and refusal to replay. They do not run installed Codex, perform
inference or replace the live vertical check.
