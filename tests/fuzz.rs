//! Port of fuzz.c as a bounded integration test. Two endpoints exchange real traffic
//! across a simulated unreliable link, so the full send -> fragment -> reassemble -> ack
//! path is exercised, not just receive() of random bytes.
//!
//! On top of valid traffic it layers adversarial behaviour: packet loss, reordering,
//! duplication, single-bit corruption of otherwise-valid packets, and injection of fully
//! random packets. Any out-of-bounds access or overflow panics and fails the test.

mod common;

use common::Rng;
use reliable::{Config, Endpoint};

const MAX_PACKET_BYTES: usize = 16 * 1024;
const MAX_QUEUE: usize = 8192;
const NUM_ITERATIONS: usize = 20000;

/// Simulated network link: packets the transmit closure hands us are copied into a
/// bounded queue, then flushed (with reordering / loss / duplication / corruption) into
/// the destination endpoint once per iteration.
#[derive(Default)]
struct Link {
    packets: Vec<Vec<u8>>,
}

impl Link {
    fn queue(&mut self, data: &[u8]) {
        if self.packets.len() == MAX_QUEUE || data.is_empty() {
            return;
        }
        self.packets.push(data.to_vec());
    }

    fn flush(&mut self, rng: &mut Rng, to: &mut Endpoint) {
        // fisher-yates shuffle for reordering
        for i in (1..self.packets.len()).rev() {
            let j = rng.next_usize(i + 1);
            self.packets.swap(i, j);
        }

        for mut data in self.packets.drain(..) {
            if rng.range(0, 99) < 10 {
                // 10% packet loss
                continue;
            }

            if rng.range(0, 99) < 20 {
                // 20% single-bit corruption
                let index = rng.next_usize(data.len());
                data[index] ^= 1 << rng.range(0, 7);
            }

            let copies = if rng.range(0, 99) < 5 { 2 } else { 1 }; // 5% duplication
            for _ in 0..copies {
                to.receive_packet(&data, |_, _| true);
            }
        }
    }
}

fn random_packet_data(rng: &mut Rng) -> Vec<u8> {
    let packet_bytes = rng.range(1, MAX_PACKET_BYTES);
    (0..packet_bytes).map(|_| rng.next_u8()).collect()
}

fn send_random_packet(rng: &mut Rng, endpoint: &mut Endpoint, link: &mut Link) {
    let packet_data = random_packet_data(rng);
    endpoint.send_packet(&packet_data, |_, data| link.queue(data));
}

fn inject_random_packet(rng: &mut Rng, endpoint: &mut Endpoint) {
    let packet_data = random_packet_data(rng);
    endpoint.receive_packet(&packet_data, |_, _| true);
}

#[test]
fn fuzz() {
    let mut rng = Rng::new(12345);
    let mut time = 100.0;
    let delta_time = 0.1;

    let mut client = Endpoint::new(
        Config {
            name: "client".to_string(),
            ..Config::default()
        },
        time,
    );
    let mut server = Endpoint::new(
        Config {
            name: "server".to_string(),
            ..Config::default()
        },
        time,
    );

    let mut client_to_server = Link::default();
    let mut server_to_client = Link::default();

    for _ in 0..NUM_ITERATIONS {
        // real traffic in both directions (most large enough to fragment)
        send_random_packet(&mut rng, &mut client, &mut client_to_server);
        send_random_packet(&mut rng, &mut server, &mut server_to_client);

        // deliver queued traffic across the lossy / reordering / corrupting link
        client_to_server.flush(&mut rng, &mut server);
        server_to_client.flush(&mut rng, &mut client);

        // inject fully random packets straight into receive (the classic fuzz path)
        inject_random_packet(&mut rng, &mut client);
        inject_random_packet(&mut rng, &mut server);

        client.update(time);
        server.update(time);

        client.clear_acks();
        server.clear_acks();

        time += delta_time;
    }
}
