use std::collections::HashMap;

use crate::arena::FrameSharedFn;
use crate::subscription::{IntoSubscriptionKey, Subscription, SubscriptionSpec};

/// Target D-Bus bus selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DbusBus {
    #[default]
    Session,
    System,
}

/// Typed D-Bus value wrapper avoiding raw stringly-typed APIs where stable.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum DbusValue {
    String(String),
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    Array(Vec<DbusValue>),
    Dict(HashMap<String, DbusValue>),
}

impl DbusValue {
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::I64(v) => Some(*v),
            Self::U64(v) => i64::try_from(*v).ok(),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::U64(v) => Some(*v),
            Self::I64(v) if *v >= 0 => Some(*v as u64),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::F64(v) => Some(*v),
            Self::I64(v) => Some(*v as f64),
            Self::U64(v) => Some(*v as f64),
            _ => None,
        }
    }
}

/// Signal payload emitted by a [`Dbus::subscribe_signal`] subscription.
#[derive(Debug, Clone, PartialEq)]
pub struct DbusSignalEvent {
    pub sender: Option<String>,
    pub path: Option<String>,
    pub interface: String,
    pub member: String,
    pub args: Vec<DbusValue>,
}

/// D-Bus signal subscription builder.
pub struct Dbus;

impl Dbus {
    /// Subscribes to a D-Bus signal on the session or system bus.
    pub fn subscribe_signal<M: 'static>(
        bus: DbusBus,
        interface: impl Into<String>,
        member: impl Into<String>,
        path: Option<String>,
        map: impl Fn(DbusSignalEvent) -> M + 'static,
    ) -> Subscription<M> {
        let iface = interface.into();
        let mem = member.into();
        let key = (bus, iface.clone(), mem.clone(), path.clone()).into_subscription_key();
        Subscription {
            spec: SubscriptionSpec::DbusSignal {
                key,
                bus,
                interface: iface,
                member: mem,
                path,
                map: FrameSharedFn::new(map),
            },
        }
    }
}
