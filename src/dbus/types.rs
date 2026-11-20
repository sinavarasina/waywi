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
    ObjectPath(String),
    Bool(bool),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    F64(f64),
    Array(Vec<DbusValue>),
    Dict(HashMap<String, DbusValue>),
    /// D-Bus structure/tuple (`(...)`), e.g. `(sv)` when paired with [`DbusValue::Variant`].
    Struct(Vec<DbusValue>),
    /// Explicit D-Bus variant container (`v`), required for signatures such as `(sv)` / `a(sv)`.
    Variant(Box<DbusValue>),
}

impl DbusValue {
    /// Wraps `value` in an explicit [`DbusValue::Variant`] (`v`).
    #[must_use]
    pub fn variant(value: Self) -> Self {
        Self::Variant(Box::new(value))
    }

    /// Unwraps any nested [`DbusValue::Variant`] layers to inspect the underlying concrete value.
    #[must_use]
    pub fn unwrap_variant(&self) -> &Self {
        let mut cur = self;
        while let Self::Variant(inner) = cur {
            cur = inner;
        }
        cur
    }

    /// Returns the inner value when this value is [`DbusValue::Variant`].
    #[must_use]
    pub fn as_variant(&self) -> Option<&Self> {
        match self {
            Self::Variant(inner) => Some(inner),
            _ => None,
        }
    }

    /// Returns field slice when this value (or its inner variant) is [`DbusValue::Struct`].
    #[must_use]
    pub fn as_struct(&self) -> Option<&[Self]> {
        match self.unwrap_variant() {
            Self::Struct(fields) => Some(fields.as_slice()),
            _ => None,
        }
    }

    /// Returns string content for `String` and `ObjectPath` values.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self.unwrap_variant() {
            Self::String(s) | Self::ObjectPath(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Returns the D-Bus object path string when this value is [`DbusValue::ObjectPath`].
    #[must_use]
    pub fn as_object_path(&self) -> Option<&str> {
        match self.unwrap_variant() {
            Self::ObjectPath(p) => Some(p.as_str()),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self.unwrap_variant() {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_i32(&self) -> Option<i32> {
        match self.unwrap_variant() {
            Self::I32(v) => Some(*v),
            Self::I64(v) => i32::try_from(*v).ok(),
            Self::U32(v) => i32::try_from(*v).ok(),
            Self::U64(v) => i32::try_from(*v).ok(),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_u32(&self) -> Option<u32> {
        match self.unwrap_variant() {
            Self::U32(v) => Some(*v),
            Self::U64(v) => u32::try_from(*v).ok(),
            Self::I32(v) => u32::try_from(*v).ok(),
            Self::I64(v) => u32::try_from(*v).ok(),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self.unwrap_variant() {
            Self::I64(v) => Some(*v),
            Self::I32(v) => Some(i64::from(*v)),
            Self::U32(v) => Some(i64::from(*v)),
            Self::U64(v) => i64::try_from(*v).ok(),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        match self.unwrap_variant() {
            Self::U64(v) => Some(*v),
            Self::U32(v) => Some(u64::from(*v)),
            Self::I32(v) if *v >= 0 => Some(*v as u64),
            Self::I64(v) if *v >= 0 => Some(*v as u64),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self.unwrap_variant() {
            Self::F64(v) => Some(*v),
            Self::I32(v) => Some(f64::from(*v)),
            Self::U32(v) => Some(f64::from(*v)),
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
