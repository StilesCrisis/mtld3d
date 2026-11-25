//! The scheduling priority of the native threads that produce frames.
//!
//! The encoder, submit and presenter threads are on every frame's critical
//! path: a frame the API thread reads back (`GetRenderTargetData` then
//! `LockRect`) or polls (`GetData`) waits for them, and the display waits for
//! the presenter. The host chooses their priority before it creates a device
//! ([`set_frame_thread_priority`], the native host's
//! `d3d9metal_set_frame_thread_priority`): normal (the default class, and the
//! default) or high (user-interactive, the class Apple gives the threads
//! that produce frames: the last to be held off a core; macOS still picks the
//! cores). Each thread takes the choice when it starts.

use std::sync::atomic::{AtomicBool, Ordering};

use super::LOG_TARGET;

static HIGH: AtomicBool = AtomicBool::new(false);

/// Choose the frame threads' priority: `true` for high, `false` for normal.
/// Threads started afterwards take it.
#[cfg_attr(not(feature = "native-host"), allow(dead_code, reason = "only a native host chooses"))]
pub fn set_frame_thread_priority(high: bool) {
    HIGH.store(high, Ordering::Release);
}

/// At the start of a frame thread: the chosen priority (nothing to do for normal).
pub fn frame_thread_start() {
    if !HIGH.load(Ordering::Acquire) {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: plain libc call on the calling thread; no pointers.
        let rc = unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
        };
        if rc == 0 {
            log::info!(
                target: LOG_TARGET,
                "{}: high priority (user-interactive)",
                std::thread::current().name().unwrap_or("frame thread")
            );
        } else {
            log::warn!(target: LOG_TARGET, "could not raise a frame thread's priority ({rc})");
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// The calling thread's scheduling class, as its number (`qos_class_t` has no `PartialEq`).
    fn own_class() -> u32 {
        let mut class = libc::qos_class_t::QOS_CLASS_UNSPECIFIED;
        // SAFETY: no arguments; the calling thread's own handle.
        let me = unsafe { libc::pthread_self() };
        // SAFETY: that thread's class, written to a local; the priority out-pointer may be null.
        unsafe { libc::pthread_get_qos_class_np(me, &raw mut class, std::ptr::null_mut()) };
        class as u32
    }

    #[test]
    fn a_thread_started_after_the_choice_takes_it() {
        set_frame_thread_priority(true);
        let high = std::thread::spawn(|| {
            frame_thread_start();
            own_class()
        })
        .join()
        .unwrap();
        assert_eq!(high, libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE as u32);
        set_frame_thread_priority(false);
        let normal = std::thread::spawn(|| {
            frame_thread_start();
            own_class()
        })
        .join()
        .unwrap();
        assert_ne!(normal, libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE as u32);
    }
}
