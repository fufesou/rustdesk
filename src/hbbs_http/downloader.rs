use super::create_http_client_async_with_url_strict;
use hbb_common::{
    bail,
    lazy_static::lazy_static,
    log,
    tokio::{
        self,
        fs::{File, OpenOptions},
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

fn finish_download(id: &str, token: u64, final_path: Option<PathBuf>) -> bool {
    with_downloader_if_token_matches(id, token, |downloader| {
        if let Some(final_path) = final_path {
            downloader.path = Some(final_path);
        }
        downloader.finalizing = false;
        downloader.finished = true;
    })
    .is_some()
}

fn fail_download(id: &str, token: u64, error: String) -> bool {
    with_downloader_if_token_matches(id, token, |downloader| {
        if let Some(path) = downloader.path.as_ref() {
            std::fs::remove_file(path).ok();
        }
        downloader.finalizing = false;
        downloader.finished = false;
        downloader.error = Some(error);
    })
    .is_some()
}

fn set_download_finalizing(id: &str, token: u64) -> bool {
    with_downloader_if_token_matches(id, token, |downloader| {
        downloader.finalizing = true;
    })
    .is_some()
}

fn resolve_total_size(
    signed_size: Option<u64>,
    content_length: Option<u64>,
    path: Option<&Path>,
    url: &str,
) -> ResultType<Option<u64>> {
    let total_size = signed_size.or(content_length);
    if total_size.is_none() && path.is_none() {
        bail!("Refusing in-memory download without a known size for {url}");
    }
    Ok(total_size)
}

fn identity_encoded_request(builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    builder.header(reqwest::header::ACCEPT_ENCODING, "identity")
}

/// This struct is used to return the download data to the caller.
/// The caller should check if the file is downloaded successfully and remove the job from the map.
/// If the file is not downloaded successfully, the `data` field will be empty.
/// If the file is downloaded successfully, the `data` field will contain the downloaded data if `path` is None.
#[derive(Serialize, Debug)]
pub struct DownloadData {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub data: Vec<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_size: Option<u64>,
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
    data: Vec<u8>,
    path: Option<PathBuf>,
    // Some file may be empty, so we use Option<u64> to indicate if the size is known
    total_size: Option<u64>,
    signed_size: Option<u64>,
    auto_del_dur: Option<Duration>,
    has_finish_handler: bool,
    downloaded_size: u64,
    error: Option<String>,
    finished: bool,
    finalizing: bool,
    tx_cancel: UnboundedSender<()>,
}

type DownloadFinishHandler = Box<dyn FnOnce(&Path) -> ResultType<PathBuf> + Send>;

fn ensure_downloader_contract_matches(
    downloader: &Downloader,
    path: Option<&Path>,
    signed_size: Option<u64>,
    auto_del_dur: Option<Duration>,
    has_finish_handler: bool,
    url: &str,
) -> ResultType<()> {
    if downloader.has_finish_handler || has_finish_handler {
        bail!("Existing downloader for {url} cannot be reused with a finish handler");
    }
    if downloader.path.as_deref() == path
        && downloader.signed_size == signed_size
        && downloader.auto_del_dur == auto_del_dur
    {
        return Ok(());
    }
    bail!("Existing downloader for {url} has different options");
}

// The caller should check if the file is downloaded successfully and remove the job from the map.
pub fn download_file(
    url: String,
    path: Option<PathBuf>,
    auto_del_dur: Option<Duration>,
    signed_size: Option<u64>,
) -> ResultType<String> {
    download_file_(url, path, auto_del_dur, signed_size, None)
}

pub fn download_file_with_finish_handler<F>(
    url: String,
    path: Option<PathBuf>,
    auto_del_dur: Option<Duration>,
    signed_size: Option<u64>,
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
        Some(Box::new(finish_handler)),
    )
}

fn download_file_(
    url: String,
    path: Option<PathBuf>,
    auto_del_dur: Option<Duration>,
    signed_size: Option<u64>,
    finish_handler: Option<DownloadFinishHandler>,
) -> ResultType<String> {
    if finish_handler.is_some() && path.is_none() {
        bail!("Download finish handler requires a file path");
    }
    let id = url.clone();
    let has_finish_handler = finish_handler.is_some();
    // First pass: if a non-error downloader exists for this URL, reuse it.
    // If an errored downloader exists, remove it so this call can retry.
    let mut stale_path = None;
    {
        let mut downloaders = DOWNLOADERS.lock().unwrap();
        if let Some(downloader) = downloaders.get(&id) {
            if downloader.error.is_none() {
                ensure_downloader_contract_matches(
                    downloader,
                    path.as_deref(),
                    signed_size,
                    auto_del_dur,
                    has_finish_handler,
                    &id,
                )?;
                return Ok(id);
            }
            stale_path = downloader.path.clone();
            downloaders.remove(&id);
        }
    }
    if let Some(p) = stale_path {
        if p.exists() {
            if let Err(e) = std::fs::remove_file(&p) {
                log::warn!(
                    "Failed to remove stale download file {}: {}",
                    p.display(),
                    e
                );
            }
        }
    }

    if let Some(path) = path.as_ref() {
        if path.exists() {
            bail!("File {} already exists", path.display());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let (tx, rx) = unbounded_channel();
    let token = next_downloader_token();
    let downloader = Downloader {
        token,
        data: Vec::new(),
        path: path.clone(),
        total_size: signed_size,
        signed_size,
        auto_del_dur,
        has_finish_handler,
        downloaded_size: 0,
        error: None,
        tx_cancel: tx,
        finished: false,
        finalizing: false,
    };
    // Second pass (atomic with insert) to avoid race with another concurrent caller.
    let mut stale_path_after_check = None;
    {
        let mut downloaders = DOWNLOADERS.lock().unwrap();
        if let Some(existing) = downloaders.get(&id) {
            if existing.error.is_none() {
                ensure_downloader_contract_matches(
                    existing,
                    path.as_deref(),
                    signed_size,
                    auto_del_dur,
                    has_finish_handler,
                    &id,
                )?;
                return Ok(id);
            }
            stale_path_after_check = existing.path.clone();
            downloaders.remove(&id);
        }
        downloaders.insert(id.clone(), downloader);
    }
    if let Some(p) = stale_path_after_check {
        if p.exists() {
            if let Err(e) = std::fs::remove_file(&p) {
                log::warn!(
                    "Failed to remove stale download file {}: {}",
                    p.display(),
                    e
                );
            }
        }
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
                        (
                            downloader.downloaded_size,
                            downloader.total_size.unwrap_or(0),
                        )
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
                        if let Some(p) = downloader.path {
                            if p.exists() {
                                std::fs::remove_file(p).ok();
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
        data: Vec::new(),
        path: Some(path),
        total_size: Some(total_size),
        signed_size: Some(total_size),
        auto_del_dur,
        has_finish_handler: false,
        downloaded_size: total_size,
        error: None,
        finished: true,
        finalizing: false,
        tx_cancel: tx,
    };
    let mut stale_path = None;
    {
        let mut downloaders = DOWNLOADERS.lock().unwrap();
        if let Some(existing) = downloaders.get(&id) {
            if existing.error.is_none() {
                if !existing.finished
                    || existing.path.as_deref() != Some(final_path.as_path())
                    || existing.total_size != Some(total_size)
                    || existing.auto_del_dur != auto_del_dur
                {
                    bail!("Existing downloader for {id} is not complete");
                }
                return Ok(id);
            }
            stale_path = existing.path.clone();
        }
        downloaders.insert(id.clone(), downloader);
    }
    if let Some(stale_path) = stale_path {
        if stale_path != final_path && stale_path.exists() {
            std::fs::remove_file(stale_path).ok();
        }
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
    path: Option<PathBuf>,
    auto_del_dur: Option<Duration>,
    signed_size: Option<u64>,
    mut finish_handler: Option<DownloadFinishHandler>,
    mut rx_cancel: UnboundedReceiver<()>,
    token: u64,
) -> ResultType<bool> {
    let client;
    tokio::select! {
        _ = rx_cancel.recv() => {
            return Ok(false);
        }
        created_client = create_http_client_async_with_url_strict(&url) => {
            client = created_client;
        }
    };

    let mut is_all_downloaded = false;
    let content_length = tokio::select! {
        _ = rx_cancel.recv() => {
            return Ok(is_all_downloaded);
        }
        head_resp = tokio::time::timeout(
            DOWNLOAD_REQUEST_TIMEOUT,
            identity_encoded_request(client.head(&url)).send(),
        ) => {
            match head_resp {
                Ok(Ok(resp)) => {
                    if resp.status().is_success() {
                        let content_length = resp
                            .headers()
                            .get(reqwest::header::CONTENT_LENGTH)
                            .and_then(|ct_len| ct_len.to_str().ok())
                            .and_then(|ct_len| ct_len.parse::<u64>().ok());
                        if let Some(content_length) = content_length {
                            if signed_size.is_none() {
                                let _ = with_downloader_if_token_matches(id, token, |downloader| {
                                    downloader.total_size = Some(content_length);
                                });
                            } else if signed_size != Some(content_length) {
                                log::warn!(
                                    "Download size from metadata differs from content length for {}: metadata {:?}, content length {}",
                                    url,
                                    signed_size,
                                    content_length
                                );
                            }
                        }
                        content_length
                    } else {
                        log::warn!("Failed to get content length: {}", resp.status());
                        None
                    }
                }
                Ok(Err(e)) => {
                    log::warn!("Failed to get content length: {}", e);
                    None
                }
                Err(_) => {
                    log::warn!("Timed out while getting download file size");
                    None
                }
            }
        }
    };
    let total_size = resolve_total_size(signed_size, content_length, path.as_deref(), &url)?;

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

    let mut dest: Option<File> = None;
    let path_for_finish = path.clone();
    if let Some(p) = path {
        dest = Some(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(p)
                .await?,
        );
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
                        if let Some(total_size) = total_size {
                            if next_downloaded_size > total_size {
                                ensure_downloaded_size_matches(next_downloaded_size, total_size, &url)?;
                            }
                        }
                        downloaded_size = next_downloaded_size;
                        match dest {
                            Some(ref mut f) => {
                                f.write_all(&chunk).await?;
                                f.flush().await?;
                                let _ = with_downloader_if_token_matches(id, token, |downloader| {
                                    downloader.downloaded_size += chunk_size;
                                });
                            }
                            None => {
                                let _ = with_downloader_if_token_matches(id, token, |downloader| {
                                    downloader.data.extend_from_slice(&chunk);
                                    downloader.downloaded_size += chunk_size;
                                });
                            }
                        }
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

    if let Some(mut f) = dest.take() {
        f.flush().await?;
    }

    let final_path = if is_all_downloaded {
        if let Some(total_size) = total_size {
            ensure_downloaded_size_matches(downloaded_size, total_size, &url)?;
        }
        if has_cancel_request(&mut rx_cancel) {
            return Ok(false);
        }
        if !set_download_finalizing(id, token) {
            return Ok(false);
        }
        if let Some(finish_handler) = finish_handler.take() {
            let Some(downloaded_path) = path_for_finish.as_ref() else {
                bail!("Download finish handler requires a file path");
            };
            Some(finish_handler(downloaded_path)?)
        } else {
            None
        }
    } else {
        None
    };

    if is_all_downloaded {
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
        let total_size = downloader.total_size.clone();
        let error = downloader.error.clone();
        let finished = downloader.finished;
        let finalizing = downloader.finalizing;
        let data = if finished
            && total_size
                .map(|total_size| total_size == downloaded_size)
                .unwrap_or(true)
            && downloader.path.is_none()
        {
            downloader.data.clone()
        } else {
            Vec::new()
        };
        let path = downloader.path.clone();
        let download_data = DownloadData {
            data,
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
                data: Vec::new(),
                path: Some(path.clone()),
                total_size: Some(8),
                signed_size: Some(8),
                auto_del_dur: None,
                has_finish_handler: true,
                downloaded_size: 0,
                error: None,
                finished: false,
                finalizing: false,
                tx_cancel: tx,
            },
        );

        let result = download_file_with_finish_handler(
            id.clone(),
            Some(path),
            None,
            Some(8),
            |_| unreachable!(),
        );

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
    fn unsigned_disk_download_allows_unknown_content_length() {
        assert_eq!(
            resolve_total_size(
                None,
                Some(8),
                Some(Path::new("/tmp/file")),
                "https://example.com/file",
            )
            .unwrap(),
            Some(8)
        );
        assert_eq!(
            resolve_total_size(
                Some(8),
                None,
                Some(Path::new("/tmp/file")),
                "https://example.com/file",
            )
            .unwrap(),
            Some(8)
        );
        assert_eq!(
            resolve_total_size(
                None,
                None,
                Some(Path::new("/tmp/file")),
                "https://example.com/file",
            )
            .unwrap(),
            None
        );
        assert!(resolve_total_size(None, None, None, "https://example.com/file").is_err());
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
        assert_eq!(data.total_size, Some(8));
        assert_eq!(data.path, Some(path.clone()));

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
        assert_eq!(data.path, Some(path.clone()));
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
        let (tx, _rx) = unbounded_channel();
        let token = next_downloader_token();
        let stale_token = token.wrapping_add(1);
        DOWNLOADERS.lock().unwrap().insert(
            id.clone(),
            Downloader {
                token,
                data: Vec::new(),
                path: None,
                total_size: Some(8),
                signed_size: Some(8),
                auto_del_dur: None,
                has_finish_handler: true,
                downloaded_size: 0,
                error: None,
                finished: false,
                finalizing: false,
                tx_cancel: tx,
            },
        );

        assert!(!set_download_finalizing(&id, stale_token));
        assert!(!finish_download(
            &id,
            stale_token,
            Some(PathBuf::from("stale.exe"))
        ));
        assert!(!fail_download(&id, stale_token, "stale error".to_owned()));

        let data = get_download_data(&id).unwrap();
        assert!(!data.finalizing);
        assert!(!data.finished);
        assert!(data.error.is_none());
        assert!(data.path.is_none());
        remove(&id);
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
                data: Vec::new(),
                path: None,
                total_size: Some(8),
                signed_size: Some(8),
                auto_del_dur: None,
                has_finish_handler: true,
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
    fn download_file_rejects_existing_url_with_different_contract() {
        let old_path = std::env::temp_dir().join(format!(
            "rustdesk-downloader-old-contract-test-{}",
            std::process::id()
        ));
        let new_path = std::env::temp_dir().join(format!(
            "rustdesk-downloader-new-contract-test-{}",
            std::process::id()
        ));
        let id = format!(
            "https://example.com/rustdesk-contract-{}.exe",
            std::process::id()
        );

        let (tx, _rx) = unbounded_channel();
        DOWNLOADERS.lock().unwrap().insert(
            id.clone(),
            Downloader {
                token: next_downloader_token(),
                data: Vec::new(),
                path: Some(old_path),
                total_size: Some(8),
                signed_size: Some(8),
                auto_del_dur: None,
                has_finish_handler: false,
                downloaded_size: 0,
                error: None,
                finished: false,
                finalizing: false,
                tx_cancel: tx,
            },
        );

        let result = download_file(id.clone(), Some(new_path), None, Some(8));

        assert!(result.is_err());
        remove(&id);
    }

    #[test]
    fn download_file_with_finish_handler_requires_path() {
        let id = format!(
            "https://example.com/rustdesk-finish-handler-no-path-{}.exe",
            std::process::id()
        );

        let result =
            download_file_with_finish_handler(id.clone(), None, None, Some(8), |_| unreachable!());

        assert!(result.is_err());
        assert!(get_download_data(&id).is_err());
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
                data: Vec::new(),
                path: Some(final_path.clone()),
                total_size: Some(8),
                signed_size: Some(8),
                auto_del_dur: None,
                has_finish_handler: false,
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
