# Ordinary multi-stage syntax, including compatibility with Kaniko 1.23.2.
# Initial artifact contract: linux/amd64. Other architectures fail explicitly.
FROM rust:1.96.0-slim-bookworm@sha256:4732ca96fd086cb9be682050c3f0176288eebaac2b80aa2bcefccfaf198e1950 AS build

RUN test "$(dpkg --print-architecture)" = amd64 \
    && apt-get update \
    && apt-get install --no-install-recommends -y build-essential ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src/ ./src/
RUN cargo build --release --locked --bin poolpartyd \
    && strip target/release/poolpartyd

FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171 AS op-download

RUN test "$(dpkg --print-architecture)" = amd64 \
    && apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates curl gnupg unzip \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /download
# The ZIP hash fixes this reviewed distribution artifact. Signature verification
# separately authenticates the executable against the documented primary key.
RUN curl --fail --show-error --silent --location --proto '=https' --tlsv1.2 \
        https://cache.agilebits.com/dist/1P/op2/pkg/v2.39.0/op_linux_amd64_v2.39.0.zip \
        --output op.zip \
    && printf '%s  %s\n' 6fba7f376b6c6dec49f41b06408930a43ad064cce103c6a2ce5b3d0413a86434 op.zip | sha256sum --check - \
    && unzip op.zip op op.sig \
    && curl --fail --show-error --silent --location --proto '=https' --tlsv1.2 \
        https://downloads.1password.com/linux/keys/1password.asc --output signing-key.asc \
    && mkdir -m 0700 /download/gnupg \
    && gpg --batch --homedir /download/gnupg --import signing-key.asc \
    && gpg --batch --homedir /download/gnupg --export 3FEF9748469ADBE15DA7CA80AC2D62742012EA22 > signing-key.gpg \
    && test -s signing-key.gpg \
    && gpgv --keyring /download/signing-key.gpg op.sig op \
    && chmod 0755 op \
    && test "$(./op --version)" = 2.39.0

FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171 AS runtime

RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates libgcc-s1 passwd \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 poolparty \
    && useradd --uid 10001 --gid 10001 --no-create-home \
        --home-dir /tmp/poolparty-home --shell /usr/sbin/nologin poolparty \
    && install -d -o 10001 -g 10001 -m 0700 \
        /var/lib/poolparty /var/lib/poolparty/state /tmp/poolparty-home /tmp/poolparty-tmp \
    && install -d -m 0755 /etc/poolparty \
    && printf '%s\n' '#!/bin/sh' 'set -eu' 'umask 077' \
        'mkdir -p "$HOME" "$TMPDIR"' 'chmod 700 "$HOME" "$TMPDIR"' \
        'exec /usr/local/bin/poolpartyd "$@"' > /usr/local/bin/poolparty-entrypoint \
    && chmod 0755 /usr/local/bin/poolparty-entrypoint

COPY --from=build /build/target/release/poolpartyd /usr/local/bin/poolpartyd
COPY --from=op-download /download/op /usr/local/bin/op
COPY LICENSE /usr/share/doc/poolparty/LICENSE

ENV HOME=/tmp/poolparty-home \
    TMPDIR=/tmp/poolparty-tmp
WORKDIR /var/lib/poolparty
USER 10001:10001
EXPOSE 8080
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/poolparty-entrypoint"]
CMD ["--serve", "/etc/poolparty/config.json"]
