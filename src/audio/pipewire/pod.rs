use std::io::Cursor;

use pipewire as pw;
use pw::spa;
use spa::param::ParamType;
use spa::pod::deserialize::PodDeserializer;
use spa::pod::serialize::PodSerializer;
use spa::pod::{Object, Property, PropertyFlags, Value, ValueArray};
use spa::sys as spa_sys;
use spa::utils::SpaTypes;

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

fn extract_props_object(obj: &Object) -> (Option<usize>, Option<f32>, Option<bool>) {
    let mut channels = None;
    let mut volume = None;
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
                    let sum: f32 = vols.iter().copied().sum();
                    volume = Some(sum / vols.len() as f32);
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

    (channels, volume.or(scalar_volume), muted)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RouteParams {
    pub(crate) index: i32,
    pub(crate) direction: Option<u32>,
    pub(crate) device: i32,
    pub(crate) channels: Option<usize>,
    pub(crate) volume: Option<f32>,
    pub(crate) muted: Option<bool>,
}

pub(crate) fn parse_props_pod(bytes: &[u8]) -> Option<(Option<usize>, Option<f32>, Option<bool>)> {
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
    let mut props = (None, None, None);

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
        device: device?,
        channels: props.0,
        volume: props.1,
        muted: props.2,
    })
}

fn props_volume_object(linear: f32, channels: usize) -> Object {
    let raw = linear_to_raw(linear);
    let vols = vec![raw; channels.max(1)];
    Object {
        type_: SpaTypes::ObjectParamProps.as_raw(),
        id: ParamType::Props.as_raw(),
        properties: vec![Property {
            key: spa_sys::SPA_PROP_channelVolumes,
            flags: PropertyFlags::empty(),
            value: Value::ValueArray(ValueArray::Float(vols)),
        }],
    }
}

fn props_mute_object(muted: bool) -> Object {
    Object {
        type_: SpaTypes::ObjectParamProps.as_raw(),
        id: ParamType::Props.as_raw(),
        properties: vec![Property {
            key: spa_sys::SPA_PROP_mute,
            flags: PropertyFlags::empty(),
            value: Value::Bool(muted),
        }],
    }
}

pub(crate) fn build_volume_pod(linear: f32, channels: usize) -> Option<Vec<u8>> {
    let obj = props_volume_object(linear, channels);
    let (cursor, _) =
        PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(obj)).ok()?;
    Some(cursor.into_inner())
}

pub(crate) fn build_mute_pod(muted: bool) -> Option<Vec<u8>> {
    let obj = props_mute_object(muted);
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

pub(crate) fn build_route_volume_pod(
    index: i32,
    device: i32,
    linear: f32,
    channels: usize,
) -> Option<Vec<u8>> {
    serialize_route_pod(index, device, props_volume_object(linear, channels))
}

pub(crate) fn build_route_mute_pod(index: i32, device: i32, muted: bool) -> Option<Vec<u8>> {
    serialize_route_pod(index, device, props_mute_object(muted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_mute_pod_roundtrip() {
        let vol_bytes = build_volume_pod(0.5, 2).expect("volume pod serialized");
        let (ch, vol, muted) = parse_props_pod(&vol_bytes).expect("volume pod parsed");
        assert_eq!(ch, Some(2));
        assert!(muted.is_none());
        let linear = raw_to_linear(vol.unwrap());
        assert!((linear - 0.5).abs() < 1e-4, "got {linear}");

        let mute_bytes = build_mute_pod(true).expect("mute pod serialized");
        let (_, _, parsed_mute) = parse_props_pod(&mute_bytes).expect("mute pod parsed");
        assert_eq!(parsed_mute, Some(true));
    }

    #[test]
    fn route_volume_mute_pod_roundtrip() {
        let r_vol = build_route_volume_pod(3, 5, 0.72, 2).expect("route vol pod serialized");
        let parsed = parse_route_pod(&r_vol).expect("route vol pod parsed");
        assert_eq!(parsed.index, 3);
        assert_eq!(parsed.device, 5);
        assert_eq!(parsed.channels, Some(2));
        let linear = raw_to_linear(parsed.volume.unwrap());
        assert!((linear - 0.72).abs() < 1e-4, "got {linear}");

        let r_mute = build_route_mute_pod(3, 5, true).expect("route mute pod serialized");
        let parsed_m = parse_route_pod(&r_mute).expect("route mute pod parsed");
        assert_eq!(parsed_m.index, 3);
        assert_eq!(parsed_m.device, 5);
        assert_eq!(parsed_m.muted, Some(true));
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
