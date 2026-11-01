use super::{directory::Directory, identity, open, publish, remove, security, Access};
use crate::install_manifest::{InstallIdentity, InstallManifest, RelativePath};
use std::{
    fs::File,
    io::{self, Read, Write},
};

pub(crate) const RECEIPT_PATH: &str = "data/meta/install-manifest.json";
pub(crate) const PAYLOAD_SLOT: &str = "install-file.tmp";
const RECEIPT_NAME: &str = "install-manifest.json";
const RECEIPT_SLOT: &str = "install-manifest.next";

pub(crate) struct Store {
    pub directory: Directory,
}

impl Store {
    pub(crate) fn open(root: &Directory) -> io::Result<Self> {
        let path = RelativePath::parse(RECEIPT_PATH).map_err(io::Error::other)?;
        let (directory, _) = root.protected_parent(&path)?;
        Ok(Self { directory })
    }

    pub(crate) fn create(root: &Directory) -> io::Result<(Self, Vec<RelativePath>)> {
        security::require_protected(&root.file)?;
        let (data, data_created) = ensure_child(root, "data")?;
        let (directory, meta_created) = ensure_child(&data, "meta")?;
        for name in [RECEIPT_NAME, RECEIPT_SLOT, PAYLOAD_SLOT] {
            match open(&directory.file, name, Access::Read) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("Unclaimed installer control file: {name}"),
                    ))
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let mut created = Vec::new();
        for (path, was_created) in [("data", data_created), ("data/meta", meta_created)] {
            if was_created {
                created.push(RelativePath::parse(path).map_err(io::Error::other)?);
            }
        }
        Ok((Self { directory }, created))
    }

    pub(crate) fn load(&self, expected: &InstallIdentity) -> io::Result<InstallManifest> {
        let mut file = open(&self.directory.file, RECEIPT_NAME, Access::Read)?;
        security::require_protected(&file)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        InstallManifest::from_bytes(&bytes, expected)
    }

    pub(crate) fn save(&self, manifest: &InstallManifest) -> io::Result<()> {
        let bytes = manifest.to_bytes()?;
        match self.load(&manifest.identity) {
            Ok(_) => self.remove_control(RECEIPT_SLOT)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut staged = self.create_slot(RECEIPT_SLOT)?;
        let result = (|| {
            staged.write_all(&bytes)?;
            staged.sync_all()?;
            match open(&self.directory.file, RECEIPT_NAME, Access::Read) {
                Ok(previous) => {
                    let previous_identity = identity(&previous)?;
                    drop(previous);
                    publish::replace_verified(
                        &staged,
                        &self.directory.file,
                        (RECEIPT_NAME, &previous_identity),
                    )
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    publish::publish_new(&staged, &self.directory.file, RECEIPT_NAME)
                }
                Err(error) => Err(error),
            }
        })();
        cleanup_failed_write(result, &staged)
    }

    pub(crate) fn create_slot(&self, name: &str) -> io::Result<File> {
        security::Descriptor::installer()?.create(&self.directory.file, name, Access::Create)
    }

    pub(crate) fn clear_unused_payload(&self, record: &InstallManifest) -> io::Result<()> {
        let persisted = self.load(&record.identity)?;
        if persisted != *record || persisted.pending_file.is_some() {
            return Err(io::Error::other(
                "Cannot remove staging while ownership is unresolved",
            ));
        }
        self.remove_control(PAYLOAD_SLOT)
    }

    fn remove_control(&self, name: &str) -> io::Result<()> {
        let file = match open(&self.directory.file, name, Access::Delete) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        security::require_protected(&file)?;
        remove(&file)
    }

    pub(crate) fn delete(self) -> io::Result<()> {
        self.remove_control(RECEIPT_SLOT)?;
        let file = open(&self.directory.file, RECEIPT_NAME, Access::Delete)?;
        remove(&file)
    }
}

fn ensure_child(parent: &Directory, name: &str) -> io::Result<(Directory, bool)> {
    let path = RelativePath::parse(name).map_err(io::Error::other)?;
    let result = match parent.descend(&path) {
        Ok(directory) => (directory, false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (parent.create_child(name)?, true),
        Err(error) => return Err(error),
    };
    security::require_protected(&result.0.file)?;
    Ok(result)
}

pub(super) fn cleanup_failed_write(result: io::Result<()>, staged: &File) -> io::Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) => match remove(staged) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(io::Error::other(format!(
                "{error}; staged file cleanup also failed: {cleanup}"
            ))),
        },
    }
}
