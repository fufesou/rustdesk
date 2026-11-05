use super::{component, handle, open_native, validate, validate_name, Access};
use std::{fs::File, io, mem::size_of, ptr};
use winapi::um::winnt::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE};
use windows::{
    core::{Owned, BOOL, PCWSTR, PWSTR},
    Wdk::Foundation::OBJECT_ATTRIBUTES,
    Win32::{
        Foundation::{GENERIC_ALL, GENERIC_WRITE, HLOCAL, OBJ_CASE_INSENSITIVE, UNICODE_STRING},
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
                SDDL_REVISION_1, SE_FILE_OBJECT,
            },
            GetAce, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, IsWellKnownSid,
            WinBuiltinAdministratorsSid, WinLocalSystemSid, ACCESS_ALLOWED_ACE, ACE_HEADER,
            DACL_SECURITY_INFORMATION, INHERIT_ONLY_ACE, OWNER_SECURITY_INFORMATION,
            PSECURITY_DESCRIPTOR, PSID,
        },
        Storage::FileSystem::{
            DELETE, FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_DELETE_CHILD, FILE_WRITE_ATTRIBUTES,
            FILE_WRITE_EA, WRITE_DAC, WRITE_OWNER,
        },
    },
};

const INSTALLER_SECURITY: &str = "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FRFX;;;BU)";
const MUTATING_ACCESS: u32 = GENERIC_ALL.0
    | GENERIC_WRITE.0
    | DELETE.0
    | WRITE_DAC.0
    | WRITE_OWNER.0
    | FILE_ADD_FILE.0
    | FILE_ADD_SUBDIRECTORY.0
    | FILE_DELETE_CHILD.0
    | FILE_WRITE_ATTRIBUTES.0
    | FILE_WRITE_EA.0;

pub(crate) struct Descriptor(Owned<HLOCAL>);

impl Descriptor {
    pub(crate) fn installer() -> io::Result<Self> {
        Self::parse(INSTALLER_SECURITY)
    }

    fn parse(sddl: &str) -> io::Result<Self> {
        let wide: Vec<_> = sddl.encode_utf16().chain([0]).collect();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(io::Error::from)?;
        Ok(Self(unsafe { Owned::new(HLOCAL(descriptor.0)) }))
    }

    pub(crate) fn create(&self, parent: &File, name: &str, access: Access) -> io::Result<File> {
        if !access.create() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Expected a create operation",
            ));
        }
        let mut wide = component(name)?;
        let bytes = (wide.len() * size_of::<u16>()) as u16;
        let name_string = UNICODE_STRING {
            Length: bytes,
            MaximumLength: bytes,
            Buffer: PWSTR(wide.as_mut_ptr()),
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: handle(parent),
            ObjectName: &name_string,
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: self.0 .0.cast(),
            ..Default::default()
        };
        let file = open_native(&attributes, access)?;
        validate(&file, access.directory())?;
        validate_name(&file, name)?;
        Ok(file)
    }

    fn verify(&self) -> io::Result<()> {
        let descriptor = PSECURITY_DESCRIPTOR(self.0 .0);
        let mut owner = PSID::default();
        let mut defaulted = BOOL::default();
        unsafe { GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted) }
            .map_err(io::Error::from)?;
        if !administrator(owner) {
            return Err(unsafe_permissions(
                "Installer directories must be owned by Administrators or SYSTEM",
            ));
        }
        let mut acl = ptr::null_mut();
        let mut present = BOOL::default();
        unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted) }
            .map_err(io::Error::from)?;
        if !present.as_bool() || acl.is_null() {
            return Err(unsafe_permissions(
                "Installer directory has no restrictive DACL",
            ));
        }
        for index in 0..u32::from(unsafe { (*acl).AceCount }) {
            let mut entry = ptr::null_mut();
            unsafe { GetAce(acl, index, &mut entry) }.map_err(io::Error::from)?;
            verify_entry(unsafe { &*entry.cast::<ACE_HEADER>() })?;
        }
        Ok(())
    }
}

pub(crate) fn require_protected(file: &File) -> io::Result<()> {
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        GetSecurityInfo(
            handle(file),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            None,
            None,
            None,
            None,
            Some(&mut descriptor),
        )
    }
    .ok()
    .map_err(io::Error::from)?;
    Descriptor(unsafe { Owned::new(HLOCAL(descriptor.0)) }).verify()
}

fn verify_entry(header: &ACE_HEADER) -> io::Result<()> {
    if u32::from(header.AceFlags) & INHERIT_ONLY_ACE.0 != 0
        || header.AceType == ACCESS_DENIED_ACE_TYPE
    {
        return Ok(());
    }
    if header.AceType != ACCESS_ALLOWED_ACE_TYPE
        || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
    {
        return Err(unsafe_permissions(
            "Unsupported directory access control entry",
        ));
    }
    let allowed = unsafe { &*(header as *const ACE_HEADER).cast::<ACCESS_ALLOWED_ACE>() };
    let sid = PSID((&allowed.SidStart as *const u32).cast_mut().cast());
    if allowed.Mask & MUTATING_ACCESS != 0 && !administrator(sid) {
        return Err(unsafe_permissions(
            "Installer directory permits modification by an unprivileged user",
        ));
    }
    Ok(())
}

fn administrator(sid: PSID) -> bool {
    !sid.0.is_null()
        && unsafe {
            IsWellKnownSid(sid, WinBuiltinAdministratorsSid).as_bool()
                || IsWellKnownSid(sid, WinLocalSystemSid).as_bool()
        }
}

fn unsafe_permissions(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_a_trusted_owner_and_rejects_unprivileged_directory_mutation() {
        Descriptor::installer().unwrap().verify().unwrap();
        for sddl in [
            "O:BUG:BUD:P(A;;FRFX;;;BU)",
            "O:BAG:BAD:P(A;;FA;;;BA)(A;;FA;;;BU)",
            "O:BAG:BAD:P(A;;FA;;;BA)(A;;DC;;;BU)",
            "O:BAG:BAD:NO_ACCESS_CONTROL",
        ] {
            assert!(
                Descriptor::parse(sddl).unwrap().verify().is_err(),
                "accepted {sddl}"
            );
        }
    }
}
