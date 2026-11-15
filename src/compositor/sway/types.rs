use serde::Deserialize;

use crate::arena::FrameSharedFn;
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

/// Target workspace reference for Sway commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkspaceRef<'a> {
    Name(&'a str),
    Number(i32),
}

/// Sway workspace snapshot (`GET_WORKSPACES` or `workspace` event payload).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SwayWorkspace {
    pub id: i64,
    pub num: i32,
    pub name: String,
    #[serde(default)]
    pub visible: bool,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub urgent: bool,
    #[serde(default)]
    pub output: String,
}

/// Sway output metadata (`GET_OUTPUTS`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SwayOutput {
    pub name: String,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub scale: Option<f32>,
}

/// Sway version metadata (`GET_VERSION`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SwayVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub human_readable: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct CommandOutcome {
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
}

/// Incremental Sway event parsed directly from subscribed `i3-ipc` event frames.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwayEvent {
    Workspace {
        change: String,
        current: Option<SwayWorkspace>,
        old: Option<SwayWorkspace>,
    },
    Output {
        change: String,
    },
    Window {
        change: String,
        container_id: Option<i64>,
        name: Option<String>,
        app_id: Option<String>,
    },
    Mode {
        change: String,
    },
    Shutdown {
        reason: String,
    },
    Disconnected,
    Reconnected,
    Unknown {
        event_type: u32,
        payload: String,
    },
}

/// Sway IPC subscription helper.
pub struct Sway;

impl Sway {
    pub fn subscribe<M: 'static>(map: impl Fn(SwayEvent) -> M + 'static) -> Subscription<M> {
        Subscription {
            spec: SubscriptionSpec::SwayEvent {
                key: "sway-ipc-subscribe".into_subscription_key(),
                map: FrameSharedFn::new(map),
            },
        }
    }
}
