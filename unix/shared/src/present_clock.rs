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
    sync::atomic::{AtomicU64, Ordering},
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
