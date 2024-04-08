use core::{
    cell::UnsafeCell,
    future::Future,
    marker::{PhantomData, PhantomPinned},
    pin::Pin,
    ptr::{self, addr_of_mut},
    task,
};

pub struct Runtime {
    raw: UnsafeCell<RawRuntime>,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        debug_assert_eq!(unsafe { (*self.raw.get()).null_pad.next }, None);
    }
}

impl Runtime {
    pub fn wait_notification(&self) -> WaitNotification<'_> {
        WaitNotification {
            pad: UnsafeCell::new(NotifyPad::new()),
        }
    }
}

pub struct WaitNotification<'a> {
    pad: UnsafeCell<NotifyPad>,
    _marker: PhantomData<&'a Runtime>,
}

impl Future for WaitNotification<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> task::Poll<Self::Output> {
        unsafe {
            let this = self.get_unchecked_mut();
            (this.rt.raw.get())
        }
    }
}

impl Drop for WaitNotification<'_> {
    fn drop(&mut self) {
        unsafe { NotifyPad::unregister(self.pad.get()) }
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

struct RawRuntime {
    null_pad: NotifyPad,
    last_pad: ptr::NonNull<NotifyPad>,
    _pinned: PhantomPinned,
}

impl RawRuntime {
    /// Initialize `RawRuntime`
    ///
    /// # Safety
    ///
    /// This object must not be moved after initialization.
    const unsafe fn init(this: *mut Self) {
        ptr::addr_of_mut!((*this).null_pad)
            .write(NotifyPad::new(ptr::NonNull::new_unchecked(this)));
        (*this)._pinned = PhantomPinned;
        (*this).last_pad =
            ptr::NonNull::new_unchecked(UnsafeCell::raw_get(ptr::addr_of!((*this).null_pad)));
    }

    unsafe fn unregister_all(this: *mut Self) {
        let Some(first) = (*this).null_pad.next else {
            return;
        };

        (*first.as_ptr()).prev = None;
        (*this).last_pad =
            ptr::NonNull::new_unchecked(UnsafeCell::raw_get(ptr::addr_of!((*this).null_pad)));
    }

    unsafe fn notify_all(this: *mut Self) {
        let mut cur_ptr = addr_of_mut!((*this).null_pad);
        while let Some(next) = (*cur_ptr).next {
            cur_ptr = next.as_ptr();
            if let Some(waker) = (*cur_ptr).waker.take() {
                waker.wake()
            }
            (*cur_ptr).ready = true;
        }
    }

    unsafe fn register(this: *mut Self, new_last: ptr::NonNull<NotifyPad>) {
        debug_assert_eq!((*new_last.as_ptr()).next, None);
        NotifyPad::join((*this).last_pad, new_last);
        (*this).last_pad = new_last;
    }
}

struct NotifyPad {
    prev: Option<ptr::NonNull<NotifyPad>>,
    next: Option<ptr::NonNull<NotifyPad>>,
    waker: Option<task::Waker>,
    raw_rt: ptr::NonNull<RawRuntime>,
    ready: bool,
    _pinned: PhantomPinned,
}

impl NotifyPad {
    const fn new(raw_rt: ptr::NonNull<RawRuntime>) -> Self {
        Self {
            prev: None,
            next: None,
            waker: None,
            raw_rt,
            ready: false,
            _pinned: PhantomPinned,
        }
    }

    unsafe fn join(heads_last: ptr::NonNull<NotifyPad>, tails_first: ptr::NonNull<NotifyPad>) {
        debug_assert_eq!((*tails_first.as_ptr()).prev, None);
        debug_assert_eq!((*heads_last.as_ptr()).next, None);
        (*heads_last.as_ptr()).next = Some(tails_first);
        (*tails_first.as_ptr()).last = Some(heads_last);
    }

    unsafe fn unregister(this: ptr::NonNull<Self>) {
        let this = this.as_ptr();
        let next = (*this).next.take();
        let prev = (*this).prev.take();
        if let Some(prev) = prev {
            debug_assert_eq!((*prev.as_ptr()).next.as_ptr(), this);
            (*prev.as_ptr()).next = next;
        }
        if let Some(next) = next {
            debug_assert_eq!((*next.as_ptr()).prev.as_ptr(), this);
            (*next.as_ptr()).prev = prev;
        }
    }
}
