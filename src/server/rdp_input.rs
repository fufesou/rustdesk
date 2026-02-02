use crate::uinput::service::{can_input_via_keysym, char_to_keysym, is_ascii_printable, map_key};
use dbus::{blocking::SyncConnection, Path};
use enigo::{Key, KeyboardControllable, MouseButton, MouseControllable};
use hbb_common::{log, ResultType};
use scrap::wayland::pipewire::{get_portal, PwStreamInfo};
use scrap::wayland::remote_desktop_portal::OrgFreedesktopPortalRemoteDesktop as remote_desktop_portal;
use std::collections::HashMap;
use std::sync::Arc;

pub mod client {
    use hbb_common::platform::linux::is_kde;

    use super::*;

    const EVDEV_MOUSE_LEFT: i32 = 272;
    const EVDEV_MOUSE_RIGHT: i32 = 273;
    const EVDEV_MOUSE_MIDDLE: i32 = 274;

    const PRESSED_DOWN_STATE: u32 = 1;
    const PRESSED_UP_STATE: u32 = 0;

    /// Modifier key state tracking for RDP input.
    /// Portal API doesn't provide a way to query key state, so we track it ourselves.
    #[derive(Default)]
    struct ModifierState {
        shift_left: bool,
        shift_right: bool,
        ctrl_left: bool,
        ctrl_right: bool,
        alt_left: bool,
        alt_right: bool,
        meta_left: bool,
        meta_right: bool,
    }

    impl ModifierState {
        fn update(&mut self, key: &Key, down: bool) {
            match key {
                Key::Shift => self.shift_left = down,
                Key::RightShift => self.shift_right = down,
                Key::Control => self.ctrl_left = down,
                Key::RightControl => self.ctrl_right = down,
                Key::Alt => self.alt_left = down,
                Key::RightAlt => self.alt_right = down,
                Key::Meta | Key::Super | Key::Windows | Key::Command => self.meta_left = down,
                Key::RWin => self.meta_right = down,
                // Handle raw keycodes for modifier keys (Linux evdev codes + 8)
                // In translate mode, modifier keys may be sent as Chr events with raw keycodes
                Key::Raw(code) => {
                    // Linux keycodes (evdev code + 8 for X11 compatibility)
                    // KEY_LEFTSHIFT = 42 + 8 = 50, KEY_RIGHTSHIFT = 54 + 8 = 62
                    // KEY_LEFTCTRL = 29 + 8 = 37, KEY_RIGHTCTRL = 97 + 8 = 105
                    // KEY_LEFTALT = 56 + 8 = 64, KEY_RIGHTALT = 100 + 8 = 108
                    // KEY_LEFTMETA = 125 + 8 = 133, KEY_RIGHTMETA = 126 + 8 = 134
                    match *code {
                        50 => self.shift_left = down,  // KEY_LEFTSHIFT
                        62 => self.shift_right = down, // KEY_RIGHTSHIFT
                        37 => self.ctrl_left = down,   // KEY_LEFTCTRL
                        105 => self.ctrl_right = down, // KEY_RIGHTCTRL
                        64 => self.alt_left = down,    // KEY_LEFTALT
                        108 => self.alt_right = down,  // KEY_RIGHTALT
                        133 => self.meta_left = down,  // KEY_LEFTMETA
                        134 => self.meta_right = down, // KEY_RIGHTMETA
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    pub struct RdpInputKeyboard {
        conn: Arc<SyncConnection>,
        session: Path<'static>,
        modifier_state: ModifierState,
    }

    impl RdpInputKeyboard {
        pub fn new(conn: Arc<SyncConnection>, session: Path<'static>) -> ResultType<Self> {
            Ok(Self {
                conn,
                session,
                modifier_state: ModifierState::default(),
            })
        }
    }

    impl KeyboardControllable for RdpInputKeyboard {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_mut_any(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn get_key_state(&mut self, key: Key) -> bool {
            // Use tracked modifier state for supported keys
            match key {
                Key::Shift => self.modifier_state.shift_left,
                Key::RightShift => self.modifier_state.shift_right,
                Key::Control => self.modifier_state.ctrl_left,
                Key::RightControl => self.modifier_state.ctrl_right,
                Key::Alt => self.modifier_state.alt_left,
                Key::RightAlt => self.modifier_state.alt_right,
                Key::Meta | Key::Super | Key::Windows | Key::Command => {
                    self.modifier_state.meta_left
                }
                Key::RWin => self.modifier_state.meta_right,
                _ => false,
            }
        }

        fn key_sequence(&mut self, s: &str) {
            for c in s.chars() {
                let keysym = char_to_keysym(c);
                // ASCII characters: use keysym
                if can_input_via_keysym(c, keysym) {
                    if let Err(e) = send_keysym(keysym, true, self.conn.clone(), &self.session) {
                        log::error!("Failed to send keysym down: {:?}", e);
                    }
                    if let Err(e) = send_keysym(keysym, false, self.conn.clone(), &self.session) {
                        log::error!("Failed to send keysym up: {:?}", e);
                    }
                } else {
                    // Non-ASCII: use clipboard
                    input_text_via_clipboard(&c.to_string(), self.conn.clone(), &self.session);
                }
            }
        }

        fn key_down(&mut self, key: Key) -> enigo::ResultType {
            // Track modifier key state
            self.modifier_state.update(&key, true);

            if let Key::Layout(chr) = key {
                let keysym = char_to_keysym(chr);
                // ASCII characters: use keysym
                if can_input_via_keysym(chr, keysym) {
                    send_keysym(keysym, true, self.conn.clone(), &self.session)?;
                } else {
                    // Non-ASCII: use clipboard (complete key press in key_down)
                    input_text_via_clipboard(&chr.to_string(), self.conn.clone(), &self.session);
                }
            } else {
                handle_key(true, key, self.conn.clone(), &self.session)?;
            }
            Ok(())
        }

        fn key_up(&mut self, key: Key) {
            // Track modifier key state
            self.modifier_state.update(&key, false);

            if let Key::Layout(chr) = key {
                // ASCII characters: send keysym up
                if is_ascii_printable(chr) {
                    let keysym = char_to_keysym(chr);
                    if keysym != 0 {
                        if let Err(e) = send_keysym(keysym, false, self.conn.clone(), &self.session)
                        {
                            log::error!("Failed to send keysym up: {:?}", e);
                        }
                    }
                }
                // Non-ASCII: already handled in key_down
            } else {
                if let Err(e) = handle_key(false, key, self.conn.clone(), &self.session) {
                    log::error!("Failed to handle key up: {:?}", e);
                }
            }
        }

        fn key_click(&mut self, key: Key) {
            if let Key::Layout(chr) = key {
                let keysym = char_to_keysym(chr);
                // ASCII characters: use keysym
                if can_input_via_keysym(chr, keysym) {
                    if let Err(e) = send_keysym(keysym, true, self.conn.clone(), &self.session) {
                        log::error!("Failed to send keysym down: {:?}", e);
                    }
                    if let Err(e) = send_keysym(keysym, false, self.conn.clone(), &self.session) {
                        log::error!("Failed to send keysym up: {:?}", e);
                    }
                } else {
                    // Non-ASCII: use clipboard
                    input_text_via_clipboard(&chr.to_string(), self.conn.clone(), &self.session);
                }
            } else {
                // Track modifier key state for click (down then up)
                self.modifier_state.update(&key, true);
                if let Err(e) = handle_key(true, key.clone(), self.conn.clone(), &self.session) {
                    log::error!("Failed to handle key down: {:?}", e);
                }
                self.modifier_state.update(&key, false);
                if let Err(e) = handle_key(false, key, self.conn.clone(), &self.session) {
                    log::error!("Failed to handle key up: {:?}", e);
                }
            }
        }
    }

    /// Input text via clipboard + Shift+Insert.
    /// Shift+Insert is more universal than Ctrl+V, works in both GUI apps and terminals.
    ///
    /// Note: Clipboard content is NOT restored after paste - see `set_clipboard_for_paste` for rationale.
    fn input_text_via_clipboard(text: &str, conn: Arc<SyncConnection>, session: &Path<'static>) {
        if !crate::server::uinput::service::set_clipboard_for_paste_sync(text) {
            return;
        }

        let portal = get_portal(&conn);
        let shift_keycode = evdev::Key::KEY_LEFTSHIFT.code() as i32;
        let insert_keycode = evdev::Key::KEY_INSERT.code() as i32;

        // Send Shift+Insert (universal paste shortcut)
        if let Err(e) = remote_desktop_portal::notify_keyboard_keycode(
            &portal,
            session,
            HashMap::new(),
            shift_keycode,
            PRESSED_DOWN_STATE,
        ) {
            log::error!("input_text_via_clipboard: failed to press Shift: {:?}", e);
            return;
        }

        // Press Insert
        if let Err(e) = remote_desktop_portal::notify_keyboard_keycode(
            &portal,
            session,
            HashMap::new(),
            insert_keycode,
            PRESSED_DOWN_STATE,
        ) {
            log::error!("input_text_via_clipboard: failed to press Insert: {:?}", e);
            // Still try to release Shift
            let _ = remote_desktop_portal::notify_keyboard_keycode(
                &portal,
                session,
                HashMap::new(),
                shift_keycode,
                PRESSED_UP_STATE,
            );
            return;
        }

        // Release Insert
        if let Err(e) = remote_desktop_portal::notify_keyboard_keycode(
            &portal,
            session,
            HashMap::new(),
            insert_keycode,
            PRESSED_UP_STATE,
        ) {
            log::error!(
                "input_text_via_clipboard: failed to release Insert: {:?}",
                e
            );
        }

        // Release Shift
        if let Err(e) = remote_desktop_portal::notify_keyboard_keycode(
            &portal,
            session,
            HashMap::new(),
            shift_keycode,
            PRESSED_UP_STATE,
        ) {
            log::error!("input_text_via_clipboard: failed to release Shift: {:?}", e);
        }
    }

    pub struct RdpInputMouse {
        conn: Arc<SyncConnection>,
        session: Path<'static>,
        stream: PwStreamInfo,
        resolution: (usize, usize),
        scale: Option<f64>,
        position: (f64, f64),
    }

    impl RdpInputMouse {
        pub fn new(
            conn: Arc<SyncConnection>,
            session: Path<'static>,
            stream: PwStreamInfo,
            resolution: (usize, usize),
        ) -> ResultType<Self> {
            // https://github.com/rustdesk/rustdesk/pull/9019#issuecomment-2295252388
            // There may be a bug in Rdp input on Gnome util Ubuntu 24.04 (Gnome 46)
            //
            // eg. Resolution 800x600, Fractional scale: 200% (logic size: 400x300)
            // https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.impl.portal.RemoteDesktop.html#:~:text=new%20pointer%20position-,in%20the%20streams%20logical%20coordinate%20space,-.
            // Then (x,y) in `mouse_move_to()` and `mouse_move_relative()` should be scaled to the logic size(stream.get_size()), which is from (0,0) to (400,300).
            // For Ubuntu 24.04(Gnome 46), (x,y) is restricted from (0,0) to (400,300), but the actual range in screen is:
            // Logic coordinate from (0,0) to (200x150).
            // Or physical coordinate from (0,0) to (400,300).
            let scale = if is_kde() {
                if resolution.0 == 0 || stream.get_size().0 == 0 {
                    Some(1.0f64)
                } else {
                    Some(resolution.0 as f64 / stream.get_size().0 as f64)
                }
            } else {
                None
            };
            let pos = stream.get_position();
            Ok(Self {
                conn,
                session,
                stream,
                resolution,
                scale,
                position: (pos.0 as f64, pos.1 as f64),
            })
        }
    }

    impl MouseControllable for RdpInputMouse {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_mut_any(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn mouse_move_to(&mut self, x: i32, y: i32) {
            let x = if let Some(s) = self.scale {
                x as f64 / s
            } else {
                x as f64
            };
            let y = if let Some(s) = self.scale {
                y as f64 / s
            } else {
                y as f64
            };
            let x = x - self.position.0;
            let y = y - self.position.1;
            let portal = get_portal(&self.conn);
            let _ = remote_desktop_portal::notify_pointer_motion_absolute(
                &portal,
                &self.session,
                HashMap::new(),
                self.stream.path as u32,
                x,
                y,
            );
        }
        fn mouse_move_relative(&mut self, x: i32, y: i32) {
            let x = if let Some(s) = self.scale {
                x as f64 / s
            } else {
                x as f64
            };
            let y = if let Some(s) = self.scale {
                y as f64 / s
            } else {
                y as f64
            };
            let portal = get_portal(&self.conn);
            let _ = remote_desktop_portal::notify_pointer_motion(
                &portal,
                &self.session,
                HashMap::new(),
                x,
                y,
            );
        }
        fn mouse_down(&mut self, button: MouseButton) -> enigo::ResultType {
            handle_mouse(true, button, self.conn.clone(), &self.session);
            Ok(())
        }
        fn mouse_up(&mut self, button: MouseButton) {
            handle_mouse(false, button, self.conn.clone(), &self.session);
        }
        fn mouse_click(&mut self, button: MouseButton) {
            handle_mouse(true, button, self.conn.clone(), &self.session);
            handle_mouse(false, button, self.conn.clone(), &self.session);
        }
        fn mouse_scroll_x(&mut self, length: i32) {
            let portal = get_portal(&self.conn);
            let _ = remote_desktop_portal::notify_pointer_axis(
                &portal,
                &self.session,
                HashMap::new(),
                length as f64,
                0 as f64,
            );
        }
        fn mouse_scroll_y(&mut self, length: i32) {
            let portal = get_portal(&self.conn);
            let _ = remote_desktop_portal::notify_pointer_axis(
                &portal,
                &self.session,
                HashMap::new(),
                0 as f64,
                length as f64,
            );
        }
    }

    /// Send a keysym via RemoteDesktop portal.
    fn send_keysym(
        keysym: i32,
        down: bool,
        conn: Arc<SyncConnection>,
        session: &Path<'static>,
    ) -> ResultType<()> {
        let state: u32 = if down {
            PRESSED_DOWN_STATE
        } else {
            PRESSED_UP_STATE
        };
        let portal = get_portal(&conn);
        log::trace!(
            "send_keysym: calling notify_keyboard_keysym, state={}",
            state
        );
        match remote_desktop_portal::notify_keyboard_keysym(
            &portal,
            session,
            HashMap::new(),
            keysym,
            state,
        ) {
            Ok(_) => {
                log::trace!("send_keysym: notify_keyboard_keysym succeeded");
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    fn get_raw_evdev_keycode(key: u16) -> i32 {
        // 8 is the offset between xkb and evdev
        let mut key = key as i32 - 8;
        // fix for right_meta key
        if key == 126 {
            key = 125;
        }
        key
    }

    fn handle_key(
        down: bool,
        key: Key,
        conn: Arc<SyncConnection>,
        session: &Path<'static>,
    ) -> ResultType<()> {
        let state: u32 = if down {
            PRESSED_DOWN_STATE
        } else {
            PRESSED_UP_STATE
        };
        let portal = get_portal(&conn);
        match key {
            Key::Raw(key) => {
                let key = get_raw_evdev_keycode(key);
                remote_desktop_portal::notify_keyboard_keycode(
                    &portal,
                    &session,
                    HashMap::new(),
                    key,
                    state,
                )?;
            }
            _ => {
                if let Ok((key, is_shift)) = map_key(&key) {
                    let shift_keycode = evdev::Key::KEY_LEFTSHIFT.code() as i32;
                    if down {
                        // Press: Shift down first, then key down
                        if is_shift {
                            if let Err(e) = remote_desktop_portal::notify_keyboard_keycode(
                                &portal,
                                &session,
                                HashMap::new(),
                                shift_keycode,
                                state,
                            ) {
                                log::error!("handle_key: failed to press Shift: {:?}", e);
                                return Err(e.into());
                            }
                        }
                        if let Err(e) = remote_desktop_portal::notify_keyboard_keycode(
                            &portal,
                            &session,
                            HashMap::new(),
                            key.code() as i32,
                            state,
                        ) {
                            log::error!("handle_key: failed to press key: {:?}", e);
                            // Best-effort: release Shift if it was pressed
                            if is_shift {
                                let _ = remote_desktop_portal::notify_keyboard_keycode(
                                    &portal,
                                    &session,
                                    HashMap::new(),
                                    shift_keycode,
                                    PRESSED_UP_STATE,
                                );
                            }
                            return Err(e.into());
                        }
                    } else {
                        // Release: key up first, then Shift up
                        if let Err(e) = remote_desktop_portal::notify_keyboard_keycode(
                            &portal,
                            &session,
                            HashMap::new(),
                            key.code() as i32,
                            state,
                        ) {
                            log::error!("handle_key: failed to release key: {:?}", e);
                            // Best-effort: still try to release Shift
                            if is_shift {
                                let _ = remote_desktop_portal::notify_keyboard_keycode(
                                    &portal,
                                    &session,
                                    HashMap::new(),
                                    shift_keycode,
                                    state,
                                );
                            }
                            return Err(e.into());
                        }
                        if is_shift {
                            if let Err(e) = remote_desktop_portal::notify_keyboard_keycode(
                                &portal,
                                &session,
                                HashMap::new(),
                                shift_keycode,
                                state,
                            ) {
                                log::error!("handle_key: failed to release Shift: {:?}", e);
                                return Err(e.into());
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_mouse(
        down: bool,
        button: MouseButton,
        conn: Arc<SyncConnection>,
        session: &Path<'static>,
    ) {
        let portal = get_portal(&conn);
        let but_key = match button {
            MouseButton::Left => EVDEV_MOUSE_LEFT,
            MouseButton::Right => EVDEV_MOUSE_RIGHT,
            MouseButton::Middle => EVDEV_MOUSE_MIDDLE,
            _ => {
                return;
            }
        };
        let state: u32 = if down {
            PRESSED_DOWN_STATE
        } else {
            PRESSED_UP_STATE
        };
        let _ = remote_desktop_portal::notify_pointer_button(
            &portal,
            &session,
            HashMap::new(),
            but_key,
            state,
        );
    }
}
