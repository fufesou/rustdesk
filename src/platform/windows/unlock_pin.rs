use hbb_common::{
    anyhow::{anyhow, ensure},
    config::Config2,
    sodiumoxide::utils,
    ResultType,
};
use windows::{
    core::{HRESULT, PCWSTR, PWSTR},
    Win32::{
        Foundation::ERROR_NOT_FOUND,
        Security::Credentials::{
            CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
            CRED_TYPE_GENERIC,
        },
    },
};

mod legacy;

pub(crate) struct ServicePin(Vec<u8>);

pub(super) enum ServicePeer {
    Session,
    PinAdministrator,
}

impl ServicePeer {
    pub(super) fn authorize(session: bool, elevated: impl FnOnce() -> bool) -> Option<Self> {
        if session {
            Some(Self::Session)
        } else if elevated() {
            Some(Self::PinAdministrator)
        } else {
            None
        }
    }

    pub(super) fn permits(&self, data: &crate::ipc::Data) -> bool {
        use crate::ipc::{unix_unlock_pin::Request, Data};
        // Deployment scripts can set the PIN from an elevated non-interactive session.
        matches!(self, Self::Session) || matches!(data, Data::UnlockPin(Request::Set(None, _)))
    }
}

impl ServicePin {
    fn target() -> ResultType<Vec<u16>> {
        ensure!(
            super::is_root(),
            "The unlock PIN requires the LocalSystem account"
        );
        Ok(super::wide_string(&format!(
            "{}/SettingsUnlockPin",
            crate::get_app_name()
        )))
    }

    pub(crate) fn load() -> ResultType<Self> {
        let target = Self::target()?;
        if let Some(pin) = read(&target)? {
            return Ok(Self(pin));
        }
        // Read-only access by other accounts permits migration of the existing PIN.
        let source = legacy::source(&Config2::file())?;
        if source != legacy::Source::Protected {
            write(&target, &[])?;
            if source == legacy::Source::Untrusted {
                hbb_common::log::warn!(
                    "Skipping PIN migration from an unprotected service profile. \
                     To use PIN unlock, an administrator must choose a different PIN. \
                     The previous PIN may still be readable in the old configuration."
                );
            }
            return Ok(Self(Vec::new()));
        }
        let mut migrated = Vec::new();
        Config2::migrate_unlock_pin(|pin| {
            write(&target, pin.as_bytes())?;
            migrated.extend_from_slice(pin.as_bytes());
            Ok(())
        })?;
        Ok(Self(migrated))
    }

    pub(crate) fn set(pin: &str) -> ResultType<()> {
        // An empty credential records removal so a stale profile cannot restore the old PIN.
        write(&Self::target()?, pin.as_bytes())
    }

    pub(crate) fn configured(&self) -> bool {
        !self.0.is_empty()
    }

    pub(crate) fn matches(&self, candidate: &str) -> ResultType<bool> {
        Ok(self.configured() && utils::memcmp(&self.0, candidate.as_bytes()))
    }
}

fn read(target: &[u16]) -> ResultType<Option<Vec<u8>>> {
    let mut entry = std::ptr::null_mut();
    unsafe {
        if let Err(error) = CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut entry)
        {
            if error.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0) {
                return Ok(None);
            }
            return Err(error.into());
        }
        let credential = entry
            .as_ref()
            .ok_or_else(|| anyhow!("Missing PIN credential"))?;
        let blob = if credential.CredentialBlobSize == 0 {
            Ok(Vec::new())
        } else if credential.CredentialBlob.is_null() {
            Err(anyhow!("Invalid PIN credential"))
        } else {
            Ok(std::slice::from_raw_parts(
                credential.CredentialBlob,
                credential.CredentialBlobSize as usize,
            )
            .to_vec())
        };
        CredFree(entry.cast());
        blob.map(Some)
    }
}

fn write(target: &[u16], value: &[u8]) -> ResultType<()> {
    let entry = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target.as_ptr().cast_mut()),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        CredentialBlobSize: value.len().try_into()?,
        CredentialBlob: if value.is_empty() {
            std::ptr::null_mut()
        } else {
            value.as_ptr().cast_mut()
        },
        ..Default::default()
    };
    unsafe { CredWriteW(&entry, 0)? };
    Ok(())
}

pub(crate) fn request_approval(key: &[u8]) -> ResultType<()> {
    let arguments = format!(
        "--approve-unlock-pin {} {}",
        std::process::id(),
        hex::encode(key)
    );
    let helper =
        super::installer_shell::launch_elevated(&std::env::current_exe()?, &arguments, false)?;
    ensure!(helper.wait()? == 0, "Authentication Required");
    Ok(())
}

pub(super) async fn initialize() {
    if let Err(err) = crate::trusted_devices::config::update(|| {
        crate::ipc::unix_unlock_pin::preserve_legacy_root_pin()?;
        if legacy::source(&Config2::file())? == legacy::Source::Protected {
            Config2::migrate_unlock_pin(|_| Ok(()))?;
        }
        Ok(())
    })
    .await
    {
        hbb_common::log::warn!("Could not initialize the Windows service unlock PIN: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::{
        Foundation::ERROR_NO_SUCH_LOGON_SESSION,
        Security::Credentials::{CredDeleteW, CredGetSessionTypes, CRED_TYPE_MAXIMUM},
    };

    #[test]
    fn cross_session_administrators_can_only_set_the_pin() {
        use crate::ipc::{unix_unlock_pin::Request, Data};
        assert!(ServicePeer::authorize(false, || false).is_none());
        let peer = ServicePeer::authorize(false, || true).unwrap();
        assert!(peer.permits(&Data::UnlockPin(Request::Set(None, "fixture-pin".into()))));
        assert!(!peer.permits(&Data::UnlockPin(Request::Approve(100, [42; 32]))));
        assert!(!peer.permits(&Data::Close));
        assert!(ServicePeer::authorize(true, || false)
            .unwrap()
            .permits(&Data::Close));
    }

    #[test]
    fn windows_vault_distinguishes_unavailable_missing_and_removed_pins() {
        let target = super::super::wide_string(&format!("RustDesk/Test/{}", uuid::Uuid::new_v4()));
        let mut persistence = [0; CRED_TYPE_MAXIMUM.0 as usize];
        if let Err(error) = unsafe { CredGetSessionTypes(&mut persistence) } {
            // Network logons have no credential set. A failed read must not start migration.
            assert_eq!(
                error.code(),
                HRESULT::from_win32(ERROR_NO_SUCH_LOGON_SESSION.0)
            );
            let actual = read(&target)
                .unwrap_err()
                .downcast::<windows::core::Error>()
                .unwrap();
            assert_eq!(actual.code(), error.code());
            return;
        }
        assert_eq!(read(&target).unwrap(), None);
        write(&target, b"fixture-pin").unwrap();
        let saved = read(&target);
        let removed = write(&target, b"").and_then(|_| read(&target));
        unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) }.unwrap();
        assert_eq!(saved.unwrap().as_deref(), Some(b"fixture-pin".as_slice()));
        assert_eq!(removed.unwrap(), Some(Vec::new()));
    }
}
