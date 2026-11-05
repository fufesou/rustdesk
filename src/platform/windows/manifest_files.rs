use crate::install_manifest::{FileIdentity, RelativePath};
use std::{
    fs::File,
    io,
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle},
};
use windows::{
    core::PWSTR,
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            NtCreateFile, FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
            FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
        },
    },
    Win32::{
        Foundation::{RtlNtStatusToDosError, HANDLE, OBJ_CASE_INSENSITIVE, UNICODE_STRING},
        Storage::FileSystem::{
            FileDispositionInfo, FileNameInfo, GetFileInformationByHandle,
            GetFileInformationByHandleEx, SetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
            DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_DISPOSITION_INFO, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_NAME_INFO,
            FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, READ_CONTROL,
            SYNCHRONIZE,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

const MAX_NATIVE_NAME_BYTES: usize = u16::MAX as usize;

#[path = "manifest_files/artifacts.rs"]
pub(crate) mod artifacts;

#[path = "manifest_files/directory.rs"]
pub(crate) mod directory;
#[path = "manifest_files/files.rs"]
pub(crate) mod files;
#[path = "manifest_files/inspect.rs"]
pub(crate) mod inspect;
#[path = "manifest_files/operation.rs"]
pub(crate) mod operation;
#[path = "manifest_files/publish.rs"]
pub(crate) mod publish;
#[path = "manifest_files/security.rs"]
pub(crate) mod security;
#[path = "manifest_files/store.rs"]
pub(crate) mod store;
#[path = "manifest_files/system.rs"]
pub(crate) mod system;

#[derive(Clone, Copy)]
pub(crate) enum Access {
    Directory,
    Read,
    Delete,
    RemoveDirectory,
    Create,
    CreateDirectory,
}

impl Access {
    fn directory(self) -> bool {
        matches!(
            self,
            Self::Directory | Self::RemoveDirectory | Self::CreateDirectory
        )
    }

    fn create(self) -> bool {
        matches!(self, Self::Create | Self::CreateDirectory)
    }

    fn delete(self) -> bool {
        matches!(self, Self::Delete | Self::RemoveDirectory | Self::Create)
    }
}

pub(crate) fn open(parent: &File, name: &str, access: Access) -> io::Result<File> {
    let mut name_buffer = component(name)?;
    let name_string = UNICODE_STRING {
        Length: (name_buffer.len() * size_of::<u16>()) as u16,
        MaximumLength: (name_buffer.len() * size_of::<u16>()) as u16,
        Buffer: PWSTR(name_buffer.as_mut_ptr()),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: handle(parent),
        ObjectName: &name_string,
        Attributes: OBJ_CASE_INSENSITIVE,
        ..Default::default()
    };
    let file = open_native(&attributes, access)?;
    validate(&file, access.directory())?;
    validate_name(&file, name)?;
    Ok(file)
}

fn open_native(attributes: &OBJECT_ATTRIBUTES, access: Access) -> io::Result<File> {
    let mut rights = if access.directory() {
        FILE_READ_ATTRIBUTES | FILE_TRAVERSE | READ_CONTROL | SYNCHRONIZE
    } else {
        FILE_GENERIC_READ
    };
    if access.create() && !access.directory() {
        rights |= FILE_GENERIC_WRITE;
    }
    if access.delete() {
        rights |= DELETE;
    }
    let share = if access.directory() && !access.delete() {
        FILE_SHARE_READ | FILE_SHARE_WRITE
    } else {
        FILE_SHARE_READ
    };
    let kind = if access.directory() {
        FILE_DIRECTORY_FILE
    } else {
        FILE_NON_DIRECTORY_FILE
    };
    let disposition = if access.create() {
        FILE_CREATE
    } else {
        FILE_OPEN
    };
    let mut result = HANDLE::default();
    let mut status = IO_STATUS_BLOCK::default();
    let code = unsafe {
        NtCreateFile(
            &mut result,
            rights,
            attributes,
            &mut status,
            None,
            FILE_ATTRIBUTE_NORMAL,
            share,
            disposition,
            kind | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            None,
            0,
        )
    };
    if code.0 < 0 {
        let error = unsafe { RtlNtStatusToDosError(code) } as i32;
        return Err(io::Error::from_raw_os_error(error));
    }
    Ok(unsafe { File::from_raw_handle(result.0) })
}

pub(crate) fn identity(file: &File) -> io::Result<FileIdentity> {
    let info = information(file)?;
    Ok(FileIdentity {
        volume_serial_number: info.dwVolumeSerialNumber,
        file_index: (u64::from(info.nFileIndexHigh) << u32::BITS) | u64::from(info.nFileIndexLow),
    })
}

pub(crate) fn validate(file: &File, directory: bool) -> io::Result<()> {
    let attributes = information(file)?.dwFileAttributes;
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Installer paths must not contain reparse points",
        ));
    }
    if (attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0) != directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Installer path has the wrong object type",
        ));
    }
    Ok(())
}

fn information(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle(file), &mut info) }.map_err(io::Error::from)?;
    Ok(info)
}

fn validate_name(file: &File, expected: &str) -> io::Result<()> {
    let bytes = size_of::<FILE_NAME_INFO>() + MAX_NATIVE_NAME_BYTES;
    let mut buffer = vec![0u64; bytes.div_ceil(size_of::<u64>())];
    unsafe {
        GetFileInformationByHandleEx(
            handle(file),
            FileNameInfo,
            buffer.as_mut_ptr().cast(),
            bytes as u32,
        )
    }
    .map_err(io::Error::from)?;
    let info = unsafe { &*buffer.as_ptr().cast::<FILE_NAME_INFO>() };
    let units = info.FileNameLength as usize / size_of::<u16>();
    if info.FileNameLength as usize > MAX_NATIVE_NAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid native file name length",
        ));
    }
    let name = unsafe { std::slice::from_raw_parts(info.FileName.as_ptr(), units) };
    let name = String::from_utf16(name)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if !name
        .rsplit('\\')
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Installer path uses a filesystem alias",
        ));
    }
    Ok(())
}

pub(crate) fn remove(file: &File) -> io::Result<()> {
    let disposition = FILE_DISPOSITION_INFO {
        DeleteFile: true.into(),
    };
    unsafe {
        SetFileInformationByHandle(
            handle(file),
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    }
    .map_err(io::Error::from)
}

pub(crate) fn handle(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}

fn component(name: &str) -> io::Result<Vec<u16>> {
    let path = RelativePath::parse(name)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    if path.as_str().contains('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected a single file name",
        ));
    }
    let wide: Vec<_> = name.encode_utf16().collect();
    if wide.len() > MAX_NATIVE_NAME_BYTES / size_of::<u16>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "File name is too long",
        ));
    }
    Ok(wide)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::windows::fs::OpenOptionsExt};
    use windows::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

    #[test]
    fn deletes_only_the_opened_file_and_preserves_nonempty_directories() {
        let temp = std::env::temp_dir().join(format!("rustdesk-manifest-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&temp).unwrap();
        fs::write(temp.join("owned.txt"), "owned").unwrap();
        fs::write(temp.join("unknown.txt"), "unknown").unwrap();
        fs::create_dir(temp.join("assets")).unwrap();
        fs::write(temp.join("assets/unknown.txt"), "unknown").unwrap();
        let parent = fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
            .open(&temp)
            .unwrap();
        let owned = open(&parent, "owned.txt", Access::Delete).unwrap();
        remove(&owned).unwrap();
        drop(owned);
        assert!(!temp.join("owned.txt").exists());
        assert_eq!(
            fs::read_to_string(temp.join("unknown.txt")).unwrap(),
            "unknown"
        );
        let assets = open(&parent, "assets", Access::RemoveDirectory).unwrap();
        assert!(remove(&assets).is_err());
        assert_eq!(
            fs::read_to_string(temp.join("assets/unknown.txt")).unwrap(),
            "unknown"
        );
        assert!(open(&parent, "../outside.txt", Access::Delete).is_err());
        drop(assets);
        drop(parent);
        fs::remove_file(temp.join("unknown.txt")).unwrap();
        fs::remove_file(temp.join("assets/unknown.txt")).unwrap();
        fs::remove_dir(temp.join("assets")).unwrap();
        fs::remove_dir(temp).unwrap();
    }
}
