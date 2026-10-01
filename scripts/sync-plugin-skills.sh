#!/usr/bin/env bash
# Regenerate the Antigravity plugin's copy of the canonical messaging skill, and
# optionally publish sirassss/hcom-plugin: a dedicated repo so no vendor's
# plugin install has to clone this whole monorepo (src/, tests/, docs/...) just
# to reach plugin/.
#
# Scope: only Cursor and Antigravity carry plugin packaging here. Upstream
# loads hooks per launch for Claude, Codex, Copilot, Pi, OMP, OpenCode and
# Kilo, so those need no plugin hooks. Cursor and Antigravity are still
# persistent-hook tools upstream; if upstream gains per-run hooks for them,
# revert to the git tag `siras/cursor-agy-plugin` (see scripts/README.md).
#
# `agy plugin install <url>` does not dereference a symlinked skills/ and reads
# skills/hooks at the repo ROOT, so plugin/hcom-agy carries real files. This
# script is the only supported way to update them: edit
# skills/hcom-agent-messaging/ and rerun. tests/plugin_payload.rs fails when the
# copy drifts. plugin/hcom/skills stays upstream's symlink; --publish
# dereferences it.
#
# Needs Bash; on Windows use Git Bash.
#
# Usage: scripts/sync-plugin-skills.sh [--publish]
#   (no args)  Regenerate plugin/hcom-agy/skills only.
#   --publish  Also publish every change under plugin/ to sirassss/hcom-plugin.
#              Layout of the published repo:
#                .claude-plugin/plugin.json       <- plugin/hcom-agy/.claude-plugin/plugin.json
#                .claude-plugin/marketplace.json  <- plugin/.claude-plugin/marketplace.json
#                skills/, hooks/                  <- plugin/hcom-agy/{skills,hooks}
#                hcom/                            <- plugin/hcom/ (Cursor package + skills)
#
#              AGY's package must BE the repo root (no ref or subdirectory
#              selector, measured 2026-09-15). Cursor registers the same URL
#              as a marketplace and reads marketplace.json's `"source": "./hcom"`.
#
#              Plain clone + overwrite + commit + push, not `git subtree`: the
#              published layout draws from two source directories (hcom-agy at
#              root, hcom nested) that one subtree prefix cannot express.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
source_tree="$root/skills/hcom-agent-messaging"
[ -d "$source_tree" ] || { echo "missing $source_tree" >&2; exit 1; }

for adapter in hcom-agy; do
    dest="$root/plugin/$adapter/skills"
    rm -rf "$dest"
    mkdir -p "$dest"
    # -RL dereferences any link in the source so the output is always real files.
    cp -RL "$source_tree" "$dest/hcom-agent-messaging"
    echo "synced plugin/$adapter/skills"
done

[ "${1:-}" = "--publish" ] || exit 0

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
