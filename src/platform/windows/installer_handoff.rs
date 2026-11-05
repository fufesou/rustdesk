pub(super) mod directory;

use super::{
    installer_files::ProtectedInstallFile,
    installer_shell::{
        get_system_executable, launch_elevated, path_for_cmd_environment,
        trusted_install_environment, BATCH_SHORTCUT_COPY_FAILURE_EXIT_CODE, CMD_RELATIVE_PATH,
        WIN7_SHELL_EXECUTE_MAX_PARAMETER_CHARS,
    },
    validate_install_app_name, ResultType,
};
use hbb_common::{anyhow::anyhow, bail, log};
use std::{fs, io, path::Path};

const CHCP_RELATIVE_PATH: &str = "chcp.com";
const UTF8_CODE_PAGE: u32 = 65001;
const BATCH_CODE_PAGE_FAILURE_EXIT_CODE: u32 = 0x5253_0005;

pub(super) struct InstallCommands {
    pub script: String,
    pub files: Vec<ProtectedInstallFile>,
}

impl From<String> for InstallCommands {
    fn from(script: String) -> Self {
        Self {
            script,
            files: Vec::new(),
        }
    }
}

struct InstallCommandScript {
    file: ProtectedInstallFile,
    _files: Vec<ProtectedInstallFile>,
}

impl InstallCommandScript {
    fn release_files(&mut self) {
        self.file.release();
        for file in &mut self._files {
            file.release();
        }
        log::warn!("Diagnostic installer handoff: no named pipe; no delay before elevated cmd");
    }
}

fn prepare_install_commands(cmds: &InstallCommands) -> ResultType<String> {
    let commands = cmds.script.replace("\r\n", "\n").replace('\n', "\r\n");
    let chcp_path = get_system_executable(CHCP_RELATIVE_PATH)?;
    let chcp = path_for_cmd_environment(&chcp_path)?;
    let files = cmds
        .files
        .iter()
        .map(|file| path_for_cmd_environment(file.path()).map(|path| format!("\"{path}\"")))
        .collect::<ResultType<Vec<_>>>()?
        .join(" ");
    // Call a fixed label so early EXIT /B still reaches cleanup, without reparsing a path.
    // Exit the dedicated cmd after self-deletion; EXIT /B would reopen the missing BAT.
    Ok(format!(
        "@echo off\r\nsetlocal EnableExtensions DisableDelayedExpansion\r\n\
         set \"ERRORLEVEL=\"\r\n\
         call :rustdesk_install\r\n\
         set \"RUSTDESK_INSTALL_EXIT_CODE=%ERRORLEVEL%\"\r\n\
         rd /s /q \"%~f0.dir\" > nul 2>&1\r\n\
         (del /q \"%~f0\" {files} > nul 2>&1 & exit %RUSTDESK_INSTALL_EXIT_CODE%)\r\n\
         :rustdesk_install\r\n\
         \"{chcp}\" {UTF8_CODE_PAGE} > nul || exit /b \
         {BATCH_CODE_PAGE_FAILURE_EXIT_CODE}\r\n\
         {}\r\n\
         set \"RUSTDESK_OUTPUT_DIR=%~f0.dir\"\r\n{commands}\r\nexit /b 0\r\n",
        trusted_install_environment()?
    ))
}

fn write_install_script(cmds: InstallCommands, path: &Path) -> ResultType<InstallCommandScript> {
    let commands = prepare_install_commands(&cmds)?;
    Ok(InstallCommandScript {
        file: ProtectedInstallFile::new(path, commands.as_bytes())?,
        _files: cmds.files,
    })
}

fn protected_install_parameters(script: &InstallCommandScript) -> ResultType<String> {
    let path = path_for_cmd_environment(script.file.path())?;
    let parameters = format!("/D /E:ON /V:OFF /C \"\"{path}\"\"");
    let length = parameters.encode_utf16().count();
    if length >= WIN7_SHELL_EXECUTE_MAX_PARAMETER_CHARS {
        bail!("Elevated command is too long: {length} UTF-16 characters");
    }
    Ok(parameters)
}

pub(super) fn run_cmds(cmds: impl Into<InstallCommands>, show: bool, tip: &str) -> ResultType<()> {
    validate_install_app_name(&crate::get_app_name())?;
    let path = directory::path()?.with_extension("bat");
    let directory = path.with_extension("bat.dir");
    let mut script = write_install_script(cmds.into(), &path)?;
    let cmd_path = get_system_executable(CMD_RELATIVE_PATH)?;
    let parameters = protected_install_parameters(&script)?;
    directory::create(&directory)?;
    // Diagnostic only: releasing before UAC can leave protected files if elevation is cancelled.
    script.release_files();
    let exit_code = run_install_command(&cmd_path, &parameters, show).map_err(|err| {
        log::debug!("Failed to launch elevated installer: {err:#}");
        err.context(format!(
            "Installer handoff failed; protected directory may remain at {}",
            directory.display()
        ))
    })?;
    // Cleanup must not change the result of an installation that already ran.
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

fn run_install_command(executable: &Path, parameters: &str, show: bool) -> ResultType<u32> {
    if super::is_elevated(None)? {
        log::info!("Diagnostic installer: elevated caller creates system CMD directly");
        return run_install_command_direct(executable, parameters, show);
    }
    launch_elevated(executable, parameters, show)?.wait()
}

fn run_install_command_direct(executable: &Path, parameters: &str, show: bool) -> ResultType<u32> {
    use std::{os::windows::process::CommandExt, process::Command};
    use windows::Win32::System::Threading::{CREATE_NEW_CONSOLE, CREATE_NO_WINDOW};

    let directory = executable
        .parent()
        .ok_or_else(|| anyhow!("System executable has no parent directory"))?;
    let flags = if show {
        CREATE_NEW_CONSOLE
    } else {
        CREATE_NO_WINDOW
    };
    let status = Command::new(executable)
        .raw_arg(parameters)
        .current_dir(directory)
        .creation_flags(flags.0)
        .status()?;
    status
        .code()
        .map(|code| code as u32)
        .ok_or_else(|| anyhow!("Installer process exited without an exit code"))
}

fn elevated_install_failure_reason(exit_code: u32) -> &'static str {
    match exit_code {
        BATCH_CODE_PAGE_FAILURE_EXIT_CODE => "failed to set the installer code page",
        BATCH_SHORTCUT_COPY_FAILURE_EXIT_CODE => "failed to copy a prepared shortcut",
        _ => "installer command failed",
    }
}

#[cfg(test)]
mod tests {
    use super::super::installer_shell::shortcut_bytes;
    use super::*;
    use std::{os::windows::process::CommandExt, process::Stdio};

    #[test]
    fn protected_handoff_preserves_shortcuts_failures_and_cleanup() {
        const COMMAND_FAILURE: u32 = 77;
        for exit_code in [0, COMMAND_FAILURE] {
            let root =
                std::env::temp_dir().join(format!("rustdesk-handoff-{}", uuid::Uuid::new_v4()));
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
            if !super::super::is_elevated(None).unwrap() {
                assert!(fs::write(script.file.path(), "exit /b 0\r\n").is_err());
            }
            let output = run_script(&mut script);
            let copied = fs::read(&marker).unwrap();
            let cleaned = !runner.exists()
                && !script.file.path().exists()
                && script._files.iter().all(|file| !file.path().exists());
            fs::remove_dir_all(&root).unwrap();
            assert_eq!(
                output.status.code().map(|code| code as u32),
                Some(exit_code),
                "{output:?}"
            );
            assert_eq!(copied, bytes);
            assert!(cleaned, "The runner and protected inputs must be removed");
        }
    }

    #[test]
    fn direct_command_preserves_staged_inputs_and_exit_codes() {
        const COMMAND_FAILURE: u32 = 77;
        for exit_code in [0, COMMAND_FAILURE] {
            let root =
                std::env::temp_dir().join(format!("rustdesk-direct-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&root).unwrap();
            let source = root.join("source !&^@().bat");
            fs::create_dir(source.with_extension("bat.dir")).unwrap();
            let bytes = shortcut_bytes(r"C:\RustDesk.exe", Some("--tray"), None).unwrap();
            let shortcut =
                ProtectedInstallFile::new(&root.join("input !&^@().lnk"), &bytes).unwrap();
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
            let parameters = protected_install_parameters(&script).unwrap();
            script.release_files();
            let result = run_install_command_direct(
                &get_system_executable(CMD_RELATIVE_PATH).unwrap(),
                &parameters,
                false,
            )
            .unwrap();
            assert_eq!(result, exit_code);
            assert_eq!(fs::read(&marker).unwrap(), bytes);
            assert!(!source.exists() && !source.with_extension("bat.dir").exists());
            assert!(script._files.iter().all(|file| !file.path().exists()));
            fs::remove_dir_all(root).unwrap();
        }
    }

    fn run_script(script: &mut InstallCommandScript) -> std::process::Output {
        let parameters = protected_install_parameters(script).unwrap();
        script.release_files();
        let child = std::process::Command::new(get_system_executable(CMD_RELATIVE_PATH).unwrap())
            .raw_arg(parameters)
            .creation_flags(::windows::Win32::System::Threading::CREATE_NO_WINDOW.0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.wait_with_output().unwrap()
    }
}
