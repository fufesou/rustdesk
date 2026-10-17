mod directory;
mod gate;
mod helper;

use self::{gate::InstallGate, helper::ProtectedHelper};

use super::{
    installer_shell::{
        get_system_executable, launch_elevated, path_for_cmd_assignment, path_for_cmd_environment,
        trusted_install_environment, BATCH_SHORTCUT_DECODE_FAILURE_EXIT_CODE, CMD_RELATIVE_PATH,
        WIN7_SHELL_EXECUTE_MAX_PARAMETER_CHARS,
    },
    validate_install_app_name, ResultType,
};
use hbb_common::{
    anyhow::anyhow,
    bail, log,
    sha2::{Digest, Sha256},
};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

const CHCP_RELATIVE_PATH: &str = "chcp.com";
const UTF8_CODE_PAGE: u32 = 65001;
const INSTALL_HANDOFF_RUNNER_EXISTS_EXIT_CODE: u32 = 0x5253_0001;
const INSTALL_HANDOFF_COPY_FAILURE_EXIT_CODE: u32 = 0x5253_0002;
const INSTALL_HANDOFF_HASH_FAILURE_EXIT_CODE: u32 = 0x5253_0003;
const INSTALL_HANDOFF_HASH_MISMATCH_EXIT_CODE: u32 = 0x5253_0004;
const INSTALL_HANDOFF_GATE_FAILURE_EXIT_CODE: u32 = 0x5253_0009;
const BATCH_CODE_PAGE_FAILURE_EXIT_CODE: u32 = 0x5253_0005;
const BATCH_OUTPUT_DIRECTORY_EXISTS_EXIT_CODE: u32 = 0x5253_0006;
const BATCH_OUTPUT_DIRECTORY_CREATE_FAILURE_EXIT_CODE: u32 = 0x5253_0007;
const SHA256_HASH_LENGTH: usize = 32;

type BatchHash = [u8; SHA256_HASH_LENGTH];

struct InstallCommandScript {
    path: PathBuf,
    expected_hash: BatchHash,
}

impl Drop for InstallCommandScript {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.path) {
            if err.kind() != io::ErrorKind::NotFound {
                log::warn!(
                    "Failed to remove temporary installer file {:?}: {err}",
                    self.path
                );
            }
        }
    }
}

fn prepare_install_commands(commands: &str) -> ResultType<String> {
    let commands = commands.replace("\r\n", "\n").replace('\n', "\r\n");
    let chcp_path = get_system_executable(CHCP_RELATIVE_PATH)?;
    let chcp = path_for_cmd_environment(&chcp_path)?;
    Ok(format!(
        "@echo off\r\nsetlocal EnableExtensions DisableDelayedExpansion\r\n\
         \"{chcp}\" {UTF8_CODE_PAGE} > nul || exit /b \
         {BATCH_CODE_PAGE_FAILURE_EXIT_CODE}\r\n\
         {}\r\n\
         if exist \"%~f0.dir\" exit /b {BATCH_OUTPUT_DIRECTORY_EXISTS_EXIT_CODE}\r\n\
         md \"%~f0.dir\" || exit /b {BATCH_OUTPUT_DIRECTORY_CREATE_FAILURE_EXIT_CODE}\r\n\
         set \"RUSTDESK_OUTPUT_DIR=%~f0.dir\"\r\n\
         set \"RUSTDESK_INSTALL_HELPER=%~dp0installer-helper.exe\"\r\n{commands}\r\nexit /b 0\r\n",
        trusted_install_environment()?
    ))
}

fn write_install_script(cmds: String) -> ResultType<InstallCommandScript> {
    let directory = std::env::temp_dir();
    path_for_cmd_environment(&directory)?;
    let commands = prepare_install_commands(&cmds)?;
    let expected_hash = Sha256::digest(commands.as_bytes()).into();
    let path = directory.join(format!(
        "rustdesk_install_{}.bat",
        uuid::Uuid::new_v4().simple()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let script = InstallCommandScript {
        path,
        expected_hash,
    };
    file.write_all(commands.as_bytes())?;
    file.sync_all()?;
    Ok(script)
}

fn verified_install_bootstrap(
    script: &InstallCommandScript,
    runner_directory: &Path,
    gate: &InstallGate,
) -> ResultType<String> {
    let source = path_for_cmd_assignment(&script.path)?;
    let directory = path_for_cmd_assignment(runner_directory)?;
    let runner = runner_directory.join("install.bat");
    let runner = path_for_cmd_assignment(&runner)?;
    let source_helper = path_for_cmd_assignment(&runner_directory.with_extension("exe"))?;
    let verifier = path_for_cmd_assignment(&runner_directory.join("installer-helper.exe"))?;
    // Execute only from the protected runner directory, keeping DLL search away
    // from adjacent files in Windows Temp or the portable application's directory.
    // Short names preserve headroom under the Windows 7 ShellExecute limit.
    // START /WAIT sets ERRORLEVEL, but || can still report successful process creation.
    // Keep cleanup outside the gate so a failed handshake cannot skip it.
    Ok(format!(
        "setlocal DisableDelayedExpansion & set \"S={source}\" & set \"R={runner}\" & \
         set \"H={source_helper}\" & set \"V={verifier}\" & \
         set \"ERRORLEVEL=\" & set \"D={directory}\" & set \"E={INSTALL_HANDOFF_GATE_FAILURE_EXIT_CODE}\" & \
         setlocal EnableDelayedExpansion & set /p RUSTDESK_HANDOFF=<\"{gate}\" >nul && (set \"E=0\" & \
         if exist \"!R!\" (set \"E={INSTALL_HANDOFF_RUNNER_EXISTS_EXIT_CODE}\") else (\
         copy /B /Y \"!H!\" \"!V!\" > nul || \
         (set \"E={INSTALL_HANDOFF_COPY_FAILURE_EXIT_CODE}\") & \
         if \"!E!\"==\"0\" (copy /Y \"!S!\" \"!R!\" > nul || \
         set \"E={INSTALL_HANDOFF_COPY_FAILURE_EXIT_CODE}\") & \
         if \"!E!\"==\"0\" (start \"\" /b /wait \"!V!\" --verify-install-file \
         \"!R!\" {} \"!R!.verified\" & \
         if errorlevel 1 set \"E={INSTALL_HANDOFF_HASH_FAILURE_EXIT_CODE}\") & \
         if \"!E!\"==\"0\" (if not exist \"!R!.verified\" \
         set \"E={INSTALL_HANDOFF_HASH_MISMATCH_EXIT_CODE}\") & \
         if \"!E!\"==\"0\" (\"!R!\" & \
         set \"E=!errorlevel!\"))) & \
         del /f /q \"!H!\" > nul 2>&1 & \
         rd /s /q \"!D!\" > nul 2>&1 & exit /b !E!",
        hex::encode(script.expected_hash),
        gate = gate.name(),
    ))
}

fn verified_install_parameters(
    script: &InstallCommandScript,
    directory: &Path,
    gate: &InstallGate,
) -> ResultType<String> {
    let parameters = format!(
        "/D /E:ON /V:ON /C ({})",
        verified_install_bootstrap(script, directory, gate)?
    );
    let length = parameters.encode_utf16().count();
    if length >= WIN7_SHELL_EXECUTE_MAX_PARAMETER_CHARS {
        bail!("Elevated command is too long: {length} UTF-16 characters");
    }
    Ok(parameters)
}

pub(super) fn run_cmds(cmds: String, show: bool, tip: &str) -> ResultType<()> {
    validate_install_app_name(&crate::get_app_name())?;
    let script = write_install_script(cmds)?;
    let cmd_path = get_system_executable(CMD_RELATIVE_PATH)?;
    let directory = directory::path()?;
    let mut helper = ProtectedHelper::new(&directory.with_extension("exe"))?;
    let gate = InstallGate::new()?;
    let parameters = verified_install_parameters(&script, &directory, &gate)?;
    directory::create(&directory)?;
    let process = launch_elevated(&cmd_path, &parameters, show).map_err(|err| {
        anyhow!(
            "Installer handoff failed; protected directory may remain at {}: {err}",
            directory.display()
        )
    })?;
    gate.wait_for_client(process.handle())?;
    helper.release();
    gate.release(process.handle())?;
    let exit_code = gate.wait_for_exit(process.handle())?;
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

fn elevated_install_failure_reason(exit_code: u32) -> &'static str {
    match exit_code {
        INSTALL_HANDOFF_RUNNER_EXISTS_EXIT_CODE => "protected runner already exists",
        INSTALL_HANDOFF_COPY_FAILURE_EXIT_CODE => "failed to copy protected runner",
        INSTALL_HANDOFF_HASH_FAILURE_EXIT_CODE => "failed to hash protected runner",
        INSTALL_HANDOFF_HASH_MISMATCH_EXIT_CODE => "protected runner hash mismatch",
        INSTALL_HANDOFF_GATE_FAILURE_EXIT_CODE => "installer handoff was not authorized",
        BATCH_CODE_PAGE_FAILURE_EXIT_CODE => "failed to set the installer code page",
        BATCH_OUTPUT_DIRECTORY_EXISTS_EXIT_CODE => "installer output directory already exists",
        BATCH_OUTPUT_DIRECTORY_CREATE_FAILURE_EXIT_CODE => {
            "failed to create the installer output directory"
        }
        BATCH_SHORTCUT_DECODE_FAILURE_EXIT_CODE => "failed to decode an embedded shortcut",
        _ => "installer command failed",
    }
}

#[cfg(test)]
mod tests {
    use super::super::installer_shell::{embedded_shortcut_commands, shortcut_bytes};
    use super::*;
    use ::windows::Win32::{Foundation::HANDLE, System::Threading};
    use std::{
        os::windows::{io::AsRawHandle, process::CommandExt},
        process::{Command, Stdio},
    };

    #[test]
    fn native_install_handoff_verifies_before_execution() {
        let runner = std::env::temp_dir().join(format!(
            "rustdesk_install_!RUSTDESK_HANDOFF_EXPAND!&^@()_{}",
            uuid::Uuid::new_v4().simple()
        ));
        let marker = runner.with_extension("lnk");
        let bytes = shortcut_bytes(r"C:\RustDesk.exe", None, None).unwrap();
        let shortcut = embedded_shortcut_commands(bytes.clone(), "test.lnk", "test");
        assert!(!shortcut.contains("certutil"));
        let script = write_install_script(format!(
            "if \"%PROGRAMDATA%\"==\"rustdesk_untrusted\" exit /b 99\r\n\
             if \"%PUBLIC%\"==\"rustdesk_untrusted\" exit /b 99\r\n\
             {shortcut}\r\n\
             copy /B /Y \"%RUSTDESK_OUTPUT_DIR%\\test.lnk\" \"{}\" >nul || exit /b 99\r\n\
             exit /b %RUSTDESK_TEST_EXIT%",
            marker.display()
        ))
        .unwrap();
        for code in [0, 77] {
            fs::create_dir(&runner).unwrap();
            let result = run_install_bootstrap_for_test(&script, &runner, code);
            assert_eq!(result.status.code(), Some(code), "{result:?}");
            assert_eq!(fs::read(&marker).unwrap(), bytes);
            fs::remove_file(&marker).unwrap();
            assert!(
                !runner.exists(),
                "runner cleanup must preserve the BAT result"
            );
            assert!(
                !runner.with_extension("exe").exists(),
                "helper source remains"
            );
        }
        fs::create_dir(&runner).unwrap();
        fs::write(
            &script.path,
            format!("> \"{}\" echo hijacked\r\n", marker.display()),
        )
        .unwrap();
        let result = run_install_bootstrap_for_test(&script, &runner, 0);
        assert_eq!(
            result.status.code(),
            Some(INSTALL_HANDOFF_HASH_FAILURE_EXIT_CODE as i32)
        );
        assert!(!marker.exists(), "a replaced BAT must not execute");
        assert!(
            !runner.exists(),
            "failed verification must clean the runner"
        );
        assert!(!runner.with_extension("exe").exists());
    }

    fn run_install_bootstrap_for_test(
        script: &InstallCommandScript,
        directory: &Path,
        exit_code: i32,
    ) -> std::process::Output {
        let mut helper = ProtectedHelper::new(&directory.with_extension("exe")).unwrap();
        let gate = InstallGate::new().unwrap();
        let parameters = verified_install_parameters(script, directory, &gate).unwrap();
        assert!(!parameters.contains("certutil.exe") && !parameters.contains("findstr.exe"));
        let child = Command::new(get_system_executable(CMD_RELATIVE_PATH).unwrap())
            .env("PROGRAMDATA", "rustdesk_untrusted")
            .env("PUBLIC", "rustdesk_untrusted")
            .env("RUSTDESK_HANDOFF_EXPAND", "expanded")
            .env("RUSTDESK_TEST_EXIT", exit_code.to_string())
            .raw_arg(parameters)
            .creation_flags(Threading::CREATE_NO_WINDOW.0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let process = HANDLE(child.as_raw_handle());
        gate.wait_for_client(process).unwrap();
        helper.release();
        gate.release(process).unwrap();
        gate.wait_for_exit(process).unwrap();
        child.wait_with_output().unwrap()
    }
}
