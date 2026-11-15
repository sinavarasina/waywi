use std::any::TypeId;
use std::hash::{Hash, Hasher};

/// Stable key identifying a subscription across `subscriptions(state)` updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionKey(pub u64);

impl SubscriptionKey {
    #[must_use]
    pub fn from_hashable<T: Hash + 'static>(value: &T) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        TypeId::of::<T>().hash(&mut hasher);
        value.hash(&mut hasher);
        Self(hasher.finish())
    }
}

/// Conversion trait allowing strings, integers, and tuples like `("osd-hide", seq)` to key
/// subscriptions.
pub trait IntoSubscriptionKey {
    fn into_subscription_key(self) -> SubscriptionKey;
}

impl<T: Hash + 'static> IntoSubscriptionKey for T {
    fn into_subscription_key(self) -> SubscriptionKey {
        SubscriptionKey::from_hashable(&self)
    }
}
