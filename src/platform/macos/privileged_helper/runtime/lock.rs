use super::super::{validate_owned_file, ExpectedOwner};
use hbb_common::ResultType;
use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
use std::path::Path;

pub(super) struct FinalizerLock {
    _file: std::fs::File,
}

impl FinalizerLock {
    pub(super) fn acquire(state_directory: &Path, owner: ExpectedOwner) -> ResultType<Self> {
        let path = state_directory.join(super::FINALIZER_LOCK_NAME);
        let file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                validate_owned_file(&path, owner, 0o600)?;
                std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(hbb_common::libc::O_NOFOLLOW)
                    .open(&path)?
            }
            Err(err) => return Err(err.into()),
        };
        validate_owned_file(&path, owner, 0o600)?;
        let result =
            unsafe { hbb_common::libc::flock(file.as_raw_fd(), hbb_common::libc::LOCK_EX) };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self { _file: file })
    }
}
