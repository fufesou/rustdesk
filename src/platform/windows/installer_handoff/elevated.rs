use super::{
    directory, elevated_install_failure_reason, get_system_executable, path_for_cmd_environment,
    write_install_script, InstallCommandScript, InstallCommands, ResultType, CMD_RELATIVE_PATH,
};
use hbb_common::{anyhow::anyhow, bail, log};
use std::{fs, io, os::windows::process::CommandExt, process::Command};
use windows::Win32::System::Threading::{CREATE_NEW_CONSOLE, CREATE_NO_WINDOW};

pub(super) fn run_cmds(cmds: InstallCommands, show: bool, tip: &str) -> ResultType<()> {
    let path = directory::path()?.with_extension("bat");
    let directory = path.with_extension("bat.dir");
    let mut script = write_install_script(cmds, &path)?;
    directory::create(&directory)?;
    log::debug!("Running protected installer directly with an elevated token");
    let exit_code = run_script(&mut script, show).map_err(|error| {
        error.context(format!(
            "Failed to run {tip} directly; protected directory may remain at {}",
            directory.display()
        ))
    })?;
    match fs::symlink_metadata(&directory) {
        Ok(_) => log::warn!(
            "Protected installer directory may remain at {}",
            directory.display()
        ),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => log::warn!(
            "Protected installer directory may remain at {}: {err}",
            directory.display()
        ),
    }
    if exit_code != 0 {
        bail!(
            "{tip} failed with elevated exit code {exit_code}: {}",
            elevated_install_failure_reason(exit_code)
        );
    }
    Ok(())
}

fn run_script(script: &mut InstallCommandScript, show: bool) -> ResultType<u32> {
    let system = get_system_executable("")?;
    let path = path_for_cmd_environment(script.file.path())?;
    let parameters = format!("/D /E:ON /V:OFF /C \"\"{path}\"\"");
    // An elevated caller can reopen protected files for cleanup if spawning fails.
    // Close the DELETE handles before cmd opens the BAT or copies its inputs.
    script.release_files();
    let status = Command::new(system.join(CMD_RELATIVE_PATH))
        .raw_arg(parameters)
        .current_dir(system)
        .creation_flags(
            if show {
                CREATE_NEW_CONSOLE
            } else {
                CREATE_NO_WINDOW
            }
            .0,
        )
        .status()?;
    let exit_code = status
        .code()
        .ok_or_else(|| anyhow!("Installer process exited without an exit code"))?;
    Ok(exit_code as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::windows::{
        installer_files::ProtectedInstallFile, installer_shell::shortcut_bytes,
    };

    #[test]
    fn direct_handoff_preserves_shortcuts_failures_and_cleanup() {
        const COMMAND_FAILURE: u32 = 77;
        for exit_code in [0, COMMAND_FAILURE] {
            let root =
                std::env::temp_dir().join(format!("rustdesk-direct-{}", uuid::Uuid::new_v4()));
            let source = root.join("source !&^@().bat");
            let runner = source.with_extension("bat.dir");
            fs::create_dir_all(&runner).unwrap();
            let bytes = shortcut_bytes(r"C:\RustDesk.exe", Some("--tray"), None).unwrap();
            let shortcut =
                ProtectedInstallFile::new(&root.join("shortcut !&^@().tmp"), &bytes).unwrap();
            let marker = root.join("copied.lnk");
            let commands = InstallCommands {
                script: format!(
                    "copy /B /Y \"{}\" \"{}\" > nul\r\nexit /b {exit_code}",
                    shortcut.path().display(),
                    marker.display()
                ),
                files: vec![shortcut],
            };
            let mut script = write_install_script(commands, &source).unwrap();
            let actual = run_script(&mut script, false).unwrap();
            let copied = fs::read(&marker).unwrap();
            let cleaned = !runner.exists()
                && !script.file.path().exists()
                && script._files.iter().all(|file| !file.path().exists());
            fs::remove_dir_all(&root).unwrap();
            assert_eq!(actual, exit_code);
            assert_eq!(copied, bytes);
            assert!(cleaned, "The runner and protected inputs must be removed");
        }
    }
}
