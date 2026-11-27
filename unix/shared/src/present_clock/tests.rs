use super::*;

#[test]
fn a_clock_is_set_read_and_cleared() {
    set(2_080 * 1_144 * 1_000_000_000, 285_500_000);
    assert_eq!(
        get(),
        Some(PresentClock {
            num: 2_379_520_000_000_000,
            den: 285_500_000
        })
    );
    set(0, 0);
    assert_eq!(get(), None);
    set(1_000_000_000, 0);
    assert_eq!(get(), None, "a zero denominator names no clock");
}

#[test]
fn a_present_time_is_taken_once_by_its_thread() {
    set_next_present_at(1_234);
    let elsewhere = std::thread::spawn(take_next_present_at).join().unwrap();
    assert_eq!(elsewhere, 0, "another thread's Present takes nothing");
    assert_eq!(take_next_present_at(), 1_234);
    assert_eq!(take_next_present_at(), 0, "taken once");
}

#[test]
fn shown_records_are_taken_oldest_first() {
    let mut out = [Shown::default(), Shown::default()];
    take_shown(&mut out);
    note_shown(10, 1.5);
    note_shown(20, 0.0);
    note_shown(30, 2.5);
    assert_eq!(take_shown(&mut out), 2);
    assert_eq!(
        out[0],
        Shown {
            present_at_ns: 10,
            presented_s: 1.5
        }
    );
    assert_eq!(
        out[1],
        Shown {
            present_at_ns: 20,
            presented_s: 0.0
        }
    );
    assert_eq!(take_shown(&mut out), 1);
    assert_eq!(out[0].present_at_ns, 30);
}
