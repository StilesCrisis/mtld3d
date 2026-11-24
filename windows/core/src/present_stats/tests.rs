use super::*;

/// A sink nothing was published into reads all zero, and counts no present.
#[test]
fn a_new_sink_reads_zero() {
    let sink = PresentStatsSink::new();
    assert_eq!(sink.read(), PresentStats::default());
    assert_eq!(sink.last_present_count(), 0);
}

/// A publication reads back whole, and the next one replaces it.
#[test]
fn a_publication_reads_back_whole() {
    let sink = PresentStatsSink::new();
    sink.publish(&PresentStats {
        present_count: 7,
        refresh_count: 9,
        sync_qpc: 0x1_0000_0002,
    });
    assert_eq!(
        sink.read(),
        PresentStats {
            present_count: 7,
            refresh_count: 9,
            sync_qpc: 0x1_0000_0002,
        }
    );
    sink.publish(&PresentStats {
        present_count: 8,
        refresh_count: 11,
        sync_qpc: 5,
    });
    assert_eq!(
        sink.read(),
        PresentStats {
            present_count: 8,
            refresh_count: 11,
            sync_qpc: 5,
        }
    );
}

/// Each noted present moves `GetLastPresentCount` by one, and no publication touches it.
#[test]
fn noted_presents_count_apart_from_publications() {
    let sink = PresentStatsSink::new();
    sink.note_present();
    sink.note_present();
    sink.publish(&PresentStats {
        present_count: 1,
        refresh_count: 1,
        sync_qpc: 1,
    });
    assert_eq!(sink.last_present_count(), 2);
}

/// A reader racing a writer only ever sees one publication's three words together.
#[test]
fn a_racing_reader_never_sees_a_torn_publication() {
    const PUBLICATIONS: u32 = 20_000;
    let sink = PresentStatsSink::new();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for n in 1..=PUBLICATIONS {
                sink.publish(&PresentStats {
                    present_count: n,
                    refresh_count: n,
                    sync_qpc: u64::from(n),
                });
            }
        });
        loop {
            let stats = sink.read();
            assert_eq!(stats.present_count, stats.refresh_count, "torn: {stats:?}");
            assert_eq!(
                u64::from(stats.present_count),
                stats.sync_qpc,
                "torn: {stats:?}"
            );
            if stats.present_count == PUBLICATIONS {
                break;
            }
        }
    });
}
