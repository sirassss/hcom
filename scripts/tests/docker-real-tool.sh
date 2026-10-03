#!/usr/bin/env bash
# Run one real-tool integration test in a clean Docker container, with no
# account, subscription or API key. Each test drives a genuine, pinned CLI
# against a localhost mock provider.
#
#   scripts/tests/docker-real-tool.sh                      # claude -> cursor scenario
#   scripts/tests/docker-real-tool.sh real_tool_claude     # any other test binary
#   scripts/tests/docker-real-tool.sh real_tool_claude_claude
#   scripts/tests/docker-real-tool.sh real_tool_claude_codex
#   scripts/tests/docker-real-tool.sh real_tool_claude_agy   # opt-in: real agy + your Google login
#
# Needs: docker; for the Cursor scenario, cursor-agent installed on the host
# (the matching version directory is mounted read-only; it carries its own node).
#
# The agy scenario is the exception to "no account": agy has no mock backend, so
# it mounts the host's agy binary read-only plus a COPY of its login (only
# antigravity-oauth-token and installation_id, taken from $HCOM_RT_AGY_AUTH_DIR or
# ~/.gemini/antigravity-cli, copied to a temp dir that is deleted on exit), keeps
# the network on for the run phase, and spends a little real model quota.
#
# Two phases. PREPARE has network: build the image, fetch crates, install the
# pinned Claude CLI, compile the tests. RUN has none (`--network none`), which
# proves the scenario needs no outside service. Caches live in named volumes, so
# the host's target/ is never touched.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TEST="${1:-real_tool_claude_cursor}"
IMAGE="${HCOM_RT_IMAGE:-hcom-real-tool:local}"
CURSOR_VERSION="${HCOM_RT_CURSOR_VERSION:-2026.09.28-64d2043}"
CURSOR_DIR="${HCOM_RT_CURSOR_DIR:-$HOME/.local/share/cursor-agent/versions/$CURSOR_VERSION}"
RUST_VERSION="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$ROOT/rust-toolchain.toml")"
NODE_VERSION="$(tr -d '[:space:]' < "$ROOT/.node-version")"

mounts=(
  -v "$ROOT:/work"
  -v hcom-rt-target:/work/target
  -v hcom-rt-cargo:/home/hcom/.cargo/registry
  -v hcom-rt-cargo-git:/home/hcom/.cargo/git
)
# Pinned npm CLIs to install: Claude always, Codex only for tests that drive it.
TOOLS="claude"
[[ "$TEST" == *codex* ]] && TOOLS="claude codex"
env_args=(-e CARGO_TARGET_DIR=/work/target -e HCOM_RT_TEST="$TEST" -e HCOM_RT_TOOLS="$TOOLS")
# Pass the CLI path to the tests; the cursor scenario needs the host install.
if [[ "$TEST" == *cursor* ]]; then
  if [[ ! -x "$CURSOR_DIR/cursor-agent" ]]; then
    echo "cursor-agent $CURSOR_VERSION not found at $CURSOR_DIR" >&2
    echo "set HCOM_RT_CURSOR_DIR to its version directory" >&2
    exit 1
  fi
  mounts+=(-v "$CURSOR_DIR:/opt/cursor-agent:ro")
  env_args+=(-e HCOM_RT_CURSOR=1)
fi

if [[ "$TEST" == *agy* ]]; then
  AGY_BIN="$(readlink -f "${HCOM_RT_AGY_BIN:-$(command -v agy || true)}")"
  AGY_SRC="${HCOM_RT_AGY_AUTH_DIR:-$HOME/.gemini/antigravity-cli}"
  if [[ ! -x "$AGY_BIN" ]]; then
    echo "agy binary not found; put agy on PATH or set HCOM_RT_AGY_BIN" >&2
    exit 1
  fi
  AGY_AUTH_TMP="$(mktemp -d)"
  trap 'rm -rf "$AGY_AUTH_TMP"' EXIT
  for f in antigravity-oauth-token installation_id; do
    cp "$AGY_SRC/$f" "$AGY_AUTH_TMP/$f"
  done
  chmod 755 "$AGY_AUTH_TMP"; chmod 644 "$AGY_AUTH_TMP"/*
  mounts+=(-v "$AGY_BIN:/opt/agy/agy:ro" -v "$AGY_AUTH_TMP:/opt/agy-auth:ro")
  env_args+=(-e HCOM_RT_AGY=1 -e HCOM_RT_AGY_AUTH_DIR=/opt/agy-auth)
fi

echo "== image $IMAGE (rust $RUST_VERSION, node $NODE_VERSION)"
docker build -q -t "$IMAGE" \
  --build-arg "RUST_VERSION=$RUST_VERSION" \
  --build-arg "NODE_VERSION=$NODE_VERSION" \
  --build-arg "UID=$(id -u)" \
  "$ROOT/docker/real-tool" >/dev/null

# Volumes are created root-owned; hand them to the container user once.
for v in hcom-rt-target hcom-rt-cargo hcom-rt-cargo-git; do docker volume create "$v" >/dev/null; done
docker run --rm -u root "${mounts[@]}" "$IMAGE" \
  chown -R hcom:hcom /work/target /home/hcom/.cargo

prepare='
set -euo pipefail
cd /work
./scripts/install-mock-tools.sh $HCOM_RT_TOOLS
cargo test --locked --test "$HCOM_RT_TEST" --no-run
'
echo "== prepare (network)"
docker run --rm "${mounts[@]}" "${env_args[@]}" "$IMAGE" bash -c "$prepare"

run='
set -euo pipefail
cd /work
export PATH="/work/target/mock-tools/bin:$PATH"
if [ -n "${HCOM_RT_CURSOR:-}" ]; then
  mkdir -p "$HOME/bin"
  ln -sf /opt/cursor-agent/cursor-agent "$HOME/bin/cursor-agent"
  export PATH="$HOME/bin:$PATH"
fi
if [ -n "${HCOM_RT_AGY:-}" ]; then
  mkdir -p "$HOME/bin"
  ln -sf /opt/agy/agy "$HOME/bin/agy"
  export PATH="$HOME/bin:$PATH"
fi
cargo test --locked --offline --test "$HCOM_RT_TEST" -- --ignored --nocapture --test-threads=1
'
# Everything but agy runs with no network at all; agy needs Google's backend.
NETWORK=none
[[ "$TEST" == *agy* ]] && NETWORK=bridge
echo "== run $TEST (network: $NETWORK)"
# --init: the lifecycle tests wait for a killed agent's process group to be gone
# (kill(-pgid, 0)), and a zombie still counts as a member. Without a PID 1 that
# reaps orphans, every `hcom kill` leaves one behind and that wait times out
# (real_tool_claude: 3 of 3 runs failed without --init, passed with it).
docker run --rm --init --network "$NETWORK" "${mounts[@]}" "${env_args[@]}" "$IMAGE" bash -c "$run"
