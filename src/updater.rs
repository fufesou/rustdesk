use crate::{
    common::{
        display_version_from_release_id, release_download_base_url, release_id_from_update_url,
        release_metadata_url, release_signature_url,
    },
    hbbs_http::create_http_client_with_url,
};
use hbb_common::log;
use hbb_common::{
    anyhow::anyhow,
    bail, config,
    update_metadata::{UpdateArtifactQuery, UpdateMetadataPolicy, VerifiedUpdateArtifact},
    ResultType,
};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{channel, Receiver, Sender},
        Mutex,
    },
    time::{Duration, Instant},
};

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

enum UpdateMsg {
    CheckUpdate,
    Exit,
}

lazy_static::lazy_static! {
    static ref TX_MSG : Mutex<Sender<UpdateMsg>> = Mutex::new(start_auto_update_check());
}

static CONTROLLING_SESSION_COUNT: AtomicUsize = AtomicUsize::new(0);

const DUR_ONE_DAY: Duration = Duration::from_secs(60 * 60 * 24);
const UPDATE_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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
        update_trace("check_update skipped because auto update is disabled and request is not manual");
        return Ok(());
    }

    let update_url = current_software_update_url()?;
    update_trace(format!("resolved software update url: {}", update_url));
    if update_url.is_empty() {
        update_trace("no update available after discovery");
    } else {
        let query = UpdateArtifactQuery {
            platform: current_update_platform(),
            arch: current_update_arch(),
            format: current_update_format(update_msi),
            file_name: None,
        };
        update_trace(format!(
            "selecting verified update artifact with query: {}",
            describe_update_query(&query)
        ));
        let artifact = download_verified_update_artifact(&update_url, query)?;
        let download_url = artifact.url.as_str();
        let version = artifact.version.as_str();
        update_trace(format!(
            "verified update artifact selected: version={}, release_id={}, file_name={}, size={}, sha256={}, url={}",
            artifact.version, artifact.release_id, artifact.file_name, artifact.size, artifact.sha256, artifact.url
        ));
        #[cfg(target_os = "windows")]
        log::debug!("New version available: {}", &version);
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
        ensure_verified_update_artifact(download_url, &file_path, artifact.size, &artifact.sha256)?;
        update_trace(format!(
            "verified update artifact is ready on disk: {}",
            file_path.display()
        ));
        // We have checked if the `conns` is empty before, but we need to check again.
        // No need to care about the downloaded file here, because it's rare case that the `conns` are empty
        // before the download, but not empty after the download.
        if has_no_active_conns() {
            update_trace("no active connections, proceeding to launch updater");
            #[cfg(target_os = "windows")]
            update_new_version(update_msi, version, &file_path, &artifact.sha256);
        } else {
            update_trace_warn("skipping updater launch because active connections still exist");
        }
    }
    Ok(())
}

fn current_software_update_url() -> ResultType<String> {
    crate::common::SOFTWARE_UPDATE_URL
        .lock()
        .map(|url| url.clone())
        .map_err(|_| anyhow!("software update URL lock poisoned"))
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
    #[cfg(not(all(target_os = "windows", feature = "flutter")))]
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

fn download_verified_update_artifact(
    update_url: &str,
    query: UpdateArtifactQuery<'_>,
) -> ResultType<VerifiedUpdateArtifact> {
    update_trace(format!(
        "download_verified_update_artifact: update_url={}, query={}",
        update_url,
        describe_update_query(&query)
    ));
    verified_update_artifact_from_release_page_url(update_url, &query)
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
    let download_query = UpdateArtifactQuery {
        platform: query.platform,
        arch: query.arch,
        format: query.format,
        file_name: Some(artifact.file_name.as_str()),
    };
    verified_update_artifact_for_download_url_with_query(&artifact.url, download_query)
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
    let result =
        hbb_common::update_metadata::verify_update_metadata(metadata_bytes, signature_bytes, &policy, query);
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
    if release_id.is_empty() || file_name.is_empty() || owner.is_empty() || repo.is_empty() {
        bail!("Update download URL has empty owner, repo, release id or file name");
    }
    if owner == "fufesou" && release_id != "fix-update-metadata" {
        bail!("Update download URL is not the fixed test release: {}", download_url);
    }
    Ok(ReleaseDownloadUrl {
        owner: owner.to_owned(),
        repo: repo.to_owned(),
        release_id: release_id.to_owned(),
        file_name: file_name.to_owned(),
    })
}

fn update_format_from_file_name(file_name: &str) -> ResultType<&'static str> {
    if file_name.ends_with(".msi") {
        return Ok("msi");
    }
    if file_name.ends_with(".exe") {
        return Ok("exe");
    }
    if file_name.ends_with(".dmg") {
        return Ok("dmg");
    }
    bail!("Unsupported update artifact file format: {}", file_name);
}

fn validate_verified_update_artifact(
    artifact: &VerifiedUpdateArtifact,
    expected_version: &str,
    expected_release_id: &str,
    expected_url_prefix: &str,
) -> ResultType<()> {
    if artifact.version != expected_version {
        bail!("update metadata version mismatch");
    }
    if artifact.release_id != expected_release_id {
        bail!("update metadata release id mismatch");
    }
    if !artifact.url.starts_with(expected_url_prefix) {
        bail!("update artifact URL is outside expected release prefix");
    }
    if artifact.package_id != "rustdesk" {
        bail!("update metadata package id is not allowed");
    }
    Ok(())
}

fn fetch_update_sidecar_bytes(url: &str) -> ResultType<Vec<u8>> {
    update_trace(format!("fetch_update_sidecar_bytes start: {}", url));
    let client = create_http_client_with_url(url, true);
    let mut response = client
        .get(url)
        .timeout(UPDATE_HTTP_REQUEST_TIMEOUT)
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
    let mut bytes = Vec::new();
    response.read_to_end(&mut bytes)?;
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
    update_trace(format!(
        "verify_existing_update_artifact start: path={}, expected_size={}, expected_sha256={}",
        file_path.display(),
        expected_size,
        expected_sha256
    ));
    let file_size = std::fs::metadata(file_path)?.len();
    update_trace(format!(
        "verify_existing_update_artifact observed file size: path={}, actual_size={}",
        file_path.display(),
        file_size
    ));
    if file_size != expected_size {
        update_trace_warn(format!(
            "verify_existing_update_artifact removing file due to size mismatch: path={}, expected_size={}, actual_size={}",
            file_path.display(),
            expected_size,
            file_size
        ));
        std::fs::remove_file(file_path)?;
        bail!(
            "Update artifact size mismatch for {}: expected {}, got {}",
            file_path.display(),
            expected_size,
            file_size
        );
    }
    if let Err(e) = verify_file_sha256(file_path, expected_sha256) {
        update_trace_warn(format!(
            "verify_existing_update_artifact removing file due to sha256 mismatch: path={}, expected_sha256={}, err={}",
            file_path.display(),
            expected_sha256,
            e
        ));
        std::fs::remove_file(file_path)?;
        return Err(e);
    }
    update_trace(format!(
        "verify_existing_update_artifact success: path={}",
        file_path.display()
    ));
    Ok(())
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
    let client = create_http_client_with_url(download_url, true);
    let mut is_file_exists = false;
    if file_path.exists() {
        let file_size = std::fs::metadata(file_path)?.len();
        update_trace(format!(
            "cached update artifact found: path={}, actual_size={}",
            file_path.display(),
            file_size
        ));
        if file_size == expected_size {
            update_trace(format!(
                "cached update artifact size matches, verifying sha256: path={}, expected_sha256={}",
                file_path.display(),
                expected_sha256
            ));
            match verify_file_sha256(file_path, expected_sha256) {
                Ok(()) => {
                    update_trace(format!(
                        "cached update artifact sha256 verified, reusing file: {}",
                        file_path.display()
                    ));
                    is_file_exists = true;
                }
                Err(e) => {
                    update_trace_warn(format!(
                        "removing cached update artifact due to sha256 mismatch: path={}, err={}",
                        file_path.display(),
                        e
                    ));
                    log::warn!("Removing cached update file with invalid SHA256: {}", e);
                    std::fs::remove_file(file_path)?;
                }
            }
        } else {
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
            std::fs::remove_file(file_path)?;
        }
    }
    if !is_file_exists {
        update_trace(format!(
            "downloading update artifact: download_url={}",
            download_url
        ));
        let mut response = client.get(download_url).send()?;
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
        write_verified_update_artifact(file_path, &mut response, expected_size, expected_sha256)?;
    }
    update_trace(format!(
        "ensure_verified_update_artifact success: file_path={}",
        file_path.display()
    ));
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
    let update_file =
        match crate::platform::verify_update_file_signature_and_sha256(p, expected_sha256) {
            Ok(update_file) => update_file,
            Err(e) => {
                update_trace_error(format!(
                    "verified_update_path failed verification: source_path={}, kind={}, err={}",
                    p, kind, e
                ));
                log::error!("Refusing to update from untrusted {}: {}", kind, e);
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
            update_trace(format!(
                "update_new_version resolved session id: session_id={}, update_msi={}, source_path={}",
                session_id, update_msi, p
            ));
            if update_msi {
                update_trace(format!(
                    "update_new_version verifying msi before launch: source_path={}",
                    p
                ));
                let Some((_update_file, update_path)) =
                    verified_update_path(p, expected_sha256, "msi", file_path)
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
                        std::fs::remove_file(&file_path).ok();
                    }
                }
            } else {
                let Some((_update_file, update_path)) =
                    verified_update_path(p, expected_sha256, "exe", file_path)
                else {
                    return;
                };
                let custom_client_staging_dir = if crate::is_custom_client() {
                    let custom_client_staging_dir =
                        crate::platform::get_custom_client_staging_dir();
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
                        std::fs::remove_file(&file_path).ok();
                        return;
                    }
                    Some(custom_client_staging_dir)
                } else {
                    // Clean up any residual staging directory from previous custom client
                    let staging_dir = crate::platform::get_custom_client_staging_dir();
                    update_trace(format!(
                        "update_new_version cleaning residual custom client staging dir: {}",
                        staging_dir.display()
                    ));
                    hbb_common::allow_err!(crate::platform::remove_custom_client_staging_dir(
                        &staging_dir
                    ));
                    None
                };
                let launch_cmd = format!("\"{}\" --update", update_path);
                update_trace(format!(
                    "update_new_version launching privileged updater: session_id={}, command={}",
                    session_id, launch_cmd
                ));
                let update_launched = match crate::platform::launch_privileged_process(
                    session_id,
                    &launch_cmd,
                ) {
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
                };
                if !update_launched {
                    if let Some(dir) = custom_client_staging_dir {
                        hbb_common::allow_err!(crate::platform::remove_custom_client_staging_dir(
                            &dir
                        ));
                    }
                    update_trace_warn(format!(
                        "update_new_version removing source update file after failed launch: {}",
                        file_path.display()
                    ));
                    std::fs::remove_file(&file_path).ok();
                }
            }
        } else {
            update_trace_error(format!(
                "update_new_version failed to resolve current process session id for source_path={}",
                file_path.display()
            ));
            log::error!(
                "Failed to get the current process session id, Error {}",
                std::io::Error::last_os_error()
            );
            std::fs::remove_file(&file_path).ok();
        }
    } else {
        // unreachable!()
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
            Ok(file) => {
                update_trace(format!(
                    "created temporary update download file: final_path={}, temp_path={}",
                    final_path.display(),
                    temp_path.display()
                ));
                return Ok((file, temp_path));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    bail!("Failed to create temporary update file");
}

fn install_verified_download(temp_path: &Path, final_path: &Path) -> ResultType<()> {
    update_trace(format!(
        "install_verified_download start: temp_path={}, final_path={}",
        temp_path.display(),
        final_path.display()
    ));
    if std::fs::symlink_metadata(final_path).is_ok() {
        update_trace_warn(format!(
            "install_verified_download removing existing destination before replace: {}",
            final_path.display()
        ));
        std::fs::remove_file(final_path)?;
    }
    if let Err(e) = std::fs::rename(temp_path, final_path) {
        update_trace_error(format!(
            "install_verified_download rename failed: temp_path={}, final_path={}, err={}",
            temp_path.display(),
            final_path.display(),
            e
        ));
        std::fs::remove_file(temp_path).ok();
        return Err(e.into());
    }
    update_trace(format!(
        "install_verified_download success: final_path={}",
        final_path.display()
    ));
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
    let actual_sha256 = sha256_file_hex(path)?;
    if actual_sha256 != expected_sha256 {
        bail!(
            "SHA256 mismatch for {}: expected {}, got {}",
            path.display(),
            expected_sha256,
            actual_sha256
        );
    }
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
    fn parse_rustdesk_release_download_url_accepts_expected_path() {
        let parsed = parse_rustdesk_release_download_url(
            "https://github.com/rustdesk/rustdesk/releases/download/v1.4.6/rustdesk.exe",
        )
        .unwrap();

        assert_eq!(parsed.release_id, "v1.4.6");
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
    }

    #[test]
    fn update_format_from_file_name_accepts_update_artifacts_only() {
        assert_eq!(update_format_from_file_name("rustdesk.exe").unwrap(), "exe");
        assert_eq!(update_format_from_file_name("rustdesk.msi").unwrap(), "msi");
        assert_eq!(update_format_from_file_name("rustdesk.dmg").unwrap(), "dmg");
        assert!(update_format_from_file_name("rustdesk.zip").is_err());
    }

    #[test]
    fn verified_update_metadata_rejects_mismatched_expected_version_release_and_prefix() {
        let mut artifact = verified_artifact();
        artifact.version = "1.4.7".to_owned();
        let version_err = validate_verified_update_artifact(
            &artifact,
            "1.4.6",
            "v1.4.6",
            "https://github.com/rustdesk/rustdesk/releases/download/v1.4.6/",
        )
        .unwrap_err()
        .to_string();
        assert!(version_err.contains("version"));

        let mut artifact = verified_artifact();
        artifact.version = "1.4.6".to_owned();
        artifact.release_id = "v1.4.7".to_owned();
        let release_err = validate_verified_update_artifact(
            &artifact,
            "1.4.6",
            "v1.4.6",
            "https://github.com/rustdesk/rustdesk/releases/download/v1.4.6/",
        )
        .unwrap_err()
        .to_string();
        assert!(release_err.contains("release"));

        let mut artifact = verified_artifact();
        artifact.version = "1.4.6".to_owned();
        artifact.release_id = "v1.4.6".to_owned();
        artifact.url =
            "https://github.com/rustdesk/rustdesk/releases/download/v1.4.5/rustdesk.exe".to_owned();
        let prefix_err = validate_verified_update_artifact(
            &artifact,
            "1.4.6",
            "v1.4.6",
            "https://github.com/rustdesk/rustdesk/releases/download/v1.4.6/",
        )
        .unwrap_err()
        .to_string();
        assert!(prefix_err.contains("release prefix"));
    }

    #[test]
    fn current_update_query_maps_platform_arch_and_format() {
        #[cfg(all(target_os = "windows", feature = "flutter"))]
        {
            assert_eq!(current_update_platform(), "windows");
            assert_eq!(current_update_arch(), "x86_64");
            assert_eq!(current_update_format(false), "exe");
            assert_eq!(current_update_format(true), "msi");
        }

        #[cfg(all(target_os = "windows", not(feature = "flutter")))]
        {
            assert_eq!(current_update_platform(), "windows");
            assert_eq!(current_update_arch(), "x86");
            assert_eq!(current_update_format(false), "exe");
            assert_eq!(current_update_format(true), "exe");
        }

        #[cfg(target_os = "macos")]
        {
            assert_eq!(current_update_platform(), "macos");
            assert_eq!(current_update_format(false), "dmg");
            #[cfg(target_arch = "aarch64")]
            assert_eq!(current_update_arch(), "aarch64");
            #[cfg(target_arch = "x86_64")]
            assert_eq!(current_update_arch(), "x86_64");
        }
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
    fn update_http_request_timeout_is_bounded() {
        assert_eq!(UPDATE_HTTP_REQUEST_TIMEOUT, Duration::from_secs(30));
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
    fn create_download_temp_file_uses_random_sibling_path() {
        let test_dir = std::env::temp_dir().join(format!(
            "rustdesk-updater-temp-file-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();
        let final_path = test_dir.join("rustdesk-update.exe");

        let (file, temp_path) = create_download_temp_file(&final_path).unwrap();

        drop(file);
        let temp_file_name = temp_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap();
        assert_ne!(temp_path, final_path);
        assert_eq!(temp_path.parent(), Some(test_dir.as_path()));
        assert!(temp_file_name.starts_with(".rustdesk-update.exe."));
        assert!(temp_file_name.ends_with(".download"));
        assert!(temp_path.exists());
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
}
