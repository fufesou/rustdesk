use super::*;
use hbb_common::tokio::{net::TcpListener, time::timeout};

const TEST_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";
const NO_CONNECTION_WAIT: Duration = Duration::from_millis(50);

#[tokio::test]
async fn unsolicited_relay_is_rejected_before_any_tcp_connection() {
    let hbbs = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host = hbbs.local_addr().unwrap();
    let mediator = RendezvousMediator {
        addr: host.into_target_addr().unwrap(),
        host: host.to_string(),
        host_prefix: String::new(),
        keep_alive: 0,
        relay_requests: Default::default(),
    };
    let request = RequestRelay {
        uuid: TEST_UUID.to_owned(),
        relay_server: relay.local_addr().unwrap().to_string(),
        socket_addr: AddrMangle::encode("192.0.2.10:40000".parse().unwrap()).into(),
        ..Default::default()
    };
    let error = mediator
        .handle_request_relay(request, crate::server::new_for_test())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Relay address was not selected"));
    assert!(timeout(NO_CONNECTION_WAIT, hbbs.accept()).await.is_err());
    assert!(timeout(NO_CONNECTION_WAIT, relay.accept()).await.is_err());
}
