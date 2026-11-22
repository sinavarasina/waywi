//! XDG Desktop Portal Settings (`org.freedesktop.portal.Settings`) client and subscription.
//!
//! Exposes desktop appearance preferences (`org.freedesktop.appearance`: `color-scheme`,
//! `contrast`, and v2 `accent-color`) as pure read-only data without imposing any visual theme
//! policy on applications.

use std::collections::HashMap;

use zbus::zvariant::{OwnedValue, Value};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::render::Color;
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::client::{DbusClient, DbusSharedBus, owned_value_to_dbus};
use super::types::{Dbus, DbusBus, DbusValue};
use super::worker::ServiceWatchWorker;

const PORTAL_DEST: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const PORTAL_SETTINGS_IFACE: &str = "org.freedesktop.portal.Settings";
const APPEARANCE_NAMESPACE: &str = "org.freedesktop.appearance";

/// Desktop color-scheme preference reported by `org.freedesktop.portal.Settings`
/// (`org.freedesktop.appearance` / `color-scheme`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PortalColorScheme {
    /// No explicit preference (or unrecognized portal value per specification).
    #[default]
    NoPreference,
    /// User prefers a dark appearance (`1`).
    PreferDark,
    /// User prefers a light appearance (`2`).
    PreferLight,
}

impl PortalColorScheme {
    /// Decodes the XDG Portal `color-scheme` integer (`0` = no preference, `1` = dark, `2` = light).
    #[inline]
    #[must_use]
    pub const fn from_portal_u32(raw: u32) -> Self {
        match raw {
            1 => Self::PreferDark,
            2 => Self::PreferLight,
            _ => Self::NoPreference,
        }
    }
}

/// Desktop contrast preference reported by `org.freedesktop.portal.Settings`
/// (`org.freedesktop.appearance` / `contrast`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PortalContrast {
    /// Standard contrast (`0`).
    #[default]
    Normal,
    /// Higher contrast requested (`1`).
    High,
}

impl PortalContrast {
    /// Decodes the XDG Portal `contrast` integer (`0` = normal, `1` = high).
    #[inline]
    #[must_use]
    pub const fn from_portal_u32(raw: u32) -> Self {
        match raw {
            1 => Self::High,
            _ => Self::Normal,
        }
    }
}

/// Normalized sRGB accent color (`0.0..=1.0` per channel) reported by
/// `org.freedesktop.portal.Settings` v2 (`org.freedesktop.appearance` / `accent-color`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PortalAccentColor {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

impl PortalAccentColor {
    /// Constructs a validated accent color, returning `None` if any channel is outside `0.0..=1.0`
    /// or non-finite (which indicates an unset accent color per the XDG Portal specification).
    #[must_use]
    pub fn from_rgb_f64(r: f64, g: f64, b: f64) -> Option<Self> {
        let valid = |v: f64| v.is_finite() && (0.0..=1.0).contains(&v);
        if valid(r) && valid(g) && valid(b) {
            Some(Self { r, g, b })
        } else {
            None
        }
    }

    /// Converts this normalized sRGB accent color to an opaque WayWi [`Color`].
    #[must_use]
    pub fn to_color(self) -> Color {
        let to_u8 = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Color::rgb(to_u8(self.r), to_u8(self.g), to_u8(self.b))
    }
}

/// Snapshot of desktop appearance preferences from `org.freedesktop.portal.Settings`
/// (`org.freedesktop.appearance`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PortalSettingsSnapshot {
    pub color_scheme: PortalColorScheme,
    pub contrast: PortalContrast,
    pub accent_color: Option<PortalAccentColor>,
}

impl Dbus {
    /// Subscribes to `org.freedesktop.portal.Settings` (`org.freedesktop.appearance`) changes on
    /// the session bus, emitting an initial snapshot followed by zero-polling signal updates.
    pub fn portal_settings<M: 'static>(
        map: impl Fn(PortalSettingsSnapshot) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::DbusPortalSettings {
                key: "dbus:portal-settings".into_subscription_key(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}

impl DbusClient {
    /// Reads the current `org.freedesktop.appearance` snapshot (`color-scheme`, `contrast`,
    /// `accent-color`) from `org.freedesktop.portal.Settings`.
    pub fn portal_settings_snapshot(&self) -> Result<PortalSettingsSnapshot> {
        let conn = self.connection(DbusBus::Session)?;
        let reply = conn
            .call_method(
                Some(PORTAL_DEST),
                PORTAL_PATH,
                Some(PORTAL_SETTINGS_IFACE),
                "ReadAll",
                &(&[APPEARANCE_NAMESPACE][..],),
            )
            .map_err(|e| WaywiError::DbusError(format!("portal Settings.ReadAll failed: {e}")))?;
        let all: HashMap<String, HashMap<String, OwnedValue>> = reply
            .body()
            .deserialize()
            .map_err(|e| WaywiError::DbusError(e.to_string()))?;
        Ok(parse_appearance_map(all.get(APPEARANCE_NAMESPACE)))
    }

    /// Reads an arbitrary setting `(namespace, key)` from `org.freedesktop.portal.Settings` using
    /// `ReadOne` (portal v2), falling back to deprecated `Read` (portal v1) automatically.
    pub fn portal_setting(&self, namespace: &str, key: &str) -> Result<DbusValue> {
        let conn = self.connection(DbusBus::Session)?;
        let raw_val: OwnedValue = match conn.call_method(
            Some(PORTAL_DEST),
            PORTAL_PATH,
            Some(PORTAL_SETTINGS_IFACE),
            "ReadOne",
            &(namespace, key),
        ) {
            Ok(reply) => reply
                .body()
                .deserialize()
                .map_err(|e| WaywiError::DbusError(e.to_string()))?,
            Err(_) => {
                let reply = conn
                    .call_method(
                        Some(PORTAL_DEST),
                        PORTAL_PATH,
                        Some(PORTAL_SETTINGS_IFACE),
                        "Read",
                        &(namespace, key),
                    )
                    .map_err(|e| {
                        WaywiError::DbusError(format!(
                            "portal Settings.Read({namespace}, {key}) failed: {e}"
                        ))
                    })?;
                reply
                    .body()
                    .deserialize()
                    .map_err(|e| WaywiError::DbusError(e.to_string()))?
            }
        };
        let mut val = owned_value_to_dbus(&raw_val);
        while let DbusValue::Variant(inner) = val {
            val = *inner;
        }
        Ok(val)
    }
}

pub(crate) fn spawn_portal_settings_worker(
    sender: calloop::channel::Sender<PortalSettingsSnapshot>,
    shared: DbusSharedBus,
) -> ServiceWatchWorker {
    ServiceWatchWorker::spawn(
        DbusBus::Session,
        "waywi-dbus-portal-settings",
        Some(PORTAL_DEST),
        Some(PORTAL_SETTINGS_IFACE),
        sender,
        shared,
        |conn| async move {
            let reply = conn
                .call_method(
                    Some(PORTAL_DEST),
                    PORTAL_PATH,
                    Some(PORTAL_SETTINGS_IFACE),
                    "ReadAll",
                    &(&[APPEARANCE_NAMESPACE][..],),
                )
                .await
                .ok()?;
            let all: HashMap<String, HashMap<String, OwnedValue>> =
                reply.body().deserialize().ok()?;
            Some(parse_appearance_map(all.get(APPEARANCE_NAMESPACE)))
        },
    )
}

fn unwrap_zvariant<'a>(mut val: &'a Value<'a>) -> &'a Value<'a> {
    while let Value::Value(inner) = val {
        val = inner;
    }
    val
}

fn value_as_u32(val: &Value<'_>) -> Option<u32> {
    match unwrap_zvariant(val) {
        Value::U32(v) => Some(*v),
        Value::I32(v) => u32::try_from(*v).ok(),
        Value::U64(v) => u32::try_from(*v).ok(),
        Value::U16(v) => Some(u32::from(*v)),
        Value::U8(v) => Some(u32::from(*v)),
        _ => None,
    }
}

fn value_as_accent_color(val: &Value<'_>) -> Option<PortalAccentColor> {
    let Value::Structure(st) = unwrap_zvariant(val) else {
        return None;
    };
    let fields = st.fields();
    if fields.len() != 3 {
        return None;
    }
    let as_f64 = |v: &Value<'_>| match unwrap_zvariant(v) {
        Value::F64(f) => Some(*f),
        _ => None,
    };
    PortalAccentColor::from_rgb_f64(
        as_f64(&fields[0])?,
        as_f64(&fields[1])?,
        as_f64(&fields[2])?,
    )
}

/// Parses an `org.freedesktop.appearance` dictionary from `Settings.ReadAll`.
pub(crate) fn parse_appearance_map(
    map: Option<&HashMap<String, OwnedValue>>,
) -> PortalSettingsSnapshot {
    let Some(map) = map else {
        return PortalSettingsSnapshot::default();
    };
    let color_scheme = map
        .get("color-scheme")
        .and_then(|v| value_as_u32(v))
        .map(PortalColorScheme::from_portal_u32)
        .unwrap_or_default();
    let contrast = map
        .get("contrast")
        .and_then(|v| value_as_u32(v))
        .map(PortalContrast::from_portal_u32)
        .unwrap_or_default();
    let accent_color = map
        .get("accent-color")
        .and_then(|v| value_as_accent_color(v));
    PortalSettingsSnapshot {
        color_scheme,
        contrast,
        accent_color,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_appearance_color_scheme_contrast_and_accent_color() {
        let mut map: HashMap<String, OwnedValue> = HashMap::new();
        map.insert("color-scheme".to_owned(), OwnedValue::from(1u32));
        map.insert("contrast".to_owned(), OwnedValue::from(1u32));
        let accent_struct = zbus::zvariant::StructureBuilder::new()
            .add_field(0.25f64)
            .add_field(0.5f64)
            .add_field(1.0f64)
            .build()
            .unwrap();
        map.insert(
            "accent-color".to_owned(),
            OwnedValue::try_from(Value::from(accent_struct)).unwrap(),
        );

        let snap = parse_appearance_map(Some(&map));
        assert_eq!(snap.color_scheme, PortalColorScheme::PreferDark);
        assert_eq!(snap.contrast, PortalContrast::High);
        let accent = snap.accent_color.expect("valid accent color");
        assert_eq!(
            accent,
            PortalAccentColor {
                r: 0.25,
                g: 0.5,
                b: 1.0
            }
        );
        assert_eq!(accent.to_color(), Color::rgb(64, 128, 255));
    }

    #[test]
    fn rejects_out_of_range_accent_color_and_unknown_enums_fallback_cleanly() {
        let mut map: HashMap<String, OwnedValue> = HashMap::new();
        map.insert("color-scheme".to_owned(), OwnedValue::from(99u32));
        map.insert("contrast".to_owned(), OwnedValue::from(42u32));
        let bad_accent = zbus::zvariant::StructureBuilder::new()
            .add_field(-0.1f64)
            .add_field(0.5f64)
            .add_field(1.2f64)
            .build()
            .unwrap();
        map.insert(
            "accent-color".to_owned(),
            OwnedValue::try_from(Value::from(bad_accent)).unwrap(),
        );

        let snap = parse_appearance_map(Some(&map));
        assert_eq!(snap.color_scheme, PortalColorScheme::NoPreference);
        assert_eq!(snap.contrast, PortalContrast::Normal);
        assert_eq!(snap.accent_color, None);
    }
}
