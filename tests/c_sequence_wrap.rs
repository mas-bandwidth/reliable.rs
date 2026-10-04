//! Port of test_sequence_wrap from the C original: a round trip long enough to cross the
//! u16 sequence wrap, where every sequence number on either side of the wrap is acked
//! like any other.

mod common;

use reliable::{Config, Endpoint};

const NUM_ITERATIONS: usize = 65536 + 64;

/// Port of test_sequence_wrap (reliable.c 3867-3920).
#[test]
fn sequence_wrap() {
    let time = 100.0;

    // C: test_pair_create checks both endpoints were created -- no Rust equivalent:
    // Endpoint::new cannot fail (an invalid config panics instead of returning NULL)
    let mut sender = Endpoint::new(
        Config {
            name: "sender".to_string(),
            ..Config::default()
        },
        time,
    );
    let mut receiver = Endpoint::new(
        Config {
            name: "receiver".to_string(),
            ..Config::default()
        },
        time,
    );

    let mut wrap_acked = [false; 65536];

    for _ in 0..NUM_ITERATIONS {
        let packet = [0u8; 8];

        sender.send_packet(&packet, |_, data| {
            receiver.receive_packet(data, |_, _| true);
        });
        receiver.send_packet(&packet, |_, data| {
            sender.receive_packet(data, |_, _| true);
        });

        for &ack in sender.acks() {
            wrap_acked[ack as usize] = true;
        }

        sender.clear_acks();
        receiver.clear_acks();
    }

    assert_eq!(sender.next_packet_sequence(), NUM_ITERATIONS as u16);

    // the sequences either side of the wrap were acked like any others

    assert!(wrap_acked[65533]);
    assert!(wrap_acked[65534]);
    assert!(wrap_acked[65535]);
    assert!(wrap_acked[0]);
    assert!(wrap_acked[1]);
    assert!(wrap_acked[2]);

    for &acked in wrap_acked.iter() {
        assert!(acked);
    }

    assert_eq!(
        receiver.counters().num_packets_received,
        NUM_ITERATIONS as u64
    );
}
