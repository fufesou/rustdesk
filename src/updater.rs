use crate::{
    common::{
        display_version_from_release_id, release_download_base_url, release_id_from_update_url,
        release_metadata_url, release_signature_url, set_fixed_test_software_update_url,
        url_has_explicit_port,
    },
    hbbs_http::create_http_client_with_url_strict,
};
use hbb_common::{
    bail, config, log,
    update_metadata::{UpdateArtifactQuery, UpdateMetadataPolicy, VerifiedUpdateArtifact},
    ResultType,
};
use std::{
    collections::HashMap,
    io::{Read, Seek, Write},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{channel, Receiver, Sender},
        Mutex,
    },
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
use std::os::{
    fd::AsRawFd,
    unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
};

#[cfg(target_os = "macos")]
struct MacUpdateLock {
    _file: std::fs::File,
}

#[cfg(target_os = "macos")]
fn acquire_mac_update_lock() -> ResultType<MacUpdateLock> {
    let path = std::path::PathBuf::from("/var/run/rustdesk-update.lock");
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .custom_flags(hbb_common::libc::O_NOFOLLOW | hbb_common::libc::O_CLOEXEC)
        .open(&path)?;
    let metadata = handle.metadata()?;
    if !metadata.file_type().is_file() || metadata.uid() != 0 {
        bail!("[root-update] update lock is not a root-owned regular file");
    }
    handle.set_permissions(std::fs::Permissions::from_mode(0o600))?;

    // Keep the descriptor open through update preparation and detached-script
    // launch. O_CLOEXEC means this lock does not cover the detached bundle
    // swap; flock is released when this guard is dropped or the process exits.
    let lock_result = unsafe {
        hbb_common::libc::flock(
            handle.as_raw_fd(),
            hbb_common::libc::LOCK_EX | hbb_common::libc::LOCK_NB,
        )
    };
    if lock_result != 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::WouldBlock {
            bail!("[root-update] another update is already running");
        }
        return Err(err.into());
    }
    Ok(MacUpdateLock { _file: handle })
}

enum UpdateMsg {
    CheckUpdate,
    Exit,
}

lazy_static::lazy_static! {
    static ref TX_MSG : Mutex<Sender<UpdateMsg>> = Mutex::new(start_auto_update_check());
    static ref VERIFIED_UPDATE_ARTIFACTS: Mutex<HashMap<String, CachedVerifiedUpdateArtifact>> =
        Mutex::new(HashMap::new());
}

#[derive(Clone)]
struct CachedVerifiedUpdateArtifact {
    artifact: VerifiedUpdateArtifact,
    platform: String,
    arch: String,
    format: String,
}

static CONTROLLING_SESSION_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Initial wait after startup before the first update check (30 seconds).
pub const INITIAL_CHECK_DELAY: Duration = Duration::from_secs(30);

/// One full day — default interval between update checks.
pub const DUR_ONE_DAY: Duration = Duration::from_secs(60 * 60 * 24);

/// Minimum interval between consecutive update checks (10 minutes).
pub const MIN_INTERVAL: Duration = Duration::from_secs(60 * 10);

/// Retry interval when an update check fails or a session is active (30 minutes).
pub const RETRY_INTERVAL: Duration = Duration::from_secs(60 * 30);
const UPDATE_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const UPDATE_SIDECAR_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const UPDATE_METADATA_SIDECAR_MAX_BYTES: u64 = 1024 * 1024;
const UPDATE_FILE_CREATE_ATTEMPTS: usize = 16;

pub fn update_controlling_session_count(count: usize) {
    CONTROLLING_SESSION_COUNT.store(count, Ordering::SeqCst);
}

#[allow(dead_code)]
pub fn start_auto_update() {
    let _sender = TX_MSG.lock().unwrap();
}

#[allow(dead_code)]
pub fn manually_check_update() -> ResultType<()> {
    let sender = TX_MSG.lock().unwrap();
    sender.send(UpdateMsg::CheckUpdate)?;
    Ok(())
}

#[allow(dead_code)]
pub fn stop_auto_update() {
    let sender = TX_MSG.lock().unwrap();
    sender.send(UpdateMsg::Exit).unwrap_or_default();
}

#[inline]
/// Returns true when there are no active incoming or outgoing connections.
/// Used to avoid updating while a remote session is in progress.
pub fn has_no_active_conns() -> bool {
    let conns = crate::Connection::alive_conns();
    conns.is_empty() && has_no_controlling_conns()
}

#[cfg(any(not(target_os = "windows"), feature = "flutter"))]
fn has_no_controlling_conns() -> bool {
    CONTROLLING_SESSION_COUNT.load(Ordering::SeqCst) == 0
}

#[cfg(not(any(not(target_os = "windows"), feature = "flutter")))]
fn has_no_controlling_conns() -> bool {
    let app_exe = format!("{}.exe", crate::get_app_name().to_lowercase());
    for arg in [
        "--connect",
        "--play",
        "--file-transfer",
        "--view-camera",
        "--port-forward",
        "--rdp",
    ] {
        if !crate::platform::get_pids_of_process_with_first_arg(&app_exe, arg).is_empty() {
            return false;
        }
    }
    true
}

fn start_auto_update_check() -> Sender<UpdateMsg> {
    let (tx, rx) = channel();
    std::thread::spawn(move || start_auto_update_check_(rx));
    return tx;
}

fn start_auto_update_check_(rx_msg: Receiver<UpdateMsg>) {
    std::thread::sleep(INITIAL_CHECK_DELAY);
    if let Err(e) = check_update(false) {
        log::error!("Error checking for updates: {}", e);
    }

    let mut last_check_time = Instant::now();
    let mut check_interval = DUR_ONE_DAY;
    loop {
        let recv_res = rx_msg.recv_timeout(check_interval);
        match &recv_res {
            Ok(UpdateMsg::CheckUpdate) | Err(_) => {
                if last_check_time.elapsed() < MIN_INTERVAL {
                    // log::debug!("Update check skipped due to minimum interval.");
                    continue;
                }
                // Don't check update if there are alive connections.
                if !has_no_active_conns() {
                    check_interval = RETRY_INTERVAL;
                    continue;
                }
                if let Err(e) = check_update(matches!(recv_res, Ok(UpdateMsg::CheckUpdate))) {
                    log::error!("Error checking for updates: {}", e);
                    check_interval = RETRY_INTERVAL;
                } else {
                    last_check_time = Instant::now();
                    check_interval = DUR_ONE_DAY;
                }
            }
            Ok(UpdateMsg::Exit) => break,
        }
    }
}

fn check_update(manually: bool) -> ResultType<()> {
    // On macOS, auto-update is handled by check_update_as_root() in the service process.
    // The shared check_update() path is only used for manual update checks from the GUI.
    #[cfg(target_os = "macos")]
    if !manually {
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    let update_msi = crate::platform::is_msi_installed()? && !crate::is_custom_client();
    #[cfg(not(target_os = "windows"))]
    let update_msi = false;
    if !(manually || config::Config::get_bool_option(config::keys::OPTION_ALLOW_AUTO_UPDATE)) {
        return Ok(());
    }
    set_fixed_test_software_update_url();

    let update_url = crate::common::SOFTWARE_UPDATE_URL.lock().unwrap().clone();
    if update_url.is_empty() {
        log::debug!("No update available.");
    } else {
        let update_format = current_update_format(update_msi);
        if update_format == "unknown" {
            log::debug!("Automatic update is not supported on this platform.");
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        if !manually {
            log::debug!("Background auto-install is not supported on macOS.");
            return Ok(());
        }
        let query = UpdateArtifactQuery {
            platform: current_update_platform(),
            arch: current_update_arch(),
            format: update_format,
            file_name: None,
        };
        let artifact = verified_update_artifact_from_release_page_url(&update_url, &query)?;
        let download_url = artifact.url.as_str();
        #[cfg(target_os = "windows")]
        let version = artifact.version.as_str();
        #[cfg(target_os = "windows")]
        log::debug!("New version available: {}", &version);
        let Some(file_path) = get_download_file_from_url(download_url) else {
            bail!("Failed to get the file path from the URL: {}", download_url);
        };
        ensure_verified_update_artifact(download_url, &file_path, artifact.size, &artifact.sha256)?;
        // We have checked if the `conns` is empty before, but we need to check again.
        // No need to care about the downloaded file here, because it's rare case that the `conns` are empty
        // before the download, but not empty after the download.
        if has_no_active_conns() {
            #[cfg(target_os = "windows")]
            update_new_version(update_msi, version, &file_path, &artifact.sha256);
            #[cfg(target_os = "macos")]
            {
                let Some(file_path) = file_path.to_str() else {
                    bail!("Invalid UTF-8 path: {}", file_path.display());
                };
                crate::platform::macos::update_to_verified_dmg(
                    file_path,
                    &artifact.sha256,
                    Some(artifact.size),
                )?;
            }
        }
    }
    Ok(())
}

pub(crate) fn current_update_platform() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "windows"
    }
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::consts::OS
    }
}

pub(crate) fn current_update_arch() -> &'static str {
    #[cfg(all(target_os = "windows", not(feature = "flutter")))]
    {
        "x86"
    }
    #[cfg(not(all(target_os = "windows", not(feature = "flutter"))))]
    {
        std::env::consts::ARCH
    }
}

pub(crate) fn current_update_format(update_msi: bool) -> &'static str {
    #[cfg(any(
        not(target_os = "windows"),
        all(target_os = "windows", not(feature = "flutter"))
    ))]
    let _ = update_msi;
    #[cfg(all(target_os = "windows", feature = "flutter"))]
    {
        if update_msi {
            return "msi";
        }
        "exe"
    }
    #[cfg(all(target_os = "windows", not(feature = "flutter")))]
    {
        "exe"
    }
    #[cfg(target_os = "macos")]
    {
        "dmg"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        "unknown"
    }
}

pub fn current_update_artifact_query(update_msi: bool) -> UpdateArtifactQuery<'static> {
    UpdateArtifactQuery {
        platform: current_update_platform(),
        arch: current_update_arch(),
        format: current_update_format(update_msi),
        file_name: None,
    }
}

pub fn verified_update_artifact_for_release_page_url(
    release_page_url: &str,
    query: UpdateArtifactQuery<'_>,
) -> ResultType<VerifiedUpdateArtifact> {
    let artifact = verified_update_artifact_from_release_page_url(release_page_url, &query)?;
    cache_verified_update_artifact(&artifact, &query);
    Ok(artifact)
}

pub fn verified_update_artifact_for_download_url(
    download_url: &str,
) -> ResultType<VerifiedUpdateArtifact> {
    let download = parse_rustdesk_release_download_url(download_url)?;
    let format = update_format_from_file_name(&download.file_name)?;
    let query = UpdateArtifactQuery {
        platform: current_update_platform(),
        arch: current_update_arch(),
        format,
        file_name: Some(download.file_name.as_str()),
    };
    verified_update_artifact_for_download_url_with_query(download_url, query)
}

fn verified_update_artifact_for_download_url_with_query(
    download_url: &str,
    query: UpdateArtifactQuery<'_>,
) -> ResultType<VerifiedUpdateArtifact> {
    if let Some(cached) = get_cached_verified_update_artifact(download_url) {
        if cached.platform != query.platform
            || cached.arch != query.arch
            || cached.format != query.format
        {
            bail!("cached update artifact selector does not match requested selector");
        }
        let artifact = cached.artifact;
        if query
            .file_name
            .is_some_and(|file_name| artifact.file_name != file_name)
        {
            bail!("cached update artifact file name does not match requested file name");
        }
        let cached_format = update_format_from_file_name(&artifact.file_name)?;
        if cached_format != query.format {
            bail!("cached update artifact format does not match requested format");
        }
        return Ok(artifact);
    }
    let download = parse_rustdesk_release_download_url(download_url)?;
    let release_page_url = format!(
        "https://github.com/{}/{}/releases/tag/{}",
        download.owner, download.repo, download.release_id
    );
    let artifact = verified_update_artifact_from_release_page_url(&release_page_url, &query)?;
    if artifact.url != download_url {
        bail!("update artifact URL does not match requested download URL");
    }
    cache_verified_update_artifact(&artifact, &query);
    Ok(artifact)
}

fn verified_update_artifact_from_release_page_url(
    update_url: &str,
    query: &UpdateArtifactQuery<'_>,
) -> ResultType<VerifiedUpdateArtifact> {
    let release_id = release_id_from_update_url(update_url)?;
    let display_version = display_version_from_release_id(&release_id)?;
    let expected_artifact_url_prefix = release_download_base_url(update_url)?;
    let metadata_url = release_metadata_url(update_url)?;
    let signature_url = release_signature_url(update_url)?;
    let metadata_bytes = fetch_update_sidecar_bytes(&metadata_url)?;
    let signature_bytes = fetch_update_sidecar_bytes(&signature_url)?;
    verify_update_metadata_bytes(
        &metadata_bytes,
        &signature_bytes,
        display_version.as_str(),
        release_id.as_str(),
        expected_artifact_url_prefix.as_str(),
        query,
    )
}

fn verify_update_metadata_bytes(
    metadata_bytes: &[u8],
    signature_bytes: &[u8],
    display_version: &str,
    release_id: &str,
    expected_artifact_url_prefix: &str,
    query: &UpdateArtifactQuery<'_>,
) -> ResultType<VerifiedUpdateArtifact> {
    let policy = UpdateMetadataPolicy {
        app: "rustdesk",
        allowed_package_ids: &["rustdesk"],
        expected_version: Some(display_version),
        expected_release_id: Some(release_id),
        expected_artifact_url_prefix: Some(expected_artifact_url_prefix),
    };
    hbb_common::update_metadata::verify_update_metadata(
        metadata_bytes,
        signature_bytes,
        &policy,
        query,
    )
}

struct ReleaseDownloadUrl {
    owner: String,
    repo: String,
    release_id: String,
    file_name: String,
}

fn parse_rustdesk_release_download_url(download_url: &str) -> ResultType<ReleaseDownloadUrl> {
    let url = url::Url::parse(download_url)?;
    if url.scheme() != "https" || url.host_str() != Some("github.com") {
        bail!(
            "Update download URL is not a GitHub HTTPS release URL: {}",
            download_url
        );
    }
    if url_has_explicit_port(download_url)
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!(
            "Update download URL must not contain credentials or an explicit port: {}",
            download_url
        );
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!(
            "Update download URL must not contain query or fragment: {}",
            download_url
        );
    }
    let Some(segments) = url.path_segments() else {
        bail!("Update download URL has no path: {}", download_url);
    };
    let segments = segments.collect::<Vec<_>>();
    let (owner, repo, release_id, file_name) = match segments.as_slice() {
        [owner @ "rustdesk", repo @ "rustdesk", "releases", "download", release_id, file_name]
        | [owner @ "fufesou", repo @ "rustdesk", "releases", "download", release_id, file_name] => {
            (*owner, *repo, *release_id, *file_name)
        }
        _ => bail!(
            "Update download URL is not a RustDesk release download URL: {}",
            download_url
        ),
    };
    if release_id.is_empty() || file_name.is_empty() {
        bail!("Update download URL has empty release id or file name");
    }
    if owner == "fufesou" && release_id != crate::common::FIXED_TEST_UPDATE_RELEASE_ID {
        bail!("Update download URL is not the fixed test release: {download_url}");
    }
    Ok(ReleaseDownloadUrl {
        owner: owner.to_owned(),
        repo: repo.to_owned(),
        release_id: release_id.to_owned(),
        file_name: file_name.to_owned(),
    })
}

fn update_format_from_file_name(file_name: &str) -> ResultType<&'static str> {
    let normalized_file_name = file_name.to_ascii_lowercase();
    if normalized_file_name.ends_with(".msi") {
        return Ok("msi");
    }
    if normalized_file_name.ends_with(".exe") {
        return Ok("exe");
    }
    if normalized_file_name.ends_with(".dmg") {
        return Ok("dmg");
    }
    bail!("Unsupported update artifact file format: {}", file_name);
}

fn cache_verified_update_artifact(
    artifact: &VerifiedUpdateArtifact,
    query: &UpdateArtifactQuery<'_>,
) {
    let cached = CachedVerifiedUpdateArtifact {
        artifact: artifact.clone(),
        platform: query.platform.to_owned(),
        arch: query.arch.to_owned(),
        format: query.format.to_owned(),
    };
    let mut cache = VERIFIED_UPDATE_ARTIFACTS.lock().unwrap();
    cache.retain(|_, entry| entry.artifact.release_id == artifact.release_id);
    cache.insert(artifact.url.clone(), cached);
}

fn get_cached_verified_update_artifact(download_url: &str) -> Option<CachedVerifiedUpdateArtifact> {
    VERIFIED_UPDATE_ARTIFACTS
        .lock()
        .unwrap()
        .get(download_url)
        .cloned()
}

fn read_limited_response_bytes<R: Read>(
    reader: &mut R,
    limit: u64,
    what: &str,
) -> ResultType<Vec<u8>> {
    let mut limited_reader = reader.take(limit.saturating_add(1));
    let mut bytes = Vec::new();
    limited_reader.read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("{what} exceeds maximum allowed size of {limit} bytes");
    }
    Ok(bytes)
}

fn fetch_update_sidecar_bytes(url: &str) -> ResultType<Vec<u8>> {
    let client = create_http_client_with_url_strict(url)?;
    let mut response = client
        .get(url)
        .timeout(UPDATE_SIDECAR_HTTP_REQUEST_TIMEOUT)
        .send()?;
    if !response.status().is_success() {
        bail!(
            "Failed to download update metadata sidecar: {}",
            response.status()
        );
    }
    read_limited_response_bytes(
        &mut response,
        UPDATE_METADATA_SIDECAR_MAX_BYTES,
        "Update metadata sidecar",
    )
}

fn ensure_verified_update_artifact(
    download_url: &str,
    file_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> ResultType<()> {
    let mut is_file_exists = false;
    if let Some(file_size) = cached_update_artifact_size(file_path)? {
        if file_size == expected_size {
            match verify_file_sha256(file_path, expected_sha256) {
                Ok(()) => is_file_exists = true,
                Err(e) => {
                    log::warn!("Removing cached update file with invalid SHA256: {}", e);
                    remove_cached_update_artifact(file_path)?;
                }
            }
        } else {
            log::warn!(
                "Removing cached update file with size mismatch for {}: expected {}, got {}",
                file_path.display(),
                expected_size,
                file_size
            );
            remove_cached_update_artifact(file_path)?;
        }
    }
    if !is_file_exists {
        let client = create_http_client_with_url_strict(download_url)?;
        let response = client
            .get(download_url)
            .timeout(UPDATE_HTTP_REQUEST_TIMEOUT)
            .send()?;
        if !response.status().is_success() {
            bail!(
                "Failed to download the new version file: {}",
                response.status()
            );
        }
        let mut limited_response = response.take(expected_size.saturating_add(1));
        write_verified_update_artifact(
            file_path,
            &mut limited_response,
            expected_size,
            expected_sha256,
        )?;
    }
    Ok(())
}

fn cached_update_artifact_size(file_path: &Path) -> ResultType<Option<u64>> {
    let metadata = match std::fs::symlink_metadata(file_path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if metadata.file_type().is_file() {
        return Ok(Some(metadata.len()));
    }
    bail!(
        "Refusing to use update cache path that is not a regular file: {}",
        file_path.display()
    )
}

fn remove_cached_update_artifact(file_path: &Path) -> ResultType<()> {
    let metadata = match std::fs::symlink_metadata(file_path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let file_type = metadata.file_type();
    if file_type.is_file() || file_type.is_symlink() {
        std::fs::remove_file(file_path)?;
    } else {
        bail!(
            "Refusing to remove update cache path that is not a file: {}",
            file_path.display()
        );
    }
    Ok(())
}

pub(crate) fn remove_update_file(file_path: &Path) {
    match std::fs::remove_file(file_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!(
            "Failed to remove update file {}: {}",
            file_path.display(),
            e
        ),
    }
}

#[cfg(target_os = "windows")]
fn verified_update_path(
    p: &str,
    expected_sha256: &str,
    kind: &str,
    file_path: &Path,
) -> Option<(crate::platform::VerifiedUpdateFile, String)> {
    let update_file = match crate::platform::copy_and_verify_update_file_sha256(p, expected_sha256)
    {
        Ok(update_file) => update_file,
        Err(e) => {
            log::error!("Refusing to update from invalid {}: {}", kind, e);
            remove_update_file(file_path);
            return None;
        }
    };
    let update_path = match update_file.path_str() {
        Ok(path) => path.to_owned(),
        Err(e) => {
            log::error!("Failed to get verified {} path: {}", kind, e);
            update_file.cleanup();
            remove_update_file(file_path);
            return None;
        }
    };
    Some((update_file, update_path))
}

#[cfg(target_os = "windows")]
fn update_new_version(update_msi: bool, version: &str, file_path: &PathBuf, expected_sha256: &str) {
    log::debug!(
        "New version is downloaded, update begin, update msi: {update_msi}, version: {version}, file: {:?}",
        file_path.to_str()
    );
    if let Some(p) = file_path.to_str() {
        if let Some(session_id) = crate::platform::get_current_process_session_id() {
            if update_msi {
                let Some((update_file, update_path)) =
                    verified_update_path(p, expected_sha256, "msi", file_path)
                else {
                    return;
                };
                let result = crate::platform::update_me_msi(&update_path, true);
                match crate::platform::finish_verified_update_launch(update_file, "msi", result) {
                    Ok(_) => {
                        log::debug!("New version \"{}\" updated.", version);
                    }
                    Err(e) => {
                        log::error!(
                            "Failed to install the new msi version  \"{}\": {}",
                            version,
                            e
                        );
                        remove_update_file(file_path);
                    }
                }
            } else {
                let Some((update_file, update_path)) =
                    verified_update_path(p, expected_sha256, "exe", file_path)
                else {
                    return;
                };
                let custom_client_staging_dir = if crate::is_custom_client() {
                    let custom_client_staging_dir =
                        crate::platform::get_custom_client_staging_dir();
                    if let Err(e) = crate::platform::handle_custom_client_staging_dir_before_update(
                        &custom_client_staging_dir,
                    ) {
                        log::error!(
                            "Failed to handle custom client staging dir before update: {}",
                            e
                        );
                        update_file.cleanup();
                        remove_update_file(file_path);
                        return;
                    }
                    Some(custom_client_staging_dir)
                } else {
                    // Clean up any residual staging directory from previous custom client
                    let staging_dir = crate::platform::get_custom_client_staging_dir();
                    hbb_common::allow_err!(crate::platform::remove_custom_client_staging_dir(
                        &staging_dir
                    ));
                    None
                };
                let update_launched = match crate::platform::launch_privileged_process(
                    session_id,
                    &format!("\"{}\" --update", update_path),
                ) {
                    Ok(h) => {
                        if h.is_null() {
                            log::error!("Failed to update to the new version: {}", version);
                            false
                        } else {
                            log::debug!("New version \"{}\" is launched.", version);
                            unsafe {
                                winapi::um::handleapi::CloseHandle(h);
                            }
                            true
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to run the new version: {}", e);
                        false
                    }
                };
                if !update_launched {
                    if let Some(dir) = custom_client_staging_dir {
                        hbb_common::allow_err!(crate::platform::remove_custom_client_staging_dir(
                            &dir
                        ));
                    }
                    update_file.cleanup();
                    remove_update_file(file_path);
                }
            }
        } else {
            log::error!(
                "Failed to get the current process session id, Error {}",
                std::io::Error::last_os_error()
            );
            remove_update_file(file_path);
        }
    } else {
        // unreachable!()
        log::error!(
            "Failed to convert the file path to string: {}",
            file_path.display()
        );
    }
}

pub fn get_update_download_file_from_url(url: &str) -> Option<PathBuf> {
    let parsed = url::Url::parse(url).ok()?;
    // Check the raw prefix before Url normalizes default ports.
    if !url.starts_with("https://github.com/")
        || parsed.scheme() != "https"
        || parsed.host_str() != Some("github.com")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }

    let mut segments = parsed.path_segments()?;
    let owner = segments.next()?;
    let repo = segments.next()?;
    let releases = segments.next()?;
    let download = segments.next()?;
    let tag = segments.next()?;
    let filename = segments.next()?;

    if !is_allowed_update_release(owner, repo, tag)
        || releases != "releases"
        || download != "download"
        || tag.is_empty()
        || segments.next().is_some()
        || !is_plain_update_filename(filename)
    {
        return None;
    }

    Some(std::env::temp_dir().join(filename))
}

fn is_allowed_update_release(owner: &str, repo: &str, tag: &str) -> bool {
    (owner == "rustdesk" && repo == "rustdesk")
        || (owner == "fufesou"
            && repo == "rustdesk"
            && tag == crate::common::FIXED_TEST_UPDATE_RELEASE_ID)
}

fn is_plain_update_filename(filename: &str) -> bool {
    if filename.is_empty()
        || filename.contains('/')
        || filename.contains('\\')
        || filename.contains(':')
    {
        return false;
    }

    let mut components = Path::new(filename).components();
    matches!(
        components.next(),
        Some(Component::Normal(name)) if name.to_str() == Some(filename)
    ) && components.next().is_none()
}

pub fn get_download_file_from_url(url: &str) -> Option<PathBuf> {
    get_update_download_file_from_url(url)
}

/// Queries all active connections (remote, file-transfer, port-forward, camera, terminal)
/// from every logged-in user's --server process via IPC.
/// The root service cannot read connection state directly since connections
/// live in user --server processes. Handles fast user switching by querying
/// all GUI users, including the login-window server at UID 0. Falls back to
/// false (assumes sessions active) on any IPC error to avoid updating during
/// an unknown session state.
#[cfg(target_os = "macos")]
pub fn has_no_active_conns_ipc() -> bool {
    let rt = match hbb_common::tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return false,
    };
    rt.block_on(async {
        // Use the same GUI-domain-filtered UID set as the update script.
        // Shell-only SSH/TTY users are excluded, while an empty GUI set maps
        // to UID 0 so the LoginWindow server is queried rather than assumed idle.
        let uids = crate::platform::get_logged_in_uids();
        // Check each user's server — fail closed if any has active connections
        for uid in uids {
            if let Ok(mut conn) = crate::ipc::connect_for_uid(1000, uid, "").await {
                if conn.send(&crate::ipc::Data::HasNoActiveConns(None)).await.is_ok() {
                    match conn.next_timeout(1000).await {
                        Ok(Some(crate::ipc::Data::HasNoActiveConns(Some(true)))) => {
                            // Explicit no active connections — safe to continue
                        }
                        Ok(Some(crate::ipc::Data::HasNoActiveConns(Some(false)))) => {
                            return false; // Explicit active connections
                        }
                        _ => {
                            return false; // Timeout/error/unexpected — fail closed
                        }
                    }
                } else {
                    return false; // Send failed — fail closed
                }
            } else {
                return false; // Connection failed — fail closed
            }
        }
        true // All users explicitly confirmed no active connections
    })
}

#[cfg(target_os = "macos")]
fn wait_for_failed_update_retry() {
    const FAILURE_MARKER: &str = "/var/root/.rustdeskupdate_failed";
    let marker = std::path::Path::new(FAILURE_MARKER);
    if !marker.exists() {
        return;
    }

    // The updater script records failure immediately before launchd restarts
    // the old daemon. Preserve the retry deadline across that restart instead
    // of consuming the marker and retrying the same broken release in 30 sec.
    let remaining = std::fs::metadata(marker)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| {
            std::time::SystemTime::now()
                .duration_since(modified)
                .ok()
        })
        .map(|elapsed| RETRY_INTERVAL.saturating_sub(elapsed))
        .unwrap_or(RETRY_INTERVAL);
    if !remaining.is_zero() {
        log::info!(
            "[root-update] Previous update failed; retrying in {} seconds.",
            remaining.as_secs()
        );
        std::thread::sleep(remaining);
    }
    match std::fs::remove_file(marker) {
        Ok(()) => log::info!("[root-update] Previous update retry interval elapsed."),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => log::warn!("[root-update] Failed to clear failure marker: {}", err),
    }
}

/// Starts the background silent auto-update scheduler for macOS.
/// Called from `start_os_service()` which runs as root via LaunchDaemon.
#[cfg(target_os = "macos")]
pub fn start_auto_update_macos() {
    let spawn_result = std::thread::Builder::new()
        .name("rustdesk-auto-update".to_owned())
        .spawn(|| {
            log::info!("[root-update] Auto-update scheduler thread started.");
            std::thread::sleep(INITIAL_CHECK_DELAY);
            wait_for_failed_update_retry();
            let mut interval = DUR_ONE_DAY;
            loop {
                log::info!("[root-update] Running scheduled update check...");
                let no_active_conns = has_no_active_conns_ipc();
                if !no_active_conns {
                    log::info!("[root-update] Active session in progress, retrying in 10 min.");
                    interval = MIN_INTERVAL;
                } else {
                    match check_update_as_root() {
                        Ok(update_started) => {
                            if update_started {
                                // The replacement script is detached and may fail
                                // after this process returns. Always retry at the
                                // failure interval until the new daemon replaces us.
                                interval = RETRY_INTERVAL;
                            } else {
                                interval = DUR_ONE_DAY;
                            }
                        }
                        Err(e) => {
                            log::error!("[root-update] Update check failed: {}", e);
                            interval = RETRY_INTERVAL;
                        }
                    }
                }
                std::thread::sleep(interval);
            }
        });
    if let Err(err) = spawn_result {
        log::error!("[root-update] Failed to start scheduler thread: {}", err);
    }
}

#[cfg(target_os = "macos")]
pub fn check_update_as_root() -> ResultType<bool> {
    let _update_lock = acquire_mac_update_lock()?;
    // Allow-auto-update setting
    if !config::Config::get_bool_option(config::keys::OPTION_ALLOW_AUTO_UPDATE) {
        log::info!("[root-update] Auto update is disabled, skipping.");
        return Ok(false);
    }
    if crate::is_custom_client() {
        log::info!("[root-update] Custom client detected, skipping stock update.");
        return Ok(false);
    }
    // Clean up only old temp dirs from previous failed updates. The detached
    // installer keeps using its update directory after this process exits and
    // releases the advisory lock, so a newly-started daemon must not remove a
    // directory that still belongs to the active transaction.
    if let Ok(entries) = std::fs::read_dir("/tmp") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with(".rustdeskupdate-root-")
                || name_str.starts_with(".rustdeskdownload-")
            {
                let path = entry.path();
                let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                let mode = metadata.mode() & 0o7777;
                let is_stale = metadata
                    .modified()
                    .ok()
                    .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok())
                    .is_some_and(|age| age >= RETRY_INTERVAL);
                if metadata.file_type().is_dir() && metadata.uid() == 0 && mode == 0o700 && is_stale
                {
                    if let Err(err) = std::fs::remove_dir_all(&path) {
                        log::warn!(
                            "[root-update] Failed to remove stale temp dir {}: {}",
                            path.display(),
                            err
                        );
                    }
                }
            }
        }
    }
    set_fixed_test_software_update_url();
    let update_url = crate::common::SOFTWARE_UPDATE_URL.lock().unwrap().clone();
    if update_url.is_empty() {
        log::info!("[root-update] No update available.");
        return Ok(false);
    }
    let query = current_update_artifact_query(false);
    let artifact = verified_update_artifact_from_release_page_url(&update_url, &query)?;
    let dmg_url = artifact.url.as_str();
    log::info!(
        "[root-update] New version: {}, downloading from {}",
        artifact.version,
        dmg_url
    );
    // Validate URL against GitHub release allowlist before downloading as root
    let Some(file_path_validated) = get_update_download_file_from_url(dmg_url) else {
        bail!("[root-update] URL failed allowlist check: {}", dmg_url);
    };
    drop(file_path_validated);
    // Use mktemp so a local user cannot pre-create a predictable path and
    // permanently deny updates for a reused service PID.
    let private_tmp_output = std::process::Command::new("/usr/bin/mktemp")
        .args(["-d", "/tmp/.rustdeskdownload-XXXXXX"])
        .output()?;
    if !private_tmp_output.status.success() {
        bail!(
            "[root-update] Failed to create private download directory: {}",
            String::from_utf8_lossy(&private_tmp_output.stderr).trim()
        );
    }
    let private_tmp = String::from_utf8(private_tmp_output.stdout)
        .map_err(|err| hbb_common::anyhow::anyhow!("[root-update] mktemp output error: {}", err))?
        .trim()
        .to_owned();
    if private_tmp.is_empty() {
        bail!("[root-update] mktemp returned an empty download directory");
    }
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&private_tmp, std::fs::Permissions::from_mode(0o700))?;
    }
    let file_path = Path::new(&private_tmp).join(&artifact.file_name);
    let tmp_path = file_path.to_string_lossy().to_string();
    if let Err(err) =
        ensure_verified_update_artifact(dmg_url, &file_path, artifact.size, &artifact.sha256)
    {
        if let Err(cleanup_err) = std::fs::remove_dir_all(&private_tmp) {
            log::warn!(
                "[root-update] Failed to remove temp dir {}: {}",
                private_tmp,
                cleanup_err
            );
        }
        return Err(err);
    }
    log::info!("[root-update] Downloaded and verified at {}", tmp_path);
    // Recheck active sessions before installing — download can take minutes
    if !has_no_active_conns_ipc() {
        if let Err(e) = std::fs::remove_dir_all(&private_tmp) {
            log::warn!("[root-update] Failed to remove temp dir {}: {}", private_tmp, e);
        }
        bail!("[root-update] Active session started during download, deferring update.");
    }
    // Install silently as root
    let result = crate::platform::update_from_dmg_as_root(&tmp_path, &artifact.version);
    // Clean up download directory
    if let Err(e) = std::fs::remove_dir_all(&private_tmp) {
        log::warn!("[root-update] Failed to remove temp dir {}: {}", private_tmp, e);
    }
    result.map(|_| true)
}

fn create_download_temp_file(final_path: &Path) -> ResultType<(std::fs::File, PathBuf)> {
    let Some(download_dir) = final_path.parent() else {
        bail!(
            "Update file has no parent directory: {}",
            final_path.display()
        );
    };
    let Some(file_name) = final_path.file_name() else {
        bail!("Update file has no file name: {}", final_path.display());
    };
    let file_name = file_name.to_string_lossy();
    for _ in 0..UPDATE_FILE_CREATE_ATTEMPTS {
        let temp_path = download_dir.join(format!(
            ".{}.{}.{}.download",
            file_name,
            std::process::id(),
            hbb_common::rand::random::<u64>()
        ));
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((file, temp_path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    bail!("Failed to create temporary update file");
}

fn write_verified_update_artifact<R: Read>(
    final_path: &Path,
    reader: &mut R,
    expected_size: u64,
    expected_sha256: &str,
) -> ResultType<()> {
    let (mut file, temp_path) = create_download_temp_file(final_path)?;
    if let Err(e) = copy_and_verify_update_artifact(
        &mut file,
        &temp_path,
        reader,
        expected_size,
        expected_sha256,
    ) {
        remove_update_file(&temp_path);
        return Err(e);
    }
    drop(file);
    if let Err(e) = remove_cached_update_artifact(final_path) {
        remove_update_file(&temp_path);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&temp_path, final_path) {
        remove_update_file(&temp_path);
        return Err(e.into());
    }
    Ok(())
}

fn copy_and_verify_update_artifact<R: Read>(
    file: &mut std::fs::File,
    temp_path: &Path,
    reader: &mut R,
    expected_size: u64,
    expected_sha256: &str,
) -> ResultType<()> {
    let bytes_written = std::io::copy(reader, file)?;
    file.flush()?;
    if bytes_written != expected_size {
        bail!(
            "Update artifact size mismatch for {}: expected {}, got {}",
            temp_path.display(),
            expected_size,
            bytes_written
        );
    }
    verify_update_file_sha256(file, temp_path, expected_sha256)
}

fn verify_file_sha256(path: &Path, expected_sha256: &str) -> ResultType<()> {
    let mut file = std::fs::File::open(path)?;
    verify_update_file_sha256(&mut file, path, expected_sha256)
}

fn verify_update_file_sha256<R: Read + Seek>(
    reader: &mut R,
    path: &Path,
    expected_sha256: &str,
) -> ResultType<()> {
    use crate::update_hash::{verify_sha256_reader, Sha256VerificationError};

    match verify_sha256_reader(reader, expected_sha256) {
        Ok(()) => Ok(()),
        Err(Sha256VerificationError::InvalidExpected) => bail!(
            "Expected update file SHA256 is malformed for {}",
            path.display()
        ),
        Err(Sha256VerificationError::Mismatch {
            expected_sha256,
            actual_sha256,
        }) => bail!(
            "SHA256 mismatch for {}: expected {}, got {}",
            path.display(),
            expected_sha256,
            actual_sha256
        ),
        Err(Sha256VerificationError::Io(err)) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_download_file_accepts_expected_github_asset_urls() {
        let file = get_download_file_from_url(
            "https://github.com/rustdesk/rustdesk/releases/download/1.4.0/rustdesk-1.4.0-x86_64.dmg",
        )
        .expect("valid GitHub release asset URL");

        assert_eq!(
            file.file_name().and_then(|name| name.to_str()),
            Some("rustdesk-1.4.0-x86_64.dmg")
        );
    }

    #[test]
    fn update_download_file_rejects_untrusted_or_malformed_urls() {
        for url in [
            "http://github.com/rustdesk/rustdesk/releases/download/1/rustdesk.exe",
            "https://example.com/rustdesk.exe",
            "https://github.com/other/project/releases/download/1/rustdesk.exe",
            "https://github.com/rustdesk/rustdesk/releases/download/1/",
            "https://github.com/rustdesk/rustdesk/releases/download/1/nested/rustdesk.exe",
            "https://github.com/rustdesk/rustdesk/releases/download/1/C:rustdesk.exe",
            "https://user@github.com/rustdesk/rustdesk/releases/download/1/rustdesk.exe",
            "https://github.com:443/rustdesk/rustdesk/releases/download/1/rustdesk.exe",
            "https://github.com/rustdesk/rustdesk/releases/download/1/rustdesk.exe?download=1",
            "https://github.com/rustdesk/rustdesk/releases/download/1/rustdesk.exe#download",
            "not a url",
        ] {
            assert!(get_download_file_from_url(url).is_none(), "{url}");
        }
    }

    fn verified_artifact() -> VerifiedUpdateArtifact {
        VerifiedUpdateArtifact {
            version: "1.4.6".to_owned(),
            release_id: "v1.4.6".to_owned(),
            package_id: "rustdesk".to_owned(),
            url: "https://github.com/rustdesk/rustdesk/releases/download/v1.4.6/rustdesk.exe"
                .to_owned(),
            file_name: "rustdesk.exe".to_owned(),
            size: 6,
            sha256: "2937013f2181810606b2a799b05bda2849f3e369a20982a4138f0e0a55984ce4".to_owned(),
        }
    }

    fn cache_windows_exe(artifact: &VerifiedUpdateArtifact) {
        cache_verified_update_artifact(
            artifact,
            &UpdateArtifactQuery {
                platform: "windows",
                arch: "x86_64",
                format: "exe",
                file_name: None,
            },
        );
    }

    #[cfg(all(target_os = "windows", not(feature = "flutter")))]
    #[test]
    fn current_update_format_uses_exe_for_non_flutter_windows() {
        assert_eq!(current_update_format(true), "exe");
        assert_eq!(current_update_format(false), "exe");
    }

    #[test]
    fn limited_sidecar_reader_rejects_oversized_payloads() {
        let mut payload: &[u8] = b"rustdesk";
        assert_eq!(
            read_limited_response_bytes(&mut payload, 8, "sidecar")
                .unwrap()
                .len(),
            8
        );

        let mut oversized: &[u8] = b"too-large";
        assert!(read_limited_response_bytes(&mut oversized, 4, "sidecar").is_err());
    }

    #[test]
    fn parse_rustdesk_release_download_url_accepts_expected_path() {
        let parsed = parse_rustdesk_release_download_url(
            "https://github.com/rustdesk/rustdesk/releases/download/v1.4.6/rustdesk.exe",
        )
        .unwrap();

        assert_eq!(parsed.release_id, "v1.4.6");
        assert_eq!(parsed.file_name, "rustdesk.exe");
    }

    #[test]
    fn fixed_test_release_download_url_is_accepted() {
        let download_url = "https://github.com/fufesou/rustdesk/releases/download/fix-update-metadata/rustdesk.exe";
        let parsed = parse_rustdesk_release_download_url(download_url).unwrap();

        assert_eq!(parsed.owner, "fufesou");
        assert_eq!(parsed.repo, "rustdesk");
        assert_eq!(parsed.release_id, "fix-update-metadata");
        assert_eq!(parsed.file_name, "rustdesk.exe");
        assert_eq!(
            get_download_file_from_url(download_url)
                .and_then(|path| path.file_name().map(|name| name.to_owned())),
            Some(std::ffi::OsString::from("rustdesk.exe"))
        );
    }

    #[test]
    fn parse_rustdesk_release_download_url_rejects_untrusted_urls() {
        assert!(parse_rustdesk_release_download_url(
            "https://example.com/rustdesk/rustdesk/releases/download/v1.4.6/rustdesk.exe",
        )
        .is_err());
        assert!(parse_rustdesk_release_download_url(
            "https://github.com/other/rustdesk/releases/download/v1.4.6/rustdesk.exe",
        )
        .is_err());
        assert!(parse_rustdesk_release_download_url(
            "https://github.com/rustdesk/rustdesk/releases/tag/v1.4.6",
        )
        .is_err());
        assert!(parse_rustdesk_release_download_url(
            "https://github.com/rustdesk/rustdesk/releases/download/v1.4.6/rustdesk.exe?x=1",
        )
        .is_err());
        assert!(parse_rustdesk_release_download_url(
            "https://user@github.com/rustdesk/rustdesk/releases/download/v1.4.6/rustdesk.exe",
        )
        .is_err());
        assert!(parse_rustdesk_release_download_url(
            "https://github.com:8443/rustdesk/rustdesk/releases/download/v1.4.6/rustdesk.exe",
        )
        .is_err());
        assert!(parse_rustdesk_release_download_url(
            "https://github.com:443/rustdesk/rustdesk/releases/download/v1.4.6/rustdesk.exe",
        )
        .is_err());
    }

    #[test]
    fn update_format_from_file_name_accepts_update_artifacts_only() {
        assert_eq!(update_format_from_file_name("rustdesk.exe").unwrap(), "exe");
        assert_eq!(update_format_from_file_name("rustdesk.msi").unwrap(), "msi");
        assert_eq!(update_format_from_file_name("rustdesk.dmg").unwrap(), "dmg");
        assert_eq!(update_format_from_file_name("RustDesk.EXE").unwrap(), "exe");
        assert_eq!(update_format_from_file_name("RustDesk.MSI").unwrap(), "msi");
        assert_eq!(update_format_from_file_name("RustDesk.DMG").unwrap(), "dmg");
        assert!(update_format_from_file_name("rustdesk.zip").is_err());
    }

    #[test]
    fn ensure_verified_update_artifact_removes_temp_file_on_sha256_mismatch() {
        let test_dir = std::env::temp_dir().join(format!(
            "rustdesk-updater-artifact-sha256-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();
        let file_path = test_dir.join("rustdesk-update.exe");

        let mut data: &[u8] = b"update";
        let result = write_verified_update_artifact(
            &file_path,
            &mut data,
            6,
            "0000000000000000000000000000000000000000000000000000000000000000",
        );

        assert!(result.is_err());
        assert!(!file_path.exists());
        assert!(std::fs::read_dir(&test_dir).unwrap().next().is_none());
        std::fs::remove_dir_all(&test_dir).unwrap();
    }

    #[test]
    fn copy_and_verify_update_artifact_hashes_open_file_handle() {
        let test_dir = std::env::temp_dir().join(format!(
            "rustdesk-updater-open-handle-sha256-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();
        let final_path = test_dir.join("rustdesk-update.exe");
        let (mut file, _) = create_download_temp_file(&final_path).unwrap();
        let display_path = test_dir.join("path-must-not-be-opened.download");
        let mut data: &[u8] = b"rustdesk";

        let result = copy_and_verify_update_artifact(
            &mut file,
            &display_path,
            &mut data,
            8,
            "304ca1638c5effa6832e0e15b958a8f74847efe4df9c3f3187216e921c168fed",
        );
        let position = result
            .as_ref()
            .ok()
            .map(|_| std::io::Seek::stream_position(&mut file).unwrap());
        drop(file);
        std::fs::remove_dir_all(&test_dir).unwrap();

        assert!(result.is_ok(), "{:?}", result.err());
        assert_eq!(position, Some(0));
    }

    #[test]
    fn verify_file_sha256_rejects_mismatched_file() {
        let file_path = std::env::temp_dir().join(format!(
            "rustdesk-updater-sha256-test-{}",
            std::process::id()
        ));
        std::fs::write(&file_path, b"rustdesk").unwrap();

        let result = verify_file_sha256(
            &file_path,
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
        std::fs::remove_file(&file_path).unwrap();

        assert!(result.is_err());
    }

    #[test]
    fn verified_update_artifact_cache_is_release_scoped_and_rejects_mismatches() {
        let artifact = verified_artifact();
        VERIFIED_UPDATE_ARTIFACTS.lock().unwrap().clear();
        cache_windows_exe(&artifact);

        let result = verified_update_artifact_for_download_url_with_query(
            &artifact.url,
            UpdateArtifactQuery {
                platform: "windows",
                arch: "x86_64",
                format: "exe",
                file_name: Some("rustdesk.msi"),
            },
        );

        assert!(result.is_err());

        let result = verified_update_artifact_for_download_url_with_query(
            &artifact.url,
            UpdateArtifactQuery {
                platform: "windows",
                arch: "aarch64",
                format: "exe",
                file_name: Some(&artifact.file_name),
            },
        );

        assert!(result.is_err());

        let mut same_release_artifact = artifact.clone();
        same_release_artifact.url = same_release_artifact
            .url
            .replace("rustdesk.exe", "other.exe");
        cache_windows_exe(&same_release_artifact);
        assert_eq!(VERIFIED_UPDATE_ARTIFACTS.lock().unwrap().len(), 2);

        let mut next_release_artifact = artifact.clone();
        next_release_artifact.release_id = "v1.4.7".to_owned();
        next_release_artifact.url = next_release_artifact.url.replace("v1.4.6", "v1.4.7");
        cache_windows_exe(&next_release_artifact);
        let cache = VERIFIED_UPDATE_ARTIFACTS.lock().unwrap();
        assert_eq!(cache.len(), 1);
        assert!(cache.contains_key(&next_release_artifact.url));
    }

    #[test]
    fn remove_cached_update_artifact_rejects_directory() {
        let test_dir = std::env::temp_dir().join(format!(
            "rustdesk-updater-cache-dir-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();
        let cache_path = test_dir.join("rustdesk-update.exe");
        std::fs::create_dir(&cache_path).unwrap();
        std::fs::write(cache_path.join("stale"), b"stale").unwrap();

        let result = remove_cached_update_artifact(&cache_path);

        assert!(result.is_err());
        assert!(cache_path.exists());
        std::fs::remove_dir_all(&test_dir).unwrap();
    }

    #[test]
    fn write_verified_download_removes_temp_file_on_install_error() {
        let test_dir = std::env::temp_dir().join(format!(
            "rustdesk-updater-install-error-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();
        let final_path = test_dir.join("rustdesk-update.exe");
        std::fs::create_dir(&final_path).unwrap();

        let mut data: &[u8] = b"update";
        let result = write_verified_update_artifact(
            &final_path,
            &mut data,
            6,
            "2937013f2181810606b2a799b05bda2849f3e369a20982a4138f0e0a55984ce4",
        );

        assert!(result.is_err());
        assert!(final_path.is_dir());
        assert_eq!(std::fs::read_dir(&test_dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&test_dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn verified_download_replaces_symlink_without_touching_target() {
        let test_dir = std::env::temp_dir().join(format!(
            "rustdesk-updater-symlink-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();
        let final_path = test_dir.join("rustdesk-update.exe");
        let victim_path = test_dir.join("victim");
        std::fs::write(&victim_path, b"victim").unwrap();
        std::os::unix::fs::symlink(&victim_path, &final_path).unwrap();
        let mut data: &[u8] = b"update";

        write_verified_update_artifact(
            &final_path,
            &mut data,
            6,
            "2937013f2181810606b2a799b05bda2849f3e369a20982a4138f0e0a55984ce4",
        )
        .unwrap();

        assert_eq!(std::fs::read(&victim_path).unwrap(), b"victim");
        assert_eq!(std::fs::read(&final_path).unwrap(), b"update");
        assert!(!std::fs::symlink_metadata(&final_path)
            .unwrap()
            .file_type()
            .is_symlink());
        std::fs::remove_dir_all(&test_dir).unwrap();
    }
}
