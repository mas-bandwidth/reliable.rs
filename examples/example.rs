//! Port of example.c: two endpoints exchange packets over a simulated network with
//! heavy packet loss, printing acks as they arrive.

use reliable::{Config, Endpoint};

// tiny deterministic xorshift64* prng so the example needs no dependencies
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

fn main() {
    println!("\nreliable example\n");

    let mut time = 0.0;

    // configure the endpoints

    let config = Config {
        max_packet_size: 32 * 1024, // maximum packet size that may be sent in bytes
        fragment_above: 1200,       // fragment and reassemble packets above this size
        max_fragments: 32,          // maximum number of fragments per-packet
        fragment_size: 1024,        // the size of each fragment sent
        ..Config::default()
    };

    let mut client = Endpoint::new(
        Config {
            name: "client".to_string(),
            ..config.clone()
        },
        time,
    );
    let mut server = Endpoint::new(
        Config {
            name: "server".to_string(),
            ..config
        },
        time,
    );

    // send packets and print out acks

    let mut rng = Rng(0x1234_5678);
    let packet = [0u8; 8];

    for i in 0..1000 {
        let client_packet_sequence = client.next_packet_sequence();

        // simulate 90% packet loss

        client.send_packet(&packet, |_, data| {
            if rng.next_u64().is_multiple_of(10) {
                server.receive_packet(data, |_, _| true);
            }
        });
        server.send_packet(&packet, |_, data| {
            if rng.next_u64().is_multiple_of(10) {
                client.receive_packet(data, |_, _| true);
            }
        });

        client.update(time);
        server.update(time);

        println!("{i}: client sent packet {client_packet_sequence}");

        for ack in client.acks() {
            println!(" --> server acked packet {ack}");
        }

        client.clear_acks();
        server.clear_acks();

        time += 0.01;
    }

    println!("\nSuccess\n");
}
