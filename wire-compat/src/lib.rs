//! Wire-compatibility tests: the Rust port exchanging packets with the vendored C
//! reference implementation of reliable 1.4.5, in process, via FFI.
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
        fn reliable_endpoint_counters(endpoint: *mut CEndpoint) -> *const u64;
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
        delivered: Vec<(u16, Vec<u8>)>,
    }

    impl CContext {
        fn new() -> Self {
            Self {
                transcript: Vec::new(),
                forward_to_peer: std::ptr::null_mut(),
                num_processed: 0,
                num_invalid_payloads: 0,
                delivered: Vec::new(),
            }
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
        sequence: u16,
        packet_data: *mut u8,
        packet_bytes: c_int,
    ) -> c_int {
        unsafe {
            let context = &mut *(context as *mut CContext);
            let payload = std::slice::from_raw_parts(packet_data, packet_bytes as usize);
            if !payload_is_valid(payload) {
                context.num_invalid_payloads += 1;
            }
            context.delivered.push((sequence, payload.to_vec()));
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

        let c_context = Box::into_raw(Box::new(CContext::new()));
        let c_endpoint = unsafe {
            wire_compat_endpoint_create(
                FRAGMENT_ABOVE as c_int,
                1,
                c_context.cast(),
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
                for datagram in unsafe { (*c_context).transcript.drain(..) } {
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

        assert_eq!(unsafe { (*c_context).num_processed }, NUM_ITERATIONS);
        assert_eq!(unsafe { (*c_context).num_invalid_payloads }, 0);
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

        unsafe {
            reliable_endpoint_destroy(c_endpoint);
            drop(Box::from_raw(c_context));
        }
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

        let context_a = Box::into_raw(Box::new(CContext::new()));
        let context_b = Box::into_raw(Box::new(CContext::new()));
        let (c_a, c_b) = unsafe {
            let c_a = wire_compat_endpoint_create(
                FRAGMENT_ABOVE as c_int,
                0,
                context_a.cast(),
                c_transmit,
                c_process,
            );
            let c_b = wire_compat_endpoint_create(
                FRAGMENT_ABOVE as c_int,
                1,
                context_b.cast(),
                c_transmit,
                c_process,
            );
            (*context_a).forward_to_peer = c_b;
            (*context_b).forward_to_peer = c_a;
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

        assert_eq!(unsafe { (*context_a).num_processed }, NUM_ITERATIONS);
        assert_eq!(unsafe { (*context_b).num_processed }, NUM_ITERATIONS);
        assert_eq!(unsafe { (*context_a).num_invalid_payloads }, 0);
        assert_eq!(unsafe { (*context_b).num_invalid_payloads }, 0);

        assert_eq!(&rust_a_transcript, unsafe { &(*context_a).transcript });
        assert_eq!(&rust_b_transcript, unsafe { &(*context_b).transcript });

        unsafe {
            reliable_endpoint_destroy(c_a);
            reliable_endpoint_destroy(c_b);
            drop(Box::from_raw(context_a));
            drop(Box::from_raw(context_b));
        }
    }

    const DIFFERENTIAL_WARMUP_STEPS: usize = 65_000;
    const DIFFERENTIAL_STEPS: usize = 2_000;

    /// Small deterministic xorshift generator, so the mixed stream is reproducible
    /// without pulling in a dependency.
    struct DifferentialRng(u64);

    impl DifferentialRng {
        fn next_u32(&mut self) -> u32 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
        }

        fn below(&mut self, bound: u32) -> u32 {
            self.next_u32() % bound
        }
    }

    /// A payload whose size selects a regular packet or a 1..4 fragment packet. The
    /// contents are seeded and irrelevant to wire compatibility; only the length decides
    /// how the endpoint splits it.
    fn differential_payload(rng: &mut DifferentialRng) -> Vec<u8> {
        const SIZES: [usize; 10] = [2, 3, 31, 64, 200, 499, 500, 501, 1_200, 4_000];
        let size = SIZES[rng.below(SIZES.len() as u32) as usize];
        let mut payload = vec![0u8; size];
        for byte in &mut payload {
            *byte = rng.next_u32() as u8;
        }
        payload
    }

    fn rust_counter_array(endpoint: &Endpoint) -> [u64; 11] {
        let counters = endpoint.counters();
        [
            counters.num_packets_sent,
            counters.num_packets_received,
            counters.num_packets_acked,
            counters.num_packets_stale,
            counters.num_packets_invalid,
            counters.num_packets_too_large_to_send,
            counters.num_packets_too_large_to_receive,
            counters.num_fragments_sent,
            counters.num_fragments_received,
            counters.num_fragments_invalid,
            counters.num_packets_duplicate,
        ]
    }

    unsafe fn c_counter_array(endpoint: *mut CEndpoint) -> [u64; 11] {
        unsafe {
            let counters = reliable_endpoint_counters(endpoint);
            let mut out = [0u64; 11];
            for (index, slot) in out.iter_mut().enumerate() {
                *slot = *counters.add(index);
            }
            out
        }
    }

    /// Two endpoints, one C and one Rust, are advanced together past the `65535 -> 0`
    /// sequence wrap, then fed the same seeded stream of regular packets, fragmented
    /// packets, duplicates, reordering, drops, truncation and corrupted headers. At every
    /// step the bytes each endpoint puts on the wire, the payloads it delivers, the acks
    /// it reports and every counter must agree.
    #[test]
    fn differential_stream_across_wrap() {
        let mut rust_endpoint = Endpoint::new(
            Config {
                name: "rust".to_string(),
                fragment_above: FRAGMENT_ABOVE,
                ..Config::default()
            },
            100.0,
        );

        let c_context = Box::into_raw(Box::new(CContext::new()));
        let c_endpoint = unsafe {
            wire_compat_endpoint_create(
                FRAGMENT_ABOVE as c_int,
                1,
                c_context.cast(),
                c_transmit,
                c_process,
            )
        };
        assert!(!c_endpoint.is_null());

        let mut rust_delivered: Vec<(u16, Vec<u8>)> = Vec::new();
        let mut rust_out: Vec<Vec<u8>> = Vec::new();
        let mut time = 100.0;

        // Advance both endpoints to sequence 65000 in lockstep. Warm-up packets are
        // small regular packets, so this only moves the send and receive windows and
        // leaves the interesting traffic to the seeded phase below.

        for step in 0..DIFFERENTIAL_WARMUP_STEPS {
            assert_eq!(rust_endpoint.next_packet_sequence(), unsafe {
                reliable_endpoint_next_packet_sequence(c_endpoint)
            });

            let payload = vec![(step & 0xFF) as u8; 16];

            rust_out.clear();
            rust_endpoint.send_packet(&payload, |_, data| rust_out.push(data.to_vec()));
            let mut c_payload = payload.clone();
            unsafe {
                reliable_endpoint_send_packet(
                    c_endpoint,
                    c_payload.as_mut_ptr(),
                    c_payload.len() as c_int,
                );
            }
            let c_out: Vec<Vec<u8>> = unsafe { (*c_context).transcript.drain(..).collect() };
            assert_eq!(rust_out, c_out, "outgoing datagrams diverged in warm-up");

            for datagram in &rust_out {
                let mut copy = datagram.clone();
                unsafe {
                    reliable_endpoint_receive_packet(
                        c_endpoint,
                        copy.as_mut_ptr(),
                        copy.len() as c_int,
                    );
                }
                rust_endpoint.receive_packet(datagram, |sequence, data| {
                    rust_delivered.push((sequence, data.to_vec()));
                    true
                });
            }

            time += 0.01;
            rust_endpoint.update(time);
            unsafe { reliable_endpoint_update(c_endpoint, time) };

            let _ = rust_endpoint.drain_acks().count();
            let mut c_acks: Vec<u16> = Vec::new();
            drain_c_acks(c_endpoint, &mut c_acks);
        }

        assert_eq!(
            rust_endpoint.next_packet_sequence(),
            DIFFERENTIAL_WARMUP_STEPS as u16
        );
        assert_eq!(
            unsafe { reliable_endpoint_next_packet_sequence(c_endpoint) },
            DIFFERENTIAL_WARMUP_STEPS as u16
        );
        assert_eq!(
            rust_counter_array(&rust_endpoint),
            unsafe { c_counter_array(c_endpoint) },
            "counters diverged during warm-up"
        );

        rust_delivered.clear();
        unsafe { (*c_context).delivered.clear() };

        // Seeded mixed stream. Most steps deliver normally, but duplicates, one-step
        // reordering, drops, truncation, header corruption and random garbage are mixed
        // in, all delivered identically to both endpoints.

        let mut rng = DifferentialRng(0x9E37_79B9_7F4A_7C15);
        let mut held: Vec<Vec<u8>> = Vec::new();
        let mut delivered_count = 0usize;
        let mut ack_count = 0usize;
        let mut max_delivered_bytes = 0usize;
        let mut reordered_steps = 0usize;

        for step in 0..DIFFERENTIAL_STEPS {
            assert_eq!(rust_endpoint.next_packet_sequence(), unsafe {
                reliable_endpoint_next_packet_sequence(c_endpoint)
            });

            let payload = differential_payload(&mut rng);

            rust_out.clear();
            rust_endpoint.send_packet(&payload, |_, data| rust_out.push(data.to_vec()));
            let mut c_payload = payload.clone();
            unsafe {
                reliable_endpoint_send_packet(
                    c_endpoint,
                    c_payload.as_mut_ptr(),
                    c_payload.len() as c_int,
                );
            }
            let c_out: Vec<Vec<u8>> = unsafe { (*c_context).transcript.drain(..).collect() };
            assert_eq!(
                rust_out, c_out,
                "outgoing datagrams diverged at step {step}"
            );

            let mut to_deliver: Vec<Vec<u8>> = Vec::new();
            match rng.below(100) {
                0..=59 => {
                    to_deliver.append(&mut held);
                    to_deliver.extend_from_slice(&rust_out);
                }
                60..=69 => {
                    // Out-of-order delivery: the current (newer) packet is
                    // delivered before any packet held from an earlier step,
                    // so a valid newer packet arrives before an older one.
                    if !held.is_empty() && !rust_out.is_empty() {
                        reordered_steps += 1;
                    }
                    to_deliver.extend_from_slice(&rust_out);
                    to_deliver.append(&mut held);
                }
                70..=78 => {
                    to_deliver.append(&mut held);
                    for datagram in &rust_out {
                        to_deliver.push(datagram.clone());
                        to_deliver.push(datagram.clone());
                    }
                }
                79..=85 => {
                    to_deliver.append(&mut held);
                    for datagram in &rust_out {
                        let mut corrupted = datagram.clone();
                        if !corrupted.is_empty() {
                            let index = rng.below(corrupted.len() as u32) as usize;
                            corrupted[index] ^= (1 << rng.below(8)) | 1;
                        }
                        to_deliver.push(corrupted);
                    }
                }
                86..=91 => {
                    to_deliver.append(&mut held);
                    for datagram in &rust_out {
                        if datagram.is_empty() {
                            continue;
                        }
                        let keep = 1 + rng.below(datagram.len() as u32) as usize;
                        to_deliver.push(datagram[..keep].to_vec());
                    }
                }
                92..=95 => {
                    held.extend_from_slice(&rust_out);
                }
                96..=98 => {
                    to_deliver.append(&mut held);
                    let bytes = 1 + rng.below(48) as usize;
                    let mut garbage = Vec::with_capacity(bytes);
                    for _ in 0..bytes {
                        garbage.push(rng.next_u32() as u8);
                    }
                    to_deliver.push(garbage);
                }
                _ => {
                    to_deliver.append(&mut held);
                }
            }

            let rust_start = rust_delivered.len();
            let c_start = unsafe { (*c_context).delivered.len() };

            for datagram in &to_deliver {
                if datagram.is_empty() {
                    continue;
                }
                let mut copy = datagram.clone();
                unsafe {
                    reliable_endpoint_receive_packet(
                        c_endpoint,
                        copy.as_mut_ptr(),
                        copy.len() as c_int,
                    );
                }
                rust_endpoint.receive_packet(datagram, |sequence, data| {
                    rust_delivered.push((sequence, data.to_vec()));
                    true
                });
            }

            assert_eq!(
                &rust_delivered[rust_start..],
                unsafe { &(*c_context).delivered[c_start..] },
                "delivered payloads diverged at step {step}"
            );

            for (_, payload) in &rust_delivered[rust_start..] {
                delivered_count += 1;
                max_delivered_bytes = max_delivered_bytes.max(payload.len());
            }

            time += 0.01;
            rust_endpoint.update(time);
            unsafe { reliable_endpoint_update(c_endpoint, time) };

            let mut rust_acks: Vec<u16> = rust_endpoint.drain_acks().collect();
            let mut c_acks: Vec<u16> = Vec::new();
            drain_c_acks(c_endpoint, &mut c_acks);
            ack_count += rust_acks.len();
            rust_acks.sort_unstable();
            c_acks.sort_unstable();
            assert_eq!(rust_acks, c_acks, "ack lists diverged at step {step}");

            assert_eq!(
                rust_counter_array(&rust_endpoint),
                unsafe { c_counter_array(c_endpoint) },
                "counters diverged at step {step}"
            );
        }

        // the send sequence wrapped while the mixed stream ran
        assert_eq!(
            rust_endpoint.next_packet_sequence(),
            (DIFFERENTIAL_WARMUP_STEPS + DIFFERENTIAL_STEPS) as u16
        );

        // the stream was not vacuous: payloads of every size were delivered, acks
        // flowed, and the hostile datagrams hit the rejection paths on both sides
        assert!(delivered_count > 100, "too few payloads delivered");
        assert!(
            reordered_steps > 0,
            "no valid packet was delivered out of order"
        );
        assert!(ack_count > 0, "no acks were exchanged");
        assert!(
            max_delivered_bytes > FRAGMENT_ABOVE,
            "no fragmented packet was reassembled"
        );
        let counters = rust_endpoint.counters();
        assert!(counters.num_fragments_received > 0);
        assert!(
            counters.num_packets_invalid
                + counters.num_packets_stale
                + counters.num_packets_duplicate
                + counters.num_fragments_invalid
                + counters.num_packets_too_large_to_receive
                > 0,
            "hostile datagrams did not reach any rejection path"
        );

        unsafe {
            reliable_endpoint_destroy(c_endpoint);
            drop(Box::from_raw(c_context));
        }
    }
}
