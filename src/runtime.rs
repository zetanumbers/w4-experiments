use core::{
    cell::UnsafeCell,
    future::Future,
    hint::unreachable_unchecked,
    marker::PhantomPinned,
    pin::Pin,
    ptr::{self, addr_of, addr_of_mut},
    task,
};

// FIXME: consider replacing debug_assert with debug_assert_unchecked?

pub struct Runtime {
    root: UnsafeCell<NotifyPad>,
    // SAFETY: allows `&mut self` to have `pad` field aliased from somewhere else.
    // STABILITY: This is an unstable work-around (see https://github.com/rust-lang/miri/pull/2713)
    // FIXME: create or find polyfill
    _pinned: PhantomPinned,
}

impl Runtime {
    pub const unsafe fn dangling() -> Self {
        Runtime {
            root: UnsafeCell::new(NotifyPad::dangling()),
            _pinned: PhantomPinned,
        }
    }

    pub unsafe fn init(&self) {
        unsafe {
            let root = UnsafeCell::raw_get(addr_of!(self.root));
            NotifyPad::init_single(root);
            (*root).waker = Some(waker());
        }
    }

    /// Notify every future
    ///
    /// # Panics
    ///
    /// This function panics if self is not initialized.
    pub fn notify_all(&self) {
        unsafe {
            let root = UnsafeCell::raw_get(addr_of!(self.root));
            let waker = (*root)
                .waker
                .as_ref()
                .expect("this runtime isn't initialized");
            let mut cur;
            loop {
                cur = NotifyPad::get_next(root);
                if cur == root {
                    break;
                }

                (*cur).waker.take().unwrap().wake();
            }
        }
    }
}

pub struct WaitNotification<'a> {
    state: WaitNotificationState<'a>,
}

impl Future for WaitNotification<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> task::Poll<Self::Output> {
        unsafe {
            let this = self.get_unchecked_mut();
            match &mut this.state {
                WaitNotificationState::Unresumed(rt) => {
                    let rt = *rt;
                    this.state =
                        WaitNotificationState::Registered(WaitNotificationRegisteredState {
                            pad: NotifyPad::dangling(),
                            _pinned: PhantomPinned,
                        });

                    let WaitNotificationState::Registered(state) = &mut this.state else {
                        unreachable_unchecked()
                    };

                    let pad = addr_of_mut!(state.pad);
                    let root = UnsafeCell::raw_get(addr_of!(rt.root));
                    NotifyPad::push_before(root, pad);
                    NotifyPad::unnotified_poll(pad, cx);

                    task::Poll::Pending
                }
                WaitNotificationState::Registered(state) => {
                    let pad = addr_of_mut!(state.pad);
                    NotifyPad::poll(pad, cx)
                }
            }
        }
    }
}

impl Drop for WaitNotification<'_> {
    fn drop(&mut self) {
        unsafe { NotifyPad::unregister(self.pad.get()) }
    }
}

// FIXME: optimize struct size with null pointers and panics unless cargo features are enabled by:
// - Assuming we would only encounter our wakers and only have to store a pointer to a runtime
//   (4 -> 3 words)
// - Assuming we won't be repoll while suspended, thus we won't need to store a pointer to runtime
//   to check if it's our waker we got (3 -> 2 words)
//
// FIXME: consider runtime with const pad capacity (n -> 1 word). Existing `heapless` async
// runtime?
enum WaitNotificationState<'a> {
    Unresumed(&'a Runtime),
    Registered(WaitNotificationRegisteredState),
}

struct WaitNotificationRegisteredState {
    pad: NotifyPad,
    // SAFETY: allows `&mut self` to have `pad` field aliased from somewhere else.
    // STABILITY: This is an unstable work-around (see https://github.com/rust-lang/miri/pull/2713)
    // FIXME: create or find polyfill
    _pinned: PhantomPinned,
}

impl Drop for WaitNotificationRegisteredState {
    fn drop(&mut self) {
        unsafe { NotifyPad::unregister(addr_of_mut!(self.pad)) }
    }
}

/// This structure is intended to form a linked cycle. Any linked cycle
struct NotifyPad {
    prev: ptr::NonNull<NotifyPad>,
    next: ptr::NonNull<NotifyPad>,
    waker: Option<task::Waker>,
    _pinned: PhantomPinned,
}

impl NotifyPad {
    /// Returns `NotifyPod` with dangling pointers. You can use such pad only after a call to
    /// [`Self::init_single`] or [`Self::push_before`].
    const fn dangling() -> Self {
        Self {
            prev: ptr::NonNull::dangling(),
            next: ptr::NonNull::dangling(),
            // we assume this equality outside of this function
            waker: None,
            _pinned: PhantomPinned,
        }
    }

    /// Initialize `NotifyPod` as a single cycle
    ///
    /// # Safety
    ///
    /// Pointer `to_init` must point to a newly created `NotifyPod`
    ///
    /// Pointee must not be moved after the return of this function to preserve safety type
    /// invatiant. Consider these rules similar to the pinning rules.
    unsafe fn init_single(to_init: *mut Self) {
        (*to_init).prev = ptr::NonNull::new_unchecked(to_init);
        (*to_init).next = ptr::NonNull::new_unchecked(to_init);
    }

    unsafe fn root_is_cycle_empty(root: *mut Self) {
        let out = (*root).prev.as_ptr() == root;
        #[cfg(debug_assertions)]
        if out {
            debug_assert_eq!((*root).next.as_ptr(), root);
        } else {
            debug_assert_ne!((*root).next.as_ptr(), root);
        }
        out
    }

    unsafe fn is_notified(this: *mut Self) -> bool {
        (*this).waker.is_none()
    }

    unsafe fn unnotified_poll(pad: *mut NotifyPad, cx: &mut task::Context<'_>) {
        (*pad).waker = Some(cx.waker().clone());
    }

    unsafe fn poll(pad: *mut Self, cx: &mut task::Context<'_>) -> task::Poll<()> {
        match &(*pad).waker {
            Some(_) => {
                Self::unnotified_poll(pad, cx);
                task::Poll::Pending
            }
            None => task::Poll::Ready(()),
        }
    }

    unsafe fn push_before(after: *mut Self, to_push: *mut Self) {
        let before = Self::get_prev(after);
        (*to_push).prev = before;
        (*to_push).next = after;
        (*after).prev = to_push;
        (*before).next = to_push;
    }

    #[inline(always)]
    unsafe fn get_next(this: *mut Self) -> *mut Self {
        let next = (*this).next.as_ptr();
        debug_assert_eq!((*next).prev.as_ptr(), this);
        next
    }

    #[inline(always)]
    unsafe fn get_prev(this: *mut Self) -> *mut Self {
        let prev = (*this).prev.as_ptr();
        debug_assert_eq!((*prev).next.as_ptr(), this);
        prev
    }

    /// Unregister a pod from the pod linked cycle.
    ///
    /// # Safety
    ///
    /// `this` must not point to an empty root pod. If it's a root pod, make sure
    unsafe fn unregister(this: *mut Self) {
        let next = Self::get_next(this);
        let prev = Self::get_prev(this);

        debug_assert_ne!(this, next);
        debug_assert_ne!(this, prev);

        (*prev).next = ptr::NonNull::new_unchecked(next);
        (*next).prev = ptr::NonNull::new_unchecked(prev);

        #[cfg(debug_assertions)]
        {
            // assign invalid pointers for segfault or MIRI error on dereferencing
            (*this).next = ptr::NonNull::dangling();
            (*this).prev = ptr::NonNull::dangling();
        }
    }
}

const fn waker() -> task::Waker {
    fn clone(this: *const ()) -> task::RawWaker {
        task::RawWaker::new(this, &VTABLE)
    }

    static VTABLE: task::RawWakerVTable = task::RawWakerVTable::new(clone, drop, drop, drop);

    static UNIT: () = ();

    let waker = task::RawWaker::new(&UNIT, &VTABLE);
    unsafe { task::Waker::from_raw(waker) }
}
