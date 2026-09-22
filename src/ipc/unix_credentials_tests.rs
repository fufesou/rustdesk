use super::{gui_request_at, tokio, uid, Connection, Data, Session, AUTH_REQUIRED, IPC_TIMEOUT};
use parity_tokio_ipc::Endpoint;

#[tokio::test]
async fn only_network_settings_can_unlock_offline() {
    let missing = std::env::temp_dir().join(format!("rd-offline-network-{}", std::process::id()));
    let stopped = missing.with_extension("sock");
    drop(std::os::unix::net::UnixListener::bind(&stopped).unwrap());
    for path in [&missing, &stopped] {
        let path = path.to_str().unwrap();
        assert_eq!(
            gui_request_at("network-status", path).await.unwrap(),
            "offline"
        );
        assert!(gui_request_at("status", path).await.is_err());
        assert!(gui_request_at("admin", path).await.is_err());
    }
    std::fs::remove_file(stopped).unwrap();
}

#[tokio::test]
async fn unapproved_socket_reports_denied_credential_reads() {
    let path = std::env::temp_dir().join(format!("rd-credential-test-{}.sock", std::process::id()));
    let mut endpoint = Endpoint::new(path.to_string_lossy().into_owned());
    let mut incoming = endpoint.incoming().unwrap();
    let (client, server) = tokio::join!(Endpoint::connect(&path), incoming.next());
    let mut client = Connection::new(client.unwrap());
    let mut server = Connection::new(server.unwrap().unwrap());
    assert_eq!(server.peer_uid(), Some(uid()));
    assert_eq!(
        super::super::ipc_auth::verify_service_uid(client.peer_uid()).is_ok(),
        uid() == 0
    );
    let mut session = Session {
        root: false,
        authorized: false,
    };
    client
        .send(&Data::Config(("unlock-pin".into(), None)))
        .await
        .unwrap();
    let request = server.next_timeout(IPC_TIMEOUT).await.unwrap().unwrap();
    assert!(session.handle(&request, &mut server).await);
    assert!(
        matches!(client.next_timeout(IPC_TIMEOUT).await.unwrap(), Some(Data::SecurityConfigError(e)) if e == AUTH_REQUIRED)
    );
    drop(incoming);
    std::fs::remove_file(path).unwrap();
}
