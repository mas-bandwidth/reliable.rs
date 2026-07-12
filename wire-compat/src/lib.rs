//! Wire-compatibility tests: the Rust port exchanging packets with the vendored C
//! reference implementation of reliable 1.3.4, in process, via FFI.
//!
//! Two invariants are locked in here, run by CI on every push and pull request:
//!
//! 1. **Cross-feed**: a Rust endpoint and a C endpoint exchange bidirectional traffic
//!    (regular and fragmented). Every payload each side delivers is validated
//!    byte-for-byte, and both sides must ack everything the other sent.
//!
//! 2. **Identical transcripts**: a Rust endpoint pair and a C endpoint pair are driven
//!    through the same deterministic exchange. Every datagram either implementation
//!    puts on the wire must be byte-identical to its counterpart.
//!
//! This crate is test scaffolding, not part of the published package: it is the one
//! place unsafe code exists (FFI into the vendored C library), and the shim keeps that
//! surface to opaque pointers and function pointers only.

#[cfg(test)]
mod tests {
    use std::ffi::{c_int, c_void};

    use reliable::{Config, Endpoint};

    #[repr(C)]
    struct CEndpoint {
        _opaque: [u8; 0],
    }

    type TransmitFn = unsafe extern "C" fn(*mut c_void, u64, u16, *mut u8, c_int);
    type ProcessFn = unsafe extern "C" fn(*mut c_void, u64, u16, *mut u8, c_int) -> c_int;

    unsafe extern "C" {
        fn wire_compat_endpoint_create(
            fragment_above: c_int,
            id: u64,
            context: *mut c_void,
            transmit: TransmitFn,
            process: ProcessFn,
        ) -> *mut CEndpoint;
        fn reliable_endpoint_destroy(endpoint: *mut CEndpoint);
        fn reliable_endpoint_send_packet(
            endpoint: *mut CEndpoint,
            packet_data: *mut u8,
            packet_bytes: c_int,
        );
        fn reliable_endpoint_receive_packet(
            endpoint: *mut CEndpoint,
            packet_data: *mut u8,
            packet_bytes: c_int,
        );
        fn reliable_endpoint_next_packet_sequence(endpoint: *mut CEndpoint) -> u16;
        fn reliable_endpoint_get_acks(endpoint: *mut CEndpoint, num_acks: *mut c_int) -> *mut u16;
        fn reliable_endpoint_clear_acks(endpoint: *mut CEndpoint);
        fn reliable_endpoint_update(endpoint: *mut CEndpoint, time: f64);
    }

    const MAX_PACKET_BYTES: usize = 4 * 1024;
    const FRAGMENT_ABOVE: usize = 500;
    const NUM_ITERATIONS: usize = 300;

    /// Payload sizes cycle from 2 bytes up to 4KB, crossing the fragmentation threshold
    /// so the exchange covers regular packets and 1..4 fragment reassemblies.
    fn generate_payload(sequence: u16) -> Vec<u8> {
        let packet_bytes = (sequence as usize * 1023) % (MAX_PACKET_BYTES - 2) + 2;
        let mut payload = vec![0u8; packet_bytes];
        payload[0] = (sequence & 0xFF) as u8;
        payload[1] = ((sequence >> 8) & 0xFF) as u8;
        for (i, byte) in payload.iter_mut().enumerate().skip(2) {
            *byte = ((i + sequence as usize) % 256) as u8;
        }
        payload
    }

    fn payload_is_valid(payload: &[u8]) -> bool {
        if payload.len() < 2 {
            return false;
        }
        let sequence = u16::from_le_bytes([payload[0], payload[1]]);
        payload == generate_payload(sequence).as_slice()
    }

    /// Context handed to the C endpoint's callbacks. Transmitted datagrams are queued
    /// (and optionally forwarded straight to a peer C endpoint); processed payloads are
    /// validated and counted.
    struct CContext {
        transcript: Vec<Vec<u8>>,
        forward_to_peer: *mut CEndpoint,
        num_processed: usize,
        num_invalid_payloads: usize,
    }

    impl CContext {
        fn new() -> Box<Self> {
            Box::new(Self {
                transcript: Vec::new(),
                forward_to_peer: std::ptr::null_mut(),
                num_processed: 0,
                num_invalid_payloads: 0,
            })
        }
    }

    unsafe extern "C" fn c_transmit(
        context: *mut c_void,
        _id: u64,
        _sequence: u16,
        packet_data: *mut u8,
        packet_bytes: c_int,
    ) {
        unsafe {
            let context = &mut *(context as *mut CContext);
            let datagram = std::slice::from_raw_parts(packet_data, packet_bytes as usize);
            context.transcript.push(datagram.to_vec());
            if !context.forward_to_peer.is_null() {
                reliable_endpoint_receive_packet(
                    context.forward_to_peer,
                    packet_data,
                    packet_bytes,
                );
            }
        }
    }

    unsafe extern "C" fn c_process(
        context: *mut c_void,
        _id: u64,
        _sequence: u16,
        packet_data: *mut u8,
        packet_bytes: c_int,
    ) -> c_int {
        unsafe {
            let context = &mut *(context as *mut CContext);
            let payload = std::slice::from_raw_parts(packet_data, packet_bytes as usize);
            if !payload_is_valid(payload) {
                context.num_invalid_payloads += 1;
            }
            context.num_processed += 1;
        }
        1
    }

    fn drain_c_acks(endpoint: *mut CEndpoint, into: &mut Vec<u16>) {
        unsafe {
            let mut num_acks: c_int = 0;
            let acks = reliable_endpoint_get_acks(endpoint, &mut num_acks);
            into.extend(std::slice::from_raw_parts(acks, num_acks as usize));
            reliable_endpoint_clear_acks(endpoint);
        }
    }

    /// A Rust endpoint and a C endpoint exchange bidirectional traffic in lockstep.
    /// Both sides must deliver every payload intact and ack everything.
    #[test]
    fn cross_feed() {
        let mut rust_endpoint = Endpoint::new(
            Config {
                name: "rust".to_string(),
                fragment_above: FRAGMENT_ABOVE,
                ..Config::default()
            },
            100.0,
        );

        let mut c_context = CContext::new();
        let c_endpoint = unsafe {
            wire_compat_endpoint_create(
                FRAGMENT_ABOVE as c_int,
                1,
                (&mut *c_context as *mut CContext).cast(),
                c_transmit,
                c_process,
            )
        };
        assert!(!c_endpoint.is_null());

        let mut time = 100.0;

        let mut rust_num_processed = 0;
        let mut rust_num_invalid_payloads = 0;
        let mut rust_acks: Vec<u16> = Vec::new();
        let mut c_acks: Vec<u16> = Vec::new();

        for _ in 0..NUM_ITERATIONS {
            // rust -> c
            {
                let payload = generate_payload(rust_endpoint.next_packet_sequence());
                let mut datagrams: Vec<Vec<u8>> = Vec::new();
                rust_endpoint.send_packet(&payload, |_, data| datagrams.push(data.to_vec()));
                for mut datagram in datagrams {
                    unsafe {
                        reliable_endpoint_receive_packet(
                            c_endpoint,
                            datagram.as_mut_ptr(),
                            datagram.len() as c_int,
                        );
                    }
                }
            }

            // c -> rust
            {
                let sequence = unsafe { reliable_endpoint_next_packet_sequence(c_endpoint) };
                let mut payload = generate_payload(sequence);
                unsafe {
                    reliable_endpoint_send_packet(
                        c_endpoint,
                        payload.as_mut_ptr(),
                        payload.len() as c_int,
                    );
                }
                for datagram in c_context.transcript.drain(..) {
                    rust_endpoint.receive_packet(&datagram, |_, data| {
                        if !payload_is_valid(data) {
                            rust_num_invalid_payloads += 1;
                        }
                        rust_num_processed += 1;
                        true
                    });
                }
            }

            time += 0.01;
            rust_endpoint.update(time);
            unsafe { reliable_endpoint_update(c_endpoint, time) };

            rust_acks.extend(rust_endpoint.drain_acks());
            drain_c_acks(c_endpoint, &mut c_acks);
        }

        // every payload was delivered exactly once, intact, in both directions

        assert_eq!(c_context.num_processed, NUM_ITERATIONS);
        assert_eq!(c_context.num_invalid_payloads, 0);
        assert_eq!(rust_num_processed, NUM_ITERATIONS);
        assert_eq!(rust_num_invalid_payloads, 0);

        // within an iteration the rust endpoint sends first, so the c endpoint's reply
        // acks rust packets 0..=i, while the c endpoint's packet i is acked one
        // iteration later by rust packet i + 1

        let mut expected_rust_acks: Vec<u16> = (0..NUM_ITERATIONS as u16).collect();
        let mut expected_c_acks: Vec<u16> = (0..NUM_ITERATIONS as u16 - 1).collect();
        rust_acks.sort_unstable();
        c_acks.sort_unstable();
        expected_rust_acks.sort_unstable();
        expected_c_acks.sort_unstable();
        assert_eq!(rust_acks, expected_rust_acks);
        assert_eq!(c_acks, expected_c_acks);

        unsafe { reliable_endpoint_destroy(c_endpoint) };
    }

    /// A Rust endpoint pair and a C endpoint pair are driven through the same
    /// deterministic exchange. Every datagram on the wire must be byte-identical
    /// between the two implementations.
    #[test]
    fn identical_transcripts() {
        // rust pair, forwarding to each other inside the transmit closure and
        // recording everything they put on the wire

        let make_rust_endpoint = |name: &str| {
            Endpoint::new(
                Config {
                    name: name.to_string(),
                    fragment_above: FRAGMENT_ABOVE,
                    ..Config::default()
                },
                100.0,
            )
        };
        let mut rust_a = make_rust_endpoint("a");
        let mut rust_b = make_rust_endpoint("b");
        let mut rust_a_transcript: Vec<Vec<u8>> = Vec::new();
        let mut rust_b_transcript: Vec<Vec<u8>> = Vec::new();

        // c pair, forwarding to each other inside the transmit callback and recording

        let mut context_a = CContext::new();
        let mut context_b = CContext::new();
        let (c_a, c_b) = unsafe {
            let c_a = wire_compat_endpoint_create(
                FRAGMENT_ABOVE as c_int,
                0,
                (&mut *context_a as *mut CContext).cast(),
                c_transmit,
                c_process,
            );
            let c_b = wire_compat_endpoint_create(
                FRAGMENT_ABOVE as c_int,
                1,
                (&mut *context_b as *mut CContext).cast(),
                c_transmit,
                c_process,
            );
            context_a.forward_to_peer = c_b;
            context_b.forward_to_peer = c_a;
            (c_a, c_b)
        };
        assert!(!c_a.is_null() && !c_b.is_null());

        let mut time = 100.0;

        for _ in 0..NUM_ITERATIONS {
            // a -> b on both pairs

            let payload = generate_payload(rust_a.next_packet_sequence());
            rust_a.send_packet(&payload, |_, data| {
                rust_a_transcript.push(data.to_vec());
                rust_b.receive_packet(data, |_, _| true);
            });

            let mut payload =
                generate_payload(unsafe { reliable_endpoint_next_packet_sequence(c_a) });
            unsafe {
                reliable_endpoint_send_packet(c_a, payload.as_mut_ptr(), payload.len() as c_int);
            }

            // b -> a on both pairs

            let payload = generate_payload(rust_b.next_packet_sequence());
            rust_b.send_packet(&payload, |_, data| {
                rust_b_transcript.push(data.to_vec());
                rust_a.receive_packet(data, |_, _| true);
            });

            let mut payload =
                generate_payload(unsafe { reliable_endpoint_next_packet_sequence(c_b) });
            unsafe {
                reliable_endpoint_send_packet(c_b, payload.as_mut_ptr(), payload.len() as c_int);
            }

            time += 0.01;
            rust_a.update(time);
            rust_b.update(time);
            unsafe {
                reliable_endpoint_update(c_a, time);
                reliable_endpoint_update(c_b, time);
            }
            rust_a.clear_acks();
            rust_b.clear_acks();
            unsafe {
                reliable_endpoint_clear_acks(c_a);
                reliable_endpoint_clear_acks(c_b);
            }
        }

        // both implementations processed every packet and put identical bytes on the wire

        assert_eq!(context_a.num_processed, NUM_ITERATIONS);
        assert_eq!(context_b.num_processed, NUM_ITERATIONS);
        assert_eq!(context_a.num_invalid_payloads, 0);
        assert_eq!(context_b.num_invalid_payloads, 0);

        assert_eq!(rust_a_transcript, context_a.transcript);
        assert_eq!(rust_b_transcript, context_b.transcript);

        unsafe {
            reliable_endpoint_destroy(c_a);
            reliable_endpoint_destroy(c_b);
        }
    }
}
