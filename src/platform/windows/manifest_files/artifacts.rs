use super::{
    directory::Directory,
    files, identity, open, publish, remove,
    store::{cleanup_failed_write, Store, PAYLOAD_SLOT},
    system, Access,
};
use crate::install_manifest::{FileIdentity, InstallManifest, RelativePath};
use std::{
    io,
    path::{Path, PathBuf},
};
use windows::{
    core::GUID,
    Win32::{
        Foundation::ERROR_DIR_NOT_EMPTY,
        System::Com::CoTaskMemFree,
        UI::Shell::{
            FOLDERID_CommonPrograms, FOLDERID_CommonStartup, FOLDERID_PublicDesktop,
            SHGetKnownFolderPath, KF_FLAG_DEFAULT,
        },
    },
};

const BROKER: &str = "RuntimeBroker_rustdesk.exe";

pub(crate) fn verify(root: &Directory, product: &str) -> io::Result<()> {
    for path in [BROKER.to_owned(), format!("Uninstall {product}.lnk")] {
        files::current(root, &relative(&path)?)?;
    }
    for path in shortcuts(product)? {
        missing_ok(files::open_absolute(&path, Access::Read).map(|_| ()))?;
    }
    Ok(())
}

pub(crate) fn install_broker(
    root: &Directory,
    store: &Store,
    record: &InstallManifest,
) -> io::Result<()> {
    let source = system::system_executable("RuntimeBroker.exe")?;
    let mut source = match files::open_absolute(&source, Access::Read) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            eprintln!(
                "RuntimeBroker is unavailable on this Windows version; no broker is installed"
            );
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let previous = files::current(root, &relative(BROKER)?)?;
    store.clear_unused_payload(record)?;
    let mut staged = store.create_slot(PAYLOAD_SLOT)?;
    let result = (|| {
        io::copy(&mut source.file, &mut staged)?;
        staged.sync_all()?;
        match previous {
            Some(previous) => publish::replace_verified(&staged, &root.file, (BROKER, &previous)),
            None => publish::publish_new(&staged, &root.file, BROKER),
        }
    })();
    cleanup_failed_write(result, &staged)
}

pub(crate) fn remove_known(root: &Directory, product: &str) -> io::Result<()> {
    for path in [BROKER.to_owned(), format!("Uninstall {product}.lnk")] {
        files::remove_relative(root, &relative(&path)?)?;
    }
    for path in shortcuts(product)? {
        let result =
            files::open_absolute(&path, Access::Delete).and_then(|entry| remove(&entry.file));
        missing_ok(result)?;
    }
    remove_empty_absolute(&known_folder(&FOLDERID_CommonPrograms)?.join(product), None)
}

pub(crate) fn remove_empty(root: &Directory, directories: &[RelativePath]) -> io::Result<()> {
    let mut paths: Vec<_> = directories.iter().collect();
    paths.sort_by_key(|path| std::cmp::Reverse(path.as_str().len()));
    for path in paths {
        let result = root.protected_parent(path).and_then(|(parent, name)| {
            let directory = open(&parent.file, name, Access::RemoveDirectory)?;
            remove(&directory)
        });
        nonempty_ok(result)?;
    }
    Ok(())
}

pub(crate) fn remove_empty_absolute(
    path: &Path,
    expected: Option<&FileIdentity>,
) -> io::Result<()> {
    let result = files::open_absolute(path, Access::RemoveDirectory).and_then(|entry| {
        if let Some(expected) = expected {
            if identity(&entry.file)? != *expected {
                return Err(io::Error::other(
                    "Installation root changed during final cleanup",
                ));
            }
        }
        remove(&entry.file)
    });
    nonempty_ok(result)
}

fn shortcuts(product: &str) -> io::Result<Vec<PathBuf>> {
    let programs = known_folder(&FOLDERID_CommonPrograms)?.join(product);
    Ok(vec![
        known_folder(&FOLDERID_PublicDesktop)?.join(format!("{product}.lnk")),
        known_folder(&FOLDERID_CommonStartup)?.join(format!("{product} Tray.lnk")),
        programs.join(format!("{product}.lnk")),
        programs.join(format!("Uninstall {product}.lnk")),
    ])
}

fn known_folder(id: &GUID) -> io::Result<PathBuf> {
    let value = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }?;
    let text = unsafe { value.to_string() }.map_err(io::Error::other);
    unsafe { CoTaskMemFree(Some(value.0.cast())) };
    Ok(PathBuf::from(text?))
}

fn relative(path: &str) -> io::Result<RelativePath> {
    RelativePath::parse(path).map_err(io::Error::other)
}

fn missing_ok(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn nonempty_ok(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.raw_os_error() == Some(ERROR_DIR_NOT_EMPTY.0 as i32) => Ok(()),
        result => missing_ok(result),
    }
}
