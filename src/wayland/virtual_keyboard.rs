//! Wayland `zwp_virtual_keyboard_v1` backend for on-screen keyboards (OSK) and synthetic input.

use std::ffi::CString;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::AsFd;
use std::sync::OnceLock;
use std::time::Instant;

use rustix::fs::{MemfdFlags, memfd_create};
use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::{wl_keyboard, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::{self, ZwpVirtualKeyboardManagerV1},
    zwp_virtual_keyboard_v1::{self, ZwpVirtualKeyboardV1},
};

use crate::capabilities::Capability;
use crate::error::{Result, WaywiError};
use crate::input::VirtualKeyState;
use crate::wayland::connection::WaylandState;

/// Minimal fallback XKB v1 keymap string used only if `xkbcommon` cannot find system
/// `xkeyboard-config` data (e.g. minimal containers or CI sandboxes).
const FALLBACK_MINIMAL_XKB_KEYMAP: &str = r#"xkb_keymap {
    xkb_keycodes "(minimal)" {
        minimum = 8;
        maximum = 255;
        <ESC>  = 9;
        <AE01> = 10;
        <BKSP> = 22;
        <TAB>  = 23;
        <AD01> = 24;
        <RTRN> = 36;
        <AC01> = 38;
        <LFSH> = 50;
        <SPCE> = 65;
    };
    xkb_types "(minimal)" {
        virtual_modifiers NumLock;
        type "ONE_LEVEL" { modifiers = none; level_name[Level1] = "Any"; };
        type "TWO_LEVEL" { modifiers = Shift; map[Shift] = Level2; level_name[Level1] = "Base"; level_name[Level2] = "Shift"; };
    };
    xkb_compatibility "(minimal)" {
        interpret.useModMapMods = AnyLevel;
        interpret Any+AnyOf(all) { action = SetMods(modifiers=modMapMods,clearLocks); };
    };
    xkb_symbols "(minimal)" {
        key <ESC>  { [ Escape ] };
        key <AE01> { [ 1, exclam ] };
        key <BKSP> { [ BackSpace ] };
        key <TAB>  { [ Tab ] };
        key <AD01> { [ q, Q ] };
        key <RTRN> { [ Return ] };
        key <AC01> { [ a, A ] };
        key <LFSH> { [ Shift_L ] };
        key <SPCE> { [ space ] };
        modifier_map Shift { <LFSH> };
    };
};
"#;

/// Lazily initializes and manages a `zwp_virtual_keyboard_v1` device.
pub(crate) struct WaylandVirtualKeyboardState {
    manager: Option<ZwpVirtualKeyboardManagerV1>,
    keyboard: Option<ZwpVirtualKeyboardV1>,
    seat_id: Option<ObjectId>,
    keymap_uploaded: bool,
    epoch: Instant,
}

impl Drop for WaylandVirtualKeyboardState {
    fn drop(&mut self) {
        if let Some(vk) = self.keyboard.take() {
            vk.destroy();
        }
    }
}

impl WaylandVirtualKeyboardState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        let manager = globals
            .bind::<ZwpVirtualKeyboardManagerV1, _, _>(qh, 1..=1, ())
            .ok();
        Self {
            manager,
            keyboard: None,
            seat_id: None,
            keymap_uploaded: false,
            epoch: Instant::now(),
        }
    }

    #[must_use]
    pub(crate) fn is_supported(&self) -> bool {
        self.manager.is_some()
    }

    /// Destroys the active virtual keyboard if its parent `wl_seat` was removed by the compositor.
    pub(crate) fn remove_seat(&mut self, seat: &wl_seat::WlSeat) {
        if self.seat_id.as_ref() == Some(&seat.id()) {
            if let Some(vk) = self.keyboard.take() {
                vk.destroy();
            }
            self.seat_id = None;
            self.keymap_uploaded = false;
        }
    }

    fn ensure_keyboard(
        &mut self,
        seat: Option<&wl_seat::WlSeat>,
        qh: &QueueHandle<WaylandState>,
    ) -> Result<&ZwpVirtualKeyboardV1> {
        if self.keyboard.is_none() {
            let mgr = self
                .manager
                .as_ref()
                .ok_or(WaywiError::UnsupportedCapability(
                    Capability::VirtualKeyboard,
                ))?;
            let seat = seat.ok_or_else(|| {
                WaywiError::InvalidState(
                    "zwp_virtual_keyboard_v1 requires an active wl_seat".to_owned(),
                )
            })?;
            let vk = mgr.create_virtual_keyboard(seat, qh, ());
            self.keyboard = Some(vk);
            self.seat_id = Some(seat.id());
            self.keymap_uploaded = false;
        }
        Ok(self.keyboard.as_ref().expect("virtual keyboard created"))
    }

    pub(crate) fn upload_keymap(
        &mut self,
        seat: Option<&wl_seat::WlSeat>,
        qh: &QueueHandle<WaylandState>,
        keymap_str: &str,
    ) -> Result<()> {
        let vk = self.ensure_keyboard(seat, qh)?.clone();
        let mut bytes = keymap_str.as_bytes().to_vec();
        if !bytes.ends_with(&[0]) {
            bytes.push(0);
        }
        let name = CString::new("waywi-vk-keymap").expect("valid cstr");
        let fd = memfd_create(&name, MemfdFlags::CLOEXEC)
            .map_err(|e| WaywiError::WaylandConnection(format!("memfd_create failed: {e}")))?;
        let mut file = std::fs::File::from(fd);
        file.write_all(&bytes)
            .map_err(|e| WaywiError::WaylandConnection(format!("keymap write failed: {e}")))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|e| WaywiError::WaylandConnection(format!("keymap seek failed: {e}")))?;
        let size = u32::try_from(bytes.len()).map_err(|_| {
            WaywiError::InvalidState("virtual keyboard keymap exceeds u32 size".to_owned())
        })?;
        vk.keymap(wl_keyboard::KeymapFormat::XkbV1 as u32, file.as_fd(), size);
        self.keymap_uploaded = true;
        Ok(())
    }

    fn ensure_keymap_ready(
        &mut self,
        seat: Option<&wl_seat::WlSeat>,
        qh: &QueueHandle<WaylandState>,
    ) -> Result<()> {
        if self.keymap_uploaded && self.keyboard.is_some() {
            return Ok(());
        }
        let default_keymap = default_xkb_keymap();
        self.upload_keymap(seat, qh, default_keymap)
    }

    pub(crate) fn send_key(
        &mut self,
        seat: Option<&wl_seat::WlSeat>,
        qh: &QueueHandle<WaylandState>,
        keycode: u32,
        state: VirtualKeyState,
    ) -> Result<()> {
        self.ensure_keymap_ready(seat, qh)?;
        let time_ms = self.epoch.elapsed().as_millis() as u32;
        let proto_state = match state {
            VirtualKeyState::Released => wl_keyboard::KeyState::Released as u32,
            VirtualKeyState::Pressed => wl_keyboard::KeyState::Pressed as u32,
        };
        if let Some(ref vk) = self.keyboard {
            vk.key(time_ms, keycode, proto_state);
        }
        Ok(())
    }

    pub(crate) fn send_modifiers(
        &mut self,
        seat: Option<&wl_seat::WlSeat>,
        qh: &QueueHandle<WaylandState>,
        depressed: u32,
        latched: u32,
        locked: u32,
        group: u32,
    ) -> Result<()> {
        self.ensure_keymap_ready(seat, qh)?;
        if let Some(ref vk) = self.keyboard {
            vk.modifiers(depressed, latched, locked, group);
        }
        Ok(())
    }
}

fn default_xkb_keymap() -> &'static str {
    static DEFAULT_KEYMAP: OnceLock<String> = OnceLock::new();
    DEFAULT_KEYMAP.get_or_init(|| {
        let ctx = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
        if let Some(km) = xkbcommon::xkb::Keymap::new_from_names(
            &ctx,
            "",
            "",
            "us",
            "",
            None,
            xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
        ) {
            let s = km.get_as_string(xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1);
            if !s.is_empty() {
                return s;
            }
        }
        FALLBACK_MINIMAL_XKB_KEYMAP.to_owned()
    })
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpVirtualKeyboardManagerV1,
        _event: zwp_virtual_keyboard_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpVirtualKeyboardV1,
        _event: zwp_virtual_keyboard_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}
