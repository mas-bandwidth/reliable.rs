//! Port of test_truncated_packets from the C original: truncated packets and truncated
//! fragments are rejected rather than acted on, and the intact ones still arrive.

mod common;

use reliable::{Config, Endpoint, FRAGMENT_HEADER_BYTES};

fn config(name: &str) -> Config {
    Config {
        name: name.to_string(),
        fragment_size: 256,
        max_fragments: 16,
        max_packet_size: 256 * 16,
        fragment_above: 256,
        ..Config::default()
    }
}

/// Port of test_truncated_packets (reliable.c 4079-4166). The C harness captures the
/// packets the sender transmits and counts the payloads the receiver processes; here the
/// capture is a `Vec` and the count a plain counter.
#[test]
fn truncated_packets() {
    let mut captured: Vec<Vec<u8>> = Vec::new();
    let mut num_processed = 0;

    let mut sender = Endpoint::new(config("sender"), 100.0);
    let mut receiver = Endpoint::new(config("receiver"), 100.0);
    // C: check(sender) and check(receiver) -- no Rust equivalent: Endpoint::new cannot
    // fail (an invalid config panics instead of returning NULL)

    // an unfragmented packet, captured on the wire

    let packet = vec![0xABu8; 200];
    sender.send_packet(&packet, |_, data| captured.push(data.to_vec()));
    assert_eq!(captured.len(), 1);

    let whole_bytes = captured[0].len();

    // every truncation of it shorter than the header is rejected, and none is processed

    for truncated_bytes in 1..4 {
        receiver.receive_packet(&captured[0][..truncated_bytes], |_, _| {
            num_processed += 1;
            true
        });
    }

    assert_eq!(receiver.counters().num_packets_invalid, 3);
    assert_eq!(num_processed, 0);

    // the whole packet still arrives

    receiver.receive_packet(&captured[0][..whole_bytes], |_, _| {
        num_processed += 1;
        true
    });
    assert_eq!(num_processed, 1);

    // now a fragmented packet

    captured.clear();

    let large_packet = vec![0xCDu8; 1024];
    sender.send_packet(&large_packet, |_, data| captured.push(data.to_vec()));
    assert_eq!(captured.len(), 4);

    // a fragment truncated inside its header is rejected

    for truncated_bytes in 1..FRAGMENT_HEADER_BYTES {
        receiver.receive_packet(&captured[1][..truncated_bytes], |_, _| {
            num_processed += 1;
            true
        });
    }

    assert_eq!(receiver.counters().num_fragments_received, 0);
    assert_eq!(
        receiver.counters().num_fragments_invalid,
        (FRAGMENT_HEADER_BYTES - 1) as u64
    );

    // a fragment that is not the last one must carry exactly fragment_size bytes, so a
    // truncated body is rejected too

    receiver.receive_packet(&captured[1][..captured[1].len() - 1], |_, _| {
        num_processed += 1;
        true
    });
    assert_eq!(receiver.counters().num_fragments_received, 0);
    assert_eq!(
        receiver.counters().num_fragments_invalid,
        FRAGMENT_HEADER_BYTES as u64
    );
    assert_eq!(num_processed, 1);

    // the intact fragments reassemble

    for fragment in &captured {
        receiver.receive_packet(fragment, |_, _| {
            num_processed += 1;
            true
        });
    }

    assert_eq!(receiver.counters().num_fragments_received, 4);
    assert_eq!(num_processed, 2);
}
