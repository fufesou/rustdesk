use super::{component, handle, identity, open, security, Access};
use crate::install_manifest::FileIdentity;
use std::{fs::File, io, mem::size_of};
use windows::{
    Wdk::Storage::FileSystem::{
        FileRenameInformation, NtSetInformationFile, FILE_RENAME_INFORMATION,
        FILE_RENAME_INFORMATION_0,
    },
    Win32::{Foundation::RtlNtStatusToDosError, System::IO::IO_STATUS_BLOCK},
};

pub(crate) fn publish_new(source: &File, parent: &File, name: &str) -> io::Result<()> {
    rename(source, parent, (name, false))
}

pub(crate) fn replace_verified(
    source: &File,
    parent: &File,
    target: (&str, &FileIdentity),
) -> io::Result<()> {
    // Win7-compatible replacement requires closing the old target. Both ACLs must
    // exclude unprivileged mutation before releasing that handle.
    security::require_protected(parent)?;
    let previous = open(parent, target.0, Access::Delete)?;
    if identity(&previous)? != *target.1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Replacement target changed after verification",
        ));
    }
    security::require_protected(&previous)?;
    drop(previous);
    rename(source, parent, (target.0, true))
}

fn rename(source: &File, parent: &File, target: (&str, bool)) -> io::Result<()> {
    let name = component(target.0)?;
    let layout = FILE_RENAME_INFORMATION::default();
    let offset = layout.FileName.as_ptr() as usize - &layout as *const _ as usize;
    let name_bytes = name.len() * size_of::<u16>();
    let bytes = offset + name_bytes;
    let mut buffer = vec![0u64; bytes.div_ceil(size_of::<u64>())];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    unsafe {
        info.write(FILE_RENAME_INFORMATION {
            Anonymous: FILE_RENAME_INFORMATION_0 {
                ReplaceIfExists: target.1.into(),
            },
            RootDirectory: handle(parent),
            FileNameLength: name_bytes as u32,
            FileName: [0],
        });
        std::ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
    }
    let mut status = IO_STATUS_BLOCK::default();
    let code = unsafe {
        NtSetInformationFile(
            handle(source),
            &mut status,
            info.cast(),
            bytes as u32,
            FileRenameInformation,
        )
    };
    if code.0 < 0 {
        let error = unsafe { RtlNtStatusToDosError(code) } as i32;
        return Err(io::Error::from_raw_os_error(error));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_manifest::{
        windows::{directory::Directory, identity, open, remove, Access},
        RelativePath,
    };
    use std::{fs, io::Write};

    #[test]
    fn publishes_the_staged_object_and_preserves_a_conflicting_target() {
        let temp = std::env::temp_dir().join(format!("rustdesk-manifest-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&temp).unwrap();
        fs::create_dir(temp.join("destination")).unwrap();
        fs::write(temp.join("destination/unknown.txt"), "keep").unwrap();
        let root = Directory::open(&temp).unwrap();
        let destination = root
            .descend(&RelativePath::parse("destination").unwrap())
            .unwrap();
        let mut staged = open(&root.file, "staged", Access::Create).unwrap();
        staged.write_all(b"new payload").unwrap();
        staged.sync_all().unwrap();
        let before = identity(&staged).unwrap();
        assert!(publish_new(&staged, &destination.file, "unknown.txt").is_err());
        assert_eq!(
            fs::read_to_string(temp.join("destination/unknown.txt")).unwrap(),
            "keep"
        );
        publish_new(&staged, &destination.file, "app.dll").unwrap();
        assert_eq!(identity(&staged).unwrap(), before);
        drop(staged);
        let installed = open(&destination.file, "app.dll", Access::Delete).unwrap();
        assert_eq!(identity(&installed).unwrap(), before);
        assert_eq!(
            fs::read_to_string(temp.join("destination/app.dll")).unwrap(),
            "new payload"
        );
        assert!(!temp.join("staged").exists());
        remove(&installed).unwrap();
        drop(installed);
        drop(destination);
        drop(root);
        fs::remove_file(temp.join("destination/unknown.txt")).unwrap();
        fs::remove_dir(temp.join("destination")).unwrap();
        fs::remove_dir(temp).unwrap();
    }

    #[test]
    fn replacement_publishes_complete_contents_without_deleting_the_old_file_first() {
        let temp = std::env::temp_dir().join(format!("rustdesk-manifest-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&temp).unwrap();
        fs::write(temp.join("owned.dll"), "old").unwrap();
        let root = Directory::open(&temp).unwrap();
        let previous = open(&root.file, "owned.dll", Access::Delete).unwrap();
        let previous_identity = identity(&previous).unwrap();
        let mut staged = open(&root.file, "staged", Access::Create).unwrap();
        staged.write_all(b"new").unwrap();
        staged.sync_all().unwrap();
        assert_eq!(fs::read_to_string(temp.join("owned.dll")).unwrap(), "old");
        drop(previous);
        rename(&staged, &root.file, ("owned.dll", true)).unwrap();
        assert_eq!(fs::read_to_string(temp.join("owned.dll")).unwrap(), "new");
        assert_ne!(previous_identity, identity(&staged).unwrap());
        remove(&staged).unwrap();
        drop(staged);
        drop(root);
        fs::remove_dir(temp).unwrap();
    }
}
