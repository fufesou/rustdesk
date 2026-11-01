use super::{
    directory::Directory,
    files::{register, verify_hash},
    identity, open, remove,
    store::{Store, PAYLOAD_SLOT},
    Access,
};
use crate::install_manifest::{
    invalid_data, FileIdentity, InstallManifest, PendingFile, PreviousTarget,
};
use std::io;

enum Location {
    Published,
    Staged,
    Missing,
}

pub(crate) fn preview(
    root: &Directory,
    store: &Store,
    record: &InstallManifest,
) -> io::Result<InstallManifest> {
    let Some(pending) = &record.pending_file else {
        return Ok(record.clone());
    };
    let target = optional(root.entry(&pending.path, Access::Read))?;
    let staged = optional(open(&store.directory.file, PAYLOAD_SLOT, Access::Read))?;
    let target_id = target
        .as_ref()
        .map(|entry| identity(&entry.file))
        .transpose()?;
    let staged_id = staged.as_ref().map(identity).transpose()?;
    if matches!(
        locate(pending, target_id.as_ref(), staged_id.as_ref())?,
        Location::Published
    ) {
        let mut target = target.ok_or_else(|| invalid_data("Published object disappeared"))?;
        verify_hash(&mut target.file, &pending.sha256)?;
        return Ok(register(record, &pending.path));
    }
    let mut next = record.clone();
    next.pending_file = None;
    Ok(next)
}

pub(crate) fn reconcile(
    root: &Directory,
    store: &Store,
    record: &InstallManifest,
) -> io::Result<InstallManifest> {
    let next = preview(root, store, record)?;
    let Some(pending) = &record.pending_file else {
        return Ok(next);
    };
    if let Some(staged) = optional(open(&store.directory.file, PAYLOAD_SLOT, Access::Delete))? {
        if identity(&staged)? != pending.identity {
            return Err(invalid_data("Pending staged object changed"));
        }
        remove(&staged)?;
    }
    store.save(&next)?;
    Ok(next)
}

fn locate(
    pending: &PendingFile,
    target: Option<&FileIdentity>,
    staged: Option<&FileIdentity>,
) -> io::Result<Location> {
    if target == Some(&pending.identity) && staged.is_none() {
        return Ok(Location::Published);
    }
    if target.is_none() && staged.is_none() {
        return Ok(Location::Missing);
    }
    if staged != Some(&pending.identity) {
        return Err(invalid_data(
            "Pending publication has no verifiable staged object",
        ));
    }
    let previous_matches = match &pending.previous_target {
        PreviousTarget::Absent => target.is_none(),
        PreviousTarget::Owned => true,
        PreviousTarget::Legacy { identity } => target == Some(identity),
    };
    if !previous_matches {
        return Err(invalid_data(
            "Pending publication conflicts with an unexpected target",
        ));
    }
    Ok(Location::Staged)
}

pub(crate) fn optional<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_manifest::{InstallIdentity, InstallState, RelativePath, SCHEMA_VERSION};
    use sha2::{Digest, Sha256};
    use std::fs;

    #[test]
    fn claims_only_the_published_pending_object_and_keeps_unpublished_legacy_files() {
        let temp = std::env::temp_dir().join(format!("rustdesk-manifest-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(temp.join("data/meta")).unwrap();
        fs::write(temp.join("RustDesk.exe"), "legacy").unwrap();
        fs::write(temp.join("data/meta").join(PAYLOAD_SLOT), "new").unwrap();
        let root = Directory::open(&temp).unwrap();
        let store = Store {
            directory: root
                .descend(&RelativePath::parse("data/meta").unwrap())
                .unwrap(),
        };
        let record = pending_record(&root, &store);
        let unpublished = preview(&root, &store, &record).unwrap();
        assert!(unpublished.files.is_empty());
        assert_eq!(unpublished.migration_targets, record.migration_targets);
        assert_eq!(
            fs::read_to_string(temp.join("RustDesk.exe")).unwrap(),
            "legacy"
        );
        fs::rename(
            temp.join("data/meta").join(PAYLOAD_SLOT),
            temp.join("RustDesk.exe"),
        )
        .unwrap();
        let published = preview(&root, &store, &record).unwrap();
        assert!(published.migration_targets.is_empty());
        assert_eq!(
            published.files,
            [RelativePath::parse("RustDesk.exe").unwrap()]
        );
        fs::write(temp.join("RustDesk.exe"), "changed").unwrap();
        assert!(preview(&root, &store, &record).is_err());
        assert_eq!(
            fs::read_to_string(temp.join("RustDesk.exe")).unwrap(),
            "changed"
        );
        drop(store);
        drop(root);
        fs::remove_file(temp.join("RustDesk.exe")).unwrap();
        fs::remove_dir(temp.join("data/meta")).unwrap();
        fs::remove_dir(temp.join("data")).unwrap();
        fs::remove_dir(temp).unwrap();
    }

    fn pending_record(root: &Directory, store: &Store) -> InstallManifest {
        let path = RelativePath::parse("RustDesk.exe").unwrap();
        let original = identity(&root.entry(&path, Access::Read).unwrap().file).unwrap();
        let staged =
            identity(&open(&store.directory.file, PAYLOAD_SLOT, Access::Read).unwrap()).unwrap();
        InstallManifest {
            schema_version: SCHEMA_VERSION,
            identity: InstallIdentity {
                product: "RustDesk".into(),
                architecture: std::env::consts::ARCH.into(),
                root: identity(&root.file).unwrap(),
            },
            package_version: "1.5.1".into(),
            state: InstallState::Installing,
            files: Vec::new(),
            created_dirs: Vec::new(),
            migration_targets: vec![crate::install_manifest::migration::MigrationTarget {
                path: path.clone(),
                identity: original.clone(),
                sha256: format!("{:x}", Sha256::digest(b"new")),
            }],
            pending_file: Some(PendingFile {
                path,
                identity: staged,
                sha256: format!("{:x}", Sha256::digest(b"new")),
                previous_target: PreviousTarget::Legacy { identity: original },
            }),
        }
    }
}
