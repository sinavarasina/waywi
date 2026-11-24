use std::ptr::NonNull;
use std::rc::Rc;

/// Reference-counted closure / mapper handle using a zero-allocation static vtable for
/// non-capturing ZST closures (`size_of::<F>() == 0 && !needs_drop::<F>()`) and `Rc<F>` for
/// capturing closures so retained nodes never pin per-frame bump arena slabs.
pub(crate) struct FrameSharedFn<Arg, Out> {
    data: NonNull<u8>,
    call: unsafe fn(NonNull<u8>, Arg) -> Out,
    clone_fn: unsafe fn(NonNull<u8>) -> NonNull<u8>,
    drop_fn: unsafe fn(NonNull<u8>),
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
            };
        }

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
        }
    }

    #[inline]
    pub(crate) fn call(&self, arg: Arg) -> Out {
        // SAFETY: Function pointer matches concrete stored type `F`.
        unsafe { (self.call)(self.data, arg) }
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
        }
    }
}

impl<Arg, Out> Drop for FrameSharedFn<Arg, Out> {
    fn drop(&mut self) {
        // SAFETY: `self.drop_fn` matches `self.data`.
        unsafe {
            (self.drop_fn)(self.data);
        }
    }
}
