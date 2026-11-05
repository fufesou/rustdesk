use super::{open, security, validate, Access};
use crate::install_manifest::RelativePath;
use std::{fs::File, io, os::windows::io::FromRawHandle, path::Path};
use windows::{
    core::PCWSTR,
    Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, OPEN_EXISTING,
        READ_CONTROL, SYNCHRONIZE,
    },
};

pub(crate) struct Directory {
    pub file: File,
    ancestors: Vec<File>,
}

pub(crate) struct Entry {
    pub file: File,
    pub parent: Directory,
}

impl Directory {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let text = path
            .to_str()
            .ok_or_else(|| invalid_path("Non-Unicode installation path"))?;
        let bytes = text.as_bytes();
        if bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || !matches!(bytes[2], b'\\' | b'/')
        {
            return Err(invalid_path("Expected an absolute local drive path"));
        }
        let relative = text[3..].trim_end_matches(['\\', '/']);
        let drive = [u16::from(bytes[0]), b':' as u16, b'\\' as u16, 0];
        let handle = unsafe {
            CreateFileW(
                PCWSTR(drive.as_ptr()),
                (FILE_READ_ATTRIBUTES | FILE_TRAVERSE | READ_CONTROL | SYNCHRONIZE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }
        .map_err(io::Error::from)?;
        let mut directory = Self {
            file: unsafe { File::from_raw_handle(handle.0) },
            ancestors: Vec::new(),
        };
        validate(&directory.file, true)?;
        if relative.is_empty() {
            return Ok(directory);
        }
        let relative = RelativePath::parse(relative).map_err(invalid_path)?;
        for name in relative.as_str().split('/') {
            let next = open(&directory.file, name, Access::Directory)?;
            directory.ancestors.push(directory.file);
            directory.file = next;
        }
        Ok(directory)
    }

    pub(crate) fn create(path: &Path) -> io::Result<Self> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| invalid_path("Missing installation directory name"))?;
        RelativePath::parse(name).map_err(invalid_path)?;
        let mut parent = path
            .parent()
            .ok_or_else(|| invalid_path("Missing installation parent"))?;
        let mut missing = Vec::new();
        let mut directory = loop {
            match Self::open(parent) {
                Ok(directory) => break directory,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let component = parent
                        .file_name()
                        .and_then(|name| name.to_str())
                        .ok_or_else(|| invalid_path("Missing installation ancestor name"))?;
                    missing.push(component);
                    parent = parent
                        .parent()
                        .ok_or_else(|| invalid_path("Missing installation ancestor"))?;
                }
                Err(error) => return Err(error),
            }
        };
        for component in missing.into_iter().rev() {
            directory = directory.create_child(component)?;
        }
        directory.create_child(name)
    }

    pub(crate) fn create_child(&self, name: &str) -> io::Result<Self> {
        let file =
            security::Descriptor::installer()?.create(&self.file, name, Access::CreateDirectory)?;
        let mut directory = self.duplicate()?;
        directory.ancestors.push(directory.file);
        directory.file = file;
        Ok(directory)
    }

    pub(crate) fn descend(&self, path: &RelativePath) -> io::Result<Self> {
        let mut directory = self.duplicate()?;
        for name in path.as_str().split('/') {
            let next = open(&directory.file, name, Access::Directory)?;
            directory.ancestors.push(directory.file);
            directory.file = next;
        }
        Ok(directory)
    }

    pub(crate) fn entry(&self, path: &RelativePath, access: Access) -> io::Result<Entry> {
        let (parent, name) = self.parent(path)?;
        Ok(Entry {
            file: open(&parent.file, name, access)?,
            parent,
        })
    }

    pub(crate) fn parent<'a>(&self, path: &'a RelativePath) -> io::Result<(Self, &'a str)> {
        match path.as_str().rsplit_once('/') {
            Some((parent, name)) => {
                let parent = RelativePath::parse(parent).map_err(invalid_path)?;
                Ok((self.descend(&parent)?, name))
            }
            None => Ok((self.duplicate()?, path.as_str())),
        }
    }

    pub(crate) fn protected_parent<'a>(
        &self,
        path: &'a RelativePath,
    ) -> io::Result<(Self, &'a str)> {
        let (parent, name) = self.parent(path)?;
        for directory in parent
            .ancestors
            .iter()
            .skip(self.ancestors.len())
            .chain([&parent.file])
        {
            security::require_protected(directory)?;
        }
        Ok((parent, name))
    }

    fn duplicate(&self) -> io::Result<Self> {
        Ok(Self {
            file: self.file.try_clone()?,
            ancestors: self
                .ancestors
                .iter()
                .map(File::try_clone)
                .collect::<io::Result<_>>()?,
        })
    }
}

fn invalid_path(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, error)
}

#[cfg(test)]
mod tests {
    use super::super::handle;
    use super::*;
    use std::{fs, mem::size_of, os::windows::fs::OpenOptionsExt};
    use winapi::um::{winioctl::FSCTL_SET_REPARSE_POINT, winnt::IO_REPARSE_TAG_MOUNT_POINT};
    use windows::{
        Wdk::Storage::FileSystem::REPARSE_DATA_BUFFER, Win32::System::IO::DeviceIoControl,
    };

    fn junction(path: &Path, target: &Path) {
        fs::create_dir(path).unwrap();
        let target: Vec<u16> = format!("\\??\\{}", target.display())
            .encode_utf16()
            .chain([0])
            .collect();
        let layout = REPARSE_DATA_BUFFER::default();
        let header = &layout.Anonymous as *const _ as usize - &layout as *const _ as usize;
        let offset =
            unsafe { layout.Anonymous.MountPointReparseBuffer.PathBuffer.as_ptr() as usize }
                - &layout as *const _ as usize;
        let bytes = offset + (target.len() + 1) * size_of::<u16>();
        let mut buffer = vec![0u64; bytes.div_ceil(size_of::<u64>())];
        let info = unsafe { &mut *buffer.as_mut_ptr().cast::<REPARSE_DATA_BUFFER>() };
        info.ReparseTag = IO_REPARSE_TAG_MOUNT_POINT;
        info.ReparseDataLength = (bytes - header) as u16;
        let mount = unsafe { &mut info.Anonymous.MountPointReparseBuffer };
        mount.SubstituteNameLength = ((target.len() - 1) * size_of::<u16>()) as u16;
        mount.PrintNameOffset = (target.len() * size_of::<u16>()) as u16;
        unsafe {
            std::ptr::copy_nonoverlapping(
                target.as_ptr(),
                mount.PathBuffer.as_mut_ptr(),
                target.len(),
            )
        };
        let directory = fs::OpenOptions::new()
            .write(true)
            .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0)
            .open(path)
            .unwrap();
        unsafe {
            DeviceIoControl(
                handle(&directory),
                FSCTL_SET_REPARSE_POINT,
                Some(buffer.as_ptr().cast()),
                bytes as u32,
                None,
                0,
                None,
                None,
            )
        }
        .unwrap();
    }

    #[test]
    fn rejects_junctions_without_touching_their_targets_and_pins_ancestors() {
        let temp = std::env::temp_dir().join(format!("rustdesk-manifest-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&temp).unwrap();
        fs::create_dir(temp.join("root")).unwrap();
        fs::create_dir(temp.join("outside")).unwrap();
        fs::write(temp.join("outside/sentinel"), "keep").unwrap();
        junction(&temp.join("root/link"), &temp.join("outside"));
        let root = Directory::open(&temp.join("root")).unwrap();
        let path = RelativePath::parse("link/sentinel").unwrap();
        assert!(root.entry(&path, Access::Delete).is_err());
        assert!(Directory::open(&temp.join("root/link")).is_err());
        assert!(Directory::create(&temp.join("root/link/nested/child")).is_err());
        assert!(!temp.join("outside/nested").exists());
        assert!(fs::rename(temp.join("root"), temp.join("moved")).is_err());
        assert_eq!(
            fs::read_to_string(temp.join("outside/sentinel")).unwrap(),
            "keep"
        );
        assert!(fs::symlink_metadata(temp.join("root/link")).is_ok());
        drop(root);
        fs::remove_dir(temp.join("root/link")).unwrap();
        fs::remove_file(temp.join("outside/sentinel")).unwrap();
        fs::remove_dir(temp.join("outside")).unwrap();
        fs::remove_dir(temp.join("root")).unwrap();
        fs::remove_dir(temp).unwrap();
    }
}
