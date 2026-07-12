//! Port of stats.c: two endpoints exchange fixed-size packets with every 5th packet
//! dropped, printing counters, rtt, jitter, packet loss and bandwidth each iteration.
//!
//! Usage: stats [num_iterations]

use reliable::{Config, Endpoint};

const MAX_PACKET_BYTES: usize = 290;

fn generate_packet_data(sequence: u16) -> Vec<u8> {
    let mut packet_data = vec![0u8; MAX_PACKET_BYTES];
    packet_data[0] = (sequence & 0xFF) as u8;
    packet_data[1] = ((sequence >> 8) & 0xFF) as u8;
    for (i, byte) in packet_data.iter_mut().enumerate().skip(2) {
        *byte = ((i + sequence as usize) % 256) as u8;
    }
    packet_data
}

fn check_packet_data(packet_data: &[u8]) {
    assert_eq!(packet_data.len(), MAX_PACKET_BYTES);
    let sequence = u16::from_le_bytes([packet_data[0], packet_data[1]]);
    for (i, &byte) in packet_data.iter().enumerate().skip(2) {
        assert_eq!(byte, ((i + sequence as usize) % 256) as u8);
    }
}

fn stats_iteration(time: f64, client: &mut Endpoint, server: &mut Endpoint) {
    {
        let sequence = client.next_packet_sequence();
        let packet_data = generate_packet_data(sequence);
        client.send_packet(&packet_data, |sequence, data| {
            // drop every 5th packet
            if sequence % 5 != 0 {
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
        server.send_packet(&packet_data, |sequence, data| {
            if sequence % 5 != 0 {
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

    let counters = client.counters();
    let bandwidth = client.bandwidth();

    println!(
        "{} sent | {} received | {} acked | rtt = {}ms | jitter = {}ms | packet loss = {}% | sent = {}kbps | recv = {}kbps | acked = {}kbps",
        counters.num_packets_sent,
        counters.num_packets_received,
        counters.num_packets_acked,
        client.rtt_min() as i32,
        client.jitter_avg_vs_min_rtt() as i32,
        (client.packet_loss() + 0.5) as i32,
        bandwidth.sent_kbps as i32,
        bandwidth.received_kbps as i32,
        bandwidth.acked_kbps as i32,
    );
}

fn main() {
    let num_iterations: i64 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(-1);

    println!("initializing");

    let mut time = 100.0;
    let delta_time = 0.01;

    let mut client = Endpoint::new(
        Config {
            name: "client".to_string(),
            fragment_above: MAX_PACKET_BYTES,
            ..Config::default()
        },
        time,
    );
    let mut server = Endpoint::new(
        Config {
            name: "server".to_string(),
            fragment_above: MAX_PACKET_BYTES,
            ..Config::default()
        },
        time,
    );

    let mut i: i64 = 0;
    while num_iterations <= 0 || i < num_iterations {
        stats_iteration(time, &mut client, &mut server);
        time += delta_time;
        i += 1;
    }

    println!("shutdown");
}
