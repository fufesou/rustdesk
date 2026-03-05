use crate::{
    common::CheckTestNatType,
    privacy_mode::PrivacyModeState,
    ui_interface::{get_local_option, set_local_option},
};
use bytes::Bytes;
use parity_tokio_ipc::{
    Connection as Conn, ConnectionClient as ConnClient, Endpoint, Incoming, SecurityAttributes,
};
use serde_derive::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU32, AtomicU64};
#[cfg(target_os = "linux")]
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(not(windows))]
use std::{fs::File, io::prelude::*};

#[cfg(all(feature = "flutter", feature = "plugin_framework"))]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use crate::plugin::ipc::Plugin;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub use clipboard::ClipboardFile;
use hbb_common::{
    allow_err, bail, bytes,
    bytes_codec::BytesCodec,
    config::{
        self,
        keys::OPTION_ALLOW_WEBSOCKET,
        Config, Config2,
    },
    futures::StreamExt as _,
    futures_util::sink::SinkExt,
    log, password_security as password, timeout,
    tokio::{
        self,
        io::{AsyncRead, AsyncWrite},
    },
    tokio_util::codec::Framed,
    ResultType,
};

use crate::{common::is_server, privacy_mode, rendezvous_mediator::RendezvousMediator};

// IPC actions here.
pub const IPC_ACTION_CLOSE: &str = "close";
pub const ENV_SERVICE_IPC_POSTFIX: &str = "RUSTDESK_SERVICE_IPC_POSTFIX";
#[cfg(any(target_os = "linux", target_os = "macos"))]
const SERVICE_IPC_POSTFIX_META_FILE: &str = "ipc_service_postfix";
#[cfg(target_os = "linux")]
const ACTIVE_UID_CACHE_TTL_SECS: u64 = 300;
#[cfg(target_os = "linux")]
const ACTIVE_UID_WARN_INTERVAL_SECS: u64 = 60;
pub static EXIT_RECV_CLOSE: AtomicBool = AtomicBool::new(true);
#[cfg(target_os = "linux")]
static LAST_ACTIVE_UID: AtomicU32 = AtomicU32::new(0);
#[cfg(target_os = "linux")]
static LAST_ACTIVE_UID_AT: AtomicU64 = AtomicU64::new(0);
#[cfg(target_os = "linux")]
static LAST_ACTIVE_UID_WARN_AT: AtomicU64 = AtomicU64::new(0);
#[cfg(target_os = "linux")]
static EXPECTED_SERVICE_PEER_UID: AtomicU32 = AtomicU32::new(0);

#[inline]
fn normalize_service_ipc_postfix(postfix: String) -> Option<String> {
    let normalized = postfix.trim().to_owned();
    let is_safe = normalized
        .chars()
        .all(|ch| ch == '_' || ch.is_ascii_alphanumeric());
    if config::is_service_ipc_postfix(&normalized) && is_safe {
        Some(normalized)
    } else {
        None
    }
}

#[inline]
fn resolve_service_ipc_postfix(
    env_postfix: Option<String>,
    metadata_postfix: Option<String>,
) -> String {
    let candidates = build_service_ipc_postfix_candidates(env_postfix, metadata_postfix);
    for candidate in candidates {
        if service_ipc_socket_exists(&candidate) {
            return candidate;
        }
    }
    crate::POSTFIX_SERVICE.to_owned()
}

#[inline]
fn build_service_ipc_postfix_candidates(
    env_postfix: Option<String>,
    metadata_postfix: Option<String>,
) -> Vec<String> {
    let env_candidate = env_postfix.and_then(normalize_service_ipc_postfix);
    let metadata_candidate = metadata_postfix.and_then(normalize_service_ipc_postfix);
    let mut candidates = Vec::with_capacity(3);
    if let Some(env) = env_candidate {
        candidates.push(env);
    }
    if let Some(metadata) = metadata_candidate {
        if !candidates.iter().any(|candidate| candidate == &metadata) {
            candidates.push(metadata);
        }
    }
    let default_postfix = crate::POSTFIX_SERVICE.to_owned();
    if !candidates
        .iter()
        .any(|candidate| candidate == crate::POSTFIX_SERVICE)
    {
        candidates.push(default_postfix);
    }
    candidates
}

#[inline]
fn service_ipc_postfix_candidates() -> Vec<String> {
    let env_postfix = std::env::var(ENV_SERVICE_IPC_POSTFIX).ok();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let metadata_postfix = read_service_ipc_postfix_metadata();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let metadata_postfix = None;
    build_service_ipc_postfix_candidates(env_postfix, metadata_postfix)
}

#[cfg(any(test, target_os = "macos", target_os = "linux"))]
#[inline]
fn is_allowed_service_peer_uid(peer_uid: u32, active_uid: Option<u32>) -> bool {
    peer_uid == 0 || active_uid.is_some_and(|uid| uid == peer_uid)
}

#[cfg(any(test, target_os = "macos", target_os = "linux"))]
#[inline]
fn resolve_active_uid(reported_uid: Option<u32>, console_uid: Option<u32>) -> Option<u32> {
    reported_uid.or(console_uid)
}

#[cfg(any(test, target_os = "linux"))]
#[inline]
fn terminal_count_candidate_uids_policy(
    effective_uid: u32,
    active_uid: Option<u32>,
    expected_uid: Option<u32>,
    discovered_uids: &[u32],
) -> Vec<u32> {
    if effective_uid != 0 {
        return vec![effective_uid];
    }
    let mut candidates = Vec::new();
    push_candidate_uid(&mut candidates, active_uid);
    push_candidate_uid(&mut candidates, expected_uid);
    for uid in discovered_uids.iter().copied() {
        push_candidate_uid(&mut candidates, Some(uid));
    }
    candidates.push(0);
    candidates
}

#[cfg(any(test, target_os = "linux"))]
#[inline]
fn should_discover_terminal_socket_uids(
    effective_uid: u32,
    active_uid: Option<u32>,
    expected_uid: Option<u32>,
) -> bool {
    effective_uid == 0 && active_uid.is_none() && expected_uid.is_none()
}

#[cfg(any(test, target_os = "linux"))]
#[inline]
fn push_candidate_uid(candidates: &mut Vec<u32>, uid: Option<u32>) {
    if let Some(uid) = uid.filter(|uid| *uid != 0) {
        if !candidates.iter().any(|candidate| *candidate == uid) {
            candidates.push(uid);
        }
    }
}

#[cfg(target_os = "linux")]
#[inline]
fn terminal_count_candidate_uids(effective_uid: u32, active_uid: Option<u32>) -> Vec<u32> {
    let expected_uid = expected_service_peer_uid();
    let discovered_uids = if should_discover_terminal_socket_uids(effective_uid, active_uid, expected_uid)
    {
        discover_terminal_socket_uids()
    } else {
        Vec::new()
    };
    terminal_count_candidate_uids_policy(
        effective_uid,
        active_uid,
        expected_uid,
        &discovered_uids,
    )
}

#[cfg(target_os = "linux")]
fn discover_terminal_socket_uids() -> Vec<u32> {
    let app_name = hbb_common::config::APP_NAME.read().unwrap().clone();
    let dir_prefix = format!("{app_name}-");
    let mut discovered_uids = Vec::new();
    let Ok(entries) = std::fs::read_dir("/tmp") else {
        return discovered_uids;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(&dir_prefix) {
            continue;
        }
        let uid_str = &name[dir_prefix.len()..];
        let Ok(uid) = uid_str.parse::<u32>() else {
            continue;
        };
        if uid == 0 {
            continue;
        }
        let ipc_path = entry.path().join("ipc");
        if ipc_path.exists() {
            push_candidate_uid(&mut discovered_uids, Some(uid));
        }
    }
    discovered_uids
}

#[cfg(target_os = "macos")]
#[inline]
fn console_owner_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/dev/console").ok().map(|metadata| metadata.uid())
}

#[cfg(target_os = "macos")]
#[inline]
fn active_uid() -> Option<u32> {
    let reported_uid = crate::platform::macos::get_active_userid().parse::<u32>().ok();
    resolve_active_uid(reported_uid, console_owner_uid())
}

#[cfg(target_os = "macos")]
#[inline]
fn active_uid_strict() -> Option<u32> {
    active_uid()
}

#[cfg(target_os = "linux")]
#[inline]
fn active_uid() -> Option<u32> {
    let reported_uid_raw = crate::platform::linux::get_active_userid();
    let reported_uid = reported_uid_raw.trim().parse::<u32>().ok();
    if let Some(uid) = reported_uid {
        remember_active_uid(uid);
        return resolve_active_uid(Some(uid), None);
    }
    if let Some(uid) = cached_active_uid() {
        log::debug!(
            "Falling back to cached active user uid on linux: uid={}, raw='{}'",
            uid,
            reported_uid_raw.trim()
        );
        return Some(uid);
    }
    log_active_uid_resolution_failure(&reported_uid_raw);
    None
}

#[cfg(target_os = "linux")]
#[inline]
fn active_uid_strict() -> Option<u32> {
    let reported_uid_raw = crate::platform::linux::get_active_userid();
    let reported_uid = reported_uid_raw.trim().parse::<u32>().ok();
    if let Some(uid) = reported_uid {
        remember_active_uid(uid);
        return resolve_active_uid(Some(uid), None);
    }
    log_active_uid_resolution_failure(&reported_uid_raw);
    None
}

#[cfg(target_os = "linux")]
#[inline]
fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(target_os = "linux")]
#[inline]
fn remember_active_uid(uid: u32) {
    if uid == 0 {
        return;
    }
    LAST_ACTIVE_UID.store(uid, Ordering::Relaxed);
    LAST_ACTIVE_UID_AT.store(unix_now_secs(), Ordering::Relaxed);
}

#[cfg(target_os = "linux")]
#[inline]
fn cached_active_uid() -> Option<u32> {
    let uid = LAST_ACTIVE_UID.load(Ordering::Relaxed);
    if uid == 0 {
        return None;
    }
    let ts = LAST_ACTIVE_UID_AT.load(Ordering::Relaxed);
    let now = unix_now_secs();
    if now.saturating_sub(ts) > ACTIVE_UID_CACHE_TTL_SECS {
        return None;
    }
    Some(uid)
}

#[cfg(target_os = "linux")]
#[inline]
pub fn set_expected_service_peer_uid(uid: Option<u32>) {
    let normalized_uid = uid.unwrap_or(0);
    let previous_uid = EXPECTED_SERVICE_PEER_UID.swap(normalized_uid, Ordering::Relaxed);
    if previous_uid != normalized_uid {
        LAST_ACTIVE_UID.store(0, Ordering::Relaxed);
        LAST_ACTIVE_UID_AT.store(0, Ordering::Relaxed);
    }
}

#[cfg(target_os = "linux")]
#[inline]
fn expected_service_peer_uid() -> Option<u32> {
    match EXPECTED_SERVICE_PEER_UID.load(Ordering::Relaxed) {
        0 => None,
        uid => Some(uid),
    }
}

#[cfg(target_os = "linux")]
#[inline]
fn resolve_service_auth_active_uid() -> (Option<u32>, bool) {
    let strict_uid = active_uid_strict();
    if strict_uid.is_some() {
        return (strict_uid, false);
    }
    let fallback_uid = expected_service_peer_uid().or_else(cached_active_uid);
    (fallback_uid, fallback_uid.is_some())
}

#[cfg(target_os = "linux")]
fn log_active_uid_resolution_failure(raw_uid: &str) {
    let now = unix_now_secs();
    let last = LAST_ACTIVE_UID_WARN_AT.load(Ordering::Relaxed);
    let should_warn = now.saturating_sub(last) >= ACTIVE_UID_WARN_INTERVAL_SECS
        && LAST_ACTIVE_UID_WARN_AT
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok();
    let trimmed = raw_uid.trim();
    if should_warn {
        if trimmed.is_empty() {
            log::warn!("Failed to resolve active user uid on linux: active uid is empty");
        } else {
            log::warn!("Failed to parse active user uid on linux: '{}'", trimmed);
        }
    } else if trimmed.is_empty() {
        log::debug!("Failed to resolve active user uid on linux: active uid is empty");
    } else {
        log::debug!("Failed to parse active user uid on linux: '{}'", trimmed);
    }
}

#[cfg(target_os = "linux")]
#[inline]
fn linux_ipc_path_for_uid(uid: u32, postfix: &str) -> String {
    let app_name = hbb_common::config::APP_NAME.read().unwrap().clone();
    format!("/tmp/{app_name}-{uid}/ipc{postfix}")
}

#[inline]
fn service_ipc_socket_exists(postfix: &str) -> bool {
    let socket_path = Config::ipc_path(postfix);
    Path::new(&socket_path).exists()
}

#[cfg(not(windows))]
#[inline]
fn ordered_service_ipc_postfix_candidates(candidates: Vec<String>) -> Vec<String> {
    let mut ready_candidates = Vec::new();
    let mut stale_candidates = Vec::new();
    for postfix in candidates {
        if service_ipc_socket_exists(&postfix) {
            ready_candidates.push(postfix);
        } else {
            stale_candidates.push(postfix);
        }
    }
    ready_candidates.extend(stale_candidates);
    ready_candidates
}

#[cfg(windows)]
#[inline]
fn ordered_service_ipc_postfix_candidates(candidates: Vec<String>) -> Vec<String> {
    candidates
}

#[inline]
fn connect_service_attempt_timeout(remaining_ms: u64, attempts_left: usize) -> u64 {
    if remaining_ms == 0 {
        return 0;
    }
    if attempts_left <= 1 {
        return remaining_ms;
    }
    std::cmp::max(1, remaining_ms / attempts_left as u64)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[inline]
fn service_ipc_metadata_path() -> std::path::PathBuf {
    let mut socket_path = std::path::PathBuf::from(Config::ipc_path(crate::POSTFIX_SERVICE));
    socket_path.pop();
    socket_path.push(SERVICE_IPC_POSTFIX_META_FILE);
    socket_path
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[inline]
fn read_service_ipc_postfix_metadata() -> Option<String> {
    let metadata_path = service_ipc_metadata_path();
    let content = std::fs::read_to_string(metadata_path).ok()?;
    let postfix = normalize_service_ipc_postfix(content)?;
    let socket_path = Config::ipc_path(&postfix);
    if std::path::Path::new(&socket_path).exists() {
        Some(postfix)
    } else {
        None
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn write_service_ipc_postfix_metadata(postfix: &str) -> ResultType<()> {
    use std::io::{Error, ErrorKind};
    use std::os::unix::fs::PermissionsExt;

    let normalized = normalize_service_ipc_postfix(postfix.to_owned()).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("invalid service ipc postfix: {postfix}"),
        )
    })?;
    let metadata_path = service_ipc_metadata_path();
    std::fs::write(&metadata_path, format!("{normalized}\n"))?;
    std::fs::set_permissions(&metadata_path, std::fs::Permissions::from_mode(0o0644))?;
    Ok(())
}

#[cfg(not(windows))]
#[inline]
fn expected_ipc_parent_mode(postfix: &str) -> u32 {
    if config::is_service_ipc_postfix(postfix) {
        0o0711
    } else {
        0o0700
    }
}

#[cfg(not(windows))]
fn ensure_ipc_parent_exists(parent_dir: &Path, postfix: &str) -> ResultType<()> {
    use std::os::unix::fs::PermissionsExt;

    if !parent_dir.exists() {
        std::fs::create_dir_all(parent_dir)?;
        let mode = expected_ipc_parent_mode(postfix);
        std::fs::set_permissions(parent_dir, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn ensure_secure_ipc_parent_dir(path: &str, postfix: &str) -> ResultType<()> {
    use std::ffi::CString;
    use std::io::{Error, ErrorKind};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let parent_dir = Path::new(path)
        .parent()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, format!("invalid ipc path: {path}")))?;
    ensure_ipc_parent_exists(parent_dir, postfix)?;
    let metadata = std::fs::symlink_metadata(parent_dir)?;
    if metadata.file_type().is_symlink() {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            format!("ipc parent is symlink: {}", parent_dir.display()),
        )
        .into());
    }
    if !metadata.file_type().is_dir() {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            format!("ipc parent is not directory: {}", parent_dir.display()),
        )
        .into());
    }

    let expected_uid = unsafe { hbb_common::libc::geteuid() as u32 };
    let mut owner_uid = metadata.uid();
    if owner_uid != expected_uid && expected_uid == 0 && config::is_service_ipc_postfix(postfix) {
        let parent_c = CString::new(parent_dir.as_os_str().to_string_lossy().as_ref())?;
        let rc = unsafe {
            hbb_common::libc::chown(
                parent_c.as_ptr(),
                expected_uid,
                hbb_common::libc::gid_t::MAX,
            )
        };
        if rc == 0 {
            owner_uid = std::fs::symlink_metadata(parent_dir)?.uid();
        }
    }
    if owner_uid != expected_uid {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            format!(
                "unsafe ipc parent owner, expected uid {expected_uid}, got {owner_uid}: {}",
                parent_dir.display()
            ),
        )
        .into());
    }

    let expected_mode = expected_ipc_parent_mode(postfix);
    let current_mode = metadata.mode() & 0o777;
    if current_mode != expected_mode {
        std::fs::set_permissions(parent_dir, std::fs::Permissions::from_mode(expected_mode))?;
    }
    Ok(())
}

#[inline]
pub fn service_ipc_postfix() -> String {
    let env_postfix = std::env::var(ENV_SERVICE_IPC_POSTFIX).ok();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let metadata_postfix = read_service_ipc_postfix_metadata();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let metadata_postfix = None;
    let resolved = resolve_service_ipc_postfix(env_postfix.clone(), metadata_postfix.clone());
    if env_postfix
        .as_ref()
        .and_then(|value| normalize_service_ipc_postfix(value.clone()))
        .is_some_and(|value| value == resolved)
    {
        return resolved;
    }
    if metadata_postfix
        .as_ref()
        .is_some_and(|value| value.trim() == resolved)
    {
        log::debug!("Resolved service ipc postfix from metadata file");
    }
    resolved
}

pub async fn connect_service(ms_timeout: u64) -> ResultType<ConnectionTmpl<ConnClient>> {
    let mut last_err = None;
    let ordered_candidates = ordered_service_ipc_postfix_candidates(service_ipc_postfix_candidates());
    if ordered_candidates.is_empty() {
        bail!("No service ipc postfix candidates available");
    }
    let start = std::time::Instant::now();
    let total_attempts = ordered_candidates.len();
    for (index, postfix) in ordered_candidates.into_iter().enumerate() {
        let elapsed_ms = start.elapsed().as_millis() as u64;
        let remaining_ms = ms_timeout.saturating_sub(elapsed_ms);
        if remaining_ms == 0 {
            break;
        }
        let attempts_left = total_attempts.saturating_sub(index);
        let per_attempt_timeout = connect_service_attempt_timeout(remaining_ms, attempts_left);
        match connect(per_attempt_timeout, &postfix).await {
            Ok(conn) => return Ok(conn),
            Err(err) => {
                log::debug!(
                    "Failed to connect service ipc with postfix '{}' ({}ms): {}",
                    postfix,
                    per_attempt_timeout,
                    err
                );
                last_err = Some(err);
            }
        }
    }
    if let Some(err) = last_err {
        Err(err)
    } else {
        bail!("Timed out while connecting to service ipc candidates");
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t", content = "c")]
pub enum FS {
    ReadEmptyDirs {
        dir: String,
        include_hidden: bool,
    },
    ReadDir {
        dir: String,
        include_hidden: bool,
    },
    RemoveDir {
        path: String,
        id: i32,
        recursive: bool,
    },
    RemoveFile {
        path: String,
        id: i32,
        file_num: i32,
    },
    CreateDir {
        path: String,
        id: i32,
    },
    NewWrite {
        path: String,
        id: i32,
        file_num: i32,
        files: Vec<(String, u64)>,
        overwrite_detection: bool,
        total_size: u64,
        conn_id: i32,
    },
    CancelWrite {
        id: i32,
    },
    WriteBlock {
        id: i32,
        file_num: i32,
        data: Bytes,
        compressed: bool,
    },
    WriteDone {
        id: i32,
        file_num: i32,
    },
    WriteError {
        id: i32,
        file_num: i32,
        err: String,
    },
    WriteOffset {
        id: i32,
        file_num: i32,
        offset_blk: u32,
    },
    CheckDigest {
        id: i32,
        file_num: i32,
        file_size: u64,
        last_modified: u64,
        is_upload: bool,
        is_resume: bool,
    },
    SendConfirm(Vec<u8>),
    Rename {
        id: i32,
        path: String,
        new_name: String,
    },
    // CM-side file reading operations (Windows only)
    // These enable Connection Manager to read files and stream them back to Connection
    ReadFile {
        path: String,
        id: i32,
        file_num: i32,
        include_hidden: bool,
        conn_id: i32,
        overwrite_detection: bool,
    },
    CancelRead {
        id: i32,
        conn_id: i32,
    },
    SendConfirmForRead {
        id: i32,
        file_num: i32,
        skip: bool,
        offset_blk: u32,
        conn_id: i32,
    },
    ReadAllFiles {
        path: String,
        id: i32,
        include_hidden: bool,
        conn_id: i32,
    },
}

#[cfg(target_os = "windows")]
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t")]
pub struct ClipboardNonFile {
    pub compress: bool,
    pub content: bytes::Bytes,
    pub content_len: usize,
    pub next_raw: bool,
    pub width: i32,
    pub height: i32,
    // message.proto: ClipboardFormat
    pub format: i32,
    pub special_name: String,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t", content = "c")]
pub enum DataKeyboard {
    Sequence(String),
    KeyDown(enigo::Key),
    KeyUp(enigo::Key),
    KeyClick(enigo::Key),
    GetKeyState(enigo::Key),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t", content = "c")]
pub enum DataKeyboardResponse {
    GetKeyState(bool),
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t", content = "c")]
pub enum DataMouse {
    MoveTo(i32, i32),
    MoveRelative(i32, i32),
    Down(enigo::MouseButton),
    Up(enigo::MouseButton),
    Click(enigo::MouseButton),
    ScrollX(i32),
    ScrollY(i32),
    Refresh,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t", content = "c")]
pub enum DataControl {
    Resolution {
        minx: i32,
        maxx: i32,
        miny: i32,
        maxy: i32,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t", content = "c")]
pub enum DataPortableService {
    Ping,
    Pong,
    ConnCount(Option<usize>),
    Mouse((Vec<u8>, i32, String, u32, bool, bool)),
    Pointer((Vec<u8>, i32)),
    Key(Vec<u8>),
    RequestStart,
    WillClose,
    CmShowElevation(bool),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "t", content = "c")]
pub enum Data {
    Login {
        id: i32,
        is_file_transfer: bool,
        is_view_camera: bool,
        is_terminal: bool,
        peer_id: String,
        name: String,
        avatar: String,
        authorized: bool,
        port_forward: String,
        keyboard: bool,
        clipboard: bool,
        audio: bool,
        file: bool,
        file_transfer_enabled: bool,
        restart: bool,
        recording: bool,
        block_input: bool,
        from_switch: bool,
    },
    ChatMessage {
        text: String,
    },
    SwitchPermission {
        name: String,
        enabled: bool,
    },
    SystemInfo(Option<String>),
    ClickTime(i64),
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    MouseMoveTime(i64),
    Authorize,
    Close,
    #[cfg(windows)]
    SAS,
    UserSid(Option<u32>),
    OnlineStatus(Option<(i64, bool)>),
    Config((String, Option<String>)),
    Options(Option<HashMap<String, String>>),
    NatType(Option<i32>),
    ConfirmedKey(Option<(Vec<u8>, Vec<u8>)>),
    RawMessage(Vec<u8>),
    Socks(Option<config::Socks5Server>),
    FS(FS),
    Test,
    SyncConfig(Option<Box<(Config, Config2)>>),
    #[cfg(target_os = "windows")]
    ClipboardFile(ClipboardFile),
    ClipboardFileEnabled(bool),
    #[cfg(target_os = "windows")]
    ClipboardNonFile(Option<(String, Vec<ClipboardNonFile>)>),
    PrivacyModeState((i32, PrivacyModeState, String)),
    TestRendezvousServer,
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    Keyboard(DataKeyboard),
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    KeyboardResponse(DataKeyboardResponse),
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    Mouse(DataMouse),
    Control(DataControl),
    Theme(String),
    Language(String),
    Empty,
    Disconnected,
    DataPortableService(DataPortableService),
    SwitchSidesRequest(String),
    SwitchSidesBack,
    UrlLink(String),
    VoiceCallIncoming,
    StartVoiceCall,
    VoiceCallResponse(bool),
    CloseVoiceCall(String),
    #[cfg(all(feature = "flutter", feature = "plugin_framework"))]
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    Plugin(Plugin),
    #[cfg(windows)]
    SyncWinCpuUsage(Option<f64>),
    FileTransferLog((String, String)),
    #[cfg(windows)]
    ControlledSessionCount(usize),
    CmErr(String),
    // CM-side file reading responses (Windows only)
    // These are sent from CM back to Connection when CM handles file reading
    /// Response to ReadFile: contains initial file list or error
    ReadJobInitResult {
        id: i32,
        file_num: i32,
        include_hidden: bool,
        conn_id: i32,
        /// Serialized protobuf bytes of FileDirectory, or error string
        result: Result<Vec<u8>, String>,
    },
    /// File data block read by CM.
    ///
    /// The actual data is sent separately via `send_raw()` after this message to avoid
    /// JSON encoding overhead for large binary data. This mirrors the `WriteBlock` pattern.
    ///
    /// **Protocol:**
    /// - Sender: `send(FileBlockFromCM{...})` then `send_raw(data)`
    /// - Receiver: `next()` returns `FileBlockFromCM`, then `next_raw()` returns data bytes
    ///
    /// **Note on empty data (e.g., empty files):**
    /// Empty data is supported. The IPC connection uses `BytesCodec` with `raw=false` (default),
    /// which prefixes each frame with a length header. So `send_raw(Bytes::new())` sends a
    /// 1-byte frame (length=0), and `next_raw()` correctly returns an empty `BytesMut`.
    /// See `libs/hbb_common/src/bytes_codec.rs` test `test_codec2` for verification.
    FileBlockFromCM {
        id: i32,
        file_num: i32,
        /// Data is sent separately via `send_raw()` to avoid JSON encoding overhead.
        /// This field is skipped during serialization; sender must call `send_raw()` after sending.
        /// Receiver must call `next_raw()` and populate this field manually.
        #[serde(skip)]
        data: bytes::Bytes,
        compressed: bool,
        conn_id: i32,
    },
    /// File read completed successfully
    FileReadDone {
        id: i32,
        file_num: i32,
        conn_id: i32,
    },
    /// File read failed with error
    FileReadError {
        id: i32,
        file_num: i32,
        err: String,
        conn_id: i32,
    },
    /// Digest info from CM for overwrite detection
    FileDigestFromCM {
        id: i32,
        file_num: i32,
        last_modified: u64,
        file_size: u64,
        is_resume: bool,
        conn_id: i32,
    },
    /// Response to ReadAllFiles: recursive directory listing
    AllFilesResult {
        id: i32,
        conn_id: i32,
        path: String,
        /// Serialized protobuf bytes of FileDirectory, or error string
        result: Result<Vec<u8>, String>,
    },
    CheckHwcodec,
    #[cfg(feature = "flutter")]
    VideoConnCount(Option<usize>),
    // Although the key is not necessary, it is used to avoid hardcoding the key.
    WaylandScreencastRestoreToken((String, String)),
    HwCodecConfig(Option<String>),
    RemoveTrustedDevices(Vec<Bytes>),
    ClearTrustedDevices,
    #[cfg(all(target_os = "windows", feature = "flutter"))]
    PrinterData(Vec<u8>),
    InstallOption(Option<(String, String)>),
    #[cfg(all(
        feature = "flutter",
        not(any(target_os = "android", target_os = "ios"))
    ))]
    ControllingSessionCount(usize),
    #[cfg(target_os = "linux")]
    TerminalSessionCount(usize),
    #[cfg(target_os = "windows")]
    PortForwardSessionCount(Option<usize>),
    SocksWs(Option<Box<(Option<config::Socks5Server>, String)>>),
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    Whiteboard((String, crate::whiteboard::CustomEvent)),
    ControlPermissionsRemoteModify(Option<bool>),
    #[cfg(target_os = "windows")]
    FileTransferEnabledState(Option<bool>),
}

#[tokio::main(flavor = "current_thread")]
pub async fn start(postfix: &str) -> ResultType<()> {
    let mut incoming = new_listener(postfix).await?;
    loop {
        if let Some(result) = incoming.next().await {
            match result {
                Ok(stream) => {
                    let mut stream = Connection::new(stream);
                    let postfix = postfix.to_owned();
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    if config::is_service_ipc_postfix(&postfix) {
                        let (authorized, peer_uid, active_uid) =
                            stream.service_authorization_status();
                        if !authorized {
                            log::warn!(
                                "Rejected unauthorized connection on protected ipc_service channel: postfix={}, peer_uid={:?}, active_uid={:?}",
                                postfix,
                                peer_uid,
                                active_uid
                            );
                            continue;
                        }
                    }
                    tokio::spawn(async move {
                        loop {
                            match stream.next().await {
                                Err(err) => {
                                    log::trace!("ipc '{}' connection closed: {}", postfix, err);
                                    break;
                                }
                                Ok(Some(data)) => {
                                    handle(data, &mut stream, &postfix).await;
                                }
                                _ => {}
                            }
                        }
                    });
                }
                Err(err) => {
                    log::error!("Couldn't get client: {:?}", err);
                }
            }
        }
    }
}

pub async fn new_listener(postfix: &str) -> ResultType<Incoming> {
    let path = Config::ipc_path(postfix);
    #[cfg(not(windows))]
    ensure_secure_ipc_parent_dir(&path, postfix)?;
    #[cfg(not(any(windows, target_os = "android", target_os = "ios")))]
    check_pid(postfix).await;
    let mut endpoint = Endpoint::new(path.clone());
    match SecurityAttributes::allow_everyone_create() {
        Ok(attr) => endpoint.set_security_attributes(attr),
        Err(err) => log::error!("Failed to set ipc{} security: {}", postfix, err),
    };
    match endpoint.incoming() {
        Ok(incoming) => {
            if config::is_service_ipc_postfix(postfix) {
                log::info!("Started protected ipc service server");
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                if let Err(err) = write_service_ipc_postfix_metadata(postfix) {
                    log::error!("Failed to write service ipc postfix metadata: {}", err);
                    std::fs::remove_file(&path).ok();
                    return Err(err);
                }
            } else {
                log::info!("Started ipc{} server at path: {}", postfix, &path);
            }
            #[cfg(not(windows))]
            {
                use std::os::unix::fs::PermissionsExt;
                let socket_mode = if config::is_service_ipc_postfix(postfix) {
                    0o0666
                } else {
                    0o0600
                };
                if let Err(err) =
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(socket_mode))
                {
                    log::error!("Failed to set permissions on ipc{} socket: {}", postfix, err);
                    std::fs::remove_file(&path).ok();
                    return Err(err.into());
                }
                write_pid(postfix);
            }
            Ok(incoming)
        }
        Err(err) => {
            log::error!(
                "Failed to start ipc{} server at path {}: {}",
                postfix,
                path,
                err
            );
            Err(err.into())
        }
    }
}

pub struct CheckIfRestart {
    stop_service: String,
    rendezvous_servers: Vec<String>,
    audio_input: String,
    voice_call_input: String,
    ws: String,
    disable_udp: String,
    allow_insecure_tls_fallback: String,
    api_server: String,
}

impl CheckIfRestart {
    pub fn new() -> CheckIfRestart {
        CheckIfRestart {
            stop_service: Config::get_option("stop-service"),
            rendezvous_servers: Config::get_rendezvous_servers(),
            audio_input: Config::get_option("audio-input"),
            voice_call_input: Config::get_option("voice-call-input"),
            ws: Config::get_option(OPTION_ALLOW_WEBSOCKET),
            disable_udp: Config::get_option(config::keys::OPTION_DISABLE_UDP),
            allow_insecure_tls_fallback: Config::get_option(
                config::keys::OPTION_ALLOW_INSECURE_TLS_FALLBACK,
            ),
            api_server: Config::get_option("api-server"),
        }
    }
}
impl Drop for CheckIfRestart {
    fn drop(&mut self) {
        // If https proxy is used, we need to restart rendezvous mediator.
        // No need to check if https proxy is used, because this option does not change frequently
        // and restarting mediator is safe even https proxy is not used.
        let allow_insecure_tls_fallback_changed = self.allow_insecure_tls_fallback
            != Config::get_option(config::keys::OPTION_ALLOW_INSECURE_TLS_FALLBACK);
        if allow_insecure_tls_fallback_changed
            || self.stop_service != Config::get_option("stop-service")
            || self.rendezvous_servers != Config::get_rendezvous_servers()
            || self.ws != Config::get_option(OPTION_ALLOW_WEBSOCKET)
            || self.disable_udp != Config::get_option(config::keys::OPTION_DISABLE_UDP)
            || self.api_server != Config::get_option("api-server")
        {
            if allow_insecure_tls_fallback_changed {
                hbb_common::tls::reset_tls_cache();
            }
            RendezvousMediator::restart();
        }
        if self.audio_input != Config::get_option("audio-input") {
            crate::audio_service::restart();
        }
        if self.voice_call_input != Config::get_option("voice-call-input") {
            crate::audio_service::set_voice_call_input_device(
                Some(Config::get_option("voice-call-input")),
                true,
            )
        }
    }
}

async fn handle(data: Data, stream: &mut Connection, postfix: &str) {
    if config::is_service_ipc_postfix(postfix) && !matches!(&data, Data::SyncConfig(_)) {
        log::warn!(
            "Rejected non-sync data on protected ipc_service channel: {:?}",
            std::mem::discriminant(&data)
        );
        return;
    }
    match data {
        Data::SystemInfo(_) => {
            let info = format!(
                "log_path: {}, config: {}, username: {}",
                Config::log_path().to_str().unwrap_or(""),
                Config::file().to_str().unwrap_or(""),
                crate::username(),
            );
            allow_err!(stream.send(&Data::SystemInfo(Some(info))).await);
        }
        Data::ClickTime(_) => {
            let t = crate::server::CLICK_TIME.load(Ordering::SeqCst);
            allow_err!(stream.send(&Data::ClickTime(t)).await);
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        Data::MouseMoveTime(_) => {
            let t = crate::server::MOUSE_MOVE_TIME.load(Ordering::SeqCst);
            allow_err!(stream.send(&Data::MouseMoveTime(t)).await);
        }
        Data::Close => {
            log::info!("Receive close message");
            if EXIT_RECV_CLOSE.load(Ordering::SeqCst) {
                #[cfg(not(target_os = "android"))]
                crate::server::input_service::fix_key_down_timeout_at_exit();
                if is_server() {
                    let _ = privacy_mode::turn_off_privacy(0, Some(PrivacyModeState::OffByPeer));
                }
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                if crate::is_main() {
                    // below part is for main windows can be reopen during rustdesk installation and installing service from UI
                    // this make new ipc server (domain socket) can be created.
                    std::fs::remove_file(&Config::ipc_path("")).ok();
                    #[cfg(target_os = "linux")]
                    {
                        hbb_common::sleep((crate::platform::SERVICE_INTERVAL * 2) as f32 / 1000.0)
                            .await;
                        // https://github.com/rustdesk/rustdesk/discussions/9254
                        crate::run_me::<&str>(vec!["--no-server"]).ok();
                    }
                    #[cfg(target_os = "macos")]
                    {
                        // our launchagent interval is 1 second
                        hbb_common::sleep(1.5).await;
                        std::process::Command::new("open")
                            .arg("-n")
                            .arg(&format!("/Applications/{}.app", crate::get_app_name()))
                            .spawn()
                            .ok();
                    }
                    // leave above open a little time
                    hbb_common::sleep(0.3).await;
                    // in case below exit failed
                    crate::platform::quit_gui();
                }
                std::process::exit(-1); // to make sure --server luauchagent process can restart because SuccessfulExit used
            }
        }
        Data::OnlineStatus(_) => {
            let x = config::get_online_state();
            let confirmed = Config::get_key_confirmed();
            allow_err!(stream.send(&Data::OnlineStatus(Some((x, confirmed)))).await);
        }
        Data::ConfirmedKey(None) => {
            let out = if Config::get_key_confirmed() {
                Some(Config::get_key_pair())
            } else {
                None
            };
            allow_err!(stream.send(&Data::ConfirmedKey(out)).await);
        }
        Data::Socks(s) => match s {
            None => {
                allow_err!(stream.send(&Data::Socks(Config::get_socks())).await);
            }
            Some(data) => {
                let _nat = CheckTestNatType::new();
                if data.proxy.is_empty() {
                    Config::set_socks(None);
                } else {
                    Config::set_socks(Some(data));
                }
                RendezvousMediator::restart();
                log::info!("socks updated");
            }
        },
        Data::SocksWs(s) => match s {
            None => {
                allow_err!(
                    stream
                        .send(&Data::SocksWs(Some(Box::new((
                            Config::get_socks(),
                            Config::get_option(OPTION_ALLOW_WEBSOCKET)
                        )))))
                        .await
                );
            }
            _ => {}
        },
        #[cfg(feature = "flutter")]
        Data::VideoConnCount(None) => {
            let n = crate::server::AUTHED_CONNS
                .lock()
                .unwrap()
                .iter()
                .filter(|x| x.conn_type == crate::server::AuthConnType::Remote)
                .count();
            allow_err!(stream.send(&Data::VideoConnCount(Some(n))).await);
        }
        Data::Config((name, value)) => match value {
            None => {
                let value;
                if name == "id" {
                    value = Some(Config::get_id());
                } else if name == "temporary-password" {
                    value = Some(password::temporary_password());
                } else if name == "permanent-password" {
                    value = Some(Config::get_permanent_password());
                } else if name == "salt" {
                    value = Some(Config::get_salt());
                } else if name == "rendezvous_server" {
                    value = Some(format!(
                        "{},{}",
                        Config::get_rendezvous_server(),
                        Config::get_rendezvous_servers().join(",")
                    ));
                } else if name == "rendezvous_servers" {
                    value = Some(Config::get_rendezvous_servers().join(","));
                } else if name == "fingerprint" {
                    value = if Config::get_key_confirmed() {
                        Some(crate::common::pk_to_fingerprint(Config::get_key_pair().1))
                    } else {
                        None
                    };
                } else if name == "hide_cm" {
                    value = if crate::hbbs_http::sync::is_pro() || crate::common::is_custom_client()
                    {
                        Some(hbb_common::password_security::hide_cm().to_string())
                    } else {
                        None
                    };
                } else if name == "voice-call-input" {
                    value = crate::audio_service::get_voice_call_input_device();
                } else if name == "unlock-pin" {
                    value = Some(Config::get_unlock_pin());
                } else if name == "trusted-devices" {
                    value = Some(Config::get_trusted_devices_json());
                } else {
                    value = None;
                }
                allow_err!(stream.send(&Data::Config((name, value))).await);
            }
            Some(value) => {
                if name == "id" {
                    Config::set_key_confirmed(false);
                    Config::set_id(&value);
                } else if name == "temporary-password" {
                    password::update_temporary_password();
                } else if name == "permanent-password" {
                    Config::set_permanent_password(&value);
                } else if name == "salt" {
                    Config::set_salt(&value);
                } else if name == "voice-call-input" {
                    crate::audio_service::set_voice_call_input_device(Some(value), true);
                } else if name == "unlock-pin" {
                    Config::set_unlock_pin(&value);
                } else {
                    return;
                }
                log::info!("{} updated", name);
            }
        },
        Data::Options(value) => match value {
            None => {
                let v = Config::get_options();
                allow_err!(stream.send(&Data::Options(Some(v))).await);
            }
            Some(value) => {
                let _chk = CheckIfRestart::new();
                let _nat = CheckTestNatType::new();
                if let Some(v) = value.get("privacy-mode-impl-key") {
                    crate::privacy_mode::switch(v);
                }
                Config::set_options(value);
                allow_err!(stream.send(&Data::Options(None)).await);
            }
        },
        Data::NatType(_) => {
            let t = Config::get_nat_type();
            allow_err!(stream.send(&Data::NatType(Some(t))).await);
        }
        Data::SyncConfig(Some(configs)) => {
            let (config, config2) = *configs;
            let _chk = CheckIfRestart::new();
            Config::set(config);
            Config2::set(config2);
            allow_err!(stream.send(&Data::SyncConfig(None)).await);
        }
        Data::SyncConfig(None) => {
            allow_err!(
                stream
                    .send(&Data::SyncConfig(Some(
                        (Config::get(), Config2::get()).into()
                    )))
                    .await
            );
        }
        #[cfg(windows)]
        Data::SyncWinCpuUsage(None) => {
            allow_err!(
                stream
                    .send(&Data::SyncWinCpuUsage(
                        hbb_common::platform::windows::cpu_uage_one_minute()
                    ))
                    .await
            );
        }
        Data::TestRendezvousServer => {
            crate::test_rendezvous_server();
        }
        Data::SwitchSidesRequest(id) => {
            let uuid = uuid::Uuid::new_v4();
            crate::server::insert_switch_sides_uuid(id, uuid.clone());
            allow_err!(
                stream
                    .send(&Data::SwitchSidesRequest(uuid.to_string()))
                    .await
            );
        }
        #[cfg(all(feature = "flutter", feature = "plugin_framework"))]
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        Data::Plugin(plugin) => crate::plugin::ipc::handle_plugin(plugin, stream).await,
        #[cfg(windows)]
        Data::ControlledSessionCount(_) => {
            allow_err!(
                stream
                    .send(&Data::ControlledSessionCount(
                        crate::Connection::alive_conns().len()
                    ))
                    .await
            );
        }
        #[cfg(all(
            feature = "flutter",
            not(any(target_os = "android", target_os = "ios"))
        ))]
        Data::ControllingSessionCount(count) => {
            crate::updater::update_controlling_session_count(count);
        }
        #[cfg(target_os = "linux")]
        Data::TerminalSessionCount(_) => {
            let count = crate::terminal_service::get_terminal_session_count(true);
            allow_err!(stream.send(&Data::TerminalSessionCount(count)).await);
        }
        #[cfg(feature = "hwcodec")]
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        Data::CheckHwcodec => {
            scrap::hwcodec::start_check_process();
        }
        #[cfg(feature = "hwcodec")]
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        Data::HwCodecConfig(c) => {
            match c {
                None => {
                    let v = match scrap::hwcodec::HwCodecConfig::get_set_value() {
                        Some(v) => Some(serde_json::to_string(&v).unwrap_or_default()),
                        None => None,
                    };
                    allow_err!(stream.send(&Data::HwCodecConfig(v)).await);
                }
                Some(v) => {
                    // --server and portable
                    scrap::hwcodec::HwCodecConfig::set(v);
                }
            }
        }
        Data::WaylandScreencastRestoreToken((key, value)) => {
            let v = if value == "get" {
                let opt = get_local_option(key.clone());
                #[cfg(not(target_os = "linux"))]
                {
                    Some(opt)
                }
                #[cfg(target_os = "linux")]
                {
                    let v = if opt.is_empty() {
                        if scrap::wayland::pipewire::is_rdp_session_hold() {
                            "fake token".to_string()
                        } else {
                            "".to_owned()
                        }
                    } else {
                        opt
                    };
                    Some(v)
                }
            } else if value == "clear" {
                set_local_option(key.clone(), "".to_owned());
                #[cfg(target_os = "linux")]
                scrap::wayland::pipewire::close_session();
                Some("".to_owned())
            } else {
                None
            };
            if let Some(v) = v {
                allow_err!(
                    stream
                        .send(&Data::WaylandScreencastRestoreToken((key, v)))
                        .await
                );
            }
        }
        Data::RemoveTrustedDevices(v) => {
            Config::remove_trusted_devices(&v);
        }
        Data::ClearTrustedDevices => {
            Config::clear_trusted_devices();
        }
        Data::InstallOption(opt) => match opt {
            Some((_k, _v)) => {
                #[cfg(target_os = "windows")]
                if let Err(e) = crate::platform::windows::update_install_option(&_k, &_v) {
                    log::error!(
                        "Failed to update install option \"{}\" to \"{}\", error: {}",
                        &_k,
                        &_v,
                        e
                    );
                }
            }
            None => {
                // `None` is usually used to get values.
                // This branch is left blank for unification and further use.
            }
        },
        #[cfg(target_os = "windows")]
        Data::PortForwardSessionCount(c) => match c {
            None => {
                let count = crate::server::AUTHED_CONNS
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|c| c.conn_type == crate::server::AuthConnType::PortForward)
                    .count();
                allow_err!(
                    stream
                        .send(&Data::PortForwardSessionCount(Some(count)))
                        .await
                );
            }
            _ => {
                // Port forward session count is only a get value.
            }
        },
        Data::ControlPermissionsRemoteModify(_) => {
            use hbb_common::rendezvous_proto::control_permissions::Permission;
            let state =
                crate::server::get_control_permission_state(Permission::remote_modify, true);
            allow_err!(
                stream
                    .send(&Data::ControlPermissionsRemoteModify(state))
                    .await
            );
        }
        #[cfg(target_os = "windows")]
        Data::FileTransferEnabledState(_) => {
            use hbb_common::rendezvous_proto::control_permissions::Permission;
            let state = crate::server::get_control_permission_state(Permission::file, false);
            let enabled = state.unwrap_or_else(|| {
                crate::server::Connection::is_permission_enabled_locally(
                    config::keys::OPTION_ENABLE_FILE_TRANSFER,
                )
            });
            allow_err!(
                stream
                    .send(&Data::FileTransferEnabledState(Some(enabled)))
                    .await
            );
        }
        _ => {}
    }
}

pub async fn connect(ms_timeout: u64, postfix: &str) -> ResultType<ConnectionTmpl<ConnClient>> {
    let path = Config::ipc_path(postfix);
    let client = timeout(ms_timeout, Endpoint::connect(&path)).await??;
    Ok(ConnectionTmpl::new(client))
}

#[cfg(target_os = "linux")]
#[tokio::main(flavor = "current_thread")]
pub async fn start_pa() {
    use crate::audio_service::AUDIO_DATA_SIZE_U8;

    match new_listener("_pa").await {
        Ok(mut incoming) => {
            loop {
                if let Some(result) = incoming.next().await {
                    match result {
                        Ok(stream) => {
                            let mut stream = Connection::new(stream);
                            let mut device: String = "".to_owned();
                            if let Some(Ok(Some(Data::Config((_, Some(x)))))) =
                                stream.next_timeout2(1000).await
                            {
                                device = x;
                            }
                            if !device.is_empty() {
                                device = crate::platform::linux::get_pa_source_name(&device);
                            }
                            if device.is_empty() {
                                device = crate::platform::linux::get_pa_monitor();
                            }
                            if device.is_empty() {
                                continue;
                            }
                            let spec = pulse::sample::Spec {
                                format: pulse::sample::Format::F32le,
                                channels: 2,
                                rate: crate::platform::PA_SAMPLE_RATE,
                            };
                            log::info!("pa monitor: {:?}", device);
                            // systemctl --user status pulseaudio.service
                            let mut buf: Vec<u8> = vec![0; AUDIO_DATA_SIZE_U8];
                            match psimple::Simple::new(
                                None,                             // Use the default server
                                &crate::get_app_name(),           // Our application’s name
                                pulse::stream::Direction::Record, // We want a record stream
                                Some(&device),                    // Use the default device
                                "record",                         // Description of our stream
                                &spec,                            // Our sample format
                                None,                             // Use default channel map
                                None, // Use default buffering attributes
                            ) {
                                Ok(s) => loop {
                                    if let Ok(_) = s.read(&mut buf) {
                                        let out =
                                            if buf.iter().filter(|x| **x != 0).next().is_none() {
                                                vec![]
                                            } else {
                                                buf.clone()
                                            };
                                        if let Err(err) = stream.send_raw(out.into()).await {
                                            log::error!("Failed to send audio data:{}", err);
                                            break;
                                        }
                                    }
                                },
                                Err(err) => {
                                    log::error!("Could not create simple pulse: {}", err);
                                }
                            }
                        }
                        Err(err) => {
                            log::error!("Couldn't get pa client: {:?}", err);
                        }
                    }
                }
            }
        }
        Err(err) => {
            log::error!("Failed to start pa ipc server: {}", err);
        }
    }
}

#[inline]
#[cfg(not(windows))]
fn get_pid_file(postfix: &str) -> String {
    let path = Config::ipc_path(postfix);
    format!("{}.pid", path)
}

#[cfg(not(any(windows, target_os = "android", target_os = "ios")))]
async fn check_pid(postfix: &str) {
    let pid_file = get_pid_file(postfix);
    if let Ok(mut file) = File::open(&pid_file) {
        let mut content = String::new();
        file.read_to_string(&mut content).ok();
        let pid = content.parse::<usize>().unwrap_or(0);
        if pid > 0 {
            use hbb_common::sysinfo::System;
            let mut sys = System::new();
            sys.refresh_processes();
            if let Some(p) = sys.process(pid.into()) {
                if let Some(current) = sys.process((std::process::id() as usize).into()) {
                    if current.name() == p.name() {
                        // double check with connect
                        if connect(1000, postfix).await.is_ok() {
                            return;
                        }
                    }
                }
            }
        }
    }
    // if not remove old ipc file, the new ipc creation will fail
    // if we remove a ipc file, but the old ipc process is still running,
    // new connection to the ipc will connect to new ipc, old connection to old ipc still keep alive
    std::fs::remove_file(&Config::ipc_path(postfix)).ok();
}

#[inline]
#[cfg(not(windows))]
fn write_pid(postfix: &str) {
    let path = get_pid_file(postfix);
    if let Ok(mut file) = File::create(&path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0600)).ok();
        file.write_all(&std::process::id().to_string().into_bytes())
            .ok();
    }
}

pub struct ConnectionTmpl<T> {
    inner: Framed<T, BytesCodec>,
}

pub type Connection = ConnectionTmpl<Conn>;

impl<T> ConnectionTmpl<T>
where
    T: AsyncRead + AsyncWrite + std::marker::Unpin,
{
    pub fn new(conn: T) -> Self {
        Self {
            inner: Framed::new(conn, BytesCodec::new()),
        }
    }

    pub async fn send(&mut self, data: &Data) -> ResultType<()> {
        let v = serde_json::to_vec(data)?;
        self.inner.send(bytes::Bytes::from(v)).await?;
        Ok(())
    }

    async fn send_config(&mut self, name: &str, value: String) -> ResultType<()> {
        self.send(&Data::Config((name.to_owned(), Some(value))))
            .await
    }

    pub async fn next_timeout(&mut self, ms_timeout: u64) -> ResultType<Option<Data>> {
        Ok(timeout(ms_timeout, self.next()).await??)
    }

    pub async fn next_timeout2(&mut self, ms_timeout: u64) -> Option<ResultType<Option<Data>>> {
        if let Ok(x) = timeout(ms_timeout, self.next()).await {
            Some(x)
        } else {
            None
        }
    }

    pub async fn next(&mut self) -> ResultType<Option<Data>> {
        match self.inner.next().await {
            Some(res) => {
                let bytes = res?;
                if let Ok(s) = std::str::from_utf8(&bytes) {
                    if let Ok(data) = serde_json::from_str::<Data>(s) {
                        return Ok(Some(data));
                    }
                }
                return Ok(None);
            }
            _ => {
                bail!("reset by the peer");
            }
        }
    }

    pub async fn send_raw(&mut self, data: Bytes) -> ResultType<()> {
        self.inner.send(data).await?;
        Ok(())
    }

    pub async fn next_raw(&mut self) -> ResultType<bytes::BytesMut> {
        match self.inner.next().await {
            Some(Ok(res)) => Ok(res),
            _ => {
                bail!("reset by the peer");
            }
        }
    }
}

#[tokio::main(flavor = "current_thread")]
pub async fn get_config(name: &str) -> ResultType<Option<String>> {
    get_config_async(name, 1_000).await
}

async fn get_config_async(name: &str, ms_timeout: u64) -> ResultType<Option<String>> {
    let mut c = connect(ms_timeout, "").await?;
    c.send(&Data::Config((name.to_owned(), None))).await?;
    if let Some(Data::Config((name2, value))) = c.next_timeout(ms_timeout).await? {
        if name == name2 {
            return Ok(value);
        }
    }
    return Ok(None);
}

pub async fn set_config_async(name: &str, value: String) -> ResultType<()> {
    let mut c = connect(1000, "").await?;
    c.send_config(name, value).await?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
pub async fn set_data(data: &Data) -> ResultType<()> {
    set_data_async(data).await
}

async fn set_data_async(data: &Data) -> ResultType<()> {
    let mut c = connect(1000, "").await?;
    c.send(data).await?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
pub async fn set_config(name: &str, value: String) -> ResultType<()> {
    set_config_async(name, value).await
}

pub fn update_temporary_password() -> ResultType<()> {
    set_config("temporary-password", "".to_owned())
}

pub fn get_permanent_password() -> String {
    if let Ok(Some(v)) = get_config("permanent-password") {
        Config::set_permanent_password(&v);
        v
    } else {
        Config::get_permanent_password()
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl<T> ConnectionTmpl<T>
where
    T: AsyncRead + AsyncWrite + std::marker::Unpin + std::os::fd::AsRawFd,
{
    fn peer_uid(&self) -> Option<u32> {
        let fd = self.inner.get_ref().as_raw_fd();
        #[cfg(target_os = "linux")]
        {
            let mut cred: hbb_common::libc::ucred = unsafe { std::mem::zeroed() };
            let mut len = std::mem::size_of::<hbb_common::libc::ucred>() as hbb_common::libc::socklen_t;
            let rc = unsafe {
                hbb_common::libc::getsockopt(
                    fd,
                    hbb_common::libc::SOL_SOCKET,
                    hbb_common::libc::SO_PEERCRED,
                    &mut cred as *mut _ as *mut hbb_common::libc::c_void,
                    &mut len,
                )
            };
            if rc == 0 {
                return Some(cred.uid as u32);
            }
            return None;
        }
        #[cfg(target_os = "macos")]
        {
        let mut uid: hbb_common::libc::uid_t = 0;
        let mut gid: hbb_common::libc::gid_t = 0;
        if unsafe { hbb_common::libc::getpeereid(fd, &mut uid, &mut gid) } == 0 {
            Some(uid as u32)
        } else {
            None
        }
        }
    }

    fn service_authorization_status(&self) -> (bool, Option<u32>, Option<u32>) {
        let peer_uid = self.peer_uid();
        #[cfg(target_os = "linux")]
        {
            let (active_uid, used_fallback) = resolve_service_auth_active_uid();
            if used_fallback {
                log::debug!(
                    "Service authorization is using fallback active uid on linux: peer_uid={:?}, active_uid={:?}",
                    peer_uid,
                    active_uid
                );
            }
            let authorized =
                peer_uid.is_some_and(|uid| is_allowed_service_peer_uid(uid, active_uid));
            return (authorized, peer_uid, active_uid);
        }
        #[cfg(target_os = "macos")]
        {
            let active_uid = active_uid_strict();
            let authorized =
                peer_uid.is_some_and(|uid| is_allowed_service_peer_uid(uid, active_uid));
            (authorized, peer_uid, active_uid)
        }
    }
}

pub fn get_fingerprint() -> String {
    get_config("fingerprint")
        .unwrap_or_default()
        .unwrap_or_default()
}

pub fn set_permanent_password(v: String) -> ResultType<()> {
    Config::set_permanent_password(&v);
    set_config("permanent-password", v)
}

#[cfg(feature = "flutter")]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn set_unlock_pin(v: String, translate: bool) -> ResultType<()> {
    let v = v.trim().to_owned();
    let min_len = 4;
    let max_len = crate::ui_interface::max_encrypt_len();
    let len = v.chars().count();
    if !v.is_empty() {
        if len < min_len {
            let err = if translate {
                crate::lang::translate(
                    "Requires at least {".to_string() + &format!("{min_len}") + "} characters",
                )
            } else {
                // Sometimes, translated can't show normally in command line
                format!("Requires at least {} characters", min_len)
            };
            bail!(err);
        }
        if len > max_len {
            bail!("No more than {max_len} characters");
        }
    }
    Config::set_unlock_pin(&v);
    set_config("unlock-pin", v)
}

#[cfg(feature = "flutter")]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn get_unlock_pin() -> String {
    if let Ok(Some(v)) = get_config("unlock-pin") {
        Config::set_unlock_pin(&v);
        v
    } else {
        Config::get_unlock_pin()
    }
}

#[cfg(feature = "flutter")]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn get_trusted_devices() -> String {
    if let Ok(Some(v)) = get_config("trusted-devices") {
        v
    } else {
        Config::get_trusted_devices_json()
    }
}

#[cfg(feature = "flutter")]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn remove_trusted_devices(hwids: Vec<Bytes>) {
    Config::remove_trusted_devices(&hwids);
    allow_err!(set_data(&Data::RemoveTrustedDevices(hwids)));
}

#[cfg(feature = "flutter")]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn clear_trusted_devices() {
    Config::clear_trusted_devices();
    allow_err!(set_data(&Data::ClearTrustedDevices));
}

pub fn get_id() -> String {
    if let Ok(Some(v)) = get_config("id") {
        // update salt also, so that next time reinstallation not causing first-time auto-login failure
        if let Ok(Some(v2)) = get_config("salt") {
            Config::set_salt(&v2);
        }
        if v != Config::get_id() {
            Config::set_key_confirmed(false);
            Config::set_id(&v);
        }
        v
    } else {
        Config::get_id()
    }
}

pub async fn get_rendezvous_server(ms_timeout: u64) -> (String, Vec<String>) {
    if let Ok(Some(v)) = get_config_async("rendezvous_server", ms_timeout).await {
        let mut urls = v.split(",");
        let a = urls.next().unwrap_or_default().to_owned();
        let b: Vec<String> = urls.map(|x| x.to_owned()).collect();
        (a, b)
    } else {
        (
            Config::get_rendezvous_server(),
            Config::get_rendezvous_servers(),
        )
    }
}

async fn get_options_(ms_timeout: u64) -> ResultType<HashMap<String, String>> {
    let mut c = connect(ms_timeout, "").await?;
    c.send(&Data::Options(None)).await?;
    if let Some(Data::Options(Some(value))) = c.next_timeout(ms_timeout).await? {
        Config::set_options(value.clone());
        Ok(value)
    } else {
        Ok(Config::get_options())
    }
}

pub async fn get_options_async() -> HashMap<String, String> {
    get_options_(1000).await.unwrap_or(Config::get_options())
}

#[tokio::main(flavor = "current_thread")]
pub async fn get_options() -> HashMap<String, String> {
    get_options_async().await
}

pub async fn get_option_async(key: &str) -> String {
    if let Some(v) = get_options_async().await.get(key) {
        v.clone()
    } else {
        "".to_owned()
    }
}

pub fn set_option(key: &str, value: &str) {
    let mut options = get_options();
    if value.is_empty() {
        options.remove(key);
    } else {
        options.insert(key.to_owned(), value.to_owned());
    }
    set_options(options).ok();
}

#[tokio::main(flavor = "current_thread")]
pub async fn set_options(value: HashMap<String, String>) -> ResultType<()> {
    let _nat = CheckTestNatType::new();
    if let Ok(mut c) = connect(1000, "").await {
        c.send(&Data::Options(Some(value.clone()))).await?;
        // do not put below before connect, because we need to check should_exit
        c.next_timeout(1000).await.ok();
    }
    Config::set_options(value);
    Ok(())
}

#[inline]
async fn get_nat_type_(ms_timeout: u64) -> ResultType<i32> {
    let mut c = connect(ms_timeout, "").await?;
    c.send(&Data::NatType(None)).await?;
    if let Some(Data::NatType(Some(value))) = c.next_timeout(ms_timeout).await? {
        Config::set_nat_type(value);
        Ok(value)
    } else {
        Ok(Config::get_nat_type())
    }
}

pub async fn get_nat_type(ms_timeout: u64) -> i32 {
    get_nat_type_(ms_timeout)
        .await
        .unwrap_or(Config::get_nat_type())
}

pub async fn get_rendezvous_servers(ms_timeout: u64) -> Vec<String> {
    if let Ok(Some(v)) = get_config_async("rendezvous_servers", ms_timeout).await {
        return v.split(',').map(|x| x.to_owned()).collect();
    }
    return Config::get_rendezvous_servers();
}

#[inline]
async fn get_socks_(ms_timeout: u64) -> ResultType<Option<config::Socks5Server>> {
    let mut c = connect(ms_timeout, "").await?;
    c.send(&Data::Socks(None)).await?;
    if let Some(Data::Socks(value)) = c.next_timeout(ms_timeout).await? {
        Config::set_socks(value.clone());
        Ok(value)
    } else {
        Ok(Config::get_socks())
    }
}

pub async fn get_socks_async(ms_timeout: u64) -> Option<config::Socks5Server> {
    get_socks_(ms_timeout).await.unwrap_or(Config::get_socks())
}

#[tokio::main(flavor = "current_thread")]
pub async fn get_socks() -> Option<config::Socks5Server> {
    get_socks_async(1_000).await
}

#[tokio::main(flavor = "current_thread")]
pub async fn set_socks(value: config::Socks5Server) -> ResultType<()> {
    let _nat = CheckTestNatType::new();
    Config::set_socks(if value.proxy.is_empty() {
        None
    } else {
        Some(value.clone())
    });
    connect(1_000, "")
        .await?
        .send(&Data::Socks(Some(value)))
        .await?;
    Ok(())
}

async fn get_socks_ws_(ms_timeout: u64) -> ResultType<(Option<config::Socks5Server>, String)> {
    let mut c = connect(ms_timeout, "").await?;
    c.send(&Data::SocksWs(None)).await?;
    if let Some(Data::SocksWs(Some(value))) = c.next_timeout(ms_timeout).await? {
        Config::set_socks(value.0.clone());
        Config::set_option(OPTION_ALLOW_WEBSOCKET.to_string(), value.1.clone());
        Ok(*value)
    } else {
        Ok((
            Config::get_socks(),
            Config::get_option(OPTION_ALLOW_WEBSOCKET),
        ))
    }
}

#[tokio::main(flavor = "current_thread")]
pub async fn get_socks_ws() -> (Option<config::Socks5Server>, String) {
    get_socks_ws_(1_000).await.unwrap_or((
        Config::get_socks(),
        Config::get_option(OPTION_ALLOW_WEBSOCKET),
    ))
}

pub fn get_proxy_status() -> bool {
    Config::get_socks().is_some()
}
#[tokio::main(flavor = "current_thread")]
pub async fn test_rendezvous_server() -> ResultType<()> {
    let mut c = connect(1000, "").await?;
    c.send(&Data::TestRendezvousServer).await?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
pub async fn send_url_scheme(url: String) -> ResultType<()> {
    connect(1_000, "_url")
        .await?
        .send(&Data::UrlLink(url))
        .await?;
    Ok(())
}

// Emit `close` events to ipc.
pub fn close_all_instances() -> ResultType<bool> {
    match crate::ipc::send_url_scheme(IPC_ACTION_CLOSE.to_owned()) {
        Ok(_) => Ok(true),
        Err(err) => Err(err),
    }
}

#[tokio::main(flavor = "current_thread")]
pub async fn connect_to_user_session(usid: Option<u32>) -> ResultType<()> {
    let mut stream = crate::ipc::connect_service(1000).await?;
    timeout(1000, stream.send(&crate::ipc::Data::UserSid(usid))).await??;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
pub async fn notify_server_to_check_hwcodec() -> ResultType<()> {
    connect(1_000, "").await?.send(&&Data::CheckHwcodec).await?;
    Ok(())
}

#[cfg(target_os = "windows")]
pub async fn get_port_forward_session_count(ms_timeout: u64) -> ResultType<usize> {
    let mut c = connect(ms_timeout, "").await?;
    c.send(&Data::PortForwardSessionCount(None)).await?;
    if let Some(Data::PortForwardSessionCount(Some(count))) = c.next_timeout(ms_timeout).await? {
        return Ok(count);
    }
    bail!("Failed to get port forward session count");
}

#[cfg(feature = "hwcodec")]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tokio::main(flavor = "current_thread")]
pub async fn get_hwcodec_config_from_server() -> ResultType<()> {
    if !scrap::codec::enable_hwcodec_option() || scrap::hwcodec::HwCodecConfig::already_set() {
        return Ok(());
    }
    let mut c = connect(50, "").await?;
    c.send(&Data::HwCodecConfig(None)).await?;
    if let Some(Data::HwCodecConfig(v)) = c.next_timeout(50).await? {
        match v {
            Some(v) => {
                scrap::hwcodec::HwCodecConfig::set(v);
                return Ok(());
            }
            None => {
                bail!("hwcodec config is none");
            }
        }
    }
    bail!("failed to get hwcodec config");
}

#[cfg(feature = "hwcodec")]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn client_get_hwcodec_config_thread(wait_sec: u64) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    if !crate::platform::is_installed()
        || !scrap::codec::enable_hwcodec_option()
        || scrap::hwcodec::HwCodecConfig::already_set()
    {
        return;
    }
    ONCE.call_once(move || {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let mut intervals: Vec<u64> = vec![wait_sec, 3, 3, 6, 9];
            for i in intervals.drain(..) {
                if i > 0 {
                    std::thread::sleep(std::time::Duration::from_secs(i));
                }
                if get_hwcodec_config_from_server().is_ok() {
                    break;
                }
            }
        });
    });
}

#[cfg(feature = "hwcodec")]
#[tokio::main(flavor = "current_thread")]
pub async fn hwcodec_process() {
    let s = scrap::hwcodec::check_available_hwcodec();
    for _ in 0..5 {
        match crate::ipc::connect(1000, "").await {
            Ok(mut conn) => {
                match conn
                    .send(&crate::ipc::Data::HwCodecConfig(Some(s.clone())))
                    .await
                {
                    Ok(()) => {
                        log::info!("send ok");
                        break;
                    }
                    Err(e) => {
                        log::error!("send failed: {e:?}");
                    }
                }
            }
            Err(e) => {
                log::error!("connect failed: {e:?}");
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

#[tokio::main(flavor = "current_thread")]
pub async fn get_wayland_screencast_restore_token(key: String) -> ResultType<String> {
    let v = handle_wayland_screencast_restore_token(key, "get".to_owned()).await?;
    Ok(v.unwrap_or_default())
}

#[tokio::main(flavor = "current_thread")]
pub async fn clear_wayland_screencast_restore_token(key: String) -> ResultType<bool> {
    if let Some(v) = handle_wayland_screencast_restore_token(key, "clear".to_owned()).await? {
        return Ok(v.is_empty());
    }
    return Ok(false);
}

#[cfg(all(
    feature = "flutter",
    not(any(target_os = "android", target_os = "ios"))
))]
#[tokio::main(flavor = "current_thread")]
pub async fn update_controlling_session_count(count: usize) -> ResultType<()> {
    let mut c = connect(1000, "").await?;
    c.send(&Data::ControllingSessionCount(count)).await?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::main(flavor = "current_thread")]
pub async fn get_terminal_session_count() -> ResultType<usize> {
    let timeout_ms = 1_000;
    let effective_uid = unsafe { hbb_common::libc::geteuid() as u32 };
    let candidate_uids = terminal_count_candidate_uids(effective_uid, active_uid());
    for candidate_uid in candidate_uids {
        let socket_path = linux_ipc_path_for_uid(candidate_uid, "");
        let Ok(connect_result) = timeout(timeout_ms, Endpoint::connect(&socket_path)).await else {
            continue;
        };
        let Ok(connection) = connect_result else {
            continue;
        };
        let mut ipc_conn = ConnectionTmpl::new(connection);
        if ipc_conn.send(&Data::TerminalSessionCount(0)).await.is_err() {
            continue;
        }
        if let Ok(Some(Data::TerminalSessionCount(session_count))) =
            ipc_conn.next_timeout(timeout_ms).await
        {
            return Ok(session_count);
        }
    }
    Ok(0)
}

async fn handle_wayland_screencast_restore_token(
    key: String,
    value: String,
) -> ResultType<Option<String>> {
    let ms_timeout = 1_000;
    let mut c = connect(ms_timeout, "").await?;
    c.send(&Data::WaylandScreencastRestoreToken((key, value)))
        .await?;
    if let Some(Data::WaylandScreencastRestoreToken((_key, v))) = c.next_timeout(ms_timeout).await?
    {
        return Ok(Some(v));
    }
    return Ok(None);
}

#[tokio::main(flavor = "current_thread")]
pub async fn set_install_option(k: String, v: String) -> ResultType<()> {
    if let Ok(mut c) = connect(1000, "").await {
        c.send(&&Data::InstallOption(Some((k, v)))).await?;
        // do not put below before connect, because we need to check should_exit
        c.next_timeout(1000).await.ok();
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn verify_ffi_enum_data_size() {
        println!("{}", std::mem::size_of::<Data>());
        assert!(std::mem::size_of::<Data>() <= 120);
    }

    #[test]
    fn test_service_ipc_postfix_compatibility() {
        assert!(config::is_service_ipc_postfix(crate::POSTFIX_SERVICE));
        assert!(config::is_service_ipc_postfix("_service_abcdef"));
        assert!(!config::is_service_ipc_postfix(""));
        assert!(!config::is_service_ipc_postfix("_services"));
    }

    #[test]
    fn test_service_peer_uid_policy() {
        assert!(is_allowed_service_peer_uid(0, None));
        assert!(is_allowed_service_peer_uid(501, Some(501)));
        assert!(!is_allowed_service_peer_uid(502, Some(501)));
        assert!(!is_allowed_service_peer_uid(501, None));
    }

    #[test]
    fn test_terminal_count_candidate_uids_policy() {
        assert_eq!(
            terminal_count_candidate_uids_policy(1000, Some(501), Some(700), &[800]),
            vec![1000]
        );
        assert_eq!(
            terminal_count_candidate_uids_policy(1000, None, None, &[800]),
            vec![1000]
        );
        assert_eq!(
            terminal_count_candidate_uids_policy(0, Some(501), None, &[]),
            vec![501, 0]
        );
        assert_eq!(
            terminal_count_candidate_uids_policy(0, None, Some(700), &[]),
            vec![700, 0]
        );
        assert_eq!(
            terminal_count_candidate_uids_policy(0, Some(0), Some(0), &[]),
            vec![0]
        );
        assert_eq!(
            terminal_count_candidate_uids_policy(0, Some(501), Some(501), &[501, 700, 700]),
            vec![501, 700, 0]
        );
    }

    #[test]
    fn test_should_discover_terminal_socket_uids_policy() {
        assert!(should_discover_terminal_socket_uids(0, None, None));
        assert!(!should_discover_terminal_socket_uids(1000, None, None));
        assert!(!should_discover_terminal_socket_uids(0, Some(501), None));
        assert!(!should_discover_terminal_socket_uids(0, None, Some(501)));
    }

    #[test]
    fn test_connect_service_attempt_timeout_policy() {
        assert_eq!(connect_service_attempt_timeout(0, 3), 0);
        assert_eq!(connect_service_attempt_timeout(1000, 1), 1000);
        assert_eq!(connect_service_attempt_timeout(1000, 3), 333);
        assert_eq!(connect_service_attempt_timeout(2, 3), 1);
    }

    #[test]
    fn test_resolve_service_ipc_postfix_policy() {
        assert_eq!(
            resolve_service_ipc_postfix(None, Some("_service_meta".to_owned())),
            crate::POSTFIX_SERVICE.to_owned()
        );
        assert_eq!(
            resolve_service_ipc_postfix(
                Some("invalid".to_owned()),
                Some("_service_meta".to_owned())
            ),
            crate::POSTFIX_SERVICE.to_owned()
        );
        assert_eq!(
            resolve_service_ipc_postfix(Some("invalid".to_owned()), None),
            crate::POSTFIX_SERVICE.to_owned()
        );
        assert_eq!(
            resolve_service_ipc_postfix(Some("_service_../../tmp".to_owned()), None),
            crate::POSTFIX_SERVICE.to_owned()
        );
    }

    #[test]
    fn test_service_ipc_postfix_candidates_policy() {
        assert_eq!(
            build_service_ipc_postfix_candidates(
                Some("_service_abcd".to_owned()),
                Some("_service_abcd".to_owned())
            ),
            vec!["_service_abcd".to_owned(), crate::POSTFIX_SERVICE.to_owned()]
        );
        assert_eq!(
            build_service_ipc_postfix_candidates(
                Some("_service_env".to_owned()),
                Some("_service_meta".to_owned())
            ),
            vec![
                "_service_env".to_owned(),
                "_service_meta".to_owned(),
                crate::POSTFIX_SERVICE.to_owned()
            ]
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn test_ensure_secure_ipc_parent_dir_rejects_symlink_parent() {
        use std::os::unix::fs::symlink;

        let unique = format!(
            "rustdesk-ipc-secure-dir-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let base = std::env::temp_dir().join(unique);
        let real_dir = base.join("real");
        let link_dir = base.join("link");
        std::fs::create_dir_all(&real_dir).unwrap();
        symlink(&real_dir, &link_dir).unwrap();
        let ipc_path = link_dir.join("ipc_service");
        let res = ensure_secure_ipc_parent_dir(ipc_path.to_string_lossy().as_ref(), "_service");
        assert!(res.is_err());
        std::fs::remove_file(&link_dir).ok();
        std::fs::remove_dir_all(&real_dir).ok();
        std::fs::remove_dir_all(&base).ok();
    }
}
