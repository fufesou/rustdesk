use super::{
    checked,
    device::Device,
    ffi,
    keymap::{Mapping, XKB_OFFSET},
    report, Shared,
};
use crate::{
    server::input_service::set_clipboard_for_paste_sync,
    uinput::service::{can_input_via_keysym, char_to_keysym, map_key},
};
use enigo::{Key, KeyboardControllable};
use hbb_common::{bail, ResultType};

pub(crate) struct Keyboard {
    pub(super) context: Shared,
}

impl Keyboard {
    fn apply<T>(&self, action: impl FnOnce(&mut Device) -> ResultType<T>) -> ResultType<T> {
        action(self.context.lock().unwrap().device(ffi::KEYBOARD)?)
    }

    fn text(&self, text: &str) -> ResultType<()> {
        if text.is_empty() {
            return Ok(());
        }
        if !text.chars().all(printable) {
            return self.paste(text);
        }
        self.apply(|device| {
            for character in text.chars() {
                device.press(Key::Layout(character))?;
                device.release(Key::Layout(character))?;
            }
            Ok(())
        })
    }

    fn paste(&self, text: &str) -> ResultType<()> {
        if !set_clipboard_for_paste_sync(text) {
            bail!("Cannot prepare clipboard for EIS text input");
        }
        self.apply(|device| {
            let shift = u32::from(evdev::Key::KEY_LEFTSHIFT.code());
            let right_shift = u32::from(evdev::Key::KEY_RIGHTSHIFT.code());
            let owned_shift = !device.is_down(shift) && !device.is_down(right_shift);
            if owned_shift {
                device.press(Key::Shift)?;
            }
            let result = device.press(Key::Insert);
            let release = device.release(Key::Insert);
            if owned_shift {
                device.release(Key::Shift)?;
            }
            result.and(release)
        })
    }

    fn down(&self, key: Key) -> ResultType<()> {
        if let Key::Layout(character) = key {
            if !printable(character) {
                return self.paste(&character.to_string());
            }
        }
        self.apply(|device| device.press(key))
    }

    fn state(&self, key: Key) -> ResultType<bool> {
        self.apply(|device| match key {
            Key::CapsLock => device.map()?.locked(b"Caps Lock\0"),
            Key::NumLock => device.map()?.locked(b"Num Lock\0"),
            Key::Layout(_) => Ok(device.pressed.contains_key(&key)),
            key => Ok(device.is_down(control_mapping(key)?.code)),
        })
    }
}

impl KeyboardControllable for Keyboard {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_mut_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn get_key_state(&mut self, key: Key) -> bool {
        match self.state(key) {
            Ok(state) => state,
            Err(error) => {
                report(Err(error));
                false
            }
        }
    }
    fn key_sequence(&mut self, text: &str) {
        report(self.text(text));
    }
    fn key_down(&mut self, key: Key) -> enigo::ResultType {
        checked(self.down(key)).map_err(Into::into)
    }
    fn key_up(&mut self, key: Key) {
        report(self.apply(|device| device.release(key)));
    }
    fn key_click(&mut self, key: Key) {
        report(self.down(key));
        report(self.apply(|device| device.release(key)));
    }
}

impl Device {
    fn is_down(&self, code: u32) -> bool {
        self.pressed
            .values()
            .any(|mapping| mapping.code == code || mapping.modifiers.contains(&code))
    }

    fn shortcut(&self) -> bool {
        [
            evdev::Key::KEY_LEFTCTRL,
            evdev::Key::KEY_RIGHTCTRL,
            evdev::Key::KEY_LEFTALT,
            evdev::Key::KEY_LEFTMETA,
            evdev::Key::KEY_RIGHTMETA,
        ]
        .iter()
        .any(|key| self.is_down(u32::from(key.code())))
    }

    fn press(&mut self, key: Key) -> ResultType<()> {
        if self.pressed.contains_key(&key) {
            return Ok(());
        }
        let mapping = match key {
            Key::Layout(character) => {
                let shortcut = self.shortcut();
                self.map()?.resolve(character, shortcut)?
            }
            key => control_mapping(key)?,
        };
        for modifier in &mapping.modifiers {
            if !self.is_down(*modifier) {
                self.send_key(*modifier, true);
            }
        }
        self.send_key(mapping.code, true);
        self.pressed.insert(key, mapping);
        Ok(())
    }

    fn release(&mut self, key: Key) -> ResultType<()> {
        let key = self.release_key(key)?;
        let Some(mapping) = key.and_then(|key| self.pressed.remove(&key)) else {
            return Ok(());
        };
        if !self.is_down(mapping.code) {
            self.send_key(mapping.code, false);
        }
        for modifier in mapping.modifiers.into_iter().rev() {
            if !self.is_down(modifier) {
                self.send_key(modifier, false);
            }
        }
        Ok(())
    }

    fn release_key(&self, key: Key) -> ResultType<Option<Key>> {
        if self.pressed.contains_key(&key) {
            return Ok(Some(key));
        }
        if let Key::Layout(character) = key {
            return Ok(self
                .pressed
                .keys()
                .find(|key| {
                    matches!(key,
                Key::Layout(pressed) if pressed.eq_ignore_ascii_case(&character))
                })
                .copied());
        }
        let code = control_mapping(key)?.code;
        Ok(self
            .pressed
            .iter()
            .find(|(key, mapping)| !matches!(key, Key::Layout(_)) && mapping.code == code)
            .map(|(key, _)| *key))
    }
}

fn printable(character: char) -> bool {
    can_input_via_keysym(character, char_to_keysym(character))
}

fn control_mapping(key: Key) -> ResultType<Mapping> {
    if let Key::Raw(code) = key {
        let Some(code) = u32::from(code).checked_sub(XKB_OFFSET) else {
            bail!("Invalid raw XKB keycode");
        };
        return Ok(Mapping {
            code,
            modifiers: Vec::new(),
        });
    }
    let (code, shift) = map_key(&key)?;
    let modifiers = if shift {
        vec![u32::from(evdev::Key::KEY_LEFTSHIFT.code())]
    } else {
        Vec::new()
    };
    Ok(Mapping {
        code: u32::from(code.code()),
        modifiers,
    })
}
