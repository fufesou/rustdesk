use super::*;
use hbb_common::{
    tcp::FramedStream,
    tokio::{net::TcpListener, process::Command, time::timeout},
};

const TEST_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";
const NO_CONNECTION_WAIT: Duration = Duration::from_millis(50);
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const CHILD_TIMEOUT: Duration = Duration::from_secs(45);
const CHILD_MARKER: &str = "RUSTDESK_RELAY_REPLY_TOKEN_TEST_CHILD";
const TOKEN_TEST: &str = "rendezvous_mediator::relay_tests::echoes_optional_tokens_only_to_hbbs";

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

#[tokio::test]
async fn echoes_optional_tokens_only_to_hbbs() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        *config::APP_NAME.write().unwrap() = format!("relay-token-test-{}", Uuid::new_v4());
        for token in [Vec::new(), Uuid::new_v4().as_bytes().to_vec()] {
            for kind in [
                ReplyKind::Punch,
                ReplyKind::Local,
                ReplyKind::Forced,
                ReplyKind::Relay,
            ] {
                capture_reply(kind, token.clone()).await;
            }
        }
        return;
    }
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", TOKEN_TEST, "--nocapture"])
        .env_clear()
        .env(CHILD_MARKER, "1")
        .kill_on_drop(true);
    let status = timeout(CHILD_TIMEOUT, child.status())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "isolated token echo failed: {status}");
}

#[derive(Clone, Copy)]
enum ReplyKind {
    Punch,
    Local,
    Forced,
    Relay,
}

struct ReplyFixture {
    hbbs: TcpListener,
    relay: TcpListener,
    controller: TcpListener,
    mediator: RendezvousMediator,
}

impl ReplyFixture {
    async fn new() -> Self {
        let hbbs = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = hbbs.local_addr().unwrap();
        Self {
            hbbs,
            relay: TcpListener::bind("127.0.0.1:0").await.unwrap(),
            controller: TcpListener::bind("127.0.0.1:0").await.unwrap(),
            mediator: RendezvousMediator {
                addr: host.into_target_addr().unwrap(),
                host: host.to_string(),
                host_prefix: String::new(),
                keep_alive: 0,
                relay_requests: Default::default(),
            },
        }
    }

    fn request(&self, kind: ReplyKind, token: Vec<u8>) -> rendezvous_message::Union {
        let socket_addr = AddrMangle::encode(self.controller.local_addr().unwrap()).into();
        let relay_server = self.relay.local_addr().unwrap().to_string();
        let relay_reply_token = token.into();
        match kind {
            ReplyKind::Punch | ReplyKind::Forced => {
                rendezvous_message::Union::PunchHole(PunchHole {
                    socket_addr,
                    relay_server,
                    relay_reply_token,
                    force_relay: matches!(kind, ReplyKind::Forced),
                    nat_type: NatType::ASYMMETRIC.into(),
                    ..Default::default()
                })
            }
            ReplyKind::Local => rendezvous_message::Union::FetchLocalAddr(FetchLocalAddr {
                socket_addr,
                relay_server,
                relay_reply_token,
                ..Default::default()
            }),
            ReplyKind::Relay => rendezvous_message::Union::RequestRelay(RequestRelay {
                socket_addr,
                relay_server,
                relay_reply_token,
                uuid: TEST_UUID.to_owned(),
                ..Default::default()
            }),
        }
    }
}

async fn answer(
    mediator: RendezvousMediator,
    request: rendezvous_message::Union,
) -> ResultType<()> {
    let server = crate::server::new_for_test();
    match request {
        rendezvous_message::Union::PunchHole(ph) => mediator.handle_punch_hole(ph, server).await,
        rendezvous_message::Union::FetchLocalAddr(fla) => {
            mediator.handle_intranet(fla, server).await
        }
        rendezvous_message::Union::RequestRelay(rr) => {
            mediator.relay_requests.lock().await.remember(
                AddrMangle::decode(&rr.socket_addr),
                &rr.relay_server,
                Instant::now(),
            );
            mediator.handle_request_relay(rr, server).await
        }
        _ => panic!("unexpected test request"),
    }
}

async fn capture_reply(kind: ReplyKind, token: Vec<u8>) {
    let fixture = ReplyFixture::new().await;
    let request = fixture.request(kind, token.clone());
    let task = tokio::spawn(answer(fixture.mediator.clone(), request));
    let (_hbbs, response) = accept_message(&fixture.hbbs).await;
    let echoed = match response.union {
        Some(rendezvous_message::Union::PunchHoleSent(response)) => response.relay_reply_token,
        Some(rendezvous_message::Union::LocalAddr(response)) => response.relay_reply_token,
        Some(rendezvous_message::Union::RelayResponse(response)) => response.relay_reply_token,
        _ => panic!("unexpected reply"),
    };
    assert_eq!(echoed.as_ref(), token.as_slice());
    if matches!(kind, ReplyKind::Forced | ReplyKind::Relay) {
        let (_relay, message) = accept_message(&fixture.relay).await;
        let Some(rendezvous_message::Union::RequestRelay(handshake)) = message.union else {
            panic!("expected a relay handshake");
        };
        assert!(handshake.relay_reply_token.is_empty());
        base::relay::validate_uuid(&handshake.uuid).unwrap();
    }
    task.abort();
    let _ = task.await;
}

async fn accept_message(listener: &TcpListener) -> (FramedStream, RendezvousMessage) {
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
    (stream, RendezvousMessage::parse_from_bytes(&bytes).unwrap())
}
