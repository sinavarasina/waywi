//! XDG Desktop Portal Settings (`org.freedesktop.portal.Settings`) client and subscription.
//!
//! Exposes desktop appearance preferences (`org.freedesktop.appearance`: `color-scheme`,
//! `contrast`, and v2 `accent-color`) as pure read-only data without imposing any visual theme
//! policy on applications.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_lite::StreamExt;
use zbus::zvariant::{Fd, OwnedObjectPath, OwnedValue, Value};

use crate::arena::FrameSharedFn;
use crate::error::{Result, WaywiError};
use crate::render::Color;
use crate::subscription::{Subscription, SubscriptionSpec};

use super::client::{
    DBUS_SLOW_TIMEOUT, DbusActionLane, DbusClient, DbusSharedBus, LaneActionSpec,
    owned_value_to_dbus, validate_object_path,
};
use super::types::{Dbus, DbusBus, DbusValue};
use super::worker::ServiceWatchWorker;
use super::zvariant_helpers::{dict_str, dict_str_vec, dict_u32, unwrap_value};

const PORTAL_DEST: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const PORTAL_SETTINGS_IFACE: &str = "org.freedesktop.portal.Settings";
const PORTAL_OPEN_URI_IFACE: &str = "org.freedesktop.portal.OpenURI";
const PORTAL_SCREENSHOT_IFACE: &str = "org.freedesktop.portal.Screenshot";
const PORTAL_FILE_CHOOSER_IFACE: &str = "org.freedesktop.portal.FileChooser";
const PORTAL_INHIBIT_IFACE: &str = "org.freedesktop.portal.Inhibit";
const PORTAL_TRASH_IFACE: &str = "org.freedesktop.portal.Trash";
const PORTAL_BACKGROUND_IFACE: &str = "org.freedesktop.portal.Background";
const PORTAL_SCREENCAST_IFACE: &str = "org.freedesktop.portal.ScreenCast";
const PORTAL_REQUEST_IFACE: &str = "org.freedesktop.portal.Request";
const PORTAL_SESSION_IFACE: &str = "org.freedesktop.portal.Session";
const APPEARANCE_NAMESPACE: &str = "org.freedesktop.appearance";

static NEXT_PORTAL_TOKEN: AtomicU64 = AtomicU64::new(1);

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
                map: FrameSharedFn::new(map),
            },
        }
    }
}

/// Options for [`PortalClient::open_uri`] / [`PortalClient::open_file_uri`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PortalOpenUriOptions {
    pub parent_window: String,
    pub writable: bool,
    pub ask: bool,
    pub activation_token: Option<String>,
}

impl PortalOpenUriOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn parent_window(mut self, window: impl Into<String>) -> Self {
        self.parent_window = window.into();
        self
    }

    #[must_use]
    pub fn writable(mut self, writable: bool) -> Self {
        self.writable = writable;
        self
    }

    #[must_use]
    pub fn ask(mut self, ask: bool) -> Self {
        self.ask = ask;
        self
    }

    #[must_use]
    pub fn activation_token(mut self, token: impl Into<String>) -> Self {
        self.activation_token = Some(token.into());
        self
    }
}

/// Options for [`PortalClient::screenshot_blocking`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalScreenshotOptions {
    pub parent_window: String,
    pub modal: bool,
    pub interactive: bool,
}

impl Default for PortalScreenshotOptions {
    fn default() -> Self {
        Self {
            parent_window: String::new(),
            modal: true,
            interactive: true,
        }
    }
}

impl PortalScreenshotOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }

    #[must_use]
    pub fn modal(mut self, modal: bool) -> Self {
        self.modal = modal;
        self
    }

    #[must_use]
    pub fn parent_window(mut self, window: impl Into<String>) -> Self {
        self.parent_window = window.into();
        self
    }
}

/// File filter entry for [`PortalFileChooserOptions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalFileFilter {
    pub label: String,
    /// `(kind, pattern)` where `kind == 0` is glob (`"*.png"`) and `kind == 1` is MIME (`"image/png"`).
    pub patterns: Vec<(u32, String)>,
}

impl PortalFileFilter {
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            patterns: Vec::new(),
        }
    }

    #[must_use]
    pub fn glob(mut self, pattern: impl Into<String>) -> Self {
        self.patterns.push((0, pattern.into()));
        self
    }

    #[must_use]
    pub fn mime(mut self, mime: impl Into<String>) -> Self {
        self.patterns.push((1, mime.into()));
        self
    }
}

/// Options for [`PortalClient::open_file_blocking`] and [`PortalClient::save_file_blocking`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PortalFileChooserOptions {
    pub parent_window: String,
    pub title: String,
    pub accept_label: Option<String>,
    pub modal: bool,
    pub multiple: bool,
    pub directory: bool,
    pub current_name: Option<String>,
    pub current_folder: Option<String>,
    pub filters: Vec<PortalFileFilter>,
}

impl PortalFileChooserOptions {
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            modal: true,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn parent_window(mut self, window: impl Into<String>) -> Self {
        self.parent_window = window.into();
        self
    }

    #[must_use]
    pub fn accept_label(mut self, label: impl Into<String>) -> Self {
        self.accept_label = Some(label.into());
        self
    }

    #[must_use]
    pub fn multiple(mut self, multiple: bool) -> Self {
        self.multiple = multiple;
        self
    }

    #[must_use]
    pub fn directory(mut self, directory: bool) -> Self {
        self.directory = directory;
        self
    }

    #[must_use]
    pub fn current_name(mut self, name: impl Into<String>) -> Self {
        self.current_name = Some(name.into());
        self
    }

    #[must_use]
    pub fn current_folder(mut self, folder: impl Into<String>) -> Self {
        self.current_folder = Some(folder.into());
        self
    }

    #[must_use]
    pub fn filter(mut self, filter: PortalFileFilter) -> Self {
        self.filters.push(filter);
        self
    }
}

bitflags::bitflags! {
    /// Bitmask for `org.freedesktop.portal.Inhibit.Inhibit`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct PortalInhibitFlags: u32 {
        const LOGOUT = 1;
        const USER_SWITCH = 2;
        const SUSPEND = 4;
        const IDLE = 8;
    }
}

/// Options for [`PortalClient::request_background_blocking`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PortalBackgroundOptions {
    pub parent_window: String,
    pub reason: Option<String>,
    pub autostart: bool,
    pub commandline: Vec<String>,
    pub dbus_activatable: bool,
}

impl PortalBackgroundOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    #[must_use]
    pub fn autostart(mut self, autostart: bool) -> Self {
        self.autostart = autostart;
        self
    }

    #[must_use]
    pub fn commandline(mut self, argv: Vec<String>) -> Self {
        self.commandline = argv;
        self
    }

    #[must_use]
    pub fn dbus_activatable(mut self, activatable: bool) -> Self {
        self.dbus_activatable = activatable;
        self
    }
}

/// Result returned by [`PortalClient::request_background_blocking`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PortalBackgroundResult {
    pub background: bool,
    pub autostart: bool,
}

bitflags::bitflags! {
    /// Source types for `org.freedesktop.portal.ScreenCast.SelectSources`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct PortalScreenCastSourceType: u32 {
        const MONITOR = 1;
        const WINDOW = 2;
        const VIRTUAL = 4;
    }
}

/// Cursor embedding mode for `org.freedesktop.portal.ScreenCast`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PortalScreenCastCursorMode {
    Hidden = 1,
    #[default]
    Embedded = 2,
    Metadata = 4,
}

/// Options for [`PortalClient::screencast_start_blocking`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalScreenCastOptions {
    pub parent_window: String,
    pub types: PortalScreenCastSourceType,
    pub multiple: bool,
    pub cursor_mode: PortalScreenCastCursorMode,
}

impl Default for PortalScreenCastOptions {
    fn default() -> Self {
        Self {
            parent_window: String::new(),
            types: PortalScreenCastSourceType::MONITOR | PortalScreenCastSourceType::WINDOW,
            multiple: false,
            cursor_mode: PortalScreenCastCursorMode::Embedded,
        }
    }
}

/// A single PipeWire video stream granted by `org.freedesktop.portal.ScreenCast`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PortalScreenCastStream {
    pub node_id: u32,
    pub source_type: Option<u32>,
    pub size: Option<(i32, i32)>,
    pub position: Option<(i32, i32)>,
}

/// Active ScreenCast session descriptor returned by [`PortalClient::screencast_start_blocking`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PortalScreenCastResult {
    /// Portal session object path (pass to [`PortalClient::close_session`] when done).
    pub session_handle: String,
    pub streams: Vec<PortalScreenCastStream>,
}

/// Zero-cost domain handle for XDG Desktop Portal services (`Settings`, `OpenURI`, `Screenshot`,
/// `FileChooser`, `Inhibit`, `Trash`, `Background`, `ScreenCast`, and `GlobalShortcuts`).
#[derive(Clone, Copy)]
pub struct PortalClient<'a>(pub(super) &'a DbusClient);

impl PortalClient<'_> {
    /// Reads the current `org.freedesktop.appearance` snapshot (`color-scheme`, `contrast`,
    /// `accent-color`) synchronously from `org.freedesktop.portal.Settings`.
    pub fn settings_blocking(&self) -> Result<PortalSettingsSnapshot> {
        let conn = self.0.async_connection(DbusBus::Session)?;
        futures_lite::future::block_on(read_portal_settings(&conn))
    }

    /// Reads an arbitrary setting `(namespace, key)` synchronously from
    /// `org.freedesktop.portal.Settings` using `ReadOne` (portal v2), falling back to deprecated
    /// `Read` (portal v1) automatically.
    pub fn read_setting_blocking(&self, namespace: &str, key: &str) -> Result<DbusValue> {
        let conn = self.0.async_connection(DbusBus::Session)?;
        let namespace = namespace.to_owned();
        let key = key.to_owned();
        futures_lite::future::block_on(async move {
            let raw_val: OwnedValue = match conn
                .call_method(
                    Some(PORTAL_DEST),
                    PORTAL_PATH,
                    Some(PORTAL_SETTINGS_IFACE),
                    "ReadOne",
                    &(namespace.as_str(), key.as_str()),
                )
                .await
            {
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
                            &(namespace.as_str(), key.as_str()),
                        )
                        .await
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
        })
    }

    /// Opens an external URI (e.g. `https://...`, `mailto:...`) via
    /// `org.freedesktop.portal.OpenURI.OpenURI` asynchronously on the Desktop lane.
    pub fn open_uri(&self, uri: &str, options: &PortalOpenUriOptions) -> Result<()> {
        if uri.trim().is_empty() {
            return Err(WaywiError::DbusError(
                "Portal OpenURI cannot be empty".into(),
            ));
        }
        let uri_owned = uri.to_owned();
        let target = uri_owned.clone();
        let opts = options.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::Session,
                "portal.OpenURI.OpenURI",
                target,
            )
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                call_portal_request(&conn, PORTAL_OPEN_URI_IFACE, "OpenURI", |token| {
                    let mut m: HashMap<&str, Value<'_>> = HashMap::new();
                    m.insert("handle_token", Value::from(token));
                    if opts.writable {
                        m.insert("writable", Value::from(true));
                    }
                    if opts.ask {
                        m.insert("ask", Value::from(true));
                    }
                    if let Some(ref tok) = opts.activation_token {
                        m.insert("activation_token", Value::from(tok.as_str()));
                    }
                    (opts.parent_window.as_str(), uri_owned.as_str(), m)
                })
                .await?;
                Ok(())
            },
        )
    }

    /// Opens a local file with the user's preferred handler via
    /// `org.freedesktop.portal.OpenURI.OpenFile` (passing an `O_RDONLY` file descriptor).
    pub fn open_file_uri(&self, path: &Path, options: &PortalOpenUriOptions) -> Result<()> {
        let file = File::open(path).map_err(|e| {
            WaywiError::DbusError(format!("OpenFile cannot open `{}`: {e}", path.display()))
        })?;
        let target = path.display().to_string();
        let opts = options.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::Session,
                "portal.OpenURI.OpenFile",
                target,
            )
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let fd = Fd::from(&file);
                call_portal_request(&conn, PORTAL_OPEN_URI_IFACE, "OpenFile", |token| {
                    let mut m: HashMap<&str, Value<'_>> = HashMap::new();
                    m.insert("handle_token", Value::from(token));
                    if opts.writable {
                        m.insert("writable", Value::from(true));
                    }
                    if opts.ask {
                        m.insert("ask", Value::from(true));
                    }
                    if let Some(ref tok) = opts.activation_token {
                        m.insert("activation_token", Value::from(tok.as_str()));
                    }
                    (opts.parent_window.as_str(), fd, m)
                })
                .await?;
                Ok(())
            },
        )
    }

    /// Reveals a local file or directory in the user's file manager via
    /// `org.freedesktop.portal.OpenURI.OpenDirectory`.
    pub fn open_directory(&self, path: &Path, options: &PortalOpenUriOptions) -> Result<()> {
        let file = File::open(path).map_err(|e| {
            WaywiError::DbusError(format!(
                "OpenDirectory cannot open `{}`: {e}",
                path.display()
            ))
        })?;
        let target = path.display().to_string();
        let opts = options.clone();
        self.0.submit_lane_action(
            LaneActionSpec::fast(
                DbusActionLane::Desktop,
                DbusBus::Session,
                "portal.OpenURI.OpenDirectory",
                target,
            )
            .with_timeout(DBUS_SLOW_TIMEOUT),
            move |conn| async move {
                let fd = Fd::from(&file);
                call_portal_request(&conn, PORTAL_OPEN_URI_IFACE, "OpenDirectory", |token| {
                    let mut m: HashMap<&str, Value<'_>> = HashMap::new();
                    m.insert("handle_token", Value::from(token));
                    if let Some(ref tok) = opts.activation_token {
                        m.insert("activation_token", Value::from(tok.as_str()));
                    }
                    (opts.parent_window.as_str(), fd, m)
                })
                .await?;
                Ok(())
            },
        )
    }

    /// Captures a screenshot via `org.freedesktop.portal.Screenshot.Screenshot`, returning the
    /// resulting `file://` URI (or `Ok(None)` if the user cancelled).
    pub fn screenshot_blocking(&self, options: &PortalScreenshotOptions) -> Result<Option<String>> {
        let conn = self.0.async_connection(DbusBus::Session)?;
        let opts = options.clone();
        futures_lite::future::block_on(async move {
            let Some(results) =
                call_portal_request(&conn, PORTAL_SCREENSHOT_IFACE, "Screenshot", |token| {
                    let mut m: HashMap<&str, Value<'_>> = HashMap::new();
                    m.insert("handle_token", Value::from(token));
                    m.insert("modal", Value::from(opts.modal));
                    m.insert("interactive", Value::from(opts.interactive));
                    (opts.parent_window.as_str(), m)
                })
                .await?
            else {
                return Ok(None);
            };
            Ok(dict_str(&results, "uri"))
        })
    }

    /// Launches the compositor's interactive color picker via
    /// `org.freedesktop.portal.Screenshot.PickColor`, returning `Ok(None)` if cancelled.
    pub fn pick_color_blocking(&self, parent_window: &str) -> Result<Option<PortalAccentColor>> {
        let conn = self.0.async_connection(DbusBus::Session)?;
        let parent = parent_window.to_owned();
        futures_lite::future::block_on(async move {
            let Some(results) =
                call_portal_request(&conn, PORTAL_SCREENSHOT_IFACE, "PickColor", |token| {
                    let mut m: HashMap<&str, Value<'_>> = HashMap::new();
                    m.insert("handle_token", Value::from(token));
                    (parent.as_str(), m)
                })
                .await?
            else {
                return Ok(None);
            };
            Ok(results.get("color").and_then(|v| value_as_accent_color(v)))
        })
    }

    /// Opens the desktop file picker via `org.freedesktop.portal.FileChooser.OpenFile`, returning
    /// selected URIs (empty if cancelled).
    pub fn open_file_blocking(&self, options: &PortalFileChooserOptions) -> Result<Vec<String>> {
        self.run_file_chooser_blocking("OpenFile", options)
    }

    /// Opens the desktop save-file dialog via `org.freedesktop.portal.FileChooser.SaveFile`,
    /// returning selected URIs (empty if cancelled).
    pub fn save_file_blocking(&self, options: &PortalFileChooserOptions) -> Result<Vec<String>> {
        self.run_file_chooser_blocking("SaveFile", options)
    }

    fn run_file_chooser_blocking(
        &self,
        method: &'static str,
        options: &PortalFileChooserOptions,
    ) -> Result<Vec<String>> {
        let conn = self.0.async_connection(DbusBus::Session)?;
        let opts = options.clone();
        futures_lite::future::block_on(async move {
            let Some(results) =
                call_portal_request(&conn, PORTAL_FILE_CHOOSER_IFACE, method, |token| {
                    let mut m: HashMap<&str, Value<'_>> = HashMap::new();
                    m.insert("handle_token", Value::from(token));
                    m.insert("modal", Value::from(opts.modal));
                    if method == "OpenFile" {
                        m.insert("multiple", Value::from(opts.multiple));
                        m.insert("directory", Value::from(opts.directory));
                    }
                    if let Some(ref lbl) = opts.accept_label {
                        m.insert("accept_label", Value::from(lbl.as_str()));
                    }
                    if let Some(ref name) = opts.current_name {
                        m.insert("current_name", Value::from(name.as_str()));
                    }
                    if let Some(ref folder) = opts.current_folder {
                        let mut bytes = folder.as_bytes().to_vec();
                        bytes.push(0);
                        m.insert("current_folder", Value::from(bytes));
                    }
                    if !opts.filters.is_empty() {
                        let encoded: Vec<(String, Vec<(u32, String)>)> = opts
                            .filters
                            .iter()
                            .map(|f| (f.label.clone(), f.patterns.clone()))
                            .collect();
                        m.insert("filters", Value::from(encoded));
                    }
                    (opts.parent_window.as_str(), opts.title.as_str(), m)
                })
                .await?
            else {
                return Ok(Vec::new());
            };
            Ok(dict_str_vec(&results, "uris"))
        })
    }

    /// Acquires a desktop session inhibitor via `org.freedesktop.portal.Inhibit.Inhibit` and
    /// returns the request handle object path (pass to [`Self::close_request`] to release).
    pub fn inhibit_blocking(
        &self,
        parent_window: &str,
        flags: PortalInhibitFlags,
        reason: &str,
    ) -> Result<String> {
        let conn = self.0.async_connection(DbusBus::Session)?;
        let parent = parent_window.to_owned();
        let reason_owned = reason.to_owned();
        futures_lite::future::block_on(async move {
            let token = next_portal_token("inhibit");
            let mut m: HashMap<&str, Value<'_>> = HashMap::new();
            m.insert("handle_token", Value::from(token.as_str()));
            if !reason_owned.is_empty() {
                m.insert("reason", Value::from(reason_owned.as_str()));
            }
            let reply = conn
                .call_method(
                    Some(PORTAL_DEST),
                    PORTAL_PATH,
                    Some(PORTAL_INHIBIT_IFACE),
                    "Inhibit",
                    &(parent.as_str(), flags.bits(), m),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("portal Inhibit failed: {e}")))?;
            let req_path: OwnedObjectPath = reply
                .body()
                .deserialize()
                .map_err(|e| WaywiError::DbusError(e.to_string()))?;
            Ok(req_path.to_string())
        })
    }

    /// Closes an active portal `org.freedesktop.portal.Request` handle (such as an inhibitor from
    /// [`Self::inhibit_blocking`]).
    pub fn close_request(&self, request_handle: &str) -> Result<()> {
        let path = validate_object_path(request_handle, "portal request handle")?;
        self.0.call_lane_method_unit(
            DbusActionLane::Desktop,
            DbusBus::Session,
            PORTAL_DEST,
            &path,
            PORTAL_REQUEST_IFACE,
            "Close",
        )
    }

    /// Closes an active portal `org.freedesktop.portal.Session` handle (such as a ScreenCast
    /// session from [`Self::screencast_start_blocking`]).
    pub fn close_session(&self, session_handle: &str) -> Result<()> {
        let path = validate_object_path(session_handle, "portal session handle")?;
        self.0.call_lane_method_unit(
            DbusActionLane::Desktop,
            DbusBus::Session,
            PORTAL_DEST,
            &path,
            PORTAL_SESSION_IFACE,
            "Close",
        )
    }

    /// Moves a local file to the user's trash via `org.freedesktop.portal.Trash.TrashFile`,
    /// returning `true` on success (`result == 1`).
    pub fn trash_file_blocking(&self, path: &Path) -> Result<bool> {
        let file = File::open(path).map_err(|e| {
            WaywiError::DbusError(format!("TrashFile cannot open `{}`: {e}", path.display()))
        })?;
        let conn = self.0.async_connection(DbusBus::Session)?;
        futures_lite::future::block_on(async move {
            let fd = Fd::from(&file);
            let reply = conn
                .call_method(
                    Some(PORTAL_DEST),
                    PORTAL_PATH,
                    Some(PORTAL_TRASH_IFACE),
                    "TrashFile",
                    &(fd,),
                )
                .await
                .map_err(|e| WaywiError::DbusError(format!("portal TrashFile failed: {e}")))?;
            let status: u32 = reply
                .body()
                .deserialize()
                .map_err(|e| WaywiError::DbusError(e.to_string()))?;
            Ok(status == 1)
        })
    }

    /// Requests background execution / autostart permission via
    /// `org.freedesktop.portal.Background.RequestBackground`.
    pub fn request_background_blocking(
        &self,
        options: &PortalBackgroundOptions,
    ) -> Result<Option<PortalBackgroundResult>> {
        let conn = self.0.async_connection(DbusBus::Session)?;
        let opts = options.clone();
        futures_lite::future::block_on(async move {
            let Some(results) = call_portal_request(
                &conn,
                PORTAL_BACKGROUND_IFACE,
                "RequestBackground",
                |token| {
                    let mut m: HashMap<&str, Value<'_>> = HashMap::new();
                    m.insert("handle_token", Value::from(token));
                    m.insert("autostart", Value::from(opts.autostart));
                    m.insert("dbus-activatable", Value::from(opts.dbus_activatable));
                    if let Some(ref reason) = opts.reason {
                        m.insert("reason", Value::from(reason.as_str()));
                    }
                    if !opts.commandline.is_empty() {
                        m.insert("commandline", Value::from(opts.commandline));
                    }
                    (opts.parent_window.as_str(), m)
                },
            )
            .await?
            else {
                return Ok(None);
            };
            Ok(Some(PortalBackgroundResult {
                background: super::zvariant_helpers::dict_bool(&results, "background")
                    .unwrap_or(false),
                autostart: super::zvariant_helpers::dict_bool(&results, "autostart")
                    .unwrap_or(false),
            }))
        })
    }

    /// Runs the full `org.freedesktop.portal.ScreenCast` handshake (`CreateSession` ->
    /// `SelectSources` -> `Start`), returning PipeWire stream node IDs and the session handle
    /// (or `Ok(None)` if the user cancelled).
    pub fn screencast_start_blocking(
        &self,
        options: &PortalScreenCastOptions,
    ) -> Result<Option<PortalScreenCastResult>> {
        let conn = self.0.async_connection(DbusBus::Session)?;
        let opts = options.clone();
        futures_lite::future::block_on(async move {
            let session_tok = next_portal_token("sc_sess");
            let Some(create_res) =
                call_portal_request(&conn, PORTAL_SCREENCAST_IFACE, "CreateSession", |token| {
                    let mut m: HashMap<&str, Value<'_>> = HashMap::new();
                    m.insert("handle_token", Value::from(token));
                    m.insert("session_handle_token", Value::from(session_tok.as_str()));
                    (m,)
                })
                .await?
            else {
                return Ok(None);
            };
            let Some(session_handle) = dict_str(&create_res, "session_handle") else {
                return Err(WaywiError::DbusError(
                    "portal ScreenCast.CreateSession omitted session_handle".into(),
                ));
            };
            let sess_obj = OwnedObjectPath::try_from(session_handle.clone())
                .map_err(|e| WaywiError::DbusError(e.to_string()))?;

            let sel =
                call_portal_request(&conn, PORTAL_SCREENCAST_IFACE, "SelectSources", |token| {
                    let mut m: HashMap<&str, Value<'_>> = HashMap::new();
                    m.insert("handle_token", Value::from(token));
                    m.insert("types", Value::from(opts.types.bits()));
                    m.insert("multiple", Value::from(opts.multiple));
                    m.insert("cursor_mode", Value::from(opts.cursor_mode as u32));
                    (&sess_obj, m)
                })
                .await?;
            if sel.is_none() {
                let _ = conn
                    .call_method(
                        Some(PORTAL_DEST),
                        session_handle.as_str(),
                        Some(PORTAL_SESSION_IFACE),
                        "Close",
                        &(),
                    )
                    .await;
                return Ok(None);
            }

            let Some(start_res) =
                call_portal_request(&conn, PORTAL_SCREENCAST_IFACE, "Start", |token| {
                    let mut m: HashMap<&str, Value<'_>> = HashMap::new();
                    m.insert("handle_token", Value::from(token));
                    (&sess_obj, opts.parent_window.as_str(), m)
                })
                .await?
            else {
                let _ = conn
                    .call_method(
                        Some(PORTAL_DEST),
                        session_handle.as_str(),
                        Some(PORTAL_SESSION_IFACE),
                        "Close",
                        &(),
                    )
                    .await;
                return Ok(None);
            };

            let streams = parse_screencast_streams(start_res.get("streams"));
            Ok(Some(PortalScreenCastResult {
                session_handle,
                streams,
            }))
        })
    }
}

fn next_portal_token(prefix: &str) -> String {
    let seq = NEXT_PORTAL_TOKEN.fetch_add(1, Ordering::Relaxed);
    format!("waywi_{prefix}_{}_{}", std::process::id(), seq)
}

fn portal_request_path(conn: &zbus::Connection, token: &str) -> Result<String> {
    let unique = conn
        .unique_name()
        .ok_or_else(|| WaywiError::DbusError("session bus has no unique name".into()))?;
    let clean = unique.as_str().trim_start_matches(':').replace('.', "_");
    Ok(format!(
        "/org/freedesktop/portal/desktop/request/{clean}/{token}"
    ))
}

/// Executes a standard XDG Desktop Portal `Request` method, pre-subscribing to
/// `org.freedesktop.portal.Request.Response` before invoking the method to eliminate races.
/// Returns `Ok(Some(results))` on `response == 0`, `Ok(None)` on user cancellation (`1`), or
/// `Err` on portal failure (`>= 2`).
async fn call_portal_request<B, F>(
    conn: &zbus::Connection,
    interface: &str,
    method: &str,
    build_body: F,
) -> Result<Option<HashMap<String, OwnedValue>>>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
    F: FnOnce(String) -> B,
{
    let token = next_portal_token("req");
    let expected_path = portal_request_path(conn, &token)?;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(PORTAL_DEST)
        .and_then(|b| b.interface(PORTAL_REQUEST_IFACE))
        .and_then(|b| b.member("Response"))
        .and_then(|b| b.path(expected_path.as_str()))
        .map_err(|e| WaywiError::DbusError(e.to_string()))?
        .build();
    let mut stream = zbus::MessageStream::for_match_rule(rule, conn, Some(4))
        .await
        .map_err(|e| WaywiError::DbusError(e.to_string()))?;

    let body = build_body(token);
    let reply = conn
        .call_method(
            Some(PORTAL_DEST),
            PORTAL_PATH,
            Some(interface),
            method,
            &body,
        )
        .await
        .map_err(|e| WaywiError::DbusError(format!("portal {interface}.{method} failed: {e}")))?;
    let _actual_path: OwnedObjectPath = reply
        .body()
        .deserialize()
        .map_err(|e| WaywiError::DbusError(e.to_string()))?;

    let Some(Ok(msg)) = stream.next().await else {
        return Err(WaywiError::DbusError(format!(
            "portal {interface}.{method} response stream closed"
        )));
    };
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg
        .body()
        .deserialize()
        .map_err(|e| WaywiError::DbusError(e.to_string()))?;
    match code {
        0 => Ok(Some(results)),
        1 => Ok(None),
        other => Err(WaywiError::DbusError(format!(
            "portal {interface}.{method} failed with response code {other}"
        ))),
    }
}

pub(crate) fn parse_screencast_streams(val: Option<&OwnedValue>) -> Vec<PortalScreenCastStream> {
    let Some(Value::Array(arr)) = val.map(|v| unwrap_value(v)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in arr.iter() {
        let Value::Structure(st) = unwrap_value(item) else {
            continue;
        };
        let fields = st.fields();
        if fields.len() < 2 {
            continue;
        }
        let Some(node_id) = value_as_u32(&fields[0]) else {
            continue;
        };
        let mut stream = PortalScreenCastStream {
            node_id,
            ..Default::default()
        };
        if let Value::Dict(dict) = unwrap_value(&fields[1]) {
            let mut map: HashMap<String, OwnedValue> = HashMap::new();
            for (k, v) in dict.iter() {
                if let Value::Str(ks) = unwrap_value(k)
                    && let Ok(ov) = OwnedValue::try_from(v.clone())
                {
                    map.insert(ks.to_string(), ov);
                }
            }
            stream.source_type = dict_u32(&map, "source_type");
            if let Some(Value::Structure(sz)) = map.get("size").map(|v| unwrap_value(v))
                && sz.fields().len() == 2
                && let (Value::I32(w), Value::I32(h)) =
                    (unwrap_value(&sz.fields()[0]), unwrap_value(&sz.fields()[1]))
            {
                stream.size = Some((*w, *h));
            }
            if let Some(Value::Structure(pos)) = map.get("position").map(|v| unwrap_value(v))
                && pos.fields().len() == 2
                && let (Value::I32(x), Value::I32(y)) = (
                    unwrap_value(&pos.fields()[0]),
                    unwrap_value(&pos.fields()[1]),
                )
            {
                stream.position = Some((*x, *y));
            }
        }
        out.push(stream);
    }
    out
}

async fn read_portal_settings(conn: &zbus::Connection) -> Result<PortalSettingsSnapshot> {
    let reply = conn
        .call_method(
            Some(PORTAL_DEST),
            PORTAL_PATH,
            Some(PORTAL_SETTINGS_IFACE),
            "ReadAll",
            &(&[APPEARANCE_NAMESPACE][..],),
        )
        .await
        .map_err(|e| WaywiError::DbusError(format!("portal Settings.ReadAll failed: {e}")))?;
    let all: HashMap<String, HashMap<String, OwnedValue>> = reply
        .body()
        .deserialize()
        .map_err(|e| WaywiError::DbusError(e.to_string()))?;
    Ok(parse_appearance_map(all.get(APPEARANCE_NAMESPACE)))
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
        |conn| async move { read_portal_settings(&conn).await.ok() },
    )
}

fn value_as_u32(val: &Value<'_>) -> Option<u32> {
    match unwrap_value(val) {
        Value::U32(v) => Some(*v),
        Value::I32(v) => u32::try_from(*v).ok(),
        Value::U64(v) => u32::try_from(*v).ok(),
        Value::U16(v) => Some(u32::from(*v)),
        Value::U8(v) => Some(u32::from(*v)),
        _ => None,
    }
}

fn value_as_accent_color(val: &Value<'_>) -> Option<PortalAccentColor> {
    let Value::Structure(st) = unwrap_value(val) else {
        return None;
    };
    let fields = st.fields();
    if fields.len() != 3 {
        return None;
    }
    let as_f64 = |v: &Value<'_>| match unwrap_value(v) {
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

    #[test]
    fn parses_screencast_streams_and_filter_builders() {
        let filter = PortalFileFilter::new("Images")
            .glob("*.png")
            .mime("image/webp");
        assert_eq!(filter.patterns.len(), 2);
        let chooser = PortalFileChooserOptions::new("Pick File")
            .multiple(true)
            .filter(filter);
        assert!(chooser.multiple);
        assert_eq!(chooser.filters.len(), 1);

        let mut props: HashMap<String, Value<'_>> = HashMap::new();
        props.insert("source_type".into(), Value::from(1u32));
        let sz = zbus::zvariant::StructureBuilder::new()
            .add_field(1920i32)
            .add_field(1080i32)
            .build()
            .unwrap();
        props.insert("size".into(), Value::from(sz));
        let entry = zbus::zvariant::StructureBuilder::new()
            .add_field(42u32)
            .add_field(props)
            .build()
            .unwrap();
        let mut arr = zbus::zvariant::Array::new(entry.signature());
        arr.append(Value::from(entry)).unwrap();
        let ov = OwnedValue::try_from(Value::Array(arr)).unwrap();
        let streams = parse_screencast_streams(Some(&ov));
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].node_id, 42);
        assert_eq!(streams[0].source_type, Some(1));
        assert_eq!(streams[0].size, Some((1920, 1080)));
    }
}
