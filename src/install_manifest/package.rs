use serde::{Deserialize, Serialize};
use std::{io, process::Command, sync::OnceLock};

#[path = "package/transport.rs"]
mod transport;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PayloadFile {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Package {
    pub version: String,
    pub executable: String,
    pub files: Vec<PayloadFile>,
}

impl Package {
    pub(crate) fn attach(&self, command: &mut Command) -> io::Result<transport::Mapping> {
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        let mapping = transport::Mapping::new(&bytes)?;
        mapping.attach(command, bytes.len());
        Ok(mapping)
    }

    pub(crate) fn has_custom_configuration(&self) -> bool {
        self.files
            .iter()
            .any(|file| file.path.eq_ignore_ascii_case("custom.txt"))
    }
}

pub(crate) fn current() -> io::Result<Option<&'static Package>> {
    static PACKAGE: OnceLock<Result<Option<Package>, String>> = OnceLock::new();
    PACKAGE
        .get_or_init(|| {
            transport::receive()
                .and_then(|bytes| {
                    bytes
                        .map(|bytes| serde_json::from_slice(&bytes).map_err(io::Error::other))
                        .transpose()
                })
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map(Option::as_ref)
        .map_err(|error| io::Error::other(error.clone()))
}

pub(crate) fn relaunch(argument: &str) -> io::Result<std::process::Child> {
    let package = current()?;
    if argument == "--install" && package.is_none() {
        return Err(io::Error::other("Install from a complete EXE package"));
    }
    let mut command = Command::new(std::env::current_exe()?);
    command.arg(argument).env("SET_FOREGROUND_WINDOW", "1");
    let _mapping = package
        .map(|package| package.attach(&mut command))
        .transpose()?;
    let child = command.spawn()?;
    if unsafe { winapi::um::winuser::AllowSetForegroundWindow(child.id()) } == 0 {
        eprintln!(
            "Cannot grant application window foreground access: {}",
            io::Error::last_os_error()
        );
    }
    Ok(child)
}
