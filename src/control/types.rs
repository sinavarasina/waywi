//! External control triggers (POSIX signals and Unix control socket subscriptions).

use std::path::{Path, PathBuf};

use crate::arena::FrameSharedFn;
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

use super::socket::{ControlRequest, resolve_socket_path};

/// Supported POSIX signals for lightweight external compositor-keybind triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    Usr1,
    Usr2,
}

/// External control triggers (POSIX signals & single-instance Unix socket).
pub struct Control;

impl Control {
    /// Subscribes to a POSIX signal (`SIGUSR1` / `SIGUSR2`) keyed by `key` and emits `msg` when
    /// received.
    pub fn signal<M: 'static>(
        key: impl IntoSubscriptionKey,
        signal: Signal,
        msg: M,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::ControlSignal {
                key: key.into_subscription_key(),
                signal,
                msg,
            },
        }
    }

    /// Listens on a single-instance control Unix socket under `$XDG_RUNTIME_DIR/<name>.sock`
    /// (or an explicit path) and delivers [`ControlRequest`] values to `map`.
    ///
    /// Applications that ignore a request's reply handle simply drop it; the write half closes
    /// immediately so [`send_command_with_reply`](super::send_command_with_reply) callers see EOF
    /// without waiting.
    pub fn socket<M: 'static>(
        name: impl Into<String>,
        map: impl Fn(ControlRequest) -> M + 'static,
    ) -> Subscription<M> {
        let name = name.into();
        let resolved = resolve_socket_path(Path::new(&name));
        Subscription {
            spec: SubscriptionSpec::ControlSocket {
                key: resolved.clone().into_subscription_key(),
                path: resolved,
                map: FrameSharedFn::new(map),
            },
        }
    }

    /// Listens on the application's default single-instance control Unix socket (resolved by the
    /// runtime from `App::control_socket` or `App::name`) and delivers [`ControlRequest`] values
    /// to `map`.
    pub fn default_socket<M: 'static>(
        map: impl Fn(ControlRequest) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::ControlSocket {
                key: "control-default-socket".into_subscription_key(),
                path: PathBuf::new(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}
