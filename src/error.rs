use std::fmt;

use crate::capabilities::Capability;

/// Convenience result alias used across the `waywi` crate.
pub type Result<T> = std::result::Result<T, WaywiError>;

/// Broad error classification matching WayWi's error-handling policy.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// A protocol, output, file, or service is absent.
    NotAvailable,
    /// The active backend or build features cannot implement the requested operation.
    NotSupported,
    /// A service is temporarily unreachable and may recover later.
    TemporarilyUnavailable,
    /// The operation was rejected due to permissions or compositor policy.
    PermissionDenied,
    /// The backend or service returned an operational error.
    OperationFailed,
    /// The caller invoked an operation in an invalid lifecycle state or with invalid parameters.
    InvalidState,
    /// An operation did not complete within its deadline.
    Timeout,
    /// The runtime is shutting down.
    Shutdown,
}

/// Top-level error type returned by WayWi runtime, asset, rendering, and integration APIs.
#[non_exhaustive]
#[derive(Debug)]
pub enum WaywiError {
    /// Connecting to or communicating with the Wayland display server failed.
    WaylandConnection(String),
    /// A required Wayland protocol, Cargo feature, or system capability is unavailable.
    UnsupportedCapability(Capability),
    /// The requested output does not exist or was disconnected.
    OutputUnavailable(String),
    /// Surface creation, configuration, or buffer submission failed.
    SurfaceError(String),
    /// Software or backend rendering failed.
    RenderError(String),
    /// Decoding an image, SVG, GIF, or font asset failed.
    AssetDecode(String),
    /// The requested asset format is not supported or its Cargo feature is disabled.
    UnsupportedAssetFormat(String),
    /// A system data provider or external desktop service is unavailable.
    ProviderUnavailable(String),
    /// Permission was denied by the OS, compositor, or IPC endpoint.
    PermissionDenied(String),
    /// Compositor or control IPC transport or payload parsing failed.
    IpcError(String),
    /// D-Bus connection, method invocation, or signal subscription failed.
    DbusError(String),
    /// PipeWire or audio backend operation failed.
    AudioError(String),
    /// Geometry, bounds, or coordinate parameters were invalid.
    InvalidGeometry(String),
    /// An operation was called at an invalid runtime or lifecycle stage.
    InvalidState(String),
    /// An IPC or service request timed out.
    Timeout(String),
    /// Underlying OS I/O error.
    Io(std::io::Error),
    /// Application runtime has shut down.
    Shutdown,
    /// Custom application-defined error propagated through `update`.
    Custom(String),
}

impl WaywiError {
    /// Returns the coarse [`ErrorKind`] for programmatic error handling.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::WaylandConnection(_)
            | Self::OutputUnavailable(_)
            | Self::ProviderUnavailable(_) => ErrorKind::NotAvailable,
            Self::UnsupportedCapability(_) | Self::UnsupportedAssetFormat(_) => {
                ErrorKind::NotSupported
            }
            Self::PermissionDenied(_) => ErrorKind::PermissionDenied,
            Self::Timeout(_) => ErrorKind::Timeout,
            Self::InvalidGeometry(_) | Self::InvalidState(_) => ErrorKind::InvalidState,
            Self::Shutdown => ErrorKind::Shutdown,
            Self::Io(err) => match err.kind() {
                std::io::ErrorKind::NotFound => ErrorKind::NotAvailable,
                std::io::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
                std::io::ErrorKind::TimedOut => ErrorKind::Timeout,
                std::io::ErrorKind::WouldBlock
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::Interrupted => ErrorKind::TemporarilyUnavailable,
                _ => ErrorKind::OperationFailed,
            },
            Self::SurfaceError(_)
            | Self::RenderError(_)
            | Self::AssetDecode(_)
            | Self::IpcError(_)
            | Self::DbusError(_)
            | Self::AudioError(_)
            | Self::Custom(_) => ErrorKind::OperationFailed,
        }
    }
}

impl fmt::Display for WaywiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WaylandConnection(msg) => write!(f, "Wayland connection error: {msg}"),
            Self::UnsupportedCapability(cap) => write!(f, "unsupported capability: {cap:?}"),
            Self::OutputUnavailable(msg) => write!(f, "output unavailable: {msg}"),
            Self::SurfaceError(msg) => write!(f, "surface error: {msg}"),
            Self::RenderError(msg) => write!(f, "render error: {msg}"),
            Self::AssetDecode(msg) => write!(f, "asset decode error: {msg}"),
            Self::UnsupportedAssetFormat(msg) => write!(f, "unsupported asset format: {msg}"),
            Self::ProviderUnavailable(msg) => write!(f, "provider unavailable: {msg}"),
            Self::PermissionDenied(msg) => write!(f, "permission denied: {msg}"),
            Self::IpcError(msg) => write!(f, "IPC error: {msg}"),
            Self::DbusError(msg) => write!(f, "D-Bus error: {msg}"),
            Self::AudioError(msg) => write!(f, "audio error: {msg}"),
            Self::InvalidGeometry(msg) => write!(f, "invalid geometry: {msg}"),
            Self::InvalidState(msg) => write!(f, "invalid state: {msg}"),
            Self::Timeout(msg) => write!(f, "operation timed out: {msg}"),
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::Shutdown => write!(f, "runtime shut down"),
            Self::Custom(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for WaywiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for WaywiError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
