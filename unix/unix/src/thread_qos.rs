//! The QoS class of the native threads that produce frames.
//!
//! The encoder, submit and presenter threads are on every frame's critical
//! path: a frame the API thread reads back (`GetRenderTargetData` then
//! `LockRect`) or polls (`GetData`) waits for them, and the display waits for
//! the presenter. At the default class the scheduler may run them late or on an
//! efficiency core, and those waits grow from a fraction of a millisecond to a
//! whole display refresh (measured on a 120 Hz game: 8 to 16 ms
//! `GetData`/`LockRect` stalls every few seconds, none in this class). Apple's
//! guidance puts the threads that produce frames in the user-interactive class.

use super::LOG_TARGET;

/// Put the calling thread in the user-interactive QoS class.
pub fn frame_critical_thread() {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: plain libc call on the calling thread; no pointers.
        let rc = unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
        };
        if rc != 0 {
            log::warn!(target: LOG_TARGET, "could not raise a frame thread's QoS to user-interactive ({rc})");
        }
    }
}
