use super::{invalid_data, InstallManifest, PreviousTarget, RelativePath};
use std::{collections::BTreeMap, io};

type PathIndex<'a> = BTreeMap<String, &'a RelativePath>;

#[derive(Clone, Copy)]
pub(crate) enum Replacement {
    Complete,
    Overlay,
}

pub(crate) struct FileChanges<'a> {
    pub files: Vec<&'a RelativePath>,
    pub remove: Vec<&'a RelativePath>,
}

impl<'a> FileChanges<'a> {
    pub(crate) fn new(
        owned: &'a [RelativePath],
        incoming: &'a [RelativePath],
        replacement: Replacement,
    ) -> io::Result<Self> {
        let previous = payload_index(owned)?;
        let next = payload_index(incoming)?;
        let mut combined = previous.clone();
        combined.extend(next.iter().map(|(key, path)| (key.clone(), *path)));
        validate_file_parents(&combined)?;
        let remove = match replacement {
            Replacement::Complete => previous
                .iter()
                .filter(|(key, _)| !next.contains_key(*key))
                .map(|(_, path)| *path)
                .collect(),
            Replacement::Overlay => Vec::new(),
        };
        let retained = match replacement {
            Replacement::Complete => next,
            Replacement::Overlay => combined,
        };
        Ok(Self {
            files: retained.into_values().collect(),
            remove,
        })
    }
}

pub(super) fn validate_manifest_sets(manifest: &InstallManifest) -> io::Result<()> {
    let files = payload_index(&manifest.files)?;
    let directories = path_index(&manifest.created_dirs)?;
    validate_file_parents(&files)?;
    for key in directories.keys() {
        if files.contains_key(key) || has_file_parent(&files, key) {
            return Err(invalid_data("A manifest directory conflicts with a file"));
        }
    }
    let Some(pending) = &manifest.pending_file else {
        return Ok(());
    };
    let key = path_key(&pending.path);
    let owned = files.contains_key(&key);
    if owned != matches!(pending.previous_target, PreviousTarget::Owned) {
        return Err(invalid_data(
            "Pending target ownership does not match the manifest",
        ));
    }
    if pending.identity.volume_serial_number != manifest.identity.root.volume_serial_number {
        return Err(invalid_data(
            "Pending publication is not on the installation volume",
        ));
    }
    let prefix = format!("{key}/");
    if has_file_parent(&files, &key)
        || directories.contains_key(&key)
        || files
            .keys()
            .chain(directories.keys())
            .any(|entry| entry.starts_with(&prefix))
    {
        return Err(invalid_data(
            "Pending target conflicts with the manifest tree",
        ));
    }
    Ok(())
}

fn path_key(path: &RelativePath) -> String {
    // Non-ASCII equivalence and filesystem aliases require the Windows object check.
    path.as_str().to_ascii_lowercase()
}

fn path_index(paths: &[RelativePath]) -> io::Result<PathIndex<'_>> {
    let mut index = BTreeMap::new();
    for path in paths {
        if index.insert(path_key(path), path).is_some() {
            return Err(invalid_data("Duplicate manifest path"));
        }
    }
    Ok(index)
}

fn payload_index(paths: &[RelativePath]) -> io::Result<PathIndex<'_>> {
    for path in paths {
        path.validate_payload_file().map_err(invalid_data)?;
    }
    path_index(paths)
}

fn has_file_parent(files: &PathIndex<'_>, key: &str) -> bool {
    key.match_indices('/')
        .any(|(separator, _)| files.contains_key(&key[..separator]))
}

fn validate_file_parents(files: &PathIndex<'_>) -> io::Result<()> {
    if files.keys().any(|key| has_file_parent(files, key)) {
        return Err(invalid_data("A manifest file is also used as a directory"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<RelativePath> {
        names
            .iter()
            .map(|name| RelativePath::parse(name).unwrap())
            .collect()
    }

    fn names(paths: &[&RelativePath]) -> Vec<String> {
        paths.iter().map(|path| path.as_str().to_owned()).collect()
    }

    #[test]
    fn replacement_prunes_while_overlay_retains_owned_customization() {
        let old = paths(&["Custom.exe", "custom.txt", "assets/logo.png", "old.dll"]);
        let new = paths(&["Custom.exe", "new.dll"]);
        let complete = FileChanges::new(&old, &new, Replacement::Complete).unwrap();
        assert_eq!(names(&complete.files), ["Custom.exe", "new.dll"]);
        assert_eq!(
            names(&complete.remove),
            ["assets/logo.png", "custom.txt", "old.dll"]
        );
        let overlay = FileChanges::new(&old, &new, Replacement::Overlay).unwrap();
        assert!(overlay.remove.is_empty());
        assert_eq!(
            names(&overlay.files),
            [
                "assets/logo.png",
                "Custom.exe",
                "custom.txt",
                "new.dll",
                "old.dll"
            ]
        );
    }

    #[test]
    fn rejects_ambiguous_packages_before_calculating_removals() {
        let old = paths(&["assets/logo.png"]);
        for new in [
            paths(&["app.exe", "APP.EXE"]),
            paths(&["data/meta/install-manifest.json"]),
            paths(&["assets"]),
        ] {
            assert!(FileChanges::new(&old, &new, Replacement::Complete).is_err());
        }
    }
}
