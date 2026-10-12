use hbb_common::{
    anyhow::anyhow,
    bail,
    libloading::os::windows::{Library, LOAD_LIBRARY_SEARCH_SYSTEM32},
    log, ResultType,
};
use std::{
    cell::Cell,
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    io, ptr,
    sync::OnceLock,
};
use winapi::{
    shared::{
        minwindef::UINT,
        windef::{DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_UNAWARE, HCURSOR, HMONITOR, POINT},
        winerror::S_OK,
    },
    um::{
        shellscalingapi::{MDT_EFFECTIVE_DPI, MONITOR_DPI_TYPE},
        winnt::HRESULT,
        winuser::{
            CopyImage, DestroyCursor, LoadCursorW, MonitorFromPoint, CURSORINFO, IDC_APPSTARTING,
            IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_HELP, IDC_IBEAM, IDC_ICON, IDC_NO, IDC_SIZE,
            IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, IDC_UPARROW, IDC_WAIT,
            IMAGE_CURSOR, LR_COPYFROMRESOURCE, MONITOR_DEFAULTTONULL, SM_CXCURSOR, SM_CYCURSOR,
            USER_DEFAULT_SCREEN_DPI,
        },
    },
};

type MonitorDpi =
    unsafe extern "system" fn(HMONITOR, MONITOR_DPI_TYPE, *mut UINT, *mut UINT) -> HRESULT;
type CursorMetrics = unsafe extern "system" fn(i32, UINT) -> i32;
type ThreadDpi = unsafe extern "system" fn(DPI_AWARENESS_CONTEXT) -> DPI_AWARENESS_CONTEXT;

#[derive(Clone, Copy)]
struct Capture {
    id: u64,
    handle: u64,
    scale: f64,
}

thread_local! {
    static CAPTURE: Cell<Option<Capture>> = const { Cell::new(None) };
}

pub(super) fn capture(info: &CURSORINFO) -> ResultType<u64> {
    let scale = monitor_scale(info.ptScreenPos)?;
    let handle = info.hCursor as u64;
    let icon = super::IconInfo::new(info.hCursor)?;
    let bitmap = super::get_bitmap(icon.0.hbmMask)?;
    // Windows can keep a cursor handle while its DPI variant changes.
    let mut hash = DefaultHasher::new();
    (
        handle,
        scale.to_bits(),
        bitmap.bmWidth,
        bitmap.bmHeight,
        icon.0.xHotspot,
        icon.0.yHotspot,
    )
        .hash(&mut hash);
    const MAX_CURSOR_ID: u64 = (1 << 53) - 1;
    let id = hash.finish() % MAX_CURSOR_ID + 1;
    CAPTURE.with(|capture| capture.set(Some(Capture { id, handle, scale })));
    Ok(id)
}

pub(super) fn captured(id: u64) -> ResultType<(u64, f64)> {
    // The cursor service reads the bitmap on the same thread as its ID poll.
    // Keep the density from that poll instead of sampling a possibly different monitor.
    CAPTURE
        .with(|capture| capture.get())
        .filter(|capture| capture.id == id)
        .map(|capture| (capture.handle, capture.scale))
        .ok_or_else(|| anyhow!("Windows cursor capture does not match requested ID {id}"))
}

pub(super) fn icon_info(handle: HCURSOR, scale: f64) -> ResultType<super::IconInfo> {
    let original = super::IconInfo::new(handle)?;
    if scale == 0.0 {
        return Ok(original);
    }
    unsafe {
        // Application-created cursors already carry their intended size and hotspot.
        let system_cursor = [
            IDC_ARROW,
            IDC_IBEAM,
            IDC_WAIT,
            IDC_CROSS,
            IDC_UPARROW,
            IDC_SIZE,
            IDC_ICON,
            IDC_SIZENWSE,
            IDC_SIZENESW,
            IDC_SIZEWE,
            IDC_SIZENS,
            IDC_SIZEALL,
            IDC_NO,
            IDC_HAND,
            IDC_APPSTARTING,
            IDC_HELP,
        ]
        .into_iter()
        .any(|resource| LoadCursorW(ptr::null_mut(), resource) == handle);
        if !system_cursor {
            return Ok(original);
        }
        let Some(((width, height), set_dpi)) = cursor_size(scale)? else {
            return Ok(original);
        };
        let bitmap = super::get_bitmap(original.0.hbmMask)?;
        let mask_height = if original.is_color() {
            height
        } else {
            height * 2
        };
        if bitmap.bmWidth == width && bitmap.bmHeight == mask_height {
            return Ok(original);
        }
        // GetIconInfo can expose the primary monitor's raster on another monitor.
        copy_icon_info(handle, (width, height), (scale == 1.0).then_some(set_dpi))
    }
}

fn copy_icon_info(
    handle: HCURSOR,
    size: (i32, i32),
    set_dpi: Option<ThreadDpi>,
) -> ResultType<super::IconInfo> {
    unsafe {
        if let Some(set_dpi) = set_dpi {
            // CopyImage rescales the primary I-beam instead of selecting its 100% variant.
            let previous = set_dpi(DPI_AWARENESS_CONTEXT_UNAWARE);
            if previous.is_null() {
                return Err(io::Error::last_os_error().into());
            }
            let result = super::IconInfo::new(handle);
            if set_dpi(previous).is_null() {
                return Err(io::Error::last_os_error().into());
            }
            return result;
        }
        let cursor = CopyImage(
            handle as _,
            IMAGE_CURSOR,
            size.0,
            size.1,
            LR_COPYFROMRESOURCE,
        ) as HCURSOR;
        if cursor.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        let result = super::IconInfo::new(cursor);
        if DestroyCursor(cursor) == 0 {
            return Err(io::Error::last_os_error().into());
        }
        result
    }
}

fn cursor_size(scale: f64) -> ResultType<Option<((i32, i32), ThreadDpi)>> {
    static API: OnceLock<Option<(Library, CursorMetrics, ThreadDpi)>> = OnceLock::new();
    let Some((_, query, set_dpi)) = API.get_or_init(|| match load_cursor_metrics() {
        Ok(api) => Some(api),
        Err(err) => {
            log::warn!("Windows cursor size API unavailable; retaining original bitmap: {err}");
            None
        }
    }) else {
        return Ok(None);
    };
    let dpi = (scale * f64::from(USER_DEFAULT_SCREEN_DPI)).round() as UINT;
    let (width, height) = unsafe { (query(SM_CXCURSOR, dpi), query(SM_CYCURSOR, dpi)) };
    if width <= 0 || height <= 0 {
        bail!("Invalid Windows cursor size ({width}, {height}) at DPI {dpi}");
    }
    Ok(Some(((width, height), *set_dpi)))
}

fn load_cursor_metrics() -> ResultType<(Library, CursorMetrics, ThreadDpi)> {
    unsafe {
        let library = Library::load_with_flags("user32.dll", LOAD_LIBRARY_SEARCH_SYSTEM32)?;
        let query = *library.get::<CursorMetrics>(b"GetSystemMetricsForDpi\0")?;
        let set_dpi = *library.get::<ThreadDpi>(b"SetThreadDpiAwarenessContext\0")?;
        Ok((library, query, set_dpi))
    }
}

fn load_monitor_dpi() -> ResultType<(Library, MonitorDpi)> {
    unsafe {
        let library = Library::load_with_flags("Shcore.dll", LOAD_LIBRARY_SEARCH_SYSTEM32)?;
        let query = *library.get::<MonitorDpi>(b"GetDpiForMonitor\0")?;
        Ok((library, query))
    }
}

fn monitor_scale(point: POINT) -> ResultType<f64> {
    // Win7/8 may lack this API. Retain its library for the function pointer's lifetime.
    static API: OnceLock<Option<(Library, MonitorDpi)>> = OnceLock::new();
    let Some((_, query)) = API.get_or_init(|| match load_monitor_dpi() {
        Ok(api) => Some(api),
        Err(err) => {
            log::warn!("Windows cursor DPI API unavailable; reporting unknown density: {err}");
            None
        }
    }) else {
        return Ok(0.0);
    };
    unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONULL);
        if monitor.is_null() {
            bail!(
                "No monitor at Windows cursor position ({}, {})",
                point.x,
                point.y
            );
        }
        let (mut dpi_x, mut dpi_y) = (0, 0);
        let result = query(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        if result != S_OK {
            bail!("Failed to query Windows cursor DPI: HRESULT {result:#x}");
        }
        if dpi_x == 0 || dpi_y != dpi_x {
            bail!("Invalid Windows cursor DPI ({dpi_x}, {dpi_y})");
        }
        Ok(f64::from(dpi_x) / f64::from(USER_DEFAULT_SCREEN_DPI))
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::CStr, hint::black_box, ptr};
    use winapi::um::{
        libloaderapi::GetModuleHandleW,
        winnt::{
            IMAGE_DIRECTORY_ENTRY_IMPORT, IMAGE_DOS_HEADER, IMAGE_IMPORT_BY_NAME,
            IMAGE_IMPORT_DESCRIPTOR, IMAGE_NT_HEADERS, IMAGE_ORDINAL_FLAG,
        },
    };

    #[test]
    fn cursor_dpi_does_not_require_a_load_time_import() {
        // Keep the production query reachable even when this is the only selected test.
        black_box(super::monitor_scale as fn(_) -> _);
        black_box(super::cursor_size as fn(_) -> _);
        unsafe {
            let base = GetModuleHandleW(ptr::null()).cast::<u8>();
            assert!(!base.is_null());
            let dos = &*base.cast::<IMAGE_DOS_HEADER>();
            let nt = &*base.add(dos.e_lfanew as usize).cast::<IMAGE_NT_HEADERS>();
            let imports = nt.OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_IMPORT as usize];
            assert_ne!(imports.VirtualAddress, 0);
            let mut descriptor = base
                .add(imports.VirtualAddress as usize)
                .cast::<IMAGE_IMPORT_DESCRIPTOR>();
            while (*descriptor).Name != 0 {
                assert!(
                    !imports_cursor_dpi(base, *(*descriptor).u.OriginalFirstThunk()),
                    "Cursor DPI APIs must be resolved at runtime"
                );
                descriptor = descriptor.add(1);
            }
        }
    }

    unsafe fn imports_cursor_dpi(base: *const u8, table: u32) -> bool {
        assert_ne!(table, 0);
        let mut entry = base.add(table as usize).cast::<usize>();
        while *entry != 0 {
            if *entry & IMAGE_ORDINAL_FLAG as usize == 0 {
                let import = &*base.add(*entry).cast::<IMAGE_IMPORT_BY_NAME>();
                if matches!(
                    CStr::from_ptr(import.Name.as_ptr()).to_bytes(),
                    b"GetDpiForMonitor"
                        | b"GetSystemMetricsForDpi"
                        | b"SetThreadDpiAwarenessContext"
                ) {
                    return true;
                }
            }
            entry = entry.add(1);
        }
        false
    }
}
