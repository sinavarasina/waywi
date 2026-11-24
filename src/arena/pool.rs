use std::alloc::Layout;
use std::cell::{Cell, RefCell, UnsafeCell};
use std::ptr::NonNull;
use std::rc::Rc;

use bumpalo::Bump;

/// Default slab reservation (32 KiB) and retain cap (256 KiB) for steady-state frames.
const DEFAULT_SLAB_BYTES: usize = 32 * 1024;
const DEFAULT_MAX_RETAIN_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FrameArenaStats {
    pub arena_allocs: usize,
    pub heap_fallbacks: usize,
    pub bytes_reserved: usize,
}

pub(crate) struct FrameSlab {
    bump: UnsafeCell<Bump>,
    arena_allocs: Cell<usize>,
}

impl FrameSlab {
    fn new(initial_bytes: usize) -> Self {
        Self {
            bump: UnsafeCell::new(Bump::with_capacity(initial_bytes)),
            arena_allocs: Cell::new(0),
        }
    }

    #[inline]
    pub(super) fn alloc_layout(&self, layout: Layout) -> Option<NonNull<u8>> {
        // SAFETY: While `ACTIVE_SLAB` is installed, no `&mut Bump` exists. `Bump::alloc_layout`
        // takes `&self` and never invalidates previously handed-out pointers until `reset()`.
        let bump = unsafe { &*self.bump.get() };
        self.arena_allocs.set(self.arena_allocs.get() + 1);
        Some(bump.alloc_layout(layout))
    }

    #[inline]
    fn allocated_bytes(&self) -> usize {
        // SAFETY: Read-only query on Bump when no mutable borrow is active.
        unsafe { &*self.bump.get() }.allocated_bytes()
    }
}

thread_local! {
    static ACTIVE_SLAB: RefCell<Option<Rc<FrameSlab>>> = const { RefCell::new(None) };
}

/// Double-buffered reusable frame arena owned by `App` and the headless test harness.
pub(crate) struct FrameArenaPool {
    slabs: [Rc<FrameSlab>; 2],
    next_index: usize,
    heap_fallbacks: usize,
    initial_bytes: usize,
    max_retain_bytes: usize,
}

impl FrameArenaPool {
    pub(crate) fn new() -> Self {
        Self::with_limits(DEFAULT_SLAB_BYTES, DEFAULT_MAX_RETAIN_BYTES)
    }

    pub(crate) fn with_limits(initial_bytes: usize, max_retain_bytes: usize) -> Self {
        let initial_bytes = initial_bytes.max(4096);
        let max_retain_bytes = max_retain_bytes.max(initial_bytes);
        Self {
            slabs: [
                Rc::new(FrameSlab::new(initial_bytes)),
                Rc::new(FrameSlab::new(initial_bytes)),
            ],
            next_index: 0,
            heap_fallbacks: 0,
            initial_bytes,
            max_retain_bytes,
        }
    }

    /// Enters a scoped frame pass, resetting the next free slab in $O(1)$ if unshared.
    ///
    /// Slabs that grew past `max_retain_bytes` during a transient spike frame are replaced with
    /// fresh `initial_bytes` slabs to avoid permanent RSS growth.
    pub(crate) fn enter_scope(&mut self) -> FrameScopeGuard {
        let idx = self.next_index;
        self.next_index = 1 - self.next_index;

        if let Some(slab_mut) = Rc::get_mut(&mut self.slabs[idx]) {
            if slab_mut.allocated_bytes() > self.max_retain_bytes {
                *slab_mut.bump.get_mut() = Bump::with_capacity(self.initial_bytes);
            } else {
                slab_mut.bump.get_mut().reset();
            }
            slab_mut.arena_allocs.set(0);
        } else {
            // Soundness fallback: if a caller retained a `Surface` or `Element` outside the
            // view pass across frames, allocate a fresh slab rather than causing use-after-free.
            self.slabs[idx] = Rc::new(FrameSlab::new(self.initial_bytes));
            self.heap_fallbacks += 1;
        }

        let slab = Rc::clone(&self.slabs[idx]);
        let previous = ACTIVE_SLAB.with(|slot| slot.borrow_mut().replace(slab));
        FrameScopeGuard { previous }
    }

    /// Shrinks both unshared slabs back down to `initial_bytes` immediately.
    pub(crate) fn trim(&mut self) {
        for slab in &mut self.slabs {
            if let Some(slab_mut) = Rc::get_mut(slab) {
                if slab_mut.allocated_bytes() > self.initial_bytes {
                    *slab_mut.bump.get_mut() = Bump::with_capacity(self.initial_bytes);
                } else {
                    slab_mut.bump.get_mut().reset();
                }
                slab_mut.arena_allocs.set(0);
            }
        }
    }

    pub(crate) fn last_stats(&self) -> FrameArenaStats {
        let prev_idx = 1 - self.next_index;
        let slab = &self.slabs[prev_idx];
        FrameArenaStats {
            arena_allocs: slab.arena_allocs.get(),
            heap_fallbacks: self.heap_fallbacks,
            bytes_reserved: slab.allocated_bytes(),
        }
    }
}

impl Default for FrameArenaPool {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) struct FrameScopeGuard {
    previous: Option<Rc<FrameSlab>>,
}

impl Drop for FrameScopeGuard {
    fn drop(&mut self) {
        ACTIVE_SLAB.with(|slot| {
            *slot.borrow_mut() = self.previous.take();
        });
    }
}

#[inline]
pub(super) fn current_slab() -> Option<Rc<FrameSlab>> {
    ACTIVE_SLAB.with(|slot| slot.borrow().as_ref().map(Rc::clone))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::{FrameSharedFn, FrameVec};

    #[test]
    fn arena_steady_state_zero_heap_fallbacks() {
        let mut pool = FrameArenaPool::new();
        for _ in 0..10 {
            let _guard = pool.enter_scope();
            let mut v = FrameVec::new();
            for i in 0..64 {
                v.push(i);
            }
            assert!(v.is_arena_backed());
            assert_eq!(v.len(), 64);

            let zst = FrameSharedFn::new(|x: i32| x + 1);
            assert_eq!(zst.call(41), 42);

            let captured = 100i32;
            let closure = FrameSharedFn::new(move |x: i32| x + captured);
            let cloned = closure.clone();
            assert_eq!(cloned.call(5), 105);
        }
        let stats = pool.last_stats();
        assert_eq!(stats.heap_fallbacks, 0);
        assert!(stats.arena_allocs > 0);
    }
}
