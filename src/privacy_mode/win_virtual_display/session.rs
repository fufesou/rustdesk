use crate::privacy_mode::{INVALID_PRIVACY_MODE_CONN_ID, OCCUPIED};
use hbb_common::{bail, ResultType};

#[derive(Clone, Copy)]
pub(super) enum Session {
    Off,
    Active(i32),
    Restoring(i32),
}

impl Session {
    pub(super) fn owner(self) -> i32 {
        match self {
            Self::Off => INVALID_PRIVACY_MODE_CONN_ID,
            Self::Active(owner) | Self::Restoring(owner) => owner,
        }
    }

    pub(super) fn check_on(self, conn_id: i32) -> ResultType<bool> {
        match self {
            Self::Off => Ok(false),
            Self::Active(owner) if owner == conn_id => Ok(true),
            Self::Restoring(owner) if owner == conn_id => {
                bail!("The physical desktop is still being restored")
            }
            _ => bail!(OCCUPIED),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privacy_mode_keeps_the_restore_owner_without_reporting_protection() {
        let session = Session::Restoring(12);
        assert_eq!(session.owner(), 12);
        assert!(session.check_on(12).is_err());
        assert!(session.check_on(13).is_err());
        assert!(Session::Active(12).check_on(12).unwrap());
        assert!(!Session::Off.check_on(12).unwrap());
    }
}
