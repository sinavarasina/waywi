//! Internal scoped double-buffered bump arena for zero-allocation steady-state `view()` and
//! `subscriptions()` passes without exposing `'arena` lifetimes in user APIs.

pub(crate) mod pool;
pub(crate) mod shared_fn;
pub(crate) mod vec;

pub(crate) use pool::FrameArenaPool;
pub(crate) use shared_fn::FrameSharedFn;
pub(crate) use vec::FrameVec;
pub use vec::FrameVecIntoIter;
