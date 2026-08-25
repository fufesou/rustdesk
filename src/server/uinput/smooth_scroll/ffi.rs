use evdev::{EventType, InputEvent, Synchronization};
use hbb_common::{libc, log};
use std::{
    fs::File,
    io::{self, Write},
    mem::{size_of, size_of_val},
    os::unix::{fs::OpenOptionsExt, io::AsRawFd},
    path::PathBuf,
    slice,
};

const UINPUT_PATH: &str = "/dev/uinput";
const UINPUT_IOCTL_BASE: u8 = b'U';
const UI_DEV_CREATE: libc::Ioctl = ioctl_none(1);
const UI_DEV_DESTROY: libc::Ioctl = ioctl_none(2);
const UI_DEV_SETUP: libc::Ioctl = ioctl_write(3, size_of::<libc::uinput_setup>());
const UI_ABS_SETUP: libc::Ioctl = ioctl_write(4, size_of::<libc::uinput_abs_setup>());
const UI_GET_SYSNAME_NUMBER: u8 = 44;
const UI_SET_EVBIT: libc::Ioctl = ioctl_write(100, size_of::<libc::c_int>());
const UI_SET_KEYBIT: libc::Ioctl = ioctl_write(101, size_of::<libc::c_int>());
const UI_SET_ABSBIT: libc::Ioctl = ioctl_write(103, size_of::<libc::c_int>());
const UI_SET_PROPBIT: libc::Ioctl = ioctl_write(110, size_of::<libc::c_int>());
#[cfg(target_arch = "sparc")]
const SPARC_IOCTL_NONE: u32 = 1;
#[cfg(target_arch = "sparc")]
const SPARC_IOCTL_READ: u32 = 2;
#[cfg(target_arch = "sparc")]
const SPARC_IOCTL_WRITE: u32 = 4;
const SYSNAME_BUFFER_SIZE: usize = 64;

const fn ioctl_none(number: u8) -> libc::Ioctl {
    #[cfg(target_arch = "sparc")]
    {
        sparc_ioctl_request(SPARC_IOCTL_NONE, number, 0)
    }
    #[cfg(not(target_arch = "sparc"))]
    {
        nix::request_code_none!(UINPUT_IOCTL_BASE, number)
    }
}

const fn ioctl_write(number: u8, size: usize) -> libc::Ioctl {
    #[cfg(target_arch = "sparc")]
    {
        sparc_ioctl_request(SPARC_IOCTL_WRITE, number, size)
    }
    #[cfg(not(target_arch = "sparc"))]
    {
        nix::request_code_write!(UINPUT_IOCTL_BASE, number, size)
    }
}

const fn ioctl_read(number: u8, size: usize) -> libc::Ioctl {
    #[cfg(target_arch = "sparc")]
    {
        sparc_ioctl_request(SPARC_IOCTL_READ, number, size)
    }
    #[cfg(not(target_arch = "sparc"))]
    {
        nix::request_code_read!(UINPUT_IOCTL_BASE, number, size)
    }
}

#[cfg(target_arch = "sparc")]
const fn sparc_ioctl_request(direction: u32, number: u8, size: usize) -> libc::Ioctl {
    const TYPE_SHIFT: u32 = 8;
    const SIZE_SHIFT: u32 = 16;
    const DIRECTION_SHIFT: u32 = 29;
    ((direction << DIRECTION_SHIFT)
        | ((UINPUT_IOCTL_BASE as u32) << TYPE_SHIFT)
        | number as u32
        | ((size as u32) << SIZE_SHIFT)) as libc::Ioctl
}

#[derive(Clone, Copy)]
pub(super) struct AxisSetup {
    pub code: u16,
    pub minimum: i32,
    pub maximum: i32,
    pub resolution: i32,
}

#[derive(Clone, Copy)]
pub(super) struct DeviceId {
    pub bustype: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

pub(super) struct Device {
    file: File,
    created: bool,
    frame: Vec<InputEvent>,
    sysname: Option<String>,
}

impl Device {
    pub(super) fn open() -> io::Result<Self> {
        let file = File::options()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(UINPUT_PATH)?;
        Ok(Self {
            file,
            created: false,
            frame: Vec::new(),
            sysname: None,
        })
    }

    pub(super) fn enable_event(&self, event: EventType) -> io::Result<()> {
        ioctl_with_value(&self.file, UI_SET_EVBIT, event.0)
    }

    pub(super) fn enable_key(&self, code: u16) -> io::Result<()> {
        ioctl_with_value(&self.file, UI_SET_KEYBIT, code)
    }

    pub(super) fn enable_property(&self, code: u16) -> io::Result<()> {
        ioctl_with_value(&self.file, UI_SET_PROPBIT, code)
    }

    pub(super) fn configure_axis(&self, config: AxisSetup) -> io::Result<()> {
        ioctl_with_value(&self.file, UI_SET_ABSBIT, config.code)?;
        let setup = libc::uinput_abs_setup {
            code: config.code,
            absinfo: libc::input_absinfo {
                value: config.minimum,
                minimum: config.minimum,
                maximum: config.maximum,
                fuzz: 0,
                flat: 0,
                resolution: config.resolution,
            },
        };
        ioctl_with_reference(&self.file, UI_ABS_SETUP, &setup)
    }

    pub(super) fn create(&mut self, name: &[u8], id: DeviceId) -> io::Result<()> {
        if name.len() >= libc::UINPUT_MAX_NAME_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "uinput device name is too long",
            ));
        }
        let mut setup = libc::uinput_setup {
            id: libc::input_id {
                bustype: id.bustype,
                vendor: id.vendor,
                product: id.product,
                version: id.version,
            },
            name: [0; libc::UINPUT_MAX_NAME_SIZE],
            ff_effects_max: 0,
        };
        for (target, source) in setup.name.iter_mut().zip(name.iter().copied()) {
            *target = source as libc::c_char;
        }
        ioctl_with_reference(&self.file, UI_DEV_SETUP, &setup)?;
        ioctl_without_value(&self.file, UI_DEV_CREATE)?;
        self.created = true;
        self.sysname = Some(get_sysname(&self.file)?);
        Ok(())
    }

    pub(super) fn is_classified_as_touchpad(&self) -> io::Result<bool> {
        let sysname = self.sysname.as_deref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "uinput device is not created")
        })?;
        let Some(event_name) = find_event_name(sysname)? else {
            return Ok(false);
        };
        let device_number_path = PathBuf::from("/sys/class/input")
            .join(&event_name)
            .join("dev");
        let Some(device_number) = read_optional_text(device_number_path)? else {
            return Ok(false);
        };
        let udev_path = PathBuf::from("/run/udev/data").join(format!("c{}", device_number.trim()));
        let Some(udev_data) = read_optional_text(udev_path)? else {
            return Ok(false);
        };
        Ok(udev_data_is_touchpad(&udev_data))
    }

    pub(super) fn emit(&mut self, events: &[InputEvent]) -> io::Result<()> {
        self.frame.clear();
        self.frame.extend_from_slice(events);
        self.frame.push(InputEvent::new(
            EventType::SYNCHRONIZATION,
            Synchronization::SYN_REPORT.0,
            0,
        ));
        // SAFETY: InputEvent is transparent over initialized Linux input_event values.
        let bytes = unsafe {
            slice::from_raw_parts(
                self.frame.as_ptr().cast::<u8>(),
                size_of_val(self.frame.as_slice()),
            )
        };
        self.file.write_all(bytes)
    }
}

fn get_sysname(file: &File) -> io::Result<String> {
    let mut buffer = [0_u8; SYSNAME_BUFFER_SIZE];
    let request = ioctl_read(UI_GET_SYSNAME_NUMBER, buffer.len());
    let result = unsafe { libc::ioctl(file.as_raw_fd(), request, buffer.as_mut_ptr()) };
    ioctl_result(result)?;
    let length = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    let sysname = std::str::from_utf8(&buffer[..length])
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let suffix = sysname.strip_prefix("input").unwrap_or_default();
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected uinput sysname: {sysname}"),
        ));
    }
    Ok(sysname.to_owned())
}

fn find_event_name(sysname: &str) -> io::Result<Option<String>> {
    let input_path = PathBuf::from("/sys/devices/virtual/input").join(sysname);
    let entries = match std::fs::read_dir(input_path) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    for entry in entries {
        let name = entry?.file_name().to_string_lossy().into_owned();
        let suffix = name.strip_prefix("event").unwrap_or_default();
        if !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()) {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

fn read_optional_text(path: PathBuf) -> io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(value) => Ok(Some(value)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn udev_data_is_touchpad(data: &str) -> bool {
    data.lines().any(|line| line == "E:ID_INPUT_TOUCHPAD=1")
}

impl Drop for Device {
    fn drop(&mut self) {
        if self.created {
            if let Err(err) = ioctl_without_value(&self.file, UI_DEV_DESTROY) {
                log::error!("Failed to destroy smooth uinput device: {err}");
            }
        }
    }
}

fn ioctl_with_value(file: &File, request: libc::Ioctl, value: u16) -> io::Result<()> {
    let result = unsafe { libc::ioctl(file.as_raw_fd(), request, libc::c_ulong::from(value)) };
    ioctl_result(result)
}

fn ioctl_with_reference<T>(file: &File, request: libc::Ioctl, value: &T) -> io::Result<()> {
    let result = unsafe { libc::ioctl(file.as_raw_fd(), request, value) };
    ioctl_result(result)
}

fn ioctl_without_value(file: &File, request: libc::Ioctl) -> io::Result<()> {
    let result = unsafe { libc::ioctl(file.as_raw_fd(), request) };
    ioctl_result(result)
}

fn ioctl_result(result: libc::c_int) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::udev_data_is_touchpad;

    #[test]
    fn requires_explicit_udev_touchpad_classification() {
        assert!(!udev_data_is_touchpad("E:ID_INPUT_MOUSE=1\n"));
        assert!(udev_data_is_touchpad(
            "E:ID_INPUT=1\nE:ID_INPUT_TOUCHPAD=1\n"
        ));
    }
}
