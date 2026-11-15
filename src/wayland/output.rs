use std::collections::HashMap;

use smithay_client_toolkit::output::{OutputInfo as SctkOutputInfo, OutputState};
use wayland_client::Proxy;
use wayland_client::protocol::wl_output;

use crate::display::{OutputId, OutputInfo, Transform};
use crate::geometry::{LogicalPx, LogicalSize, Point, ScaleFactor};

/// Maps Wayland `wl_output::Transform` onto WayWi's [`Transform`].
#[must_use]
pub(crate) fn map_transform(tr: wl_output::Transform) -> Transform {
    match tr {
        wl_output::Transform::Normal => Transform::Normal,
        wl_output::Transform::_90 => Transform::Rotate90,
        wl_output::Transform::_180 => Transform::Rotate180,
        wl_output::Transform::_270 => Transform::Rotate270,
        wl_output::Transform::Flipped => Transform::Flipped,
        wl_output::Transform::Flipped90 => Transform::Flipped90,
        wl_output::Transform::Flipped180 => Transform::Flipped180,
        wl_output::Transform::Flipped270 => Transform::Flipped270,
        _ => Transform::Normal,
    }
}

/// Stable WayWi output identifier derived from `wl_output` registry/object identity.
#[must_use]
pub(crate) fn output_id_for(
    wl_out: &wl_output::WlOutput,
    info: Option<&SctkOutputInfo>,
) -> OutputId {
    if let Some(info) = info {
        OutputId(info.id as u64)
    } else {
        OutputId(wl_out.id().protocol_id() as u64)
    }
}

/// Converts SCTK output metadata into sorted [`OutputInfo`] records plus `OutputId -> WlOutput` lookup.
pub(crate) fn collect_outputs(
    state: &OutputState,
) -> (Vec<OutputInfo>, HashMap<OutputId, wl_output::WlOutput>) {
    let mut infos = Vec::new();
    let mut map = HashMap::new();

    for wl_out in state.outputs() {
        let Some(sctk_info) = state.info(&wl_out) else {
            continue;
        };
        let id = output_id_for(&wl_out, Some(&sctk_info));
        let (lx, ly) = sctk_info
            .logical_position
            .or(Some(sctk_info.location))
            .unwrap_or((0, 0));
        let (lw, lh) = sctk_info.logical_size.unwrap_or_else(|| {
            sctk_info
                .modes
                .iter()
                .find(|m| m.current)
                .map(|m| (m.dimensions.0, m.dimensions.1))
                .unwrap_or((1920, 1080))
        });
        let refresh_mhz = sctk_info
            .modes
            .iter()
            .find(|m| m.current)
            .map(|m| m.refresh_rate)
            .unwrap_or(60_000);

        let info = OutputInfo {
            id,
            name: sctk_info.name.clone(),
            description: sctk_info.description.clone(),
            make: sctk_info.make.clone(),
            model: sctk_info.model.clone(),
            logical_position: Point::new(LogicalPx(lx as f32), LogicalPx(ly as f32)),
            logical_size: LogicalSize::new(
                LogicalPx((lw.max(1)) as f32),
                LogicalPx((lh.max(1)) as f32),
            ),
            physical_size_mm: sctk_info.physical_size,
            scale_factor: ScaleFactor::from_integer(sctk_info.scale_factor.max(1)),
            transform: map_transform(sctk_info.transform),
            refresh_mhz,
        };
        infos.push(info);
        map.insert(id, wl_out);
    }

    infos.sort_by_key(|a| a.id);
    (infos, map)
}
