# Clean Linux environment for the real-tool integration tests, matching the
# GitHub runner (ubuntu, Rust from rust-toolchain.toml, Node from .node-version).
# Nothing here needs an account, subscription or API key: the tests point the
# real CLIs at localhost mock providers.
#
# Build args are read from the repo by scripts/tests/docker-real-tool.sh.
FROM ubuntu:24.04

ARG RUST_VERSION
ARG NODE_VERSION
ARG UID=1000

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install --yes --no-install-recommends \
      build-essential pkg-config git curl ca-certificates xz-utils \
      procps psmisc less \
 && rm -rf /var/lib/apt/lists/*

# Node, for the npm-installed Claude CLI (scripts/install-mock-tools.sh).
RUN curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-x64.tar.xz" \
    | tar -xJ -C /usr/local --strip-components=1

# Same uid as the host user so bind-mounted files stay writable. Ubuntu 24.04
# ships a default `ubuntu` user on uid 1000; drop it first.
RUN (userdel -r ubuntu 2>/dev/null || true) \
 && useradd --create-home --uid "${UID}" --shell /bin/bash hcom

USER hcom
ENV HOME=/home/hcom \
    RUSTUP_HOME=/home/hcom/.rustup \
    CARGO_HOME=/home/hcom/.cargo \
    PATH=/home/hcom/.cargo/bin:/usr/local/bin:/usr/bin:/bin
RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain "${RUST_VERSION}" \
       --component clippy --component rustfmt

WORKDIR /work
