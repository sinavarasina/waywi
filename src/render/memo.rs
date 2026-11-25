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
    pub(crate) children: Vec<MemoId>,
}

#[derive(Default)]
pub(crate) struct MemoStore {
    pub(crate) entries: HashMap<MemoId, MemoEntry>,
    pub(crate) frame_seq: u64,
    active_stack: Vec<(MemoId, Vec<MemoId>)>,
}

impl MemoStore {
    pub(crate) fn check_or_record<D: PartialEq + Clone + 'static>(
        &mut self,
        id: MemoId,
        dep: &D,
    ) -> bool {
        if let Some((_, parent_children)) = self.active_stack.last_mut()
            && !parent_children.contains(&id)
        {
            parent_children.push(id);
        }

        if let Some(existing) = self.entries.get_mut(&id)
            && existing.type_id == TypeId::of::<D>()
            && (existing.eq_fn)(existing.value.as_ref(), dep)
        {
            self.mark_subtree_seen(id);
            return true;
        }

        fn eq_impl<T: PartialEq + 'static>(a: &dyn Any, b: &dyn Any) -> bool {
            a.downcast_ref::<T>()
                .zip(b.downcast_ref::<T>())
                .is_some_and(|(x, y)| x == y)
        }

        let seq = self.frame_seq;
        if let Some(existing) = self.entries.get_mut(&id) {
            if let Some(slot) = existing.value.downcast_mut::<D>() {
                slot.clone_from(dep);
            } else {
                existing.type_id = TypeId::of::<D>();
                existing.value = Box::new(dep.clone());
                existing.eq_fn = eq_impl::<D>;
            }
            existing.last_seen_frame = seq;
            existing.children.clear();
        } else {
            self.entries.insert(
                id,
                MemoEntry {
                    type_id: TypeId::of::<D>(),
                    value: Box::new(dep.clone()),
                    eq_fn: eq_impl::<D>,
                    last_seen_frame: seq,
                    children: Vec::new(),
                },
            );
        }
        false
    }

    fn mark_subtree_seen(&mut self, root: MemoId) {
        let seq = self.frame_seq;
        let mut stack = vec![root];
        while let Some(cur) = stack.pop() {
            if let Some(entry) = self.entries.get_mut(&cur) {
                if entry.last_seen_frame == seq && cur != root {
                    continue;
                }
                entry.last_seen_frame = seq;
                stack.extend(entry.children.iter().copied());
            }
        }
    }

    fn push_build(&mut self, id: MemoId) {
        self.active_stack.push((id, Vec::new()));
    }

    fn pop_build(&mut self) {
        if let Some((id, children)) = self.active_stack.pop()
            && !children.is_empty()
            && let Some(entry) = self.entries.get_mut(&id)
        {
            entry.children = children;
        }
    }

    pub(crate) fn prune_stale(&mut self) {
        let current = self.frame_seq;
        self.entries.retain(|_, v| v.last_seen_frame == current);
        if self.entries.capacity() > (self.entries.len().saturating_mul(4)).max(32) {
            self.entries
                .shrink_to((self.entries.len().saturating_mul(2)).max(16));
        }
    }

    pub(crate) fn trim(&mut self) {
        self.entries.shrink_to_fit();
    }

    /// Invalidates every memo entry across all surfaces (e.g. when `wl_output` topology changes
    /// so `Surface::memo` and `Element::memo` rebuild cleanly for newly added or replaced outputs).
    pub(crate) fn invalidate_all(&mut self) {
        self.entries.clear();
    }

    /// Invalidates all memo entries belonging to `surface_hash` (both the `Surface::memo` shell
    /// and any nested `Element::memo` subtrees, e.g. on resize, scale change, or layer teardown).
    pub(crate) fn invalidate_surface(&mut self, surface_hash: u64) {
        self.entries.retain(|id, _| id.surface_hash != surface_hash);
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
    store.active_stack.clear();
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

pub(crate) struct MemoBuildGuard {
    pushed: bool,
}

impl Drop for MemoBuildGuard {
    fn drop(&mut self) {
        if !self.pushed {
            return;
        }
        MEMO_CTX.with(|slot| {
            let borrow = slot.borrow();
            let Some(ref ctx) = *borrow else {
                return;
            };
            // SAFETY: Identical unique-scope lifetime contract as `begin_memo_check`.
            let store = unsafe { &mut *ctx.store };
            store.pop_build();
        });
    }
}

/// Checks memoization for `(current_surface_hash, key)` against `dep`.
///
/// Returns `None` on a memo hit (automatically preserving all nested child memo entries for this
/// frame), or `Some(MemoBuildGuard)` on a miss so nested memo calls inside the rebuild closure
/// register as children until the guard drops.
pub(crate) fn begin_memo_check<D: PartialEq + Clone + 'static>(
    key: ElementKey,
    dep: &D,
) -> Option<MemoBuildGuard> {
    MEMO_CTX.with(|slot| {
        let borrow = slot.borrow();
        let Some(ref ctx) = *borrow else {
            return Some(MemoBuildGuard { pushed: false });
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
        if store.check_or_record(id, dep) {
            None
        } else {
            store.push_build(id);
            Some(MemoBuildGuard { pushed: true })
        }
    })
}

#[cfg(test)]
pub(crate) fn check_memo<D: PartialEq + Clone + 'static>(key: ElementKey, dep: &D) -> bool {
    begin_memo_check(key, dep).is_none()
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
