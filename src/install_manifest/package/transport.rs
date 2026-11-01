use std::{io, process::Command};
use windows::{
    core::Owned,
    Win32::{
        Foundation::{DuplicateHandle, HANDLE, INVALID_HANDLE_VALUE},
        System::{
            Memory::{
                CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, FILE_MAP_WRITE,
                MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
            },
            Threading::GetCurrentProcess,
        },
    },
};

const CONTEXT_ENV: &str = "RUSTDESK_INSTALL_PAYLOAD";

pub(crate) struct Mapping(Owned<HANDLE>);

struct View(MEMORY_MAPPED_VIEW_ADDRESS);

impl Drop for View {
    fn drop(&mut self) {
        if let Err(error) = unsafe { UnmapViewOfFile(self.0) } {
            eprintln!("Cannot unmap installation package context: {error}");
        }
    }
}

impl Mapping {
    pub(crate) fn new(bytes: &[u8]) -> io::Result<Self> {
        let size = u32::try_from(bytes.len()).map_err(io::Error::other)?;
        let writable = unsafe {
            Owned::new(CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                size,
                None,
            )?)
        };
        let view = map(*writable, FILE_MAP_WRITE, bytes.len())?;
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), view.0.Value.cast(), bytes.len()) };
        drop(view);
        let mut readonly = HANDLE::default();
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                *writable,
                GetCurrentProcess(),
                &mut readonly,
                FILE_MAP_READ.0,
                true,
                Default::default(),
            )?;
        }
        // No writable handle or view survives the transfer to the application.
        Ok(Self(unsafe { Owned::new(readonly) }))
    }

    pub(crate) fn attach(&self, command: &mut Command, length: usize) {
        command.env(CONTEXT_ENV, format!("{}:{length}", self.0 .0 as usize));
    }
}

pub(super) fn receive() -> io::Result<Option<Vec<u8>>> {
    let Some(value) = std::env::var_os(CONTEXT_ENV) else {
        return Ok(None);
    };
    std::env::remove_var(CONTEXT_ENV);
    let value = value
        .to_str()
        .ok_or_else(|| io::Error::other("Invalid package context"))?;
    let (handle, length) = value
        .split_once(':')
        .ok_or_else(|| io::Error::other("Invalid package context"))?;
    let handle = handle.parse::<usize>().map_err(io::Error::other)?;
    let length = length.parse::<usize>().map_err(io::Error::other)?;
    if handle == 0 || handle == usize::MAX || length == 0 {
        return Err(io::Error::other("Empty package context"));
    }
    let handle = unsafe { Owned::new(HANDLE(handle as _)) };
    let view = map(*handle, FILE_MAP_READ, length)?;
    Ok(Some(unsafe {
        std::slice::from_raw_parts(view.0.Value.cast::<u8>(), length).to_vec()
    }))
}

fn map(
    handle: HANDLE,
    access: windows::Win32::System::Memory::FILE_MAP,
    length: usize,
) -> io::Result<View> {
    let address = unsafe { MapViewOfFile(handle, access, 0, 0, length) };
    if address.Value.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(View(address))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfers_readonly_context_to_a_child() {
        const PAYLOAD: &[u8] = b"embedded package list";
        if std::env::var_os(CONTEXT_ENV).is_some() {
            assert_eq!(receive().unwrap().unwrap(), PAYLOAD);
            return;
        }
        let mapping = Mapping::new(PAYLOAD).unwrap();
        assert!(map(*mapping.0, FILE_MAP_WRITE, PAYLOAD.len()).is_err());
        let mut command = Command::new(std::env::current_exe().unwrap());
        let module = module_path!().split_once("::").unwrap().1;
        command.args([
            "--exact",
            &format!("{module}::transfers_readonly_context_to_a_child"),
            "--nocapture",
        ]);
        mapping.attach(&mut command, PAYLOAD.len());
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
