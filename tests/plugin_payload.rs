//! Cursor and Antigravity plugin packaging (the only tools packaged as plugins;
//! the rest load hooks per launch). See `scripts/sync-plugin-skills.sh`.

use std::path::{Path, PathBuf};

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn files_below(root: &Path) -> Vec<PathBuf> {
    fn visit(root: &Path, current: &Path, files: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.push(path.strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }

    let mut files = Vec::new();
    visit(root, root, &mut files);
    files.sort();
    files
}

fn json(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn agy_package_carries_the_canonical_skill() {
    let canonical_root = root().join("skills/hcom-agent-messaging");
    let bundled_root = root().join("plugin/hcom-agy/skills/hcom-agent-messaging");

    let canonical_files = files_below(&canonical_root);
    assert!(
        canonical_files.contains(&PathBuf::from("references/scripts/basic-messaging.sh")),
        "canonical skill lost references/scripts"
    );
    assert_eq!(
        canonical_files,
        files_below(&bundled_root),
        "file set drifted: run scripts/sync-plugin-skills.sh"
    );
    for relative in canonical_files {
        assert_eq!(
            std::fs::read(canonical_root.join(&relative)).unwrap(),
            std::fs::read(bundled_root.join(&relative)).unwrap(),
            "bundled skill differs at {}: run scripts/sync-plugin-skills.sh",
            relative.display()
        );
    }
    assert!(
        !std::fs::symlink_metadata(root().join("plugin/hcom-agy/skills"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "agy does not dereference a symlinked skills/"
    );
}

#[test]
fn agy_hooks_route_to_hcom_gemini_handlers() {
    let hooks = json(&root().join("plugin/hcom-agy/hooks/hooks.json"));
    let text = hooks.to_string();
    for handler in [
        "gemini-sessionstart",
        "gemini-beforeagent",
        "gemini-afteragent",
        "gemini-beforetool",
        "gemini-aftertool",
        "gemini-sessionend",
    ] {
        assert!(text.contains(handler), "AGY hooks.json lacks {handler}");
    }
    assert!(
        root()
            .join("plugin/hcom-agy/.claude-plugin/plugin.json")
            .is_file()
    );
}

#[test]
fn cursor_plugin_points_at_its_hooks_and_skills() {
    let manifest = json(&root().join("plugin/hcom/.cursor-plugin/plugin.json"));
    assert_eq!(manifest["skills"], "./skills/");
    assert_eq!(manifest["hooks"], "./hooks/hooks-cursor.json");

    let hooks = json(&root().join("plugin/hcom/hooks/hooks-cursor.json"));
    let text = hooks.to_string();
    for handler in [
        "cursor-sessionstart",
        "cursor-beforesubmitprompt",
        "cursor-pretooluse",
        "cursor-posttooluse",
        "cursor-stop",
        "cursor-sessionend",
    ] {
        assert!(text.contains(handler), "hooks-cursor.json lacks {handler}");
    }
    // plugin/hcom/skills is upstream's symlink to the canonical skills dir.
    assert!(
        root()
            .join("plugin/hcom/skills/hcom-agent-messaging/SKILL.md")
            .is_file()
    );
}
