#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
#[path = "../../install_manifest.rs"]
mod install_manifest;
#[cfg(windows)]
#[path = "../../update_hash.rs"]
mod update_hash;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match read_request() {
        Ok(request) => finish(
            &request,
            install_manifest::windows::operation::run(&request),
        ),
        Err(error) => {
            eprintln!("Cannot read installer request: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("This installer file worker is supported only on Windows");
    std::process::ExitCode::FAILURE
}

#[cfg(windows)]
fn read_request() -> std::io::Result<install_manifest::request::Request> {
    use install_manifest::windows::{files::open_absolute, Access};
    use std::{io::Read, path::PathBuf};
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .ok_or_else(|| std::io::Error::other("Missing installer request"))?;
    if args.next().is_some() {
        return Err(std::io::Error::other("Unexpected installer arguments"));
    }
    let mut file = open_absolute(&PathBuf::from(path), Access::Read)?;
    let mut bytes = Vec::new();
    file.file.read_to_end(&mut bytes)?;
    install_manifest::request::Request::from_bytes(&bytes)
}

#[cfg(windows)]
fn finish(
    request: &install_manifest::request::Request,
    result: std::io::Result<()>,
) -> std::process::ExitCode {
    let (message, code) = match result {
        Ok(()) => (
            "Operation completed successfully.".to_owned(),
            std::process::ExitCode::SUCCESS,
        ),
        Err(error) => {
            let command = match request.operation {
                install_manifest::request::Operation::Install => "--install",
                install_manifest::request::Operation::Update => "--update",
                install_manifest::request::Operation::Uninstall => "--uninstall",
            };
            let message = format!("Operation incomplete: {error}\n\nInstallation: {}\nIf the manifest remains, retry {command} from a complete EXE package for the same product supporting this manifest format. A final cleanup error after application files were removed requires manual cleanup of the reported residual item.", request.root.display());
            eprintln!("{message}");
            (message, std::process::ExitCode::FAILURE)
        }
    };
    if request.show_result {
        use windows::{
            core::{w, PCWSTR},
            Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_OK},
        };
        let text: Vec<_> = message.encode_utf16().chain([0]).collect();
        unsafe {
            MessageBoxW(
                None,
                PCWSTR(text.as_ptr()),
                w!("RustDesk installation"),
                MB_OK,
            )
        };
    }
    code
}
