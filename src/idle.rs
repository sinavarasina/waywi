use std::time::Duration;

use crate::arena::FrameSharedFn;
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

/// Wayland seat user-idle notification event (`ext-idle-notify-v1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdleEvent {
    /// The seat has been inactive for at least the configured timeout duration.
    Idled(Duration),
    /// User activity resumed after an idle period.
    Resumed(Duration),
}

/// Builder for Wayland `ext-idle-notify-v1` user-idle subscriptions.
pub struct Idle;

impl Idle {
    /// Subscribes to compositor idle notifications obeying active idle inhibitors
    /// (such as fullscreen video playback).
    pub fn notify<M: 'static>(
        timeout: Duration,
        map: impl Fn(IdleEvent) -> M + 'static,
    ) -> Subscription<M> {
        Self::build(timeout, false, map)
    }

    /// Subscribes to input-only idle notifications that ignore idle inhibitors when supported
    /// by `ext_idle_notifier_v1` v2+, falling back to normal idle notifications on v1.
    pub fn input_notify<M: 'static>(
        timeout: Duration,
        map: impl Fn(IdleEvent) -> M + 'static,
    ) -> Subscription<M> {
        Self::build(timeout, true, map)
    }

    fn build<M: 'static>(
        timeout: Duration,
        input_only: bool,
        map: impl Fn(IdleEvent) -> M + 'static,
    ) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::IdleNotify {
                key: ("wayland:idle", timeout.as_millis() as u64, input_only)
                    .into_subscription_key(),
                timeout,
                input_only,
                map: FrameSharedFn::new(map),
            },
        }
    }
}
