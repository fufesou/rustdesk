use super::{
    installer_files::ProtectedInstallFile,
    installer_handoff::{directory, run_cmds, InstallCommands},
    installer_shell::{path_for_cmd_environment, trusted_install_environment},
    ResultType,
};
use crate::install_manifest::{
    package,
    request::{Operation, Request, Script, REQUEST_VERSION},
    windows::{identity, inspect, system},
};
use hbb_common::{anyhow::anyhow, bail};
use std::{io::Read, path::PathBuf};

#[path = "manifest_install/payload.rs"]
mod payload;

pub(super) struct Prepared {
    pub request: Request,
    files: Vec<ProtectedInstallFile>,
}

impl Prepared {
    pub(super) fn new(operation: Operation, root: PathBuf, silent: bool) -> ResultType<Self> {
        let product = crate::get_app_name();
        super::validate_install_app_name(&product)?;
        super::validate_install_value(
            root.to_str()
                .ok_or_else(|| anyhow!("Invalid installation directory"))?,
        )?;
        let snapshot = inspect::read(&root, &product)?;
        let request = describe(operation, (root, product), &snapshot)?;
        system::verify_registration(&request)?;
        let inputs = if operation == Operation::Uninstall {
            Vec::new()
        } else {
            payload::prepare(&snapshot, &request, silent)?
        };
        let request = Request {
            files: inputs,
            ..request
        };
        request.to_bytes()?;
        inspect::verify(&request, &snapshot)?;
        let (inputs, files) = payload::stage(request.files)?;
        let request = Request {
            files: inputs,
            ..request
        };
        Ok(Self { request, files })
    }

    pub(super) fn run(mut self, scripts: (String, InstallCommands), debug: bool) -> ResultType<()> {
        self.request.before = self.stage_script(&scripts.0)?;
        self.request.after = self.stage_script(&scripts.1.script)?;
        self.files.extend(scripts.1.files);
        let worker = self.stage_worker()?;
        let request = ProtectedInstallFile::new(
            &directory::path()?.with_extension("json"),
            &self.request.to_bytes()?,
        )?;
        let command = format!(
            "\"{}\" \"{}\"\r\nexit /b %errorlevel%",
            path_for_cmd_environment(worker.path())?,
            path_for_cmd_environment(request.path())?
        );
        self.files.extend([worker, request]);
        run_cmds(
            InstallCommands {
                script: command,
                files: self.files,
            },
            debug,
            "installation file operation",
        )
    }

    fn stage_script(&mut self, commands: &str) -> ResultType<Option<Script>> {
        if commands.trim().is_empty() {
            return Ok(None);
        }
        let contents = format!("@echo off\r\nsetlocal EnableExtensions DisableDelayedExpansion\r\n{}\r\n{}\r\nexit /b 0\r\n", trusted_install_environment()?, checked_commands(commands));
        let bytes = contents.as_bytes();
        let file = ProtectedInstallFile::new(&directory::path()?.with_extension("bat"), bytes)?;
        let script = Script {
            path: file.path().to_owned(),
            sha256: crate::update_hash::sha256_reader_hex(&mut &bytes[..])?,
        };
        self.files.push(file);
        Ok(Some(script))
    }

    fn stage_worker(&self) -> ResultType<ProtectedInstallFile> {
        let (source, digest) = payload::worker_source(&self.request)?;
        let mut source = std::fs::File::open(source)?;
        crate::update_hash::verify_sha256_reader(&mut source, &digest)
            .map_err(|error| anyhow!("Invalid file worker: {error:?}"))?;
        let mut bytes = Vec::new();
        source.read_to_end(&mut bytes)?;
        if crate::update_hash::sha256_reader_hex(&mut bytes.as_slice())? != digest {
            bail!("File worker changed while staging");
        }
        ProtectedInstallFile::new(&directory::path()?.with_extension("exe"), &bytes)
    }
}

fn describe(
    operation: Operation,
    location: (PathBuf, String),
    snapshot: &inspect::Snapshot,
) -> ResultType<Request> {
    Ok(Request {
        schema_version: REQUEST_VERSION,
        operation,
        product: location.1,
        root: location.0,
        architecture: std::env::consts::ARCH.to_owned(),
        root_identity: snapshot
            .root
            .as_ref()
            .map(|root| identity(&root.file))
            .transpose()?,
        registry_subkey: super::get_install_info().0,
        expected_manifest_sha256: snapshot.digest.clone(),
        package_version: crate::VERSION.to_owned(),
        preserve_unlisted: operation == Operation::Update
            && crate::is_custom_client()
            && !package::current()?.is_some_and(|package| package.has_custom_configuration()),
        files: Vec::new(),
        before: None,
        after: None,
        show_result: false,
    })
}

pub fn run_component(command: &str) -> Option<ResultType<()>> {
    if std::env::var_os("RUSTDESK_MANIFEST_OPERATION").as_deref() != Some(std::ffi::OsStr::new("1"))
    {
        return None;
    }
    match command {
        "--uninstall-cert" => Some(super::uninstall_cert()),
        "--uninstall-amyuni-idd" => {
            Some(crate::virtual_display_manager::amyuni_idd::uninstall_driver())
        }
        "--install-remote-printer" if super::is_win_10_or_greater() => Some(
            remote_printer::install_update_printer(&crate::get_app_name()),
        ),
        "--uninstall-remote-printer" if super::is_win_10_or_greater() => Some(remove_printer()),
        _ => None,
    }
}

fn remove_printer() -> ResultType<()> {
    let product = crate::get_app_name();
    remote_printer::uninstall_printer(&product);
    if remote_printer::is_rd_printer_installed(&product)? {
        bail!("Remote printer removal did not complete");
    }
    Ok(())
}

pub(super) fn install_path(requested: &str) -> ResultType<String> {
    if !requested.is_empty() {
        return Ok(requested.trim_end_matches('\\').to_owned());
    }
    let subkey = super::get_install_info().0;
    Ok(super::get_reg_string_of(&subkey, "InstallLocation")?
        .unwrap_or_else(|| super::get_default_install_info().1))
}

pub(super) fn uninstall(mut prepared: Prepared, kill_self: bool) -> ResultType<()> {
    use crate::install_manifest::{windows::directory::Directory, InstallState};
    let current = std::env::current_exe()?;
    let parent = current
        .parent()
        .ok_or_else(|| anyhow!("Missing executable directory"))?;
    let current_root = Directory::open(parent)?;
    let kill_current =
        kill_self && Some(identity(&current_root.file)?) == prepared.request.root_identity;
    drop(current_root);
    let session = super::get_session_id_of_process(std::process::id())
        .ok_or_else(|| anyhow!("Cannot identify the uninstaller's Windows session"))?;
    prepared.request.show_result = kill_current && session != 0;
    let snapshot = inspect::read(&prepared.request.root, &prepared.request.product)?;
    let record = snapshot
        .record
        .ok_or_else(|| anyhow!("Uninstallation requires a valid manifest"))?;
    let before = if record.state == InstallState::Uninstalling {
        String::new()
    } else {
        let executable = prepared
            .request
            .root
            .join(format!("{}.exe", prepared.request.product));
        let exe = path_for_cmd_environment(&executable)?;
        format!("{}\r\n\"{exe}\" --uninstall-remote-printer\r\n\"{exe}\" --uninstall-cert\r\n\"{exe}\" --uninstall-amyuni-idd", super::get_before_uninstall(kill_current)?)
    };
    drop(snapshot.root);
    prepared.run((before, String::new().into()), false)
}

fn checked_commands(commands: &str) -> String {
    commands
        .lines()
        .map(|line| {
            let text = line.trim();
            let required = text.starts_with("reg add ")
                || text.starts_with("copy ")
                || text.starts_with("netsh ") && !text.contains(" delete ")
                || text.starts_with('"') && text.contains(" --");
            if required && !text.contains("||") {
                format!("{line} || exit /b 1")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_config_keeps_its_service_start_failure_contract() {
        let commands = super::super::get_import_config(r"C:\Apps\RustDesk.exe");
        let checked = checked_commands(&commands);
        let start = format!("sc start {}", crate::get_app_name());
        assert!(checked.lines().any(|line| line == start));
        assert!(!checked.contains(&format!("{start} || exit")));
        let required = format!("{start} || exit /b 1");
        assert_eq!(checked_commands(&required), required);
    }
}
