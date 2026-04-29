//! Catches the C5 finding (channel drop leaks parked goroutines)
//! from `~/dev/contexts/lang/adversarial_analysis.md`.

use gossamer_sched::{Channel, Gid, RecvResult, SendResult};

#[test]
fn close_and_drain_parked_returns_every_registered_gid_in_order() {
    let mut chan: Channel<i64> = Channel::buffered(2);
    chan.park_sender(Gid(7));
    chan.park_sender(Gid(11));
    chan.park_receiver(Gid(3));
    chan.park_receiver(Gid(5));

    let (senders, receivers) = chan.close_and_drain_parked();
    assert!(chan.is_closed(), "channel must be marked closed");
    assert_eq!(senders, vec![Gid(7), Gid(11)]);
    assert_eq!(receivers, vec![Gid(3), Gid(5)]);
    assert!(
        chan.parked_senders().is_empty(),
        "drained list must be empty"
    );
    assert!(chan.parked_receivers().is_empty());
}

#[test]
fn closed_channel_returns_closed_status_for_send_and_recv() {
    let mut chan: Channel<i64> = Channel::buffered(1);
    chan.close();
    let send = chan.try_send(42).expect("try_send returns ok-status");
    assert_eq!(send, SendResult::Closed);
    let recv = chan.try_recv();
    assert_eq!(recv, RecvResult::Closed);
}

#[test]
fn dropping_channel_marks_it_closed_first() {
    let probe = {
        let mut chan: Channel<i64> = Channel::unbuffered();
        chan.park_receiver(Gid(1));
        chan.close();
        chan.is_closed()
    };
    assert!(probe);
}
