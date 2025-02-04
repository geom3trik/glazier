// Copyright 2021 The Druid Authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! A minimal wrapper around Xkb for our use.

mod keycodes;
mod xkbcommon_sys;
use crate::{
    backend::shared::{code_to_location, hardware_keycode_to_code},
    KeyEvent, KeyState, Modifiers,
};
use keyboard_types::{Code, Key};
use std::convert::TryFrom;
use std::os::raw::c_char;
use xkbcommon_sys::*;

#[cfg(feature = "x11")]
use x11rb::xcb_ffi::XCBConnection;

#[cfg(feature = "x11")]
pub struct DeviceId(pub std::os::raw::c_int);

/// A global xkb context object.
///
/// Reference counted under the hood.
// Assume this isn't threadsafe unless proved otherwise. (e.g. don't implement Send/Sync)
pub struct Context(*mut xkb_context);

impl Context {
    /// Create a new xkb context.
    ///
    /// The returned object is lightweight and clones will point at the same context internally.
    pub fn new() -> Self {
        unsafe { Self(xkb_context_new(XKB_CONTEXT_NO_FLAGS)) }
    }

    #[cfg(feature = "x11")]
    pub fn core_keyboard_device_id(&self, conn: &XCBConnection) -> Option<DeviceId> {
        let id = unsafe {
            xkb_x11_get_core_keyboard_device_id(
                conn.get_raw_xcb_connection() as *mut xcb_connection_t
            )
        };
        if id != -1 {
            Some(DeviceId(id))
        } else {
            None
        }
    }

    #[cfg(feature = "x11")]
    pub fn keymap_from_device(&self, conn: &XCBConnection, device: &DeviceId) -> Option<Keymap> {
        let key_map = unsafe {
            xkb_x11_keymap_new_from_device(
                self.0,
                conn.get_raw_xcb_connection() as *mut xcb_connection_t,
                device.0,
                XKB_KEYMAP_COMPILE_NO_FLAGS,
            )
        };
        if key_map.is_null() {
            return None;
        }
        Some(Keymap(key_map))
    }

    #[cfg(feature = "x11")]
    pub fn state_from_x11_keymap(
        &self,
        keymap: &Keymap,
        conn: &XCBConnection,
        device: &DeviceId,
    ) -> Option<State> {
        let state = unsafe {
            xkb_x11_state_new_from_device(
                keymap.0,
                conn.get_raw_xcb_connection() as *mut xcb_connection_t,
                device.0,
            )
        };
        if state.is_null() {
            return None;
        }
        Some(State::new(keymap, state))
    }

    #[cfg(feature = "wayland")]
    pub fn state_from_keymap(&self, keymap: &Keymap) -> Option<State> {
        let state = unsafe { xkb_state_new(keymap.0) };
        if state.is_null() {
            return None;
        }
        Some(State::new(keymap, state))
    }
    /// Create a keymap from some given data.
    ///
    /// Uses `xkb_keymap_new_from_buffer` under the hood.
    #[cfg(feature = "wayland")]
    pub fn keymap_from_slice(&self, buffer: &[u8]) -> Keymap {
        // TODO we hope that the keymap doesn't borrow the underlying data. If it does' we need to
        // use Rc. We'll find out soon enough if we get a segfault.
        // TODO we hope that the keymap inc's the reference count of the context.
        assert!(
            buffer.iter().copied().any(|byte| byte == 0),
            "`keymap_from_slice` expects a null-terminated string"
        );
        unsafe {
            let keymap = xkb_keymap_new_from_string(
                self.0,
                buffer.as_ptr() as *const i8,
                XKB_KEYMAP_FORMAT_TEXT_V1,
                XKB_KEYMAP_COMPILE_NO_FLAGS,
            );
            assert!(!keymap.is_null());
            Keymap(keymap)
        }
    }

    /// Set the log level using `tracing` levels.
    ///
    /// Because `xkb` has a `critical` error, each rust error maps to 1 above (e.g. error ->
    /// critical, warn -> error etc.)
    #[allow(unused)]
    pub fn set_log_level(&self, level: tracing::Level) {
        use tracing::Level;
        let level = match level {
            Level::ERROR => XKB_LOG_LEVEL_CRITICAL,
            Level::WARN => XKB_LOG_LEVEL_ERROR,
            Level::INFO => XKB_LOG_LEVEL_WARNING,
            Level::DEBUG => XKB_LOG_LEVEL_INFO,
            Level::TRACE => XKB_LOG_LEVEL_DEBUG,
        };
        unsafe {
            xkb_context_set_log_level(self.0, level);
        }
    }
}

impl Clone for Context {
    fn clone(&self) -> Self {
        Self(unsafe { xkb_context_ref(self.0) })
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe {
            xkb_context_unref(self.0);
        }
    }
}

pub struct Keymap(*mut xkb_keymap);

impl Keymap {
    #[cfg(feature = "wayland")]
    pub fn repeats(&mut self, key: u32) -> bool {
        unsafe { xkb_keymap_key_repeats(self.0, key) == 1 }
    }
}

impl Clone for Keymap {
    fn clone(&self) -> Self {
        Self(unsafe { xkb_keymap_ref(self.0) })
    }
}

impl Drop for Keymap {
    fn drop(&mut self) {
        unsafe {
            xkb_keymap_unref(self.0);
        }
    }
}

pub struct State {
    state: *mut xkb_state,
    mods: ModsIndices,
}

#[derive(Clone, Copy, Debug)]
pub struct ModsIndices {
    control: xkb_mod_index_t,
    shift: xkb_mod_index_t,
    alt: xkb_mod_index_t,
    super_: xkb_mod_index_t,
    caps_lock: xkb_mod_index_t,
    num_lock: xkb_mod_index_t,
}

#[derive(Clone, Copy)]
pub struct ActiveModifiers {
    pub base_mods: xkb_mod_mask_t,
    pub latched_mods: xkb_mod_mask_t,
    pub locked_mods: xkb_mod_mask_t,
    pub base_layout: xkb_layout_index_t,
    pub latched_layout: xkb_layout_index_t,
    pub locked_layout: xkb_layout_index_t,
}

impl State {
    pub fn new(keymap: &Keymap, state: *mut xkb_state) -> Self {
        let keymap = keymap.0;
        let mod_idx = |str: &'static [u8]| unsafe {
            xkb_keymap_mod_get_index(keymap, str.as_ptr() as *mut c_char)
        };
        Self {
            state,
            mods: ModsIndices {
                control: mod_idx(XKB_MOD_NAME_CTRL),
                shift: mod_idx(XKB_MOD_NAME_SHIFT),
                alt: mod_idx(XKB_MOD_NAME_ALT),
                super_: mod_idx(XKB_MOD_NAME_LOGO),
                caps_lock: mod_idx(XKB_MOD_NAME_CAPS),
                num_lock: mod_idx(XKB_MOD_NAME_NUM),
            },
        }
    }

    pub fn update_xkb_state(&mut self, mods: ActiveModifiers) {
        unsafe {
            xkb_state_update_mask(
                self.state,
                mods.base_mods,
                mods.latched_mods,
                mods.locked_mods,
                mods.base_layout,
                mods.latched_layout,
                mods.locked_layout,
            )
        };
    }

    pub fn key_event(&mut self, scancode: u32, state: KeyState, repeat: bool) -> KeyEvent {
        let code = u16::try_from(scancode)
            .map(hardware_keycode_to_code)
            .unwrap_or(Code::Unidentified);
        let key = self.get_logical_key(scancode);
        // TODO this is lazy - really should use xkb i.e. augment the get_logical_key method.
        let location = code_to_location(code);

        // TODO not sure how to get this
        let is_composing = false;

        let mut mods = Modifiers::empty();
        // Update xkb's state (e.g. return capitals if we've pressed shift)
        unsafe {
            // compiler will unroll this loop
            for (idx, mod_) in [
                (self.mods.control, Modifiers::CONTROL),
                (self.mods.shift, Modifiers::SHIFT),
                (self.mods.super_, Modifiers::SUPER),
                (self.mods.alt, Modifiers::ALT),
                (self.mods.caps_lock, Modifiers::CAPS_LOCK),
                (self.mods.num_lock, Modifiers::NUM_LOCK),
            ] {
                if xkb_state_mod_index_is_active(self.state, idx, XKB_STATE_MODS_EFFECTIVE) != 0 {
                    mods |= mod_;
                }
            }
        }
        KeyEvent {
            state,
            key,
            code,
            location,
            mods,
            repeat,
            is_composing,
        }
    }

    fn get_logical_key(&mut self, scancode: u32) -> Key {
        let keysym = self.key_get_one_sym(scancode);
        let mut key = keycodes::map_key(keysym);
        if matches!(key, Key::Unidentified) {
            if let Some(s) = self.key_get_utf8(keysym) {
                key = Key::Character(s);
            }
        }
        key
    }

    fn key_get_one_sym(&mut self, scancode: u32) -> u32 {
        unsafe { xkb_state_key_get_one_sym(self.state, scancode) }
    }

    /// Get the string representation of a key.
    // TODO `keyboard_types` forces us to return a String, but it would be nicer if we could stay
    // on the stack, especially since we know all results will only contain 1 unicode codepoint
    fn key_get_utf8(&mut self, keysym: u32) -> Option<String> {
        // We convert the XKB 'symbol' to a string directly, rather than using the XKB 'string'
        // because (experimentally) [UI Events Keyboard Events](https://www.w3.org/TR/uievents-key/#key-attribute-value)
        // use the symbol rather than the x11 string (which includes the ctrl KeySym transformation)
        // If we used the KeySym transformation, it would not be possible to use keyboard shortcuts containing the
        // control key, for example
        let chr = unsafe { xkb_keysym_to_utf32(keysym) };
        if chr == 0 {
            // There is no unicode representation of this symbol
            return None;
        }
        let chr = char::from_u32(chr).expect("xkb should give valid UTF-32 char");
        Some(String::from(chr))
    }
}

impl Clone for State {
    fn clone(&self) -> Self {
        Self {
            state: unsafe { xkb_state_ref(self.state) },
            mods: self.mods,
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        unsafe {
            xkb_state_unref(self.state);
        }
    }
}
