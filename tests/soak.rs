//! Port of soak.c as a bounded integration test: two endpoints exchange randomly sized
//! packets (most large enough to fragment) across a lossy link, validating the contents
//! of every delivered packet.

mod common;

use common::Rng;
use reliable::{Config, Endpoint};

const MAX_PACKET_BYTES: usize = 16 * 1024;
const NUM_ITERATIONS: usize = 8192;

fn generate_packet_data(sequence: u16) -> Vec<u8> {
    let packet_bytes = (sequence as usize * 1023) % (MAX_PACKET_BYTES - 2) + 2;
    let mut packet_data = vec![0u8; packet_bytes];
    packet_data[0] = (sequence & 0xFF) as u8;
    packet_data[1] = ((sequence >> 8) & 0xFF) as u8;
    for (i, byte) in packet_data.iter_mut().enumerate().skip(2) {
        *byte = ((i + sequence as usize) % 256) as u8;
    }
    packet_data
}

fn check_packet_data(packet_data: &[u8]) {
    assert!(packet_data.len() >= 2);
    assert!(packet_data.len() <= MAX_PACKET_BYTES);
    let sequence = u16::from_le_bytes([packet_data[0], packet_data[1]]);
    assert_eq!(
        packet_data.len(),
        (sequence as usize * 1023) % (MAX_PACKET_BYTES - 2) + 2
    );
    for (i, &byte) in packet_data.iter().enumerate().skip(2) {
        assert_eq!(byte, ((i + sequence as usize) % 256) as u8);
    }
}

#[test]
fn soak() {
    let mut rng = Rng::new(0x1234_5678);
    let mut time = 100.0;
    let delta_time = 0.1;

    let mut client = Endpoint::new(
        Config {
            name: "client".to_string(),
            fragment_above: 500,
            ..Config::default()
        },
        time,
    );
    let mut server = Endpoint::new(
        Config {
            name: "server".to_string(),
            fragment_above: 500,
            ..Config::default()
        },
        time,
    );

    for _ in 0..NUM_ITERATIONS {
        {
            let sequence = client.next_packet_sequence();
            let packet_data = generate_packet_data(sequence);
            client.send_packet(&packet_data, |_, data| {
                // 5% packet loss
                if rng.range(0, 100) >= 5 {
                    server.receive_packet(data, |_, data| {
                        check_packet_data(data);
                        true
                    });
                }
            });
        }

        {
            let sequence = server.next_packet_sequence();
            let packet_data = generate_packet_data(sequence);
            server.send_packet(&packet_data, |_, data| {
                if rng.range(0, 100) >= 5 {
                    client.receive_packet(data, |_, data| {
                        check_packet_data(data);
                        true
                    });
                }
            });
        }

        client.update(time);
        server.update(time);

        client.clear_acks();
        server.clear_acks();

        time += delta_time;
    }

    assert!(client.counters().num_packets_received > 0);
    assert!(server.counters().num_packets_received > 0);
}
