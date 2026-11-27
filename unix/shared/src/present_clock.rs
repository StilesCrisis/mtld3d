//! The host's present clock: a native host that paces each `Present` itself.
//!
//! A native host that returns the game's `Present` on a clock of its own, at
//! the rate the game's display was built for (the host's
//! `d3d9metal_set_present_clock`), sets it here before it creates a device.
//! A device created afterwards latches it ([`get`]) and presents for that
//! clock: presents on a queue of their own reading a copy of each frame, the
//! presenter showing the newest finished frame on the display, and no
//! frame-latency bound in `Present` (the host's clock paces the game). A
//! device created without it presents as before.
//!
//! The rate is a rational period, `num / den` nanoseconds, so a host can name
//! a rate no whole number of hertz or nanoseconds expresses.
//!
//! The host can also name when a frame is shown: before a `Present`, the time
//! on the host's clock (`mach_absolute_time` in nanoseconds, the base of
//! `CACurrentMediaTime`) at which that frame's present should reach the
//! display ([`set_next_present_at`], the host's `d3d9metal_present_at`). The
//! `Present` takes it ([`take_next_present_at`]) and its frame is presented at
//! that time rather than as soon as it is finished, so which refresh shows it
//! depends on the host's schedule and not on how long the frame took.

use std::{
    cell::Cell,
    collections::VecDeque,
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

/// The host's choice, `num / den` ns per tick; `den == 0` means none.
///
/// A static because the caller has no object to reach: the host makes the
/// call before any device exists, as it chooses the frame threads' priority,
/// and each device latches it at creation, where the state then lives.
static NUM: AtomicU64 = AtomicU64::new(0);
static DEN: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The next `Present` on this thread: when its frame is shown (0: as soon as it is finished).
    ///
    /// A static because the caller has no object to reach: the host sets it
    /// on the thread about to call `Present`, before it reaches any device,
    /// and that `Present` takes it onto its frame, where the state then lives.
    static NEXT_PRESENT_AT: Cell<u64> = const { Cell::new(0) };
}

/// When the calling thread's next `Present` shows its frame.
///
/// Host-clock nanoseconds (0: as soon as it is finished).
pub fn set_next_present_at(ns: u64) {
    NEXT_PRESENT_AT.with(|next| next.set(ns));
}

/// The time the host set for this thread's `Present`, cleared as it is taken (0: none).
#[must_use]
pub fn take_next_present_at() -> u64 {
    NEXT_PRESENT_AT.with(Cell::take)
}

/// A frame the host named a time for, as the display took it.
///
/// `#[repr(C)]`: the host reads these through `d3d9metal_take_presented`.
#[repr(C)]
#[derive(Debug, Default, PartialEq)]
pub struct Shown {
    /// The time the host named for the frame (`d3d9metal_present_at`), host-clock nanoseconds.
    pub present_at_ns: u64,
    /// When the display showed it.
    ///
    /// `presentedTime`, in `CACurrentMediaTime` seconds; 0 when it was not
    /// shown: a newer frame was due first, or the display let it go.
    pub presented_s: f64,
}

/// The records a host has not taken yet, at most this many (the oldest go first).
pub const SHOWN_CAPACITY: usize = 4096;

/// What the display did with the frames the host named a time for, oldest first.
///
/// A static because the caller has no object to reach: the host drains it
/// through `d3d9metal_take_presented`, which names no device, while a
/// presenter's completion handlers add to it.
static SHOWN: Mutex<VecDeque<Shown>> = Mutex::new(VecDeque::new());

/// Record what the display did with a frame the host named a time for.
pub fn note_shown(present_at_ns: u64, presented_s: f64) {
    let mut shown = SHOWN.lock().unwrap_or_else(PoisonError::into_inner);
    if shown.len() >= SHOWN_CAPACITY {
        shown.pop_front();
    }
    shown.push_back(Shown {
        present_at_ns,
        presented_s,
    });
}

/// Move up to `out.len()` records, oldest first, into `out`; the number moved.
pub fn take_shown(out: &mut [Shown]) -> usize {
    let mut shown = SHOWN.lock().unwrap_or_else(PoisonError::into_inner);
    let n = out.len().min(shown.len());
    for (slot, record) in out.iter_mut().zip(shown.drain(..n)) {
        *slot = record;
    }
    drop(shown);
    n
}

/// A present clock's period, `num / den` ns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentClock {
    pub num: u64,
    pub den: u64,
}

/// Set the host's present clock (`den == 0`: none). Devices created afterwards take it.
pub fn set(num: u64, den: u64) {
    DEN.store(0, Ordering::Release);
    NUM.store(num, Ordering::Release);
    DEN.store(if num == 0 { 0 } else { den }, Ordering::Release);
}

/// The host's present clock, `None` when it paces nothing.
#[must_use]
pub fn get() -> Option<PresentClock> {
    let den = DEN.load(Ordering::Acquire);
    let num = NUM.load(Ordering::Acquire);
    (den != 0 && num != 0).then_some(PresentClock { num, den })
}

#[cfg(test)]
mod tests;
