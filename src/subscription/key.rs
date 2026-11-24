use std::hash::Hash;

/// Stable key identifying a subscription across `subscriptions(state)` updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionKey(pub u64);

impl SubscriptionKey {
    #[must_use]
    pub fn from_hashable<T: Hash + 'static>(value: &T) -> Self {
        Self(crate::arena::hash_typed_key::<T, Self>(value, |k| k.0))
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
