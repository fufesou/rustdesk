use hbb_common::{
    bail, log,
    sha2::{Digest, Sha256},
    ResultType,
};
use std::{
    fs,
    path::{Path, PathBuf},
};
use windows::{
    core::{GUID, HSTRING},
    Win32::{
        Devices::DeviceAndDriverInstallation::SetupUninstallOEMInfW,
        Foundation::{GetLastError, ERROR_INF_IN_USE_BY_DEVICES, ERROR_SUCCESS_REBOOT_REQUIRED},
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_Windows, SHGetKnownFolderPath, KF_FLAG_DEFAULT},
    },
};

// Exact INF files from the two build_github_ci_test_sign archives pinned in res/windows_idd.py.
const INF_BYTES: u64 = 4416;
const INF_DIGESTS: &[&str] = &[
    "30e72e61a99204b651f2209012774c0ce83949146e5a984c16f21a585a74578e",
    "60208d8609d0735fa4a50f3fba113ece526d1db81900c31c50b9d26c7816e496",
];

fn known_folder(id: &GUID) -> ResultType<PathBuf> {
    let value = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }?;
    let path = unsafe { value.to_string() };
    unsafe { CoTaskMemFree(Some(value.0.cast())) };
    Ok(PathBuf::from(path?))
}

pub fn uninstall_test_driver(reboot: &mut bool) -> ResultType<()> {
    let inf_dir = known_folder(&FOLDERID_Windows)?.join("INF");
    for package in published_packages(&inf_dir, INF_BYTES, INF_DIGESTS)? {
        *reboot |= remove_unused_package(&package)?;
    }
    Ok(())
}

fn remove_unused_package(name: &str) -> ResultType<bool> {
    let package = HSTRING::from(name);
    if unsafe { SetupUninstallOEMInfW(&package, 0, None) }.as_bool() {
        return Ok(false);
    }
    let error = unsafe { GetLastError() };
    match error {
        ERROR_INF_IN_USE_BY_DEVICES => {
            log::info!("Keeping display driver {name} in use by another device");
            Ok(false)
        }
        ERROR_SUCCESS_REBOOT_REQUIRED => Ok(true),
        _ => bail!(
            "Cannot remove {name}: {}",
            std::io::Error::from_raw_os_error(error.0 as i32)
        ),
    }
}

fn published_packages(dir: &Path, size: u64, digests: &[&str]) -> ResultType<Vec<String>> {
    let mut packages = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        let Some(number) = name
            .strip_prefix("oem")
            .and_then(|s| s.strip_suffix(".inf"))
        else {
            continue;
        };
        if number.is_empty()
            || !number.bytes().all(|b| b.is_ascii_digit())
            || entry.metadata()?.len() != size
        {
            continue;
        }
        let digest = format!("{:x}", Sha256::digest(fs::read(entry.path())?));
        if digests.contains(&digest.as_str()) {
            packages.push(name);
        }
    }
    Ok(packages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_source_finds_only_the_published_test_package() {
        let dir = std::env::temp_dir().join(hbb_common::uuid::Uuid::new_v4().to_string());
        fs::create_dir(&dir).unwrap();
        let contents = b"package A";
        let digest = format!("{:x}", Sha256::digest(contents));
        let find = || published_packages(&dir, contents.len() as u64, &[&digest]).unwrap();
        assert!(find().is_empty());
        fs::write(dir.join("oem11.inf"), b"package B").unwrap();
        fs::write(dir.join("other.inf"), contents).unwrap();
        fs::write(dir.join("oem42.inf"), contents).unwrap();
        assert_eq!(find(), ["oem42.inf"]);
        fs::remove_dir_all(dir).unwrap();
    }
}
