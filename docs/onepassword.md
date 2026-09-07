# 1Password credential storage

The CLI adapter maps each logical credential to stable vault, item and field IDs.
Titles and field labels never select a credential. Each item holds one mapped
credential because the item version is the credential generation. Changes to any
field advance that generation. `latest` obtains the current version and value;
`load` requires the exact version requested by an admitted attempt.

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

`replace` serializes access, reads and checks the expected item version, edits the
complete item through stdin, and verifies both the edit result and a fresh read.
It checks the next version, new value and preservation of other item content.
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
