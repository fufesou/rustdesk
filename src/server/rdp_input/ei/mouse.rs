use super::{checked, context::Context, ffi, report, Shared};
use enigo::{MouseButton, MouseControllable};
use hbb_common::{anyhow::anyhow, bail, ResultType};
use scrap::wayland::pipewire::PwStreamInfo;
use std::{
    ffi::CStr,
    sync::{Arc, Mutex},
};

pub(crate) struct Mouse {
    pub(super) context: Shared,
    pub(super) stream: PwStreamInfo,
    pub(super) resolution: Arc<Mutex<Option<(usize, usize)>>>,
}

struct Region {
    device: usize,
    mapping_id: Option<String>,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Mouse {
    fn apply(&self, action: impl FnOnce(&mut Context) -> ResultType<()>) -> ResultType<()> {
        let mut context = self.context.lock().unwrap();
        context.dispatch()?;
        action(&mut context)
    }

    fn move_to(&self, position: (i32, i32)) -> ResultType<()> {
        let resolution = self
            .resolution
            .lock()
            .unwrap()
            .unwrap_or(self.stream.get_size());
        if resolution.0 == 0 || resolution.1 == 0 {
            bail!("Portal stream has no input resolution");
        }
        self.apply(|context| {
            let region = select_region(context, &self.stream)?;
            let origin = self.stream.get_position();
            let x = region.x
                + (f64::from(position.0) - f64::from(origin.0)) * region.width
                    / resolution.0 as f64;
            let y = region.y
                + (f64::from(position.1) - f64::from(origin.1)) * region.height
                    / resolution.1 as f64;
            if x < region.x
                || y < region.y
                || x >= region.x + region.width
                || y >= region.y + region.height
            {
                bail!("Pointer position is outside its EIS capture region");
            }
            let device = &context.devices[region.device];
            unsafe {
                (device.api.ei_device_pointer_motion_absolute)(device.pointer.as_ptr(), x, y);
            }
            device.frame();
            Ok(())
        })
    }

    fn button(&self, button: MouseButton, down: bool) -> ResultType<()> {
        let code = match button {
            MouseButton::Left => evdev::Key::BTN_LEFT,
            MouseButton::Right => evdev::Key::BTN_RIGHT,
            MouseButton::Middle => evdev::Key::BTN_MIDDLE,
            _ => bail!("Unsupported EIS mouse button"),
        };
        self.apply(|context| {
            let device = context.device(ffi::BUTTON)?;
            unsafe {
                (device.api.ei_device_button_button)(
                    device.pointer.as_ptr(),
                    u32::from(code.code()),
                    down,
                );
            }
            device.frame();
            Ok(())
        })
    }

    fn scroll(&self, delta: (i32, i32)) -> ResultType<()> {
        self.apply(|context| {
            let device = context.device(ffi::SCROLL)?;
            unsafe {
                (device.api.ei_device_scroll_delta)(
                    device.pointer.as_ptr(),
                    f64::from(delta.0),
                    f64::from(delta.1),
                );
            }
            device.frame();
            Ok(())
        })
    }
}

impl MouseControllable for Mouse {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_mut_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn mouse_move_to(&mut self, x: i32, y: i32) {
        report(self.move_to((x, y)));
    }
    fn mouse_move_relative(&mut self, x: i32, y: i32) {
        report(self.apply(|context| {
            let device = context.device(ffi::POINTER)?;
            unsafe {
                (device.api.ei_device_pointer_motion)(
                    device.pointer.as_ptr(),
                    f64::from(x),
                    f64::from(y),
                );
            }
            device.frame();
            Ok(())
        }));
    }
    fn mouse_down(&mut self, button: MouseButton) -> enigo::ResultType {
        checked(self.button(button, true)).map_err(Into::into)
    }
    fn mouse_up(&mut self, button: MouseButton) {
        report(self.button(button, false));
    }
    fn mouse_click(&mut self, button: MouseButton) {
        report(self.button(button, true));
        report(self.button(button, false));
    }
    fn mouse_scroll_x(&mut self, length: i32) {
        report(self.scroll((length, 0)));
    }
    fn mouse_scroll_y(&mut self, length: i32) {
        report(self.scroll((0, length)));
    }
}

fn select_region(context: &Context, stream: &PwStreamInfo) -> ResultType<Region> {
    let mut regions = Vec::new();
    for (index, device) in context.devices.iter().enumerate() {
        if !device.resumed || device.capabilities & ffi::ABSOLUTE == 0 {
            continue;
        }
        let mut offset = 0;
        loop {
            let pointer =
                unsafe { (device.api.ei_device_get_region)(device.pointer.as_ptr(), offset) };
            if pointer.is_null() {
                break;
            }
            regions.push(read_region(&device.api, (index, pointer))?);
            offset += 1;
        }
    }
    if let Some(mapping_id) = stream.get_mapping_id() {
        return regions
            .into_iter()
            .rev()
            .find(|region| region.mapping_id.as_deref() == Some(mapping_id))
            .ok_or_else(|| anyhow!("No EIS pointer region matches the Portal capture stream"));
    }
    // Older Portals omit mapping_id. A single region has no mapping ambiguity.
    if regions.len() == 1 {
        return regions
            .pop()
            .ok_or_else(|| anyhow!("EIS region disappeared"));
    }
    bail!("Portal must supply mapping_id to select among multiple EIS pointer regions")
}

fn read_region(api: &ffi::Api, handle: (usize, *mut ffi::Region)) -> ResultType<Region> {
    let (device, pointer) = handle;
    unsafe {
        let id = (api.ei_region_get_mapping_id)(pointer);
        let mapping_id = if id.is_null() {
            None
        } else {
            Some(CStr::from_ptr(id).to_str()?.to_owned())
        };
        Ok(Region {
            device,
            mapping_id,
            x: f64::from((api.ei_region_get_x)(pointer)),
            y: f64::from((api.ei_region_get_y)(pointer)),
            width: f64::from((api.ei_region_get_width)(pointer)),
            height: f64::from((api.ei_region_get_height)(pointer)),
        })
    }
}
