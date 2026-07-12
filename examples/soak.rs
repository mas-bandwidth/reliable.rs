//! Port of soak.c: two endpoints exchange randomly sized packets (most large enough to
//! fragment) across a lossy link forever, validating the contents of every delivered
//! packet.
//!
//! Usage: soak [num_iterations] [--quiet]

use reliable::{Config, Endpoint};

const MAX_PACKET_BYTES: usize = 16 * 1024;

struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// random integer in [a, b] inclusive
    fn range(&mut self, a: u64, b: u64) -> u64 {
        a + self.next_u64() % (b - a + 1)
    }
}

struct StdoutLogger;

impl log::Log for StdoutLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        println!("{}", record.args());
    }

    fn flush(&self) {}
}

static LOGGER: StdoutLogger = StdoutLogger;

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

fn soak_iteration(time: f64, client: &mut Endpoint, server: &mut Endpoint, rng: &mut Rng) {
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
}

fn main() {
    let mut num_iterations: i64 = -1;
    let mut quiet = false;

    for arg in std::env::args().skip(1) {
        if arg == "--quiet" {
            quiet = true;
        } else {
            num_iterations = arg.parse().unwrap_or(-1);
        }
    }

    println!("initializing");

    if !quiet {
        log::set_logger(&LOGGER).expect("set logger");
        log::set_max_level(log::LevelFilter::Debug);
    }

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

    let mut rng = Rng(0x1234_5678);

    let mut i: i64 = 0;
    while num_iterations <= 0 || i < num_iterations {
        soak_iteration(time, &mut client, &mut server, &mut rng);
        time += delta_time;
        i += 1;
    }

    println!("shutdown");
}
