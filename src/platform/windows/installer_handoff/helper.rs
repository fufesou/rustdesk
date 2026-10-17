use super::super::wide_string;
use hbb_common::{anyhow::anyhow, log, ResultType};
use std::{
    fs::{self, File},
    io::{self, Write},
    os::windows::io::{AsRawHandle, FromRawHandle},
    path::{Path, PathBuf},
};
use windows::{
    core::{w, Owned, PCWSTR},
    Win32::{
        Foundation::{GENERIC_WRITE, HANDLE, HLOCAL},
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        },
        Storage::FileSystem::{
            CreateFileW, FileDispositionInfo, SetFileInformationByHandle, CREATE_NEW, DELETE,
            FILE_ATTRIBUTE_NORMAL, FILE_DISPOSITION_INFO, FILE_SHARE_READ,
        },
    },
};

const HELPER_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/installer-helper.exe"));

pub(super) struct ProtectedHelper {
    path: PathBuf,
    file: Option<File>,
}

impl ProtectedHelper {
    pub(super) fn new(path: &Path) -> ResultType<Self> {
        let mut file = create_protected_file(path)?;
        if let Err(error) = file.write_all(HELPER_BYTES).and_then(|_| file.sync_all()) {
            if let Err(cleanup) = delete_file(&file) {
                log::warn!(
                    "Failed to remove incomplete installer helper {}: {cleanup}",
                    path.display()
                );
            }
            return Err(error.into());
        }
        Ok(Self {
            path: path.to_owned(),
            file: Some(file),
        })
    }

    pub(super) fn release(&mut self) {
        // Closing the original writable file object is required before execution.
        // The elevated cmd waits at the gate until this handle has been released.
        drop(self.file.take());
    }
}

impl Drop for ProtectedHelper {
    fn drop(&mut self) {
        let result = match &self.file {
            // UAC cancellation cannot reopen this path with DELETE permission.
            Some(file) => delete_file(file),
            None => match fs::remove_file(&self.path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.into()),
                _ => Ok(()),
            },
        };
        if let Err(error) = result {
            log::warn!(
                "Protected installer helper may remain at {}: {error}",
                self.path.display()
            );
        }
    }
}

fn create_protected_file(path: &Path) -> ResultType<File> {
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // OWNER RIGHTS also removes the creator's implicit permission to change the
    // ACL. The initial creation handle remains available for cancellation cleanup.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            w!("D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FRFX;;;OW)"),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )?;
    }
    let _descriptor = unsafe { Owned::new(HLOCAL(descriptor.0)) };
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let path_string = wide_string(
        path.to_str()
            .ok_or_else(|| anyhow!("Invalid installer helper path"))?,
    );
    let handle = unsafe {
        CreateFileW(
            PCWSTR(path_string.as_ptr()),
            GENERIC_WRITE.0 | DELETE.0,
            FILE_SHARE_READ,
            Some(&attributes),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )?
    };
    Ok(unsafe { File::from_raw_handle(handle.0) })
}

fn delete_file(file: &File) -> ResultType<()> {
    let disposition = FILE_DISPOSITION_INFO {
        DeleteFile: true.into(),
    };
    unsafe {
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileDispositionInfo,
            &disposition as *const _ as *const _,
            std::mem::size_of_val(&disposition) as u32,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_is_removed_when_elevation_does_not_start() {
        let path = super::super::directory::path()
            .unwrap()
            .with_extension("exe");
        let helper = ProtectedHelper::new(&path).unwrap();
        assert!(path.exists());
        drop(helper);
        assert!(
            !path.exists(),
            "cancellation must remove the protected helper"
        );
    }
}
