//! Port of test_fragment_reassembly_buffer_zeroed from the C original. The C test
//! poisons the receiver's allocations with 0xCC and checks the reassembly buffer
//! allocated for a partial reassembly is zeroed where fragment 0 never wrote; the Rust
//! port pins the observable half: a packet reassembled after a partial one carries no
//! byte of the earlier one.

mod common;

use reliable::{Config, Endpoint};

const TEST_MAX_PACKET_BYTES: usize = 4 * 1024;

fn generate_packet_data_with_size(sequence: u16, packet_bytes: usize) -> Vec<u8> {
    assert!(packet_bytes >= 2);
    assert!(packet_bytes <= TEST_MAX_PACKET_BYTES);
    let mut packet_data = vec![0u8; packet_bytes];
    packet_data[0] = (sequence & 0xFF) as u8;
    packet_data[1] = ((sequence >> 8) & 0xFF) as u8;
    for (i, byte) in packet_data.iter_mut().enumerate().skip(2) {
        *byte = ((i + sequence as usize) % 256) as u8;
    }
    packet_data
}

/// Port of test_fragment_reassembly_buffer_zeroed (reliable.c 3157-3225).
#[test]
fn fragment_reassembly_buffer_zeroed() {
    let time = 100.0;

    // C: check(sender) and check(receiver) -- no Rust equivalent: Endpoint::new cannot
    // fail (an invalid config panics instead of returning NULL)
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

    // C: from here on the receiver's allocations are poisoned with 0xCC, so a buffer
    // that is not explicitly zeroed is visibly non-zero -- no Rust equivalent: Config
    // has no allocator hooks, and the port allocates the reassembly buffer with
    // vec![0; ...], zeroed by construction

    // deliver only the first fragment: the reassembly buffer is allocated and fragment 0
    // is stored, but the tail of the buffer is never written

    let mut allow_packets = 1;

    let fragment_size = sender.config().fragment_size;
    let packet_bytes = fragment_size + fragment_size / 2;

    let earlier_sequence = sender.next_packet_sequence();
    let earlier_packet = generate_packet_data_with_size(earlier_sequence, packet_bytes);
    sender.send_packet(&earlier_packet, |_, data| {
        if allow_packets > 0 {
            allow_packets -= 1;
            receiver.receive_packet(data, |_, _| true);
        }
    });

    // C: the partial packet's reassembly data was found, with its packet buffer allocated
    // -- no Rust equivalent: the reassembly entry is private to the endpoint; the fragment
    // counter pins that the partial reassembly happened
    assert_eq!(receiver.counters().num_fragments_received, 1);

    // C: the last byte of that reassembly buffer is still zero, where fragment 0 never
    // wrote and the poison would have shown -- no Rust equivalent at that byte: the port
    // reads only the bytes its fragments wrote. the observable half is pinned instead:
    // a packet reassembled after a partial one carries no byte of the earlier one

    // the later packet's payload is the earlier one's complement, so no byte position of
    // the two payloads can agree and any stale byte of the earlier packet shows

    let later_packet: Vec<u8> = earlier_packet.iter().map(|byte| !byte).collect();

    let mut processed: Vec<Vec<u8>> = Vec::new();
    sender.send_packet(&later_packet, |_, data| {
        receiver.receive_packet(data, |_, data| {
            processed.push(data.to_vec());
            true
        });
    });

    assert_eq!(processed.len(), 1);
    assert_eq!(processed[0], later_packet);
}
