use super::{
    directory::{Directory, Entry},
    identity, open, publish, remove, security,
    store::{cleanup_failed_write, Store, PAYLOAD_SLOT},
    Access,
};
use crate::{
    install_manifest::{
        invalid_data, request::InputFile, FileIdentity, InstallManifest, PreviousTarget,
        RelativePath,
    },
    update_hash::verify_sha256_reader,
};
use std::{fs::File, io, path::Path};

pub(crate) struct Files<'a> {
    pub root: &'a Directory,
    pub store: &'a Store,
}

impl Files<'_> {
    pub(crate) fn install(
        &self,
        record: &InstallManifest,
        input: &InputFile,
    ) -> io::Result<InstallManifest> {
        if !contains(&record.files, &input.path) {
            return Err(invalid_data(
                "File is outside the installation's managed paths",
            ));
        }
        let next = self.ensure_parents(record, &input.path)?;
        let previous = check_target(self.root, input)?;
        self.store.clear_unused_payload(&next)?;
        let staged = self.stage(input)?;
        let (parent, name) = self.root.protected_parent(&input.path)?;
        match previous {
            Some(previous) => publish::replace_verified(&staged, &parent.file, (name, &previous))?,
            None => publish::publish_new(&staged, &parent.file, name)?,
        }
        Ok(next)
    }

    fn stage(&self, input: &InputFile) -> io::Result<File> {
        let mut source = open_absolute(&input.source, Access::Read)?;
        verify_hash(&mut source.file, &input.sha256)?;
        let mut staged = self.store.create_slot(PAYLOAD_SLOT)?;
        let result = (|| {
            io::copy(&mut source.file, &mut staged)?;
            staged.sync_all()?;
            verify_hash(&mut staged, &input.sha256)
        })();
        cleanup_failed_write(result, &staged)?;
        Ok(staged)
    }

    fn ensure_parents(
        &self,
        record: &InstallManifest,
        path: &RelativePath,
    ) -> io::Result<InstallManifest> {
        let mut next = record.clone();
        for (end, _) in path.as_str().match_indices('/') {
            let relative = RelativePath::parse(&path.as_str()[..end]).map_err(invalid_data)?;
            match self.root.descend(&relative) {
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let (parent, name) = self.root.protected_parent(&relative)?;
            parent.create_child(name)?;
            if !contains(&next.created_dirs, &relative) {
                next.created_dirs.push(relative);
            }
            self.store.save(&next)?;
        }
        Ok(next)
    }

    pub(crate) fn remove_owned(
        &self,
        record: &InstallManifest,
        path: &RelativePath,
    ) -> io::Result<()> {
        if !contains(&record.files, path) {
            return Err(invalid_data("File is not owned by this installation"));
        }
        remove_relative(self.root, path)
    }
}

pub(crate) fn check_target(
    root: &Directory,
    input: &InputFile,
) -> io::Result<Option<FileIdentity>> {
    let previous = current(root, &input.path)?;
    let matches = match &input.previous {
        PreviousTarget::Owned => true,
        PreviousTarget::Absent => previous.is_none(),
        PreviousTarget::Legacy { identity } => previous.as_ref() == Some(identity),
    };
    if !matches {
        return Err(invalid_data(format!(
            "Unconfirmed replacement target: {}",
            input.path.as_str()
        )));
    }
    Ok(previous)
}

pub(crate) fn current(root: &Directory, path: &RelativePath) -> io::Result<Option<FileIdentity>> {
    let result = (|| {
        let (parent, name) = root.protected_parent(path)?;
        let file = open(&parent.file, name, Access::Read)?;
        security::require_protected(&file)?;
        identity(&file)
    })();
    match result {
        Ok(identity) => Ok(Some(identity)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn remove_relative(root: &Directory, path: &RelativePath) -> io::Result<()> {
    let result = (|| {
        let (parent, name) = root.protected_parent(path)?;
        let file = open(&parent.file, name, Access::Delete)?;
        remove(&file)
    })();
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

pub(crate) fn contains(files: &[RelativePath], path: &RelativePath) -> bool {
    files.iter().any(|file| same_path(file, path))
}

fn same_path(left: &RelativePath, right: &RelativePath) -> bool {
    left.as_str().eq_ignore_ascii_case(right.as_str())
}

pub(crate) fn open_absolute(path: &Path, access: Access) -> io::Result<Entry> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_data("Missing source parent"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid_data("Invalid source name"))?;
    let name = RelativePath::parse(name).map_err(invalid_data)?;
    Directory::open(parent)?.entry(&name, access)
}

pub(crate) fn verify_hash(file: &mut File, hash: &str) -> io::Result<()> {
    verify_sha256_reader(file, hash)
        .map_err(|error| invalid_data(format!("File content verification failed: {error:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_manifest::{InstallIdentity, InstallState, SCHEMA_VERSION};
    use sha2::{Digest, Sha256};
    use std::{fs, io::Write};

    #[test]
    #[ignore = "requires an elevated Windows token to create administrator-owned fixtures"]
    fn publishes_updates_and_removes_owned_files_without_claiming_unknown_files() {
        let parent =
            std::env::temp_dir().join(format!("rustdesk-manifest-{}", uuid::Uuid::new_v4()));
        let temp = parent.join("nested/root");
        let root = Directory::create(&temp).unwrap();
        let (store, dirs) = Store::create(&root).unwrap();
        let record = initial(&root, dirs);
        store.save(&record).unwrap();
        fs::write(temp.join("source.bin"), b"new application").unwrap();
        fs::write(temp.join("unknown.txt"), b"preserve").unwrap();
        let files = Files {
            root: &root,
            store: &store,
        };
        let input = input(&temp, PreviousTarget::Absent, b"new application");
        let installed = files.install(&record, &input).unwrap();
        assert_eq!(
            fs::read(temp.join("RustDesk.exe")).unwrap(),
            b"new application"
        );
        assert_eq!(
            store.load(&record.identity).unwrap().files,
            [input.path.clone()]
        );
        fs::write(temp.join("source.bin"), b"older application").unwrap();
        let old_input = input_for_update(&temp);
        let updated = files.install(&installed, &old_input).unwrap();
        assert_eq!(
            fs::read(temp.join("RustDesk.exe")).unwrap(),
            b"older application"
        );
        let held = root.entry(&input.path, Access::Read).unwrap();
        assert!(files.remove_owned(&updated, &input.path).is_err());
        assert_eq!(store.load(&record.identity).unwrap(), updated);
        drop(held);
        files.remove_owned(&updated, &input.path).unwrap();
        files.remove_owned(&updated, &input.path).unwrap();
        assert_eq!(store.load(&record.identity).unwrap(), updated);
        assert!(!temp.join("RustDesk.exe").exists());
        assert_eq!(fs::read(temp.join("unknown.txt")).unwrap(), b"preserve");
        store.delete().unwrap();
        super::super::artifacts::remove_empty(&root, &updated.created_dirs).unwrap();
        drop(root);
        fs::remove_file(temp.join("source.bin")).unwrap();
        fs::remove_file(temp.join("unknown.txt")).unwrap();
        fs::remove_dir(temp).unwrap();
        fs::remove_dir(parent.join("nested")).unwrap();
        fs::remove_dir(parent).unwrap();
    }

    fn initial(root: &Directory, dirs: Vec<RelativePath>) -> InstallManifest {
        InstallManifest {
            schema_version: SCHEMA_VERSION,
            identity: InstallIdentity {
                product: "RustDesk".into(),
                architecture: std::env::consts::ARCH.into(),
                root: identity(&root.file).unwrap(),
            },
            package_version: "2.0".into(),
            state: InstallState::Installing,
            files: vec![RelativePath::parse("RustDesk.exe").unwrap()],
            created_dirs: dirs,
            pending_file: None,
            migration_targets: Vec::new(),
        }
    }

    fn input(root: &Path, previous: PreviousTarget, bytes: &[u8]) -> InputFile {
        InputFile {
            path: RelativePath::parse("RustDesk.exe").unwrap(),
            source: root.join("source.bin"),
            sha256: format!("{:x}", Sha256::digest(bytes)),
            previous,
        }
    }

    fn input_for_update(root: &Path) -> InputFile {
        input(root, PreviousTarget::Owned, b"older application")
    }

    #[test]
    #[ignore = "requires an elevated Windows token to create administrator-owned fixtures"]
    fn failed_copy_keeps_adopted_legacy_paths_available_for_uninstallation() {
        let temp = std::env::temp_dir().join(format!("rustdesk-manifest-{}", uuid::Uuid::new_v4()));
        let root = Directory::create(&temp).unwrap();
        let (store, dirs) = Store::create(&root).unwrap();
        let record = initial(&root, dirs);
        let mut legacy = security::Descriptor::installer()
            .unwrap()
            .create(&root.file, "RustDesk.exe", Access::Create)
            .unwrap();
        legacy.write_all(b"legacy application").unwrap();
        let previous = PreviousTarget::Legacy {
            identity: identity(&legacy).unwrap(),
        };
        drop(legacy);
        fs::write(temp.join("source.bin"), b"changed source").unwrap();
        let input = input(&temp, previous, b"expected application");
        let files = Files {
            root: &root,
            store: &store,
        };
        assert!(files.install(&record, &input).is_err());
        assert_eq!(
            fs::read(temp.join("RustDesk.exe")).unwrap(),
            b"legacy application"
        );
        store.save(&record).unwrap();
        assert!(files.install(&record, &input).is_err());
        assert_eq!(store.load(&record.identity).unwrap(), record);
        assert_eq!(
            fs::read(temp.join("RustDesk.exe")).unwrap(),
            b"legacy application"
        );
        files.remove_owned(&record, &input.path).unwrap();
        assert!(!temp.join("RustDesk.exe").exists());
        store.delete().unwrap();
        super::super::artifacts::remove_empty(&root, &record.created_dirs).unwrap();
        drop(root);
        fs::remove_file(temp.join("source.bin")).unwrap();
        fs::remove_dir(temp).unwrap();
    }
}
