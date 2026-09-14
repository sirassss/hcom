use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Materialize one plugin adapter and the canonical skills into a new artifact.
///
/// Source symlinks are followed only when they resolve inside the tree being
/// copied. The destination must not exist and must be outside `source_root`, so
/// traversal cannot discover its own output.
pub(crate) fn materialize_plugin_artifact(
    source_root: &Path,
    adapter: &str,
    destination: &Path,
) -> Result<(), String> {
    if !matches!(adapter, "hcom" | "hcom-agy") {
        return Err("plugin adapter must be hcom or hcom-agy".to_string());
    }
    match std::fs::symlink_metadata(destination) {
        Ok(_) => {
            return Err(format!(
                "plugin staging destination already exists: {}",
                destination.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "could not inspect plugin staging destination {}: {error}",
                destination.display()
            ));
        }
    }

    let source_root = canonicalize_source(source_root, "source root")?;
    let adapter_root =
        canonicalize_source(&source_root.join("plugin").join(adapter), "plugin adapter")?;
    let skills_root = canonicalize_source(&source_root.join("skills"), "canonical skills")?;
    ensure_within(&adapter_root, &source_root, "plugin adapter")?;
    ensure_within(&skills_root, &source_root, "canonical skills")?;

    let destination_parent = destination.parent().ok_or_else(|| {
        format!(
            "plugin staging destination has no parent: {}",
            destination.display()
        )
    })?;
    let destination_parent = destination_parent.canonicalize().map_err(|error| {
        format!(
            "plugin staging destination parent {} is not readable: {error}",
            destination_parent.display()
        )
    })?;
    if destination_parent.starts_with(&source_root) {
        return Err(format!(
            "plugin staging destination {} must be outside source root {}",
            destination.display(),
            source_root.display()
        ));
    }

    std::fs::create_dir(destination).map_err(|error| {
        format!(
            "could not create plugin staging destination {}: {error}",
            destination.display()
        )
    })?;

    let result = materialize_contents(&adapter_root, &skills_root, destination);
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(destination);
        return Err(error);
    }
    Ok(())
}

fn materialize_contents(
    adapter_root: &Path,
    skills_root: &Path,
    destination: &Path,
) -> Result<(), String> {
    let mut adapter_entries = read_entries(adapter_root)?;
    adapter_entries.retain(|entry| entry.file_name().is_none_or(|name| name != "skills"));
    let mut ancestors = HashSet::new();
    for source in adapter_entries {
        let file_name = source.file_name().ok_or_else(|| {
            format!(
                "plugin adapter entry has no file name: {}",
                source.display()
            )
        })?;
        copy_materialized(
            &source,
            adapter_root,
            &destination.join(file_name),
            &mut ancestors,
        )?;
    }
    copy_materialized(
        skills_root,
        skills_root,
        &destination.join("skills"),
        &mut ancestors,
    )
}

fn copy_materialized(
    source: &Path,
    boundary: &Path,
    destination: &Path,
    ancestors: &mut HashSet<PathBuf>,
) -> Result<(), String> {
    let resolved = source.canonicalize().map_err(|error| {
        format!(
            "could not resolve plugin source entry {}: {error}",
            source.display()
        )
    })?;
    ensure_within(&resolved, boundary, "plugin source entry")?;
    let metadata = std::fs::metadata(&resolved).map_err(|error| {
        format!(
            "could not inspect plugin source entry {}: {error}",
            source.display()
        )
    })?;

    if metadata.is_file() {
        std::fs::copy(&resolved, destination).map_err(|error| {
            format!(
                "could not copy plugin source file {} to {}: {error}",
                source.display(),
                destination.display()
            )
        })?;
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(format!(
            "plugin source entry is neither a file nor directory: {}",
            source.display()
        ));
    }
    if !ancestors.insert(resolved.clone()) {
        return Err(format!(
            "plugin source directory cycle detected at {}",
            source.display()
        ));
    }

    let result = (|| {
        std::fs::create_dir(destination).map_err(|error| {
            format!(
                "could not create staged plugin directory {}: {error}",
                destination.display()
            )
        })?;
        for child in read_entries(&resolved)? {
            let file_name = child.file_name().ok_or_else(|| {
                format!("plugin source entry has no file name: {}", child.display())
            })?;
            copy_materialized(&child, boundary, &destination.join(file_name), ancestors)?;
        }
        Ok(())
    })();
    ancestors.remove(&resolved);
    result
}

fn read_entries(root: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(root).map_err(|error| {
        format!(
            "could not read plugin source directory {}: {error}",
            root.display()
        )
    })?;
    let mut paths = entries
        .map(|entry| {
            entry.map(|entry| entry.path()).map_err(|error| {
                format!(
                    "could not read an entry under plugin source directory {}: {error}",
                    root.display()
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    Ok(paths)
}

fn canonicalize_source(path: &Path, label: &str) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|error| format!("{label} {} is not readable: {error}", path.display()))
}

fn ensure_within(path: &Path, boundary: &Path, label: &str) -> Result<(), String> {
    if path.starts_with(boundary) {
        Ok(())
    } else {
        Err(format!(
            "{label} {} resolves outside source tree {}",
            path.display(),
            boundary.display()
        ))
    }
}
