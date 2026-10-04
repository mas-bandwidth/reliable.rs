//! Port of test_fragment_counts from the C original: fragment counts at both ends of the
//! range, and a payload that is an exact multiple of the fragment size.

mod common;

use reliable::{Config, Endpoint};

/// Port of test_fragment_case, the helper behind test_fragment_counts (reliable.c
/// 3949-4010). The C harness reassembled payload lands in a fixed 64KiB capture buffer
/// with a bounds check on the way in; here it lands in a `Vec`.
fn fragment_case(
    fragment_size: usize,
    max_fragments: usize,
    packet_bytes: usize,
    expected_fragments: usize,
) {
    let config = |name: &str| Config {
        name: name.to_string(),
        fragment_size,
        max_fragments,
        max_packet_size: fragment_size * max_fragments,
        fragment_above: fragment_size,
        ..Config::default()
    };

    let mut sender = Endpoint::new(config("sender"), 100.0);
    let mut receiver = Endpoint::new(config("receiver"), 100.0);

    let packet: Vec<u8> = (0..packet_bytes).map(|i| ((i * 31) + 7) as u8).collect();

    let mut processed: Vec<Vec<u8>> = Vec::new();

    sender.send_packet(&packet, |_, data| {
        receiver.receive_packet(data, |_, data| {
            processed.push(data.to_vec());
            true
        });
    });

    assert_eq!(processed.len(), 1);
    assert_eq!(processed[0].len(), packet_bytes);
    assert_eq!(processed[0], packet);

    let fragments_sent = sender.counters().num_fragments_sent;
    let fragments_received = receiver.counters().num_fragments_received;

    if expected_fragments == 0 {
        // below the threshold: sent whole, no fragments at all
        assert_eq!(fragments_sent, 0);
        assert_eq!(fragments_received, 0);
    } else {
        assert_eq!(fragments_sent, expected_fragments as u64);
        assert_eq!(fragments_received, expected_fragments as u64);
    }
}

/// Port of test_fragment_counts (reliable.c 4020-4039).
#[test]
fn fragment_counts() {
    // an exact multiple of the fragment size, where the last fragment is full rather than a remainder
    fragment_case(512, 16, 512 * 4, 4);

    // the largest packet the config allows, again an exact multiple
    fragment_case(512, 16, 512 * 16, 16);

    // one fragment: above the threshold by a single byte
    fragment_case(512, 16, 513, 2);
    fragment_case(64, 256, 65, 2);

    // 256 fragments, the maximum the wire format can express
    fragment_case(64, 256, 64 * 256, 256);
    fragment_case(64, 256, 64 * 255 + 1, 256);

    // at or below the threshold the packet is not fragmented
    fragment_case(512, 16, 512, 0);
    fragment_case(512, 16, 1, 0);
}
