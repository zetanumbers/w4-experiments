#![no_std]

#[cfg(feature = "buddy-alloc")]
mod alloc;
mod runtime;
mod wasm4;

use core::mem;
use core::pin::Pin;
use core::task;
use core::{future::Future, ptr::addr_of_mut};

use glam::{IVec2, Vec2};
use wasm4 as w4;

static mut LAST_UV: IVec2 = IVec2::ZERO;
static mut TIME: f32 = 0.0;
const TIME_STEP: f32 = 0.01;

async fn main() {
    unsafe {
        *w4::SYSTEM_FLAGS |= w4::SYSTEM_PRESERVE_FRAMEBUFFER;
        LAST_UV = to_viewport(heart(TIME));
        TIME += TIME_STEP;

        loop {
            wait_update().await;
            let new_position = to_viewport(heart(TIME));
            w4::line(LAST_UV.x, LAST_UV.y, new_position.x, new_position.y);
            LAST_UV = new_position;
            TIME += TIME_STEP;
        }
    }
}

fn heart(t: f32) -> Vec2 {
    Vec2 {
        x: 16.0 * t.sin().powi(3),
        y: 13.0 * t.cos() - 5.0 * (2.0 * t).cos() - 2.0 * (3.0 * t).cos() - (4.0 * t).cos(),
    } / 18.0
}

fn to_viewport_scalar(s: f32) -> i32 {
    (w4::SCREEN_SIZE as f32 / 2.0 * (s + 1.0)) as i32
}

fn to_viewport(p: Vec2) -> IVec2 {
    IVec2 {
        x: to_viewport_scalar(p.x),
        y: to_viewport_scalar(-p.y),
    }
}

const FUTURE_SIZE: usize = {
    const fn size_of_output_fut<F, R>(_: &F) -> usize
    where
        F: Fn() -> R,
    {
        core::mem::size_of::<R>()
    }

    size_of_output_fut(&main)
};

// Assert alignment is not exotic
const _: () = {
    const fn align_of_output_fut<F, R>(_: &F) -> usize
    where
        F: Fn() -> R,
    {
        core::mem::align_of::<R>()
    }

    let align = align_of_output_fut(&main);
    assert!(align.is_power_of_two() && align <= 16);
};

#[repr(C, align(16))]
#[derive(Debug)]
struct Align16<T>(T);

type FuturePlaceholder = mem::MaybeUninit<Align16<mem::MaybeUninit<[u8; FUTURE_SIZE]>>>;
static FUTURE_PLACEHOLDER: FuturePlaceholder = mem::MaybeUninit::uninit();

unsafe fn poll_fut<F, R>(
    erased: *mut FuturePlaceholder,
    cx: &mut task::Context<'_>,
    _: F,
) -> task::Poll<()>
where
    F: Fn() -> R,
    R: Future<Output = ()>,
{
    <R as Future>::poll(
        unsafe { Pin::new_unchecked(&mut (*erased.cast::<Align16<R>>()).0) },
        cx,
    )
}

#[no_mangle]
unsafe extern "C" fn start() {
    FUTURE_PLACEHOLDER = mem::transmute(Align16(main()));
    addr_of_mut!(FUTURE_PLACEHOLDER)
}

#[no_mangle]
unsafe extern "C" fn update() {}

fn main() {
    let mut placeholder: FuturePlaceholder = unsafe { core::mem::transmute(Align16(update())) };
    let waker = task::Waker::from(Arc::new(NopWake));
    let mut cx = task::Context::from_waker(&waker);
    while let task::Poll::Pending = unsafe { poll_fut(addr_of_mut!(placeholder), &mut cx, update) }
    {
    }
}

struct NopWake;

impl task::Wake for NopWake {
    fn wake(self: Arc<Self>) {}

    fn wake_by_ref(self: &Arc<Self>) {}
}
