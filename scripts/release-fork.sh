#!/usr/bin/env bash
# Build a personal Linux release from this checkout and publish it to this
# fork's GitHub releases (curl-installer included), without touching the
# homebrew/pypi publish jobs dist-workspace.toml wires up for upstream.
#
# Usage: scripts/release-fork.sh [--build-only] [tag]
#   tag defaults to v<Cargo.toml version>-fork.<short sha>
#   --build-only: skip the CI gate and the upload; leave artifacts in target/distrib
#
# Requires: docker, `dist` pinned to the version in dist-workspace.toml (for the
# installer step on the host), and `gh` authenticated for this fork.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

BUILD_ONLY=0
if [ "${1:-}" = "--build-only" ]; then
    BUILD_ONLY=1
    shift
fi

REPO="$(git remote get-url origin | sed -E 's#\.git$##; s#.*[:/]([^/]+/[^/]+)$#\1#')"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
TAG="${1:-v${VERSION}-fork.$(git rev-parse --short HEAD)}"
DIST_VERSION="$(sed -n 's/^cargo-dist-version = "\(.*\)"/\1/p' dist-workspace.toml)"
RUST_VERSION="$(rustc --version | awk '{print $2}')"

# Each Linux target builds inside a container whose libc sets the floor for
# who can run the binary. Building on the host links against the host's glibc
# (2.39 on Ubuntu 24.04), which the installer can't detect: its check_glibc
# only rejects systems older than 2.31 and would otherwise hand out a gnu
# binary that fails to start. Bullseye ships glibc 2.31, matching that check;
# the musl build is static and is what the installer falls back to below it.
LINUX_TARGETS=(x86_64-unknown-linux-gnu x86_64-unknown-linux-musl)
image_for() {
    case "$1" in
        *-linux-gnu) echo "rust:${RUST_VERSION}-bullseye" ;;
        *-linux-musl) echo "rust:${RUST_VERSION}-alpine" ;;
        *) echo "[release-fork] no build image for $1" >&2; return 1 ;;
    esac
}

echo "[release-fork] repo=$REPO tag=$TAG targets=${LINUX_TARGETS[*]}"

if [ "$BUILD_ONLY" -eq 0 ]; then
    if ! git merge-base --is-ancestor HEAD "origin/$(git branch --show-current)" 2>/dev/null; then
        echo "[release-fork] HEAD isn't on origin yet — push the branch first (git push origin $(git branch --show-current))" >&2
        exit 1
    fi

    COMMIT_SHA="$(git rev-parse HEAD)"
    echo "[release-fork] checking CI status for $COMMIT_SHA..."
    # dist's own "Release" workflow fires on any tag matching **N.N.N* and then
    # refuses a fork tag: the workspace is version 0.7.27, the tag is
    # v0.7.27-siras, so `dist host` reports "This workspace doesn't have anything
    # for dist to Release!". That run is upstream's publisher, not a test of this
    # code, and it can never go green here - this script is what publishes the
    # fork. Judge the gate on everything else.
    runs="$(gh run list --repo "$REPO" -c "$COMMIT_SHA" --json status,conclusion,name,url \
        | jq '[.[] | select(.name != "Release")]')"
    if [ "$(echo "$runs" | jq 'length')" -eq 0 ]; then
        echo "[release-fork] no CI runs found for $COMMIT_SHA — push may not have triggered CI yet" >&2
        exit 1
    fi
    if echo "$runs" | jq -e '.[] | select(.status != "completed")' >/dev/null; then
        echo "[release-fork] CI still running for $COMMIT_SHA — wait for it to finish" >&2
        exit 1
    fi
    if echo "$runs" | jq -e '.[] | select(.conclusion != "success")' >/dev/null; then
        echo "[release-fork] CI failed for $COMMIT_SHA:" >&2
        echo "$runs" | jq -r '.[] | select(.conclusion != "success") | "  - \(.name): \(.conclusion) (\(.url))"' >&2
        exit 1
    fi
    echo "[release-fork] CI passed for $COMMIT_SHA"
fi

# The host `dist` links against the host glibc and can't run in the build
# containers, so fetch the static musl build of the pinned version once.
CACHE=target/fork-release
CONTAINER_DIST="$CACHE/dist-$DIST_VERSION/dist"
if [ ! -x "$CONTAINER_DIST" ]; then
    dl="$CACHE/dist-$DIST_VERSION"
    mkdir -p "$dl"
    gh release download "v$DIST_VERSION" --repo axodotdev/cargo-dist \
        -p 'cargo-dist-x86_64-unknown-linux-musl.tar.xz*' -D "$dl" --clobber
    (cd "$dl" && sha256sum -c cargo-dist-x86_64-unknown-linux-musl.tar.xz.sha256)
    tar -xJf "$dl/cargo-dist-x86_64-unknown-linux-musl.tar.xz" -C "$dl" --strip-components=1
fi
mkdir -p "$CACHE/cargo-home"

# dist bakes the repo owner/repo into the installer from Cargo.toml's
# `repository` field, which points at upstream. Patch it just for this build,
# always restore on exit — this file must never land on disk changed.
trap 'git checkout -- Cargo.toml' EXIT
sed -i.bak -E "s#^repository = .*#repository = \"https://github.com/${REPO}\"#" Cargo.toml
rm -f Cargo.toml.bak

# Start from an empty distrib dir so no archive built on the host (or by an
# earlier run) can be picked up and uploaded.
rm -rf target/distrib
mkdir -p target/distrib

for target in "${LINUX_TARGETS[@]}"; do
    image="$(image_for "$target")"
    echo "[release-fork] building $target in $image"
    # A per-target CARGO_TARGET_DIR keeps these objects apart from host builds:
    # cargo's fingerprints don't cover the libc, so a shared dir would reuse
    # C objects (bundled sqlite, aws-lc) compiled against the host glibc.
    # RUSTUP_TOOLCHAIN skips rust-toolchain.toml's clippy/rustfmt components,
    # which rustup would otherwise re-download into every throwaway container.
    # dist still writes its archives to /src/target/distrib, and reads every
    # *-dist-manifest.json already there, so the manifest is written elsewhere
    # first: redirecting straight in would hand dist an empty file to parse.
    docker run --rm \
        --user "$(id -u):$(id -g)" \
        -v "$PWD:/src" -w /src \
        -e HOME=/tmp \
        -e RUSTUP_TOOLCHAIN="$RUST_VERSION" \
        -e CARGO_HOME="/src/$CACHE/cargo-home" \
        -e CARGO_TARGET_DIR="/src/$CACHE/target-$target" \
        "$image" \
        "/src/$CONTAINER_DIST" build --artifacts=local --target="$target" \
            --tag="$TAG" --force-tag --allow-dirty --output-format=json \
        > "$CACHE/$target-dist-manifest.json"
    mv "$CACHE/$target-dist-manifest.json" target/distrib/
done

# The global step reads the per-target manifests above for the archive
# checksums, so the installer verifies what it downloads.
target_flags=()
for target in "${LINUX_TARGETS[@]}"; do target_flags+=(--target="$target"); done
dist build --artifacts=global "${target_flags[@]}" --tag="$TAG" --force-tag --allow-dirty --installer=shell

git checkout -- Cargo.toml
trap - EXIT

assets=(target/distrib/hcom-installer.sh target/distrib/sha256.sum
        target/distrib/source.tar.gz target/distrib/source.tar.gz.sha256)
for target in "${LINUX_TARGETS[@]}"; do
    assets+=(target/distrib/hcom-"$target".tar.gz target/distrib/hcom-"$target".tar.gz.sha256)
done

if [ "$BUILD_ONLY" -eq 1 ]; then
    echo "[release-fork] built (not uploaded):"
    printf '  %s\n' "${assets[@]}"
    exit 0
fi

if gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
    gh release upload "$TAG" "${assets[@]}" --repo "$REPO" --clobber
else
    gh release create "$TAG" "${assets[@]}" \
        --repo "$REPO" \
        --target "$(git branch --show-current)" \
        --title "hcom $TAG (personal build)" \
        --notes "Personal build off $(git rev-parse --short HEAD). Not an official versioned release." \
        --prerelease
fi

echo "[release-fork] done: https://github.com/${REPO}/releases/tag/${TAG}"
echo "[release-fork] install: curl --proto '=https' --tlsv1.2 -LsSf https://github.com/${REPO}/releases/download/${TAG}/hcom-installer.sh | sh"
