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

pub(super) fn active_source(reply: &[u8]) -> ResultType<Option<String>> {
    if reply.len() != REPLY_SIZE {
        bail!(
            "Invalid RustDesk IDD target response length: {}",
            reply.len()
        );
    }
    // The wire fields are integers and the byte buffer need not be aligned.
    let target = unsafe { reply.as_ptr().cast::<CaptureColor>().read_unaligned() };
    let (mut path_count, mut mode_count) = (0, 0);
    unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count) }
        .ok()?;
    if path_count == 0 {
        return Ok(None);
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
    let Some(path) = paths[..path_count as usize].iter().find(|path| {
        path.targetInfo.adapterId == target.adapter && path.targetInfo.id == target.target_id
    }) else {
        return Ok(None);
    };
    let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
    source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
    source.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
    source.header.adapterId = path.sourceInfo.adapterId;
    source.header.id = path.sourceInfo.id;
    let status = unsafe { DisplayConfigGetDeviceInfo(&mut source.header) };
    if status != ERROR_SUCCESS.0 as i32 {
        return Err(io::Error::from_raw_os_error(status).into());
    }
    let name = &source.viewGdiDeviceName;
    let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
    if end == 0 {
        return Ok(None);
    }
    Ok(Some(String::from_utf16(&name[..end])?))
}
