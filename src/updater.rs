use crate::{
    common::{
        display_version_from_release_id, release_download_base_url, release_id_from_update_url,
        release_metadata_url, release_signature_url, url_has_explicit_port,
    },
    hbbs_http::create_http_client_with_url_strict,
};
use hbb_common::log;
use hbb_common::{
    bail, config,
    update_metadata::{UpdateArtifactQuery, UpdateMetadataPolicy, VerifiedUpdateArtifact},
    ResultType,
};
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{channel, Receiver, Sender},
        Mutex,
    },
    time::{Duration, Instant},
};

enum UpdateMsg {
    CheckUpdate,
    Exit,
}

lazy_static::lazy_static! {
    static ref TX_MSG : Mutex<Sender<UpdateMsg>> = Mutex::new(start_auto_update_check());
    static ref VERIFIED_UPDATE_ARTIFACTS: Mutex<HashMap<String, VerifiedUpdateArtifact>> =
        Mutex::new(HashMap::new());
}

static CONTROLLING_SESSION_COUNT: AtomicUsize = AtomicUsize::new(0);

const DUR_ONE_DAY: Duration = Duration::from_secs(60 * 60 * 24);
const UPDATE_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const UPDATE_SIDECAR_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const UPDATE_METADATA_SIDECAR_MAX_BYTES: u64 = 1024 * 1024;
const UPDATE_TRACE_PREFIX: &str = "========================";

fn update_trace(message: impl AsRef<str>) {
    log::info!("{} {}", UPDATE_TRACE_PREFIX, message.as_ref());
}

fn update_trace_warn(message: impl AsRef<str>) {
    log::warn!("{} {}", UPDATE_TRACE_PREFIX, message.as_ref());
}

fn update_trace_error(message: impl AsRef<str>) {
    log::error!("{} {}", UPDATE_TRACE_PREFIX, message.as_ref());
}

fn describe_update_query(query: &UpdateArtifactQuery<'_>) -> String {
    format!(
        "platform={}, arch={}, format={}, file_name={}",
        query.platform,
        query.arch,
        query.format,
        query.file_name.unwrap_or("<none>")
    )
}

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
fn has_no_active_conns() -> bool {
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
    std::thread::sleep(Duration::from_secs(30));
    if let Err(e) = check_update(false) {
        log::error!("Error checking for updates: {}", e);
    }

    const MIN_INTERVAL: Duration = Duration::from_secs(60 * 10);
    const RETRY_INTERVAL: Duration = Duration::from_secs(60 * 30);
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
    #[cfg(target_os = "windows")]
    let update_msi = crate::platform::is_msi_installed()? && !crate::is_custom_client();
    #[cfg(not(target_os = "windows"))]
    let update_msi = false;
    let allow_auto_update = config::Config::get_bool_option(config::keys::OPTION_ALLOW_AUTO_UPDATE);
    update_trace(format!(
        "check_update start: manually={}, allow_auto_update={}, update_msi={}",
        manually, allow_auto_update, update_msi
    ));
    if !(manually || allow_auto_update) {
        update_trace(
            "check_update skipped because auto update is disabled and request is not manual",
        );
        return Ok(());
    }
    crate::common::set_fixed_test_software_update_url()?;

    let update_url = current_software_update_url()?;
    update_trace(format!("resolved software update url: {}", update_url));
    if update_url.is_empty() {
        update_trace("no update available after discovery");
        return Ok(());
    }
    let update_format = current_update_format(update_msi);
    if update_format == "unknown" {
        update_trace("automatic update is not supported on this platform");
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    if !manually {
        update_trace("background auto-install is not supported on macOS");
        return Ok(());
    }
    let query = UpdateArtifactQuery {
        platform: current_update_platform(),
        arch: current_update_arch(),
        format: update_format,
        file_name: None,
    };
    install_available_update(&update_url, update_msi, query)
}

fn install_available_update(
    update_url: &str,
    update_msi: bool,
    query: UpdateArtifactQuery<'_>,
) -> ResultType<()> {
    #[cfg(not(target_os = "windows"))]
    let _ = update_msi;
    update_trace(format!(
        "selecting verified update artifact with query: {}",
        describe_update_query(&query)
    ));
    let artifact = verified_update_artifact_from_release_page_url(update_url, &query)?;
    let download_url = artifact.url.as_str();
    update_trace(format!(
        "verified update artifact selected: version={}, release_id={}, file_name={}, size={}, sha256={}, url={}",
        artifact.version, artifact.release_id, artifact.file_name, artifact.size, artifact.sha256, artifact.url
    ));
    #[cfg(target_os = "windows")]
    log::debug!("New version available: {}", artifact.version);
    let file_path = verified_update_download_path(download_url)?;
    ensure_verified_update_artifact(download_url, &file_path, artifact.size, &artifact.sha256)?;
    update_trace(format!(
        "verified update artifact is ready on disk: {}",
        file_path.display()
    ));
    if has_no_active_conns() {
        update_trace("no active connections, proceeding to launch updater");
        #[cfg(target_os = "windows")]
        update_new_version(update_msi, &artifact.version, &file_path, &artifact.sha256);
    } else {
        update_trace_warn("skipping updater launch because active connections still exist");
    }
    Ok(())
}

fn verified_update_download_path(download_url: &str) -> ResultType<PathBuf> {
    let Some(file_path) = get_download_file_from_url(download_url) else {
        update_trace_error(format!(
            "failed to derive local download path from url: {}",
            download_url
        ));
        bail!("Failed to get the file path from the URL: {}", download_url);
    };
    update_trace(format!(
        "resolved local download path: {}",
        file_path.display()
    ));
    Ok(file_path)
}

fn current_software_update_url() -> ResultType<String> {
    Ok(crate::common::SOFTWARE_UPDATE_URL.lock().unwrap().clone())
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
    #[cfg(all(target_os = "windows", feature = "flutter"))]
    {
        "x86_64"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "aarch64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "x86_64"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::consts::ARCH
    }
}

pub(crate) fn current_update_format(update_msi: bool) -> &'static str {
    #[cfg(not(target_os = "windows"))]
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
        if update_msi {
            "msi"
        } else {
            "exe"
        }
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
    cache_verified_update_artifact(&artifact);
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
    update_trace(format!(
        "verifying requested download url: {}, query={}",
        download_url,
        describe_update_query(&query)
    ));
    if let Some(artifact) = get_cached_verified_update_artifact(download_url, &query) {
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
    update_trace(format!(
        "derived release page url from download url: {}",
        release_page_url
    ));
    let artifact = verified_update_artifact_from_release_page_url(&release_page_url, &query)?;
    if artifact.url != download_url {
        update_trace_error(format!(
            "verified artifact url mismatch: requested={}, verified={}",
            download_url, artifact.url
        ));
        bail!("update artifact URL does not match requested download URL");
    }
    update_trace(format!(
        "download url verification succeeded for file: {}",
        artifact.file_name
    ));
    cache_verified_update_artifact(&artifact);
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
    update_trace(format!(
        "fetching update metadata sidecars: metadata_url={}, signature_url={}",
        metadata_url, signature_url
    ));
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
    update_trace(format!(
        "verifying update metadata bytes: display_version={}, release_id={}, query={}, metadata_bytes={}, signature_bytes={}, expected_artifact_url_prefix={}",
        display_version,
        release_id,
        describe_update_query(query),
        metadata_bytes.len(),
        signature_bytes.len(),
        expected_artifact_url_prefix
    ));
    let policy = UpdateMetadataPolicy {
        app: "rustdesk",
        allowed_package_ids: &["rustdesk"],
        expected_version: Some(display_version),
        expected_release_id: Some(release_id),
        expected_artifact_url_prefix: Some(expected_artifact_url_prefix),
    };
    let result = hbb_common::update_metadata::verify_update_metadata(
        metadata_bytes,
        signature_bytes,
        &policy,
        query,
    );
    match &result {
        Ok(artifact) => update_trace(format!(
            "update metadata verification succeeded: file_name={}, url={}, size={}, sha256={}",
            artifact.file_name, artifact.url, artifact.size, artifact.sha256
        )),
        Err(err) => update_trace_error(format!(
            "update metadata verification failed for release_id={}, query={}: {}",
            release_id,
            describe_update_query(query),
            err
        )),
    }
    result
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
    release_download_url_from_segments(&segments, download_url)
}

fn release_download_url_from_segments(
    segments: &[&str],
    download_url: &str,
) -> ResultType<ReleaseDownloadUrl> {
    let (owner, repo, release_id, file_name) = match segments {
        [owner @ "rustdesk", repo @ "rustdesk", "releases", "download", release_id, file_name]
        | [owner @ "fufesou", repo @ "rustdesk", "releases", "download", release_id, file_name] => {
            (*owner, *repo, *release_id, *file_name)
        }
        _ => bail!(
            "Update download URL is not a RustDesk release download URL: {}",
            download_url
        ),
    };
    if release_id.is_empty() || file_name.is_empty() || owner.is_empty() || repo.is_empty() {
        bail!("Update download URL has empty owner, repo, release id or file name");
    }
    if owner == "fufesou" && release_id != crate::common::FIXED_TEST_UPDATE_RELEASE_ID {
        bail!(
            "Update download URL is not the fixed test release: {}",
            download_url
        );
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

fn cache_verified_update_artifact(artifact: &VerifiedUpdateArtifact) {
    VERIFIED_UPDATE_ARTIFACTS
        .lock()
        .unwrap()
        .insert(artifact.url.clone(), artifact.clone());
}

fn get_cached_verified_update_artifact(
    download_url: &str,
    query: &UpdateArtifactQuery<'_>,
) -> Option<VerifiedUpdateArtifact> {
    let artifact = VERIFIED_UPDATE_ARTIFACTS
        .lock()
        .unwrap()
        .get(download_url)
        .cloned()?;
    let _ = query;
    Some(artifact)
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
    update_trace(format!("fetch_update_sidecar_bytes start: {}", url));
    let client = create_http_client_with_url_strict(url);
    let mut response = client
        .get(url)
        .timeout(UPDATE_SIDECAR_HTTP_REQUEST_TIMEOUT)
        .send()?;
    if !response.status().is_success() {
        update_trace_error(format!(
            "fetch_update_sidecar_bytes failed: url={}, status={}",
            url,
            response.status()
        ));
        bail!(
            "Failed to download update metadata sidecar: {}",
            response.status()
        );
    }
    let bytes = read_limited_response_bytes(
        &mut response,
        UPDATE_METADATA_SIDECAR_MAX_BYTES,
        "Update metadata sidecar",
    )?;
    update_trace(format!(
        "fetch_update_sidecar_bytes success: url={}, bytes={}",
        url,
        bytes.len()
    ));
    Ok(bytes)
}

pub fn verify_existing_update_artifact(
    file_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> ResultType<()> {
    let metadata = std::fs::symlink_metadata(file_path)?;
    if !metadata.file_type().is_file() {
        bail!(
            "Refusing to verify update artifact that is not a regular file: {}",
            file_path.display()
        );
    }
    let file_size = metadata.len();
    if file_size != expected_size {
        bail!(
            "Update artifact size mismatch for {}: expected {}, got {}",
            file_path.display(),
            expected_size,
            file_size
        );
    }
    verify_file_sha256(file_path, expected_sha256)
}

fn ensure_verified_update_artifact(
    download_url: &str,
    file_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> ResultType<()> {
    update_trace(format!(
        "ensure_verified_update_artifact start: download_url={}, file_path={}, expected_size={}, expected_sha256={}",
        download_url,
        file_path.display(),
        expected_size,
        expected_sha256
    ));
    if !verify_cached_update_artifact(file_path, expected_size, expected_sha256)? {
        download_update_artifact(download_url, file_path, expected_size, expected_sha256)?;
    }
    update_trace(format!(
        "ensure_verified_update_artifact success: file_path={}",
        file_path.display()
    ));
    Ok(())
}

fn verify_cached_update_artifact(
    file_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> ResultType<bool> {
    let Some(file_size) = cached_update_artifact_size(file_path)? else {
        return Ok(false);
    };
    update_trace(format!(
        "cached update artifact found: path={}, actual_size={}",
        file_path.display(),
        file_size
    ));
    if file_size != expected_size {
        remove_cached_update_artifact_with_size_mismatch(file_path, expected_size, file_size)?;
        return Ok(false);
    }
    match verify_file_sha256(file_path, expected_sha256) {
        Ok(()) => {
            update_trace(format!(
                "cached update artifact sha256 verified, reusing file: {}",
                file_path.display()
            ));
            Ok(true)
        }
        Err(e) => {
            update_trace_warn(format!(
                "removing cached update artifact due to sha256 mismatch: path={}, err={}",
                file_path.display(),
                e
            ));
            log::warn!("Removing cached update file with invalid SHA256: {}", e);
            remove_cached_update_artifact(file_path)?;
            Ok(false)
        }
    }
}

fn remove_cached_update_artifact_with_size_mismatch(
    file_path: &Path,
    expected_size: u64,
    file_size: u64,
) -> ResultType<()> {
    update_trace_warn(format!(
        "removing cached update artifact due to size mismatch: path={}, expected_size={}, actual_size={}",
        file_path.display(),
        expected_size,
        file_size
    ));
    log::warn!(
        "Removing cached update file with size mismatch for {}: expected {}, got {}",
        file_path.display(),
        expected_size,
        file_size
    );
    remove_cached_update_artifact(file_path)
}

fn download_update_artifact(
    download_url: &str,
    file_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> ResultType<()> {
    update_trace(format!(
        "downloading update artifact: download_url={}",
        download_url
    ));
    let client = create_http_client_with_url_strict(download_url);
    let response = client
        .get(download_url)
        .timeout(UPDATE_HTTP_REQUEST_TIMEOUT)
        .send()?;
    if !response.status().is_success() {
        update_trace_error(format!(
            "update artifact download failed: download_url={}, status={}",
            download_url,
            response.status()
        ));
        bail!(
            "Failed to download the new version file: {}",
            response.status()
        );
    }
    update_trace(format!(
        "update artifact download response ok: download_url={}, status={}",
        download_url,
        response.status()
    ));
    let mut limited_response = response.take(expected_size.saturating_add(1));
    write_verified_update_artifact(
        file_path,
        &mut limited_response,
        expected_size,
        expected_sha256,
    )
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

#[cfg(target_os = "windows")]
fn verified_update_path(
    p: &str,
    expected_sha256: &str,
    kind: &str,
    file_path: &Path,
) -> Option<(crate::platform::VerifiedUpdateFile, String)> {
    update_trace(format!(
        "verified_update_path start: source_path={}, kind={}, expected_sha256={}",
        p, kind, expected_sha256
    ));
    let update_file = match crate::platform::copy_and_verify_update_file_sha256(p, expected_sha256)
    {
        Ok(update_file) => update_file,
        Err(e) => {
            update_trace_error(format!(
                "verified_update_path failed verification: source_path={}, kind={}, err={}",
                p, kind, e
            ));
            log::error!("Refusing to update from invalid {}: {}", kind, e);
            std::fs::remove_file(file_path).ok();
            return None;
        }
    };
    let update_path = match update_file.path_str() {
        Ok(path) => path.to_owned(),
        Err(e) => {
            update_trace_error(format!(
                "verified_update_path failed to read verified path: source_path={}, kind={}, err={}",
                p, kind, e
            ));
            log::error!("Failed to get verified {} path: {}", kind, e);
            std::fs::remove_file(file_path).ok();
            return None;
        }
    };
    update_trace(format!(
        "verified_update_path success: kind={}, source_path={}, verified_path={}",
        kind, p, update_path
    ));
    Some((update_file, update_path))
}

#[cfg(target_os = "windows")]
fn update_new_version(update_msi: bool, version: &str, file_path: &PathBuf, expected_sha256: &str) {
    update_trace(format!(
        "update_new_version start: update_msi={}, version={}, file_path={}, expected_sha256={}",
        update_msi,
        version,
        file_path.display(),
        expected_sha256
    ));
    log::debug!(
        "New version is downloaded, update begin, update msi: {update_msi}, version: {version}, file: {:?}",
        file_path.to_str()
    );
    if let Some(p) = file_path.to_str() {
        if let Some(session_id) = crate::platform::get_current_process_session_id() {
            update_new_version_in_session(
                update_msi,
                version,
                file_path,
                expected_sha256,
                p,
                session_id,
            );
        } else {
            update_trace_error(format!(
                "update_new_version failed to resolve current process session id for source_path={}",
                file_path.display()
            ));
            log::error!(
                "Failed to get the current process session id, Error {}",
                std::io::Error::last_os_error()
            );
            std::fs::remove_file(file_path).ok();
        }
    } else {
        update_trace_error(format!(
            "update_new_version failed to convert source path to string: {}",
            file_path.display()
        ));
        log::error!(
            "Failed to convert the file path to string: {}",
            file_path.display()
        );
    }
}

#[cfg(target_os = "windows")]
fn update_new_version_in_session(
    update_msi: bool,
    version: &str,
    file_path: &PathBuf,
    expected_sha256: &str,
    source_path: &str,
    session_id: u32,
) {
    update_trace(format!(
        "update_new_version resolved session id: session_id={}, update_msi={}, source_path={}",
        session_id, update_msi, source_path
    ));
    if update_msi {
        update_new_msi_version(version, file_path, expected_sha256, source_path);
    } else {
        update_new_exe_version(version, file_path, expected_sha256, source_path, session_id);
    }
}

#[cfg(target_os = "windows")]
fn update_new_msi_version(
    version: &str,
    file_path: &PathBuf,
    expected_sha256: &str,
    source_path: &str,
) {
    update_trace(format!(
        "update_new_version verifying msi before launch: source_path={}",
        source_path
    ));
    let Some((_update_file, update_path)) =
        verified_update_path(source_path, expected_sha256, "msi", file_path)
    else {
        return;
    };
    update_trace(format!(
        "update_new_version launching msi installer: verified_path={}",
        update_path
    ));
    match crate::platform::update_me_msi(&update_path, true) {
        Ok(_) => {
            update_trace(format!(
                "update_new_version msi launch succeeded: version={}, verified_path={}",
                version, update_path
            ));
            log::debug!("New version \"{}\" updated.", version);
        }
        Err(e) => {
            update_trace_error(format!(
                "update_new_version msi launch failed: version={}, verified_path={}, err={}",
                version, update_path, e
            ));
            log::error!(
                "Failed to install the new msi version  \"{}\": {}",
                version,
                e
            );
            std::fs::remove_file(file_path).ok();
        }
    }
}

#[cfg(target_os = "windows")]
fn update_new_exe_version(
    version: &str,
    file_path: &PathBuf,
    expected_sha256: &str,
    source_path: &str,
    session_id: u32,
) {
    let Some((_update_file, update_path)) =
        verified_update_path(source_path, expected_sha256, "exe", file_path)
    else {
        return;
    };
    let custom_client_staging_dir = prepare_custom_client_staging_dir(file_path);
    let Some(custom_client_staging_dir) = custom_client_staging_dir else {
        return;
    };
    let launch_cmd = format!(
        "\"{}\" --update {}",
        update_path,
        crate::platform::UPDATE_SINGLE_EXE_ARG
    );
    update_trace(format!(
        "update_new_version launching privileged updater: session_id={}, command={}",
        session_id, launch_cmd
    ));
    if !launch_verified_exe_update(version, &update_path, session_id, &launch_cmd) {
        cleanup_failed_exe_update(file_path, custom_client_staging_dir);
    }
}

#[cfg(target_os = "windows")]
fn prepare_custom_client_staging_dir(file_path: &PathBuf) -> Option<Option<PathBuf>> {
    if crate::is_custom_client() {
        let custom_client_staging_dir = crate::platform::get_custom_client_staging_dir();
        update_trace(format!(
            "update_new_version preparing custom client staging dir: {}",
            custom_client_staging_dir.display()
        ));
        if let Err(e) = crate::platform::handle_custom_client_staging_dir_before_update(
            &custom_client_staging_dir,
        ) {
            update_trace_error(format!(
                "update_new_version failed to prepare custom client staging dir: dir={}, err={}",
                custom_client_staging_dir.display(),
                e
            ));
            log::error!(
                "Failed to handle custom client staging dir before update: {}",
                e
            );
            std::fs::remove_file(file_path).ok();
            return None;
        }
        Some(Some(custom_client_staging_dir))
    } else {
        cleanup_residual_custom_client_staging_dir();
        Some(None)
    }
}

#[cfg(target_os = "windows")]
fn cleanup_residual_custom_client_staging_dir() {
    let staging_dir = crate::platform::get_custom_client_staging_dir();
    update_trace(format!(
        "update_new_version cleaning residual custom client staging dir: {}",
        staging_dir.display()
    ));
    hbb_common::allow_err!(crate::platform::remove_custom_client_staging_dir(
        &staging_dir
    ));
}

#[cfg(target_os = "windows")]
fn launch_verified_exe_update(
    version: &str,
    update_path: &str,
    session_id: u32,
    launch_cmd: &str,
) -> bool {
    match crate::platform::launch_privileged_process(session_id, launch_cmd) {
        Ok(h) => {
            if h.is_null() {
                update_trace_error(format!(
                    "update_new_version launch returned null handle: version={}, verified_path={}",
                    version, update_path
                ));
                log::error!("Failed to update to the new version: {}", version);
                false
            } else {
                update_trace(format!(
                    "update_new_version privileged updater launched: version={}, verified_path={}",
                    version, update_path
                ));
                log::debug!("New version \"{}\" is launched.", version);
                unsafe {
                    winapi::um::handleapi::CloseHandle(h);
                }
                true
            }
        }
        Err(e) => {
            update_trace_error(format!(
                "update_new_version privileged updater launch failed: version={}, verified_path={}, err={}",
                version, update_path, e
            ));
            log::error!("Failed to run the new version: {}", e);
            false
        }
    }
}

#[cfg(target_os = "windows")]
fn cleanup_failed_exe_update(file_path: &PathBuf, custom_client_staging_dir: Option<PathBuf>) {
    if let Some(dir) = custom_client_staging_dir {
        hbb_common::allow_err!(crate::platform::remove_custom_client_staging_dir(&dir));
    }
    update_trace_warn(format!(
        "update_new_version removing source update file after failed launch: {}",
        file_path.display()
    ));
    std::fs::remove_file(file_path).ok();
}

pub fn get_download_file_from_url(url: &str) -> Option<PathBuf> {
    let filename = url.split('/').last()?;
    Some(std::env::temp_dir().join(filename))
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
    for _ in 0..16 {
        let temp_path = download_dir.join(format!(
            ".{}.{}.{}.download",
            file_name,
            std::process::id(),
            hbb_common::rand::random::<u64>()
        ));
        match std::fs::OpenOptions::new()
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

fn update_download_backup_path(final_path: &Path) -> ResultType<PathBuf> {
    let Some(file_name) = final_path.file_name() else {
        bail!("Update file has no file name: {}", final_path.display());
    };
    let file_name = file_name.to_string_lossy();
    Ok(final_path.with_file_name(format!(
        ".{}.{}.{}.backup",
        file_name,
        std::process::id(),
        hbb_common::rand::random::<u64>()
    )))
}

fn move_existing_update_to_backup(final_path: &Path) -> ResultType<Option<PathBuf>> {
    let metadata = match std::fs::symlink_metadata(final_path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let file_type = metadata.file_type();
    if !file_type.is_file() && !file_type.is_symlink() {
        bail!(
            "Refusing to replace update cache path that is not a file: {}",
            final_path.display()
        );
    }
    let backup_path = update_download_backup_path(final_path)?;
    std::fs::rename(final_path, &backup_path)?;
    Ok(Some(backup_path))
}

pub(crate) fn install_verified_download(temp_path: &Path, final_path: &Path) -> ResultType<()> {
    let backup_path = move_existing_update_to_backup(final_path)?;
    if let Err(e) = std::fs::rename(temp_path, final_path) {
        if let Some(backup_path) = backup_path.as_ref() {
            if let Err(restore_err) = std::fs::rename(backup_path, final_path) {
                bail!(
                    "Failed to replace {} with {}, and failed to restore backup {}: replace error {}, restore error {}",
                    final_path.display(),
                    temp_path.display(),
                    backup_path.display(),
                    e,
                    restore_err
                );
            }
        }
        return Err(e.into());
    }
    if let Some(backup_path) = backup_path {
        if let Err(e) = std::fs::remove_file(&backup_path) {
            log::warn!(
                "Failed to remove update backup {}: {}",
                backup_path.display(),
                e
            );
        }
    }
    Ok(())
}

fn write_verified_update_artifact<R: Read>(
    final_path: &Path,
    reader: &mut R,
    expected_size: u64,
    expected_sha256: &str,
) -> ResultType<()> {
    update_trace(format!(
        "write_verified_update_artifact start: final_path={}, expected_size={}, expected_sha256={}",
        final_path.display(),
        expected_size,
        expected_sha256
    ));
    let (mut file, temp_path) = create_download_temp_file(final_path)?;
    if let Err(e) = copy_and_verify_update_artifact(
        &mut file,
        &temp_path,
        reader,
        expected_size,
        expected_sha256,
    ) {
        update_trace_error(format!(
            "write_verified_update_artifact verification failed: temp_path={}, err={}",
            temp_path.display(),
            e
        ));
        std::fs::remove_file(temp_path).ok();
        return Err(e);
    }
    drop(file);
    if let Err(e) = install_verified_download(&temp_path, final_path) {
        update_trace_error(format!(
            "write_verified_update_artifact install failed: temp_path={}, final_path={}, err={}",
            temp_path.display(),
            final_path.display(),
            e
        ));
        std::fs::remove_file(temp_path).ok();
        return Err(e);
    }
    update_trace(format!(
        "write_verified_update_artifact success: final_path={}",
        final_path.display()
    ));
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
    update_trace(format!(
        "copy_and_verify_update_artifact wrote bytes: temp_path={}, bytes_written={}, expected_size={}, expected_sha256={}",
        temp_path.display(),
        bytes_written,
        expected_size,
        expected_sha256
    ));
    if bytes_written != expected_size {
        update_trace_error(format!(
            "copy_and_verify_update_artifact size mismatch: temp_path={}, expected_size={}, actual_size={}",
            temp_path.display(),
            expected_size,
            bytes_written
        ));
        bail!(
            "Update artifact size mismatch for {}: expected {}, got {}",
            temp_path.display(),
            expected_size,
            bytes_written
        );
    }
    verify_file_sha256(temp_path, expected_sha256)
}

fn verify_file_sha256(path: &Path, expected_sha256: &str) -> ResultType<()> {
    let expected_sha256 = expected_sha256.trim().to_ascii_lowercase();
    if expected_sha256.len() != 64 || !expected_sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!(
            "Expected update file SHA256 is malformed for {}",
            path.display()
        );
    }

    let actual_sha256 = sha256_file_hex(path)?;
    if actual_sha256 != expected_sha256 {
        update_trace_error(format!(
            "SHA256 mismatch for {}: expected {}, got {}",
            path.display(),
            expected_sha256,
            actual_sha256
        ));
        bail!(
            "SHA256 mismatch for {}: expected {}, got {}",
            path.display(),
            expected_sha256,
            actual_sha256
        );
    }
    update_trace(format!(
        "SHA256 verified for {}: {}",
        path.display(),
        actual_sha256
    ));
    Ok(())
}

fn sha256_file_hex(path: &Path) -> ResultType<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = sha2::Sha256::default();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        sha2::Digest::update(&mut hasher, &buffer[..count]);
    }
    Ok(format!("{:x}", sha2::Digest::finalize(hasher)))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn verified_update_artifact_cache_reuses_verified_download_url() {
        let artifact = verified_artifact();
        VERIFIED_UPDATE_ARTIFACTS.lock().unwrap().clear();
        cache_verified_update_artifact(&artifact);

        let cached = get_cached_verified_update_artifact(
            &artifact.url,
            &UpdateArtifactQuery {
                platform: "windows",
                arch: "x86_64",
                format: "exe",
                file_name: Some("rustdesk.exe"),
            },
        )
        .unwrap();

        assert_eq!(cached.url, artifact.url);
        assert_eq!(cached.sha256, artifact.sha256);
    }

    #[cfg(all(target_os = "windows", not(feature = "flutter")))]
    #[test]
    fn current_update_format_honors_msi_on_non_flutter_windows() {
        assert_eq!(current_update_format(true), "msi");
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
    fn parse_rustdesk_release_download_url_accepts_fixed_test_release_path() {
        let parsed = parse_rustdesk_release_download_url(
            "https://github.com/fufesou/rustdesk/releases/download/fix-update-metadata/rustdesk.exe",
        )
        .unwrap();

        assert_eq!(parsed.release_id, "fix-update-metadata");
        assert_eq!(parsed.file_name, "rustdesk.exe");
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
    fn verify_file_sha256_accepts_uppercase_and_whitespace() {
        let file_path = std::env::temp_dir().join(format!(
            "rustdesk-updater-sha256-normalized-test-{}",
            std::process::id()
        ));
        std::fs::write(&file_path, b"rustdesk").unwrap();

        let result = verify_file_sha256(
            &file_path,
            "  304CA1638C5EFFA6832E0E15B958A8F74847EFE4DF9C3F3187216E921C168FED\n",
        );
        std::fs::remove_file(&file_path).unwrap();

        assert!(result.is_ok());
    }

    #[test]
    fn verify_file_sha256_rejects_malformed_expected_hash() {
        let file_path = std::env::temp_dir().join(format!(
            "rustdesk-updater-sha256-malformed-test-{}",
            std::process::id()
        ));
        std::fs::write(&file_path, b"rustdesk").unwrap();

        let result = verify_file_sha256(&file_path, "not-a-sha256");
        std::fs::remove_file(&file_path).unwrap();

        assert!(result.is_err());
    }

    #[test]
    fn verify_existing_update_artifact_keeps_caller_owned_file_on_failure() {
        let file_path = std::env::temp_dir().join(format!(
            "rustdesk-updater-existing-artifact-test-{}",
            std::process::id()
        ));
        std::fs::write(&file_path, b"rustdesk").unwrap();

        let result = verify_existing_update_artifact(
            &file_path,
            8,
            "0000000000000000000000000000000000000000000000000000000000000000",
        );

        assert!(result.is_err());
        assert!(file_path.exists());
        std::fs::remove_file(&file_path).unwrap();
    }

    #[test]
    fn verified_update_artifact_cache_rejects_mismatched_file_name_query() {
        let artifact = verified_artifact();
        VERIFIED_UPDATE_ARTIFACTS.lock().unwrap().clear();
        cache_verified_update_artifact(&artifact);

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
    }

    #[cfg(unix)]
    #[test]
    fn verify_existing_update_artifact_rejects_symlink() {
        let test_dir = std::env::temp_dir().join(format!(
            "rustdesk-updater-existing-artifact-symlink-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();
        let target_path = test_dir.join("target");
        let link_path = test_dir.join("rustdesk-update.exe");
        std::fs::write(&target_path, b"rustdesk").unwrap();
        std::os::unix::fs::symlink(&target_path, &link_path).unwrap();

        let result = verify_existing_update_artifact(
            &link_path,
            8,
            "fcda7b18b4640138305e3acc7e7d2a023f120a7b1530ecd2982fadc38d208e2f",
        );

        assert!(result.is_err());
        assert_eq!(std::fs::read(&target_path).unwrap(), b"rustdesk");
        std::fs::remove_dir_all(&test_dir).unwrap();
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
    fn install_verified_download_replaces_symlink_without_touching_target() {
        let test_dir = std::env::temp_dir().join(format!(
            "rustdesk-updater-symlink-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();
        let final_path = test_dir.join("rustdesk-update.exe");
        let temp_path = test_dir.join(".rustdesk-update.exe.tmp");
        let victim_path = test_dir.join("victim");
        std::fs::write(&victim_path, b"victim").unwrap();
        std::os::unix::fs::symlink(&victim_path, &final_path).unwrap();
        std::fs::write(&temp_path, b"update").unwrap();

        install_verified_download(&temp_path, &final_path).unwrap();

        assert_eq!(std::fs::read(&victim_path).unwrap(), b"victim");
        assert_eq!(std::fs::read(&final_path).unwrap(), b"update");
        assert!(!std::fs::symlink_metadata(&final_path)
            .unwrap()
            .file_type()
            .is_symlink());
        std::fs::remove_dir_all(&test_dir).unwrap();
    }

    #[test]
    fn install_verified_download_keeps_existing_file_when_replace_fails() {
        let test_dir = std::env::temp_dir().join(format!(
            "rustdesk-updater-replace-failure-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();
        let final_path = test_dir.join("rustdesk-update.exe");
        let missing_temp_path = test_dir.join(".missing-rustdesk-update.exe.tmp");
        std::fs::write(&final_path, b"old-verified-update").unwrap();

        let result = install_verified_download(&missing_temp_path, &final_path);

        assert!(result.is_err());
        assert_eq!(std::fs::read(&final_path).unwrap(), b"old-verified-update");
        std::fs::remove_dir_all(&test_dir).unwrap();
    }
}
