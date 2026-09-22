#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::ipc::{
    constant_time_ipc_token_eq, generate_one_time_ipc_token, is_valid_main_ipc_token,
};
use crate::ipc::{Connection, ConnectionTmpl};
#[cfg(all(windows, not(feature = "flutter")))]
use hbb_common::sha2::{Digest, Sha256};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use hbb_common::{anyhow, bail, log, ResultType};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use hbb_common::{
    libc,
    tokio::io::{AsyncRead, AsyncWrite},
};
#[cfg(windows)]
use parity_tokio_ipc::SecurityAttributes;
#[cfg(windows)]
use std::io;
#[cfg(all(windows, not(feature = "flutter")))]
use std::io::Read;
#[cfg(target_os = "macos")]
use std::os::unix::fs::MetadataExt;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::io::RawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
use std::{
    fs,
    path::{Path, PathBuf},
};
#[cfg(windows)]
use windows::Win32::{Foundation::HANDLE, System::Pipes::GetNamedPipeClientProcessId};

#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAIN_IPC_PENDING_CAPABILITY_TTL: Duration = Duration::from_secs(60);
// Idle lifetime for capability reuse, refreshed by successful token checks.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAIN_IPC_APPROVED_CAPABILITY_TTL: Duration = Duration::from_secs(30 * 60);
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAIN_IPC_PIN_ATTEMPT_INTERVAL: Duration = Duration::from_secs(1);
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAIN_IPC_MAX_PENDING_CAPABILITIES: usize = 32;
// Full capacity rejects new tokens without evicting unexpired approvals.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAIN_IPC_MAX_APPROVED_CAPABILITIES: usize = 16;

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct PendingMainIpcCapability {
    token: String,
    expires_at: Instant,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Default)]
struct MainIpcCapabilityState {
    pending: HashMap<String, PendingMainIpcCapability>,
    approved: HashMap<String, Instant>,
    next_pin_attempt: Option<Instant>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl MainIpcCapabilityState {
    fn prune(&mut self, now: Instant) {
        self.pending.retain(|_, entry| entry.expires_at > now);
        self.approved.retain(|_, expires_at| *expires_at > now);
    }

    fn request(&mut self, token: String, request_id: String, now: Instant) -> ResultType<()> {
        if !is_valid_main_ipc_token(&token) || !is_valid_main_ipc_token(&request_id) {
            bail!("Invalid main IPC capability value");
        }
        self.prune(now);
        self.pending.retain(|_, entry| entry.token != token);
        if self.pending.len() >= MAIN_IPC_MAX_PENDING_CAPABILITIES {
            bail!("Too many pending main IPC capability requests");
        }
        if self.pending.contains_key(&request_id) {
            bail!("Duplicate main IPC capability request");
        }
        self.pending.insert(
            request_id,
            PendingMainIpcCapability {
                token,
                expires_at: now + MAIN_IPC_PENDING_CAPABILITY_TTL,
            },
        );
        Ok(())
    }

    fn approve_pending(&mut self, request_id: &str, now: Instant) -> bool {
        self.prune(now);
        let Some(entry) = self.pending.remove(request_id) else {
            return false;
        };
        self.approve_token(entry.token, now)
    }

    fn approve_token(&mut self, token: String, now: Instant) -> bool {
        if !is_valid_main_ipc_token(&token) {
            return false;
        }
        self.prune(now);
        if self.approved.len() >= MAIN_IPC_MAX_APPROVED_CAPABILITIES
            && !self.approved.contains_key(&token)
        {
            return false;
        }
        self.approved
            .insert(token, now + MAIN_IPC_APPROVED_CAPABILITY_TTL);
        true
    }

    fn is_approved(&mut self, token: &str, now: Instant) -> bool {
        self.prune(now);
        let approved_token = self
            .approved
            .keys()
            .find(|approved| constant_time_ipc_token_eq(approved, token))
            .cloned();
        let Some(approved_token) = approved_token else {
            return false;
        };
        self.approved
            .insert(approved_token, now + MAIN_IPC_APPROVED_CAPABILITY_TTL);
        true
    }

    fn begin_pin_attempt(&mut self, now: Instant) -> bool {
        if self.next_pin_attempt.is_some_and(|next| next > now) {
            return false;
        }
        self.next_pin_attempt = Some(now + MAIN_IPC_PIN_ATTEMPT_INTERVAL);
        true
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn main_ipc_capability_state() -> &'static Mutex<MainIpcCapabilityState> {
    static STATE: OnceLock<Mutex<MainIpcCapabilityState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(MainIpcCapabilityState::default()))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn main_ipc_client_token_state() -> &'static Mutex<Option<String>> {
    static TOKEN: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    TOKEN.get_or_init(|| Mutex::new(None))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn create_main_ipc_approval_request(token: &str) -> ResultType<String> {
    let request_id = generate_one_time_ipc_token()?;
    main_ipc_capability_state().lock().unwrap().request(
        token.to_owned(),
        request_id.clone(),
        Instant::now(),
    )?;
    Ok(request_id)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn approve_main_ipc_request(request_id: &str) -> bool {
    main_ipc_capability_state()
        .lock()
        .unwrap()
        .approve_pending(request_id, Instant::now())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn approve_main_ipc_token(token: &str) -> bool {
    main_ipc_capability_state()
        .lock()
        .unwrap()
        .approve_token(token.to_owned(), Instant::now())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn is_main_ipc_token_approved(token: &str) -> bool {
    main_ipc_capability_state()
        .lock()
        .unwrap()
        .is_approved(token, Instant::now())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn begin_main_ipc_pin_attempt() -> bool {
    main_ipc_capability_state()
        .lock()
        .unwrap()
        .begin_pin_attempt(Instant::now())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn set_main_ipc_client_token(token: Option<String>) -> bool {
    if token
        .as_deref()
        .is_some_and(|value| !is_valid_main_ipc_token(value))
    {
        return false;
    }
    *main_ipc_client_token_state().lock().unwrap() = token;
    true
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn main_ipc_client_token() -> Option<String> {
    main_ipc_client_token_state().lock().unwrap().clone()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn clear_main_ipc_client_token_if_matches(token: &str) {
    let mut current = main_ipc_client_token_state().lock().unwrap();
    if current
        .as_deref()
        .is_some_and(|value| constant_time_ipc_token_eq(value, token))
    {
        *current = None;
    }
}

#[cfg(windows)]
#[inline]
pub(crate) fn is_windows_main_or_service_ipc(postfix: &str) -> bool {
    postfix.is_empty() || hbb_common::config::is_service_ipc_postfix(postfix)
}

#[cfg(windows)]
#[inline]
pub(crate) fn portable_service_listener_security_attributes() -> io::Result<SecurityAttributes> {
    let user_sid = crate::platform::windows::current_process_user_sid_string().map_err(|err| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("failed to resolve current process SID: {}", err),
        )
    })?;
    debug_assert!(
        user_sid.starts_with("S-1-")
            && user_sid
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'-'),
        "current_process_user_sid_string returned a non-SDDL SID: {}",
        user_sid
    );
    // SDDL:
    // - `D:P`                => protected DACL (no inherited ACEs)
    // - `(A;;GA;;;SY)`       => allow GENERIC_ALL to LocalSystem
    // - `(A;;GA;;;{user_sid})` => allow GENERIC_ALL to current process user SID
    // References:
    // - Security Descriptor String Format: https://learn.microsoft.com/en-us/windows/win32/secauthz/security-descriptor-string-format
    // - ACE strings in SDDL: https://learn.microsoft.com/en-us/windows/win32/secauthz/ace-strings
    let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{user_sid})");
    SecurityAttributes::from_sddl(&sddl).map_err(|err| {
        io::Error::new(
            io::ErrorKind::Other,
            format!(
                "failed to build portable service listener security attributes from SDDL '{}': {}",
                sddl, err
            ),
        )
    })
}

#[cfg(target_os = "macos")]
#[inline]
fn macos_service_ipc_allows_gui_and_service_binaries(
    peer_exe: &Path,
    current_exe: &Path,
    postfix: &str,
) -> bool {
    if postfix != crate::POSTFIX_SERVICE {
        return false;
    }
    let Some(peer_dir) = peer_exe.parent() else {
        return false;
    };
    let Some(current_dir) = current_exe.parent() else {
        return false;
    };
    if !executable_paths_match(peer_dir, current_dir) {
        return false;
    }

    // On installed macOS builds, `_service` is listened by the `service` binary while the GUI
    // process connects from the app executable within the same app bundle.
    let gui_exe_name = std::ffi::OsString::from(crate::get_app_name());
    let gui_exe = gui_exe_name.as_os_str();
    let service_exe = std::ffi::OsStr::new("service");
    let allowed_exe = [Some(gui_exe), Some(service_exe)];
    let peer_name = peer_exe.file_name();
    let current_name = current_exe.file_name();
    allowed_exe
        .iter()
        .any(|name| os_str_eq_ignore_ascii_case(peer_name, *name))
        && allowed_exe
            .iter()
            .any(|name| os_str_eq_ignore_ascii_case(current_name, *name))
}

#[cfg(target_os = "windows")]
#[inline]
fn windows_portable_service_ipc_allows_logon_helper_executable(
    _peer_exe: &Path,
    postfix: &str,
) -> bool {
    if postfix != "_portable_service" {
        return false;
    }
    #[cfg(feature = "flutter")]
    {
        false
    }
    #[cfg(not(feature = "flutter"))]
    {
        let Some((_, expected)) = crate::platform::windows::portable_service_logon_helper_paths()
        else {
            return false;
        };
        let Ok(expected) = fs::canonicalize(expected) else {
            return false;
        };
        let Ok(current_exe) = current_exe_canonical_path() else {
            return false;
        };
        portable_service_helper_is_trusted(_peer_exe, &expected, &current_exe)
    }
}

#[cfg(windows)]
#[inline]
pub(crate) fn is_allowed_windows_session_scoped_peer(
    client_is_system: bool,
    client_session_id: Option<u32>,
    expected_session_id: Option<u32>,
) -> bool {
    client_is_system
        || matches!(
            (client_session_id, expected_session_id),
            (Some(client), Some(expected)) if client == expected
        )
}

#[cfg(windows)]
#[inline]
fn is_allowed_windows_portable_service_peer(
    client_is_system: Option<bool>,
    _client_session_id: Option<u32>,
    _expected_session_id: Option<u32>,
) -> bool {
    // Portable-service listener DACL includes SYSTEM and current-process SID.
    // In the portable-service path, current process is expected to run as SYSTEM,
    // and the higher-layer peer policy stays SYSTEM-only.
    matches!(client_is_system, Some(true))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[inline]
pub(crate) fn is_allowed_service_peer_uid(peer_uid: u32, active_uid: Option<u32>) -> bool {
    // Root is allowed at the UID gate because the service side may run as root.
    // Callers still enforce executable matching before accepting service-scoped peers.
    peer_uid == 0 || active_uid.is_some_and(|uid| uid == peer_uid)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[inline]
fn is_allowed_unix_main_peer_uid(peer_uid: u32, server_uid: u32) -> bool {
    peer_uid == 0 || peer_uid == server_uid
}

#[cfg(target_os = "macos")]
#[inline]
fn console_owner_uid() -> Option<u32> {
    fs::metadata("/dev/console")
        .ok()
        .map(|metadata| metadata.uid())
}

#[cfg(target_os = "macos")]
#[inline]
fn active_uid_strict() -> Option<u32> {
    // Prefer the filesystem metadata over parsing external command output.
    console_owner_uid()
}

#[cfg(target_os = "linux")]
#[inline]
fn active_uid_strict() -> Option<u32> {
    let reported_uid_raw = crate::platform::linux::get_active_userid();
    let trimmed = reported_uid_raw.trim();
    if let Ok(uid) = trimmed.parse::<u32>() {
        return Some(uid);
    }
    if trimmed.is_empty() {
        log::debug!("Failed to resolve active user uid on linux: active uid is empty");
    } else {
        log::warn!("Failed to parse active user uid on linux: '{}'", trimmed);
    }
    None
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[inline]
pub(crate) fn active_uid() -> Option<u32> {
    active_uid_strict()
}

/// The active session uid read ONLY from the service-loop cache, never from a fresh (blocking) seat0
/// lookup. `None` on a cache miss. For hot, latency-sensitive, fail-closed re-auth on an async runtime
/// thread (the `_drm` per-frame re-auth), where a blocking `loginctl` per frame would stall the stream.
// Gated with the feature, not just the OS: the `_drm` per-frame re-auth is its only caller, so a
// drm-off Linux build would carry it as dead code and warn about it.
#[cfg(all(target_os = "linux", feature = "drm"))]
#[inline]
pub(crate) fn active_uid_cached() -> Option<u32> {
    crate::platform::linux::get_active_userid_cached()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[inline]
pub(crate) fn peer_uid_from_fd(fd: RawFd) -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        return peer_cred_from_fd(fd).map(|cred| cred.uid as u32);
    }
    #[cfg(target_os = "macos")]
    {
        let mut uid = 0;
        let mut gid = 0;
        if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } == 0 {
            Some(uid as u32)
        } else {
            None
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[inline]
fn peer_pid_from_fd(fd: RawFd) -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        return peer_cred_from_fd(fd).and_then(|cred| (cred.pid > 0).then_some(cred.pid as u32));
    }
    #[cfg(target_os = "macos")]
    {
        let mut pid = 0;
        let mut len = std::mem::size_of::<libc::pid_t>() as _;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                &mut pid as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if rc == 0 && pid > 0 {
            Some(pid as _)
        } else {
            None
        }
    }
}

#[cfg(target_os = "linux")]
#[inline]
fn peer_cred_from_fd(fd: RawFd) -> Option<libc::ucred> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as _;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc == 0 {
        Some(cred)
    } else {
        None
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[inline]
fn current_exe_canonical_path() -> ResultType<PathBuf> {
    let current = std::env::current_exe()
        .map_err(|err| anyhow::anyhow!("Failed to resolve current executable path: {}", err))?;
    fs::canonicalize(&current).map_err(|err| {
        anyhow::anyhow!(
            "Failed to canonicalize current executable path '{}': {}",
            current.display(),
            err
        )
        .into()
    })
}

#[cfg(target_os = "linux")]
#[inline]
fn peer_exe_canonical_path_by_pid(peer_pid: u32) -> ResultType<PathBuf> {
    let proc_exe = PathBuf::from(format!("/proc/{peer_pid}/exe"));
    let peer_exe = fs::read_link(&proc_exe).map_err(|err| {
        anyhow::anyhow!(
            "Failed to read peer executable link '{}': {}",
            proc_exe.display(),
            err
        )
    })?;
    fs::canonicalize(&peer_exe).map_err(|err| {
        anyhow::anyhow!(
            "Failed to canonicalize peer executable path '{}': {}",
            peer_exe.display(),
            err
        )
        .into()
    })
}

#[cfg(target_os = "macos")]
#[inline]
fn peer_exe_canonical_path_by_pid(peer_pid: u32) -> ResultType<PathBuf> {
    const PROC_PIDPATH_BUF_SIZE: usize = libc::PROC_PIDPATHINFO_MAXSIZE as _;
    let mut buffer = vec![0u8; PROC_PIDPATH_BUF_SIZE];
    let length = unsafe {
        libc::proc_pidpath(
            peer_pid as _,
            buffer.as_mut_ptr() as _,
            PROC_PIDPATH_BUF_SIZE as _,
        )
    };
    if length <= 0 {
        bail!("Failed to query peer process path from pid {}", peer_pid);
    }
    buffer.truncate(length as _);
    let path = PathBuf::from(String::from_utf8_lossy(&buffer).to_string());
    fs::canonicalize(&path).map_err(|err| {
        anyhow::anyhow!(
            "Failed to canonicalize peer executable path '{}': {}",
            path.display(),
            err
        )
        .into()
    })
}

#[cfg(target_os = "windows")]
#[inline]
fn peer_exe_canonical_path_by_pid(peer_pid: u32) -> ResultType<PathBuf> {
    let path = crate::platform::windows::get_process_executable_path(peer_pid)?;
    fs::canonicalize(&path).map_err(|err| {
        anyhow::anyhow!(
            "Failed to canonicalize peer executable path '{}': {}",
            path.display(),
            err
        )
        .into()
    })
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[inline]
pub(crate) fn executable_paths_match(left: &Path, right: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        // Callers pass paths resolved through fs::canonicalize() first, so NT
        // namespace paths and 8.3 short names are expected to be resolved before
        // this check. Keep this normalization limited to remaining Win32 spelling
        // differences.
        fn normalize(path: &Path) -> String {
            let mut normalized = path.to_string_lossy().replace('/', "\\");
            if let Some(stripped) = normalized.strip_prefix(r"\\?\") {
                normalized = stripped.to_owned();
            }
            normalized.to_ascii_lowercase()
        }
        return normalize(left) == normalize(right);
    }
    #[cfg(target_os = "macos")]
    {
        return paths_refer_to_same_file(left, right);
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        left == right
    }
}

#[cfg(target_os = "macos")]
#[inline]
fn paths_refer_to_same_file(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    let (Ok(left), Ok(right)) = (fs::metadata(left), fs::metadata(right)) else {
        return false;
    };
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(target_os = "macos")]
#[inline]
fn os_str_eq_ignore_ascii_case(
    left: Option<&std::ffi::OsStr>,
    right: Option<&std::ffi::OsStr>,
) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(all(windows, not(feature = "flutter")))]
#[inline]
fn file_sha256(path: &Path) -> ResultType<[u8; 32]> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8 * 1024];
    loop {
        let read_bytes = file.read(&mut buffer)?;
        if read_bytes == 0 {
            break;
        }
        hasher.update(&buffer[..read_bytes]);
    }
    Ok(hasher.finalize().into())
}

#[cfg(all(windows, not(feature = "flutter")))]
#[inline]
fn portable_service_helper_is_trusted(
    peer_exe: &Path,
    expected_exe: &Path,
    current_exe: &Path,
) -> bool {
    if !executable_paths_match(peer_exe, expected_exe) {
        return false;
    }
    let peer_hash = match file_sha256(peer_exe) {
        Ok(hash) => hash,
        Err(err) => {
            log::warn!(
                "Failed to hash peer portable helper executable '{}': {}",
                peer_exe.display(),
                err
            );
            return false;
        }
    };
    let current_hash = match file_sha256(current_exe) {
        Ok(hash) => hash,
        Err(err) => {
            log::warn!(
                "Failed to hash current executable '{}' for portable helper trust check: {}",
                current_exe.display(),
                err
            );
            return false;
        }
    };
    peer_hash == current_hash
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[inline]
fn ensure_peer_executable_matches_current_by_pid(peer_pid: u32, postfix: &str) -> ResultType<()> {
    let peer_exe = peer_exe_canonical_path_by_pid(peer_pid)?;
    let current_exe = current_exe_canonical_path()?;
    if executable_paths_match(&peer_exe, &current_exe) {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    if macos_service_ipc_allows_gui_and_service_binaries(&peer_exe, &current_exe, postfix) {
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    if windows_portable_service_ipc_allows_logon_helper_executable(&peer_exe, postfix) {
        return Ok(());
    }
    bail!(
        "Peer executable path mismatch on ipc channel '{}': peer_pid={}, peer_exe='{}', current_exe='{}'",
        postfix,
        peer_pid,
        peer_exe.display(),
        current_exe.display()
    );
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[inline]
pub(crate) fn ensure_peer_executable_matches_current_by_pid_opt(
    peer_pid: Option<u32>,
    postfix: &str,
) -> ResultType<()> {
    let peer_pid = peer_pid.ok_or_else(|| {
        anyhow::anyhow!("Failed to resolve peer pid on ipc channel '{}'", postfix)
    })?;
    ensure_peer_executable_matches_current_by_pid(peer_pid, postfix)
}

#[cfg(target_os = "linux")]
#[inline]
pub(crate) fn ensure_peer_executable_matches_current_by_fd(
    fd: RawFd,
    postfix: &str,
) -> ResultType<()> {
    let peer_pid = peer_pid_from_fd(fd).ok_or_else(|| {
        anyhow::anyhow!("Failed to resolve peer pid on ipc channel '{}'", postfix)
    })?;
    ensure_peer_executable_matches_current_by_pid(peer_pid, postfix)
}

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
const UNAUTHORIZED_IPC_LOG_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[inline]
fn log_rejected_service_connection(postfix: &str, peer_uid: Option<u32>, active_uid: Option<u32>) {
    hbb_common::throttled_log!(
        UNAUTHORIZED_IPC_LOG_INTERVAL,
        warn,
        "Rejected unauthorized connection on protected service-scoped IPC channel: postfix={}, peer_uid={:?}, active_uid={:?}",
        postfix,
        peer_uid,
        active_uid
    );
}

#[cfg(target_os = "linux")]
#[inline]
pub(crate) fn log_rejected_uinput_connection(
    postfix: &str,
    peer_uid: Option<u32>,
    active_uid: Option<u32>,
) {
    hbb_common::throttled_log!(
        UNAUTHORIZED_IPC_LOG_INTERVAL,
        warn,
        "Rejected unauthorized connection on uinput ipc channel: postfix={}, peer_uid={:?}, active_uid={:?}",
        postfix,
        peer_uid,
        active_uid
    );
}

#[cfg(windows)]
#[inline]
pub(crate) fn log_rejected_windows_ipc_connection(
    postfix: &str,
    peer_pid: Option<u32>,
    peer_session_id: Option<u32>,
    expected_session_id: Option<u32>,
    peer_is_system: Option<bool>,
    peer_is_elevated: Option<bool>,
) {
    hbb_common::throttled_log!(
        UNAUTHORIZED_IPC_LOG_INTERVAL,
        warn,
        "Rejected unauthorized connection on ipc channel: postfix={}, peer_pid={:?}, peer_session_id={:?}, expected_session_id={:?}, peer_is_system={:?}, peer_is_elevated={:?}",
        postfix,
        peer_pid,
        peer_session_id,
        expected_session_id,
        peer_is_system,
        peer_is_elevated
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn authorize_unix_main_ipc_connection(stream: &Connection, postfix: &str) -> bool {
    let peer_uid = stream.peer_uid();
    let peer_pid = stream.peer_pid();
    let server_uid = unsafe { libc::geteuid() as u32 };
    if !peer_uid.is_some_and(|uid| is_allowed_unix_main_peer_uid(uid, server_uid)) {
        hbb_common::throttled_log!(
            UNAUTHORIZED_IPC_LOG_INTERVAL,
            warn,
            "Rejected unauthorized connection on Unix main IPC channel: postfix={}, peer_uid={:?}, server_uid={}, peer_pid={:?}",
            postfix,
            peer_uid,
            server_uid,
            peer_pid
        );
        return false;
    }
    if peer_uid == Some(0) {
        return true;
    }
    if let Err(err) = ensure_peer_executable_matches_current_by_pid_opt(peer_pid, postfix) {
        hbb_common::throttled_log!(
            UNAUTHORIZED_IPC_LOG_INTERVAL,
            warn,
            "Rejected unauthorized connection on Unix main IPC channel due to executable mismatch: postfix={}, peer_uid={:?}, peer_pid={:?}, err={}",
            postfix,
            peer_uid,
            peer_pid,
            err
        );
        return false;
    }
    true
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn authorize_service_scoped_ipc_connection(stream: &Connection, postfix: &str) -> bool {
    let peer_pid = stream.peer_pid();
    let (authorized, peer_uid, active_uid) = stream.service_authorization_status();
    if !authorized {
        log_rejected_service_connection(postfix, peer_uid, active_uid);
        return false;
    }
    if let Err(err) = ensure_peer_executable_matches_current_by_pid_opt(peer_pid, postfix) {
        log::warn!(
            "Rejected unauthorized connection on protected service-scoped IPC channel due to executable mismatch: postfix={}, peer_pid={:?}, err={}",
            postfix,
            peer_pid,
            err
        );
        return false;
    }
    true
}

#[cfg(target_os = "macos")]
pub(crate) fn authorize_user_server_process(
    peer_uid: Option<u32>,
    peer_pid: Option<u32>,
    expected_uid: u32,
) -> bool {
    if peer_uid != Some(expected_uid) {
        return false;
    }
    let Some(peer_pid) = peer_pid else {
        return false;
    };
    let Ok(peer_exe) = peer_exe_canonical_path_by_pid(peer_pid) else {
        return false;
    };
    let expected_path = PathBuf::from(format!(
        "/Applications/{}.app/Contents/MacOS/{}",
        crate::get_app_name(),
        crate::get_app_name()
    ));
    let Ok(expected_path) = fs::canonicalize(expected_path) else {
        return false;
    };
    paths_refer_to_same_file(&peer_exe, &expected_path)
}

#[cfg(windows)]
pub(crate) fn authorize_windows_main_ipc_connection(stream: &Connection, postfix: &str) -> bool {
    let (
        authorized,
        peer_pid,
        peer_session_id,
        server_session_id,
        peer_is_system,
        peer_is_elevated,
    ) = stream.server_authorization_status();
    if !authorized {
        log_rejected_windows_ipc_connection(
            postfix,
            peer_pid,
            peer_session_id,
            server_session_id,
            peer_is_system,
            peer_is_elevated,
        );
        return false;
    }
    if let Err(err) = ensure_peer_executable_matches_current_by_pid_opt(peer_pid, postfix) {
        log::warn!(
            "Rejected unauthorized connection on ipc channel due to executable mismatch: postfix={}, peer_pid={:?}, err={}",
            postfix,
            peer_pid,
            err
        );
        return false;
    }
    true
}

#[cfg(windows)]
pub(crate) fn authorize_windows_portable_service_ipc_connection(
    stream: &Connection,
    postfix: &str,
) -> bool {
    // Portable service IPC policy:
    // - only SYSTEM peers are authorized by is_allowed_windows_portable_service_peer()
    // - expected_session_id is still collected for diagnostics and identity checks
    // - final privilege boundary is enforced by named-pipe ACL + one-time token handshake
    // - when peer identity is unavailable on some hosts, executable verification remains
    //   best-effort telemetry (not fail-closed) to avoid breaking valid SYSTEM bootstrap
    //   flows that cannot be fully introspected
    let expected_session_id = crate::platform::windows::get_current_process_session_id();
    let (authorized, peer_pid, peer_session_id, peer_is_system) =
        stream.portable_service_authorization_status_for_session(expected_session_id);
    if !authorized {
        // Session lookup may succeed while SYSTEM identity lookup fails, so only the
        // SYSTEM identity result determines whether peer identity is unavailable here.
        // Don't use `peer_pid.is_some() && peer_session_id.is_none() && peer_is_system.is_none();` here.
        let identity_unavailable = peer_pid.is_some() && peer_is_system.is_none();
        if identity_unavailable {
            // In portable-service startup, resolving SYSTEM peer identity may fail on some hosts.
            // `ProcessIdToSessionId` can still succeed while `OpenProcessToken(TOKEN_QUERY)` is
            // denied by the peer token DACL or missing privileges. Treat that partial identity
            // failure as unavailable and defer final authorization to pipe ACL + token handshake.
            if let Err(err) = ensure_peer_executable_matches_current_by_pid_opt(peer_pid, postfix) {
                log::warn!(
                    "Portable service ipc peer identity unavailable and executable verification failed; continue with ACL+token-gated flow: postfix={}, peer_pid={:?}, err={}",
                    postfix,
                    peer_pid,
                    err
                );
            } else {
                log::warn!(
                    "Portable service ipc peer identity unavailable; executable verification matched, continue with ACL+token-gated flow: postfix={}, peer_pid={:?}, expected_session_id={:?}",
                    postfix,
                    peer_pid,
                    expected_session_id
                );
            }
            return true;
        }
        log::warn!(
            "Rejected unauthorized connection on portable service ipc channel: postfix={}, peer_pid={:?}, peer_session_id={:?}, expected_session_id={:?}, peer_is_system={:?}",
            postfix,
            peer_pid,
            peer_session_id,
            expected_session_id,
            peer_is_system
        );
        return false;
    }
    true
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl<T> ConnectionTmpl<T>
where
    T: AsyncRead + AsyncWrite + std::marker::Unpin + std::os::unix::io::AsRawFd,
{
    pub(super) fn peer_uid(&self) -> Option<u32> {
        peer_uid_from_fd(self.inner.get_ref().as_raw_fd())
    }

    fn service_authorization_status(&self) -> (bool, Option<u32>, Option<u32>) {
        let peer_uid = self.peer_uid();
        // On Linux, `_service` can use the cached active UID from the service loop for
        // stable config sync. Uinput does a fresh active-UID lookup in its own authorizer.
        let active_uid = active_uid();
        let authorized = peer_uid.is_some_and(|uid| is_allowed_service_peer_uid(uid, active_uid));
        (authorized, peer_uid, active_uid)
    }

    pub(super) fn peer_pid(&self) -> Option<u32> {
        peer_pid_from_fd(self.inner.get_ref().as_raw_fd())
    }
}

#[cfg(windows)]
impl ConnectionTmpl<parity_tokio_ipc::Connection> {
    fn peer_pid(&self) -> Option<u32> {
        let pipe_handle = self.inner.get_ref().as_raw_handle();
        if pipe_handle.is_null() {
            return None;
        }
        let mut pid = 0u32;
        let ok = unsafe { GetNamedPipeClientProcessId(HANDLE(pipe_handle), &mut pid as *mut u32) }
            .is_ok();
        if ok && pid != 0 {
            Some(pid)
        } else {
            None
        }
    }

    fn server_authorization_status(
        &self,
    ) -> (
        bool,
        Option<u32>,
        Option<u32>,
        Option<u32>,
        Option<bool>,
        Option<bool>,
    ) {
        let peer_pid = self.peer_pid();
        let server_session_id = crate::platform::windows::get_current_process_session_id();
        let peer_session_id =
            peer_pid.and_then(crate::platform::windows::get_session_id_of_process);
        let peer_is_system_result =
            peer_pid.map(crate::platform::windows::is_process_running_as_system);
        let peer_is_system = peer_is_system_result
            .as_ref()
            .and_then(|r| r.as_ref().ok().copied());
        let session_authorized = is_allowed_windows_session_scoped_peer(
            peer_is_system.unwrap_or(false),
            peer_session_id,
            server_session_id,
        );
        let peer_is_elevated_result = if session_authorized {
            None
        } else {
            peer_pid.map(|pid| crate::platform::windows::is_elevated(Some(pid)))
        };
        let peer_is_elevated = peer_is_elevated_result
            .as_ref()
            .and_then(|r| r.as_ref().ok().copied());
        if server_session_id.is_none()
            && !peer_is_system.unwrap_or(false)
            && !peer_is_elevated.unwrap_or(false)
        {
            // When the server session id cannot be determined, the session-id allow-path is
            // disabled and only privileged peers can be authorized.
            log::debug!(
                "IPC authorization: server session id unavailable; rejecting non-privileged peer, peer_pid={:?}, peer_session_id={:?}",
                peer_pid,
                peer_session_id
            );
        }
        // Main IPC trusts same-session peers, LocalSystem, and elevated administrators.
        // Service-scoped IPC channels keep their own stricter authorization paths.
        let authorized = session_authorized || peer_is_elevated.unwrap_or(false);
        if !authorized {
            if let (Some(pid), Some(Err(err))) = (peer_pid, peer_is_system_result.as_ref()) {
                log::debug!(
                    "Failed to determine whether peer process is SYSTEM, pid={}, err={}",
                    pid,
                    err
                );
            }
            if let (Some(pid), Some(Err(err))) = (peer_pid, peer_is_elevated_result.as_ref()) {
                log::debug!(
                    "Failed to determine whether peer process is elevated, pid={}, err={}",
                    pid,
                    err
                );
            }
        }
        (
            authorized,
            peer_pid,
            peer_session_id,
            server_session_id,
            peer_is_system,
            peer_is_elevated,
        )
    }

    pub(crate) fn service_authorization_status_for_session(
        &self,
        expected_active_session_id: Option<u32>,
    ) -> (bool, Option<u32>, Option<u32>, Option<bool>) {
        let peer_pid = self.peer_pid();
        let peer_session_id =
            peer_pid.and_then(crate::platform::windows::get_session_id_of_process);
        let peer_is_system_result =
            peer_pid.map(crate::platform::windows::is_process_running_as_system);
        let peer_is_system = peer_is_system_result
            .as_ref()
            .and_then(|r| r.as_ref().ok().copied());
        let authorized = is_allowed_windows_session_scoped_peer(
            peer_is_system.unwrap_or(false),
            peer_session_id,
            expected_active_session_id,
        );
        if !authorized {
            if let (Some(pid), Some(Err(err))) = (peer_pid, peer_is_system_result.as_ref()) {
                log::debug!(
                    "Failed to determine whether peer process is SYSTEM, pid={}, err={}",
                    pid,
                    err
                );
            }
        }
        (authorized, peer_pid, peer_session_id, peer_is_system)
    }

    pub(crate) fn portable_service_authorization_status_for_session(
        &self,
        expected_active_session_id: Option<u32>,
    ) -> (bool, Option<u32>, Option<u32>, Option<bool>) {
        // Portable-service policy:
        // only SYSTEM peers are allowed.
        let (_service_authorized, peer_pid, peer_session_id, peer_is_system) =
            self.service_authorization_status_for_session(expected_active_session_id);
        (
            is_allowed_windows_portable_service_peer(
                peer_is_system,
                peer_session_id,
                expected_active_session_id,
            ),
            peer_pid,
            peer_session_id,
            peer_is_system,
        )
    }
}

#[cfg(test)]
mod tests {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use super::{Connection, ConnectionTmpl};
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use crate::ipc::{self, Data, DataMainIpcAuth};
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use hbb_common::{futures::StreamExt as _, tokio};

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn test_service_peer_uid_policy() {
        assert!(super::is_allowed_service_peer_uid(0, None));
        assert!(super::is_allowed_service_peer_uid(501, Some(501)));
        assert!(!super::is_allowed_service_peer_uid(502, Some(501)));
        assert!(!super::is_allowed_service_peer_uid(501, None));
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn test_unix_main_peer_uid_policy() {
        assert!(super::is_allowed_unix_main_peer_uid(0, 501));
        assert!(super::is_allowed_unix_main_peer_uid(501, 501));
        assert!(!super::is_allowed_unix_main_peer_uid(502, 501));
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn test_main_ipc_capability_lifecycle() {
        use std::time::{Duration, Instant};

        let now = Instant::now();
        let token = "a".repeat(crate::ipc::IPC_TOKEN_LEN);
        let request_id = "b".repeat(crate::ipc::IPC_TOKEN_LEN);
        let mut state = super::MainIpcCapabilityState::default();

        state
            .request(token.clone(), request_id.clone(), now)
            .unwrap();
        assert!(!state.is_approved(&token, now));
        assert!(state.approve_pending(&request_id, now));
        assert!(state.is_approved(&token, now));
        assert!(!state.is_approved(
            &token,
            now + super::MAIN_IPC_APPROVED_CAPABILITY_TTL + Duration::from_secs(1)
        ));

        let expired_request = "c".repeat(crate::ipc::IPC_TOKEN_LEN);
        state.request(token, expired_request.clone(), now).unwrap();
        assert!(!state.approve_pending(
            &expired_request,
            now + super::MAIN_IPC_PENDING_CAPABILITY_TTL + Duration::from_secs(1)
        ));

        assert!(state.begin_pin_attempt(now));
        assert!(!state.begin_pin_attempt(now));
        assert!(state.begin_pin_attempt(now + super::MAIN_IPC_PIN_ATTEMPT_INTERVAL));
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn test_main_ipc_capability_capacity_recovers_after_expiry() {
        let now = std::time::Instant::now();
        let mut state = super::MainIpcCapabilityState::default();
        let tokens: Vec<_> = (0..=super::MAIN_IPC_MAX_APPROVED_CAPABILITIES)
            .map(|index| format!("{index:0width$x}", width = crate::ipc::IPC_TOKEN_LEN))
            .collect();
        for token in &tokens[..super::MAIN_IPC_MAX_APPROVED_CAPABILITIES] {
            assert!(state.approve_token(token.clone(), now));
        }
        let next_token = tokens.last().unwrap();
        assert!(!state.approve_token(next_token.clone(), now));
        assert!(state.approve_token(tokens[0].clone(), now));
        assert!(state.is_approved(&tokens[0], now));

        let after_expiry = now + super::MAIN_IPC_APPROVED_CAPABILITY_TTL;
        assert!(state.approve_token(next_token.clone(), after_expiry));
        assert!(state.is_approved(next_token, after_expiry));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test(flavor = "current_thread")]
    async fn test_main_ipc_capability_expiry_preserves_authenticated_connection() {
        let token = ipc::generate_one_time_ipc_token().unwrap();
        let path = std::env::temp_dir().join(format!("rustdesk-ipc-ttl-{}.sock", &token[..16]));
        let path = path.to_string_lossy().into_owned();
        let mut endpoint = ipc::Endpoint::new(path.clone());
        let mut incoming = endpoint.incoming().unwrap();
        let pair = tokio::join!(ipc::Endpoint::connect(&path), incoming.next());
        let mut client = ConnectionTmpl::new(pair.0.unwrap());
        let mut server = (Connection::new(pair.1.unwrap().unwrap()), false);
        assert!(super::approve_main_ipc_token(&token));
        assert!(connection_accepts_token(&mut client, &mut server, &token).await);

        // Expire only this test's token, leaving concurrent tests' approvals intact.
        super::main_ipc_capability_state()
            .lock()
            .unwrap()
            .approved
            .insert(token.clone(), std::time::Instant::now());
        assert!(!super::is_main_ipc_token_approved(&token));
        assert!(connection_accepts_token(&mut client, &mut server, &token).await);

        let pair = tokio::join!(ipc::Endpoint::connect(&path), incoming.next());
        let mut new_client = ConnectionTmpl::new(pair.0.unwrap());
        let mut new_server = (Connection::new(pair.1.unwrap().unwrap()), false);
        assert!(!connection_accepts_token(&mut new_client, &mut new_server, &token).await);
        drop((client, server, new_client, new_server, incoming));
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    async fn connection_accepts_token(
        client: &mut ConnectionTmpl<parity_tokio_ipc::ConnClient>,
        server: &mut (Connection, bool),
        token: &str,
    ) -> bool {
        client
            .send(&Data::MainIpcAuth(DataMainIpcAuth::Authenticate(
                token.to_owned(),
            )))
            .await
            .unwrap();
        let data = server
            .0
            .next_timeout(ipc::MAIN_IPC_AUTH_TIMEOUT_MS)
            .await
            .unwrap()
            .unwrap();
        let peer_uid = server.0.peer_uid();
        assert!(super::authorize_unix_main_ipc_connection(&server.0, ""));
        assert!(
            ipc::handle_unix_main_ipc_auth(&data, &mut server.0, peer_uid, &mut server.1).await
        );
        match client
            .next_timeout(ipc::MAIN_IPC_AUTH_TIMEOUT_MS)
            .await
            .unwrap()
        {
            Some(Data::MainIpcAuth(DataMainIpcAuth::Result(accepted))) => accepted,
            _ => panic!("Expected an authorization result"),
        }
    }

    #[test]
    #[cfg(windows)]
    fn test_windows_server_peer_policy() {
        assert!(super::is_allowed_windows_session_scoped_peer(
            true, None, None
        ));
        assert!(super::is_allowed_windows_session_scoped_peer(
            false,
            Some(1),
            Some(1)
        ));
        assert!(!super::is_allowed_windows_session_scoped_peer(
            false,
            Some(1),
            Some(2)
        ));
        assert!(!super::is_allowed_windows_session_scoped_peer(
            false,
            None,
            Some(1)
        ));
    }

    #[test]
    #[cfg(windows)]
    fn test_windows_portable_service_peer_policy() {
        assert!(super::is_allowed_windows_portable_service_peer(
            Some(true),
            None,
            None
        ));
        assert!(!super::is_allowed_windows_portable_service_peer(
            Some(false),
            Some(1),
            Some(1)
        ));
        assert!(!super::is_allowed_windows_portable_service_peer(
            Some(false),
            Some(1),
            Some(2)
        ));
        assert!(!super::is_allowed_windows_portable_service_peer(
            None,
            Some(1),
            Some(1)
        ));
    }

    #[cfg(all(windows, not(feature = "flutter")))]
    struct TempDirGuard(std::path::PathBuf);

    #[cfg(all(windows, not(feature = "flutter")))]
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    #[cfg(all(windows, not(feature = "flutter")))]
    fn test_portable_service_helper_trust_requires_content_match() {
        let unique = format!(
            "rustdesk-portable-helper-trust-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let base = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&base).unwrap();
        let _cleanup = TempDirGuard(base.clone());

        let current_exe = base.join("current.exe");
        let helper_exe = base.join("helper.exe");
        std::fs::write(&current_exe, b"trusted-binary").unwrap();
        std::fs::write(&helper_exe, b"tampered-binary").unwrap();

        assert!(
            !super::portable_service_helper_is_trusted(&helper_exe, &helper_exe, &current_exe),
            "helper trust check must reject path-match-only binaries with mismatched content"
        );
    }
}
