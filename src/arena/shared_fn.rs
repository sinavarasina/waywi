use std::alloc::Layout;
use std::cell::Cell;
use std::ptr::NonNull;
use std::rc::Rc;

use super::pool::{FrameSlab, current_slab};

/// Reference-counted closure / mapper handle backed by the frame arena (or ZST sentinel when
/// `size_of::<F>() == 0`, performing zero allocations anywhere).
pub(crate) struct FrameSharedFn<Arg, Out> {
    data: NonNull<u8>,
    call: unsafe fn(NonNull<u8>, Arg) -> Out,
    clone_fn: unsafe fn(NonNull<u8>) -> NonNull<u8>,
    drop_fn: unsafe fn(NonNull<u8>),
    slab: Option<Rc<FrameSlab>>,
}

#[repr(C)]
struct SharedHeader<F> {
    refs: Cell<usize>,
    func: F,
}

impl<Arg, Out> FrameSharedFn<Arg, Out> {
    pub(crate) fn new<F>(func: F) -> Self
    where
        F: Fn(Arg) -> Out + 'static,
    {
        if size_of::<F>() == 0 && !std::mem::needs_drop::<F>() {
            unsafe fn call_zst<F: Fn(Arg) -> Out, Arg, Out>(_ptr: NonNull<u8>, arg: Arg) -> Out {
                // SAFETY: `F` is a zero-sized type; `NonNull::dangling()` is aligned and valid
                // to dereference without reading memory.
                let f = unsafe { &*(NonNull::<F>::dangling().as_ptr()) };
                f(arg)
            }
            unsafe fn clone_zst(ptr: NonNull<u8>) -> NonNull<u8> {
                ptr
            }
            unsafe fn drop_zst(_ptr: NonNull<u8>) {
                // `!needs_drop::<F>()` was checked at construction; nothing to drop on clones.
            }
            std::mem::forget(func);
            return Self {
                data: NonNull::dangling(),
                call: call_zst::<F, Arg, Out>,
                clone_fn: clone_zst,
                drop_fn: drop_zst,
                slab: None,
            };
        }

        if let Some(slab) = current_slab() {
            let layout = Layout::new::<SharedHeader<F>>();
            if let Some(raw) = slab.alloc_layout(layout) {
                let typed = raw.cast::<SharedHeader<F>>();
                // SAFETY: `typed` is properly aligned and allocated for `SharedHeader<F>`.
                unsafe {
                    typed.as_ptr().write(SharedHeader {
                        refs: Cell::new(1),
                        func,
                    });
                }

                unsafe fn call_arena<F: Fn(Arg) -> Out, Arg, Out>(
                    ptr: NonNull<u8>,
                    arg: Arg,
                ) -> Out {
                    // SAFETY: `ptr` points to a live `SharedHeader<F>` in the arena slab.
                    let header = unsafe { &*ptr.cast::<SharedHeader<F>>().as_ptr() };
                    (header.func)(arg)
                }
                unsafe fn clone_arena<F>(ptr: NonNull<u8>) -> NonNull<u8> {
                    // SAFETY: Increment single-threaded refcount inside `SharedHeader<F>`.
                    let header = unsafe { &*ptr.cast::<SharedHeader<F>>().as_ptr() };
                    header.refs.set(header.refs.get() + 1);
                    ptr
                }
                unsafe fn drop_arena<F>(ptr: NonNull<u8>) {
                    // SAFETY: Decrement refcount; drop `F` in place on last reference without freeing.
                    let header = unsafe { &*ptr.cast::<SharedHeader<F>>().as_ptr() };
                    let next = header.refs.get() - 1;
                    header.refs.set(next);
                    if next == 0 {
                        // SAFETY: This is the final reference (`next == 0` above), so `F` has not
                        // been dropped before; the slab keeps the storage alive for the arena.
                        unsafe {
                            std::ptr::drop_in_place(
                                &raw mut (*ptr.cast::<SharedHeader<F>>().as_ptr()).func,
                            );
                        }
                    }
                }

                return Self {
                    data: raw,
                    call: call_arena::<F, Arg, Out>,
                    clone_fn: clone_arena::<F>,
                    drop_fn: drop_arena::<F>,
                    slab: Some(slab),
                };
            }
        }

        // Heap fallback outside `App`.
        let rc = Rc::new(func);
        let raw = NonNull::new(Rc::into_raw(rc) as *mut u8).expect("Rc::into_raw non-null");

        unsafe fn call_rc<F: Fn(Arg) -> Out, Arg, Out>(ptr: NonNull<u8>, arg: Arg) -> Out {
            // SAFETY: `ptr` comes from `Rc::into_raw`.
            let f = unsafe { &*ptr.cast::<F>().as_ptr() };
            f(arg)
        }
        unsafe fn clone_rc<F>(ptr: NonNull<u8>) -> NonNull<u8> {
            // SAFETY: Increment Rc count without dropping original.
            unsafe {
                Rc::increment_strong_count(ptr.cast::<F>().as_ptr());
            }
            ptr
        }
        unsafe fn drop_rc<F>(ptr: NonNull<u8>) {
            // SAFETY: Reconstruct and drop one `Rc<F>` handle.
            unsafe {
                drop(Rc::from_raw(ptr.cast::<F>().as_ptr()));
            }
        }

        Self {
            data: raw,
            call: call_rc::<F, Arg, Out>,
            clone_fn: clone_rc::<F>,
            drop_fn: drop_rc::<F>,
            slab: None,
        }
    }

    #[inline]
    pub(crate) fn call(&self, arg: Arg) -> Out {
        // SAFETY: Function pointer matches concrete stored type `F`.
        unsafe { (self.call)(self.data, arg) }
    }

    #[cfg(test)]
    pub(crate) fn is_arena_backed(&self) -> bool {
        self.slab.is_some()
    }
}

impl<Arg, Out> Clone for FrameSharedFn<Arg, Out> {
    fn clone(&self) -> Self {
        // SAFETY: `self.clone_fn` matches the concrete representation backing `self.data`.
        let data = unsafe { (self.clone_fn)(self.data) };
        Self {
            data,
            call: self.call,
            clone_fn: self.clone_fn,
            drop_fn: self.drop_fn,
            slab: self.slab.as_ref().map(Rc::clone),
        }
    }
}

impl<Arg, Out> Drop for FrameSharedFn<Arg, Out> {
    fn drop(&mut self) {
        // SAFETY: `self.drop_fn` matches `self.data`, and `self.slab` drops only afterwards.
        unsafe {
            (self.drop_fn)(self.data);
        }
    }
}
