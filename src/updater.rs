use crate::{
    common::{
        display_version_from_release_id, do_check_software_update, release_download_base_url,
        release_id_from_update_url, release_metadata_url, release_signature_url,
    },
    hbbs_http::create_http_client_with_url,
};
use hbb_common::{
    anyhow::anyhow,
    bail, config,
    update_metadata::{UpdateArtifactQuery, UpdateMetadataPolicy, VerifiedUpdateArtifact},
    ResultType,
};
#[cfg(test)]
use hbb_common::update_metadata::TrustedUpdateKey;
use hbb_common::log;
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
#[cfg(test)]
use std::collections::HashMap;

enum UpdateMsg {
    CheckUpdate,
    Exit,
}

lazy_static::lazy_static! {
    static ref TX_MSG : Mutex<Sender<UpdateMsg>> = Mutex::new(start_auto_update_check());
}

#[cfg(test)]
lazy_static::lazy_static! {
    static ref DOWNLOAD_FILE_SHA256_CACHE: Mutex<HashMap<String, String>> = Default::default();
    static ref TEST_VERIFIED_UPDATE_ARTIFACT: Mutex<Option<VerifiedUpdateArtifact>> = Default::default();
    static ref TEST_UPDATE_URL: Mutex<Option<String>> = Default::default();
    static ref TEST_DOWNLOADS: Mutex<HashMap<String, Vec<u8>>> = Default::default();
    static ref TEST_UPDATE_LAUNCH: Mutex<Option<TestUpdateLaunch>> = Default::default();
    static ref TEST_UPDATE_SIDECARS: Mutex<HashMap<String, Vec<u8>>> = Default::default();
    static ref TEST_UPDATE_SIDECAR_REQUESTS: Mutex<Vec<String>> = Default::default();
    static ref TEST_TRUSTED_UPDATE_PUBLIC_KEY: Mutex<Option<[u8; 32]>> = Default::default();
}

static CONTROLLING_SESSION_COUNT: AtomicUsize = AtomicUsize::new(0);

const DUR_ONE_DAY: Duration = Duration::from_secs(60 * 60 * 24);
const UPDATE_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
const TEST_TRUSTED_UPDATE_KEY_ID: &str = "test-ed25519-main";

#[cfg(test)]
#[derive(Clone)]
struct TestUpdateLaunch {
    version: String,
    expected_sha256: String,
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
    #[cfg(all(target_os = "windows", not(test)))]
    let update_msi = crate::platform::is_msi_installed()? && !crate::is_custom_client();
    #[cfg(any(not(target_os = "windows"), test))]
    let update_msi = false;
    if !(manually || config::Config::get_bool_option(config::keys::OPTION_ALLOW_AUTO_UPDATE)) {
        return Ok(());
    }
    perform_update_discovery()?;

    let update_url = current_software_update_url()?;
    if update_url.is_empty() {
        log::debug!("No update available.");
    } else {
        let query = UpdateArtifactQuery {
            platform: current_update_platform(),
            arch: current_update_arch(),
            format: current_update_format(update_msi),
            file_name: None,
        };
        let artifact = download_verified_update_artifact(&update_url, query)?;
        let download_url = artifact.url.as_str();
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

fn perform_update_discovery() -> ResultType<()> {
    #[cfg(test)]
    {
        if let Some(update_url) = test_update_url()? {
            let mut stored_url = crate::common::SOFTWARE_UPDATE_URL
                .lock()
                .map_err(|_| anyhow!("software update URL lock poisoned"))?;
            *stored_url = update_url;
            return Ok(());
        }
    }
    do_check_software_update()
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
    let download = parse_rustdesk_release_download_url(download_url)?;
    let release_page_url = format!(
        "https://github.com/rustdesk/rustdesk/releases/tag/{}",
        download.release_id
    );
    let artifact = verified_update_artifact_from_release_page_url(&release_page_url, &query)?;
    if artifact.url != download_url {
        bail!("update artifact URL does not match requested download URL");
    }
    Ok(artifact)
}

fn verified_update_artifact_from_release_page_url(
    update_url: &str,
    query: &UpdateArtifactQuery<'_>,
) -> ResultType<VerifiedUpdateArtifact> {
    let release_id = release_id_from_update_url(update_url)?;
    let display_version = display_version_from_release_id(&release_id)?;
    let expected_artifact_url_prefix = release_download_base_url(update_url)?;
    #[cfg(test)]
    {
        if let Some(artifact) = test_verified_update_artifact()? {
            validate_verified_update_artifact(
                &artifact,
                display_version.as_str(),
                release_id.as_str(),
                expected_artifact_url_prefix.as_str(),
            )?;
            return Ok(artifact);
        }
    }

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
    #[cfg(test)]
    {
        if let Some(public_key) = test_trusted_update_public_key()? {
            let trusted_key = TrustedUpdateKey {
                key_id: TEST_TRUSTED_UPDATE_KEY_ID,
                algorithm: "ed25519",
                public_key,
            };
            return hbb_common::update_metadata::verify_update_metadata_with_keys(
                metadata_bytes,
                signature_bytes,
                &policy,
                query,
                &[trusted_key],
            );
        }
    }
    hbb_common::update_metadata::verify_update_metadata(metadata_bytes, signature_bytes, &policy, query)
}

struct ReleaseDownloadUrl {
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
    let (release_id, file_name) = match segments.as_slice() {
        ["rustdesk", "rustdesk", "releases", "download", release_id, file_name] => {
            (*release_id, *file_name)
        }
        _ => bail!(
            "Update download URL is not a RustDesk release download URL: {}",
            download_url
        ),
    };
    if release_id.is_empty() || file_name.is_empty() {
        bail!("Update download URL has empty release id or file name");
    }
    Ok(ReleaseDownloadUrl {
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
    #[cfg(test)]
    if let Some(bytes) = test_update_sidecar_bytes(url)? {
        return Ok(bytes);
    }
    let client = create_http_client_with_url(url, true);
    let mut response = client
        .get(url)
        .timeout(UPDATE_HTTP_REQUEST_TIMEOUT)
        .send()?;
    if !response.status().is_success() {
        bail!("Failed to download update metadata sidecar: {}", response.status());
    }
    let mut bytes = Vec::new();
    response.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub fn verify_existing_update_artifact(
    file_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> ResultType<()> {
    let file_size = std::fs::metadata(file_path)?.len();
    if file_size != expected_size {
        std::fs::remove_file(file_path)?;
        bail!(
            "Update artifact size mismatch for {}: expected {}, got {}",
            file_path.display(),
            expected_size,
            file_size
        );
    }
    if let Err(e) = verify_file_sha256(file_path, expected_sha256) {
        std::fs::remove_file(file_path)?;
        return Err(e);
    }
    Ok(())
}

fn ensure_verified_update_file(
    download_url: &str,
    file_path: &Path,
    expected_sha256: &str,
) -> ResultType<()> {
    let client = create_http_client_with_url(download_url, true);
    let mut is_file_exists = false;
    if file_path.exists() {
        // Check if the file size is the same as the server file size
        // If the file size is the same, we don't need to download it again.
        let file_size = std::fs::metadata(file_path)?.len();
        let response = client
            .head(download_url)
            .timeout(UPDATE_HTTP_REQUEST_TIMEOUT)
            .send()?;
        if !response.status().is_success() {
            bail!("Failed to get the file size: {}", response.status());
        }
        let total_size = response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|ct_len| ct_len.to_str().ok())
            .and_then(|ct_len| ct_len.parse::<u64>().ok());
        let Some(total_size) = total_size else {
            bail!("Failed to get content length");
        };
        if file_size == total_size {
            match verify_file_sha256(file_path, expected_sha256) {
                Ok(()) => is_file_exists = true,
                Err(e) => {
                    log::warn!("Removing cached update file with invalid SHA256: {}", e);
                    std::fs::remove_file(file_path)?;
                }
            }
        } else {
            std::fs::remove_file(file_path)?;
        }
    }
    if !is_file_exists {
        let mut response = client.get(download_url).send()?;
        if !response.status().is_success() {
            bail!(
                "Failed to download the new version file: {}",
                response.status()
            );
        }
        write_verified_download_from_reader(file_path, &mut response, expected_sha256)?;
    }
    Ok(())
}

fn ensure_verified_update_artifact(
    download_url: &str,
    file_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> ResultType<()> {
    let client = create_http_client_with_url(download_url, true);
    let mut is_file_exists = false;
    if file_path.exists() {
        let file_size = std::fs::metadata(file_path)?.len();
        if file_size == expected_size {
            match verify_file_sha256(file_path, expected_sha256) {
                Ok(()) => is_file_exists = true,
                Err(e) => {
                    log::warn!("Removing cached update file with invalid SHA256: {}", e);
                    std::fs::remove_file(file_path)?;
                }
            }
        } else {
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
        #[cfg(test)]
        if let Some(data) = test_download_bytes(download_url)? {
            return write_verified_update_artifact(
                file_path,
                &mut data.as_slice(),
                expected_size,
                expected_sha256,
            );
        }
        let mut response = client.get(download_url).send()?;
        if !response.status().is_success() {
            bail!(
                "Failed to download the new version file: {}",
                response.status()
            );
        }
        write_verified_update_artifact(file_path, &mut response, expected_size, expected_sha256)?;
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
    let update_file =
        match crate::platform::verify_update_file_signature_and_sha256(p, expected_sha256) {
            Ok(update_file) => update_file,
            Err(e) => {
                log::error!("Refusing to update from untrusted {}: {}", kind, e);
                std::fs::remove_file(file_path).ok();
                return None;
            }
        };
    let update_path = match update_file.path_str() {
        Ok(path) => path.to_owned(),
        Err(e) => {
            log::error!("Failed to get verified {} path: {}", kind, e);
            std::fs::remove_file(file_path).ok();
            return None;
        }
    };
    Some((update_file, update_path))
}

#[cfg(all(target_os = "windows", test))]
fn update_new_version(_update_msi: bool, version: &str, _file_path: &PathBuf, expected_sha256: &str) {
    record_test_update_launch(version, expected_sha256);
}

#[cfg(all(target_os = "windows", not(test)))]
fn update_new_version(update_msi: bool, version: &str, file_path: &PathBuf, expected_sha256: &str) {
    log::debug!(
        "New version is downloaded, update begin, update msi: {update_msi}, version: {version}, file: {:?}",
        file_path.to_str()
    );
    if let Some(p) = file_path.to_str() {
        if let Some(session_id) = crate::platform::get_current_process_session_id() {
            if update_msi {
                let Some((_update_file, update_path)) =
                    verified_update_path(p, expected_sha256, "msi", file_path)
                else {
                    return;
                };
                match crate::platform::update_me_msi(&update_path, true) {
                    Ok(_) => {
                        log::debug!("New version \"{}\" updated.", version);
                    }
                    Err(e) => {
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
                    if let Err(e) = crate::platform::handle_custom_client_staging_dir_before_update(
                        &custom_client_staging_dir,
                    ) {
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
                    std::fs::remove_file(&file_path).ok();
                }
            }
        } else {
            log::error!(
                "Failed to get the current process session id, Error {}",
                std::io::Error::last_os_error()
            );
            std::fs::remove_file(&file_path).ok();
        }
    } else {
        // unreachable!()
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
            Ok(file) => return Ok((file, temp_path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    bail!("Failed to create temporary update file");
}

fn install_verified_download(temp_path: &Path, final_path: &Path) -> ResultType<()> {
    if std::fs::symlink_metadata(final_path).is_ok() {
        std::fs::remove_file(final_path)?;
    }
    if let Err(e) = std::fs::rename(temp_path, final_path) {
        std::fs::remove_file(temp_path).ok();
        return Err(e.into());
    }
    Ok(())
}

fn copy_and_verify_download_file<R: Read>(
    file: &mut std::fs::File,
    temp_path: &Path,
    reader: &mut R,
    expected_sha256: &str,
) -> ResultType<()> {
    std::io::copy(reader, file)?;
    file.flush()?;
    verify_file_sha256(temp_path, expected_sha256)
}

fn write_verified_download_from_reader<R: Read>(
    final_path: &Path,
    reader: &mut R,
    expected_sha256: &str,
) -> ResultType<()> {
    let (mut file, temp_path) = create_download_temp_file(final_path)?;
    if let Err(e) = copy_and_verify_download_file(&mut file, &temp_path, reader, expected_sha256) {
        std::fs::remove_file(temp_path).ok();
        return Err(e);
    }
    drop(file);
    if let Err(e) = install_verified_download(&temp_path, final_path) {
        std::fs::remove_file(temp_path).ok();
        return Err(e);
    }
    Ok(())
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
        std::fs::remove_file(temp_path).ok();
        return Err(e);
    }
    drop(file);
    if let Err(e) = install_verified_download(&temp_path, final_path) {
        std::fs::remove_file(temp_path).ok();
        return Err(e);
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
    verify_file_sha256(temp_path, expected_sha256)
}

#[cfg(test)]
fn write_verified_download(
    final_path: &Path,
    file_data: &[u8],
    expected_sha256: &str,
) -> ResultType<()> {
    let mut reader = file_data;
    write_verified_download_from_reader(final_path, &mut reader, expected_sha256)
}

#[cfg(test)]
fn set_test_verified_update_artifact(artifact: VerifiedUpdateArtifact) {
    if let Ok(mut stored_artifact) = TEST_VERIFIED_UPDATE_ARTIFACT.lock() {
        *stored_artifact = Some(artifact);
    }
}

#[cfg(test)]
fn test_verified_update_artifact() -> ResultType<Option<VerifiedUpdateArtifact>> {
    TEST_VERIFIED_UPDATE_ARTIFACT
        .lock()
        .map(|mut artifact| artifact.take())
        .map_err(|_| anyhow!("test verified update artifact lock poisoned"))
}

#[cfg(test)]
fn clear_test_verified_update_artifact() {
    if let Ok(mut stored_artifact) = TEST_VERIFIED_UPDATE_ARTIFACT.lock() {
        *stored_artifact = None;
    }
}

#[cfg(test)]
fn set_test_check_update_inputs(update_url: &str, download_url: &str, data: &[u8]) {
    if let Ok(mut stored_url) = TEST_UPDATE_URL.lock() {
        *stored_url = Some(update_url.to_owned());
    }
    if let Ok(mut downloads) = TEST_DOWNLOADS.lock() {
        downloads.insert(download_url.to_owned(), data.to_vec());
    }
    if let Ok(mut launch) = TEST_UPDATE_LAUNCH.lock() {
        *launch = None;
    }
}

#[cfg(test)]
fn test_update_url() -> ResultType<Option<String>> {
    TEST_UPDATE_URL
        .lock()
        .map(|url| url.clone())
        .map_err(|_| anyhow!("test update URL lock poisoned"))
}

#[cfg(test)]
fn test_download_bytes(download_url: &str) -> ResultType<Option<Vec<u8>>> {
    TEST_DOWNLOADS
        .lock()
        .map(|downloads| downloads.get(download_url).cloned())
        .map_err(|_| anyhow!("test downloads lock poisoned"))
}

#[cfg(test)]
fn clear_test_check_update_inputs() {
    if let Ok(mut stored_url) = TEST_UPDATE_URL.lock() {
        *stored_url = None;
    }
    if let Ok(mut downloads) = TEST_DOWNLOADS.lock() {
        downloads.clear();
    }
    if let Ok(mut launch) = TEST_UPDATE_LAUNCH.lock() {
        *launch = None;
    }
}

#[cfg(test)]
fn set_test_update_sidecar(url: &str, bytes: Vec<u8>) {
    if let Ok(mut sidecars) = TEST_UPDATE_SIDECARS.lock() {
        sidecars.insert(url.to_owned(), bytes);
    }
}

#[cfg(test)]
fn test_update_sidecar_bytes(url: &str) -> ResultType<Option<Vec<u8>>> {
    let mut requests = TEST_UPDATE_SIDECAR_REQUESTS
        .lock()
        .map_err(|_| anyhow!("test update sidecar request lock poisoned"))?;
    requests.push(url.to_owned());
    drop(requests);
    TEST_UPDATE_SIDECARS
        .lock()
        .map(|sidecars| sidecars.get(url).cloned())
        .map_err(|_| anyhow!("test update sidecar lock poisoned"))
}

#[cfg(test)]
fn take_test_update_sidecar_requests() -> Vec<String> {
    TEST_UPDATE_SIDECAR_REQUESTS
        .lock()
        .map(|mut requests| std::mem::take(&mut *requests))
        .unwrap_or_default()
}

#[cfg(test)]
fn set_test_trusted_update_public_key(public_key: [u8; 32]) {
    if let Ok(mut trusted_key) = TEST_TRUSTED_UPDATE_PUBLIC_KEY.lock() {
        *trusted_key = Some(public_key);
    }
}

#[cfg(test)]
fn test_trusted_update_public_key() -> ResultType<Option<[u8; 32]>> {
    TEST_TRUSTED_UPDATE_PUBLIC_KEY
        .lock()
        .map(|public_key| *public_key)
        .map_err(|_| anyhow!("test trusted update public key lock poisoned"))
}

#[cfg(test)]
fn clear_test_update_sidecars() {
    if let Ok(mut sidecars) = TEST_UPDATE_SIDECARS.lock() {
        sidecars.clear();
    }
    if let Ok(mut requests) = TEST_UPDATE_SIDECAR_REQUESTS.lock() {
        requests.clear();
    }
    if let Ok(mut trusted_key) = TEST_TRUSTED_UPDATE_PUBLIC_KEY.lock() {
        *trusted_key = None;
    }
}

#[cfg(test)]
fn record_test_update_launch(version: &str, expected_sha256: &str) {
    if let Ok(mut launch) = TEST_UPDATE_LAUNCH.lock() {
        *launch = Some(TestUpdateLaunch {
            version: version.to_owned(),
            expected_sha256: expected_sha256.to_owned(),
        });
    }
}

#[cfg(test)]
fn take_test_update_launch() -> Option<TestUpdateLaunch> {
    TEST_UPDATE_LAUNCH.lock().ok().and_then(|mut launch| launch.take())
}

#[cfg(test)]
#[derive(serde::Deserialize)]
struct GithubRelease {
    assets: Vec<GithubReleaseAsset>,
}

#[cfg(test)]
#[derive(serde::Deserialize)]
struct GithubReleaseAsset {
    name: String,
    digest: Option<String>,
}

#[cfg(test)]
fn fetch_github_asset_sha256(
    release_or_download_url: &str,
    download_url: &str,
) -> ResultType<String> {
    let api_url = github_release_api_url(release_or_download_url)?;
    let asset_name = download_asset_name(download_url)?;
    let metadata = fetch_github_release_metadata(&api_url)?;
    github_release_asset_sha256(&metadata, &asset_name)
}

#[cfg(test)]
fn fetch_github_release_metadata(api_url: &str) -> ResultType<String> {
    let client = create_http_client_with_url(&api_url, true);
    let response = client
        .get(api_url)
        .header(reqwest::header::USER_AGENT, "rustdesk-updater")
        .timeout(UPDATE_HTTP_REQUEST_TIMEOUT)
        .send()?;
    if !response.status().is_success() {
        let status = response.status();
        if status == reqwest::StatusCode::FORBIDDEN
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        {
            bail!(
                "Failed to get GitHub release metadata: {}. GitHub API rate limit may have been reached. Please retry later or download from the release page.",
                status
            );
        }
        bail!("Failed to get GitHub release metadata: {}", status);
    }
    Ok(response.text()?)
}

#[cfg(test)]
fn normalize_sha256_hex(sha256: &str) -> ResultType<String> {
    let sha256 = sha256.trim().to_ascii_lowercase();
    if sha256.len() != 64 || !sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("Update file SHA256 is malformed");
    }
    Ok(sha256)
}

#[cfg(test)]
fn cache_download_file_expected_sha256(
    download_url: &str,
    expected_sha256: &str,
) -> ResultType<String> {
    let expected_sha256 = normalize_sha256_hex(expected_sha256)?;
    DOWNLOAD_FILE_SHA256_CACHE
        .lock()
        .unwrap()
        .insert(download_url.to_owned(), expected_sha256.clone());
    Ok(expected_sha256)
}

#[cfg(test)]
fn cached_download_file_expected_sha256(download_url: &str) -> Option<String> {
    DOWNLOAD_FILE_SHA256_CACHE
        .lock()
        .unwrap()
        .get(download_url)
        .cloned()
}

#[cfg(test)]
pub fn clear_download_file_expected_sha256(download_url: &str) {
    DOWNLOAD_FILE_SHA256_CACHE
        .lock()
        .unwrap()
        .remove(download_url);
}

#[cfg(test)]
pub fn refresh_download_file_expected_sha256(download_url: &str) -> ResultType<String> {
    let expected_sha256 = fetch_github_asset_sha256(download_url, download_url)?;
    cache_download_file_expected_sha256(download_url, &expected_sha256)
}

#[cfg(test)]
pub fn download_file_expected_sha256(download_url: &str) -> ResultType<String> {
    match refresh_download_file_expected_sha256(download_url) {
        Ok(expected_sha256) => Ok(expected_sha256),
        Err(e) => {
            if let Some(expected_sha256) = cached_download_file_expected_sha256(download_url) {
                log::warn!(
                    "Failed to refresh update file SHA256 for {}, using cached value: {}",
                    download_url,
                    e
                );
                return Ok(expected_sha256);
            }
            Err(e)
        }
    }
}

#[cfg(test)]
fn github_release_api_url(update_url: &str) -> ResultType<String> {
    let url = reqwest::Url::parse(update_url)?;
    if url.scheme() != "https" || url.host_str() != Some("github.com") {
        bail!(
            "Update URL is not a GitHub HTTPS release URL: {}",
            update_url
        );
    }

    let Some(mut segments) = url.path_segments() else {
        bail!("GitHub update URL has no path: {}", update_url);
    };
    let Some(owner) = segments.next() else {
        bail!("GitHub update URL has no owner: {}", update_url);
    };
    let Some(repo) = segments.next() else {
        bail!("GitHub update URL has no repo: {}", update_url);
    };
    if owner != "rustdesk" || repo != "rustdesk" {
        bail!(
            "GitHub update URL is not a RustDesk release URL: {}",
            update_url
        );
    }
    if segments.next() != Some("releases") {
        bail!("GitHub update URL is not a release URL: {}", update_url);
    }

    let tag = match segments.next() {
        Some("tag") => segments.collect::<Vec<_>>().join("/"),
        Some("download") => {
            let Some(tag) = segments.next() else {
                bail!("GitHub update URL has no release tag: {}", update_url);
            };
            if segments.next().is_none() {
                bail!("GitHub update URL has no release asset: {}", update_url);
            }
            tag.to_owned()
        }
        _ => bail!(
            "GitHub update URL is not a release tag or download URL: {}",
            update_url
        ),
    };
    if tag.is_empty() {
        bail!("GitHub update URL has no release tag: {}", update_url);
    }

    Ok(format!(
        "https://api.github.com/repos/{owner}/{repo}/releases/tags/{tag}"
    ))
}

#[cfg(test)]
fn download_asset_name(download_url: &str) -> ResultType<String> {
    let Some(asset_name) = download_url.split('/').last() else {
        bail!("Download URL has no asset name: {}", download_url);
    };
    if asset_name.is_empty() {
        bail!("Download URL has empty asset name: {}", download_url);
    }
    Ok(asset_name.to_owned())
}

#[cfg(test)]
fn github_release_asset_sha256(release_json: &str, asset_name: &str) -> ResultType<String> {
    let release: GithubRelease = serde_json::from_str(release_json)?;
    let Some(asset) = release.assets.iter().find(|asset| asset.name == asset_name) else {
        bail!("GitHub release asset not found: {}", asset_name);
    };
    let Some(digest) = asset.digest.as_deref() else {
        bail!("GitHub release asset has no digest: {}", asset_name);
    };
    parse_sha256_digest(digest)
}

#[cfg(test)]
fn parse_sha256_digest(digest: &str) -> ResultType<String> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        bail!("GitHub release asset digest is not SHA256: {}", digest);
    };
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!(
            "GitHub release asset SHA256 digest is malformed: {}",
            digest
        );
    }
    Ok(hex.to_lowercase())
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
    use hbb_common::{
        base64::{engine::general_purpose::STANDARD, Engine as _},
        sodiumoxide::crypto::sign,
        update_metadata::UPDATE_METADATA_SIGNATURE_CONTEXT,
    };
    use serde_json::{json, Value};

    static CHECK_UPDATE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    const TEST_UPDATE_KEY_ID: &str = "test-ed25519-main";
    const TEST_SHA256: &str =
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    struct UpdateMetadataFixture {
        metadata: Vec<u8>,
        signature: Vec<u8>,
        public_key: [u8; 32],
    }

    fn stale_update_artifact() -> VerifiedUpdateArtifact {
        VerifiedUpdateArtifact {
            version: "9.9.9".to_owned(),
            release_id: "v9.9.9".to_owned(),
            package_id: "rustdesk".to_owned(),
            url: "https://github.com/rustdesk/rustdesk/releases/download/v9.9.9/rustdesk-file-name-does-not-contain-version.exe".to_owned(),
            file_name: "rustdesk-file-name-does-not-contain-version.exe".to_owned(),
            size: 6,
            sha256: "2937013f2181810606b2a799b05bda2849f3e369a20982a4138f0e0a55984ce4"
                .to_owned(),
        }
    }

    fn update_artifact(file_name: &str, platform: &str, arch: &str, format: &str) -> Value {
        json!({
            "platform": platform,
            "arch": arch,
            "format": format,
            "url": format!("https://github.com/rustdesk/rustdesk/releases/download/v1.4.6/{file_name}"),
            "file_name": file_name,
            "size": 123456,
            "sha256": TEST_SHA256,
        })
    }

    fn signed_update_metadata(metadata: Value) -> UpdateMetadataFixture {
        hbb_common::sodiumoxide::init().expect("test sodiumoxide init");
        let metadata = serde_json::to_vec(&metadata).expect("metadata JSON");
        let (public_key, secret_key) = sign::gen_keypair();
        let mut signed = UPDATE_METADATA_SIGNATURE_CONTEXT.to_vec();
        signed.extend_from_slice(&metadata);
        let signature = sign::sign_detached(&signed, &secret_key);
        let signature = json!({
            "schema_version": 1,
            "algorithm": "ed25519",
            "key_id": TEST_UPDATE_KEY_ID,
            "signature": STANDARD.encode(signature.to_bytes()),
        });
        UpdateMetadataFixture {
            metadata,
            signature: serde_json::to_vec(&signature).expect("signature JSON"),
            public_key: public_key.0,
        }
    }

    fn metadata_with_artifacts(artifacts: Vec<Value>) -> Value {
        json!({
            "schema_version": 1,
            "app": "rustdesk",
            "package_id": "rustdesk",
            "version": "1.4.6",
            "release_id": "v1.4.6",
            "published_at": "2026-05-14T00:00:00Z",
            "signature_key_id": TEST_UPDATE_KEY_ID,
            "artifacts": artifacts,
        })
    }

    fn set_metadata_field(metadata: &mut Value, key: &str, value: Value) {
        metadata
            .as_object_mut()
            .expect("metadata object")
            .insert(key.to_owned(), value);
    }

    fn set_artifact_field(metadata: &mut Value, key: &str, value: Value) {
        metadata["artifacts"][0]
            .as_object_mut()
            .expect("artifact object")
            .insert(key.to_owned(), value);
    }

    fn set_test_release_sidecars(release_id: &str, fixture: &UpdateMetadataFixture) {
        set_test_update_sidecar(
            &format!("https://github.com/rustdesk/rustdesk/releases/download/{release_id}/rustdesk-update.json"),
            fixture.metadata.clone(),
        );
        set_test_update_sidecar(
            &format!("https://github.com/rustdesk/rustdesk/releases/download/{release_id}/rustdesk-update.json.sig"),
            fixture.signature.clone(),
        );
        set_test_trusted_update_public_key(fixture.public_key);
    }

    fn current_test_artifact_file_name() -> String {
        match current_update_format(false) {
            "exe" => format!("rustdesk-1.4.6-{}.exe", current_update_arch()),
            "msi" => "rustdesk-1.4.6-x86_64.msi".to_owned(),
            "dmg" => format!("rustdesk-1.4.6-{}.dmg", current_update_arch()),
            format => format!("rustdesk-1.4.6.{}", format),
        }
    }

    fn current_test_artifact() -> Value {
        update_artifact(
            &current_test_artifact_file_name(),
            current_update_platform(),
            current_update_arch(),
            current_update_format(false),
        )
    }

    #[test]
    fn verified_download_url_derives_release_metadata_and_signature_urls() {
        let _guard = CHECK_UPDATE_TEST_LOCK.lock().unwrap();
        clear_test_update_sidecars();
        let file_name = current_test_artifact_file_name();
        let download_url =
            format!("https://github.com/rustdesk/rustdesk/releases/download/v1.4.6/{file_name}");
        let fixture = signed_update_metadata(metadata_with_artifacts(vec![current_test_artifact()]));
        set_test_release_sidecars("v1.4.6", &fixture);

        let artifact = verified_update_artifact_for_download_url(&download_url).unwrap();

        assert_eq!(artifact.url, download_url);
        assert_eq!(
            take_test_update_sidecar_requests(),
            vec![
                "https://github.com/rustdesk/rustdesk/releases/download/v1.4.6/rustdesk-update.json".to_owned(),
                "https://github.com/rustdesk/rustdesk/releases/download/v1.4.6/rustdesk-update.json.sig".to_owned(),
            ]
        );
        clear_test_update_sidecars();
    }

    #[test]
    fn verified_download_url_rejects_file_name_package_version_release_and_url_mismatches() {
        let _guard = CHECK_UPDATE_TEST_LOCK.lock().unwrap();
        let file_name = current_test_artifact_file_name();
        let download_url =
            format!("https://github.com/rustdesk/rustdesk/releases/download/v1.4.6/{file_name}");

        for label in ["file", "package", "version", "release", "artifact"] {
            clear_test_update_sidecars();
            let mut metadata = metadata_with_artifacts(vec![current_test_artifact()]);
            match label {
                "file" => set_artifact_field(&mut metadata, "file_name", json!("other.exe")),
                "package" => set_metadata_field(&mut metadata, "package_id", json!("custom")),
                "version" => set_metadata_field(&mut metadata, "version", json!("1.4.7")),
                "release" => set_metadata_field(&mut metadata, "release_id", json!("v1.4.7")),
                "artifact" => set_artifact_field(
                    &mut metadata,
                    "url",
                    json!(format!(
                        "https://github.com/rustdesk/rustdesk/releases/download/v1.4.7/{file_name}"
                    )),
                ),
                _ => unreachable!(),
            }
            let fixture = signed_update_metadata(metadata);
            set_test_release_sidecars("v1.4.6", &fixture);

            assert!(
                verified_update_artifact_for_download_url(&download_url).is_err(),
                "{label}"
            );
        }
        clear_test_update_sidecars();
    }

    #[test]
    fn verified_download_url_rejects_non_rustdesk_release_download_url() {
        assert!(verified_update_artifact_for_download_url(
            "https://example.com/rustdesk/rustdesk/releases/download/v1.4.6/rustdesk.exe"
        )
        .is_err());
        assert!(verified_update_artifact_for_download_url(
            "https://github.com/other/rustdesk/releases/download/v1.4.6/rustdesk.exe"
        )
        .is_err());
        assert!(verified_update_artifact_for_download_url(
            "https://github.com/rustdesk/rustdesk/releases/tag/v1.4.6"
        )
        .is_err());
        assert!(verified_update_artifact_for_download_url(
            "https://github.com/rustdesk/rustdesk/releases/download/v1.4.6/rustdesk.exe?x=1"
        )
        .is_err());
    }

    #[test]
    fn verified_release_page_url_selects_artifact_from_query() {
        let _guard = CHECK_UPDATE_TEST_LOCK.lock().unwrap();
        clear_test_update_sidecars();
        let exe_name = "rustdesk-1.4.6-x86_64.exe";
        let msi_name = "rustdesk-1.4.6-x86_64.msi";
        let fixture = signed_update_metadata(metadata_with_artifacts(vec![
            update_artifact(exe_name, "windows", "x86_64", "exe"),
            update_artifact(msi_name, "windows", "x86_64", "msi"),
        ]));
        set_test_release_sidecars("v1.4.6", &fixture);

        let msi = verified_update_artifact_for_release_page_url(
            "https://github.com/rustdesk/rustdesk/releases/tag/v1.4.6",
            UpdateArtifactQuery {
                platform: "windows",
                arch: "x86_64",
                format: "msi",
                file_name: None,
            },
        )
        .unwrap();
        let exe = verified_update_artifact_for_release_page_url(
            "https://github.com/rustdesk/rustdesk/releases/tag/v1.4.6",
            UpdateArtifactQuery {
                platform: "windows",
                arch: "x86_64",
                format: "exe",
                file_name: None,
            },
        )
        .unwrap();

        assert_eq!(msi.file_name, msi_name);
        assert_eq!(exe.file_name, exe_name);
        clear_test_update_sidecars();
    }

    #[test]
    fn verified_release_page_url_rejects_non_rustdesk_release_page_url() {
        assert!(verified_update_artifact_for_release_page_url(
            "https://example.com/rustdesk/rustdesk/releases/tag/v1.4.6",
            current_update_artifact_query(false),
        )
        .is_err());
        assert!(verified_update_artifact_for_release_page_url(
            "https://github.com/rustdesk/rustdesk/releases/download/v1.4.6/rustdesk.exe",
            current_update_artifact_query(false),
        )
        .is_err());
    }

    #[test]
    fn current_update_artifact_query_uses_requested_package_format() {
        let exe = current_update_artifact_query(false);
        let msi = current_update_artifact_query(true);

        assert_eq!(exe.platform, current_update_platform());
        assert_eq!(exe.arch, current_update_arch());
        assert_eq!(exe.file_name, None);
        assert_eq!(exe.format, current_update_format(false));
        assert_eq!(msi.format, current_update_format(true));
    }

    #[test]
    fn verified_update_metadata_rejects_mismatched_expected_version_release_and_prefix() {
        let _guard = CHECK_UPDATE_TEST_LOCK.lock().unwrap();
        let update_url = "https://github.com/rustdesk/rustdesk/releases/tag/v1.4.6";
        set_test_verified_update_artifact(stale_update_artifact());

        let version_err = download_verified_update_artifact(
            update_url,
            hbb_common::update_metadata::UpdateArtifactQuery {
                platform: "windows",
                arch: "x86_64",
                format: "exe",
                file_name: None,
            },
        )
        .err()
        .map(|err| err.to_string())
        .unwrap_or_default();
        assert!(version_err.contains("version"));

        let mut artifact = stale_update_artifact();
        artifact.version = "1.4.6".to_owned();
        set_test_verified_update_artifact(artifact);
        let release_err = download_verified_update_artifact(
            update_url,
            hbb_common::update_metadata::UpdateArtifactQuery {
                platform: "windows",
                arch: "x86_64",
                format: "exe",
                file_name: None,
            },
        )
        .err()
        .map(|err| err.to_string())
        .unwrap_or_default();
        assert!(release_err.contains("release"));

        let mut artifact = stale_update_artifact();
        artifact.version = "1.4.6".to_owned();
        artifact.release_id = "v1.4.6".to_owned();
        artifact.url =
            "https://github.com/rustdesk/rustdesk/releases/download/v1.4.5/rustdesk.exe"
                .to_owned();
        set_test_verified_update_artifact(artifact);
        let prefix_err = download_verified_update_artifact(
            update_url,
            hbb_common::update_metadata::UpdateArtifactQuery {
                platform: "windows",
                arch: "x86_64",
                format: "exe",
                file_name: None,
            },
        )
        .err()
        .map(|err| err.to_string())
        .unwrap_or_default();
        assert!(prefix_err.contains("release prefix"));
        clear_test_verified_update_artifact();
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
    fn ensure_verified_update_artifact_redownloads_after_cached_size_mismatch() {
        let test_dir = std::env::temp_dir().join(format!(
            "rustdesk-updater-size-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();
        let file_path = test_dir.join("rustdesk-update.exe");
        std::fs::write(&file_path, b"update").unwrap();
        let download_url =
            "https://github.com/rustdesk/rustdesk/releases/download/v1.4.6/rustdesk-update.exe";
        set_test_check_update_inputs(
            "https://github.com/rustdesk/rustdesk/releases/tag/v1.4.6",
            download_url,
            b"downloaded",
        );

        let result = ensure_verified_update_artifact(
            download_url,
            &file_path,
            10,
            "b7a8a844a613be796bc1892dc480f9d92c50d32a5713a87758e5c5addc4ec814",
        );

        assert!(result.is_ok());
        assert_eq!(std::fs::read(&file_path).unwrap(), b"downloaded");
        std::fs::remove_dir_all(&test_dir).unwrap();
        clear_test_check_update_inputs();
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
    fn automatic_update_uses_verified_artifact_version_not_url_basename() {
        let _guard = CHECK_UPDATE_TEST_LOCK.lock().unwrap();
        let artifact = stale_update_artifact();
        let download_url = artifact.url.clone();
        let expected_sha256 = artifact.sha256.clone();
        set_test_verified_update_artifact(artifact);
        set_test_check_update_inputs(
            "https://github.com/rustdesk/rustdesk/releases/tag/v9.9.9",
            download_url.as_str(),
            b"update",
        );

        check_update(true).unwrap();

        let launched = take_test_update_launch().unwrap();
        assert_eq!(launched.version, "9.9.9");
        assert_eq!(launched.expected_sha256, expected_sha256);
        clear_test_verified_update_artifact();
        clear_test_check_update_inputs();
    }

    #[test]
    fn github_release_api_url_accepts_tag_url() {
        let api_url =
            github_release_api_url("https://github.com/rustdesk/rustdesk/releases/tag/1.4.3")
                .unwrap();

        assert_eq!(
            api_url,
            "https://api.github.com/repos/rustdesk/rustdesk/releases/tags/1.4.3"
        );
    }

    #[test]
    fn github_release_api_url_accepts_download_url() {
        let api_url = github_release_api_url(
            "https://github.com/rustdesk/rustdesk/releases/download/1.4.3/rustdesk-1.4.3-x86_64.exe",
        )
        .unwrap();

        assert_eq!(
            api_url,
            "https://api.github.com/repos/rustdesk/rustdesk/releases/tags/1.4.3"
        );
    }

    #[test]
    fn github_release_api_url_rejects_non_release_download_url() {
        assert!(github_release_api_url(
            "https://github.com/rustdesk/rustdesk/archive/refs/tags/1.4.3.zip"
        )
        .is_err());
    }

    #[test]
    fn github_release_api_url_rejects_non_github_url() {
        assert!(github_release_api_url("https://example.com/rustdesk/releases/tag/1.4.3").is_err());
    }

    #[test]
    fn github_release_api_url_rejects_non_rustdesk_repo() {
        assert!(
            github_release_api_url("https://github.com/other/rustdesk/releases/tag/1.4.3").is_err()
        );
        assert!(
            github_release_api_url("https://github.com/rustdesk/other/releases/tag/1.4.3").is_err()
        );
    }

    #[test]
    fn github_release_digest_requires_exact_asset_name_and_sha256_digest() {
        let json = r#"{
            "assets": [
                {"name": "rustdesk-1.4.3-x86_64.exe", "digest": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"},
                {"name": "rustdesk-1.4.3-x86_64.msi", "digest": "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"}
            ]
        }"#;

        let digest = github_release_asset_sha256(json, "rustdesk-1.4.3-x86_64.exe").unwrap();

        assert_eq!(
            digest,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
    }

    #[test]
    fn github_release_digest_rejects_missing_or_malformed_digest() {
        let missing = r#"{"assets": [{"name": "rustdesk.exe"}]}"#;
        let malformed = r#"{"assets": [{"name": "rustdesk.exe", "digest": "sha1:abcd"}]}"#;

        assert!(github_release_asset_sha256(missing, "rustdesk.exe").is_err());
        assert!(github_release_asset_sha256(malformed, "rustdesk.exe").is_err());
    }

    #[test]
    fn update_http_request_timeout_is_bounded() {
        assert_eq!(UPDATE_HTTP_REQUEST_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn download_file_sha256_cache_roundtrips_and_clears() {
        let download_url = format!(
            "https://github.com/rustdesk/rustdesk/releases/download/test/rustdesk-cache-test-{}-{}.exe",
            std::process::id(),
            hbb_common::rand::random::<u64>()
        );
        let expected_sha256 = "ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789";
        clear_download_file_expected_sha256(&download_url);

        let cached = cache_download_file_expected_sha256(&download_url, expected_sha256).unwrap();

        assert_eq!(
            cached,
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        );
        assert_eq!(
            cached_download_file_expected_sha256(&download_url),
            Some(cached)
        );
        clear_download_file_expected_sha256(&download_url);
        assert_eq!(cached_download_file_expected_sha256(&download_url), None);
    }

    #[test]
    fn download_file_sha256_cache_rejects_malformed_digest() {
        let download_url = format!(
            "https://github.com/rustdesk/rustdesk/releases/download/test/rustdesk-cache-test-{}-{}.exe",
            std::process::id(),
            hbb_common::rand::random::<u64>()
        );
        clear_download_file_expected_sha256(&download_url);

        assert!(cache_download_file_expected_sha256(&download_url, "sha256:not-hex").is_err());
        assert_eq!(cached_download_file_expected_sha256(&download_url), None);
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
    fn write_verified_download_removes_temp_file_on_sha256_error() {
        let test_dir = std::env::temp_dir().join(format!(
            "rustdesk-updater-cleanup-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();
        let final_path = test_dir.join("rustdesk-update.exe");

        let result = write_verified_download(
            &final_path,
            b"update",
            "0000000000000000000000000000000000000000000000000000000000000000",
        );

        assert!(result.is_err());
        assert!(!final_path.exists());
        assert!(std::fs::read_dir(&test_dir).unwrap().next().is_none());
        std::fs::remove_dir_all(&test_dir).unwrap();
    }

    #[test]
    fn write_verified_download_from_reader_installs_verified_file() {
        let test_dir = std::env::temp_dir().join(format!(
            "rustdesk-updater-reader-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();
        let final_path = test_dir.join("rustdesk-update.exe");
        let mut data: &[u8] = b"update";

        write_verified_download_from_reader(
            &final_path,
            &mut data,
            "2937013f2181810606b2a799b05bda2849f3e369a20982a4138f0e0a55984ce4",
        )
        .unwrap();

        assert_eq!(std::fs::read(&final_path).unwrap(), b"update");
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

        let result = write_verified_download(
            &final_path,
            b"update",
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
