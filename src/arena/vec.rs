use std::alloc::Layout;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::rc::Rc;

use super::pool::{FrameSlab, current_slab};

/// Vector-like container backed by the active scoped frame arena inside `App` and normal `Vec<T>`
/// when constructed outside `App` (e.g. unit tests).
pub(crate) struct FrameVec<T> {
    inner: FrameVecInner<T>,
}

enum FrameVecInner<T> {
    Heap(Vec<T>),
    Arena {
        ptr: NonNull<T>,
        len: usize,
        cap: usize,
        slab: Rc<FrameSlab>,
    },
}

impl<T> FrameVec<T> {
    pub(crate) fn new() -> Self {
        if let Some(slab) = current_slab() {
            Self {
                inner: FrameVecInner::Arena {
                    ptr: NonNull::dangling(),
                    len: 0,
                    cap: if size_of::<T>() == 0 { usize::MAX } else { 0 },
                    slab,
                },
            }
        } else {
            Self {
                inner: FrameVecInner::Heap(Vec::new()),
            }
        }
    }

    pub(crate) fn with_capacity(capacity: usize) -> Self {
        if capacity == 0 {
            return Self::new();
        }
        if let Some(slab) = current_slab() {
            if size_of::<T>() == 0 {
                return Self {
                    inner: FrameVecInner::Arena {
                        ptr: NonNull::dangling(),
                        len: 0,
                        cap: usize::MAX,
                        slab,
                    },
                };
            }
            if let Ok(layout) = Layout::array::<T>(capacity)
                && let Some(raw) = slab.alloc_layout(layout)
            {
                return Self {
                    inner: FrameVecInner::Arena {
                        ptr: raw.cast::<T>(),
                        len: 0,
                        cap: capacity,
                        slab,
                    },
                };
            }
        }
        Self {
            inner: FrameVecInner::Heap(Vec::with_capacity(capacity)),
        }
    }

    pub(crate) fn push(&mut self, value: T) {
        match &mut self.inner {
            FrameVecInner::Heap(v) => v.push(value),
            FrameVecInner::Arena {
                ptr,
                len,
                cap,
                slab,
            } => {
                if *len == *cap {
                    let new_cap = if *cap == 0 { 4 } else { cap.saturating_mul(2) };
                    let layout = Layout::array::<T>(new_cap).expect("FrameVec capacity overflow");
                    let new_ptr = slab
                        .alloc_layout(layout)
                        .expect("arena allocation failed")
                        .cast::<T>();
                    if *len > 0 {
                        // SAFETY: Old `ptr` is valid for `*len` initialized elements and `new_ptr`
                        // is freshly allocated and non-overlapping.
                        unsafe {
                            std::ptr::copy_nonoverlapping(ptr.as_ptr(), new_ptr.as_ptr(), *len);
                        }
                    }
                    *ptr = new_ptr;
                    *cap = new_cap;
                }
                // SAFETY: `*len < *cap` and `ptr.add(*len)` points to uninitialized slot within
                // the arena allocation.
                unsafe {
                    ptr.as_ptr().add(*len).write(value);
                }
                *len += 1;
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn len(&self) -> usize {
        match &self.inner {
            FrameVecInner::Heap(v) => v.len(),
            FrameVecInner::Arena { len, .. } => *len,
        }
    }

    pub(crate) fn clear(&mut self) {
        match &mut self.inner {
            FrameVecInner::Heap(v) => v.clear(),
            FrameVecInner::Arena { ptr, len, .. } => {
                let count = *len;
                *len = 0;
                if count > 0 {
                    // SAFETY: `ptr` holds `count` valid elements; `len` was set to 0 first for
                    // drop panic safety.
                    unsafe {
                        std::ptr::drop_in_place(std::ptr::slice_from_raw_parts_mut(
                            ptr.as_ptr(),
                            count,
                        ));
                    }
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn is_arena_backed(&self) -> bool {
        matches!(self.inner, FrameVecInner::Arena { .. })
    }
}

impl<T> Default for FrameVec<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Deref for FrameVec<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        match &self.inner {
            FrameVecInner::Heap(v) => v.as_slice(),
            FrameVecInner::Arena { ptr, len, .. } => unsafe {
                // SAFETY: `ptr` is non-null, aligned, and initialized for `*len` items.
                std::slice::from_raw_parts(ptr.as_ptr(), *len)
            },
        }
    }
}

impl<T> DerefMut for FrameVec<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match &mut self.inner {
            FrameVecInner::Heap(v) => v.as_mut_slice(),
            FrameVecInner::Arena { ptr, len, .. } => unsafe {
                // SAFETY: Exclusive `&mut self` guarantees unique mutable access to `*len` items.
                std::slice::from_raw_parts_mut(ptr.as_ptr(), *len)
            },
        }
    }
}

impl<T> Drop for FrameVec<T> {
    fn drop(&mut self) {
        if matches!(self.inner, FrameVecInner::Arena { .. }) {
            self.clear();
        }
    }
}

impl<T> FromIterator<T> for FrameVec<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let iter = iter.into_iter();
        let (low, _) = iter.size_hint();
        let mut out = Self::with_capacity(low);
        for item in iter {
            out.push(item);
        }
        out
    }
}

impl<T: Clone> Clone for FrameVec<T> {
    fn clone(&self) -> Self {
        let mut out = Self::with_capacity(self.len());
        for item in self.iter() {
            out.push(item.clone());
        }
        out
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for FrameVec<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.deref().fmt(f)
    }
}

/// Draining iterator for `FrameVec<T>`.
#[doc(hidden)]
pub struct FrameVecIntoIter<T> {
    inner: FrameVec<T>,
    index: usize,
}

impl<T> Iterator for FrameVecIntoIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner.inner {
            FrameVecInner::Heap(v) => {
                if self.index < v.len() {
                    // SAFETY: `self.index < v.len()` and elements before `self.index` were already
                    // moved out; `Drop` skips `[0..self.index]`.
                    let item = unsafe { std::ptr::read(v.as_ptr().add(self.index)) };
                    self.index += 1;
                    Some(item)
                } else {
                    None
                }
            }
            FrameVecInner::Arena { ptr, len, .. } => {
                if self.index < *len {
                    // SAFETY: `self.index < *len` and elements before `self.index` were already
                    // moved out; `Drop` only drops `[self.index..*len]`.
                    let item = unsafe { std::ptr::read(ptr.as_ptr().add(self.index)) };
                    self.index += 1;
                    Some(item)
                } else {
                    None
                }
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let rem = self.inner.len().saturating_sub(self.index);
        (rem, Some(rem))
    }
}

impl<T> Drop for FrameVecIntoIter<T> {
    fn drop(&mut self) {
        match &mut self.inner.inner {
            FrameVecInner::Heap(v) => {
                let len = v.len();
                // SAFETY: Prevent double-drop of elements already yielded by `next()`.
                unsafe {
                    v.set_len(0);
                    if self.index < len {
                        std::ptr::drop_in_place(std::ptr::slice_from_raw_parts_mut(
                            v.as_mut_ptr().add(self.index),
                            len - self.index,
                        ));
                    }
                }
            }
            FrameVecInner::Arena { ptr, len, .. } => {
                let total = *len;
                *len = 0;
                if self.index < total {
                    // SAFETY: Only unconsumed tail elements `[index..total]` remain initialized.
                    unsafe {
                        std::ptr::drop_in_place(std::ptr::slice_from_raw_parts_mut(
                            ptr.as_ptr().add(self.index),
                            total - self.index,
                        ));
                    }
                }
            }
        }
    }
}

impl<T> IntoIterator for FrameVec<T> {
    type Item = T;
    type IntoIter = FrameVecIntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        FrameVecIntoIter {
            inner: self,
            index: 0,
        }
    }
}
