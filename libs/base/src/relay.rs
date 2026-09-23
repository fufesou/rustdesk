use hbb_common::{
    uuid::{Uuid, Variant, Version},
    TargetAddr,
};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

const RELAY_SETUP_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Default)]
pub struct RelayRequests {
    selected: HashMap<(IpAddr, String), Instant>,
}

fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        _ => ip,
    }
}

impl RelayRequests {
    pub fn remember(&mut self, peer: SocketAddr, relay: &str, now: Instant) {
        self.selected.retain(|_, expires| now < *expires);
        self.selected.insert(
            (normalize_ip(peer.ip()), relay.to_owned()),
            now + RELAY_SETUP_TIMEOUT,
        );
    }

    pub fn select(
        &self,
        peer: SocketAddr,
        relay: &str,
        now: Instant,
    ) -> Result<String, &'static str> {
        // Legacy controllers open a new TCP socket for each relay attempt.
        let key = (normalize_ip(peer.ip()), relay.to_owned());
        if self
            .selected
            .get(&key)
            .is_some_and(|expires| now < *expires)
        {
            return Ok(key.1);
        }
        // Older hbbs versions rewrite the controller's LAN/public relay address.
        let mut choices = self
            .selected
            .iter()
            .filter(|((ip, _), expires)| *ip == key.0 && now < **expires)
            .map(|((_, relay), _)| relay);
        let selected = choices
            .next()
            .ok_or("Relay address was not selected for this peer's punch exchange")?;
        if choices.next().is_some() {
            return Err("Relay request does not identify a unique selected relay");
        }
        Ok(selected.clone())
    }
}

pub fn validate_uuid(value: &str) -> Result<(), &'static str> {
    let uuid = Uuid::parse_str(value).map_err(|_| "Invalid relay UUID")?;
    if uuid.get_version() != Some(Version::Random)
        || uuid.get_variant() != Variant::RFC4122
        || uuid.hyphenated().to_string() != value
    {
        return Err("Invalid relay UUID");
    }
    Ok(())
}

pub fn is_rendezvous_source(source: &TargetAddr<'_>, expected: &TargetAddr<'_>) -> bool {
    match (source, expected) {
        (TargetAddr::Ip(source), TargetAddr::Ip(expected)) => {
            hbb_common::try_into_v4(*source) == hbb_common::try_into_v4(*expected)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELAY_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";

    fn peer() -> SocketAddr {
        "192.0.2.10:40000".parse().unwrap()
    }

    #[test]
    fn rejects_injected_and_noncanonical_uuids() {
        assert!(validate_uuid(RELAY_UUID).is_ok());
        for value in [
            "",
            "marker\r\nPING\r\n",
            "550e8400-e29b-41d4-a716-446655440000\r\nPING\r\n",
            "550e8400e29b41d4a716446655440000",
            "550E8400-E29B-41D4-A716-446655440000",
            "550e8400-e29b-11d4-a716-446655440000",
            "00000000-0000-0000-0000-000000000000",
        ] {
            assert!(validate_uuid(value).is_err(), "accepted {:?}", value);
        }
    }

    #[test]
    fn rejects_unsolicited_and_ambiguous_relay_destinations() {
        let mut relays = RelayRequests::default();
        let now = Instant::now();
        assert!(relays.select(peer(), "127.0.0.1:6379", now).is_err());
        relays.remember(peer(), "relay.example.test:21117", now);
        relays.remember(peer(), "second.example.test:21117", now);
        for destination in [
            "127.0.0.1:6379",
            "attacker.example.test:21117",
            "relay.example.test:6379",
        ] {
            assert!(relays.select(peer(), destination, now).is_err());
        }
    }

    #[test]
    fn retains_its_own_relay_choice_when_legacy_hbbs_rewrites_the_address() {
        let mut relays = RelayRequests::default();
        let now = Instant::now();
        let selected = "10.0.0.5:22000";
        relays.remember(peer(), selected, now);
        for advertised in [
            "public.example.test:22000",
            "127.0.0.1:6379",
            "attacker.example.test",
        ] {
            assert_eq!(relays.select(peer(), advertised, now).unwrap(), selected);
        }
    }

    #[test]
    fn preserves_selected_private_relays_custom_ports_and_legacy_retries() {
        let mut relays = RelayRequests::default();
        let now = Instant::now();
        let choices = [
            "relay.example.test",
            "10.0.0.5:22000",
            "[fd00::1]:22001",
            "127.0.0.1:22002",
        ];
        for destination in choices {
            relays.remember(peer(), destination, now);
        }
        for destination in choices {
            let retry = "192.0.2.10:40001".parse().unwrap();
            assert!(relays.select(retry, destination, now).is_ok());
            assert!(relays.select(peer(), destination, now).is_ok());
        }
    }

    #[test]
    fn excludes_other_controllers_and_expired_choices() {
        let mut relays = RelayRequests::default();
        let now = Instant::now();
        relays.remember(peer(), "relay.test", now);
        assert!(relays
            .select("192.0.2.11:40000".parse().unwrap(), "relay.test", now)
            .is_err());
        assert!(relays
            .select(peer(), "relay.test", now + RELAY_SETUP_TIMEOUT)
            .is_err());
    }

    #[test]
    fn fresh_punch_refreshes_expired_selection_after_controller_address_change() {
        let mut relays = RelayRequests::default();
        let now = Instant::now();
        relays.remember(peer(), "old.test", now);
        let later = now + RELAY_SETUP_TIMEOUT + Duration::from_secs(1);
        let current = "198.51.100.10:40001".parse().unwrap();
        let relay = "10.0.0.5:22000";
        assert!(relays.select(current, relay, later).is_err());
        relays.remember(current, relay, later);
        assert_eq!(relays.select(current, relay, later), Ok(relay.to_owned()));
        assert!(relays.select(peer(), "old.test", later).is_err());
    }

    #[test]
    fn handles_ipv4_mapped_addresses() {
        let mut relays = RelayRequests::default();
        let now = Instant::now();
        relays.remember(peer(), "relay.test", now);
        assert!(relays
            .select(
                "[::ffff:192.0.2.10]:40001".parse().unwrap(),
                "relay.test",
                now
            )
            .is_ok());
    }

    #[test]
    fn only_accepts_the_configured_rendezvous_udp_source() {
        let expected = hbb_common::TargetAddr::Ip("192.0.2.20:21116".parse().unwrap());
        assert!(is_rendezvous_source(&expected, &expected));
        for source in ["192.0.2.30:21116", "192.0.2.20:21117"] {
            let source = hbb_common::TargetAddr::Ip(source.parse().unwrap());
            assert!(!is_rendezvous_source(&source, &expected));
        }
    }
}
