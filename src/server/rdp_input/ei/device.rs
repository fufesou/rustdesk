use super::{
    ffi,
    keymap::{Keymap, Mapping},
};
use enigo::Key;
use hbb_common::{anyhow::anyhow, ResultType};
use std::{collections::HashMap, ptr::NonNull, sync::Arc};

pub(super) struct Device {
    pub api: Arc<ffi::Api>,
    pub pointer: NonNull<ffi::Device>,
    pub context: NonNull<ffi::Ei>,
    pub capabilities: u32,
    pub resumed: bool,
    pub keymap: Option<Keymap>,
    pub pressed: HashMap<Key, Mapping>,
    sequence: u32,
}

impl Device {
    pub fn new(
        api: Arc<ffi::Api>,
        handles: (NonNull<ffi::Ei>, NonNull<ffi::Device>),
    ) -> ResultType<Self> {
        let (context, pointer) = handles;
        unsafe {
            (api.ei_device_ref)(pointer.as_ptr());
        }
        let capabilities = ffi::CAPABILITIES
            .iter()
            .filter(|capability| unsafe {
                (api.ei_device_has_capability)(pointer.as_ptr(), **capability)
            })
            .fold(0, |mask, capability| mask | capability);
        let mut device = Self {
            api,
            pointer,
            context,
            capabilities,
            resumed: false,
            keymap: None,
            pressed: HashMap::new(),
            sequence: 0,
        };
        if capabilities & ffi::KEYBOARD != 0 {
            device.keymap = Some(Keymap::new(&device.api, pointer.as_ptr())?);
        }
        Ok(device)
    }

    pub fn resume(&mut self) {
        self.sequence = self.sequence.wrapping_add(1);
        unsafe {
            (self.api.ei_device_start_emulating)(self.pointer.as_ptr(), self.sequence);
        }
        self.resumed = true;
    }

    pub fn pause(&mut self) {
        self.resumed = false;
        // The EI protocol releases this device's keys when it is paused/removed.
        self.pressed.clear();
    }

    pub fn map(&mut self) -> ResultType<&mut Keymap> {
        self.keymap
            .as_mut()
            .ok_or_else(|| anyhow!("EIS device has no keyboard map"))
    }

    pub fn send_key(&mut self, code: u32, down: bool) {
        unsafe {
            (self.api.ei_device_keyboard_key)(self.pointer.as_ptr(), code, down);
        }
        self.frame();
    }

    pub fn frame(&self) {
        unsafe {
            (self.api.ei_device_frame)(
                self.pointer.as_ptr(),
                (self.api.ei_now)(self.context.as_ptr()),
            );
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            (self.api.ei_device_unref)(self.pointer.as_ptr());
        }
    }
}
