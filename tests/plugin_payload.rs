use std::path::{Path, PathBuf};

#[path = "../src/hooks/plugin_stage.rs"]
mod plugin_stage;

fn files_below(root: &Path) -> Vec<PathBuf> {
    fn visit(root: &Path, current: &Path, files: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(current).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
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

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_tree(&source_path, &destination_path);
        } else {
            std::fs::copy(&source_path, &destination_path).unwrap();
        }
    }
}

#[test]
fn agy_package_carries_the_canonical_skill() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let canonical_root = root.join("skills/hcom-agent-messaging");
    let fixture = tempfile::tempdir().unwrap();
    let source_root = fixture.path().join("source");
    copy_tree(
        &root.join("plugin/hcom-agy"),
        &source_root.join("plugin/hcom-agy"),
    );
    copy_tree(&root.join("skills"), &source_root.join("skills"));

    let artifact_root = fixture.path().join("artifact");
    plugin_stage::materialize_plugin_artifact(&source_root, "hcom-agy", &artifact_root).unwrap();
    std::fs::rename(&source_root, fixture.path().join("source-unavailable")).unwrap();

    let bundled_root = artifact_root.join("skills/hcom-agent-messaging");

    let canonical_files = files_below(&canonical_root);
    assert!(
        canonical_files.contains(&PathBuf::from("references/scripts/basic-messaging.sh")),
        "recursive fixture omitted references/scripts"
    );
    for relative in canonical_files {
        let canonical = canonical_root.join(&relative);
        let bundled = bundled_root.join(&relative);
        assert_eq!(
            std::fs::read(&canonical).unwrap(),
            std::fs::read(&bundled).unwrap(),
            "bundled payload differs at {}",
            relative.display()
        );
        assert!(
            bundled
                .canonicalize()
                .unwrap()
                .starts_with(artifact_root.canonicalize().unwrap()),
            "{} escapes artifact root {}",
            bundled.display(),
            artifact_root.display()
        );
    }
    assert_eq!(files_below(&canonical_root), files_below(&bundled_root));
    assert_eq!(
        std::fs::read(root.join("plugin/hcom-agy/hooks/hooks.json")).unwrap(),
        std::fs::read(artifact_root.join("hooks/hooks.json")).unwrap(),
        "staging changed the AGY hook payload"
    );
}

#[test]
fn staging_rejects_an_existing_destination_without_overwriting_it() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = tempfile::tempdir().unwrap();
    let destination = fixture.path().join("already-there");
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(destination.join("keep"), "owned by caller").unwrap();

    let error =
        plugin_stage::materialize_plugin_artifact(root, "hcom-agy", &destination).unwrap_err();

    assert!(
        error.contains("already exists"),
        "unexpected error: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("keep")).unwrap(),
        "owned by caller"
    );
}

#[test]
fn shared_adapter_staging_materializes_the_canonical_skills_directory() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = tempfile::tempdir().unwrap();
    let destination = fixture.path().join("hcom");

    plugin_stage::materialize_plugin_artifact(root, "hcom", &destination).unwrap();
    assert!(destination.join(".claude-plugin/plugin.json").is_file());
    assert!(destination.join("hooks/hooks.json").is_file());
    assert!(destination.join("hooks/hooks-cursor.json").is_file());
    // The Codex overlay is a second descriptor directory beside the Claude one;
    // staging copies the adapter tree recursively, and this pins that the
    // artifact a vendor installs actually carries it.
    assert!(destination.join("hooks/hooks-codex.json").is_file());
    assert!(destination.join(".codex-plugin/plugin.json").is_file());
    assert!(destination.join(".cursor-plugin/plugin.json").is_file());
    assert!(
        !std::fs::symlink_metadata(destination.join("skills"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "staged skills must be materialized, not linked back to the checkout"
    );
    assert_eq!(
        std::fs::read(root.join("skills/hcom-agent-messaging/SKILL.md")).unwrap(),
        std::fs::read(destination.join("skills/hcom-agent-messaging/SKILL.md")).unwrap()
    );
}

#[test]
fn staging_rejects_a_destination_inside_the_source_tree() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = tempfile::tempdir().unwrap();
    let source_root = fixture.path().join("source");
    copy_tree(
        &root.join("plugin/hcom-agy"),
        &source_root.join("plugin/hcom-agy"),
    );
    copy_tree(&root.join("skills"), &source_root.join("skills"));
    let destination = source_root.join("artifact");

    let error = plugin_stage::materialize_plugin_artifact(&source_root, "hcom-agy", &destination)
        .unwrap_err();

    assert!(
        error.contains("outside source root"),
        "unexpected error: {error}"
    );
    assert!(!destination.exists());
}

#[cfg(unix)]
#[test]
fn staging_rejects_links_outside_the_canonical_skills_tree() {
    use std::os::unix::fs::symlink;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = tempfile::tempdir().unwrap();
    let source_root = fixture.path().join("source");
    copy_tree(
        &root.join("plugin/hcom-agy"),
        &source_root.join("plugin/hcom-agy"),
    );
    copy_tree(&root.join("skills"), &source_root.join("skills"));
    let external = fixture.path().join("external.md");
    std::fs::write(&external, "outside source tree\n").unwrap();
    let linked = source_root.join("skills/hcom-agent-messaging/references/cross-tool.md");
    std::fs::remove_file(&linked).unwrap();
    symlink(&external, &linked).unwrap();
    let destination = fixture.path().join("artifact");

    let error = plugin_stage::materialize_plugin_artifact(&source_root, "hcom-agy", &destination)
        .unwrap_err();

    assert!(
        error.contains("outside source tree"),
        "unexpected error: {error}"
    );
    assert!(
        !destination.exists(),
        "failed staging must clean partial output"
    );
}

#[cfg(unix)]
#[test]
fn staging_rejects_directory_cycles_without_leaving_partial_output() {
    use std::os::unix::fs::symlink;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = tempfile::tempdir().unwrap();
    let source_root = fixture.path().join("source");
    copy_tree(
        &root.join("plugin/hcom-agy"),
        &source_root.join("plugin/hcom-agy"),
    );
    copy_tree(&root.join("skills"), &source_root.join("skills"));
    let references = source_root.join("skills/hcom-agent-messaging/references");
    symlink(".", references.join("cycle")).unwrap();
    let destination = fixture.path().join("artifact");

    let error = plugin_stage::materialize_plugin_artifact(&source_root, "hcom-agy", &destination)
        .unwrap_err();

    assert!(
        error.contains("cycle detected"),
        "unexpected error: {error}"
    );
    assert!(
        !destination.exists(),
        "failed staging must clean partial output"
    );
}

/// Each adapter carries its own real copy because vendors disagree about
/// symlinks: Claude dereferences `plugin/hcom/skills`, Codex skips it
/// (measured 0.154.0), which shipped Codex hooks with no skill. The copies are
/// generated, so drift means someone hand-edited one or forgot to regenerate.
#[test]
fn every_adapter_carries_a_byte_identical_copy_of_the_skill() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let canonical = root.join("skills/hcom-agent-messaging");
    let expected = files_below(&canonical);
    assert!(
        expected.contains(&PathBuf::from("references/scripts/basic-messaging.sh")),
        "canonical tree walk missed references/scripts"
    );

    for adapter in ["hcom", "hcom-agy"] {
        let copy = root
            .join("plugin")
            .join(adapter)
            .join("skills/hcom-agent-messaging");
        assert_eq!(
            files_below(&copy),
            expected,
            "plugin/{adapter}/skills is out of sync — run scripts/sync-plugin-skills.sh"
        );
        for relative in &expected {
            assert_eq!(
                std::fs::read(canonical.join(relative)).unwrap(),
                std::fs::read(copy.join(relative)).unwrap(),
                "plugin/{adapter}/skills/hcom-agent-messaging/{} differs — run scripts/sync-plugin-skills.sh",
                relative.display()
            );
        }
    }
}

/// A link anywhere under `plugin/` is the bug this replaced: the vendor that
/// skips it ships a package missing whatever it pointed at. Linux CI is the
/// authoritative gate — with `core.symlinks=false` a Windows checkout
/// materializes a tracked link as ordinary text and this check would miss it.
#[test]
fn no_plugin_file_is_a_symlink() {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
            {
                found.push(path);
            } else if path.is_dir() {
                walk(&path, found);
            }
        }
    }

    let mut found = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin"),
        &mut found,
    );
    assert!(found.is_empty(), "symlinks under plugin/: {found:?}");
}
