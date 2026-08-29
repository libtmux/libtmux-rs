use super::*;
use std::time::{Duration, Instant};

use crate::text::TextFilter;

fn ring() -> Ring {
    Ring {
        bytes: Vec::new(),
        start: 0,
        checkpoint: TextFilter::new(),
        closed: false,
    }
}

fn tails_with_owner(owner: u128) -> Tails {
    Tails::with_owner(owner)
}

fn cursor_for(tails: &Tails, pane: &str, epoch: u64, offset: u64) -> Cursor {
    Cursor {
        pane: pane.to_owned(),
        owner: tails.owner().expect("the test owner is available"),
        epoch,
        offset,
    }
}

fn resume_for(tails: &Tails, ring: &Ring, cursor: Option<&Cursor>, epoch: u64) -> (u64, bool) {
    resume_at(
        ring,
        cursor,
        tails.owner().expect("the test owner is available"),
        epoch,
    )
}

#[test]
fn a_cursor_survives_a_round_trip() {
    let tails = tails_with_owner(1);
    let cursor = cursor_for(&tails, "%12", 3, 4096);

    assert_eq!(
        cursor.encode(),
        "%12:00000000000000000000000000000001:3:4096"
    );
    assert_eq!(Cursor::decode(&cursor.encode()), Ok(cursor));
}

#[test]
fn foreign_text_is_not_a_cursor() {
    assert!(Cursor::decode("nonsense").is_err());
    assert!(
        Cursor::decode("%1:1:0").is_err(),
        "old cursors lack an owner"
    );
    assert!(
        Cursor::decode("%1:notanowner:1:0").is_err(),
        "an owner is fixed-width hexadecimal"
    );
    assert!(
        Cursor::decode("1:00000000000000000000000000000001:2:3").is_err(),
        "a pane id always starts with %"
    );
}

#[test]
fn a_ring_reads_from_an_offset() {
    let mut ring = ring();
    ring.push(b"hello world");

    assert_eq!(ring.read_from(6), (&b"world"[..], false));
    assert_eq!(ring.end(), 11);
}

#[test]
fn reading_at_the_end_yields_nothing() {
    let mut ring = ring();
    ring.push(b"hello");

    assert_eq!(ring.read_from(5), (&b""[..], false));
}

#[test]
fn an_offset_past_the_end_admits_the_cursor_is_invalid() {
    let mut ring = ring();
    ring.push(b"hello");

    assert_eq!(ring.read_from(99), (&b""[..], true));
}

#[test]
fn overflow_drops_the_oldest_and_says_so() {
    let mut ring = ring();
    let mut stream = b"\x1b[31mred".to_vec();
    stream.resize(RING_BYTES + 4, b'a');
    ring.push(&stream);

    assert_eq!(ring.start, 4);
    assert_eq!(ring.end(), RING_BYTES as u64 + 4);
    let (bytes, missed) = ring.read_from(0);
    assert!(missed, "the bytes at offset 0 are gone");
    assert_eq!(bytes.len(), RING_BYTES);
    let read = ring.snapshot_from(0);
    let text = read.text();
    assert!(text.starts_with("red"));
    assert_eq!(text.len(), RING_BYTES - 1);
}

#[test]
fn a_first_read_resumes_at_the_end_and_has_missed_nothing() {
    let tails = tails_with_owner(1);
    let mut ring = ring();
    ring.push(b"written before anyone looked");

    assert_eq!(resume_for(&tails, &ring, None, 1), (ring.end(), false));
}

#[test]
fn a_cursor_from_this_tail_resumes_where_it_says() {
    let tails = tails_with_owner(1);
    let mut ring = ring();
    ring.push(b"hello");
    let cursor = cursor_for(&tails, "%1", 1, 2);

    assert_eq!(resume_for(&tails, &ring, Some(&cursor), 1), (2, false));
}

#[test]
fn a_cursor_from_an_evicted_tail_admits_the_gap() {
    let tails = tails_with_owner(1);
    let mut ring = ring();
    ring.push(b"written after the tail came back");
    let cursor = cursor_for(&tails, "%1", 1, 0);

    assert_eq!(
        resume_for(&tails, &ring, Some(&cursor), 2),
        (ring.end(), true),
        "an offset from a previous tail names a place in a buffer that is gone"
    );
}

#[test]
fn a_cursor_from_a_fresh_owner_admits_the_gap() {
    let first = tails_with_owner(1);
    let second = tails_with_owner(2);
    let mut ring = ring();
    ring.push(b"written after the server restarted");
    let cursor = cursor_for(&first, "%1", first.next_epoch(), 0);
    let epoch = second.next_epoch();

    assert_eq!(
        resume_for(&second, &ring, Some(&cursor), epoch),
        (ring.end(), true),
        "an offset from another cursor owner names a different buffer"
    );
}

#[test]
fn tail_epochs_increase_within_one_owner() {
    let tails = Tails::new(Arc::new(InstanceIdentity::new()));

    assert_eq!(tails.next_epoch(), 1);
    assert_eq!(tails.next_epoch(), 2);
}

#[test]
fn a_cursor_still_inside_the_ring_is_not_a_miss() {
    let mut ring = ring();
    ring.push(&vec![b'a'; RING_BYTES]);
    ring.push(b"tail");

    let (bytes, missed) = ring.read_from(ring.end() - 4);
    assert!(!missed);
    assert_eq!(bytes, b"tail");
}

#[test]
fn a_cursor_inside_a_control_sequence_resumes_its_state() {
    let mut ring = ring();
    ring.push(b"before\x1b[31");
    let cursor = ring.end();
    ring.push(b"mred");

    let read = ring.snapshot_from(cursor);

    assert_eq!(read.text(), "red");
    assert!(!read.missed);
}

#[test]
fn reusing_an_old_cursor_preserves_a_pending_return() {
    let mut ring = ring();
    ring.push(b"working\r");
    let cursor = ring.end();
    ring.push(b"done");

    assert_eq!(ring.snapshot_from(cursor).text(), "\ndone");

    ring.push(b"!\n");

    assert_eq!(ring.snapshot_from(cursor).text(), "\ndone!\n");
}

#[tokio::test]
async fn publishing_a_tail_evicts_the_exact_lru_and_aborts_its_reader() {
    let mut table = TailTable::new(2);
    let now = Instant::now();
    let make_tail = |epoch, last_read| {
        let reader = tokio::spawn(std::future::pending());
        let abort = reader.abort_handle();
        let (snapshots, _requests) = tokio::sync::mpsc::channel(1);
        (
            Tail {
                epoch,
                ring: Arc::new(Mutex::new(ring())),
                snapshots,
                reader,
                last_read,
            },
            abort,
        )
    };
    let (oldest, oldest_reader) = make_tail(
        1,
        now.checked_sub(Duration::from_secs(2))
            .expect("two seconds fit before now"),
    );
    let (newer, _newer_reader) = make_tail(
        2,
        now.checked_sub(Duration::from_secs(1))
            .expect("one second fits before now"),
    );
    let (replacement, _replacement_reader) = make_tail(3, now);
    table.insert("%oldest".to_owned(), oldest);
    table.insert("%newer".to_owned(), newer);

    table.insert("%replacement".to_owned(), replacement);

    assert!(!table.contains("%oldest"));
    assert!(table.contains("%newer"));
    assert!(table.contains("%replacement"));
    tokio::task::yield_now().await;
    assert!(oldest_reader.is_finished(), "eviction aborts its reader");

    table.remove_if_epoch("%replacement", 2);
    assert!(
        table.contains("%replacement"),
        "an old failure cannot remove a newer reader",
    );
    table.remove_if_epoch("%replacement", 3);
    assert!(!table.contains("%replacement"));
}

#[tokio::test]
async fn baseline_admission_is_bounded_and_fail_fast() {
    let (snapshots, _requests) = tokio::sync::mpsc::channel(1);
    let tail = RetainedTail {
        epoch: 1,
        ring: Arc::new(Mutex::new(ring())),
        snapshots,
    };
    let first = {
        let tail = tail.clone();
        tokio::spawn(async move { tail.snapshot().await })
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        while tail.snapshots.capacity() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the first baseline occupies the one-slot mailbox");

    assert!(
        matches!(tail.snapshot().await, Err(SnapshotError::Busy { limit: 1 })),
        "another baseline is refused rather than retained out of bounds",
    );

    first.abort();
    let _ = first.await;
}

#[tokio::test]
async fn opening_is_fail_fast_and_a_failed_replacement_preserves_tails() {
    use libtmux::ControlClientLimits;
    use libtmux::test::TestServer;

    let guard = TestServer::builder()
        .control_client_limits(
            ControlClientLimits::default()
                .max_clients(1)
                .acquire_timeout(Some(Duration::from_millis(200))),
        )
        .start()
        .await
        .expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("tail-opening").await.expect("session");
    let pane = session.panes().await.expect("panes").remove(0);
    let occupied = pane
        .stream_output()
        .await
        .expect("the only persistent-client slot is occupied");

    let opening = Arc::new(tails_with_owner(1));
    let first = {
        let opening = Arc::clone(&opening);
        let pane = pane.clone();
        let id = pane.id().to_string();
        tokio::spawn(async move { opening.ensure(&pane, &id).await })
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        while opening.opening.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the first open owns admission");

    let second = tokio::time::timeout(
        Duration::from_millis(50),
        opening.ensure(&pane, pane.id().as_ref()),
    )
    .await;
    assert!(
        second.is_ok_and(|result| {
            matches!(
                result,
                Err(TailError::OpeningAtCapacity {
                    limit: MAX_TAIL_OPENERS
                })
            )
        }),
        "a second first-time open must fail rather than queue"
    );
    first.abort();
    let _ = first.await;

    let full = tails_with_owner(2);
    for index in 0..MAX_TAILS {
        let (snapshots, _requests) = tokio::sync::mpsc::channel(1);
        hold(&full.inner).insert(
            format!("%fake-{index}"),
            Tail {
                epoch: u64::try_from(index).expect("eight indices fit"),
                ring: Arc::new(Mutex::new(ring())),
                snapshots,
                reader: tokio::spawn(std::future::pending()),
                last_read: Instant::now(),
            },
        );
    }

    let error = full
        .ensure(&pane, pane.id().as_ref())
        .await
        .expect_err("persistent-client admission is full");
    assert!(
        matches!(error, TailError::Tmux(Error::Overloaded { .. })),
        "{error:?}"
    );
    assert_eq!(
        hold(&full.inner).len(),
        MAX_TAILS,
        "a failed replacement must not evict a healthy tail"
    );

    occupied
        .shutdown()
        .await
        .expect("occupied stream shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}
