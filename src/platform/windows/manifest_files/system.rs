use super::{directory::Directory, files, identity, Access};
use crate::{
    install_manifest::request::{Operation, Request, Script},
    update_hash::sha256_reader_hex,
};
use std::{io, os::windows::process::CommandExt, path::PathBuf, process::Command};
use windows::{
    core::{Owned, PCWSTR},
    Win32::{
        Foundation::{HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::{
            SystemInformation::GetSystemDirectoryW,
            Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject, CREATE_NO_WINDOW},
        },
    },
};
use winreg::{enums::HKEY_LOCAL_MACHINE, RegKey};

const HKLM: &str = "HKEY_LOCAL_MACHINE\\";
const UNINSTALL: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\";
const LEGACY_ID: &str = "{54E86BC2-6C85-41F3-A9EB-1A94AC9B1F93}_is1";

pub(crate) struct OperationLock(Owned<HANDLE>);

impl OperationLock {
    pub(crate) fn acquire(product: &str) -> io::Result<Self> {
        let digest = sha256_reader_hex(&mut product.to_lowercase().as_bytes())?;
        let name: Vec<_> = format!("Global\\RustDeskInstall-{digest}")
            .encode_utf16()
            .chain([0])
            .collect();
        let handle = unsafe { Owned::new(CreateMutexW(None, false, PCWSTR(name.as_ptr()))?) };
        match unsafe { WaitForSingleObject(*handle, 0) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self(handle)),
            WAIT_TIMEOUT => Err(io::Error::other(
                "Another installation operation is running",
            )),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

impl Drop for OperationLock {
    fn drop(&mut self) {
        if let Err(error) = unsafe { ReleaseMutex(*self.0) } {
            eprintln!("Cannot release installation lock: {error}");
        }
    }
}

pub(crate) fn registry_path(request: &Request) -> io::Result<&str> {
    let path = request
        .registry_subkey
        .strip_prefix(HKLM)
        .ok_or_else(|| io::Error::other("Expected machine installation registration"))?;
    let allowed = [
        format!("{UNINSTALL}{}", request.product),
        format!("{UNINSTALL}{LEGACY_ID}"),
        format!(
            "Software\\{}\\InstallState\\{}",
            request.product, request.product
        ),
    ];
    if !allowed.iter().any(|candidate| {
        path.eq_ignore_ascii_case(candidate)
            || path.eq_ignore_ascii_case(&candidate.replacen(
                "Microsoft",
                "Wow6432Node\\Microsoft",
                1,
            ))
    }) {
        return Err(io::Error::other(
            "Registration does not belong to this product",
        ));
    }
    Ok(path)
}

pub(crate) fn verify_registration(request: &Request) -> io::Result<()> {
    let key = match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(registry_path(request)?) {
        Ok(key) => key,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                && request.operation == Operation::Install =>
        {
            return Ok(())
        }
        Err(error) => return Err(error),
    };
    if is_msi(&key)? {
        return Err(io::Error::other(
            "An EXE operation cannot change an MSI installation",
        ));
    }
    if optional(key.get_value::<u32, _>("InstallManifestVersion"))?.is_some()
        && request.expected_manifest_sha256.is_none()
    {
        return Err(io::Error::other(
            "The registered installation manifest is missing",
        ));
    }
    let Some(location) = optional(key.get_value::<String, _>("InstallLocation"))? else {
        return Err(io::Error::other("Registered installation has no location"));
    };
    let registered = Directory::open(std::path::Path::new(&location))?;
    if Some(identity(&registered.file)?) != request.root_identity {
        return Err(io::Error::other(
            "The installation directory cannot be changed",
        ));
    }
    Ok(())
}

fn is_msi(key: &RegKey) -> io::Result<bool> {
    let state = optional(key.get_value::<u32, _>("WindowsInstaller"))?;
    if let Some(state) = state {
        return Ok(state == 1);
    }
    if optional(key.get_value::<String, _>("MsiProductCode"))?.is_some() {
        return Ok(true);
    }
    Ok(optional(key.get_value::<String, _>("UninstallString"))?
        .is_some_and(|value| value.to_ascii_lowercase().contains("msiexec.exe")))
}

pub(crate) fn retain_location(request: &Request) -> io::Result<()> {
    let (key, _) = RegKey::predef(HKEY_LOCAL_MACHINE).create_subkey(registry_path(request)?)?;
    let root = request
        .root
        .to_str()
        .ok_or_else(|| io::Error::other("Invalid installation path"))?;
    key.set_value("InstallLocation", &root)?;
    key.set_value("WindowsInstaller", &0u32)?;
    key.set_value(
        "InstallManifestVersion",
        &crate::install_manifest::SCHEMA_VERSION,
    )
}

pub(crate) fn remove_registration(request: &Request) -> io::Result<()> {
    RegKey::predef(HKEY_LOCAL_MACHINE).delete_subkey(registry_path(request)?)
}

pub(crate) fn verify_script(script: &Script) -> io::Result<super::directory::Entry> {
    let text = script
        .path
        .to_str()
        .ok_or_else(|| io::Error::other("Invalid script path"))?;
    if text
        .chars()
        .any(|ch| matches!(ch, '%' | '!' | '"' | '\r' | '\n'))
    {
        return Err(io::Error::other("Unsafe script path"));
    }
    let mut file = files::open_absolute(&script.path, Access::Read)?;
    files::verify_hash(&mut file.file, &script.sha256)?;
    Ok(file)
}

pub(crate) fn run(script: Option<&Script>) -> io::Result<()> {
    let Some(script) = script else {
        return Ok(());
    };
    let _file = verify_script(script)?;
    let status = Command::new(system_executable("cmd.exe")?)
        .env("RUSTDESK_MANIFEST_OPERATION", "1")
        .args(["/D", "/E:ON", "/V:OFF", "/C"])
        .raw_arg(format!("\"\"{}\"\"", script.path.display()))
        .creation_flags(CREATE_NO_WINDOW.0)
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "System integration failed: {status}"
        )));
    }
    Ok(())
}

pub(crate) fn system_executable(name: &str) -> io::Result<PathBuf> {
    let length = unsafe { GetSystemDirectoryW(None) };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0; length as usize];
    let written = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    if written == 0 || written >= buffer.len() {
        return Err(io::Error::last_os_error());
    }
    let directory = String::from_utf16(&buffer[..written]).map_err(io::Error::other)?;
    Ok(PathBuf::from(directory).join(name))
}

fn optional<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
