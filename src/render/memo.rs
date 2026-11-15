use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;

use crate::input::ElementKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MemoId {
    pub(crate) surface_hash: u64,
    pub(crate) element_key: ElementKey,
}

pub(crate) struct MemoEntry {
    pub(crate) type_id: TypeId,
    pub(crate) value: Box<dyn Any>,
    pub(crate) eq_fn: fn(&dyn Any, &dyn Any) -> bool,
    pub(crate) last_seen_frame: u64,
}

#[derive(Default)]
pub(crate) struct MemoStore {
    pub(crate) entries: HashMap<MemoId, MemoEntry>,
    pub(crate) frame_seq: u64,
}

impl MemoStore {
    pub(crate) fn check_or_record<D: PartialEq + Clone + 'static>(
        &mut self,
        id: MemoId,
        dep: &D,
    ) -> bool {
        if let Some(existing) = self.entries.get_mut(&id)
            && existing.type_id == TypeId::of::<D>()
            && (existing.eq_fn)(existing.value.as_ref(), dep)
        {
            existing.last_seen_frame = self.frame_seq;
            return true;
        }
        fn eq_impl<T: PartialEq + 'static>(a: &dyn Any, b: &dyn Any) -> bool {
            a.downcast_ref::<T>()
                .zip(b.downcast_ref::<T>())
                .is_some_and(|(x, y)| x == y)
        }
        self.entries.insert(
            id,
            MemoEntry {
                type_id: TypeId::of::<D>(),
                value: Box::new(dep.clone()),
                eq_fn: eq_impl::<D>,
                last_seen_frame: self.frame_seq,
            },
        );
        false
    }

    pub(crate) fn prune_stale(&mut self) {
        let current = self.frame_seq;
        self.entries.retain(|_, v| v.last_seen_frame == current);
    }
}

struct ActiveMemoContext {
    store: *mut MemoStore,
    current_surface_hash: u64,
}

thread_local! {
    static MEMO_CTX: RefCell<Option<ActiveMemoContext>> = const { RefCell::new(None) };
}

pub(crate) struct MemoScopeGuard {
    prev: Option<ActiveMemoContext>,
}

impl Drop for MemoScopeGuard {
    fn drop(&mut self) {
        MEMO_CTX.with(|slot| {
            *slot.borrow_mut() = self.prev.take();
        });
    }
}

pub(crate) fn enter_memo_scope(store: &mut MemoStore) -> MemoScopeGuard {
    store.frame_seq += 1;
    let prev = MEMO_CTX.with(|slot| {
        slot.borrow_mut().replace(ActiveMemoContext {
            store: store as *mut MemoStore,
            current_surface_hash: 0,
        })
    });
    MemoScopeGuard { prev }
}

pub(crate) fn set_memo_surface_hash(surface_hash: u64) {
    MEMO_CTX.with(|slot| {
        if let Some(ref mut ctx) = *slot.borrow_mut() {
            ctx.current_surface_hash = surface_hash;
        }
    });
}

pub(crate) fn check_memo<D: PartialEq + Clone + 'static>(key: ElementKey, dep: &D) -> bool {
    MEMO_CTX.with(|slot| {
        let borrow = slot.borrow();
        let Some(ref ctx) = *borrow else {
            return false;
        };
        // SAFETY: `store` points to `MemoStore` exclusively held for the duration of
        // `MemoScopeGuard` on this thread; `check_or_record` does not re-enter `MEMO_CTX`.
        let store = unsafe { &mut *ctx.store };
        let id = MemoId {
            surface_hash: ctx.current_surface_hash,
            element_key: key,
        };
        store.check_or_record(id, dep)
    })
}
