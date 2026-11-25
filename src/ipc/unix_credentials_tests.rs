use super::super::ipc_auth;
use super::{uid, Connection, Data, Session, AUTH_REQUIRED, IPC_TIMEOUT};
use hbb_common::tokio;
use parity_tokio_ipc::Endpoint;

async fn private_peers(case: &str) -> (Connection, Connection) {
    use std::{fs, os::unix::fs::DirBuilderExt};

    let dir = std::env::temp_dir().join(format!("rd-{case}-{}", std::process::id()));
    fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
    let path = dir.join("ipc");
    let mut endpoint = Endpoint::new(path.to_string_lossy().into_owned());
    let mut incoming = endpoint.incoming().unwrap();
    let (client, server) = tokio::join!(Endpoint::connect(&path), incoming.next());
    drop(incoming);
    fs::remove_file(path).unwrap();
    fs::remove_dir(dir).unwrap();
    (
        Connection::new(client.unwrap()),
        Connection::new(server.unwrap().unwrap()),
    )
}

#[tokio::test]
async fn unapproved_socket_rejects_full_configuration_writes() {
    let (mut client, mut server) = private_peers("read").await;
    assert_eq!(server.peer_uid(), Some(uid()));
    assert_eq!(
        ipc_auth::verify_service_uid(client.peer_uid()).is_ok(),
        uid() == 0
    );
    let mut session = Session { authorized: false };
    client
        .send(&Data::SyncConfig(Some(Box::default())))
        .await
        .unwrap();
    let request = server.next_timeout(IPC_TIMEOUT).await.unwrap().unwrap();
    assert!(session.handle(&request, &mut server).await);
    assert!(
        matches!(client.next_timeout(IPC_TIMEOUT).await.unwrap(), Some(Data::SecurityConfigError(e)) if e == AUTH_REQUIRED)
    );
}

#[tokio::test]
async fn locked_password_writer_requires_settings_unlock() {
    let (mut client, _server) = private_peers("locked-writer").await;
    let error = super::use_cached(&mut client).await.unwrap_err();
    assert_eq!(error.to_string(), AUTH_REQUIRED);
}

#[tokio::test]
async fn accepted_peer_password_write_requires_approval_or_root() {
    let (mut client, mut server) = private_peers("write").await;
    assert_eq!(server.peer_pid(), Some(std::process::id()));
    assert_eq!(server.peer_uid(), Some(uid()));
    assert!(ipc_auth::authorize_unix_main_ipc_connection(&server));
    let mut session = Session::new(&server);
    let value = std::process::id().to_string();
    client
        .send(&Data::Config(("permanent-password".into(), Some(value))))
        .await
        .unwrap();
    let request = server.next_timeout(IPC_TIMEOUT).await.unwrap().unwrap();
    let handled = session.handle(&request, &mut server).await;
    if uid() == 0 {
        assert!(!handled);
        return;
    }
    assert!(
        handled,
        "an unapproved write must not reach the configuration handler"
    );
    assert!(
        matches!(client.next_timeout(IPC_TIMEOUT).await.unwrap(), Some(Data::SecurityConfigError(e)) if e == AUTH_REQUIRED)
    );
}
