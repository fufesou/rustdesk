use super::Data;
use hbb_common::sha2::{Digest, Sha256};
use serde_derive::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt,
    time::{Duration, Instant},
};

pub(super) fn is_credential_key(key: &str) -> bool {
    matches!(key, "permanent-password" | "salt" | "unlock-pin")
}

pub(super) fn requires_approval(data: &Data) -> bool {
    match data {
        Data::Config((key, Some(_))) => is_credential_key(key),
        Data::Config((key, None)) => matches!(
            key.as_str(),
            "unlock-pin" | "permanent-password-storage-and-salt"
        ),
        Data::SyncConfig(_) => true,
        _ => false,
    }
}

// The elevated helper receives only the digest. The random bearer token stays in the GUI.
pub(super) type Grant = [u8; 32];

#[derive(Clone, Serialize, Deserialize)]
pub enum CredentialAuth {
    Approve(Grant),
    Pin(String, Grant),
    Use(Grant),
    Result(bool),
}

impl fmt::Debug for CredentialAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CredentialAuth")
    }
}

pub(super) fn digest(token: &Grant) -> Grant {
    Sha256::digest(token).into()
}

#[derive(Default)]
pub(super) struct Approvals {
    // A PIN change affects later PIN checks. Existing grants keep their idle timeout.
    grants: HashMap<Grant, Instant>,
    last_pin_attempt: Option<Instant>,
}

const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const PIN_INTERVAL: Duration = Duration::from_secs(1);
const MAX_GRANTS: usize = 16;

impl Approvals {
    pub(super) fn approve(&mut self, grant: Grant, now: Instant) -> bool {
        self.grants.retain(|_, deadline| *deadline > now);
        if self.grants.len() >= MAX_GRANTS && !self.grants.contains_key(&grant) {
            return false;
        }
        self.grants.insert(grant, now + IDLE_TIMEOUT);
        true
    }

    pub(super) fn approve_pin(
        &mut self,
        grant: Grant,
        pin: &str,
        expected: &str,
        now: Instant,
    ) -> bool {
        if self
            .last_pin_attempt
            .is_some_and(|last| now - last < PIN_INTERVAL)
        {
            return false;
        }
        self.last_pin_attempt = Some(now);
        !expected.is_empty()
            && hbb_common::sodiumoxide::utils::memcmp(pin.as_bytes(), expected.as_bytes())
            && self.approve(grant, now)
    }

    pub(super) fn authenticate(&mut self, token: &Grant, now: Instant) -> bool {
        let Some(deadline) = self.grants.get_mut(&digest(token)) else {
            return false;
        };
        if *deadline <= now {
            return false;
        }
        *deadline = now + IDLE_TIMEOUT;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_approved_secret_authorizes_a_credential_write() {
        let now = Instant::now();
        let token = [42; 32];
        let grant = digest(&token);
        let mut approvals = Approvals::default();
        assert!(!approvals.authenticate(&token, now));
        assert!(!approvals.approve_pin(grant, "wrong", "pin", now));
        assert!(!approvals.approve_pin(grant, "pin", "pin", now));
        let retry = now + PIN_INTERVAL;
        assert!(approvals.approve_pin(grant, "pin", "pin", retry));
        assert!(!approvals.authenticate(&grant, retry));
        assert!(approvals.authenticate(&token, retry));
        assert!(!approvals.authenticate(&token, retry + IDLE_TIMEOUT));
    }

    #[test]
    fn unapproved_main_connection_cannot_read_or_replace_credentials() {
        for key in ["permanent-password", "salt", "unlock-pin"] {
            for value in ["new value", ""] {
                assert!(requires_approval(&Data::Config((
                    key.into(),
                    Some(value.into())
                ))));
            }
        }
        for key in ["unlock-pin", "permanent-password-storage-and-salt"] {
            assert!(requires_approval(&Data::Config((key.into(), None))));
        }
        assert!(requires_approval(&Data::SyncConfig(Some(Box::default()))));
        assert!(requires_approval(&Data::SyncConfig(None)));
        assert!(!requires_approval(&Data::Config((
            "unlock-pin-set".into(),
            None
        ))));
        assert!(!requires_approval(&Data::Options(None)));
        assert!(!requires_approval(&Data::Socks(None)));
    }
}
