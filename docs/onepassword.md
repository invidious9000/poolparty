# 1Password credential storage

The CLI adapter maps each logical credential to stable vault, item and field IDs.
Titles and field labels never select a credential. Each item holds one mapped
credential because the item version is the credential generation. Changes to any
field advance that generation. `latest` obtains the latest observed version and
value; `load` requires the exact version requested by an admitted attempt.

The service account token enters only the CLI child's environment. Credential
values travel through captured output and JSON on stdin, never arguments or
temporary files. Child environments exclude inherited 1Password settings, proxy
credentials and provider credentials. Output sizes and execution time are bounded;
subprocess errors produce sanitized errors. The executable path is trusted
operator configuration, and must be absolute.

Each subprocess receives a fresh owner-only temporary configuration directory
through `--config`, including service-account calls. The adapter does not pass
`HOME`, `XDG_CONFIG_HOME` or an inherited `OP_CONFIG_DIR` to the child. The directory
is created beneath the daemon's temporary directory (normally `TMPDIR` on Unix)
with mode 0700 and removed when the invocation returns or is cancelled. It can
contain CLI-managed configuration metadata; credential payloads remain on the
stdin/stdout channel. `--cache=false` disables the CLI's background caching daemon
so it cannot retain this configuration directory after the invocation. Timeouts
kill and reap the subprocess before cleanup; cancellation retains the subprocess's
kill-on-drop behavior. Abrupt daemon termination can leave temporary files for
the deployment's ephemeral-volume lifecycle to remove. The vendor documents
[the configuration directory and cache flags](https://www.1password.dev/cli/reference)
and [their environment-variable counterparts](https://www.1password.dev/cli/environment-variables).

The persistent service caches each credential's value and generation in process
memory for at most one hour after a verified read. Repeated resolution, usage and
inference share that entry; access does not extend its expiry. Expired entries
require a fresh read and are never served on failure. The default store and
one-shot maintenance commands keep fresh-read behavior. This cache is separate
from the disabled CLI cache; it writes no credential payload to disk.

CLI failures invalidate the affected entry and stop all store reads and
replacements for 15 minutes, including otherwise valid cache hits. Invalid item
content, generation rollback and semantic replacement conflicts instead fence
only the affected credential for 15 minutes, preserving healthy accounts. After that deadline, a fresh successful
vault read is required before cached service resumes. This shared backoff prevents
account fanout and inference traffic from repeatedly hitting an unavailable vault,
and prevents known vault failure from permitting an OAuth exchange whose writeback
cannot proceed. Restart clears both cache and backoff, so repeated restarts are
not a rate-limit recovery procedure. Pending refresh fences remain durable.

The one-hour interval is also the maximum normal delay before discovering a
vault-only edit, deletion or service-account grant revocation. The manager's more
frequent expiry/usage checks do not establish a fresh vault read. Provider-side
revocation can still reject the next upstream request. For immediate removal,
disable the account enrollment or caller grant and perform a controlled restart;
do not rely on editing the vault while the owner is active. A restarted process
requires fresh credentials and does not restore cached secrets from its ledger.

Size enrollment and operational reads against the account's shared
[service-account rate limits](https://www.1password.dev/service-accounts/rate-limits).
Daily allowances cover all service accounts in the account, and some CLI commands
use multiple API requests. `op service-account ratelimit --format json`, with the
service token supplied through the environment, reports the applicable remaining
allowances and reset intervals. Usage checks against provider APIs retain their
own freshness cadence; they do not need a new vault read for every observation.

`replace` invalidates the cached entry, serializes access, freshly reads and checks the expected item version, edits the
complete item through stdin, and verifies both the edit result and a fresh read.
Only complete successful verification repopulates the cache. It checks the next
version, new value and preservation of other item content.
Empty DATE fields are omitted from edits to avoid a zero-date round trip. Missing,
null and empty ordinary field values are equivalent during preservation checks;
empty DATE fields may be absent. Populated dates remain unchanged. Version and
update timestamp and server-owned `last_edited_by` audit metadata are expected
to change. Item and field IDs, types, labels,
sections, tags and other metadata remain part of the comparison.

Use credential-only items without passkeys or attachments. The adapter refuses
detected unsupported content before writing. The [CLI item documentation](https://www.1password.dev/cli/reference/management-commands/item)
describes stdin JSON editing and warns that JSON template edits overwrite passkeys.

This backend supports **one writer**. The integrating process holds exclusive
ownership; operators and other processes must not edit its credential items during
rotation. CLI item editing does not provide a conditional write primitive. The
precheck and readback detect many conflicts but cannot prevent an independent
writer from racing or an edit from overwriting that writer's change. Do not treat
these checks as distributed compare-and-set.

A timeout or failed readback after edit can mean that the write succeeded. There
is no automatic retry or rollback. Re-read and reconcile the current credential
before continuing; a previously admitted generation fails explicitly. In-process
observed generation decreases are rejected. Persistent generation history and
startup reconciliation belong to the integrating credential manager.

Tests use a synthetic executable and temporary fixtures. They make no live vault
calls and do not establish conformance for a particular installed CLI version.
