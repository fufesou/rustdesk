use super::installer_shell::{get_known_folder, get_system_executable};
use hbb_common::{bail, ResultType};
use sha2::{Digest, Sha256};
use std::{fs, os::windows::process::CommandExt, path::Path, process::Command};
use windows::Win32::{
    Foundation::ERROR_SUCCESS_REBOOT_REQUIRED, System::Threading::CREATE_NO_WINDOW,
    UI::Shell::FOLDERID_Windows,
};

// Exact INF files from the two build_github_ci_test_sign archives pinned in res/windows_idd.py.
const INF_BYTES: u64 = 4416;
const INF_DIGESTS: &[&str] = &[
    "30e72e61a99204b651f2209012774c0ce83949146e5a984c16f21a585a74578e",
    "60208d8609d0735fa4a50f3fba113ece526d1db81900c31c50b9d26c7816e496",
];

pub(crate) fn uninstall(reboot: &mut bool) -> ResultType<()> {
    let inf_dir = get_known_folder(&FOLDERID_Windows)?.join("INF");
    for package in published_packages(&inf_dir, INF_BYTES, INF_DIGESTS)? {
        let output = Command::new(get_system_executable("pnputil.exe")?)
            .args(["/delete-driver", &package, "/uninstall"])
            .creation_flags(CREATE_NO_WINDOW.0)
            .output()?;
        match output.status.code() {
            Some(0) => {}
            Some(code) if code == ERROR_SUCCESS_REBOOT_REQUIRED.0 as i32 => *reboot = true,
            _ => bail!(
                "Failed to uninstall {package} ({}): {} {}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        }
    }
    Ok(())
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
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
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
