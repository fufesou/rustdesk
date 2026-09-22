use super::{
    connect_with_path,
    credentials::{digest, requires_approval, Approvals, CredentialAuth, Grant},
    ipc_auth::ensure_peer_executable_matches_current_by_pid_opt,
    Connection, Data,
};
use hbb_common::{
    anyhow::{anyhow, ensure},
    config::Config,
    libc, log,
    rand::{rngs::OsRng, RngCore},
    tokio, ResultType,
};
use std::{
    sync::{Mutex, OnceLock},
    time::Instant,
};

const IPC_TIMEOUT: u64 = 1_000;
const AUTH_REQUIRED: &str = "Authentication Required";
static APPROVALS: OnceLock<Mutex<Approvals>> = OnceLock::new();
static CLIENT_TOKEN: Mutex<Option<Grant>> = Mutex::new(None);

fn uid() -> u32 {
    unsafe { libc::geteuid() }
}

pub(super) async fn verified_main(ms_timeout: u64, target: u32) -> ResultType<Connection> {
    let connection = connect_with_path(ms_timeout, &Config::ipc_path_for_uid(target, "")).await?;
    verify_main_peer(&connection, target)?;
    Ok(connection)
}

fn verify_main_peer(connection: &Connection, target: u32) -> ResultType<()> {
    ensure!(
        connection.peer_uid() == Some(target),
        "Unexpected main IPC server UID"
    );
    ensure_peer_executable_matches_current_by_pid_opt(connection.peer_pid(), "")
}

// Full service sync delegates credential changes to the actual user main server.
async fn verify_sync_client(connection: &Connection) -> ResultType<()> {
    let uid = connection
        .peer_uid()
        .ok_or_else(|| anyhow!("Missing sync client UID"))?;
    if uid == 0 {
        return Ok(());
    }
    let pid = connection
        .peer_pid()
        .ok_or_else(|| anyhow!("Missing sync client PID"))?;
    let main = connect_with_path(IPC_TIMEOUT, &Config::ipc_path_for_uid(uid, "")).await?;
    ensure!(
        main.peer_uid() == Some(uid) && main.peer_pid() == Some(pid),
        "Expected the user main IPC server"
    );
    Ok(())
}

pub(super) struct Session {
    root: bool,
    authorized: bool,
}

pub(super) async fn session_for(postfix: &str, stream: &Connection) -> ResultType<Option<Session>> {
    if postfix == crate::POSTFIX_SERVICE {
        verify_sync_client(stream).await?;
    }
    Ok((postfix.is_empty() && crate::platform::is_installed()).then(|| Session::new(stream)))
}

impl Session {
    pub(super) fn new(connection: &Connection) -> Self {
        let root = connection.peer_uid() == Some(0);
        Self {
            root,
            authorized: root,
        }
    }

    fn authorize(&mut self, request: &CredentialAuth) -> bool {
        let mut approvals = APPROVALS.get_or_init(Default::default).lock().unwrap();
        let now = Instant::now();
        if let CredentialAuth::Approve(grant) = request {
            return self.root && approvals.approve(*grant, now);
        }
        match request {
            CredentialAuth::Use(token) => {
                let accepted = approvals.authenticate(token, now);
                // The idle timeout applies when opening a connection. A verified socket
                // stays authorized until disconnect, including across PIN changes.
                self.authorized |= accepted;
                accepted
            }
            CredentialAuth::Pin(pin, grant) => {
                approvals.approve_pin(*grant, pin, &Config::get_unlock_pin(), now)
            }
            _ => false,
        }
    }

    pub(super) async fn handle(&mut self, data: &Data, connection: &mut Connection) -> bool {
        let response = match data {
            Data::CredentialAuth(request) => {
                Data::CredentialAuth(CredentialAuth::Result(self.authorize(request)))
            }
            _ if !self.authorized && requires_approval(data) => {
                Data::SecurityConfigError(AUTH_REQUIRED.into())
            }
            Data::Config((key, None))
                if key == "unlock-pin-set" || key == "local-permanent-password-set" =>
            {
                let present = if key == "unlock-pin-set" {
                    !Config::get_unlock_pin().is_empty()
                } else {
                    Config::has_local_permanent_password()
                };
                Data::Config((key.clone(), Some(if present { "Y" } else { "N" }.into())))
            }
            _ => return false,
        };
        if let Err(err) = connection.send(&response).await {
            log::trace!("Credential IPC response failed: {err}");
        }
        true
    }
}

async fn exchange(connection: &mut Connection, request: CredentialAuth) -> ResultType<bool> {
    connection.send(&Data::CredentialAuth(request)).await?;
    match connection.next_timeout(IPC_TIMEOUT).await? {
        Some(Data::CredentialAuth(CredentialAuth::Result(result))) => Ok(result),
        _ => Err(anyhow!("Invalid credential authorization response")),
    }
}

async fn use_cached(connection: &mut Connection) -> ResultType<bool> {
    let token = *CLIENT_TOKEN.lock().unwrap();
    let Some(token) = token else { return Ok(false) };
    if exchange(connection, CredentialAuth::Use(token)).await? {
        return Ok(true);
    }
    let mut current = CLIENT_TOKEN.lock().unwrap();
    if *current == Some(token) {
        *current = None;
    }
    Ok(false)
}

async fn unlock(connection: &mut Connection, pin: Option<String>) -> ResultType<bool> {
    let mut token = Grant::default();
    OsRng
        .try_fill_bytes(&mut token)
        .map_err(|err| anyhow!("Failed to generate credential token: {err}"))?;
    let grant = digest(&token);
    if let Some(pin) = pin {
        if !exchange(connection, CredentialAuth::Pin(pin, grant)).await? {
            return Ok(false);
        }
    } else {
        tokio::task::spawn_blocking(move || elevate(uid(), grant)).await??;
    }
    ensure!(
        exchange(connection, CredentialAuth::Use(token)).await?,
        "{AUTH_REQUIRED}"
    );
    *CLIENT_TOKEN.lock().unwrap() = Some(token);
    Ok(true)
}

pub(super) async fn for_write() -> ResultType<Connection> {
    let caller = uid();
    let target = if caller == 0 && super::USE_USER_MAIN_IPC.with(|scope| scope.get()) {
        super::user_main_ipc_server_uid()?
    } else {
        caller
    };
    let mut connection = verified_main(IPC_TIMEOUT, target).await?;
    if caller == 0 {
        return Ok(connection);
    }
    if !use_cached(&mut connection).await? {
        #[cfg(feature = "flutter")]
        return Err(anyhow!(AUTH_REQUIRED));
        #[cfg(not(feature = "flutter"))]
        ensure!(unlock(&mut connection, None).await?, "{AUTH_REQUIRED}");
    }
    Ok(connection)
}

pub(super) async fn set_credential(key: &str, value: String) -> ResultType<()> {
    let mut connection = for_write().await?;
    // A root CLI targeting a user's host also updates its separate root configuration.
    let pin_for_root_config =
        (key == "unlock-pin" && uid() == 0 && connection.peer_uid().is_some_and(|peer| peer != 0))
            .then(|| value.clone());
    connection.send_config(key, value).await?;
    match connection.next_timeout(IPC_TIMEOUT).await? {
        Some(Data::Config((name, Some(ack)))) if key == name && ack == "Y" => {
            if let Some(pin) = pin_for_root_config {
                Config::set_unlock_pin(&pin);
            }
            Ok(())
        }
        Some(Data::SecurityConfigError(error)) => Err(anyhow!(error)),
        _ => Err(anyhow!("Credential update rejected")),
    }
}

#[cfg(feature = "flutter")]
#[tokio::main(flavor = "current_thread")]
pub(crate) async fn gui_request(action: &str) -> ResultType<String> {
    gui_request_at(action, &Config::ipc_path_for_uid(uid(), "")).await
}

#[cfg(any(feature = "flutter", test))]
async fn gui_request_at(action: &str, path: &str) -> ResultType<String> {
    use std::io::ErrorKind::{ConnectionRefused, NotFound};

    let mut connection = match connect_with_path(IPC_TIMEOUT, path).await {
        Ok(connection) => connection,
        Err(err) => {
            let kind = err.downcast_ref::<std::io::Error>().map(|io| io.kind());
            if action == "network-status" && matches!(kind, Some(NotFound | ConnectionRefused)) {
                return Ok("offline".into());
            }
            return Err(err);
        }
    };
    verify_main_peer(&connection, uid())?;
    let result = match action {
        "pin-set" => match super::read_config(&mut connection, "unlock-pin-set", IPC_TIMEOUT)
            .await?
            .as_deref()
        {
            Some("Y") => true,
            Some("N") => false,
            _ => return Err(anyhow!("Invalid unlock PIN state response")),
        },
        "status" | "network-status" => uid() == 0 || use_cached(&mut connection).await?,
        "admin" => uid() == 0 || unlock(&mut connection, None).await?,
        _ => {
            let pin = action
                .strip_prefix("pin:")
                .ok_or_else(|| anyhow!("Unknown credential request"))?;
            unlock(&mut connection, Some(pin.to_owned())).await?
        }
    };
    Ok(result.to_string())
}

#[tokio::main(flavor = "current_thread")]
pub(crate) async fn approve(target: &str, encoded: &str) -> ResultType<()> {
    ensure!(
        uid() == 0 && crate::platform::is_installed(),
        "{AUTH_REQUIRED}"
    );
    let mut grant = Grant::default();
    hex::decode_to_slice(encoded, &mut grant)?;
    let mut connection = verified_main(IPC_TIMEOUT, target.parse()?).await?;
    ensure!(
        exchange(&mut connection, CredentialAuth::Approve(grant)).await?,
        "Credential approval rejected"
    );
    Ok(())
}

fn elevate(target: u32, grant: Grant) -> ResultType<()> {
    let exe = std::env::current_exe()?;
    let exe = exe
        .to_str()
        .ok_or_else(|| anyhow!("Invalid executable path"))?;
    let target = target.to_string();
    let grant = hex::encode(grant);
    #[cfg(target_os = "linux")]
    let status = std::process::Command::new(exe)
        .args(["-gtk-sudo", "credential"])
        .args([exe, "--approve-credential", &target, &grant])
        .status()?;
    #[cfg(target_os = "macos")]
    const SCRIPT: &str = r#"on run argv
        set cmd to quoted form of (item 1 of argv) & " --approve-credential "
        set cmd to cmd & quoted form of (item 2 of argv) & " " & quoted form of (item 3 of argv)
        do shell script cmd with administrator privileges
    end run"#;
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", SCRIPT, exe, &target, &grant])
        .status()?;
    ensure!(status.success(), "{AUTH_REQUIRED}");
    Ok(())
}

#[cfg(test)]
#[path = "unix_credentials_tests.rs"]
mod tests;
