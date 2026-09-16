#!/usr/bin/env bash
# Build a personal Linux release from this checkout and publish it to this
# fork's GitHub releases (curl-installer included), without touching the
# homebrew/pypi publish jobs dist-workspace.toml wires up for upstream.
#
# Usage: scripts/release-fork.sh [tag]
#   tag defaults to v<Cargo.toml version>-fork.<short sha>
#
# Requires: cargo-dist (`dist --version`) pinned to the version in
# dist-workspace.toml, and `gh` authenticated for this fork.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

REPO="$(git remote get-url origin | sed -E 's#\.git$##; s#.*[:/]([^/]+/[^/]+)$#\1#')"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
TAG="${1:-v${VERSION}-fork.$(git rev-parse --short HEAD)}"
HOST_TARGET="$(rustc -print host-tuple)"

echo "[release-fork] repo=$REPO tag=$TAG target=$HOST_TARGET"

if ! git merge-base --is-ancestor HEAD "origin/$(git branch --show-current)" 2>/dev/null; then
    echo "[release-fork] HEAD isn't on origin yet — push the branch first (git push origin $(git branch --show-current))" >&2
    exit 1
fi

# dist bakes the repo owner/repo into the installer from Cargo.toml's
# `repository` field, which points at upstream. Patch it just for this build,
# always restore on exit — this file must never land on disk changed.
trap 'git checkout -- Cargo.toml' EXIT
sed -i.bak -E "s#^repository = .*#repository = \"https://github.com/${REPO}\"#" Cargo.toml
rm -f Cargo.toml.bak

dist build --artifacts=local --target="$HOST_TARGET" --tag="$TAG" --force-tag --allow-dirty
dist build --artifacts=global --tag="$TAG" --force-tag --allow-dirty --installer=shell

git checkout -- Cargo.toml
trap - EXIT

assets=(target/distrib/hcom-"${HOST_TARGET}".tar.gz*
        target/distrib/hcom-installer.sh
        target/distrib/sha256.sum)

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
