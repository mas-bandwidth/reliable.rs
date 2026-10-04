//! Port of test_endpoint_create_invalid_config from the C original: a valid config
//! creates an endpoint and every invalid one is refused. The C endpoint returns NULL on
//! an invalid config; the Rust [`Endpoint::new`] panics on one (its asserts), so each
//! refused case is a `#[should_panic]` test pinned to the create-time check it names.
//! Cases the `endpoint_rejects_*` unit tests in src/endpoint.rs already cover are named
//! at their place in the C order rather than duplicated.

mod common;

use reliable::{Config, Endpoint};

/// C: a valid config creates an endpoint, which is then destroyed.
#[test]
fn accepts_a_valid_default_config() {
    // C: the valid config carries transmit and process packet functions -- no Rust
    // equivalent: the closures are per-call arguments to send_packet and receive_packet,
    // not Config fields
    let _endpoint = Endpoint::new(Config::default(), 0.0);
}

// C: reliable_endpoint_create(NULL, 0.0) returns NULL -- no Rust equivalent:
// Endpoint::new takes an owned Config by value and there is no null config to pass

// C: config.fragment_above = config.max_packet_size + 1 is refused -- covered by
// endpoint_rejects_fragment_threshold_above_max_packet in src/endpoint.rs

// C: max_fragments = 4 with fragment_size = 1024 and max_packet_size = 4 * 1024 + 1 is
// refused -- covered by endpoint_rejects_fragment_capacity_below_max_packet in
// src/endpoint.rs

/// C: a fragment capacity that exactly covers the maximum packet size is allowed, where
/// one fragment short is not.
#[test]
fn accepts_fragment_capacity_exactly_covering_max_packet() {
    let _endpoint = Endpoint::new(
        Config {
            max_fragments: 4,
            fragment_size: 1024,
            max_packet_size: 4 * 1024,
            fragment_above: 1024,
            ..Config::default()
        },
        0.0,
    );
}

/// C: `config.max_packet_size = 0` is refused.
#[test]
#[should_panic(expected = "config.max_packet_size > 0")]
fn rejects_zero_max_packet_size() {
    Endpoint::new(
        Config {
            max_packet_size: 0,
            ..Config::default()
        },
        0.0,
    );
}

/// C: `config.fragment_above = 0` is refused.
#[test]
#[should_panic(expected = "config.fragment_above > 0")]
fn rejects_zero_fragment_above() {
    Endpoint::new(
        Config {
            fragment_above: 0,
            ..Config::default()
        },
        0.0,
    );
}

/// C: `config.fragment_size = 0` is refused.
#[test]
#[should_panic(expected = "config.fragment_size > 0")]
fn rejects_zero_fragment_size() {
    Endpoint::new(
        Config {
            fragment_size: 0,
            ..Config::default()
        },
        0.0,
    );
}

/// C: `config.max_fragments = 0` is refused.
#[test]
#[should_panic(expected = "config.max_fragments > 0")]
fn rejects_zero_max_fragments() {
    Endpoint::new(
        Config {
            max_fragments: 0,
            ..Config::default()
        },
        0.0,
    );
}

/// C: `config.max_fragments = 257` is refused.
#[test]
#[should_panic(expected = "config.max_fragments <= 256")]
fn rejects_max_fragments_above_256() {
    Endpoint::new(
        Config {
            max_fragments: 257,
            ..Config::default()
        },
        0.0,
    );
}

// C: max_fragments = 256 with fragment_size = max_packet_size = 8421505 and
// fragment_above = 1 is refused: the product does not fit a packet length (INT_MAX - 9
// in C) -- no Rust equivalent at this magnitude: the port's packet lengths are u32, and
// 256 * 8421505 fits one; the same refusal is pinned above a u32 packet length by
// endpoint_rejects_fragment_capacity_above_a_packet_length in src/endpoint.rs

// C: max_packet_size = INT_MAX - 10 with fragment_size = max_packet_size, max_fragments
// = 1 and fragment_above = 1 is refused: fragment_size and the transmit buffer do not
// fit an int -- no Rust equivalent at this magnitude: the port's packet lengths are u32,
// and these values fit one; the same refusal is pinned above a u32 packet length by the
// endpoint_rejects_* unit tests in src/endpoint.rs

/// C: `config.ack_buffer_size = 0` is refused.
#[test]
#[should_panic(expected = "config.ack_buffer_size > 0")]
fn rejects_zero_ack_buffer_size() {
    Endpoint::new(
        Config {
            ack_buffer_size: 0,
            ..Config::default()
        },
        0.0,
    );
}

/// C: `config.sent_packets_buffer_size = -1` is refused. The field is a `usize` here, so
/// the test drives the same positive-size refusal with 0, which C refuses too.
#[test]
#[should_panic(expected = "config.sent_packets_buffer_size > 0")]
fn rejects_zero_sent_packets_buffer_size() {
    Endpoint::new(
        Config {
            sent_packets_buffer_size: 0,
            ..Config::default()
        },
        0.0,
    );
}

/// C: `config.received_packets_buffer_size = 0` is refused.
#[test]
#[should_panic(expected = "config.received_packets_buffer_size > 0")]
fn rejects_zero_received_packets_buffer_size() {
    Endpoint::new(
        Config {
            received_packets_buffer_size: 0,
            ..Config::default()
        },
        0.0,
    );
}

// C: config.fragment_reassembly_buffer_size = 0 is refused -- covered by
// endpoint_rejects_zero_fragment_reassembly_buffer in src/endpoint.rs

/// C: `config.rtt_history_size = 0` is refused.
#[test]
#[should_panic(expected = "config.rtt_history_size > 0")]
fn rejects_zero_rtt_history_size() {
    Endpoint::new(
        Config {
            rtt_history_size: 0,
            ..Config::default()
        },
        0.0,
    );
}

// C: config.packet_header_size = -1 is refused -- no Rust equivalent: the field is a
// usize and cannot be negative; a negative value cast into it lands on a huge size and
// is refused by the u32 packet length checks

// C: config.packet_header_size = INT_MAX is refused: packet_header_size plus
// max_packet_size does not fit an int -- no Rust equivalent at this magnitude: the
// port's packet lengths are u32, and this sum fits one; the same refusal is pinned above
// a u32 packet length by endpoint_rejects_packet_header_size_plus_max_packet_size_above_u32
// in src/endpoint.rs

// C: config.transmit_packet_function = NULL is refused -- no Rust equivalent: Config has
// no transmit function field; the closure is a per-call argument to send_packet

// C: config.process_packet_function = NULL is refused -- no Rust equivalent: Config has
// no process function field; the closure is a per-call argument to receive_packet

// C: reliable_checked_size(1, 4) succeeds with bytes == 4, and
// reliable_checked_size(256, sizeof(uint16_t)) succeeds with bytes == 512 -- no Rust
// equivalent: the helper is private to the C create path
// C: reliable_checked_size refuses 0 and -1 counts, a zero element size, and a product
// that does not fit a size_t (4 * SIZE_MAX / 2, INT_MAX * SIZE_MAX / 3) rather than
// wrapping -- no Rust equivalent: the helper is private to the C create path; the
// port's create-time checks refuse the same configs without a public checked multiply
