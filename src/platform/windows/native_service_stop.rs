use super::{get_install_info, installer_shell::launch_elevated, is_elevated};
use hbb_common::{anyhow::anyhow, bail, log, ResultType};
use std::{io, path::Path};

pub(super) fn run() -> ResultType<()> {
    let application = crate::get_app_name();
    crate::platform::validate_install_app_name(&application)?;
    let (_, _, _, installed_executable) = get_install_info();
    let current = std::env::current_exe()?;
    let current = current
        .to_str()
        .ok_or_else(|| anyhow!("Current executable path is not valid Unicode"))?;
    if !current.eq_ignore_ascii_case(&installed_executable) {
        bail!("Run the installed diagnostic before testing native Stop service");
    }
    let broker = super::WIN_TOPMOST_INJECTED_PROCESS_EXE;
    let parameters = format!(
        "--native-service-stop \"{application}\" \"{broker}\" {}",
        std::process::id()
    );
    log::info!("Diagnostic native service stop: existing runner; no CMD, BAT or installer pipe");
    let code = launch(Path::new(&installed_executable), &parameters)?;
    log::info!("Native service-stop runner exited with code {code}");
    if code != 0 {
        bail!(
            "Native service-stop runner failed with code {code}: {}",
            io::Error::from_raw_os_error(code as i32)
        );
    }
    Ok(())
}

fn launch(executable: &Path, parameters: &str) -> ResultType<u32> {
    use std::{os::windows::process::CommandExt, process::Command};
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;

    if !is_elevated(None)? {
        return launch_elevated(executable, parameters, false)?.wait();
    }
    let directory = executable
        .parent()
        .ok_or_else(|| anyhow!("Native service-stop executable has no parent directory"))?;
    log::info!("Native service stop: direct launch with an elevated token");
    let status = Command::new(executable)
        .raw_arg(parameters)
        .current_dir(directory)
        .creation_flags(CREATE_NO_WINDOW.0)
        .status()?;
    let code = status
        .code()
        .ok_or_else(|| anyhow!("Native service-stop process exited without an exit code"))?;
    Ok(code as u32)
}
