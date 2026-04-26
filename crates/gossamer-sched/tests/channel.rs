//! Tests covering channel send/recv semantics and `select` polling.

use gossamer_sched::{Channel, Gid, RecvResult, SelectOp, SelectOutcome, SendResult, poll_select};

#[test]
fn buffered_channel_accepts_sends_up_to_capacity() {
    let mut chan: Channel<i32> = Channel::buffered(2);
    assert_eq!(chan.try_send(1).unwrap(), SendResult::Sent);
    assert_eq!(chan.try_send(2).unwrap(), SendResult::Sent);
    assert!(chan.try_send(3).is_err());
    assert_eq!(chan.len(), 2);
}

#[test]
fn buffered_channel_round_trips_values() {
    let mut chan: Channel<&'static str> = Channel::buffered(3);
    for msg in ["a", "b", "c"] {
        assert_eq!(chan.try_send(msg).unwrap(), SendResult::Sent);
    }
    let collected: Vec<_> = (0..3)
        .map(|_| match chan.try_recv() {
            RecvResult::Value(v) => v,
            other => panic!("unexpected: {other:?}"),
        })
        .collect();
    assert_eq!(collected, ["a", "b", "c"]);
}

#[test]
fn unbuffered_channel_has_no_capacity() {
    let mut chan: Channel<i32> = Channel::unbuffered();
    assert!(chan.try_send(1).is_err());
    assert_eq!(chan.try_recv(), RecvResult::<i32>::WouldBlock);
}

#[test]
fn closed_channel_drains_then_reports_closed() {
    let mut chan: Channel<i32> = Channel::buffered(2);
    chan.try_send(7).unwrap();
    chan.close();
    assert_eq!(chan.try_recv(), RecvResult::Value(7));
    assert_eq!(chan.try_recv(), RecvResult::<i32>::Closed);
    assert_eq!(chan.try_send(99).unwrap(), SendResult::Closed);
}

#[test]
fn parking_stores_goroutine_ids_for_later_wakeup() {
    let mut chan: Channel<i32> = Channel::unbuffered();
    chan.park_sender(Gid(1));
    chan.park_sender(Gid(2));
    assert_eq!(chan.parked_senders(), &[Gid(1), Gid(2)]);
    assert_eq!(chan.wake_sender(), Some(Gid(1)));
    assert_eq!(chan.wake_sender(), Some(Gid(2)));
    assert_eq!(chan.wake_sender(), None);
}

#[test]
fn select_picks_first_ready_recv() {
    let mut a: Channel<i32> = Channel::buffered(1);
    let mut b: Channel<i32> = Channel::buffered(1);
    b.try_send(42).unwrap();
    let outcome = poll_select(vec![
        SelectOp::Recv { chan: &mut a },
        SelectOp::Recv { chan: &mut b },
    ]);
    assert_eq!(
        outcome,
        SelectOutcome::Received {
            index: 1,
            value: 42
        }
    );
}

#[test]
fn select_reports_would_block_when_no_arm_ready() {
    let mut a: Channel<i32> = Channel::buffered(1);
    let mut b: Channel<i32> = Channel::buffered(1);
    let outcome = poll_select::<i32>(vec![
        SelectOp::Recv { chan: &mut a },
        SelectOp::Recv { chan: &mut b },
    ]);
    assert_eq!(outcome, SelectOutcome::WouldBlock);
}

#[test]
fn select_can_report_closed_arm() {
    let mut a: Channel<i32> = Channel::buffered(1);
    a.close();
    let outcome = poll_select::<i32>(vec![SelectOp::Recv { chan: &mut a }]);
    assert_eq!(outcome, SelectOutcome::Closed { index: 0 });
}

#[test]
fn select_can_send_on_ready_channel() {
    let mut chan: Channel<i32> = Channel::buffered(1);
    let outcome = poll_select(vec![SelectOp::Send {
        chan: &mut chan,
        value: 5,
    }]);
    assert_eq!(outcome, SelectOutcome::Sent { index: 0 });
    assert_eq!(chan.try_recv(), RecvResult::Value(5));
}
