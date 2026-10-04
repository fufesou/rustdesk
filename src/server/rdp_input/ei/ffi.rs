use hbb_common::{libloading::Library, ResultType};
use std::ffi::{c_char, c_int, c_void};

macro_rules! opaque {
    ($($name:ident),+) => { $(
        #[repr(C)]
        pub(super) struct $name { _private: [u8; 0] }
    )+ };
}
opaque!(Ei, Event, Seat, Device, Keymap, Region);

pub(super) const POINTER: u32 = 1;
pub(super) const ABSOLUTE: u32 = 1 << 1;
pub(super) const KEYBOARD: u32 = 1 << 2;
pub(super) const SCROLL: u32 = 1 << 4;
pub(super) const BUTTON: u32 = 1 << 5;
pub(super) const CAPABILITIES: [u32; 5] = [POINTER, ABSOLUTE, KEYBOARD, SCROLL, BUTTON];

pub(super) const DISCONNECT: c_int = 2;
pub(super) const SEAT_ADDED: c_int = 3;
pub(super) const DEVICE_ADDED: c_int = 5;
pub(super) const DEVICE_REMOVED: c_int = 6;
pub(super) const DEVICE_PAUSED: c_int = 7;
pub(super) const DEVICE_RESUMED: c_int = 8;
pub(super) const KEYBOARD_MODIFIERS: c_int = 9;
pub(super) const KEYMAP_XKB: c_int = 1;

macro_rules! api {
    ($($name:ident: $signature:ty),+ $(,)?) => {
        pub(super) struct Api {
            $(pub $name: $signature,)+
            _library: Library,
        }

        impl Api {
            pub fn load() -> ResultType<Self> {
                // Keep the library alive for all function pointers and EI objects.
                let library = unsafe { Library::new("libei.so.1") }?;
                Ok(Self {
                    $($name: unsafe { *library.get(concat!(stringify!($name), "\0").as_bytes())? },)+
                    _library: library,
                })
            }
        }
    };
}

api! {
    ei_new_sender: unsafe extern "C" fn(*mut c_void) -> *mut Ei,
    ei_unref: unsafe extern "C" fn(*mut Ei) -> *mut Ei,
    ei_configure_name: unsafe extern "C" fn(*mut Ei, *const c_char),
    ei_setup_backend_fd: unsafe extern "C" fn(*mut Ei, c_int) -> c_int,
    ei_get_fd: unsafe extern "C" fn(*mut Ei) -> c_int,
    ei_dispatch: unsafe extern "C" fn(*mut Ei),
    ei_get_event: unsafe extern "C" fn(*mut Ei) -> *mut Event,
    ei_event_unref: unsafe extern "C" fn(*mut Event) -> *mut Event,
    ei_event_get_type: unsafe extern "C" fn(*mut Event) -> c_int,
    ei_event_get_seat: unsafe extern "C" fn(*mut Event) -> *mut Seat,
    ei_event_get_device: unsafe extern "C" fn(*mut Event) -> *mut Device,
    ei_seat_bind_capabilities: unsafe extern "C" fn(*mut Seat, ...),
    ei_device_ref: unsafe extern "C" fn(*mut Device) -> *mut Device,
    ei_device_unref: unsafe extern "C" fn(*mut Device) -> *mut Device,
    ei_device_has_capability: unsafe extern "C" fn(*mut Device, u32) -> bool,
    ei_device_start_emulating: unsafe extern "C" fn(*mut Device, u32),
    ei_device_frame: unsafe extern "C" fn(*mut Device, u64),
    ei_now: unsafe extern "C" fn(*mut Ei) -> u64,
    ei_device_keyboard_get_keymap: unsafe extern "C" fn(*mut Device) -> *mut Keymap,
    ei_keymap_get_type: unsafe extern "C" fn(*mut Keymap) -> c_int,
    ei_keymap_get_fd: unsafe extern "C" fn(*mut Keymap) -> c_int,
    ei_keymap_get_size: unsafe extern "C" fn(*mut Keymap) -> usize,
    ei_event_keyboard_get_xkb_mods_depressed: unsafe extern "C" fn(*mut Event) -> u32,
    ei_event_keyboard_get_xkb_mods_latched: unsafe extern "C" fn(*mut Event) -> u32,
    ei_event_keyboard_get_xkb_mods_locked: unsafe extern "C" fn(*mut Event) -> u32,
    ei_event_keyboard_get_xkb_group: unsafe extern "C" fn(*mut Event) -> u32,
    ei_device_keyboard_key: unsafe extern "C" fn(*mut Device, u32, bool),
    ei_device_pointer_motion: unsafe extern "C" fn(*mut Device, f64, f64),
    ei_device_pointer_motion_absolute: unsafe extern "C" fn(*mut Device, f64, f64),
    ei_device_button_button: unsafe extern "C" fn(*mut Device, u32, bool),
    ei_device_scroll_delta: unsafe extern "C" fn(*mut Device, f64, f64),
    ei_device_get_region: unsafe extern "C" fn(*mut Device, usize) -> *mut Region,
    ei_region_get_x: unsafe extern "C" fn(*mut Region) -> u32,
    ei_region_get_y: unsafe extern "C" fn(*mut Region) -> u32,
    ei_region_get_width: unsafe extern "C" fn(*mut Region) -> u32,
    ei_region_get_height: unsafe extern "C" fn(*mut Region) -> u32,
    ei_region_get_mapping_id: unsafe extern "C" fn(*mut Region) -> *const c_char,
}
