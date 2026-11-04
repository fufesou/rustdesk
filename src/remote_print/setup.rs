use hbb_common::{bail, ResultType};
use std::mem::size_of;
use windows::{
    core::{w, HSTRING, PCWSTR},
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE},
        UI::{
            Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW},
            WindowsAndMessaging::SW_HIDE,
        },
    },
};

pub(crate) const INSTALL: &str = "--install-remote-printer";
pub(crate) const UNINSTALL: &str = "--uninstall-remote-printer";
const INSTALL_MARKER: &str = "PDFPRINTER";

pub(crate) fn execute(command: &str) -> ResultType<()> {
    #[cfg(not(feature = "flutter"))]
    if command == INSTALL {
        bail!("PDF capture requires a Flutter client build");
    }
    if !crate::platform::is_win_10_or_greater() {
        bail!("PDF capture requires Windows 10 or 11");
    }
    if !remote_printer::pdf::is_elevated()? {
        bail!("PDF printer setup requires administrator privileges");
    }
    match command {
        INSTALL => {
            let instance = uuid::Uuid::new_v4().simple().to_string();
            remote_printer::pdf::verify_namespace(&instance)?;
            let present = remote_printer::is_rd_printer_installed(&crate::get_app_name())?;
            remote_printer::pdf::install(&crate::get_app_name(), &instance)?;
            if let Err(error) = write_marker(true) {
                if !present {
                    remote_printer::pdf::uninstall(&crate::get_app_name())?;
                }
                return Err(error);
            }
        }
        UNINSTALL => {
            remote_printer::pdf::uninstall(&crate::get_app_name())?;
            write_marker(false)?;
        }
        _ => bail!("Unknown PDF printer setup operation"),
    }
    Ok(())
}

pub(crate) fn uninstall_if_owned() -> ResultType<()> {
    if installed_marker()? {
        request(UNINSTALL)?;
    }
    Ok(())
}

pub(crate) fn installed_marker() -> ResultType<bool> {
    use winreg::{enums::HKEY_LOCAL_MACHINE, RegKey};
    read_marker(
        &RegKey::predef(HKEY_LOCAL_MACHINE),
        &marker_key(&crate::get_app_name()),
    )
}

fn marker_key(app_name: &str) -> String {
    format!(r"Software\{app_name}\RemotePrinting")
}

fn read_marker(root: &winreg::RegKey, name: &str) -> ResultType<bool> {
    let value = root
        .open_subkey(name)
        .and_then(|key| key.get_value::<String, _>(INSTALL_MARKER));
    match value {
        Ok(value) => Ok(value == "1"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn write_marker(installed: bool) -> ResultType<()> {
    use winreg::{enums::HKEY_LOCAL_MACHINE, RegKey};
    set_marker(
        &RegKey::predef(HKEY_LOCAL_MACHINE),
        &marker_key(&crate::get_app_name()),
        installed,
    )
}

fn set_marker(root: &winreg::RegKey, name: &str, installed: bool) -> ResultType<()> {
    use winreg::enums::KEY_SET_VALUE;
    if installed {
        let (key, _) = root.create_subkey(name)?;
        key.set_value(INSTALL_MARKER, &"1")?;
        return Ok(());
    }
    let result = root
        .open_subkey_with_flags(name, KEY_SET_VALUE)
        .and_then(|key| key.delete_value(INSTALL_MARKER));
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn request(command: &str) -> ResultType<()> {
    if command != INSTALL && command != UNINSTALL {
        bail!("Unknown PDF printer setup operation");
    }
    if !crate::platform::is_installed() {
        bail!("Install the application before configuring PDF capture");
    }
    if remote_printer::pdf::is_elevated()? {
        return execute(command);
    }
    let exe = HSTRING::from(std::env::current_exe()?.as_os_str());
    let parameters = HSTRING::from(command);
    let mut execution = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut execution)? };
    if execution.hProcess.is_invalid() {
        bail!("Windows returned no PDF setup process");
    }
    let process = Process(execution.hProcess);
    if unsafe { WaitForSingleObject(process.0, INFINITE) } != WAIT_OBJECT_0 {
        return Err(windows::core::Error::from_win32().into());
    }
    let mut exit_code = 0;
    unsafe { GetExitCodeProcess(process.0, &mut exit_code)? };
    if exit_code != 0 {
        bail!("PDF printer setup failed (exit code {exit_code}); see the application log for the Windows error");
    }
    Ok(())
}

struct Process(HANDLE);

#[cfg(feature = "flutter")]
pub(crate) fn request_from_ui() {
    std::thread::spawn(move || {
        let result = request(INSTALL).and_then(|()| {
            crate::ipc::set_install_option(
                crate::platform::REG_NAME_INSTALL_PRINTER.to_owned(),
                "1".to_owned(),
            )
        });
        let error = result.as_ref().err().map(|error| format!("{error:#}"));
        if let Some(error) = &error {
            hbb_common::log::error!("PDF printer setup: {error}");
        }
        let event = serde_json::json!({
            "name": "install-printer-res",
            "success": result.is_ok(),
            "msg": error.unwrap_or_default(),
        });
        if crate::flutter::push_global_event(crate::flutter::APP_TYPE_MAIN, event.to_string())
            != Some(true)
        {
            hbb_common::log::trace!("PDF setup result has no active settings listener");
        }
    });
}

impl Drop for Process {
    fn drop(&mut self) {
        if let Err(error) = unsafe { CloseHandle(self.0) } {
            hbb_common::log::error!("Failed to close the PDF setup process handle: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};

    #[test]
    fn pdf_marker_survives_file_association_cleanup() {
        let fixture = format!(r"Software\RustDesk-PdfMarkerTest-{}", uuid::Uuid::new_v4());
        let current_user = RegKey::predef(HKEY_CURRENT_USER);
        let (root, _) = current_user.create_subkey(&fixture).unwrap();
        let app_name = "Branded Client";
        let marker = marker_key(app_name);
        set_marker(&root, &marker, true).unwrap();
        let association = format!(r"Software\Classes\.{}", app_name.to_lowercase());
        root.create_subkey(&association).unwrap();
        root.delete_subkey_all(&association).unwrap();
        let retained = read_marker(&root, &marker).unwrap();
        set_marker(&root, &marker, false).unwrap();
        let removed = !read_marker(&root, &marker).unwrap();
        drop(root);
        current_user.delete_subkey_all(&fixture).unwrap();
        assert!(
            retained,
            "EXE upgrade cleanup removed the PDF installation marker"
        );
        assert!(
            removed,
            "PDF uninstall must still remove its installation marker"
        );
    }
}
