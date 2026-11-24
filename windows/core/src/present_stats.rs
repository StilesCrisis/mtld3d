//! What `IDirect3DSwapChain9Ex::GetPresentStats` and `GetLastPresentCount` report.
//!
//! A device's presents are numbered from 1 in the order the game made them,
//! on both sides of the boundary: the API side counts the `Present` calls
//! that hand a frame to the window ([`PresentStatsSink::note_present`]), and
//! the presenter numbers the present packets it takes for that window in the
//! same order, one packet per such call.
//!
//! When the display shows a drawable, its presented handler publishes the
//! number of the present it carried, the refresh it was shown on and that
//! refresh's time on the application's `QueryPerformanceCounter`, as one
//! [`PresentStats`]. The refresh count is the display's: it advances by the
//! refreshes that passed between two shown presents,
//! so a present shown one refresh late moves it one further than the present
//! count, which is how a game counts the refreshes it missed (the presenter's
//! `refreshes_between`).
//!
//! The three words are written by one writer at a time (the unix side
//! publishes under its attachment registry lock) and read by the API thread,
//! through a sequence word, so a reader never sees half of one publication.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

/// One publication: the last present the display showed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PresentStats {
    /// The number of the present shown, counted from 1 in the order the game made them; 0 = none yet.
    pub present_count: u32,
    /// The display refreshes counted up to the one the present was shown on.
    pub refresh_count: u32,
    /// The time of that refresh on the application's `QueryPerformanceCounter`, 0 = unknown.
    pub sync_qpc: u64,
}

/// One device's present statistics, shared between the API side and the presenter.
///
/// The device keeps it at one address for its lifetime and hands that
/// address to the unix side at attach
/// (`AttachMetalLayerParams::present_stats_ptr`).
pub struct PresentStatsSink {
    /// Odd while a publication is being written; bumped twice by each.
    sequence: AtomicU32,
    present_count: AtomicU32,
    refresh_count: AtomicU32,
    sync_qpc: AtomicU64,
    /// The `Present` calls that handed a frame to the window, `GetLastPresentCount`.
    presents: AtomicU32,
}

impl Default for PresentStatsSink {
    fn default() -> Self {
        Self::new()
    }
}

impl PresentStatsSink {
    /// No present made and none shown.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sequence: AtomicU32::new(0),
            present_count: AtomicU32::new(0),
            refresh_count: AtomicU32::new(0),
            sync_qpc: AtomicU64::new(0),
            presents: AtomicU32::new(0),
        }
    }

    /// Count one `Present` that handed a frame to the window; the API thread alone calls it.
    pub fn note_present(&self) {
        self.presents.fetch_add(1, Ordering::Relaxed);
    }

    /// The number of the last `Present` that handed a frame to the window, 0 before the first.
    #[must_use]
    pub fn last_present_count(&self) -> u32 {
        self.presents.load(Ordering::Relaxed)
    }

    /// Publish what the display showed last.
    ///
    /// One writer at a time: the caller serialises publications to this sink.
    pub fn publish(&self, stats: &PresentStats) {
        self.sequence.fetch_add(1, Ordering::Relaxed);
        fence(Ordering::Release);
        self.present_count
            .store(stats.present_count, Ordering::Relaxed);
        self.refresh_count
            .store(stats.refresh_count, Ordering::Relaxed);
        self.sync_qpc.store(stats.sync_qpc, Ordering::Relaxed);
        self.sequence.fetch_add(1, Ordering::Release);
    }

    /// The last publication whole, all zero before the first.
    #[must_use]
    pub fn read(&self) -> PresentStats {
        loop {
            let before = self.sequence.load(Ordering::Acquire);
            if before % 2 == 1 {
                core::hint::spin_loop();
                continue;
            }
            let stats = PresentStats {
                present_count: self.present_count.load(Ordering::Relaxed),
                refresh_count: self.refresh_count.load(Ordering::Relaxed),
                sync_qpc: self.sync_qpc.load(Ordering::Relaxed),
            };
            fence(Ordering::Acquire);
            if self.sequence.load(Ordering::Relaxed) == before {
                return stats;
            }
        }
    }
}

#[cfg(test)]
mod tests;
