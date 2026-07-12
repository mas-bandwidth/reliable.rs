//! Port of fuzz.c. This fuzzer drives two endpoints exchanging real traffic across an
//! unreliable link, so it exercises the full send -> fragment -> corrupt -> reassemble
//! path (the interesting parser and reassembly code), not just receive() of random bytes.
//!
//! On top of valid traffic it layers adversarial behaviour: packet loss, reordering,
//! duplication, single-bit corruption of otherwise-valid packets, and injection of fully
//! random packets. Any out-of-bounds access or overflow panics and fails the run.
//!
//! Usage: fuzz [num_iterations] [seed]   (num_iterations <= 0 runs until Ctrl-C)

use reliable::{Config, Endpoint};

const MAX_PACKET_BYTES: usize = 16 * 1024;
const MAX_QUEUE: usize = 8192;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn next_u8(&mut self) -> u8 {
        (self.next_u64() >> 56) as u8
    }

    fn next_usize(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// random integer in [a, b] inclusive
    fn range(&mut self, a: usize, b: usize) -> usize {
        a + self.next_usize(b - a + 1)
    }
}

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

fn main() {
    println!("[fuzz]");

    let args: Vec<String> = std::env::args().collect();

    let num_iterations: i64 = args.get(1).and_then(|arg| arg.parse().ok()).unwrap_or(-1);

    let seed: u64 = args
        .get(2)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time")
                .as_secs()
        });

    println!("seed = {seed}");

    let mut rng = Rng::new(seed);
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

    let mut i: i64 = 0;
    while num_iterations <= 0 || i < num_iterations {
        if i % 1000 == 0 {
            print!(".");
            use std::io::Write;
            std::io::stdout().flush().expect("flush stdout");
        }

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
        i += 1;
    }

    println!("\nshutdown");
}
