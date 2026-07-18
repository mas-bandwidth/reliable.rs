[![CI](https://github.com/mas-bandwidth/reliable.rs/actions/workflows/ci.yml/badge.svg)](https://github.com/mas-bandwidth/reliable.rs/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/reliable.svg)](https://crates.io/crates/reliable)
[![docs.rs](https://img.shields.io/docsrs/reliable)](https://docs.rs/reliable)

# Introduction

**reliable.rs** is a simple packet acknowledgement system for UDP-based protocols, written in Rust.

It's useful in situations where you need to know which UDP packets you sent were received by the other side.

It has the following features:

1. Acknowledgement when packets are received
2. Packet fragmentation and reassembly
3. RTT, jitter and packet loss estimates
4. Duplicate packets are detected and dropped

It is a faithful port of the C library [reliable](https://github.com/mas-bandwidth/reliable) and is wire compatible with it: packets written by one implementation are read by the other, byte for byte.

# Usage

reliable is designed to operate with your own network socket library.

First, create an endpoint on each side of the connection. In a client/server setup you would have one endpoint on each client, and n endpoints on the server, one for each client slot:

```rust
use reliable::{Config, Endpoint};

let config = Config {
    max_packet_size: 32 * 1024,
    fragment_above: 1200,
    max_fragments: 32,
    fragment_size: 1024,
    ..Config::default()
};

let mut endpoint = Endpoint::new(config, time);
```

Where the C library takes transmit and process packet callbacks in its config, the Rust port takes closures at the call site. Send packets through the endpoint, transmitting the framed packet (or its fragments) over your UDP socket:

```rust
endpoint.send_packet(packet_data, |sequence, data| {
    // send data over your own udp socket
});
```

For each packet you receive from your UDP socket, call this on the endpoint that should receive it:

```rust
endpoint.receive_packet(received_data, |sequence, data| {
    // read the packet here and process its contents,
    // return false if the packet should not be acked
    true
});
```

And get acks like this:

```rust
for &acked_sequence in endpoint.acks() {
    println!("acked packet {acked_sequence}");
}
```

Once you process all acks, clear them:

```rust
endpoint.clear_acks();
```

Or do both in one call with `endpoint.drain_acks()`, which yields the acked sequence numbers and leaves the array empty.

Before you send a packet, you can ask reliable what sequence number the sent packet will have:

```rust
let sequence = endpoint.next_packet_sequence();
```

This way you can map acked sequence numbers to the contents of packets you sent, for example, resending unacked messages until a packet that included that message was acked.

Make sure to update each endpoint once per-frame. This keeps track of network stats like latency, jitter, packet loss and bandwidth:

```rust
endpoint.update(time);
```

You can then grab stats from the endpoint:

```rust
println!(
    "rtt = {:.1}ms | jitter = {:.1}ms | packet loss = {:.1}%",
    endpoint.rtt_min(),
    endpoint.jitter_avg_vs_min_rtt(),
    endpoint.packet_loss(),
);
```

Endpoints clean up after themselves when dropped.

# Mapping from the C API

| C | Rust |
|---|---|
| `reliable_default_config` | `Config::default()` |
| `reliable_endpoint_create` | `Endpoint::new(config, time)` |
| `reliable_endpoint_destroy` | `Drop` |
| `reliable_endpoint_send_packet` + `transmit_packet_function` | `endpoint.send_packet(data, transmit)` |
| `reliable_endpoint_receive_packet` + `process_packet_function` | `endpoint.receive_packet(data, process)` |
| `reliable_endpoint_get_acks` / `clear_acks` | `endpoint.acks()` / `endpoint.clear_acks()` |
| `reliable_endpoint_next_packet_sequence` | `endpoint.next_packet_sequence()` |
| `reliable_endpoint_update` | `endpoint.update(time)` |
| `reliable_endpoint_rtt` etc. | `endpoint.rtt()` etc. |
| `reliable_endpoint_bandwidth` | `endpoint.bandwidth()` |
| `reliable_endpoint_counters` | `endpoint.counters()` |
| `reliable_endpoint_reset` | `endpoint.reset()` |
| `reliable_log_level` / `reliable_set_printf_function` | the [`log`](https://crates.io/crates/log) crate facade |
| `reliable_init` / `reliable_term` / custom allocators / assert handler | not needed in Rust |

# Building and testing

```
cargo build
cargo test
```

The minimum supported Rust version is 1.88. The crate contains no unsafe code (`#![forbid(unsafe_code)]`) and its only dependency is the [`log`](https://crates.io/crates/log) facade.

The test suite includes the C library's tests ported one for one, plus bounded runs of its soak harness (randomly sized fragmented packets over a lossy link, contents validated) and its fuzz harness (loss, reordering, duplication, bit corruption and random packet injection over a simulated link).

The C repo's example programs are ported under [examples](examples):

```
cargo run --example example
cargo run --example stats
cargo run --example soak -- 8192 --quiet
cargo run --example fuzz -- 100000 12345
```

# Wire compatibility

Wire compatibility with the C library is a locked-in invariant, not a one-time claim. The [wire-compat](wire-compat) test crate vendors the C reference implementation (pinned at 1.3.4) and links it directly into the test binary via FFI. On every push and pull request, on Linux, macOS and Windows, CI verifies that:

1. A Rust endpoint and a C endpoint exchanging bidirectional traffic (regular and fragmented) deliver every payload intact and ack everything the other sent.
2. A Rust endpoint pair and a C endpoint pair driven through the same deterministic exchange put **byte-identical** datagrams on the wire.

```
cargo test --manifest-path wire-compat/Cargo.toml
```

# Fuzzing

The C repo's libFuzzer harness (`fuzz_target.c`) is ported as a [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) target. The fuzz input is a script of send/inject operations against a live endpoint pair, so coverage-guided fuzzing reaches the header parser, fragment reassembly, ack processing and stale/duplicate rejection:

```
cargo install cargo-fuzz
cargo +nightly fuzz run fuzz_endpoint
```

CI runs a one minute fuzz smoke test on every push, and a scheduled workflow fuzzes for 30 minutes weekly.

# Caveats

reliable is a packet acknowledgement system, not a full messaging layer. Keep the following in mind:

1. Acks accumulate until you call `clear_acks`, so make sure you clear acks once you have processed them each frame. If the ack buffer fills up, additional acks are dropped and an error is logged.

2. The transmit closure passed to `send_packet` must not send packets on the same endpoint: it is called synchronously while the endpoint's transmit scratch buffer is in use. Sending on a *different* endpoint (e.g. loopback tests) is fine.

3. Endpoints are not thread safe. Use one endpoint per-thread, or protect each endpoint with your own lock.

# Author

The author of this library is [Glenn Fiedler](https://www.linkedin.com/in/glenn-fiedler-11b735302/).

Open source libraries by the same author include: [netcode](https://github.com/mas-bandwidth/netcode), [serialize](https://github.com/mas-bandwidth/serialize), and [yojimbo](https://github.com/mas-bandwidth/yojimbo)

If you find this software useful, [please consider sponsoring it](https://github.com/sponsors/mas-bandwidth). Thanks!

# License

[MBSL](LICENSE).

## Crediting

This library is licensed under the [Más Bandwidth Source License (MBSL)](LICENSE),
which is BSD 3-Clause plus one clause: products that incorporate it must include
this credit in their product credits, or in their documentation:

> **Más Bandwidth LLC**
> reliable.rs by Glenn Fiedler

Free to use, source open, credit required. Fair credit keeps open source honest.
