use super::Data;

pub(super) fn is_credential_key(key: &str) -> bool {
    matches!(key, "permanent-password" | "salt")
}

pub(super) fn requires_approval(data: &Data) -> bool {
    match data {
        Data::Config((key, Some(_))) => is_credential_key(key),
        Data::SyncConfig(Some(_)) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_host_credential_writes_require_approval() {
        for key in ["permanent-password", "salt"] {
            for value in ["new value", ""] {
                assert!(requires_approval(&Data::Config((
                    key.into(),
                    Some(value.into())
                ))));
            }
        }
        assert!(!requires_approval(&Data::Config(("salt".into(), None))));
        assert!(requires_approval(&Data::SyncConfig(Some(Box::default()))));
        assert!(!requires_approval(&Data::SyncConfig(None)));
        assert!(!requires_approval(&Data::Options(None)));
        assert!(!requires_approval(&Data::Socks(None)));
    }
}
