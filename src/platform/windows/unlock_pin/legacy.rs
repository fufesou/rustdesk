use hbb_common::ResultType;
use std::{
    fs::{self, File},
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::Path,
};
use winapi::um::winnt::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE};
use windows::{
    core::{Owned, PWSTR},
    Win32::{
        Foundation::{GENERIC_ALL, GENERIC_WRITE, HANDLE, HLOCAL},
        Security::{
            Authorization::{ConvertSidToStringSidW, GetSecurityInfo, SE_FILE_OBJECT},
            GetAce, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION,
            INHERIT_ONLY_ACE, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        },
        Storage::FileSystem::{
            DELETE, FILE_APPEND_DATA, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
            FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA, READ_CONTROL, WRITE_DAC,
            WRITE_OWNER,
        },
    },
};

#[derive(PartialEq)]
pub(super) enum Source {
    Missing,
    Protected,
    Untrusted,
}

pub(super) fn source(path: &Path) -> ResultType<Source> {
    match fs::symlink_metadata(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Source::Missing),
        Err(err) => return Err(err.into()),
        Ok(meta) if !meta.is_file() => return Ok(Source::Untrusted),
        Ok(_) => {}
    }
    // A writable ancestor can replace a protected child. Check from the volume root down.
    let ancestors: Vec<_> = path.ancestors().collect();
    for &part in ancestors.iter().rev() {
        let direct = part == path || Some(part) == path.parent();
        if !protected_entry(part, direct)? {
            return Ok(Source::Untrusted);
        }
    }
    Ok(Source::Protected)
}

fn protected_entry(path: &Path, direct: bool) -> ResultType<bool> {
    let file = File::options()
        .access_mode(READ_CONTROL.0 | FILE_READ_ATTRIBUTES.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)?;
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Ok(false);
    }
    let mut owner = PSID::default();
    let mut dacl = std::ptr::null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        GetSecurityInfo(
            HANDLE(file.as_raw_handle()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut dacl),
            None,
            Some(&mut descriptor),
        )
        .ok()?;
        let _descriptor = Owned::new(HLOCAL(descriptor.0));
        protected_acl(owner, dacl, direct)
    }
}

unsafe fn protected_acl(owner: PSID, dacl: *const ACL, direct: bool) -> ResultType<bool> {
    if dacl.is_null() || owner.is_invalid() || !privileged_sid(owner)? {
        return Ok(false);
    }
    let mut writes = DELETE.0
        | WRITE_DAC.0
        | WRITE_OWNER.0
        | FILE_DELETE_CHILD.0
        | FILE_WRITE_ATTRIBUTES.0
        | FILE_WRITE_EA.0
        | GENERIC_WRITE.0
        | GENERIC_ALL.0;
    // On an ancestor, creating siblings alone cannot replace the protected descendant.
    if direct {
        writes |= FILE_WRITE_DATA.0 | FILE_APPEND_DATA.0;
    }
    for index in 0..(*dacl).AceCount {
        let mut entry = std::ptr::null_mut();
        GetAce(dacl, index.into(), &mut entry)?;
        let header = &*entry.cast::<ACE_HEADER>();
        if header.AceFlags & INHERIT_ONLY_ACE.0 as u8 != 0
            || header.AceType == ACCESS_DENIED_ACE_TYPE
        {
            continue;
        }
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE
            || usize::from(header.AceSize) < std::mem::size_of::<ACCESS_ALLOWED_ACE>()
        {
            return Ok(false);
        }
        let ace = &*entry.cast::<ACCESS_ALLOWED_ACE>();
        if ace.Mask & writes != 0
            && !privileged_sid(PSID(std::ptr::addr_of!(ace.SidStart).cast_mut().cast()))?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

unsafe fn privileged_sid(sid: PSID) -> ResultType<bool> {
    let mut text = PWSTR::null();
    ConvertSidToStringSidW(sid, &mut text)?;
    let _text = Owned::new(HLOCAL(text.0.cast()));
    const TRUSTED_INSTALLER: &str =
        "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
    // LocalSystem, the built-in Administrators group and TrustedInstaller.
    Ok(matches!(
        text.to_string()?.as_str(),
        "S-1-5-18" | "S-1-5-32-544" | TRUSTED_INSTALLER
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::{
        core::PCWSTR,
        Win32::Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
        },
    };

    fn permits_migration(sddl: &str, direct: bool) -> bool {
        let wide: Vec<_> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let mut owner = PSID::default();
        let mut dacl = std::ptr::null_mut();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
            .unwrap();
            let _descriptor = Owned::new(HLOCAL(descriptor.0));
            GetSecurityDescriptorOwner(descriptor, &mut owner, &mut Default::default()).unwrap();
            GetSecurityDescriptorDacl(
                descriptor,
                &mut Default::default(),
                &mut dacl,
                &mut Default::default(),
            )
            .unwrap();
            protected_acl(owner, dacl, direct).unwrap()
        }
    }

    #[test]
    fn only_a_protected_legacy_pin_can_be_migrated() {
        assert!(permits_migration("O:SYD:(A;;FA;;;BA)(A;;FR;;;BU)", true));
        assert!(!permits_migration(
            "O:SYD:(A;;FA;;;S-1-5-21-1-2-3-1001)",
            true
        ));
        assert!(!permits_migration("O:BUD:(A;;FA;;;SY)", true));
        let replace_child = format!("O:SYD:(A;;0x{:x};;;BU)", FILE_DELETE_CHILD.0);
        let create_sibling = format!("O:SYD:(A;;0x{:x};;;BU)", FILE_APPEND_DATA.0);
        assert!(!permits_migration(&replace_child, false));
        assert!(permits_migration(&create_sibling, false));
        assert!(!permits_migration(&create_sibling, true));
    }
}
