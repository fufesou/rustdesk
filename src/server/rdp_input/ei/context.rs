use super::{device::Device, ffi};
use dbus::arg::OwnedFd;
use hbb_common::{anyhow::anyhow, bail, libc, ResultType};
use std::{
    io,
    ptr::NonNull,
    sync::Arc,
    time::{Duration, Instant},
};

const SETUP_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) struct Context {
    pub api: Arc<ffi::Api>,
    pointer: NonNull<ffi::Ei>,
    pub devices: Vec<Device>,
    failure: Option<String>,
}

// Every operation, including dispatch and destruction, is serialized by the shared Mutex.
unsafe impl Send for Context {}

impl Context {
    pub fn new(api: Arc<ffi::Api>, fd: OwnedFd) -> ResultType<Self> {
        let pointer = NonNull::new(unsafe { (api.ei_new_sender)(std::ptr::null_mut()) })
            .ok_or_else(|| anyhow!("Cannot create libei sender"))?;
        let mut context = Self {
            api,
            pointer,
            devices: Vec::new(),
            failure: None,
        };
        unsafe {
            (context.api.ei_configure_name)(pointer.as_ptr(), b"RustDesk\0".as_ptr().cast());
            let result = (context.api.ei_setup_backend_fd)(pointer.as_ptr(), fd.into_fd());
            if result < 0 {
                return Err(io::Error::from_raw_os_error(-result).into());
            }
        }
        context.wait_ready()?;
        Ok(context)
    }

    fn wait_ready(&mut self) -> ResultType<()> {
        let deadline = Instant::now() + SETUP_TIMEOUT;
        loop {
            self.dispatch()?;
            let keyboard = self.devices.iter().any(|device| {
                device.resumed && device.keymap.as_ref().is_some_and(|map| map.ready)
            });
            let pointer = self
                .devices
                .iter()
                .any(|device| device.resumed && device.capabilities & ffi::ABSOLUTE != 0);
            if keyboard && pointer {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!("Portal EIS did not provide a ready pointer and keyboard layout/modifier state; disable libei and restart RustDesk to use D-Bus input");
            }
            self.wait(deadline)?;
        }
    }

    fn wait(&self, deadline: Instant) -> ResultType<()> {
        let mut fd = libc::pollfd {
            fd: unsafe { (self.api.ei_get_fd)(self.pointer.as_ptr()) },
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout = i32::try_from(
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis(),
        )?;
        let ready = unsafe { libc::poll(&mut fd, 1, timeout) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error.into());
            }
        }
        Ok(())
    }

    pub fn dispatch(&mut self) -> ResultType<()> {
        if let Some(error) = &self.failure {
            bail!("{error}");
        }
        if let Err(error) = self.read_events() {
            self.failure = Some(error.to_string());
            return Err(error);
        }
        Ok(())
    }

    fn read_events(&mut self) -> ResultType<()> {
        unsafe {
            (self.api.ei_dispatch)(self.pointer.as_ptr());
        }
        while let Some(event) =
            NonNull::new(unsafe { (self.api.ei_get_event)(self.pointer.as_ptr()) })
        {
            let result = self.event(event.as_ptr());
            unsafe {
                (self.api.ei_event_unref)(event.as_ptr());
            }
            result?;
        }
        Ok(())
    }

    fn event(&mut self, event: *mut ffi::Event) -> ResultType<()> {
        let kind = unsafe { (self.api.ei_event_get_type)(event) };
        if kind == ffi::DISCONNECT {
            bail!("Portal EIS connection closed");
        }
        if kind == ffi::SEAT_ADDED {
            let seat = unsafe { (self.api.ei_event_get_seat)(event) };
            if seat.is_null() {
                bail!("EIS seat event has no seat");
            }
            unsafe {
                (self.api.ei_seat_bind_capabilities)(
                    seat,
                    ffi::POINTER,
                    ffi::ABSOLUTE,
                    ffi::KEYBOARD,
                    ffi::SCROLL,
                    ffi::BUTTON,
                    0u32,
                );
            }
            return Ok(());
        }
        let Some(pointer) = NonNull::new(unsafe { (self.api.ei_event_get_device)(event) }) else {
            return Ok(());
        };
        if kind == ffi::DEVICE_ADDED {
            self.devices
                .push(Device::new(self.api.clone(), (self.pointer, pointer))?);
            return Ok(());
        }
        if kind == ffi::DEVICE_REMOVED {
            self.devices.retain(|device| device.pointer != pointer);
            return Ok(());
        }
        let Some(device) = self
            .devices
            .iter_mut()
            .find(|device| device.pointer == pointer)
        else {
            return Ok(());
        };
        match kind {
            ffi::DEVICE_RESUMED => device.resume(),
            ffi::DEVICE_PAUSED => device.pause(),
            ffi::KEYBOARD_MODIFIERS => device.map()?.update(&self.api, event)?,
            _ => (),
        }
        Ok(())
    }

    pub fn device(&mut self, capability: u32) -> ResultType<&mut Device> {
        self.dispatch()?;
        self.devices
            .iter_mut()
            .rev()
            .find(|device| device.resumed && device.capabilities & capability != 0)
            .ok_or_else(|| anyhow!("Requested EIS input device is unavailable or paused"))
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        self.devices.clear();
        unsafe {
            (self.api.ei_unref)(self.pointer.as_ptr());
        }
    }
}
