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
