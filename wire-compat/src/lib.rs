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
    use std::ffi::{CString, c_char, c_int, c_longlong, c_void};

    use reliable::{Config, Counters, Endpoint};

    // differential oracle exports from the C shim. see wire-compat/c/shim.c.

    unsafe extern "C" {
        fn wire_compat_diff_create(fragment_above: c_int) -> *mut c_void;
        fn wire_compat_diff_destroy(handle: *mut c_void);
        fn wire_compat_diff_feed(
            handle: *mut c_void,
            data: *const u8,
            len: c_int,
            n: c_int,
            out: *mut u8,
            cap: c_int,
        ) -> c_int;
    }

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

    // the eleven counters in reliable.h order, with the names the C shim prints.

    const COUNTER_NAMES: [&str; 11] = [
        "num_packets_sent",
        "num_packets_received",
        "num_packets_acked",
        "num_packets_stale",
        "num_packets_invalid",
        "num_packets_too_large_to_send",
        "num_packets_too_large_to_receive",
        "num_fragments_sent",
        "num_fragments_received",
        "num_fragments_invalid",
        "num_packets_duplicate",
    ];

    fn counter_array(counters: &Counters) -> [u64; 11] {
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

    // one line per input, byte-identical to the C shim's wire_compat_diff_feed format:
    //   <n> <accepted|refused> <changed counter name or none> <seq:len | -> <eleven counters>

    fn rust_diff_line(
        n: usize,
        endpoint: &mut Endpoint,
        previous: &mut [u64; 11],
        input: &[u8],
    ) -> String {
        let mut delivered: Option<(u16, usize)> = None;
        endpoint.receive_packet(input, |sequence, payload| {
            delivered = Some((sequence, payload.len()));
            true
        });

        let counters = counter_array(endpoint.counters());

        let mut changed = "none";
        for (i, slot) in previous.iter_mut().enumerate() {
            if counters[i] != *slot {
                changed = COUNTER_NAMES[i];
                break;
            }
        }
        *previous = counters;

        let mut line = format!(
            "{} {} {} {} ",
            n,
            if delivered.is_some() { "accepted" } else { "refused" },
            changed,
            if delivered.is_some() { "" } else { "-" },
        );
        if let Some((sequence, bytes)) = delivered {
            line.push_str(&format!("{sequence}:{bytes} "));
        }
        for (i, counter) in counters.iter().enumerate() {
            if i + 1 < counters.len() {
                line.push_str(&format!("{counter} "));
            } else {
                line.push_str(&format!("{counter}"));
            }
        }
        line
    }

    fn c_diff_line(handle: *mut c_void, n: usize, input: &[u8]) -> String {
        let mut out = vec![0u8; 4096];
        let written = unsafe {
            wire_compat_diff_feed(
                handle,
                input.as_ptr(),
                input.len() as c_int,
                n as c_int,
                out.as_mut_ptr(),
                out.len() as c_int,
            )
        };
        assert!(written >= 0, "wire_compat_diff_feed output buffer too small");
        String::from_utf8(out[..written as usize].to_vec()).expect("utf8 transcript line")
    }

    /// Differential test against the C reference: the SAME raw datagrams are fed to a C
    /// endpoint and to the Rust port, in one process, and the two transcripts must compare
    /// byte for byte. Covers valid regular traffic, fragments, and one hostile input per
    /// checklist item 5 shape that both sides can observe without aborting (oversize,
    /// truncated header, mismatched fragment count).
    ///
    /// The fourth checklist-item-5 shape, a zero-length receive, is deliberately absent
    /// from the transcript: both implementations abort on it rather than counting it — the
    /// Rust port's `assert!(!packet_data.is_empty())` panics and the C reference's
    /// `reliable_assert(packet_bytes > 0)` traps under `RELIABLE_DEBUG` (build.rs), so
    /// neither side can yield a comparable "refused" line. That the two abort in the same
    /// place is itself the finding (notes/c-parity-2026-10-14.md item 5).
    ///
    /// Gated on `RELIABLE_C_DIR`, which is used only to refresh the vendored copy. When it
    /// is unset the test skips with a loud message, so the unit gate stays hermetic. Run:
    ///
    ///     cargo test --manifest-path wire-compat/Cargo.toml differential_against_c_reference
    #[test]
    fn differential_against_c_reference() {
        if std::env::var_os("RELIABLE_C_DIR").is_none() {
            eprintln!(
                "SKIP differential_against_c_reference: RELIABLE_C_DIR is unset; \
                 no C checkout holding the audit fixes is reachable, so this leg is \
                 not-done. Set RELIABLE_C_DIR to a mas-bandwidth/reliable checkout at \
                 sprint/reliable-audit-fixes (>= 7862477e), refresh wire-compat/c, then \
                 run `cargo test --manifest-path wire-compat/Cargo.toml \
                 differential_against_c_reference`."
            );
            return;
        }

        const FRAGMENT_ABOVE: usize = 500;

        // build the input vectors with a Rust endpoint pair, then feed the identical bytes
        // to both implementations

        let mut producer = Endpoint::new(
            Config {
                name: "producer".to_string(),
                fragment_above: FRAGMENT_ABOVE,
                ..Config::default()
            },
            100.0,
        );

        let mut inputs: Vec<Vec<u8>> = Vec::new();

        // a valid regular packet
        {
            let mut datagrams: Vec<Vec<u8>> = Vec::new();
            producer.send_packet(&[1, 2, 3], |_, data| datagrams.push(data.to_vec()));
            inputs.extend(datagrams);
        }

        // a valid fragmented packet (spans the threshold, several fragments)
        {
            let payload = generate_payload(producer.next_packet_sequence());
            let mut datagrams: Vec<Vec<u8>> = Vec::new();
            producer.send_packet(&payload, |_, data| datagrams.push(data.to_vec()));
            assert!(datagrams.len() > 1, "payload must fragment");
            inputs.extend(datagrams);
        }

        // hostile: oversize (past max_packet_size + packet and fragment headers)
        {
            let max_packet_size = Config::default().max_packet_size;
            inputs.push(vec![0u8; max_packet_size + 9 + 5 + 1]);
        }

        // hostile: truncated packet header
        inputs.push(vec![0x00, 0x01]);

        // hostile: mismatched fragment count (num_fragments above max_fragments)
        inputs.push(vec![1, 0x00, 0x00, 0x00, 0xFF, 0x00]);

        let diffs = unsafe { wire_compat_diff_create(FRAGMENT_ABOVE as c_int) };
        assert!(!diffs.is_null(), "C differential endpoint create failed");

        let mut c_lines: Vec<String> = Vec::new();
        let mut rust_lines: Vec<String> = Vec::new();

        let mut endpoint = Endpoint::new(
            Config {
                name: "rust".to_string(),
                fragment_above: FRAGMENT_ABOVE,
                ..Config::default()
            },
            100.0,
        );
        let mut previous = [0u64; 11];

        for (n, input) in inputs.iter().enumerate() {
            c_lines.push(c_diff_line(diffs, n, input));
            rust_lines.push(rust_diff_line(n, &mut endpoint, &mut previous, input));
        }

        unsafe { wire_compat_diff_destroy(diffs) };

        for (n, (c_line, rust_line)) in c_lines.iter().zip(rust_lines.iter()).enumerate() {
            if c_line != rust_line {
                panic!(
                    "transcript differs at input {n}:\n  C   : {c_line}\n  rust: {rust_line}"
                );
            }
        }
    }

    // fragment oracle declarations. see wire-compat/c/shim.c.

    unsafe extern "C" {
        fn wire_compat_frag_oracle(
            case_name: *const c_char,
            fragment_above: c_int,
            fragment_sequence: c_int,
            advance: *const u8,
            advance_len: c_int,
            fragment1: *const u8,
            fragment1_len: c_int,
            fragment2: *const u8,
            fragment2_len: c_int,
            genuine: *const u8,
            genuine_len: c_int,
            out: *mut u8,
            cap: c_int,
        ) -> c_int;
    }

    /// A regular packet at sequence 0, built by a Rust producer and fed byte-identically to
    /// both implementations.
    fn build_genuine_seq0() -> Vec<u8> {
        let mut producer = Endpoint::new(
            Config {
                name: "producer".to_string(),
                fragment_above: FRAGMENT_ABOVE,
                ..Config::default()
            },
            100.0,
        );
        let mut datagrams: Vec<Vec<u8>> = Vec::new();
        producer.send_packet(&[0u8; 8], |_, data| datagrams.push(data.to_vec()));
        assert_eq!(datagrams.len(), 1);
        datagrams.remove(0)
    }

    /// A regular packet at sequence 1000, fed first to advance a receiver's window to
    /// 1001 before the crafted fragment arrives.
    fn build_advance_seq1000() -> Vec<u8> {
        let mut producer = Endpoint::new(
            Config {
                name: "advance".to_string(),
                fragment_above: FRAGMENT_ABOVE,
                ..Config::default()
            },
            100.0,
        );
        let mut packet: Vec<u8> = Vec::new();
        for _ in 0..=1000 {
            producer.send_packet(&[0u8; 8], |sequence, data| {
                if sequence == 1000 {
                    packet = data.to_vec();
                }
            });
        }
        assert!(!packet.is_empty());
        packet
    }

    /// Case C: an in-order fragmented packet of 1.5 x fragment_size at sequence 0, which
    /// reassembles from two fragments and delivers at full size.
    fn build_case_c_fragments() -> Vec<Vec<u8>> {
        let mut producer = Endpoint::new(
            Config {
                name: "c-producer".to_string(),
                fragment_above: FRAGMENT_ABOVE,
                ..Config::default()
            },
            100.0,
        );
        let fragment_size = producer.config().fragment_size;
        let payload = vec![0u8; fragment_size + fragment_size / 2];
        let mut datagrams: Vec<Vec<u8>> = Vec::new();
        producer.send_packet(&payload, |_, data| datagrams.push(data.to_vec()));
        assert!(datagrams.len() >= 2, "case C payload must fragment");
        datagrams
    }

    /// A lone last fragment (id 1 of 2) at `sequence` with zero or one payload byte:
    /// `[1, sequence_lo, sequence_hi, fragment_id, count - 1, payload..]`.
    fn lone_last_fragment(sequence: u16, payload_bytes: usize) -> Vec<u8> {
        let mut fragment = vec![
            1u8,
            (sequence & 0xFF) as u8,
            ((sequence >> 8) & 0xFF) as u8,
            1,
            1,
        ];
        fragment.extend(std::iter::repeat_n(0u8, payload_bytes));
        fragment
    }

    // one line per crafted input, byte-identical to the C shim's wire_compat_frag_oracle:
    //   <case> <fragment seq> <refused|accepted> <genuine seq 0 delivered 0|1> <eleven counters>

    fn rust_frag_line(
        case_name: &str,
        fragment_sequence: u16,
        advance: Option<&[u8]>,
        fragments: &[Vec<u8>],
        genuine: &[u8],
    ) -> String {
        let mut endpoint = Endpoint::new(
            Config {
                name: "rust".to_string(),
                fragment_above: FRAGMENT_ABOVE,
                ..Config::default()
            },
            100.0,
        );

        if let Some(advance) = advance {
            endpoint.receive_packet(advance, |_, _| true);
        }

        let invalid_before = endpoint.counters().num_fragments_invalid;
        let mut num_processed = 0usize;

        for fragment in fragments {
            endpoint.receive_packet(fragment, |_, _| {
                num_processed += 1;
                true
            });
        }

        let refused = endpoint.counters().num_fragments_invalid != invalid_before;

        endpoint.receive_packet(genuine, |_, _| {
            num_processed += 1;
            true
        });

        let delivered = num_processed > 0;
        let counters = counter_array(endpoint.counters());

        let mut line = format!(
            "{} {} {} {} ",
            case_name,
            fragment_sequence,
            if refused { "refused" } else { "accepted" },
            if delivered { 1 } else { 0 },
        );
        for (i, counter) in counters.iter().enumerate() {
            if i + 1 < counters.len() {
                line.push_str(&format!("{counter} "));
            } else {
                line.push_str(&format!("{counter}"));
            }
        }
        line
    }

    fn c_frag_line(
        case_name: &str,
        fragment_sequence: u16,
        advance: Option<&[u8]>,
        fragments: &[Vec<u8>],
        genuine: &[u8],
    ) -> String {
        let case = CString::new(case_name).expect("case name");
        let (advance_ptr, advance_len) = match advance {
            Some(advance) => (advance.as_ptr(), advance.len() as c_int),
            None => (std::ptr::null(), 0),
        };
        let fragment1 = &fragments[0];
        let fragment2 = fragments.get(1);

        let mut out = vec![0u8; 4096];
        let written = unsafe {
            wire_compat_frag_oracle(
                case.as_ptr(),
                FRAGMENT_ABOVE as c_int,
                fragment_sequence as c_int,
                advance_ptr,
                advance_len,
                fragment1.as_ptr(),
                fragment1.len() as c_int,
                fragment2.map_or(std::ptr::null(), |fragment| fragment.as_ptr()),
                fragment2.map_or(0, |fragment| fragment.len() as c_int),
                genuine.as_ptr(),
                genuine.len() as c_int,
                out.as_mut_ptr(),
                out.len() as c_int,
            )
        };
        assert!(written >= 0, "wire_compat_frag_oracle output buffer too small");
        String::from_utf8(out[..written as usize].to_vec()).expect("utf8 oracle line")
    }

    /// One crafted fragment input: case name, fragment sequence, optional advance packet,
    /// fragment datagrams, then the genuine sequence 0 packet.
    type FragInput = (String, u16, Option<Vec<u8>>, Vec<Vec<u8>>, Vec<u8>);

    /// Differential test against the C reference for the fragment / received-window fix
    /// (C reliable PR 68, merge dfd5541c): the crafted fragment sequences of C's cases A,
    /// B and C, each followed by a genuine sequence 0, plus every fragment sequence in the
    /// audit list fed to a cold receiver and to a receiver advanced to 1000, each as a lone
    /// last fragment with zero and one payload byte. C and the port print one line per
    /// input and the two transcripts must compare byte for byte.
    ///
    /// Gated on `RELIABLE_C_DIR` (used only to refresh the vendored C): when it is unset
    /// the test skips with a loud message so the unit gate stays hermetic. Run:
    ///
    ///     cargo test --manifest-path wire-compat/Cargo.toml differential_fragments_against_c_reference
    #[test]
    fn differential_fragments_against_c_reference() {
        if std::env::var_os("RELIABLE_C_DIR").is_none() {
            eprintln!(
                "SKIP differential_fragments_against_c_reference: RELIABLE_C_DIR is unset; \
                 no C checkout holding the fragment / received-window fix (PR 68, merge \
                 dfd5541c) is reachable, so this leg is not-done. Refresh wire-compat/c from \
                 such a checkout, then run `cargo test --manifest-path wire-compat/Cargo.toml \
                 differential_fragments_against_c_reference`."
            );
            return;
        }

        let genuine = build_genuine_seq0();
        let advance = build_advance_seq1000();
        let case_c = build_case_c_fragments();

        let mut inputs: Vec<FragInput> = vec![
            ("A".to_string(), 512, None, vec![lone_last_fragment(512, 0)], genuine.clone()),
            ("A-one".to_string(), 512, None, vec![lone_last_fragment(512, 1)], genuine.clone()),
            ("B".to_string(), 32512, None, vec![lone_last_fragment(32512, 0)], genuine.clone()),
            ("C".to_string(), 0, None, case_c.clone(), genuine.clone()),
        ];

        for sequence in [
            0u16, 1, 255, 256, 511, 512, 513, 32511, 32512, 32513, 32768, 65280, 65535,
        ] {
            for (state, advance) in [("cold", None), ("adv", Some(advance.clone()))] {
                for payload_bytes in [0usize, 1] {
                    inputs.push((
                        format!("{sequence}-{state}-{payload_bytes}"),
                        sequence,
                        advance.clone(),
                        vec![lone_last_fragment(sequence, payload_bytes)],
                        genuine.clone(),
                    ));
                }
            }
        }

        let mut c_lines: Vec<String> = Vec::new();
        let mut rust_lines: Vec<String> = Vec::new();

        for (case_name, sequence, advance, fragments, genuine) in &inputs {
            c_lines.push(c_frag_line(case_name, *sequence, advance.as_deref(), fragments, genuine));
            rust_lines.push(rust_frag_line(case_name, *sequence, advance.as_deref(), fragments, genuine));
        }

        for ((case_name, _, _, _, _), (c_line, rust_line)) in inputs.iter().zip(c_lines.iter().zip(rust_lines.iter())) {
            if c_line != rust_line {
                panic!(
                    "transcript differs at case {case_name}:\n  C   : {c_line}\n  rust: {rust_line}"
                );
            }
        }
    }

    /// Differential test against the C reference for the empty-packet fix. An empty
    /// datagram is counted invalid and ignored by the C reference (C reliable PR 69,
    /// merge 190bce5b) rather than aborting the receive; the port must match byte for
    /// byte.
    ///
    /// The inputs are a zero-length packet, one-byte packets {0} and {1}, a packet of
    /// exactly max_packet_size + 14 bytes of zeros, one byte more than that, and a valid
    /// packet built by a sender — each fed in turn to the same receiver.
    ///
    /// Gated on `RELIABLE_C_DIR` (used only to refresh the vendored C): when it is unset
    /// the test skips with a loud message so the unit gate stays hermetic. Run:
    ///
    ///     cargo test --manifest-path wire-compat/Cargo.toml differential_empty_packet_against_c_reference
    #[test]
    fn differential_empty_packet_against_c_reference() {
        if std::env::var_os("RELIABLE_C_DIR").is_none() {
            eprintln!(
                "SKIP differential_empty_packet_against_c_reference: RELIABLE_C_DIR is unset; \
                 no C checkout holding the empty-packet fix (PR 69, merge 190bce5b) is \
                 reachable, so this leg is not-done. Refresh wire-compat/c from such a \
                 checkout, then run `cargo test --manifest-path wire-compat/Cargo.toml \
                 differential_empty_packet_against_c_reference`."
            );
            return;
        }

        const FRAGMENT_ABOVE: usize = 500;

        let max_packet_size = Config::default().max_packet_size;

        let mut inputs: Vec<Vec<u8>> = Vec::new();
        inputs.push(Vec::new());                      // zero length
        inputs.push(vec![0u8]);                       // {0}
        inputs.push(vec![1u8]);                       // {1}
        inputs.push(vec![0u8; max_packet_size + 14]); // exactly the receive limit
        inputs.push(vec![0u8; max_packet_size + 15]); // one byte past it

        let mut producer = Endpoint::new(
            Config {
                name: "producer".to_string(),
                fragment_above: FRAGMENT_ABOVE,
                ..Config::default()
            },
            100.0,
        );
        producer.send_packet(&[1, 2, 3], |_, data| inputs.push(data.to_vec()));

        let diffs = unsafe { wire_compat_diff_create(FRAGMENT_ABOVE as c_int) };
        assert!(!diffs.is_null(), "C differential endpoint create failed");

        let mut c_lines: Vec<String> = Vec::new();
        let mut rust_lines: Vec<String> = Vec::new();

        let mut endpoint = Endpoint::new(
            Config {
                name: "rust".to_string(),
                fragment_above: FRAGMENT_ABOVE,
                ..Config::default()
            },
            100.0,
        );
        let mut previous = [0u64; 11];

        for (n, input) in inputs.iter().enumerate() {
            c_lines.push(c_diff_line(diffs, n, input));
            rust_lines.push(rust_diff_line(n, &mut endpoint, &mut previous, input));
        }

        unsafe { wire_compat_diff_destroy(diffs) };

        for (n, (c_line, rust_line)) in c_lines.iter().zip(rust_lines.iter()).enumerate() {
            if c_line != rust_line {
                panic!(
                    "transcript differs at input {n}:\n  C   : {c_line}\n  rust: {rust_line}"
                );
            }
        }
    }

    // config oracle declarations. see wire-compat/c/shim.c.

    unsafe extern "C" {
        fn wire_compat_config_buffer_oracle(
            field: *const c_char,
            value: c_longlong,
            out: *mut u8,
            cap: c_int,
        ) -> c_int;
        fn wire_compat_config_length_oracle(
            packet_header_size: c_int,
            max_packet_size: c_longlong,
            out: *mut u8,
            cap: c_int,
        ) -> c_int;
    }

    // one line per input, byte-identical to the C shim's config oracle:
    //   <field> <value> <created|refused> [ack <sent bytes acked by a peer>]

    fn config_buffer_config(field: &str, value: usize) -> Config {
        match field {
            "sent_packets_buffer_size" => Config {
                sent_packets_buffer_size: value,
                ..Config::default()
            },
            "received_packets_buffer_size" => Config {
                received_packets_buffer_size: value,
                ..Config::default()
            },
            "fragment_reassembly_buffer_size" => Config {
                fragment_reassembly_buffer_size: value,
                ..Config::default()
            },
            _ => unreachable!("unknown sequence buffer field"),
        }
    }

    fn measure_config_ack(mut sender: Endpoint) -> u64 {
        let mut receiver = Endpoint::new(Config::default(), 100.0);
        sender.send_packet(&[0u8], |_, data| {
            receiver.receive_packet(data, |_, _| true);
        });
        receiver.send_packet(&[0u8], |_, data| {
            sender.receive_packet(data, |_, _| true);
        });
        sender.update(100.01);
        receiver.update(100.01);
        if sender.acks().contains(&0) { 1 } else { 0 }
    }

    fn rust_config_buffer_line(field: &str, value: usize) -> String {
        let config = config_buffer_config(field, value);
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Endpoint::new(config, 100.0)
        })) {
            Err(_) => format!("{field} {value} refused"),
            Ok(sender) => {
                let bytes_acked = measure_config_ack(sender);
                format!("{field} {value} created ack {bytes_acked}")
            }
        }
    }

    fn rust_config_length_line(packet_header_size: usize, max_packet_size: usize) -> String {
        let config = Config {
            packet_header_size,
            max_packet_size,
            fragment_above: 1024,
            max_fragments: 1,
            fragment_size: max_packet_size,
            ..Config::default()
        };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Endpoint::new(config, 100.0)
        })) {
            Err(_) => format!("packet_header_size+max_packet_size {max_packet_size} refused"),
            Ok(sender) => {
                let bytes_acked = measure_config_ack(sender);
                format!("packet_header_size+max_packet_size {max_packet_size} created ack {bytes_acked}")
            }
        }
    }

    /// Differential test against the C reference for the sequence-buffer size and
    /// received-bytes fixes (C 7862477e): each of the three sequence-buffer fields at
    /// 0, 1, 2, 3, 100, 255, 256, 257, 16383, 16384, 16385, 32768, 65535, 65536 and 65537
    /// with every other field at its default, and the packet_header_size + max_packet_size
    /// edge configs at INT_MAX - 14 - 28, one above it, and header 9 at INT_MAX - 23. Each
    /// side prints one line per config and the two transcripts must compare byte for byte.
    ///
    /// Gated on `RELIABLE_C_DIR` (used only to refresh the vendored C): when it is unset the
    /// test skips with a loud message so the unit gate stays hermetic. Run:
    ///
    ///     cargo test --manifest-path wire-compat/Cargo.toml differential_config_against_c_reference
    #[test]
    fn differential_config_against_c_reference() {
        if std::env::var_os("RELIABLE_C_DIR").is_none() {
            eprintln!(
                "SKIP differential_config_against_c_reference: RELIABLE_C_DIR is unset; no C \
                 checkout holding the sequence-buffer size and received-bytes fixes (C 7862477e) \
                 is reachable, so this leg is not-done. Refresh wire-compat/c from such a \
                 checkout, then run `cargo test --manifest-path wire-compat/Cargo.toml \
                 differential_config_against_c_reference`."
            );
            return;
        }

        const FIELDS: [&str; 3] = [
            "sent_packets_buffer_size",
            "received_packets_buffer_size",
            "fragment_reassembly_buffer_size",
        ];
        const SIZES: [usize; 15] = [
            0, 1, 2, 3, 100, 255, 256, 257, 16383, 16384, 16385, 32768, 65535, 65536, 65537,
        ];

        let int_max = c_longlong::from(i32::MAX);
        let length_inputs: [(c_int, c_longlong); 3] = [
            (28, int_max - 14 - 28),
            (28, int_max - 14 - 28 + 1),
            (9, int_max - 23),
        ];

        let mut c_lines: Vec<String> = Vec::new();
        let mut rust_lines: Vec<String> = Vec::new();
        let mut out = vec![0u8; 4096];

        for field in FIELDS {
            for value in SIZES {
                let c_field = CString::new(field).expect("field name");
                let written = unsafe {
                    wire_compat_config_buffer_oracle(
                        c_field.as_ptr(),
                        value as c_longlong,
                        out.as_mut_ptr(),
                        out.len() as c_int,
                    )
                };
                assert!(written >= 0, "config oracle output buffer too small");
                c_lines.push(
                    String::from_utf8(out[..written as usize].to_vec()).expect("utf8 config line"),
                );
                rust_lines.push(rust_config_buffer_line(field, value));
            }
        }

        for (packet_header_size, max_packet_size) in length_inputs {
            let written = unsafe {
                wire_compat_config_length_oracle(
                    packet_header_size,
                    max_packet_size,
                    out.as_mut_ptr(),
                    out.len() as c_int,
                )
            };
            assert!(written >= 0, "config oracle output buffer too small");
            c_lines.push(
                String::from_utf8(out[..written as usize].to_vec()).expect("utf8 config line"),
            );
            rust_lines.push(rust_config_length_line(
                packet_header_size as usize,
                max_packet_size as usize,
            ));
        }

        for (n, (c_line, rust_line)) in c_lines.iter().zip(rust_lines.iter()).enumerate() {
            if c_line != rust_line {
                panic!("transcript differs at config {n}:\n  C   : {c_line}\n  rust: {rust_line}");
            }
        }
    }
}
