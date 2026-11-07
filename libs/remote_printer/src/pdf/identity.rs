use super::{
    handles::{check_size, Buffer, Handle},
    queues,
    security::Descriptor,
};
use hbb_common::{anyhow::Context, bail, ResultType};
use std::{mem::size_of, path::Path, time::Duration};
use windows::{
    core::{w, HSTRING, PCWSTR, PWSTR},
    Win32::{
        Foundation::{LocalFree, HANDLE, HLOCAL},
        Security::{
            Authorization::ConvertSidToStringSidW, GetTokenInformation, LookupAccountNameW,
            TokenUser, PSID, SID_NAME_USE, TOKEN_QUERY, TOKEN_USER,
        },
        Storage::FileSystem::CreateDirectoryW,
        System::{
            RemoteDesktop::{
                ProcessIdToSessionId, WTSDomainName, WTSFreeMemory, WTSGetActiveConsoleSessionId,
                WTSQuerySessionInformationW, WTSUserName, WTS_INFO_CLASS,
            },
            Services::{
                CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_HANDLE,
                SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
                SERVICE_STATUS_PROCESS,
            },
            Threading::{GetCurrentProcess, GetCurrentProcessId, OpenProcessToken},
        },
    },
};

pub fn controlled_user_sid() -> ResultType<String> {
    let mut session = 0;
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session)? };
    if session != unsafe { WTSGetActiveConsoleSessionId() } {
        bail!("PDF capture requires the active physical console session");
    }
    let user = session_text(session, WTSUserName)?;
    let domain = session_text(session, WTSDomainName)?;
    if user.is_empty() || domain.is_empty() {
        bail!("The controlled Windows desktop has no identifiable signed-in user");
    }
    account_sid(&format!(r"{domain}\{user}"))
}

fn session_text(session: u32, class: WTS_INFO_CLASS) -> ResultType<String> {
    let mut buffer = PWSTR::null();
    let mut length = 0;
    unsafe { WTSQuerySessionInformationW(None, session, class, &mut buffer, &mut length)? };
    let result = queues::text(buffer);
    unsafe { WTSFreeMemory(buffer.0.cast()) };
    result
}

pub(crate) fn account_sid(account: &str) -> ResultType<String> {
    if account.is_empty() || account.contains('\0') {
        bail!("Invalid print job account");
    }
    let account = HSTRING::from(account);
    let mut sid_length = 0;
    let mut domain_length = 0;
    let mut kind = SID_NAME_USE::default();
    check_size(unsafe {
        LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(account.as_ptr()),
            None,
            &mut sid_length,
            None,
            &mut domain_length,
            &mut kind,
        )
    })?;
    let mut sid = Buffer::new(sid_length)?;
    let mut domain = vec![0u16; domain_length as usize];
    let sid = PSID(sid.bytes().as_mut_ptr().cast());
    unsafe {
        LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(account.as_ptr()),
            Some(sid),
            &mut sid_length,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut domain_length,
            &mut kind,
        )?
    };
    sid_text(sid)
}

fn current_user_sid() -> ResultType<String> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)? };
    let token = Handle(token);
    let buffer = Buffer::query(|buffer, length| unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.map(|bytes| bytes.as_mut_ptr().cast()),
            *length,
            length,
        )
    })?
    .context("The print API returned an empty buffer size")?;
    sid_text(unsafe { buffer.item::<TOKEN_USER>(0)? }.User.Sid)
}

pub(crate) fn require_printing_user() -> ResultType<()> {
    const LOCAL_SYSTEM_SID: &str = "S-1-5-18";
    if current_user_sid()? == LOCAL_SYSTEM_SID {
        bail!("Received PDFs must be rendered by the controller user, not LocalSystem");
    }
    Ok(())
}

fn sid_text(sid: PSID) -> ResultType<String> {
    let mut value = PWSTR::null();
    unsafe { ConvertSidToStringSidW(sid, &mut value)? };
    let result = queues::text(value);
    let remaining = unsafe { LocalFree(Some(HLOCAL(value.0.cast()))) };
    if !remaining.is_invalid() {
        bail!("Failed to release the Windows SID string");
    }
    result
}

pub fn create_private_directory(path: &Path) -> ResultType<()> {
    let sid = current_user_sid()?;
    let descriptor = Descriptor::new(&format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{sid})"))?;
    let path = HSTRING::from(path.as_os_str());
    unsafe { CreateDirectoryW(PCWSTR(path.as_ptr()), Some(&descriptor.attributes()))? };
    Ok(())
}

pub fn spooler_pid() -> ResultType<u32> {
    let manager = ServiceHandle(unsafe {
        OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)?
    });
    let service =
        ServiceHandle(unsafe { OpenServiceW(manager.0, w!("Spooler"), SERVICE_QUERY_STATUS)? });
    let mut status = SERVICE_STATUS_PROCESS::default();
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast::<u8>(),
            size_of::<SERVICE_STATUS_PROCESS>(),
        )
    };
    let mut needed = 0;
    unsafe { QueryServiceStatusEx(service.0, SC_STATUS_PROCESS_INFO, Some(bytes), &mut needed)? };
    if status.dwCurrentState != SERVICE_RUNNING || status.dwProcessId == 0 {
        bail!("The Windows Print Spooler is not running");
    }
    Ok(status.dwProcessId)
}

struct ServiceHandle(SC_HANDLE);

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        if let Err(error) = unsafe { CloseServiceHandle(self.0) } {
            hbb_common::throttled_log!(
                Duration::from_secs(60),
                warn,
                "PDF service handle cleanup failed: {error}"
            );
        }
    }
}
