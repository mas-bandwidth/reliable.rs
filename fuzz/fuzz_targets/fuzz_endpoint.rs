//! libFuzzer harness for coverage-guided fuzzing, a port of the C library's
//! fuzz_target.c.
//!
//! The fuzz input is interpreted as a script of operations against a pair of endpoints
//! connected back to back: send a packet with attacker-chosen contents, or inject raw
//! bytes straight into receive. This reaches the packet header parser, fragment
//! reassembly, ack processing, and stale/duplicate rejection. Payloads are passed as
//! exact-size heap copies so out-of-bounds reads land outside the allocation.
//!
//! Run with: cargo +nightly fuzz run fuzz_endpoint

#![no_main]

use libfuzzer_sys::fuzz_target;
use reliable::{Config, Endpoint};

const MAX_PACKET_BYTES: usize = 16 * 1024;

fuzz_target!(|data: &[u8]| {
    let mut time = 100.0;

    // fragment aggressively so reassembly is exercised by small inputs
    let config = Config { fragment_above: 500, ..Config::default() };

    let mut client = Endpoint::new(config.clone(), time);
    let mut server = Endpoint::new(config, time);

    // the input is a sequence of operations: [op:1][length:2][payload:length]

    let mut p = data;

    while p.len() >= 3 {
        let op = p[0] % 4;
        let length = 1 + usize::from(u16::from_le_bytes([p[1], p[2]])) % MAX_PACKET_BYTES;
        p = &p[3..];

        let length = length.min(p.len());

        if length > 0 {
            let packet_data = p[..length].to_vec();

            match op {
                0 => client.send_packet(&packet_data, |_, data| {
                    server.receive_packet(data, |_, _| true);
                }),
                1 => server.send_packet(&packet_data, |_, data| {
                    client.receive_packet(data, |_, _| true);
                }),
                2 => client.receive_packet(&packet_data, |_, _| true),
                _ => server.receive_packet(&packet_data, |_, _| true),
            }

            p = &p[length..];
        }

        time += 0.01;

        client.update(time);
        server.update(time);

        client.clear_acks();
        server.clear_acks();
    }
});
