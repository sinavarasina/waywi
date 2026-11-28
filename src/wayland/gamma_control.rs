//! Wayland `zwlr_gamma_control_manager_v1` per-output gamma table & color-temperature backend.

use std::collections::HashMap;
use std::ffi::CString;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::AsFd;

use rustix::fs::{MemfdFlags, memfd_create};
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_output;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_wlr::gamma_control::v1::client::{
    zwlr_gamma_control_manager_v1::{self, ZwlrGammaControlManagerV1},
    zwlr_gamma_control_v1::{self, ZwlrGammaControlV1},
};

use crate::display::{OutputId, OutputInfo, resolve_outputs};
use crate::effect::GammaAction;
use crate::error::{Result, WaywiError};
use crate::wayland::connection::WaylandState;
use crate::wayland_ext::GammaRamp;

#[derive(Debug, Clone)]
enum PendingGamma {
    Ramp(GammaRamp),
    Temperature { kelvin: u32, brightness: f32 },
}

struct OutputGammaEntry {
    control: ZwlrGammaControlV1,
    gamma_size: Option<u32>,
    pending: Option<PendingGamma>,
}

/// Manages per-output `zwlr_gamma_control_v1` handles; automatically restores compositor gamma
/// tables when reset or dropped.
pub(crate) struct WaylandGammaControlState {
    manager: Option<ZwlrGammaControlManagerV1>,
    controls: HashMap<OutputId, OutputGammaEntry>,
}

impl Drop for WaylandGammaControlState {
    fn drop(&mut self) {
        self.clear_all();
        if let Some(mgr) = self.manager.take() {
            mgr.destroy();
        }
    }
}

impl WaylandGammaControlState {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<WaylandState>) -> Self {
        let manager = globals
            .bind::<ZwlrGammaControlManagerV1, _, _>(qh, 1..=1, ())
            .ok();
        Self {
            manager,
            controls: HashMap::new(),
        }
    }

    #[must_use]
    pub(crate) fn is_supported(&self) -> bool {
        self.manager.is_some()
    }

    pub(crate) fn apply_action(
        &mut self,
        action: &GammaAction,
        outputs: &[OutputInfo],
        output_handles: &HashMap<OutputId, wl_output::WlOutput>,
        qh: &QueueHandle<WaylandState>,
    ) -> bool {
        let Some(mgr) = self.manager.clone() else {
            return false;
        };
        let target_sel = match action {
            GammaAction::SetRamp { output, .. }
            | GammaAction::SetTemperature { output, .. }
            | GammaAction::Reset { output } => output,
        };
        let matched = resolve_outputs(target_sel, outputs);
        if matched.is_empty() {
            return false;
        }

        let mut acted = false;
        for out_info in matched {
            let out_id = out_info.id;
            match action {
                GammaAction::Reset { .. } => {
                    if let Some(entry) = self.controls.remove(&out_id) {
                        entry.control.destroy();
                        acted = true;
                    }
                }
                GammaAction::SetRamp { ramp, .. } => {
                    let pending = PendingGamma::Ramp(ramp.clone());
                    acted |= self.apply_to_output(&mgr, out_id, output_handles, pending, qh);
                }
                GammaAction::SetTemperature {
                    kelvin, brightness, ..
                } => {
                    let pending = PendingGamma::Temperature {
                        kelvin: *kelvin,
                        brightness: *brightness,
                    };
                    acted |= self.apply_to_output(&mgr, out_id, output_handles, pending, qh);
                }
            }
        }
        acted
    }

    fn apply_to_output(
        &mut self,
        mgr: &ZwlrGammaControlManagerV1,
        out_id: OutputId,
        output_handles: &HashMap<OutputId, wl_output::WlOutput>,
        pending: PendingGamma,
        qh: &QueueHandle<WaylandState>,
    ) -> bool {
        if let Some(entry) = self.controls.get_mut(&out_id) {
            if let Some(size) = entry.gamma_size {
                let ramp = resolve_pending_ramp(&pending, size as usize);
                if upload_gamma_table(&entry.control, &ramp).is_ok() {
                    entry.pending = None;
                    return true;
                }
                return false;
            }
            entry.pending = Some(pending);
            return true;
        }
        let Some(wl_out) = output_handles.get(&out_id) else {
            return false;
        };
        let control = mgr.get_gamma_control(wl_out, qh, out_id);
        self.controls.insert(
            out_id,
            OutputGammaEntry {
                control,
                gamma_size: None,
                pending: Some(pending),
            },
        );
        true
    }

    pub(crate) fn clear_all(&mut self) {
        for (_, entry) in self.controls.drain() {
            entry.control.destroy();
        }
    }
}

fn resolve_pending_ramp(pending: &PendingGamma, size: usize) -> GammaRamp {
    match pending {
        PendingGamma::Ramp(r) => r.resample(size),
        PendingGamma::Temperature { kelvin, brightness } => {
            GammaRamp::from_temperature(size, *kelvin, *brightness)
        }
    }
}

fn upload_gamma_table(control: &ZwlrGammaControlV1, ramp: &GammaRamp) -> Result<()> {
    ramp.validate()?;
    let n = ramp.red.len();
    let mut bytes = Vec::with_capacity(n * 3 * 2);
    for &v in &ramp.red {
        bytes.extend_from_slice(&v.to_ne_bytes());
    }
    for &v in &ramp.green {
        bytes.extend_from_slice(&v.to_ne_bytes());
    }
    for &v in &ramp.blue {
        bytes.extend_from_slice(&v.to_ne_bytes());
    }

    let name = CString::new("waywi-gamma-lut").expect("valid cstr");
    let fd = memfd_create(&name, MemfdFlags::CLOEXEC)
        .map_err(|e| WaywiError::WaylandConnection(format!("gamma memfd_create failed: {e}")))?;
    let mut file = std::fs::File::from(fd);
    file.write_all(&bytes)?;
    file.seek(SeekFrom::Start(0))?;
    control.set_gamma(file.as_fd());
    Ok(())
}

impl Dispatch<ZwlrGammaControlManagerV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrGammaControlManagerV1,
        _event: zwlr_gamma_control_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrGammaControlV1, OutputId> for WaylandState {
    fn event(
        state: &mut Self,
        _proxy: &ZwlrGammaControlV1,
        event: zwlr_gamma_control_v1::Event,
        output_id: &OutputId,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_gamma_control_v1::Event::GammaSize { size } => {
                if size == 0 {
                    if let Some(entry) = state.gamma_control.controls.remove(output_id) {
                        entry.control.destroy();
                    }
                    return;
                }
                if let Some(entry) = state.gamma_control.controls.get_mut(output_id) {
                    entry.gamma_size = Some(size);
                    if let Some(pending) = entry.pending.take() {
                        let ramp = resolve_pending_ramp(&pending, size as usize);
                        let _ = upload_gamma_table(&entry.control, &ramp);
                    }
                }
            }
            zwlr_gamma_control_v1::Event::Failed => {
                if let Some(entry) = state.gamma_control.controls.remove(output_id) {
                    entry.control.destroy();
                }
            }
            _ => {}
        }
    }
}
