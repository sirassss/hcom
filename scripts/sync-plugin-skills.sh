#!/usr/bin/env bash
# Regenerate each plugin adapter's copy of the canonical messaging skill.
#
# Vendors copy a plugin package into their own cache at install time and do not
# agree on what to do with a symlink: Claude dereferences it, Codex skips it
# (measured 0.154.0, which shipped Codex hooks and no skill). So every adapter
# carries real files. This script is the only supported way to update them —
# edit skills/hcom-agent-messaging/ and rerun.
#
# Needs Bash; on Windows use Git Bash. tests/plugin_payload.rs fails when a copy
# drifts, and Linux CI is the authoritative no-symlink gate.
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
