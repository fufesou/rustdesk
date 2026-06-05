use super::create_http_client_async_with_url_strict;
use hbb_common::{
    bail,
    lazy_static::lazy_static,
    log,
    tokio::{
        self,
        fs::OpenOptions,
        io::AsyncWriteExt,
        sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    },
    ResultType,
};
use serde_derive::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::Duration,
};

lazy_static! {
    static ref DOWNLOADERS: Mutex<HashMap<String, Downloader>> = Default::default();
}

// Download IDs are URL-based and may be reused after cancel/retry.
// The token identifies one worker attempt so stale workers cannot mutate a new entry.
static NEXT_DOWNLOADER_TOKEN: AtomicU64 = AtomicU64::new(1);

const DOWNLOAD_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(60);

fn next_downloader_token() -> u64 {
    NEXT_DOWNLOADER_TOKEN.fetch_add(1, Ordering::Relaxed)
}

fn with_downloader_if_token_matches<R>(
    id: &str,
    token: u64,
    update: impl FnOnce(&mut Downloader) -> R,
) -> Option<R> {
    let mut downloaders = DOWNLOADERS.lock().unwrap();
    let downloader = downloaders.get_mut(id)?;
    if downloader.token != token {
        // The entry was removed/replaced while this worker was still running.
        return None;
    }
    Some(update(downloader))
}

fn take_downloader_if_token_matches(id: &str, token: u64) -> Option<Downloader> {
    let mut downloaders = DOWNLOADERS.lock().unwrap();
    if !downloaders
        .get(id)
        .is_some_and(|downloader| downloader.token == token)
    {
        return None;
    }
    downloaders.remove(id)
}

fn remove_downloader_if_token_matches(id: &str, token: u64) {
    let _ = take_downloader_if_token_matches(id, token);
}

fn ensure_success_status(status: reqwest::StatusCode, context: &str) -> ResultType<()> {
    if status.is_success() {
        return Ok(());
    }
    bail!("{context}: {status}");
}

fn ensure_downloaded_size_matches(
    downloaded_size: u64,
    total_size: u64,
    url: &str,
) -> ResultType<()> {
    if downloaded_size == total_size {
        return Ok(());
    }
    bail!("Downloaded size mismatch for {url}: expected {total_size}, got {downloaded_size}");
}

fn has_cancel_request(rx_cancel: &mut UnboundedReceiver<()>) -> bool {
    rx_cancel.try_recv().is_ok()
}

fn finish_download(id: &str, token: u64, final_path: PathBuf) -> bool {
    with_downloader_if_token_matches(id, token, |downloader| {
        downloader.path = final_path;
        downloader.finalizing = false;
        downloader.finished = true;
    })
    .is_some()
}

fn fail_download(id: &str, token: u64, error: String) -> bool {
    with_downloader_if_token_matches(id, token, |downloader| {
        if downloader.path_owned {
            std::fs::remove_file(&downloader.path).ok();
        }
        downloader.finalizing = false;
        downloader.finished = false;
        downloader.error = Some(error);
    })
    .is_some()
}

fn remove_owned_stale_path(stale_path: (PathBuf, bool)) {
    let (path, true) = stale_path else {
        return;
    };
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            log::warn!(
                "Failed to remove stale download file {}: {}",
                path.display(),
                e
            );
        }
    }
}

fn set_download_finalizing(id: &str, token: u64, rx_cancel: &mut UnboundedReceiver<()>) -> bool {
    let mut downloaders = DOWNLOADERS.lock().unwrap();
    let Some(downloader) = downloaders.get_mut(id) else {
        return false;
    };
    if downloader.token != token || has_cancel_request(rx_cancel) {
        return false;
    }
    downloader.finalizing = true;
    true
}

fn identity_encoded_request(builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    builder.header(reqwest::header::ACCEPT_ENCODING, "identity")
}

/// This struct is used to return the download data to the caller.
/// The caller should check if the file is downloaded successfully and remove the job from the map.
#[derive(Serialize, Debug)]
pub struct DownloadData {
    pub path: PathBuf,
    pub total_size: u64,
    pub downloaded_size: u64,
    pub finished: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub finalizing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

struct Downloader {
    token: u64,
    path: PathBuf,
    path_owned: bool,
    total_size: u64,
    auto_del_dur: Option<Duration>,
    downloaded_size: u64,
    error: Option<String>,
    finished: bool,
    finalizing: bool,
    tx_cancel: UnboundedSender<()>,
}

type DownloadFinishHandler = Box<dyn FnOnce(&Path) -> ResultType<PathBuf> + Send>;

// The caller should check if the file is downloaded successfully and remove the job from the map.
pub fn download_file_with_finish_handler<F>(
    url: String,
    path: PathBuf,
    auto_del_dur: Option<Duration>,
    signed_size: u64,
    finish_handler: F,
) -> ResultType<String>
where
    F: FnOnce(&Path) -> ResultType<PathBuf> + Send + 'static,
{
    download_file_(
        url,
        path,
        auto_del_dur,
        signed_size,
        Box::new(finish_handler),
    )
}

fn download_file_(
    url: String,
    path: PathBuf,
    auto_del_dur: Option<Duration>,
    signed_size: u64,
    finish_handler: DownloadFinishHandler,
) -> ResultType<String> {
    let id = url.clone();
    // First pass: if a non-error downloader exists for this URL, reuse it.
    // If an errored downloader exists, remove it so this call can retry.
    {
        let mut downloaders = DOWNLOADERS.lock().unwrap();
        if let Some(downloader) = downloaders.get(&id) {
            if downloader.error.is_none() {
                bail!("Existing downloader for {id} cannot be reused");
            }
            let stale_path = (downloader.path.clone(), downloader.path_owned);
            remove_owned_stale_path(stale_path);
            downloaders.remove(&id);
        }
    }

    if path.exists() {
        bail!("File {} already exists", path.display());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let (tx, rx) = unbounded_channel();
    let token = next_downloader_token();
    let downloader = Downloader {
        token,
        path: path.clone(),
        path_owned: false,
        total_size: signed_size,
        auto_del_dur,
        downloaded_size: 0,
        error: None,
        tx_cancel: tx,
        finished: false,
        finalizing: false,
    };
    // Second pass (atomic with insert) to avoid race with another concurrent caller.
    {
        let mut downloaders = DOWNLOADERS.lock().unwrap();
        if let Some(existing) = downloaders.get(&id) {
            if existing.error.is_none() {
                bail!("Existing downloader for {id} cannot be reused");
            }
            let stale_path = (existing.path.clone(), existing.path_owned);
            remove_owned_stale_path(stale_path);
            downloaders.remove(&id);
        }
        downloaders.insert(id.clone(), downloader);
    }

    let id2 = id.clone();
    std::thread::spawn(move || {
        match do_download(
            &id2,
            url,
            path,
            auto_del_dur,
            signed_size,
            finish_handler,
            rx,
            token,
        ) {
            Ok(is_all_downloaded) => {
                let (downloaded_size, total_size) =
                    with_downloader_if_token_matches(&id2, token, |downloader| {
                        (downloader.downloaded_size, downloader.total_size)
                    })
                    .unwrap_or((0, 0));
                log::info!(
                    "Download {} end, {}/{}, {:.2} %",
                    &id2,
                    downloaded_size,
                    total_size,
                    if total_size == 0 {
                        0.0
                    } else {
                        downloaded_size as f64 / total_size as f64 * 100.0
                    }
                );

                let is_canceled = !is_all_downloaded;
                if is_canceled {
                    if let Some(downloader) = take_downloader_if_token_matches(&id2, token) {
                        if downloader.path.exists() {
                            if downloader.path_owned {
                                std::fs::remove_file(downloader.path).ok();
                            }
                        }
                    }
                }
            }
            Err(e) => {
                let err = e.to_string();
                log::error!("Download {}, failed: {}", &id2, &err);
                fail_download(&id2, token, err);
            }
        }
    });

    Ok(id)
}

pub fn complete_existing_file(
    id: String,
    path: PathBuf,
    total_size: u64,
    auto_del_dur: Option<Duration>,
) -> ResultType<String> {
    let final_path = path.clone();
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file() {
        bail!("Download path is not a file: {}", path.display());
    }
    ensure_downloaded_size_matches(metadata.len(), total_size, &id)?;

    let (tx, _rx) = unbounded_channel();
    let token = next_downloader_token();
    let downloader = Downloader {
        token,
        path,
        path_owned: false,
        total_size,
        auto_del_dur,
        downloaded_size: total_size,
        error: None,
        finished: true,
        finalizing: false,
        tx_cancel: tx,
    };
    {
        let mut downloaders = DOWNLOADERS.lock().unwrap();
        if let Some(existing) = downloaders.get(&id) {
            if existing.error.is_none() {
                if !existing.finished
                    || existing.path != final_path
                    || existing.total_size != total_size
                    || existing.auto_del_dur != auto_del_dur
                {
                    bail!("Existing downloader for {id} is not complete");
                }
                return Ok(id);
            }
            if existing.path != final_path {
                let stale_path = (existing.path.clone(), existing.path_owned);
                remove_owned_stale_path(stale_path);
            }
        }
        downloaders.insert(id.clone(), downloader);
    }

    if let Some(dur) = auto_del_dur {
        let id_del = id.clone();
        std::thread::spawn(move || {
            std::thread::sleep(dur);
            remove_downloader_if_token_matches(&id_del, token);
        });
    }
    Ok(id)
}

#[tokio::main(flavor = "current_thread")]
async fn do_download(
    id: &str,
    url: String,
    path: PathBuf,
    auto_del_dur: Option<Duration>,
    signed_size: u64,
    finish_handler: DownloadFinishHandler,
    mut rx_cancel: UnboundedReceiver<()>,
    token: u64,
) -> ResultType<bool> {
    let client;
    tokio::select! {
        _ = rx_cancel.recv() => {
            return Ok(false);
        }
        created_client = create_http_client_async_with_url_strict(&url) => {
            client = created_client?;
        }
    };

    let mut is_all_downloaded = false;
    let total_size = signed_size;

    let mut response;
    tokio::select! {
        _ = rx_cancel.recv() => {
            return Ok(is_all_downloaded);
        }
        resp = tokio::time::timeout(
            DOWNLOAD_REQUEST_TIMEOUT,
            identity_encoded_request(client.get(&url)).send(),
        ) => {
            match resp {
                Ok(Ok(resp)) => {
                    ensure_success_status(resp.status(), "Failed to start download")?;
                    response = resp;
                }
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => bail!("Timed out while starting download"),
            }
        }
    }

    let mut dest = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .await?;
    if with_downloader_if_token_matches(id, token, |downloader| {
        downloader.path_owned = true;
    })
    .is_none()
    {
        std::fs::remove_file(&path).ok();
        return Ok(false);
    }

    let mut downloaded_size = 0_u64;
    loop {
        tokio::select! {
            _ = rx_cancel.recv() => {
                break;
            }
            chunk = tokio::time::timeout(DOWNLOAD_READ_TIMEOUT, response.chunk()) => {
                match chunk {
                    Ok(Ok(Some(chunk))) => {
                        let chunk_size = chunk.len() as u64;
                        let Some(next_downloaded_size) = downloaded_size.checked_add(chunk_size) else {
                            bail!("Downloaded size overflow for {url}");
                        };
                        if next_downloaded_size > total_size {
                            ensure_downloaded_size_matches(next_downloaded_size, total_size, &url)?;
                        }
                        downloaded_size = next_downloaded_size;
                        dest.write_all(&chunk).await?;
                        dest.flush().await?;
                        let _ = with_downloader_if_token_matches(id, token, |downloader| {
                            downloader.downloaded_size += chunk_size;
                        });
                    }
                    Ok(Ok(None)) => {
                        is_all_downloaded = true;
                        break;
                    },
                    Ok(Err(e)) => {
                        log::error!("Download {} failed: {}", id, e);
                        return Err(e.into());
                    }
                    Err(_) => bail!("Timed out while reading download data"),
                }
            }
        }
    }

    dest.flush().await?;

    let final_path = if is_all_downloaded {
        ensure_downloaded_size_matches(downloaded_size, total_size, &url)?;
        if has_cancel_request(&mut rx_cancel) {
            return Ok(false);
        }
        if !set_download_finalizing(id, token, &mut rx_cancel) {
            return Ok(false);
        }
        Some(finish_handler(&path)?)
    } else {
        None
    };

    if is_all_downloaded {
        let Some(final_path) = final_path else {
            return Ok(false);
        };
        if finish_download(id, token, final_path) {
            let id_del = id.to_string();
            if let Some(dur) = auto_del_dur {
                std::thread::spawn(move || {
                    std::thread::sleep(dur);
                    remove_downloader_if_token_matches(&id_del, token);
                });
            }
        }
    }
    Ok(is_all_downloaded)
}

pub fn get_download_data(id: &str) -> ResultType<DownloadData> {
    let downloaders = DOWNLOADERS.lock().unwrap();
    if let Some(downloader) = downloaders.get(id) {
        let downloaded_size = downloader.downloaded_size;
        let total_size = downloader.total_size;
        let error = downloader.error.clone();
        let finished = downloader.finished;
        let finalizing = downloader.finalizing;
        let path = downloader.path.clone();
        let download_data = DownloadData {
            path,
            total_size,
            downloaded_size,
            finished,
            finalizing,
            error,
        };
        Ok(download_data)
    } else {
        bail!("Downloader not found")
    }
}

pub fn cancel(id: &str) {
    let downloaders = DOWNLOADERS.lock().unwrap();
    if downloaders
        .get(id)
        .is_some_and(|downloader| downloader.finished)
    {
        return;
    }
    if downloaders
        .get(id)
        .is_some_and(|downloader| downloader.finalizing)
    {
        return;
    }
    if let Some(downloader) = downloaders.get(id) {
        // downloader.is_canceled.store(true, Ordering::SeqCst);
        // The receiver may not be able to receive the cancel signal, so we also set the atomic bool to true
        let _ = downloader.tx_cancel.send(());
    }
}

pub fn remove(id: &str) {
    let _ = DOWNLOADERS.lock().unwrap().remove(id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_file_with_finish_handler_does_not_reuse_existing_downloader() {
        let path = std::env::temp_dir().join(format!(
            "rustdesk-downloader-finish-handler-contract-test-{}",
            std::process::id()
        ));
        let id = format!(
            "https://example.com/rustdesk-finish-handler-contract-{}.exe",
            std::process::id()
        );

        let (tx, _rx) = unbounded_channel();
        DOWNLOADERS.lock().unwrap().insert(
            id.clone(),
            Downloader {
                token: next_downloader_token(),
                path: path.clone(),
                path_owned: false,
                total_size: 8,
                auto_del_dur: None,
                downloaded_size: 0,
                error: None,
                finished: false,
                finalizing: false,
                tx_cancel: tx,
            },
        );

        let result =
            download_file_with_finish_handler(id.clone(), path, None, 8, |_| unreachable!());

        assert!(result.is_err());
        remove(&id);
    }

    #[test]
    fn downloaded_size_must_match_content_length() {
        assert!(ensure_downloaded_size_matches(10, 10, "https://example.com/file").is_ok());
        assert!(ensure_downloaded_size_matches(9, 10, "https://example.com/file").is_err());
        assert!(ensure_downloaded_size_matches(11, 10, "https://example.com/file").is_err());
    }

    #[test]
    fn completed_existing_file_is_reported_as_finished() {
        let path = std::env::temp_dir().join(format!(
            "rustdesk-downloader-complete-test-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"rustdesk").unwrap();
        let id = format!("https://example.com/rustdesk-{}.exe", std::process::id());

        let result = complete_existing_file(id.clone(), path.clone(), 8, None);
        assert!(result.is_ok());

        let data = get_download_data(&id).unwrap();
        assert!(data.finished);
        assert_eq!(data.downloaded_size, 8);
        assert_eq!(data.total_size, 8);
        assert_eq!(data.path, path.clone());

        remove(&id);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn cancel_keeps_finished_existing_file_visible_without_deleting_it() {
        let path = std::env::temp_dir().join(format!(
            "rustdesk-downloader-cancel-finished-test-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"rustdesk").unwrap();
        let id = format!(
            "https://example.com/rustdesk-cancel-finished-{}.exe",
            std::process::id()
        );

        complete_existing_file(id.clone(), path.clone(), 8, None).unwrap();
        cancel(&id);

        let data = get_download_data(&id).unwrap();
        assert!(data.finished);
        assert_eq!(data.path, path.clone());
        assert!(path.exists());
        remove(&id);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn stale_worker_state_updates_do_not_mutate_replaced_downloader() {
        let id = format!(
            "https://example.com/rustdesk-stale-token-{}.exe",
            std::process::id()
        );
        let (tx, mut rx) = unbounded_channel();
        let token = next_downloader_token();
        let stale_token = token.wrapping_add(1);
        DOWNLOADERS.lock().unwrap().insert(
            id.clone(),
            Downloader {
                token,
                path: PathBuf::from("active.exe"),
                path_owned: false,
                total_size: 8,
                auto_del_dur: None,
                downloaded_size: 0,
                error: None,
                finished: false,
                finalizing: false,
                tx_cancel: tx,
            },
        );

        assert!(!set_download_finalizing(&id, stale_token, &mut rx));
        assert!(!finish_download(
            &id,
            stale_token,
            PathBuf::from("stale.exe")
        ));
        assert!(!fail_download(&id, stale_token, "stale error".to_owned()));

        let data = get_download_data(&id).unwrap();
        assert!(!data.finalizing);
        assert!(!data.finished);
        assert!(data.error.is_none());
        assert_eq!(data.path, PathBuf::from("active.exe"));
        remove(&id);
    }

    #[test]
    fn fail_download_keeps_unowned_path() {
        let path = std::env::temp_dir().join(format!(
            "rustdesk-downloader-unowned-path-test-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"external").unwrap();
        let id = format!(
            "https://example.com/rustdesk-unowned-path-{}.exe",
            std::process::id()
        );
        let (tx, _rx) = unbounded_channel();
        let token = next_downloader_token();
        DOWNLOADERS.lock().unwrap().insert(
            id.clone(),
            Downloader {
                token,
                path: path.clone(),
                path_owned: false,
                total_size: 8,
                auto_del_dur: None,
                downloaded_size: 0,
                error: None,
                finished: false,
                finalizing: false,
                tx_cancel: tx,
            },
        );

        assert!(fail_download(&id, token, "open failed".to_owned()));

        assert!(path.exists());
        remove(&id);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn retry_after_error_keeps_unowned_stale_path() {
        let stale_path = std::env::temp_dir().join(format!(
            "rustdesk-downloader-retry-unowned-path-test-{}",
            std::process::id()
        ));
        let conflicting_path = std::env::temp_dir().join(format!(
            "rustdesk-downloader-retry-conflict-test-{}",
            std::process::id()
        ));
        std::fs::write(&stale_path, b"external").unwrap();
        std::fs::write(&conflicting_path, b"conflict").unwrap();
        let id = format!(
            "https://example.com/rustdesk-retry-unowned-path-{}.exe",
            std::process::id()
        );
        let (tx, _rx) = unbounded_channel();
        DOWNLOADERS.lock().unwrap().insert(
            id.clone(),
            Downloader {
                token: next_downloader_token(),
                path: stale_path.clone(),
                path_owned: false,
                total_size: 8,
                auto_del_dur: None,
                downloaded_size: 0,
                error: Some("open failed".to_owned()),
                finished: false,
                finalizing: false,
                tx_cancel: tx,
            },
        );

        let result = download_file_with_finish_handler(
            id,
            conflicting_path.clone(),
            None,
            8,
            |_| unreachable!(),
        );

        assert!(result.is_err());
        assert!(stale_path.exists());
        std::fs::remove_file(stale_path).unwrap();
        std::fs::remove_file(conflicting_path).unwrap();
    }

    #[test]
    fn complete_existing_file_keeps_unowned_stale_path() {
        let stale_path = std::env::temp_dir().join(format!(
            "rustdesk-downloader-complete-unowned-path-test-{}",
            std::process::id()
        ));
        let final_path = std::env::temp_dir().join(format!(
            "rustdesk-downloader-complete-final-test-{}",
            std::process::id()
        ));
        std::fs::write(&stale_path, b"external").unwrap();
        std::fs::write(&final_path, b"rustdesk").unwrap();
        let id = format!(
            "https://example.com/rustdesk-complete-unowned-path-{}.exe",
            std::process::id()
        );
        let (tx, _rx) = unbounded_channel();
        DOWNLOADERS.lock().unwrap().insert(
            id.clone(),
            Downloader {
                token: next_downloader_token(),
                path: stale_path.clone(),
                path_owned: false,
                total_size: 8,
                auto_del_dur: None,
                downloaded_size: 0,
                error: Some("open failed".to_owned()),
                finished: false,
                finalizing: false,
                tx_cancel: tx,
            },
        );

        let result = complete_existing_file(id.clone(), final_path.clone(), 8, None);

        assert!(result.is_ok());
        assert!(stale_path.exists());
        remove(&id);
        std::fs::remove_file(stale_path).unwrap();
        std::fs::remove_file(final_path).unwrap();
    }

    #[test]
    fn cancel_keeps_finalizing_downloader_visible() {
        let id = format!(
            "https://example.com/rustdesk-cancel-finalizing-{}.exe",
            std::process::id()
        );
        let (tx, _rx) = unbounded_channel();
        DOWNLOADERS.lock().unwrap().insert(
            id.clone(),
            Downloader {
                token: next_downloader_token(),
                path: PathBuf::from("finalizing.exe"),
                path_owned: false,
                total_size: 8,
                auto_del_dur: None,
                downloaded_size: 8,
                error: None,
                finished: false,
                finalizing: true,
                tx_cancel: tx,
            },
        );

        cancel(&id);

        let data = get_download_data(&id).unwrap();
        assert!(data.finalizing);
        assert!(!data.finished);
        remove(&id);
    }

    #[test]
    fn complete_existing_file_rejects_incomplete_existing_downloader() {
        let final_path = std::env::temp_dir().join(format!(
            "rustdesk-downloader-incomplete-existing-test-{}.exe",
            std::process::id()
        ));
        std::fs::write(&final_path, b"rustdesk").unwrap();
        let id = format!(
            "https://example.com/rustdesk-incomplete-{}.exe",
            std::process::id()
        );

        let (tx, _rx) = unbounded_channel();
        DOWNLOADERS.lock().unwrap().insert(
            id.clone(),
            Downloader {
                token: next_downloader_token(),
                path: final_path.clone(),
                path_owned: false,
                total_size: 8,
                auto_del_dur: None,
                downloaded_size: 0,
                error: None,
                finished: false,
                finalizing: false,
                tx_cancel: tx,
            },
        );

        let result = complete_existing_file(id.clone(), final_path.clone(), 8, None);

        assert!(result.is_err());
        remove(&id);
        std::fs::remove_file(final_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn complete_existing_file_rejects_symlink() {
        let test_dir = std::env::temp_dir().join(format!(
            "rustdesk-downloader-symlink-existing-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&test_dir);
        std::fs::create_dir_all(&test_dir).unwrap();
        let target_path = test_dir.join("target");
        let link_path = test_dir.join("rustdesk-update.exe");
        std::fs::write(&target_path, b"rustdesk").unwrap();
        std::os::unix::fs::symlink(&target_path, &link_path).unwrap();
        let id = format!(
            "https://example.com/rustdesk-symlink-{}.exe",
            std::process::id()
        );

        let result = complete_existing_file(id, link_path, 8, None);

        assert!(result.is_err());
        std::fs::remove_dir_all(test_dir).unwrap();
    }
}
