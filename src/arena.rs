//! Internal scoped double-buffered bump arena for zero-allocation steady-state `view()` and
//! `subscriptions()` passes without exposing `'arena` lifetimes in user APIs.

pub(crate) mod pool;
pub(crate) mod shared_fn;
pub(crate) mod vec;

pub(crate) use pool::FrameArenaPool;
pub(crate) use shared_fn::FrameSharedFn;
pub(crate) use vec::FrameVec;
pub use vec::FrameVecIntoIter;

/// Hashes `value` with its concrete `TypeId` unless `value` already has type `K`,
/// preventing double-hashing when a pre-hashed `*Key` is passed to `Into*Key`.
#[inline]
pub(crate) fn hash_typed_key<T: std::hash::Hash + 'static, K: Copy + 'static>(
    value: &T,
    raw: fn(&K) -> u64,
) -> u64 {
    if let Some(existing) = (value as &dyn std::any::Any).downcast_ref::<K>() {
        return raw(existing);
    }
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::any::TypeId::of::<T>().hash(&mut hasher);
    value.hash(&mut hasher);
    hasher.finish()
}
