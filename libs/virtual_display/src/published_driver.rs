use hbb_common::{
    anyhow::Context,
    bail, log,
    sha2::{Digest, Sha256},
    ResultType,
};
use std::{
    fs::File,
    io::{self, Read},
    os::windows::io::AsRawHandle,
    path::{Path, PathBuf},
};
use windows::{
    core::{GUID, HRESULT, HSTRING, PCWSTR},
    Win32::{
        Devices::DeviceAndDriverInstallation::{
            SetupCopyOEMInfW, SetupUninstallOEMInfW, SPOST_PATH, SP_COPY_NOOVERWRITE,
        },
        Foundation::{
            GetLastError, ERROR_FILE_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_INF_IN_USE_BY_DEVICES,
            ERROR_SUCCESS_REBOOT_REQUIRED, HANDLE, MAX_PATH,
        },
        Storage::FileSystem::{FileIdInfo, GetFileInformationByHandleEx, FILE_ID_INFO},
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_Windows, SHGetKnownFolderPath, KF_FLAG_DEFAULT},
    },
};
use winreg::{
    enums::{HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE, KEY_WOW64_64KEY},
    types::FromRegValue,
    RegKey,
};

// Exact INF files from the two build_github_ci_test_sign archives pinned in res/windows_idd.py.
const INF_BYTES: u64 = 4416;
const INF_DIGESTS: &[&str] = &[
    "30e72e61a99204b651f2209012774c0ce83949146e5a984c16f21a585a74578e",
    "60208d8609d0735fa4a50f3fba113ece526d1db81900c31c50b9d26c7816e496",
];
const OWNED_PACKAGES: &str = r"SOFTWARE\RustDesk\TestIddPackages";

fn receipt_key(installed_exe: &Path) -> ResultType<String> {
    if !installed_exe.is_absolute() {
        bail!("Expected an absolute installed executable path");
    }
    let path = installed_exe
        .to_str()
        .context("Invalid installed executable path")?
        .replace('/', r"\")
        .to_lowercase();
    // The installer and current_exe can use different spellings of the same Windows path.
    let path = if let Some(unc) = path.strip_prefix(r"\\?\unc\") {
        format!(r"\\{unc}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned()
    };
    Ok(format!(
        r"{OWNED_PACKAGES}\{:x}",
        Sha256::digest(path.as_bytes())
    ))
}

fn known_folder(id: &GUID) -> ResultType<PathBuf> {
    let value = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }?;
    let path = unsafe { value.to_string() };
    unsafe { CoTaskMemFree(Some(value.0.cast())) };
    Ok(PathBuf::from(path?))
}

pub fn stage_test_driver(source: &Path) -> ResultType<()> {
    if !matches_inf(&File::open(source)?, INF_DIGESTS)? {
        return Ok(());
    }
    let owner_key = receipt_key(&std::env::current_exe()?)?;
    let mut published = [0u16; MAX_PATH as usize];
    let copied = unsafe {
        SetupCopyOEMInfW(
            &HSTRING::from(source.as_os_str()),
            PCWSTR::null(),
            SPOST_PATH,
            SP_COPY_NOOVERWRITE,
            Some(&mut published),
            None,
            None,
        )
    };
    if let Err(error) = copied {
        if error.code() == HRESULT::from_win32(ERROR_FILE_EXISTS.0) {
            return Ok(());
        }
        return Err(error).context("Stage RustDesk IDD package");
    }
    let path = PathBuf::from(unsafe { PCWSTR(published.as_ptr()).to_string()? });
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .context("Missing published INF name")?;
    let name = name.to_ascii_lowercase();
    check_published_name(&name)?;
    if let Err(error) = record_package(&path, &name, &owner_key) {
        match remove_unused_package(&name) {
            Ok(PackageRemoval::Removed { .. }) => {}
            Ok(PackageRemoval::InUse) => {
                return Err(error).with_context(|| format!("Cannot record in-use package {name}"));
            }
            Err(cleanup) => {
                bail!("Cannot record newly staged {name}: {error:#}. Cleanup failed: {cleanup:#}");
            }
        }
        return Err(error).with_context(|| format!("Cannot record newly staged {name}"));
    }
    Ok(())
}

fn record_package(path: &Path, name: &str, owner_key: &str) -> ResultType<()> {
    let identity = file_identity(&File::open(path)?)?;
    let (key, _) = RegKey::predef(HKEY_LOCAL_MACHINE)
        .create_subkey_with_flags(owner_key, KEY_SET_VALUE | KEY_WOW64_64KEY)?;
    key.set_value(identity, &name)?;
    Ok(())
}

pub fn uninstall_test_driver(reboot: &mut bool) -> ResultType<()> {
    uninstall_test_driver_for(&std::env::current_exe()?, reboot)
}

pub fn uninstall_test_driver_for(installed_exe: &Path, reboot: &mut bool) -> ResultType<()> {
    // Flat values from older builds have no recorded product owner.
    let key = match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(
        receipt_key(installed_exe)?,
        KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_WOW64_64KEY,
    ) {
        Ok(key) => key,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let inf_dir = known_folder(&FOLDERID_Windows)?.join("INF");
    let receipts = key.enum_values().collect::<io::Result<Vec<_>>>()?;
    for (identity, value) in receipts {
        let name = String::from_reg_value(&value)?;
        check_published_name(&name)?;
        if owns_package(&inf_dir.join(&name), &identity, INF_DIGESTS)? {
            match remove_unused_package(&name)? {
                PackageRemoval::InUse => continue,
                PackageRemoval::Removed { reboot_required } => *reboot |= reboot_required,
            }
        }
        match key.delete_value(identity) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
    }
    Ok(())
}

enum PackageRemoval {
    InUse,
    Removed { reboot_required: bool },
}

fn remove_unused_package(name: &str) -> ResultType<PackageRemoval> {
    let package = HSTRING::from(name);
    if unsafe { SetupUninstallOEMInfW(&package, 0, None) }.as_bool() {
        return Ok(PackageRemoval::Removed {
            reboot_required: false,
        });
    }
    let error = unsafe { GetLastError() };
    match error {
        ERROR_INF_IN_USE_BY_DEVICES => {
            log::info!("Keeping display driver {name} because a device still uses it");
            Ok(PackageRemoval::InUse)
        }
        ERROR_FILE_NOT_FOUND | ERROR_SUCCESS_REBOOT_REQUIRED => Ok(PackageRemoval::Removed {
            reboot_required: error == ERROR_SUCCESS_REBOOT_REQUIRED,
        }),
        _ => bail!(
            "Cannot remove {name}: {}",
            std::io::Error::from_raw_os_error(error.0 as i32)
        ),
    }
}

fn check_published_name(name: &str) -> ResultType<()> {
    if name
        .strip_prefix("oem")
        .and_then(|s| s.strip_suffix(".inf"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    {
        return Ok(());
    }
    bail!("Invalid published INF name {name}")
}

fn file_identity(file: &File) -> ResultType<String> {
    let mut info = FILE_ID_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle()),
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )?;
    }
    Ok(format!(
        "{:016x}-{:032x}",
        info.VolumeSerialNumber,
        u128::from_le_bytes(info.FileId.Identifier)
    ))
}

fn matches_inf(file: &File, digests: &[&str]) -> ResultType<bool> {
    if file.metadata()?.len() != INF_BYTES {
        return Ok(false);
    }
    let mut contents = Vec::new();
    file.take(INF_BYTES + 1).read_to_end(&mut contents)?;
    let digest = format!("{:x}", Sha256::digest(contents));
    Ok(digests.contains(&digest.as_str()))
}

fn owns_package(path: &Path, identity: &str, digests: &[&str]) -> ResultType<bool> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    Ok(file_identity(&file)? == identity && matches_inf(&file, digests)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn product_uninstall_uses_the_same_receipts_only_for_the_same_installed_executable() {
        let app = receipt_key(Path::new(r"C:\Custom Clients\Alpha.exe")).unwrap();
        assert_eq!(
            app,
            receipt_key(Path::new(r"\\?\C:\custom clients\ALPHA.EXE")).unwrap()
        );
        for other in [
            r"C:\Custom Clients\Beta.exe",
            r"D:\Custom Clients\Alpha.exe",
        ] {
            assert_ne!(app, receipt_key(Path::new(other)).unwrap());
        }
        assert_ne!(app, OWNED_PACKAGES);
        assert!(receipt_key(Path::new("Alpha.exe")).is_err());
    }

    #[test]
    fn missing_source_only_removes_the_test_package_staged_by_rustdesk() {
        let dir = std::env::temp_dir().join(hbb_common::uuid::Uuid::new_v4().to_string());
        fs::create_dir(&dir).unwrap();
        let contents = vec![b'A'; INF_BYTES as usize];
        let digest = format!("{:x}", Sha256::digest(&contents));
        let owned = dir.join("oem42.inf");
        let foreign = dir.join("oem73.inf");
        fs::write(&owned, &contents).unwrap();
        fs::write(&foreign, &contents).unwrap();
        let identity = file_identity(&File::open(&owned).unwrap()).unwrap();
        let eligible = |path: &Path| owns_package(path, &identity, &[&digest]).unwrap();
        assert!(eligible(&owned));
        assert!(!eligible(&foreign));
        fs::write(&owned, vec![b'B'; contents.len()]).unwrap();
        assert!(!eligible(&owned));
        fs::rename(&owned, dir.join("original.inf")).unwrap();
        assert!(!eligible(&owned));
        fs::rename(&foreign, &owned).unwrap();
        assert!(!eligible(&owned));
        fs::remove_dir_all(dir).unwrap();
    }
}
