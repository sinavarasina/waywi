//! External control triggers (POSIX signals and Unix control socket subscriptions).

use std::path::Path;

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
    /// Subscribes to a POSIX signal (`SIGUSR1` / `SIGUSR2`) and emits `msg` when received.
    pub fn signal<M: 'static>(signal: Signal, msg: M) -> Subscription<M> {
        let key = match signal {
            Signal::Usr1 => "control-sigusr1",
            Signal::Usr2 => "control-sigusr2",
        };
        Subscription {
            spec: SubscriptionSpec::ControlSignal {
                key: key.into_subscription_key(),
                signal,
                msg,
            },
        }
    }

    /// Listens on a single-instance control Unix socket under `$XDG_RUNTIME_DIR/<name>.sock`
    /// (or an explicit path) and emits `map(command)` into `update`.
    pub fn socket<M: 'static>(
        socket_name_or_path: impl AsRef<Path>,
        map: impl Fn(String) -> Option<M> + 'static,
    ) -> Subscription<M> {
        let resolved = resolve_socket_path(socket_name_or_path.as_ref());
        Subscription {
            spec: SubscriptionSpec::ControlSocket {
                key: resolved.clone().into_subscription_key(),
                socket_path: resolved,
                map: FrameSharedFn::new(map),
            },
        }
    }

    /// Listens on a single-instance control Unix socket and yields [`ControlRequest`] values so
    /// the application can send one-shot replies back to [`send_command_with_reply`](super::send_command_with_reply)
    /// callers (one newline-delimited request and response per connection).
    pub fn socket_requests<M: 'static>(
        socket_name_or_path: impl AsRef<Path>,
        map: impl Fn(ControlRequest) -> Option<M> + 'static,
    ) -> Subscription<M> {
        let resolved = resolve_socket_path(socket_name_or_path.as_ref());
        Subscription {
            spec: SubscriptionSpec::ControlSocketRequest {
                key: resolved.clone().into_subscription_key(),
                socket_path: resolved,
                map: FrameSharedFn::new(map),
            },
        }
    }
}
