//! Port of test_rtt from the C original: 1000 round trips over a live pair of endpoints,
//! then the rtt statistics they report are sane.

mod common;

use reliable::{Config, Endpoint};

/// Port of test_rtt (reliable.c 3344-3397).
#[test]
fn rtt() {
    let mut time = 100.0;
    let delta_time = 0.01;

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

    for _ in 0..1000 {
        let dummy_packet = [0u8; 8];

        sender.send_packet(&dummy_packet, |_, data| {
            receiver.receive_packet(data, |_, _| true);
        });
        receiver.send_packet(&dummy_packet, |_, data| {
            sender.receive_packet(data, |_, _| true);
        });

        sender.update(time);
        receiver.update(time);

        time += delta_time;
    }

    let rtt = sender.rtt();
    let rtt_min = sender.rtt_min();
    let rtt_max = sender.rtt_max();
    let rtt_avg = sender.rtt_avg();

    // C: check(rtt == rtt && rtt >= 0): the rtt is finite and non-negative
    assert!(rtt.is_finite() && rtt >= 0.0);
    assert!(rtt_min >= 0.0 && rtt_min <= rtt_avg && rtt_avg <= rtt_max);
    // C: check(rtt_max < 1000.0): the rtt is in milliseconds
    assert!(rtt_max < 1000.0);
}
