use super::{directory::Directory, files, identity, security, store::Store};
use crate::{
    install_manifest::{
        collection::{FileChanges, Replacement},
        request::{Operation, Request},
        InstallIdentity, InstallManifest, InstallState, PreviousTarget, RelativePath,
        SCHEMA_VERSION,
    },
    update_hash::sha256_reader_hex,
};
use std::{io, path::Path};

pub(crate) struct Snapshot {
    pub root: Option<Directory>,
    pub record: Option<InstallManifest>,
    pub digest: Option<String>,
}

pub(crate) fn read(path: &Path, product: &str) -> io::Result<Snapshot> {
    let root = match Directory::open(path) {
        Ok(root) => root,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Snapshot {
                root: None,
                record: None,
                digest: None,
            });
        }
        Err(error) => return Err(error),
    };
    security::require_protected(&root.file)?;
    let expected = installation_identity(&root, product)?;
    let loaded = Store::open(&root).and_then(|store| {
        let record = store.load(&expected)?;
        let digest = hash(&record)?;
        Ok((record, digest))
    });
    let (record, digest) = match loaded {
        Ok((record, digest)) => (Some(record), Some(digest)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (None, None),
        Err(error) => return Err(error),
    };
    Ok(Snapshot {
        root: Some(root),
        record,
        digest,
    })
}

pub(crate) fn installation_identity(
    root: &Directory,
    product: &str,
) -> io::Result<InstallIdentity> {
    Ok(InstallIdentity {
        product: product.to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        root: identity(&root.file)?,
    })
}

pub(crate) fn empty(root: &Directory, request: &Request) -> io::Result<InstallManifest> {
    Ok(InstallManifest {
        schema_version: SCHEMA_VERSION,
        identity: installation_identity(root, &request.product)?,
        package_version: request.package_version.clone(),
        state: InstallState::Installing,
        files: Vec::new(),
        created_dirs: Vec::new(),
        pending_file: None,
        migration_targets: Vec::new(),
    })
}

pub(crate) fn hash(record: &InstallManifest) -> io::Result<String> {
    sha256_reader_hex(&mut record.to_bytes()?.as_slice())
}

pub(crate) fn verify(request: &Request, snapshot: &Snapshot) -> io::Result<()> {
    let actual_root = snapshot
        .root
        .as_ref()
        .map(|root| identity(&root.file))
        .transpose()?;
    if actual_root != request.root_identity || snapshot.digest != request.expected_manifest_sha256 {
        return Err(io::Error::other(
            "Installation changed after confirmation; retry the operation",
        ));
    }
    if request.operation == Operation::Uninstall && snapshot.record.is_none() {
        return Err(io::Error::other(
            "A valid installation manifest is required before uninstallation",
        ));
    }
    let old = snapshot
        .record
        .as_ref()
        .map(|record| record.files.as_slice())
        .unwrap_or(&[]);
    let incoming: Vec<_> = request.files.iter().map(|file| file.path.clone()).collect();
    if request.operation != Operation::Uninstall {
        FileChanges::new(old, &incoming, replacement(request))?;
    }
    if let Some(root) = &snapshot.root {
        let empty = empty(root, request)?;
        let record = snapshot.record.as_ref().unwrap_or(&empty);
        verify_targets(root, record, request)?;
    }
    for input in &request.files {
        let mut source = files::open_absolute(&input.source, super::Access::Read)?;
        files::verify_hash(&mut source.file, &input.sha256)?;
    }
    Ok(())
}

fn verify_targets(root: &Directory, record: &InstallManifest, request: &Request) -> io::Result<()> {
    if record.state == InstallState::Uninstalling && request.operation != Operation::Uninstall {
        return Err(io::Error::other(
            "Finish the interrupted uninstallation before reinstalling",
        ));
    }
    if request.operation != Operation::Uninstall {
        for input in &request.files {
            if files::contains(&record.files, &input.path)
                != matches!(input.previous, PreviousTarget::Owned)
            {
                return Err(io::Error::other(
                    "Replacement ownership changed; retry the operation",
                ));
            }
            verify_parents(root, &input.path)?;
            files::check_target(root, input)?;
        }
    }
    for path in &record.files {
        verify_parents(root, path)?;
        files::current(root, path)?;
    }
    Ok(())
}

fn verify_parents(root: &Directory, path: &RelativePath) -> io::Result<()> {
    for (end, _) in path.as_str().match_indices('/') {
        let relative = RelativePath::parse(&path.as_str()[..end]).map_err(io::Error::other)?;
        match root.descend(&relative) {
            Ok(directory) => security::require_protected(&directory.file)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub(crate) fn replacement(request: &Request) -> Replacement {
    if request.preserve_unlisted {
        Replacement::Overlay
    } else {
        Replacement::Complete
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_manifest::request::REQUEST_VERSION;

    #[test]
    fn refuses_uninstallation_without_an_ownership_record() {
        let request = Request {
            schema_version: REQUEST_VERSION, operation: Operation::Uninstall,
            product: "RustDesk".into(), architecture: std::env::consts::ARCH.into(),
            root: std::env::temp_dir().join("unregistered-rustdesk"), root_identity: None,
            registry_subkey: "HKEY_LOCAL_MACHINE\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\RustDesk".into(),
            expected_manifest_sha256: None, package_version: "1.0".into(),
            preserve_unlisted: false, files: Vec::new(), before: None, after: None, show_result: false,
        };
        let request = Request::from_bytes(&request.to_bytes().unwrap()).unwrap();
        let missing = Snapshot {
            root: None,
            record: None,
            digest: None,
        };
        assert!(verify(&request, &missing).is_err());
    }
}
