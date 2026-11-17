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
    /// `Some` when a scope was overwritten by a nested one. Memorized only so
    /// nested `enter_memo_scope` calls are detectable while debugging; the
    /// previous raw pointer is *never* re-installed on drop (doing so would
    /// resurrect a pointer invalidated by this scope's own `&mut MemoStore`
    /// borrow -- caught by Miri in `render::memo::tests`).
    _prev: Option<ActiveMemoContext>,
}

impl Drop for MemoScopeGuard {
    fn drop(&mut self) {
        MEMO_CTX.with(|slot| {
            *slot.borrow_mut() = None;
        });
    }
}

#[must_use]
pub(crate) fn enter_memo_scope(store: &mut MemoStore) -> MemoScopeGuard {
    store.frame_seq += 1;
    let prev = MEMO_CTX.with(|slot| {
        slot.borrow_mut().replace(ActiveMemoContext {
            store: store as *mut MemoStore,
            current_surface_hash: 0,
        })
    });
    MemoScopeGuard { _prev: prev }
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
        // SAFETY: `store` points to the `MemoStore` whose unique borrow created this
        // context, and it is dereferenced only while it is the *current* context on
        // this thread: a nested `enter_memo_scope` overwrites the context (and its
        // raw pointer is dropped, never restored), so no invalidated pointer is ever
        // dereferenced. See the Miri-checked `render::memo::tests`.
        let store = unsafe { &mut *ctx.store };
        let id = MemoId {
            surface_hash: ctx.current_surface_hash,
            element_key: key,
        };
        store.check_or_record(id, dep)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::ElementKey;

    fn memo_id(key: u64) -> MemoId {
        MemoId {
            surface_hash: 1,
            element_key: ElementKey(key),
        }
    }

    #[test]
    fn first_dependent_change_misses_then_hits() {
        let mut store = MemoStore::default();
        let id = memo_id(1);

        assert!(!store.check_or_record::<u32>(id, &7));
        assert!(store.check_or_record::<u32>(id, &7));
        assert!(!store.check_or_record::<u32>(id, &8));
        assert!(store.check_or_record::<u32>(id, &8));
    }

    #[test]
    fn different_types_same_id_replace_each_other() {
        let mut store = MemoStore::default();
        let id = memo_id(2);

        assert!(!store.check_or_record::<u32>(id, &7));
        // A same-id entry with a different `D` replaces the previous one.
        assert!(!store.check_or_record::<u64>(id, &7));
        assert_eq!(store.entries.len(), 1);
        assert!(store.check_or_record::<u64>(id, &7));
        assert!(!store.check_or_record::<u32>(id, &7));
    }

    #[test]
    fn prune_keeps_only_current_frame_entries() {
        let mut store = MemoStore::default();
        let id = memo_id(3);

        {
            let _guard = enter_memo_scope(&mut store);
            assert!(!store.check_or_record::<u32>(id, &1));
            assert!(store.check_or_record::<u32>(id, &1));
        }

        {
            // A new frame in which `id` is never seen.
            let _guard = enter_memo_scope(&mut store);
        }

        store.prune_stale();
        assert!(store.entries.is_empty());
    }

    #[test]
    fn nested_scopes_overwrite_and_out_of_scope_misses() {
        let mut store = MemoStore::default();
        let key = ElementKey(4);

        assert!(!check_memo::<u32>(key, &1), "no active scope -> miss");

        {
            let _outer = enter_memo_scope(&mut store);
            assert!(!check_memo::<u32>(key, &1));
            assert!(check_memo::<u32>(key, &1));

            {
                let _inner = enter_memo_scope(&mut store);
                // Entries persist across frames: an unchanged dependency hits
                // in a nested scope too. (Scopes are not re-entrant; the outer
                // raw pointer is dropped, never restored.)
                assert!(check_memo::<u32>(key, &1));
            }

            assert!(
                !check_memo::<u32>(key, &1),
                "inner scope drop clears the context entirely"
            );

            {
                // Re-entering a scope restores store access; the unchanged dep hits.
                let _outer2 = enter_memo_scope(&mut store);
                assert!(check_memo::<u32>(key, &1));
            }
        }

        assert!(
            !check_memo::<u32>(key, &1),
            "no active scope after guard drop"
        );
    }
}
