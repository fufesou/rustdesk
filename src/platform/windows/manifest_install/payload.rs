use super::super::{installer_files::ProtectedInstallFile, ResultType};
use crate::install_manifest::{
    package,
    request::{InputFile, Operation, Request, FILE_WORKER_NAME},
    windows::{files, inspect::Snapshot},
    PreviousTarget, RelativePath,
};
use hbb_common::{anyhow::anyhow, bail};
use std::path::PathBuf;

pub(super) fn prepare(snapshot: &Snapshot, request: &Request) -> ResultType<Vec<InputFile>> {
    let package = package::current()?
        .ok_or_else(|| anyhow!("Use a complete EXE package to install or update"))?;
    let source = std::env::current_exe()?;
    let source = source
        .parent()
        .ok_or_else(|| anyhow!("Missing package directory"))?;
    let executable = relative(&package.executable)?;
    if executable.as_str().contains('/') {
        bail!("Package executable must be at its root");
    }
    let mut inputs = Vec::new();
    for file in &package.files {
        let path = relative(&file.path)?;
        let destination = if path.as_str().eq_ignore_ascii_case(executable.as_str()) {
            relative(&format!("{}.exe", request.product))?
        } else {
            path.clone()
        };
        destination
            .validate_payload_file()
            .map_err(|error| anyhow!(error))?;
        inputs.push(InputFile {
            previous: if request.operation == Operation::Uninstall {
                PreviousTarget::Absent
            } else {
                previous(snapshot, &destination)?
            },
            path: destination,
            source: source.join(path.as_str()),
            sha256: file.sha256.clone(),
        });
    }
    Ok(inputs)
}

fn previous(snapshot: &Snapshot, path: &RelativePath) -> ResultType<PreviousTarget> {
    if snapshot
        .record
        .as_ref()
        .is_some_and(|record| files::contains(&record.files, path))
    {
        return Ok(PreviousTarget::Owned);
    }
    let Some(root) = &snapshot.root else {
        return Ok(PreviousTarget::Absent);
    };
    let Some(identity) = files::current(root, path)? else {
        return Ok(PreviousTarget::Absent);
    };
    if snapshot.record.is_some() {
        bail!("Package would replace an unowned file: {}", path.as_str());
    }
    Ok(PreviousTarget::Legacy { identity })
}

pub(super) fn confirm_migration(request: &Request, silent: bool) -> ResultType<()> {
    use windows::{
        core::{w, PCWSTR},
        Win32::UI::WindowsAndMessaging::{
            MessageBoxW, IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_YESNO,
        },
    };
    let collisions = request
        .files
        .iter()
        .filter(|file| matches!(file.previous, PreviousTarget::Legacy { .. }))
        .count();
    if collisions == 0
        || request.expected_manifest_sha256.is_some()
        || std::env::args().any(|arg| arg == "--migrate-legacy-installation")
    {
        return Ok(());
    }
    if silent {
        bail!("Legacy migration requires --migrate-legacy-installation to adopt {collisions} existing package paths for replacement and uninstallation, including files not overwritten if installation fails");
    }
    let message = format!("Migrate the existing {} installation to version {}?\n\nFolder: {}\n\nThis installer will manage {collisions} existing files at its package paths before copying starts. If installation fails, uninstall can remove these files even if they were not overwritten. Files outside the package list will be preserved.\n\nAllow this one-time migration?", request.product, request.package_version, request.root.display());
    let wide: Vec<_> = message.encode_utf16().chain([0]).collect();
    if unsafe {
        MessageBoxW(
            None,
            PCWSTR(wide.as_ptr()),
            w!("RustDesk installation migration"),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        )
    } != IDYES
    {
        bail!("Legacy migration was not confirmed");
    }
    Ok(())
}

pub(super) fn stage(
    inputs: Vec<InputFile>,
) -> ResultType<(Vec<InputFile>, Vec<ProtectedInstallFile>)> {
    let mut staged = Vec::new();
    let mut files = Vec::new();
    for input in inputs {
        let file = ProtectedInstallFile::copy_verified(&input.source, &input.sha256)?;
        staged.push(InputFile {
            source: file.path().to_owned(),
            ..input
        });
        files.push(file);
    }
    Ok((staged, files))
}

pub(super) fn worker_source(request: &Request) -> ResultType<(PathBuf, String)> {
    if let Some(file) = request
        .files
        .iter()
        .find(|file| file.path.as_str().eq_ignore_ascii_case(FILE_WORKER_NAME))
    {
        return Ok((file.source.clone(), file.sha256.clone()));
    }
    if let Some(package) = package::current()? {
        let file = package
            .files
            .iter()
            .find(|file| file.path.eq_ignore_ascii_case(FILE_WORKER_NAME))
            .ok_or_else(|| anyhow!("The complete package is missing its file worker"))?;
        let executable = std::env::current_exe()?;
        let source = executable
            .parent()
            .ok_or_else(|| anyhow!("Missing package directory"))?;
        return Ok((source.join(FILE_WORKER_NAME), file.sha256.clone()));
    }
    let snapshot =
        crate::install_manifest::windows::inspect::read(&request.root, &request.product)?;
    let record = snapshot
        .record
        .ok_or_else(|| anyhow!("Missing installation manifest"))?;
    let path = relative(FILE_WORKER_NAME)?;
    if !files::contains(&record.files, &path) {
        bail!("Use a matching complete EXE package to retry uninstallation");
    }
    let root = snapshot
        .root
        .ok_or_else(|| anyhow!("Missing installation root"))?;
    let mut worker = root.entry(&path, crate::install_manifest::windows::Access::Read)?;
    crate::install_manifest::windows::security::require_protected(&worker.file)?;
    let digest = crate::update_hash::sha256_reader_hex(&mut worker.file)?;
    Ok((request.root.join(FILE_WORKER_NAME), digest))
}

fn relative(path: &str) -> ResultType<RelativePath> {
    RelativePath::parse(path).map_err(|error| anyhow!(error))
}
