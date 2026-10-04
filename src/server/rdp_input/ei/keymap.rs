use super::ffi;
use hbb_common::{anyhow::anyhow, bail, libloading::Library, ResultType};
use std::{
    ffi::{c_char, c_int, CStr},
    fs::File,
    os::fd::BorrowedFd,
    os::unix::fs::FileExt,
    ptr::NonNull,
};
use xkbcommon_dl::{self as xkb, keysyms};

pub(super) const XKB_OFFSET: u32 = 8;

#[derive(Clone)]
pub(super) struct Mapping {
    pub code: u32,
    pub modifiers: Vec<u32>,
}

#[derive(Clone, Copy)]
struct Modifier {
    code: u32,
    mask: u32,
}

pub(super) struct Keymap {
    api: &'static xkb::XkbCommon,
    map: NonNull<xkb::xkb_keymap>,
    state: NonNull<xkb::xkb_state>,
    lookup: NonNull<xkb::xkb_state>,
    pub ready: bool,
    led: unsafe extern "C" fn(*mut xkb::xkb_state, *const c_char) -> c_int,
    _library: Library,
}

impl Keymap {
    pub fn new(api: &ffi::Api, device: *mut ffi::Device) -> ResultType<Self> {
        // xkbcommon-dl 0.4 does not expose the indicator-state API.
        let library = unsafe { Library::new("libxkbcommon.so.0") }?;
        let led = unsafe { *library.get(b"xkb_state_led_name_is_active\0")? };
        let bytes = read(api, device)?;
        let api = xkb::xkbcommon_option().ok_or_else(|| anyhow!("Cannot load libxkbcommon"))?;
        let context = unsafe {
            (api.xkb_context_new)(xkb::xkb_context_flags::XKB_CONTEXT_NO_ENVIRONMENT_NAMES)
        };
        let context = NonNull::new(context).ok_or_else(|| anyhow!("Cannot create XKB context"))?;
        let map = unsafe {
            let map = (api.xkb_keymap_new_from_string)(
                context.as_ptr(),
                bytes.as_ptr().cast(),
                xkb::xkb_keymap_format::XKB_KEYMAP_FORMAT_TEXT_V1,
                xkb::xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
            );
            (api.xkb_context_unref)(context.as_ptr());
            map
        };
        let map = NonNull::new(map).ok_or_else(|| anyhow!("Invalid EIS XKB keymap"))?;
        let state = unsafe { (api.xkb_state_new)(map.as_ptr()) };
        let lookup = unsafe { (api.xkb_state_new)(map.as_ptr()) };
        match (NonNull::new(state), NonNull::new(lookup)) {
            (Some(state), Some(lookup)) => Ok(Self {
                api,
                map,
                state,
                lookup,
                ready: false,
                led,
                _library: library,
            }),
            _ => {
                unsafe {
                    (api.xkb_state_unref)(state);
                    (api.xkb_state_unref)(lookup);
                    (api.xkb_keymap_unref)(map.as_ptr());
                }
                bail!("Cannot create EIS XKB state")
            }
        }
    }

    pub fn update(&mut self, api: &ffi::Api, event: *mut ffi::Event) -> ResultType<()> {
        unsafe {
            let group = (api.ei_event_keyboard_get_xkb_group)(event);
            if group >= (self.api.xkb_keymap_num_layouts)(self.map.as_ptr()) {
                bail!("EIS reported a group outside its keyboard map");
            }
            (self.api.xkb_state_update_mask)(
                self.state.as_ptr(),
                (api.ei_event_keyboard_get_xkb_mods_depressed)(event),
                (api.ei_event_keyboard_get_xkb_mods_latched)(event),
                (api.ei_event_keyboard_get_xkb_mods_locked)(event),
                0,
                0,
                group,
            );
        }
        self.ready = true;
        Ok(())
    }

    pub fn locked(&self, name: &[u8]) -> ResultType<bool> {
        if !self.ready {
            bail!("EIS keyboard has not reported its modifier state");
        }
        let active = unsafe { (self.led)(self.state.as_ptr(), name.as_ptr().cast()) };
        if active < 0 {
            bail!("EIS keyboard map does not define the requested lock indicator");
        }
        Ok(active != 0)
    }

    pub fn resolve(&mut self, character: char, shortcut: bool) -> ResultType<Mapping> {
        if !self.ready {
            bail!("EIS keyboard has not reported its layout/modifier state");
        }
        let character = if shortcut {
            character.to_ascii_lowercase()
        } else {
            character
        };
        let locked = self.locked_mask(shortcut);
        self.set_lookup(0, 0);
        let modifiers = [
            self.modifier(&[keysyms::Shift_L, keysyms::Shift_R]),
            self.modifier(&[keysyms::ISO_Level3_Shift]),
        ];
        for combination in 0..(1usize << modifiers.len()) {
            let selected: Vec<_> = modifiers
                .iter()
                .enumerate()
                .filter(|(index, _)| combination & (1 << index) != 0)
                .filter_map(|(_, modifier)| *modifier)
                .collect();
            let depressed = selected
                .iter()
                .fold(0, |mask, modifier| mask | modifier.mask);
            self.set_lookup(depressed, locked);
            if let Some(code) = self
                .codes()
                .find(|code| self.character(*code) == character as u32)
            {
                return Ok(Mapping {
                    code: code - XKB_OFFSET,
                    modifiers: selected
                        .iter()
                        .map(|modifier| modifier.code - XKB_OFFSET)
                        .collect(),
                });
            }
        }
        bail!("Character is unavailable in the EIS keyboard layout")
    }

    fn locked_mask(&self, shortcut: bool) -> u32 {
        unsafe {
            let locked = (self.api.xkb_state_serialize_mods)(
                self.state.as_ptr(),
                xkb::xkb_state_component::XKB_STATE_MODS_LOCKED,
            );
            let caps =
                (self.api.xkb_keymap_mod_get_index)(self.map.as_ptr(), b"Lock\0".as_ptr().cast());
            if shortcut && caps < u32::BITS {
                locked & !(1 << caps)
            } else {
                locked
            }
        }
    }

    fn set_lookup(&mut self, depressed: u32, locked: u32) {
        unsafe {
            let group = (self.api.xkb_state_serialize_layout)(
                self.state.as_ptr(),
                xkb::xkb_state_component::XKB_STATE_LAYOUT_EFFECTIVE,
            );
            (self.api.xkb_state_update_mask)(
                self.lookup.as_ptr(),
                depressed,
                0,
                locked,
                0,
                0,
                group,
            );
        }
    }

    fn codes(&self) -> std::ops::RangeInclusive<u32> {
        unsafe {
            (self.api.xkb_keymap_min_keycode)(self.map.as_ptr()).max(XKB_OFFSET)
                ..=(self.api.xkb_keymap_max_keycode)(self.map.as_ptr())
        }
    }

    fn character(&self, code: u32) -> u32 {
        unsafe { (self.api.xkb_state_key_get_utf32)(self.lookup.as_ptr(), code) }
    }

    fn modifier(&mut self, symbols: &[u32]) -> Option<Modifier> {
        let preferred = [
            evdev::Key::KEY_LEFTSHIFT,
            evdev::Key::KEY_RIGHTSHIFT,
            evdev::Key::KEY_RIGHTALT,
        ];
        let candidates = preferred
            .iter()
            .map(|key| u32::from(key.code()) + XKB_OFFSET)
            .chain(self.codes());
        for code in candidates {
            let symbol =
                unsafe { (self.api.xkb_state_key_get_one_sym)(self.lookup.as_ptr(), code) };
            if !symbols.contains(&symbol) {
                continue;
            }
            let mask = unsafe {
                (self.api.xkb_state_update_key)(
                    self.lookup.as_ptr(),
                    code,
                    xkb::xkb_key_direction::XKB_KEY_DOWN,
                );
                let mask = (self.api.xkb_state_serialize_mods)(
                    self.lookup.as_ptr(),
                    xkb::xkb_state_component::XKB_STATE_MODS_DEPRESSED,
                );
                (self.api.xkb_state_update_key)(
                    self.lookup.as_ptr(),
                    code,
                    xkb::xkb_key_direction::XKB_KEY_UP,
                );
                mask
            };
            self.set_lookup(0, 0);
            if mask != 0 {
                return Some(Modifier { code, mask });
            }
        }
        None
    }
}

impl Drop for Keymap {
    fn drop(&mut self) {
        unsafe {
            (self.api.xkb_state_unref)(self.state.as_ptr());
            (self.api.xkb_state_unref)(self.lookup.as_ptr());
            (self.api.xkb_keymap_unref)(self.map.as_ptr());
        }
    }
}

fn read(api: &ffi::Api, device: *mut ffi::Device) -> ResultType<Vec<u8>> {
    let keymap = NonNull::new(unsafe { (api.ei_device_keyboard_get_keymap)(device) })
        .ok_or_else(|| anyhow!("EIS keyboard has no keymap"))?;
    let keymap = keymap.as_ptr();
    let (fd, size) = unsafe {
        if (api.ei_keymap_get_type)(keymap) != ffi::KEYMAP_XKB {
            bail!("EIS keymap is not XKB");
        }
        (
            (api.ei_keymap_get_fd)(keymap),
            (api.ei_keymap_get_size)(keymap),
        )
    };
    if fd < 0 {
        bail!("EIS keymap has no file descriptor");
    }
    let file = File::from(unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?);
    if file.metadata()?.len() < size as u64 {
        bail!("EIS keymap file is truncated");
    }
    let mut bytes = vec![0; size];
    file.read_exact_at(&mut bytes, 0)?;
    if bytes.last() != Some(&0) {
        bytes.push(0);
    }
    CStr::from_bytes_with_nul(&bytes)?;
    Ok(bytes)
}
