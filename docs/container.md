# Container artifact and runtime contract

The public Dockerfile builds a `linux/amd64` image containing `poolpartyd`,
1Password CLI and CA certificates. Its default command is
`poolpartyd --serve /etc/poolparty/config.json`. Provider credentials, vault
references, grants and deployment configuration are supplied at runtime; no
operator configuration is built into the image.

## Build inputs and verification

| Input | Pin |
| --- | --- |
| Rust builder | `rust:1.96.0-slim-bookworm@sha256:4732ca96fd086cb9be682050c3f0176288eebaac2b80aa2bcefccfaf198e1950` |
| Runtime/distribution stage | `debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171` |
| 1Password CLI | `2.39.0`, official Linux amd64 ZIP |
| ZIP SHA-256 | `6fba7f376b6c6dec49f41b06408930a43ad064cce103c6a2ce5b3d0413a86434` |
| Signing-key fingerprint | `3FEF9748469ADBE15DA7CA80AC2D62742012EA22` |

The release build uses Rust 1.96.0 and `cargo build --release --locked --bin
poolpartyd`. The committed lockfile determines Rust dependency versions. Debian
package updates are resolved at build time; this is not a claim of bit-for-bit
reproducibility. Refresh base-image digests and the dependency/CLI pins through
reviewed changes.

The CLI comes directly from
`https://cache.agilebits.com/dist/1P/op2/pkg/v2.39.0/op_linux_amd64_v2.39.0.zip`.
The download stage checks its pinned SHA-256, extracts only `op` and `op.sig`,
and verifies the executable's detached signature using only the exported key
matching the pinned full fingerprint. The public key is fetched from 1Password's
official distribution host. The verified ZIP/signature were inspected when this
packaging was authored; the Dockerfile repeats verification on each uncached
download-stage build. See the vendor's [signature instructions](https://www.1password.dev/cli/verify),
[installation instructions](https://www.1password.dev/cli/get-started) and
[release notes](https://releases.1password.com/developers/cli/).

Only `poolpartyd`, `op` and Poolparty's license cross from the application/download
inputs into the runtime image. Build compilers, GPG and archive tools stay in build
stages. The 1Password binary remains governed by its vendor terms; Poolparty's MIT
license applies to Poolparty's own code.

The context allowlist admits manifests, the pinned toolchain, license and Rust
source files. It excludes `.git`, private/local configuration, native histories,
state databases, credential caches, tests and `target/`. When source embeds a new
non-Rust build input, review and explicitly extend the allowlist and `COPY` rules.

```sh
docker build --platform linux/amd64 --tag poolparty:review .
```

The Dockerfile uses ordinary multi-stage instructions and requires no BuildKit
mounts or frontend extensions. A native amd64 Kaniko 1.23.2 executor can build the
same root context. Other architectures currently fail explicitly before compilation
or CLI installation; an arm64 image needs its own CLI digest and validation.

The builder needs access to Docker Hub, Debian package mirrors, crates.io/Rust
distribution endpoints and the official 1Password distribution hosts. It needs no
runtime secrets. Run the workspace checks from [development](development.md)
separately; compiling the image does not run the test suite.

## Runtime files, identity and networking

The image runs as UID/GID `10001:10001`. The entrypoint sets a private umask,
creates owner-only temporary home/work directories, then replaces itself with the
daemon so SIGTERM reaches it directly.

| Path/configuration | Requirement |
| --- | --- |
| `/etc/poolparty/config.json` | Read-only mounted daemon JSON, readable by UID/GID 10001 |
| `inventory.op_executable` | `/usr/local/bin/op` |
| Durable volume mount | `/var/lib/poolparty`, writable by UID/GID 10001 |
| `inventory.state_dir` | `/var/lib/poolparty/state`, a private child directory |
| Writable ephemeral mount | `/tmp`, suitable for a tmpfs or emptyDir |
| Private home/temp children | `/tmp/poolparty-home` and `/tmp/poolparty-tmp`, mode 0700 |
| Listener for in-container ingress | Explicit `listen: "0.0.0.0:8080"` plus `trusted_ingress: true` |

Mount the durable volume at the parent directory so the nonroot daemon can create
its own `state` child with mode 0700. A volume root made group-writable by a
container orchestrator is not itself a valid private state directory. A host bind
mount must already allow UID/GID 10001 to create that child. The image does not
start as root or recursively change ownership of mounted data.

`/tmp` is writable even when the image root filesystem is read-only. The passwd
entry and image `HOME` identify the private temporary home. The 1Password adapter
clears inherited environment settings before spawning `op`, including `HOME`.
It creates a fresh mode-0700 configuration directory under the daemon's `TMPDIR`,
passes its path explicitly with `--config`, and disables the CLI cache with
`--cache=false`. This is required even for service-account calls; a passwd home
entry alone does not provide the CLI's configuration location under the cleared
environment. See [CLI isolation and cleanup](onepassword.md). Persistent provider
credentials remain in the vault, while database watermarks and refresh fences
remain on the durable state volume.

Supply `POOLPARTY_OP_SERVICE_ACCOUNT_TOKEN` and the configured
`POOLPARTY_GRANT_*` environment variables through runtime secret delivery.
Do not pass them as build arguments. The daemon's [JSON schema and grant
rules](daemon.md) apply unchanged. A non-loopback listener requires the explicit
trusted-ingress setting; the daemon serves HTTP behind the deployment's HTTPS
ingress and continues to authenticate callers itself.

Run one active daemon against the inventory and state. Prefer replacement without
overlapping writers, and retain the same state volume on restart. Never create a
second state directory to bypass ownership for the same credentials. Give shutdown
more than the daemon's 30-second drain deadline. A timeout or uncertain request
does not authorize deleting markers or releasing pressure.

## Artifact and deployment checks

Before publishing/deploying an image, verify its architecture and runtime user,
`poolpartyd --help`, `/usr/local/bin/op --version`, and operation with a read-only
root filesystem plus writable state and `/tmp`. Confirm that no build context
secrets or private files entered image layers. Pin deployments to the resulting
image digest rather than a mutable development tag.

Use the synthetic demo for lifecycle tests without provider access. Its loopback
listener can be exercised inside the container's network namespace; publishing a
host port does not make a loopback-only in-container socket externally reachable.
For `--serve`, verify unauthenticated `/healthz`, rejection of ungranted account
queries, and scoped account access before any explicit inference probe. Liveness
does not prove provider eligibility. Live vault/refresh checks require the
deployment's explicitly authorized inventory and private evidence handling.

Kubernetes resources, ingress identities, secret delivery, volume provisioning,
registry locations and actual runtime configuration belong in the private overlay.
This public packaging does not deploy or mutate those resources.
