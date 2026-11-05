use super::{artifacts, directory::Directory, files::Files, inspect, store::Store, system};
use crate::install_manifest::{
    collection::FileChanges,
    request::{Operation, Request, COMPONENT_RUNTIME_DIR},
    InstallManifest, InstallState,
};
use std::io;

pub(crate) fn run(request: &Request) -> io::Result<()> {
    let _lock = system::OperationLock::acquire(&request.product)?;
    let snapshot = inspect::read(&request.root, &request.product)?;
    system::verify_registration(request)?;
    inspect::verify(request, &snapshot)?;
    for script in [&request.before, &request.after].into_iter().flatten() {
        system::verify_script(script)?;
    }
    if let Some(root) = &snapshot.root {
        artifacts::verify(root, &request.product)?;
    }
    let root = match snapshot.root {
        Some(root) => root,
        None => Directory::create(&request.root)?,
    };
    let (store, record) = prepare_record(&root, request)?;
    match request.operation {
        Operation::Install | Operation::Update => install(request, (&root, &store), record),
        Operation::Uninstall => uninstall(request, (root, store), record),
    }
}

fn prepare_record(root: &Directory, request: &Request) -> io::Result<(Store, InstallManifest)> {
    if request.expected_manifest_sha256.is_some() {
        let store = Store::open(root)?;
        let record = store.load(&inspect::installation_identity(root, &request.product)?)?;
        return Ok((store, record));
    }
    let (store, directories) = Store::create(root)?;
    let mut record = inspect::empty(root, request)?;
    record.created_dirs = directories;
    Ok((store, record))
}

fn install(
    request: &Request,
    location: (&Directory, &Store),
    record: InstallManifest,
) -> io::Result<()> {
    let (root, store) = location;
    let incoming: Vec<_> = request.files.iter().map(|file| file.path.clone()).collect();
    let changes = FileChanges::new(&record.files, &incoming, inspect::replacement(request))?;
    let mut next = record.clone();
    next.files = changes.managed.into_iter().cloned().collect();
    next.state = if request.operation == Operation::Install {
        InstallState::Installing
    } else {
        InstallState::Updating
    };
    store.save(&next)?;
    system::retain_location(request)?;
    system::run(request.before.as_ref())?;
    if request.operation == Operation::Install {
        artifacts::remove_known(root, &request.product)?;
    }
    let files = Files { root, store };
    for input in &request.files {
        next = files.install(&next, input)?;
    }
    for path in changes.remove {
        files.remove_owned(&next, path)?;
    }
    artifacts::install_broker(root, store, &next)?;
    system::run(request.after.as_ref())?;
    if !request.preserve_unlisted {
        next.files = incoming;
    }
    next.state = InstallState::Ready;
    next.package_version = request.package_version.clone();
    store.save(&next)
}

fn uninstall(
    request: &Request,
    location: (Directory, Store),
    record: InstallManifest,
) -> io::Result<()> {
    let (root, store) = location;
    let mut next = record.clone();
    if next.state != InstallState::Uninstalling {
        let _runtime = component_runtime(request)?;
        system::run(request.before.as_ref())?;
        next.state = InstallState::Uninstalling;
        store.save(&next)?;
    }
    let files = Files {
        root: &root,
        store: &store,
    };
    for path in &record.files {
        files.remove_owned(&next, path)?;
    }
    artifacts::remove_known(&root, &request.product)?;
    store.clear_unused_payload(&next)?;
    store
        .delete()
        .map_err(|error| finalization_error("installation manifest", error))?;
    system::remove_registration(request)
        .map_err(|error| finalization_error("uninstall registration", error))?;
    artifacts::remove_empty(&root, &next.created_dirs)
        .map_err(|error| finalization_error("empty installation directories", error))?;
    drop(root);
    artifacts::remove_empty_absolute(&request.root, Some(&next.identity.root))
        .map_err(|error| finalization_error("empty installation root", error))
}

fn component_runtime(request: &Request) -> io::Result<Option<Directory>> {
    if request.files.is_empty() {
        return Ok(None);
    }
    let output = std::env::var_os("RUSTDESK_OUTPUT_DIR")
        .ok_or_else(|| io::Error::other("Missing protected installer output directory"))?;
    let root =
        Directory::open(std::path::Path::new(&output))?.create_child(COMPONENT_RUNTIME_DIR)?;
    let (store, directories) = Store::create(&root)?;
    let mut record = inspect::empty(&root, request)?;
    record.files = request.files.iter().map(|file| file.path.clone()).collect();
    record.created_dirs = directories;
    store.save(&record)?;
    let files = Files {
        root: &root,
        store: &store,
    };
    for input in &request.files {
        record = files.install(&record, input)?;
    }
    Ok(Some(root))
}

fn finalization_error(item: &str, error: io::Error) -> io::Error {
    io::Error::other(format!(
        "Application files removed; could not finish {item}: {error}"
    ))
}
