#!/usr/bin/env bash
# Regenerate each plugin adapter's copy of the canonical messaging skill, and
# optionally publish sirassss/hcom-plugin: a dedicated repo so no vendor's
# marketplace or install has to clone this whole monorepo (src/, tests/,
# docs/...) just to reach plugin/.
#
# Vendors copy a plugin package into their own cache at install time and do not
# agree on what to do with a symlink: Claude dereferences it, Codex skips it
# (measured 0.154.0, which shipped Codex hooks and no skill). So every adapter
# carries real files. This script is the only supported way to update them —
# edit skills/hcom-agent-messaging/ and rerun.
#
# Needs Bash; on Windows use Git Bash. tests/plugin_payload.rs fails when a copy
# drifts, and Linux CI is the authoritative no-symlink gate.
#
# Usage: scripts/sync-plugin-skills.sh [--publish]
#   (no args)  Regenerate plugin/{hcom,hcom-agy}/skills only.
#   --publish  Also publish every change under plugin/ to sirassss/hcom-plugin.
#              Layout of the published repo:
#                .claude-plugin/plugin.json       <- plugin/hcom-agy/.claude-plugin/plugin.json
#                .claude-plugin/marketplace.json  <- plugin/.claude-plugin/marketplace.json
#                skills/, hooks/                  <- plugin/hcom-agy/{skills,hooks}
#                hcom/                            <- plugin/hcom/ (Claude/Codex/Cursor package)
#
#              `agy plugin install <url>` clones a URL's default branch and reads
#              skills/hooks at its ROOT, with no ref or subdirectory selector
#              (measured 2026-09-15: `owner/repo@ref` is parsed as `@marketplace`,
#              and a URL `#ref` fragment is silently ignored) - so AGY's package
#              must BE this repo's root. Claude/Codex/Cursor instead register this
#              same URL as a marketplace and read marketplace.json's
#              `"source": "./hcom"`, ordinary indirection that needs no special
#              root placement.
#
#              This is a plain clone + overwrite + commit + push, not `git
#              subtree`: the published layout draws from two separate source
#              directories (hcom-agy at root, hcom nested) that a single subtree
#              prefix cannot express. The published repo's history is its own,
#              not shared blame with this monorepo - matching how generated
#              adapter trees already work here.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
source_tree="$root/skills/hcom-agent-messaging"
[ -d "$source_tree" ] || { echo "missing $source_tree" >&2; exit 1; }

for adapter in hcom hcom-agy; do
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
