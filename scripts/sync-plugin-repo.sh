#!/usr/bin/env bash
# Publish sirassss/hcom-plugin: a dedicated repo so no vendor's marketplace or
# install has to clone this whole monorepo (src/, tests/, docs/...) just to
# reach plugin/.
#
# Layout of the published repo:
#   .claude-plugin/plugin.json       <- plugin/hcom-agy/.claude-plugin/plugin.json
#   .claude-plugin/marketplace.json  <- plugin/.claude-plugin/marketplace.json
#   skills/, hooks/                  <- plugin/hcom-agy/{skills,hooks}
#   hcom/                            <- plugin/hcom/ (Claude/Codex/Cursor package)
#
# `agy plugin install <url>` clones a URL's default branch and reads
# skills/hooks at its ROOT, with no ref or subdirectory selector (measured
# 2026-09-15: `owner/repo@ref` is parsed as `@marketplace`, and a URL `#ref`
# fragment is silently ignored) - so AGY's package must BE this repo's root.
# Claude/Codex/Cursor instead register this same URL as a marketplace and
# read marketplace.json's `"source": "./hcom"`, ordinary indirection that
# needs no special root placement.
#
# This is a plain clone + overwrite + commit + push, not `git subtree`: the
# published layout draws from two separate source directories (hcom-agy at
# root, hcom nested) that a single subtree prefix cannot express. The
# published repo's history is its own, not shared blame with this monorepo -
# matching how generated adapter trees already work here. Run this after any
# change under plugin/ you want vendors to see.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
remote="${HCOM_PLUGIN_REPO_REMOTE:-git@sirassss:sirassss/hcom-plugin.git}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

if ! git clone --quiet --depth 1 "$remote" "$work" 2>/dev/null; then
    git -C "$work" init --quiet --initial-branch=main
fi
find "$work" -mindepth 1 -maxdepth 1 -not -name .git -exec rm -rf {} +

cp -RL "$root/plugin/hcom-agy/.claude-plugin" "$work/.claude-plugin"
cp -RL "$root/plugin/hcom-agy/skills" "$work/skills"
cp -RL "$root/plugin/hcom-agy/hooks" "$work/hooks"
cp "$root/plugin/.claude-plugin/marketplace.json" "$work/.claude-plugin/marketplace.json"
mkdir -p "$work/hcom"
cp -RL "$root/plugin/hcom/." "$work/hcom/"

git -C "$work" add -A
if git -C "$work" rev-parse --verify -q HEAD >/dev/null && git -C "$work" diff --cached --quiet; then
    echo "hcom-plugin: no changes to publish"
    exit 0
fi

user_name="$(git -C "$root" config user.name || echo "hcom-plugin sync")"
user_email="$(git -C "$root" config user.email || echo "noreply@localhost")"
git -C "$work" -c user.name="$user_name" -c user.email="$user_email" \
    commit --quiet -m "sync from $(git -C "$root" rev-parse --short HEAD)"
git -C "$work" push --quiet "$remote" HEAD:main
echo "hcom-plugin: published"
