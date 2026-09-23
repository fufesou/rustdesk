use super::Client;
use hbb_common::{
    anyhow::{anyhow, Context},
    bail,
    config::CONNECT_TIMEOUT,
    log,
    rendezvous_proto::{
        punch_hole_response, rendezvous_message, ConnType, NatType, PunchHoleRequest,
        PunchHoleResponse, RendezvousMessage,
    },
    socket_client::connect_tcp,
    ResultType, Stream,
};

const RELAY_ATTEMPTS: usize = 3;

pub(super) struct RelaySetup<'a> {
    pub peer: &'a str,
    pub rendezvous_server: &'a str,
    pub key: &'a str,
    pub token: &'a str,
    pub conn_type: ConnType,
    pub switch_code: &'a str,
}

impl Client {
    pub(super) async fn request_relay_with_punch(setup: RelaySetup<'_>) -> ResultType<Stream> {
        for attempt in 1..=RELAY_ATTEMPTS {
            let mut socket = connect_tcp(setup.rendezvous_server, CONNECT_TIMEOUT)
                .await
                .with_context(|| "Failed to connect to rendezvous server")?;
            if !setup.key.is_empty() && (!setup.token.is_empty() || !setup.switch_code.is_empty()) {
                crate::secure_tcp(&mut socket, setup.key).await?;
            }
            let ipv4 = socket.local_addr().is_ipv4();
            log::info!("#{attempt} renew relay authorization, id: {}", setup.peer);
            // A fresh authenticated punch survives changed egress IPs and expired setup state.
            socket.send(&punch_request(&setup)).await?;
            let Some(message) =
                crate::get_next_nonkeyexchange_msg(&mut socket, Some(CONNECT_TIMEOUT)).await
            else {
                continue;
            };
            return finish_relay_request(&setup, message, ipv4).await;
        }
        bail!("Timeout");
    }
}

fn punch_request(setup: &RelaySetup<'_>) -> RendezvousMessage {
    let mut message = RendezvousMessage::new();
    message.set_punch_hole_request(PunchHoleRequest {
        id: setup.peer.to_owned(),
        token: setup.token.to_owned(),
        licence_key: setup.key.to_owned(),
        conn_type: setup.conn_type.into(),
        switch_code: setup.switch_code.to_owned(),
        version: crate::VERSION.to_owned(),
        force_relay: true,
        nat_type: NatType::SYMMETRIC.into(),
        ..Default::default()
    });
    message
}

async fn finish_relay_request(
    setup: &RelaySetup<'_>,
    message: RendezvousMessage,
    ipv4: bool,
) -> ResultType<Stream> {
    match message.union {
        Some(rendezvous_message::Union::RelayResponse(response)) => {
            if !response.refuse_reason.is_empty() {
                bail!(response.refuse_reason);
            }
            if response.relay_server.is_empty() {
                bail!("Missing relay address in forced relay response");
            }
            Client::create_relay(
                setup.peer,
                response.uuid,
                response.relay_server,
                setup.key,
                setup.conn_type,
                ipv4,
            )
            .await
        }
        Some(rendezvous_message::Union::PunchHoleResponse(response)) => {
            if response.socket_addr.is_empty() {
                return Err(punch_error(response));
            }
            if response.relay_server.is_empty() {
                bail!("Missing relay address in punch response");
            }
            log::info!("Server returned a LAN punch; requesting relay after renewed authorization");
            Client::request_relay(
                setup.peer,
                response.relay_server,
                setup.rendezvous_server,
                !response.pk.is_empty(),
                setup.key,
                setup.token,
                setup.conn_type,
                setup.switch_code,
            )
            .await
        }
        _ => bail!("Unexpected message in forced relay response"),
    }
}

fn punch_error(response: PunchHoleResponse) -> hbb_common::anyhow::Error {
    if !response.other_failure.is_empty() {
        return anyhow!(response.other_failure);
    }
    anyhow!(match response.failure.enum_value() {
        Ok(punch_hole_response::Failure::ID_NOT_EXIST) => "ID does not exist",
        Ok(punch_hole_response::Failure::OFFLINE) => "Remote desktop is offline",
        Ok(punch_hole_response::Failure::LICENSE_MISMATCH) => "Key mismatch",
        Ok(punch_hole_response::Failure::LICENSE_OVERUSE) => "Key overuse",
        _ => "other punch hole failure",
    })
}
