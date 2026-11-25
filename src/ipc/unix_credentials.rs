use super::{
    connect_root_service, connect_with_path,
    credentials::requires_approval,
    ipc_auth::ensure_peer_executable_matches_current_by_pid_opt,
    unix_unlock_pin::{self, Request, REQUIRED as AUTH_REQUIRED},
    Connection, Data,
};
use hbb_common::{
    anyhow::{anyhow, ensure},
    config::Config,
    libc, log, ResultType,
};

const IPC_TIMEOUT: u64 = 1_000;
pub(super) const PASSWORD_STORAGE_KEY: &str = "permanent-password-storage-and-salt";

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
pub(super) async fn verify_sync_client(connection: &Connection) -> ResultType<()> {
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
    authorized: bool,
}

impl Session {
    pub(super) fn new(connection: &Connection) -> Self {
        Self {
            authorized: connection.peer_uid() == Some(0),
        }
    }

    pub(super) async fn handle(&mut self, data: &Data, connection: &mut Connection) -> bool {
        let response = match data {
            Data::UnlockPin(request @ Request::Authorized(_)) => {
                // An accepted non-root peer shares the main server's UID. The root
                // service verifies that UID's settings grant for this connection.
                let accepted = check_with_root(request.clone()).await;
                self.authorized |= matches!(accepted, Ok(true));
                Data::UnlockPinResult(accepted.map_err(|err| err.to_string()))
            }
            _ if !self.authorized && requires_approval(data) => {
                Data::SecurityConfigError(AUTH_REQUIRED.into())
            }
            _ => return false,
        };
        if let Err(err) = connection.send(&response).await {
            log::trace!("Credential IPC response failed: {err}");
        }
        true
    }
}

pub(super) async fn for_read(ms_timeout: u64) -> ResultType<Connection> {
    let caller = uid();
    let target = if caller == 0 && super::USE_USER_MAIN_IPC.with(|scope| scope.get()) {
        super::user_main_ipc_server_uid()?
    } else {
        caller
    };
    verified_main(ms_timeout, target).await
}

pub(super) async fn for_write() -> ResultType<Connection> {
    let mut connection = for_read(IPC_TIMEOUT).await?;
    if uid() != 0 {
        use_cached(&mut connection).await?;
    }
    Ok(connection)
}

pub(super) async fn read_password_storage(
    connection: &mut Connection,
    ms_timeout: u64,
) -> ResultType<Option<String>> {
    connection
        .send(&Data::Config((PASSWORD_STORAGE_KEY.into(), None)))
        .await?;
    match connection.next_timeout(ms_timeout).await? {
        Some(Data::Config((name, value))) if name == PASSWORD_STORAGE_KEY => Ok(value),
        Some(Data::SecurityConfigError(error)) => Err(anyhow!(error)),
        _ => Ok(None),
    }
}

pub(super) async fn sync_password_storage(connection: &mut Connection) -> ResultType<()> {
    let value = read_password_storage(connection, IPC_TIMEOUT)
        .await?
        .ok_or_else(|| anyhow!("Missing permanent password storage response"))?;
    super::apply_permanent_password_storage_and_salt_payload(Some(&value))
}

async fn check_with_root(request: Request) -> ResultType<bool> {
    let mut service = connect_root_service(IPC_TIMEOUT).await?;
    unix_unlock_pin::exchange(&mut service, request).await
}

async fn use_cached(stream: &mut Connection) -> ResultType<()> {
    let request = Request::cached_authorization()?;
    ensure!(
        unix_unlock_pin::exchange(stream, request).await?,
        "{AUTH_REQUIRED}"
    );
    Ok(())
}

pub(super) async fn set_credential(key: &str, value: String) -> ResultType<()> {
    let mut connection = for_write().await?;
    connection.send_config(key, value).await?;
    match connection.next_timeout(IPC_TIMEOUT).await? {
        Some(Data::Config((name, Some(ack)))) if key == name && ack == "Y" => Ok(()),
        Some(Data::SecurityConfigError(error)) => Err(anyhow!(error)),
        _ => Err(anyhow!("Credential update rejected")),
    }
}

#[cfg(test)]
#[path = "unix_credentials_tests.rs"]
mod grant_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use hbb_common::tokio::{
        self,
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixListener,
        process::{Child, Command},
    };
    use std::{env, fs, os::unix::fs::DirBuilderExt, path::Path, process::Stdio};

    const ROLE: &str = "RUSTDESK_TEST_SYNC_PEER";
    const TEST_TIMEOUT_MS: u64 = 10_000;
    // libtest also exits successfully when an exact filter selects no tests.
    const OWNER_VERIFIED: i32 = 42;

    #[tokio::test]
    async fn service_sync_requires_main_listener_owner() -> ResultType<()> {
        let mut child = Command::new(env::current_exe()?);
        child
            .arg("--exact")
            .arg("ipc::unix_credentials::tests::service_sync_requires_main_listener_owner")
            .kill_on_drop(true);
        let Some(role) = env::var_os(ROLE) else {
            let result =
                hbb_common::timeout(TEST_TIMEOUT_MS, child.env(ROLE, "owner").output()).await??;
            assert_eq!(
                result.status.code(),
                Some(OWNER_VERIFIED),
                "{}",
                String::from_utf8_lossy(&result.stdout)
            );
            return Ok(());
        };
        // Start both isolated peers before dropping privileges, even in a root-only build directory.
        let other = if role == "owner" {
            Some(child.env(ROLE, "sender").stdin(Stdio::piped()).spawn()?)
        } else {
            None
        };
        if unsafe { hbb_common::libc::geteuid() } == 0 {
            const UNPRIVILEGED_UID: u32 = 65_534;
            if unsafe { hbb_common::libc::setuid(UNPRIVILEGED_UID) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        if let Some(other) = other {
            check_sync_senders(other).await?;
            std::process::exit(OWNER_VERIFIED);
        }
        let mut path = String::new();
        tokio::io::stdin().read_to_string(&mut path).await?;
        let mut socket = tokio::net::UnixStream::connect(path).await?;
        // Keep the sender alive until the verifier closes its side.
        assert_eq!(socket.read(&mut [0]).await?, 0);
        Ok(())
    }

    async fn check_sync_senders(mut other: Child) -> ResultType<()> {
        *hbb_common::config::APP_NAME.write().unwrap() =
            format!("rd-sync-{}", uuid::Uuid::new_v4());
        let main_path = Config::ipc_path("");
        let directory = Path::new(&main_path).parent().unwrap().to_owned();
        fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let sync_path = format!("{main_path}.sync");
        let _cleanup = crate::SimpleCallOnReturn {
            b: true,
            f: Box::new(move || fs::remove_dir_all(&directory).unwrap()),
        };
        let _main = UnixListener::bind(main_path)?;
        let mut sync = parity_tokio_ipc::Endpoint::new(sync_path.clone()).incoming()?;

        let _owner = tokio::net::UnixStream::connect(&sync_path).await?;
        let owner = Connection::new(sync.next().await.unwrap()?);
        verify_sync_client(&owner).await?;

        other
            .stdin
            .take()
            .unwrap()
            .write_all(sync_path.as_bytes())
            .await?;
        let sender = Connection::new(sync.next().await.unwrap()?);
        assert_eq!(sender.peer_uid(), owner.peer_uid());
        let result = verify_sync_client(&sender).await;
        drop(sender);
        assert!(other.wait().await?.success());
        assert_eq!(
            result.unwrap_err().to_string(),
            "Expected the user main IPC server"
        );
        Ok(())
    }
}
