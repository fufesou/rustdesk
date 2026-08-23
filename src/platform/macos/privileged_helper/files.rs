use super::{
    clear_extended_acl, validate_helper_tree, validate_owned_directory, ExpectedOwner, HelperPaths,
    HelperSource, CUSTOM_MODE, SERVICE_MODE,
};
use hbb_common::{anyhow::anyhow, bail, ResultType};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
pub(super) struct OwnedFileOptions {
    pub(super) owner: ExpectedOwner,
    pub(super) mode: u32,
}

pub(super) fn create_owned_directory(
    path: &Path,
    owner: ExpectedOwner,
    mode: u32,
) -> ResultType<()> {
    std::fs::DirBuilder::new().mode(mode).create(path)?;
    harden_owned_directory(path, owner, mode)
}

pub(super) fn harden_owned_directory(
    path: &Path,
    owner: ExpectedOwner,
    mode: u32,
) -> ResultType<()> {
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(hbb_common::libc::O_NOFOLLOW | hbb_common::libc::O_DIRECTORY)
        .open(path)?;
    let opened = directory.metadata()?;
    if !opened.file_type().is_dir() {
        bail!("Expected a real directory: {}", path.display());
    }
    set_file_owner(&directory, owner)?;
    clear_extended_acl(&directory)?;
    directory.set_permissions(std::fs::Permissions::from_mode(mode))?;
    directory.sync_all()?;
    let hardened = directory.metadata()?;
    let linked = std::fs::symlink_metadata(path)?;
    if linked.file_type().is_symlink()
        || !linked.file_type().is_dir()
        || hardened.dev() != linked.dev()
        || hardened.ino() != linked.ino()
    {
        bail!("Directory path identity changed: {}", path.display());
    }
    validate_owned_directory(path, owner)?;
    if hardened.mode() & 0o7777 != mode {
        bail!("Unexpected directory mode: {}", path.display());
    }
    Ok(())
}

pub(super) fn create_staged_helper(
    paths: &HelperPaths,
    source: HelperSource<'_>,
    owner: ExpectedOwner,
) -> ResultType<()> {
    create_owned_directory(&paths.privileged_tools, owner, 0o700)?;
    for directory in [
        &paths.bundle,
        &paths.contents,
        &paths.macos,
        &paths.resources,
    ] {
        create_owned_directory(directory, owner, 0o755)?;
    }
    copy_owned_regular(
        source.service,
        &paths.service,
        OwnedFileOptions {
            owner,
            mode: SERVICE_MODE,
        },
    )?;
    if let Some(custom) = source.custom {
        copy_owned_regular(
            custom,
            &paths.custom,
            OwnedFileOptions {
                owner,
                mode: CUSTOM_MODE,
            },
        )?;
    }
    validate_helper_tree(paths, owner)
}

pub(super) fn read_regular_file(path: &Path) -> ResultType<Vec<u8>> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(hbb_common::libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        bail!("Expected a regular file: {}", path.display());
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub(super) fn legacy_custom_path(service: &Path) -> ResultType<Option<PathBuf>> {
    let Some(contents) = service.parent().and_then(Path::parent) else {
        bail!("Legacy service path has no Contents directory");
    };
    let custom = contents.join("Resources/custom.txt");
    match std::fs::symlink_metadata(&custom) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(Some(custom)),
        Ok(_) => bail!("Legacy custom.txt is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn helper_contents_match(
    left: &super::HelperPaths,
    right: &super::HelperPaths,
) -> ResultType<bool> {
    if read_regular_file(&left.service)? != read_regular_file(&right.service)? {
        return Ok(false);
    }
    Ok(read_optional_regular_file(&left.custom)? == read_optional_regular_file(&right.custom)?)
}

fn read_optional_regular_file(path: &Path) -> ResultType<Option<Vec<u8>>> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(Some(read_regular_file(path)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn write_owned_atomically(
    path: &Path,
    bytes: &[u8],
    options: OwnedFileOptions,
) -> ResultType<()> {
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let result = (|| -> ResultType<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(options.mode)
            .open(&temporary)?;
        file.write_all(bytes)?;
        set_file_owner(&file, options.owner)?;
        clear_extended_acl(&file)?;
        file.set_permissions(std::fs::Permissions::from_mode(options.mode))?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        sync_parent(path)
    })();
    let Err(operation_error) = result else {
        return Ok(());
    };
    match std::fs::remove_file(&temporary) {
        Ok(()) => Err(operation_error),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(operation_error),
        Err(cleanup_error) => Err(anyhow!(
            "Atomic write failed ({operation_error}); temporary cleanup failed ({cleanup_error})"
        )),
    }
}

pub(super) fn sync_parent(path: &Path) -> ResultType<()> {
    let Some(parent) = path.parent() else {
        bail!("Path has no parent directory: {}", path.display());
    };
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

pub(super) fn reject_existing_path(path: &Path) -> ResultType<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => bail!("Protected path already exists: {}", path.display()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn copy_owned_regular(
    source: &Path,
    destination: &Path,
    options: OwnedFileOptions,
) -> ResultType<()> {
    let mut input = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(hbb_common::libc::O_NOFOLLOW)
        .open(source)?;
    if !input.metadata()?.file_type().is_file() {
        bail!("Helper source is not a regular file: {}", source.display());
    }
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(options.mode)
        .open(destination)?;
    std::io::copy(&mut input, &mut output)?;
    set_file_owner(&output, options.owner)?;
    clear_extended_acl(&output)?;
    output.set_permissions(std::fs::Permissions::from_mode(options.mode))?;
    output.sync_all()?;
    Ok(())
}

fn set_file_owner(file: &std::fs::File, owner: ExpectedOwner) -> ResultType<()> {
    let result = unsafe { hbb_common::libc::fchown(file.as_raw_fd(), owner.uid, owner.gid) };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
