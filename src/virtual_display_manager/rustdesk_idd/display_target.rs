use hbb_common::{bail, ResultType};
use std::{io, mem::size_of};
use windows::Win32::{
    Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_MODE_INFO,
        DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
    },
    Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LUID},
};

// Header-only IOCTL_IDD_QUERY_COLOR reply (CtlCaptureColor in DisplayProfile.h).
#[repr(C)]
struct CaptureColor {
    color_state: [u32; 15],
    generation: u64,
    adapter: LUID,
    target_id: u32,
    render_adapter: LUID,
}
pub(super) const REPLY_SIZE: usize = size_of::<CaptureColor>();

pub(super) type Target = (LUID, u32);

pub(super) fn target_id(reply: &[u8]) -> ResultType<Target> {
    if reply.len() != REPLY_SIZE {
        bail!(
            "Invalid RustDesk IDD target response length: {}",
            reply.len()
        );
    }
    // The wire fields are integers and the byte buffer need not be aligned.
    let target = unsafe { reply.as_ptr().cast::<CaptureColor>().read_unaligned() };
    Ok((target.adapter, target.target_id))
}

pub(super) fn active_paths() -> ResultType<Option<Vec<DISPLAYCONFIG_PATH_INFO>>> {
    let (mut path_count, mut mode_count) = (0, 0);
    unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count) }
        .ok()?;
    if path_count == 0 {
        return Ok(Some(Vec::new()));
    }
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
    let status = unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        )
    };
    if status == ERROR_INSUFFICIENT_BUFFER {
        return Ok(None); // Windows changed the topology between the two queries.
    }
    status.ok()?;
    paths.truncate(path_count as usize);
    Ok(Some(paths))
}

pub(super) fn path_target(path: &DISPLAYCONFIG_PATH_INFO) -> Target {
    (path.targetInfo.adapterId, path.targetInfo.id)
}

pub(super) fn active_source(target: Target) -> ResultType<Option<String>> {
    let Some(paths) = active_paths()? else {
        return Ok(None);
    };
    let Some(path) = paths.iter().find(|path| path_target(path) == target) else {
        return Ok(None);
    };
    let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
    source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
    source.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
    source.header.adapterId = path.sourceInfo.adapterId;
    source.header.id = path.sourceInfo.id;
    let status = unsafe { DisplayConfigGetDeviceInfo(&mut source.header) };
    if status != ERROR_SUCCESS.0 as i32 {
        // Windows can replace the source between enumerating the paths and reading its name.
        let still_active = active_paths()?.is_some_and(|paths| {
            paths.iter().any(|p| {
                path_target(p) == target
                    && p.sourceInfo.adapterId == source.header.adapterId
                    && p.sourceInfo.id == source.header.id
            })
        });
        if !still_active {
            return Ok(None);
        }
        return Err(io::Error::from_raw_os_error(status).into());
    }
    let name = &source.viewGdiDeviceName;
    let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
    if end == 0 {
        return Ok(None);
    }
    Ok(Some(String::from_utf16(&name[..end])?))
}
