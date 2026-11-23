use std::io::Cursor;

use pipewire as pw;
use pw::spa;
use spa::param::ParamType;
use spa::pod::deserialize::PodDeserializer;
use spa::pod::serialize::PodSerializer;
use spa::pod::{Object, Property, PropertyFlags, Value, ValueArray};
use spa::sys as spa_sys;
use spa::utils::SpaTypes;

use crate::audio::types::{AudioChannelPosition, AudioPortAvailability};

/// Converts PipeWire linear amplitude (`channelVolumes`) to user-perceived linear level (`0.0..=1.5`).
#[must_use]
pub(crate) fn raw_to_linear(raw: f32) -> f32 {
    if !raw.is_finite() || raw <= 0.0 {
        0.0
    } else {
        raw.cbrt().clamp(0.0, 1.5)
    }
}

/// Converts user-perceived linear volume (`0.0..=1.5`) into PipeWire linear amplitude (`v^3`).
#[must_use]
pub(crate) fn linear_to_raw(linear: f32) -> f32 {
    if !linear.is_finite() || linear <= 0.0 {
        0.0
    } else {
        linear.clamp(0.0, 1.5).powi(3)
    }
}

/// Converts PipeWire linear amplitude into decibels when audible.
#[must_use]
pub(crate) fn raw_to_db(raw: f32) -> Option<f32> {
    if !raw.is_finite() || raw <= 1e-6 {
        None
    } else {
        Some(20.0 * raw.log10())
    }
}

pub(crate) fn parse_default_node_name(json_val: &str) -> Option<String> {
    let trimmed = json_val.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return None;
    }
    let key_pos = trimmed.find("\"name\"")?;
    let after_key = &trimmed[key_pos + "\"name\"".len()..];
    let colon = after_key.find(':')?;
    let after_colon = after_key[colon + 1..].trim_start();
    let rest = after_colon.strip_prefix('"')?;
    let mut out = String::new();
    let mut escaped = false;
    for ch in rest.chars() {
        if escaped {
            out.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return if out.is_empty() { None } else { Some(out) };
        } else {
            out.push(ch);
        }
    }
    None
}

pub(crate) fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct PropsExtracted {
    pub(crate) channels: Option<usize>,
    pub(crate) volume: Option<f32>,
    pub(crate) channel_volumes: Option<Vec<f32>>,
    pub(crate) channel_map: Option<Vec<AudioChannelPosition>>,
    pub(crate) muted: Option<bool>,
}

pub(crate) fn map_spa_audio_channel(raw_id: u32) -> AudioChannelPosition {
    match raw_id {
        x if x == spa_sys::SPA_AUDIO_CHANNEL_MONO => AudioChannelPosition::Mono,
        x if x == spa_sys::SPA_AUDIO_CHANNEL_FL => AudioChannelPosition::FrontLeft,
        x if x == spa_sys::SPA_AUDIO_CHANNEL_FR => AudioChannelPosition::FrontRight,
        x if x == spa_sys::SPA_AUDIO_CHANNEL_FC => AudioChannelPosition::FrontCenter,
        x if x == spa_sys::SPA_AUDIO_CHANNEL_LFE || x == spa_sys::SPA_AUDIO_CHANNEL_LFE2 => {
            AudioChannelPosition::Lfe
        }
        x if x == spa_sys::SPA_AUDIO_CHANNEL_SL || x == spa_sys::SPA_AUDIO_CHANNEL_TSL => {
            AudioChannelPosition::SideLeft
        }
        x if x == spa_sys::SPA_AUDIO_CHANNEL_SR || x == spa_sys::SPA_AUDIO_CHANNEL_TSR => {
            AudioChannelPosition::SideRight
        }
        x if x == spa_sys::SPA_AUDIO_CHANNEL_FLC
            || x == spa_sys::SPA_AUDIO_CHANNEL_FLW
            || x == spa_sys::SPA_AUDIO_CHANNEL_FLH
            || x == spa_sys::SPA_AUDIO_CHANNEL_LLFE
            || x == spa_sys::SPA_AUDIO_CHANNEL_BLC =>
        {
            AudioChannelPosition::FrontLeftCenter
        }
        x if x == spa_sys::SPA_AUDIO_CHANNEL_FRC
            || x == spa_sys::SPA_AUDIO_CHANNEL_FRW
            || x == spa_sys::SPA_AUDIO_CHANNEL_FRH
            || x == spa_sys::SPA_AUDIO_CHANNEL_RLFE
            || x == spa_sys::SPA_AUDIO_CHANNEL_BRC =>
        {
            AudioChannelPosition::FrontRightCenter
        }
        x if x == spa_sys::SPA_AUDIO_CHANNEL_RC || x == spa_sys::SPA_AUDIO_CHANNEL_BC => {
            AudioChannelPosition::RearCenter
        }
        x if x == spa_sys::SPA_AUDIO_CHANNEL_RL || x == spa_sys::SPA_AUDIO_CHANNEL_RLC => {
            AudioChannelPosition::RearLeft
        }
        x if x == spa_sys::SPA_AUDIO_CHANNEL_RR || x == spa_sys::SPA_AUDIO_CHANNEL_RRC => {
            AudioChannelPosition::RearRight
        }
        x if x == spa_sys::SPA_AUDIO_CHANNEL_TC => AudioChannelPosition::TopCenter,
        x if x == spa_sys::SPA_AUDIO_CHANNEL_TFL || x == spa_sys::SPA_AUDIO_CHANNEL_TFLC => {
            AudioChannelPosition::TopFrontLeft
        }
        x if x == spa_sys::SPA_AUDIO_CHANNEL_TFC || x == spa_sys::SPA_AUDIO_CHANNEL_FCH => {
            AudioChannelPosition::TopFrontCenter
        }
        x if x == spa_sys::SPA_AUDIO_CHANNEL_TFR || x == spa_sys::SPA_AUDIO_CHANNEL_TFRC => {
            AudioChannelPosition::TopFrontRight
        }
        x if x == spa_sys::SPA_AUDIO_CHANNEL_TRL => AudioChannelPosition::TopRearLeft,
        x if x == spa_sys::SPA_AUDIO_CHANNEL_TRC => AudioChannelPosition::TopRearCenter,
        x if x == spa_sys::SPA_AUDIO_CHANNEL_TRR => AudioChannelPosition::TopRearRight,
        x if (spa_sys::SPA_AUDIO_CHANNEL_START_Aux
            ..spa_sys::SPA_AUDIO_CHANNEL_START_Aux + 4096)
            .contains(&x) =>
        {
            AudioChannelPosition::Aux((x - spa_sys::SPA_AUDIO_CHANNEL_START_Aux) as u16)
        }
        _ => AudioChannelPosition::Unknown,
    }
}

fn extract_props_object(obj: &Object) -> PropsExtracted {
    let mut channels = None;
    let mut volume = None;
    let mut channel_volumes = None;
    let mut channel_map = None;
    let mut scalar_volume = None;
    let mut muted = None;

    for prop in &obj.properties {
        match prop.key {
            spa_sys::SPA_PROP_mute => {
                if let Value::Bool(b) = prop.value {
                    muted = Some(b);
                }
            }
            spa_sys::SPA_PROP_channelVolumes => {
                if let Value::ValueArray(ValueArray::Float(vols)) = &prop.value
                    && !vols.is_empty()
                {
                    channels = Some(vols.len());
                    let peak = vols
                        .iter()
                        .copied()
                        .filter(|v| v.is_finite())
                        .fold(0.0_f32, f32::max);
                    volume = Some(peak);
                    channel_volumes = Some(vols.clone());
                }
            }
            spa_sys::SPA_PROP_channelMap => {
                if let Value::ValueArray(ValueArray::Id(ids)) = &prop.value
                    && !ids.is_empty()
                {
                    channel_map = Some(ids.iter().map(|id| map_spa_audio_channel(id.0)).collect());
                }
            }
            spa_sys::SPA_PROP_volume => {
                if let Value::Float(v) = prop.value {
                    scalar_volume = Some(v);
                }
            }
            _ => {}
        }
    }

    PropsExtracted {
        channels: channels.or_else(|| channel_map.as_ref().map(Vec::len)),
        volume: volume.or(scalar_volume),
        channel_volumes,
        channel_map,
        muted,
    }
}

fn map_spa_availability(raw_id: u32) -> AudioPortAvailability {
    match raw_id {
        x if x == spa_sys::SPA_PARAM_AVAILABILITY_yes => AudioPortAvailability::Available,
        x if x == spa_sys::SPA_PARAM_AVAILABILITY_no => AudioPortAvailability::Unavailable,
        _ => AudioPortAvailability::Unknown,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RouteParams {
    pub(crate) index: i32,
    pub(crate) direction: Option<u32>,
    pub(crate) device: Option<i32>,
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) priority: u32,
    pub(crate) availability: AudioPortAvailability,
    pub(crate) profiles: Vec<i32>,
    pub(crate) devices: Vec<i32>,
    pub(crate) channels: Option<usize>,
    pub(crate) volume: Option<f32>,
    pub(crate) channel_volumes: Option<Vec<f32>>,
    pub(crate) channel_map: Option<Vec<AudioChannelPosition>>,
    pub(crate) muted: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProfileParams {
    pub(crate) index: i32,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) priority: u32,
    pub(crate) available: bool,
}

pub(crate) fn parse_props_pod(bytes: &[u8]) -> Option<PropsExtracted> {
    let (_, value) = PodDeserializer::deserialize_any_from(bytes).ok()?;
    let Value::Object(obj) = value else {
        return None;
    };
    Some(extract_props_object(&obj))
}

pub(crate) fn parse_route_pod(bytes: &[u8]) -> Option<RouteParams> {
    let (_, value) = PodDeserializer::deserialize_any_from(bytes).ok()?;
    let Value::Object(obj) = value else {
        return None;
    };
    let mut index = None;
    let mut direction = None;
    let mut device = None;
    let mut name = None;
    let mut description = None;
    let mut priority = 0_u32;
    let mut availability = AudioPortAvailability::Unknown;
    let mut profiles = Vec::new();
    let mut devices = Vec::new();
    let mut props = PropsExtracted::default();

    for prop in &obj.properties {
        match prop.key {
            spa_sys::SPA_PARAM_ROUTE_index => {
                if let Value::Int(i) = prop.value {
                    index = Some(i);
                }
            }
            spa_sys::SPA_PARAM_ROUTE_direction => {
                if let Value::Id(id) = prop.value {
                    direction = Some(id.0);
                }
            }
            spa_sys::SPA_PARAM_ROUTE_device => {
                if let Value::Int(d) = prop.value {
                    device = Some(d);
                }
            }
            spa_sys::SPA_PARAM_ROUTE_name => {
                if let Value::String(s) = &prop.value
                    && !s.is_empty()
                {
                    name = Some(s.clone());
                }
            }
            spa_sys::SPA_PARAM_ROUTE_description => {
                if let Value::String(s) = &prop.value
                    && !s.is_empty()
                {
                    description = Some(s.clone());
                }
            }
            spa_sys::SPA_PARAM_ROUTE_priority => {
                if let Value::Int(p) = prop.value {
                    priority = u32::try_from(p).unwrap_or(0);
                }
            }
            spa_sys::SPA_PARAM_ROUTE_available => {
                if let Value::Id(id) = prop.value {
                    availability = map_spa_availability(id.0);
                }
            }
            spa_sys::SPA_PARAM_ROUTE_profiles => {
                if let Value::ValueArray(ValueArray::Int(v)) = &prop.value {
                    profiles.clone_from(v);
                }
            }
            spa_sys::SPA_PARAM_ROUTE_devices => {
                if let Value::ValueArray(ValueArray::Int(v)) = &prop.value {
                    devices.clone_from(v);
                }
            }
            spa_sys::SPA_PARAM_ROUTE_props => {
                if let Value::Object(props_obj) = &prop.value {
                    props = extract_props_object(props_obj);
                }
            }
            _ => {}
        }
    }

    Some(RouteParams {
        index: index?,
        direction,
        device,
        name,
        description,
        priority,
        availability,
        profiles,
        devices,
        channels: props.channels,
        volume: props.volume,
        channel_volumes: props.channel_volumes,
        channel_map: props.channel_map,
        muted: props.muted,
    })
}

pub(crate) fn parse_profile_pod(bytes: &[u8]) -> Option<ProfileParams> {
    let (_, value) = PodDeserializer::deserialize_any_from(bytes).ok()?;
    let Value::Object(obj) = value else {
        return None;
    };
    let mut index = None;
    let mut name = None;
    let mut description = None;
    let mut priority = 0_u32;
    let mut available = true;

    for prop in &obj.properties {
        match prop.key {
            spa_sys::SPA_PARAM_PROFILE_index => {
                if let Value::Int(i) = prop.value {
                    index = Some(i);
                }
            }
            spa_sys::SPA_PARAM_PROFILE_name => {
                if let Value::String(s) = &prop.value
                    && !s.is_empty()
                {
                    name = Some(s.clone());
                }
            }
            spa_sys::SPA_PARAM_PROFILE_description => {
                if let Value::String(s) = &prop.value
                    && !s.is_empty()
                {
                    description = Some(s.clone());
                }
            }
            spa_sys::SPA_PARAM_PROFILE_priority => {
                if let Value::Int(p) = prop.value {
                    priority = u32::try_from(p).unwrap_or(0);
                }
            }
            spa_sys::SPA_PARAM_PROFILE_available => {
                if let Value::Id(id) = prop.value {
                    available = id.0 != spa_sys::SPA_PARAM_AVAILABILITY_no;
                }
            }
            _ => {}
        }
    }

    let name = name?;
    let description = description.unwrap_or_else(|| name.clone());
    Some(ProfileParams {
        index: index?,
        name,
        description,
        priority,
        available,
    })
}

fn props_channel_volumes_object(object_id: ParamType, linear_channels: &[f32]) -> Object {
    let vols: Vec<f32> = if linear_channels.is_empty() {
        vec![0.0]
    } else {
        linear_channels.iter().copied().map(linear_to_raw).collect()
    };
    Object {
        type_: SpaTypes::ObjectParamProps.as_raw(),
        id: object_id.as_raw(),
        properties: vec![Property {
            key: spa_sys::SPA_PROP_channelVolumes,
            flags: PropertyFlags::empty(),
            value: Value::ValueArray(ValueArray::Float(vols)),
        }],
    }
}

fn props_volume_object(object_id: ParamType, linear: f32, channels: usize) -> Object {
    let raw = linear_to_raw(linear);
    let vols = vec![raw; channels.max(1)];
    Object {
        type_: SpaTypes::ObjectParamProps.as_raw(),
        id: object_id.as_raw(),
        properties: vec![Property {
            key: spa_sys::SPA_PROP_channelVolumes,
            flags: PropertyFlags::empty(),
            value: Value::ValueArray(ValueArray::Float(vols)),
        }],
    }
}

fn props_mute_object(object_id: ParamType, muted: bool) -> Object {
    Object {
        type_: SpaTypes::ObjectParamProps.as_raw(),
        id: object_id.as_raw(),
        properties: vec![Property {
            key: spa_sys::SPA_PROP_mute,
            flags: PropertyFlags::empty(),
            value: Value::Bool(muted),
        }],
    }
}

pub(crate) fn build_volume_pod_channels(linear_channels: &[f32]) -> Option<Vec<u8>> {
    let obj = props_channel_volumes_object(ParamType::Props, linear_channels);
    let (cursor, _) =
        PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(obj)).ok()?;
    Some(cursor.into_inner())
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn build_volume_pod(linear: f32, channels: usize) -> Option<Vec<u8>> {
    let obj = props_volume_object(ParamType::Props, linear, channels);
    let (cursor, _) =
        PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(obj)).ok()?;
    Some(cursor.into_inner())
}

pub(crate) fn build_mute_pod(muted: bool) -> Option<Vec<u8>> {
    let obj = props_mute_object(ParamType::Props, muted);
    let (cursor, _) =
        PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(obj)).ok()?;
    Some(cursor.into_inner())
}

fn serialize_route_pod(index: i32, device: i32, props_obj: Object) -> Option<Vec<u8>> {
    let route_obj = Object {
        type_: SpaTypes::ObjectParamRoute.as_raw(),
        id: ParamType::Route.as_raw(),
        properties: vec![
            Property {
                key: spa_sys::SPA_PARAM_ROUTE_index,
                flags: PropertyFlags::empty(),
                value: Value::Int(index),
            },
            Property {
                key: spa_sys::SPA_PARAM_ROUTE_device,
                flags: PropertyFlags::empty(),
                value: Value::Int(device),
            },
            Property {
                key: spa_sys::SPA_PARAM_ROUTE_props,
                flags: PropertyFlags::empty(),
                value: Value::Object(props_obj),
            },
            Property {
                key: spa_sys::SPA_PARAM_ROUTE_save,
                flags: PropertyFlags::empty(),
                value: Value::Bool(true),
            },
        ],
    };
    let (cursor, _) =
        PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(route_obj)).ok()?;
    Some(cursor.into_inner())
}

pub(crate) fn build_route_volume_pod_channels(
    index: i32,
    device: i32,
    linear_channels: &[f32],
) -> Option<Vec<u8>> {
    serialize_route_pod(
        index,
        device,
        props_channel_volumes_object(ParamType::Route, linear_channels),
    )
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn build_route_volume_pod(
    index: i32,
    device: i32,
    linear: f32,
    channels: usize,
) -> Option<Vec<u8>> {
    serialize_route_pod(
        index,
        device,
        props_volume_object(ParamType::Route, linear, channels),
    )
}

pub(crate) fn build_route_mute_pod(index: i32, device: i32, muted: bool) -> Option<Vec<u8>> {
    serialize_route_pod(index, device, props_mute_object(ParamType::Route, muted))
}

/// Builds a `ParamType::Route` selection POD switching `device` (`card.profile.device`) to
/// route `index` with `save = true` so WirePlumber restores stored route props and persists the
/// user's port choice.
pub(crate) fn build_route_select_pod(index: i32, device: i32) -> Option<Vec<u8>> {
    let route_obj = Object {
        type_: SpaTypes::ObjectParamRoute.as_raw(),
        id: ParamType::Route.as_raw(),
        properties: vec![
            Property {
                key: spa_sys::SPA_PARAM_ROUTE_index,
                flags: PropertyFlags::empty(),
                value: Value::Int(index),
            },
            Property {
                key: spa_sys::SPA_PARAM_ROUTE_device,
                flags: PropertyFlags::empty(),
                value: Value::Int(device),
            },
            Property {
                key: spa_sys::SPA_PARAM_ROUTE_save,
                flags: PropertyFlags::empty(),
                value: Value::Bool(true),
            },
        ],
    };
    let (cursor, _) =
        PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(route_obj)).ok()?;
    Some(cursor.into_inner())
}

/// Builds a `ParamType::Profile` selection POD switching a card to profile `index` with
/// `save = true`.
pub(crate) fn build_profile_pod(index: i32) -> Option<Vec<u8>> {
    let profile_obj = Object {
        type_: SpaTypes::ObjectParamProfile.as_raw(),
        id: ParamType::Profile.as_raw(),
        properties: vec![
            Property {
                key: spa_sys::SPA_PARAM_PROFILE_index,
                flags: PropertyFlags::empty(),
                value: Value::Int(index),
            },
            Property {
                key: spa_sys::SPA_PARAM_PROFILE_save,
                flags: PropertyFlags::empty(),
                value: Value::Bool(true),
            },
        ],
    };
    let (cursor, _) =
        PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(profile_obj)).ok()?;
    Some(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_mute_pod_roundtrip() {
        let vol_bytes = build_volume_pod(0.5, 2).expect("volume pod serialized");
        let extracted = parse_props_pod(&vol_bytes).expect("volume pod parsed");
        assert_eq!(extracted.channels, Some(2));
        assert!(extracted.muted.is_none());
        let linear = raw_to_linear(extracted.volume.unwrap());
        assert!((linear - 0.5).abs() < 1e-4, "got {linear}");

        let ch_bytes =
            build_volume_pod_channels(&[0.8, 0.4]).expect("per-channel volume pod serialized");
        let ch_ext = parse_props_pod(&ch_bytes).expect("per-channel volume pod parsed");
        let ch_vols = ch_ext.channel_volumes.expect("channel_volumes present");
        assert_eq!(ch_vols.len(), 2);
        assert!((raw_to_linear(ch_vols[0]) - 0.8).abs() < 1e-4);
        assert!((raw_to_linear(ch_vols[1]) - 0.4).abs() < 1e-4);
        assert!((raw_to_linear(ch_ext.volume.unwrap()) - 0.8).abs() < 1e-4);

        let mute_bytes = build_mute_pod(true).expect("mute pod serialized");
        let mute_ext = parse_props_pod(&mute_bytes).expect("mute pod parsed");
        assert_eq!(mute_ext.muted, Some(true));
    }

    #[test]
    fn route_volume_mute_pod_roundtrip() {
        let r_vol = build_route_volume_pod(3, 5, 0.72, 2).expect("route vol pod serialized");
        let (_, Value::Object(vol_outer)) =
            PodDeserializer::deserialize_any_from(&r_vol).expect("route vol outer")
        else {
            panic!("expected outer Object");
        };
        assert_eq!(vol_outer.id, ParamType::Route.as_raw());
        let inner_vol_id = vol_outer.properties.iter().find_map(|p| {
            if p.key == spa_sys::SPA_PARAM_ROUTE_props
                && let Value::Object(o) = &p.value
            {
                Some(o.id)
            } else {
                None
            }
        });
        assert_eq!(inner_vol_id, Some(ParamType::Route.as_raw()));

        let parsed = parse_route_pod(&r_vol).expect("route vol pod parsed");
        assert_eq!(parsed.index, 3);
        assert_eq!(parsed.device, Some(5));
        assert_eq!(parsed.channels, Some(2));
        let linear = raw_to_linear(parsed.volume.unwrap());
        assert!((linear - 0.72).abs() < 1e-4, "got {linear}");

        let r_ch = build_route_volume_pod_channels(4, 5, &[0.9, 0.45])
            .expect("route channel vol pod serialized");
        let parsed_ch = parse_route_pod(&r_ch).expect("route channel vol pod parsed");
        assert_eq!(parsed_ch.index, 4);
        assert_eq!(parsed_ch.device, Some(5));
        let r_vols = parsed_ch.channel_volumes.expect("route channel volumes");
        assert!((raw_to_linear(r_vols[0]) - 0.9).abs() < 1e-4);
        assert!((raw_to_linear(r_vols[1]) - 0.45).abs() < 1e-4);

        let r_sel = build_route_select_pod(4, 5).expect("route select pod serialized");
        let parsed_sel = parse_route_pod(&r_sel).expect("route select pod parsed");
        assert_eq!(parsed_sel.index, 4);
        assert_eq!(parsed_sel.device, Some(5));

        let r_mute = build_route_mute_pod(4, 5, true).expect("route mute pod serialized");
        let (_, Value::Object(mute_outer)) =
            PodDeserializer::deserialize_any_from(&r_mute).expect("route mute outer")
        else {
            panic!("expected outer Object");
        };
        let inner_mute_id = mute_outer.properties.iter().find_map(|p| {
            if p.key == spa_sys::SPA_PARAM_ROUTE_props
                && let Value::Object(o) = &p.value
            {
                Some(o.id)
            } else {
                None
            }
        });
        assert_eq!(inner_mute_id, Some(ParamType::Route.as_raw()));

        let parsed_m = parse_route_pod(&r_mute).expect("route mute pod parsed");
        assert_eq!(parsed_m.index, 4);
        assert_eq!(parsed_m.device, Some(5));
        assert_eq!(parsed_m.muted, Some(true));
    }

    #[test]
    fn parse_route_pod_handles_full_pipewire_route_payload() {
        let props_obj = Object {
            type_: SpaTypes::ObjectParamProps.as_raw(),
            id: ParamType::Route.as_raw(),
            properties: vec![
                Property {
                    key: spa_sys::SPA_PROP_mute,
                    flags: PropertyFlags::empty(),
                    value: Value::Bool(false),
                },
                Property {
                    key: spa_sys::SPA_PROP_channelVolumes,
                    flags: PropertyFlags::empty(),
                    value: Value::ValueArray(ValueArray::Float(vec![0.125, 0.125])),
                },
                Property {
                    key: spa_sys::SPA_PROP_channelMap,
                    flags: PropertyFlags::empty(),
                    value: Value::ValueArray(ValueArray::Id(vec![
                        spa::utils::Id(3),
                        spa::utils::Id(4),
                    ])),
                },
                Property {
                    key: spa_sys::SPA_PROP_latencyOffsetNsec,
                    flags: PropertyFlags::empty(),
                    value: Value::Long(0),
                },
            ],
        };
        let full_route = Object {
            type_: SpaTypes::ObjectParamRoute.as_raw(),
            id: ParamType::Route.as_raw(),
            properties: vec![
                Property {
                    key: spa_sys::SPA_PARAM_ROUTE_index,
                    flags: PropertyFlags::empty(),
                    value: Value::Int(4),
                },
                Property {
                    key: spa_sys::SPA_PARAM_ROUTE_direction,
                    flags: PropertyFlags::empty(),
                    value: Value::Id(spa::utils::Id(spa_sys::SPA_DIRECTION_OUTPUT)),
                },
                Property {
                    key: spa_sys::SPA_PARAM_ROUTE_name,
                    flags: PropertyFlags::empty(),
                    value: Value::String("analog-output-headphones".to_owned()),
                },
                Property {
                    key: spa_sys::SPA_PARAM_ROUTE_description,
                    flags: PropertyFlags::empty(),
                    value: Value::String("Headphones".to_owned()),
                },
                Property {
                    key: spa_sys::SPA_PARAM_ROUTE_priority,
                    flags: PropertyFlags::empty(),
                    value: Value::Int(9900),
                },
                Property {
                    key: spa_sys::SPA_PARAM_ROUTE_available,
                    flags: PropertyFlags::empty(),
                    value: Value::Id(spa::utils::Id(spa_sys::SPA_PARAM_AVAILABILITY_yes)),
                },
                Property {
                    key: spa_sys::SPA_PARAM_ROUTE_info,
                    flags: PropertyFlags::empty(),
                    value: Value::Struct(vec![
                        Value::Int(1),
                        Value::String("port.type".to_owned()),
                        Value::String("headphones".to_owned()),
                    ]),
                },
                Property {
                    key: spa_sys::SPA_PARAM_ROUTE_profiles,
                    flags: PropertyFlags::empty(),
                    value: Value::ValueArray(ValueArray::Int(vec![1, 2])),
                },
                Property {
                    key: spa_sys::SPA_PARAM_ROUTE_device,
                    flags: PropertyFlags::empty(),
                    value: Value::Int(5),
                },
                Property {
                    key: spa_sys::SPA_PARAM_ROUTE_props,
                    flags: PropertyFlags::empty(),
                    value: Value::Object(props_obj),
                },
                Property {
                    key: spa_sys::SPA_PARAM_ROUTE_devices,
                    flags: PropertyFlags::empty(),
                    value: Value::ValueArray(ValueArray::Int(vec![5])),
                },
                Property {
                    key: spa_sys::SPA_PARAM_ROUTE_save,
                    flags: PropertyFlags::empty(),
                    value: Value::Bool(false),
                },
            ],
        };
        let (cursor, _) =
            PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(full_route))
                .expect("serialize full route");
        let parsed = parse_route_pod(cursor.get_ref()).expect("parse full route");
        assert_eq!(parsed.index, 4);
        assert_eq!(parsed.direction, Some(spa_sys::SPA_DIRECTION_OUTPUT));
        assert_eq!(parsed.device, Some(5));
        assert_eq!(parsed.name.as_deref(), Some("analog-output-headphones"));
        assert_eq!(parsed.description.as_deref(), Some("Headphones"));
        assert_eq!(parsed.priority, 9900);
        assert_eq!(parsed.availability, AudioPortAvailability::Available);
        assert_eq!(parsed.profiles, vec![1, 2]);
        assert_eq!(parsed.devices, vec![5]);
        assert_eq!(parsed.channels, Some(2));
        assert_eq!(
            parsed.channel_map,
            Some(vec![
                AudioChannelPosition::FrontLeft,
                AudioChannelPosition::FrontRight,
            ])
        );
        assert_eq!(parsed.muted, Some(false));
        assert!((raw_to_linear(parsed.volume.unwrap()) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn profile_pod_parse_and_build() {
        let prof_obj = Object {
            type_: SpaTypes::ObjectParamProfile.as_raw(),
            id: ParamType::EnumProfile.as_raw(),
            properties: vec![
                Property {
                    key: spa_sys::SPA_PARAM_PROFILE_index,
                    flags: PropertyFlags::empty(),
                    value: Value::Int(1),
                },
                Property {
                    key: spa_sys::SPA_PARAM_PROFILE_name,
                    flags: PropertyFlags::empty(),
                    value: Value::String("output:analog-stereo+input:analog-stereo".to_owned()),
                },
                Property {
                    key: spa_sys::SPA_PARAM_PROFILE_description,
                    flags: PropertyFlags::empty(),
                    value: Value::String("Analog Stereo Duplex".to_owned()),
                },
                Property {
                    key: spa_sys::SPA_PARAM_PROFILE_priority,
                    flags: PropertyFlags::empty(),
                    value: Value::Int(6565),
                },
                Property {
                    key: spa_sys::SPA_PARAM_PROFILE_available,
                    flags: PropertyFlags::empty(),
                    value: Value::Id(spa::utils::Id(spa_sys::SPA_PARAM_AVAILABILITY_yes)),
                },
            ],
        };
        let (cursor, _) =
            PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(prof_obj))
                .expect("serialize profile");
        let parsed = parse_profile_pod(cursor.get_ref()).expect("parse profile");
        assert_eq!(parsed.index, 1);
        assert_eq!(parsed.name, "output:analog-stereo+input:analog-stereo");
        assert_eq!(parsed.description, "Analog Stereo Duplex");
        assert_eq!(parsed.priority, 6565);
        assert!(parsed.available);

        let set_pod = build_profile_pod(1).expect("build_profile_pod");
        let (_, Value::Object(set_obj)) =
            PodDeserializer::deserialize_any_from(&set_pod).expect("deserialize set_profile")
        else {
            panic!("expected Object");
        };
        assert_eq!(set_obj.id, ParamType::Profile.as_raw());
    }

    #[test]
    fn parse_default_node_json_handles_escapes() {
        assert_eq!(
            parse_default_node_name("{\"name\":\"alsa_output.pci-0000_00_1f.3.analog-stereo\"}")
                .as_deref(),
            Some("alsa_output.pci-0000_00_1f.3.analog-stereo")
        );
        assert_eq!(parse_default_node_name("null"), None);
    }
}
