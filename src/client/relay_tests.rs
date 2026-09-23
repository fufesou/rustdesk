use super::*;
use hbb_common::{
    tcp::FramedStream,
    tokio::{net::TcpListener, process::Command, task::JoinHandle, time::timeout},
};

const NO_CONNECTION_WAIT: Duration = Duration::from_millis(50);
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const CHILD_TIMEOUT: Duration = Duration::from_secs(45);
const CHILD_MARKER: &str = "RUSTDESK_RELAY_RENEWAL_TEST_CHILD";
const RENEWAL_TEST: &str = "client::relay_tests::relay_fallback_renews_authorization";
const TEST_PEER: &str = "relay-test-peer";
const TEST_KEY: &str = "nonsecret-test-rendezvous-key";
const TEST_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";

#[tokio::test]
async fn invalid_uuid_is_rejected_before_connecting_to_relay() {
    let relay = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let result = Client::create_relay(
        "test-peer",
        "marker\r\nPING\r\n".to_owned(),
        relay.local_addr().unwrap().to_string(),
        "test-key",
        ConnType::DEFAULT_CONN,
        true,
    )
    .await;
    assert!(matches!(result, Err(ref err) if err.to_string() == "Invalid relay UUID"));
    assert!(timeout(NO_CONNECTION_WAIT, relay.accept()).await.is_err());
}

#[tokio::test]
async fn relay_fallback_renews_authorization() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        // Run with an unused configuration namespace in an isolated process.
        *config::APP_NAME.write().unwrap() = format!("relay-renewal-test-{}", Uuid::new_v4());
        renews_without_an_existing_punch().await;
        preserves_legacy_intranet_relay_fallback().await;
        preserves_punch_authorization_errors().await;
        rejects_invalid_new_relay_responses().await;
        return;
    }
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", RENEWAL_TEST, "--nocapture"])
        .env_clear()
        .env(CHILD_MARKER, "1")
        .kill_on_drop(true);
    let status = timeout(CHILD_TIMEOUT, child.status())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "isolated relay renewal failed: {status}");
}

struct RelayFixture {
    hbbs: TcpListener,
    relay: TcpListener,
}

impl RelayFixture {
    async fn new() -> Self {
        Self {
            hbbs: TcpListener::bind("127.0.0.1:0").await.unwrap(),
            relay: TcpListener::bind("127.0.0.1:0").await.unwrap(),
        }
    }

    async fn request(&self) -> (FramedStream, JoinHandle<ResultType<Stream>>) {
        let rendezvous = self.hbbs.local_addr().unwrap().to_string();
        let client = tokio::spawn(async move {
            Client::request_relay_with_punch(RelaySetup {
                peer: TEST_PEER,
                rendezvous_server: &rendezvous,
                key: TEST_KEY,
                token: "",
                conn_type: ConnType::FILE_TRANSFER,
                switch_code: "",
            })
            .await
        });
        let (stream, message) = accept_frame(&self.hbbs).await;
        assert!(message.has_punch_hole_request());
        let request = message.punch_hole_request();
        assert_eq!(request.id, TEST_PEER);
        assert_eq!(request.licence_key, TEST_KEY);
        assert_eq!(request.conn_type.enum_value(), Ok(ConnType::FILE_TRANSFER));
        assert!(request.force_relay);
        assert_eq!(request.nat_type.enum_value(), Ok(NatType::SYMMETRIC));
        assert_eq!(request.version, crate::VERSION);
        (stream, client)
    }

    async fn assert_no_relay_connection(&self) {
        assert!(timeout(NO_CONNECTION_WAIT, self.relay.accept())
            .await
            .is_err());
    }
}

async fn renews_without_an_existing_punch() {
    let fixture = RelayFixture::new().await;
    let (mut hbbs, client) = fixture.request().await;
    let mut response = RendezvousMessage::new();
    response.set_relay_response(RelayResponse {
        uuid: TEST_UUID.to_owned(),
        relay_server: fixture.relay.local_addr().unwrap().to_string(),
        ..Default::default()
    });
    hbbs.send(&response).await.unwrap();
    let (_, message) = accept_frame(&fixture.relay).await;
    assert!(message.has_request_relay());
    let request = message.request_relay();
    assert_eq!(request.uuid, TEST_UUID);
    assert_eq!(request.licence_key, TEST_KEY);
    assert_eq!(request.conn_type.enum_value(), Ok(ConnType::FILE_TRANSFER));
    assert!(timeout(IO_TIMEOUT, client).await.unwrap().unwrap().is_ok());
}

async fn preserves_legacy_intranet_relay_fallback() {
    let fixture = RelayFixture::new().await;
    let (mut hbbs, client) = fixture.request().await;
    let relay_server = fixture.relay.local_addr().unwrap().to_string();
    let mut punch = PunchHoleResponse {
        socket_addr: AddrMangle::encode("192.0.2.20:40000".parse().unwrap()).into(),
        relay_server: relay_server.clone(),
        ..Default::default()
    };
    punch.set_is_local(true);
    let mut response = RendezvousMessage::new();
    response.set_punch_hole_response(punch);
    hbbs.send(&response).await.unwrap();
    // Old hbbs consumes the reply route, so the relay needs its own TCP connection.
    let (mut retry, message) = accept_frame(&fixture.hbbs).await;
    assert!(message.has_request_relay());
    let request = message.request_relay();
    assert_eq!(request.id, TEST_PEER);
    assert_eq!(request.relay_server, relay_server);
    assert!(!request.secure);
    base::relay::validate_uuid(&request.uuid).unwrap();
    response.set_relay_response(RelayResponse::new());
    retry.send(&response).await.unwrap();
    let (_, message) = accept_frame(&fixture.relay).await;
    assert!(message.has_request_relay());
    let handshake = message.request_relay();
    assert_eq!(handshake.uuid, request.uuid);
    assert_eq!(handshake.licence_key, TEST_KEY);
    assert!(timeout(IO_TIMEOUT, client).await.unwrap().unwrap().is_ok());
}

async fn accept_frame(listener: &TcpListener) -> (FramedStream, RendezvousMessage) {
    let (stream, addr) = timeout(IO_TIMEOUT, listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut stream = FramedStream::from(stream, addr);
    let bytes = timeout(IO_TIMEOUT, stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let message = RendezvousMessage::parse_from_bytes(&bytes).unwrap();
    (stream, message)
}

async fn preserves_punch_authorization_errors() {
    for (failure, other_failure, expected) in [
        (
            punch_hole_response::Failure::LICENSE_MISMATCH,
            "",
            "Key mismatch",
        ),
        (
            punch_hole_response::Failure::OFFLINE,
            "",
            "Remote desktop is offline",
        ),
        (
            punch_hole_response::Failure::ID_NOT_EXIST,
            "Permission denied",
            "Permission denied",
        ),
    ] {
        let fixture = RelayFixture::new().await;
        let (mut hbbs, client) = fixture.request().await;
        let mut response = RendezvousMessage::new();
        response.set_punch_hole_response(PunchHoleResponse {
            failure: failure.into(),
            other_failure: other_failure.to_owned(),
            ..Default::default()
        });
        hbbs.send(&response).await.unwrap();
        let result = timeout(IO_TIMEOUT, client).await.unwrap().unwrap();
        assert!(matches!(result, Err(err) if err.to_string() == expected));
        fixture.assert_no_relay_connection().await;
    }
}

async fn rejects_invalid_new_relay_responses() {
    for (uuid, refuse_reason, expected) in [
        ("marker\r\nPING\r\n", "", "Invalid relay UUID"),
        ("", "Relay denied", "Relay denied"),
    ] {
        let fixture = RelayFixture::new().await;
        let (mut hbbs, client) = fixture.request().await;
        let mut response = RendezvousMessage::new();
        response.set_relay_response(RelayResponse {
            uuid: uuid.to_owned(),
            relay_server: fixture.relay.local_addr().unwrap().to_string(),
            refuse_reason: refuse_reason.to_owned(),
            ..Default::default()
        });
        hbbs.send(&response).await.unwrap();
        let result = timeout(IO_TIMEOUT, client).await.unwrap().unwrap();
        assert!(matches!(result, Err(err) if err.to_string() == expected));
        fixture.assert_no_relay_connection().await;
    }
}
