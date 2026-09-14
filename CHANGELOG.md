# Changelog

Versions track the upstream C library.

## 1.4.5 (2026-09-13)

Tracks C reliable 1.4.5. The vendored reference in `wire-compat/c/` moves from 1.3.4 to
1.4.5 (tag `v1.4.5`, commit `e4e70927d1656ffcd822309fc2aa7af004c58f6a`) and the
wire-compatibility tests run the port against it. Nothing on the wire changes: no C
release between 1.3.4 and 1.4.5 touched the packet format, and a Rust pair and a C pair
driven through the same exchange still put byte-identical datagrams on the wire.

Every upstream change from 1.3.4 — the version this port was written against — through
1.4.5 is dispositioned below. The ones with a Rust analogue are carried; the ones
without say why.

1.3.5 and 1.4.0 come first because they are small. 1.3.5 changed no library code at all:
it is the crediting request and the funding link. 1.4.0 removed two dead helpers,
`reliable_read_bytes` and `reliable_write_bytes`, which were unreachable in C and were
never written here; fixed an MSVC-hostile empty initializer in the C test driver
(`uint8_t packet_data[TEST_MAX_PACKET_BYTES] = {}` to `= {0}`), which is C syntax with no
Rust counterpart; and added an rtt test. 1.4.0 is also where `STANDARD.md` became a
normative document; it is vendored in this repo verbatim, with a CI job that fails if it
drifts from upstream.

### Carried into the port

- **A configuration that cannot fit a packet is refused at endpoint create** (C 1.4.3;
  reliable.rs#20, PR #22, reported as security#41). `Endpoint::new` rejects
  `fragment_above > max_packet_size`, and a fragment capacity too small to carry a
  maximum-sized packet, using a division rather than a product that could overflow.
  Such a configuration used to be accepted and then fail on the first large packet.

- **`reset` clears every field a getter can return** (C 1.4.2). `Endpoint::reset` now
  zeroes rtt, rtt min/max/avg, all three jitter statistics, packet loss and the three
  bandwidth figures, and refills the rtt history with the `-1.0` empty sentinel. It
  cleared the acks, counters, sequence number and tracking buffers before; the
  statistics survived a reset, and the rtt history survived it well enough that the
  next `update` recomputed the old samples.

- **An empty rtt history is decided by the sample count, not by a sentinel value**
  (C 1.4.2). `update` started its minimum at `10000.0` and treated that value as
  "no samples", so a link whose round trip actually reached ten seconds reported an
  rtt min, max and average of zero. The minimum now starts at `f32::MAX` and the
  sample count decides. Ported from the upstream tests: `rtt_min_large` (RL-02) and
  `endpoint_reset_clears_stats` (RL-03), both red against the previous code.

- **A zero `fragment_reassembly_buffer_size` is refused at create, by name** (C 1.4.2).
  It was refused before, but opaquely: the panic came from an assert inside the sequence
  buffer's constructor, two calls down, naming `num_entries`. `Endpoint::new` now refuses
  it beside the other config checks, and the test names the check rather than accepting
  any panic.

- **Two create-time refusals on `packet_header_size`** (C 1.4.3, `6055a51`,
  `reliable.c:638` and `reliable.c:644`). C refuses a negative `packet_header_size`, and
  a `packet_header_size` plus `max_packet_size` that does not fit its `int` packet
  length. The port cannot receive a negative value — the field is a `usize` — and a
  caller who casts a negative `int` into it lands on a huge value, which the second
  check refuses. That second check is carried with `u32` as the bound rather than
  `INT_MAX`, because a `u32` is what the port stores the length in: `send_packet` and
  `receive_packet` record `packet_header_size + packet_bytes` in the `u32` the bandwidth
  statistics are computed from, and both sites used to cast with `as u32`, which
  truncates in silence. They are `u32::try_from(..)` now, and the create-time checks are
  what make the conversion unable to fail — the send path is bounded by
  `packet_header_size + max_packet_size`, the receive path by that sum plus the packet
  and fragment headers its length gate allows, and each bound is refused at create if it
  does not fit.

### Not applicable to the Rust port

- **Strict floating-point builds** (C 1.4.1) — the C build replaced `-ffast-math` /
  `/fp:fast` with `-ffp-contract=off` / `/fp:precise` so float statistics near a wire
  cannot diverge between architectures. rustc does no fast-math and fuses no
  multiply-add on its own; the port contains no `mul_add`, and its float arithmetic is
  already IEEE-strict. Nothing floating point reaches the wire in either language.

- **A bounded export surface, static internals, and a C-only build** (C 1.4.2) — the
  27 exported functions under `RELIABLE_EXPORT`, the internal helpers made `static`,
  and `c_std_99` are C linkage and build concerns. Rust's module system already scopes
  the port: `packet`, `sequence_buffer` and `endpoint` are private modules and the
  public surface is what `src/lib.rs` re-exports.

- **`reliable_endpoint_create` returns NULL on any allocation failure** (C 1.4.2) —
  Rust's allocator does not return null to safe code; an allocation that cannot be
  satisfied aborts the process, and there is no pointer for the caller to check. The
  port's matching contract is the documented one: a config error is a programmer bug
  and `Endpoint::new` panics on it rather than returning a `Result`.

- **`reliable_endpoint_get_acks` returns a const view** (C 1.4.2) — `Endpoint::acks`
  already returns `&[u16]` (`src/endpoint.rs:689`), a shared borrow the compiler will
  not let the caller write through or hold across a mutation.

- **The lifetime of every pointer a callback receives is stated in the header**
  (C 1.4.2) — the port hands callbacks `&[u8]` slices whose lifetimes the borrow
  checker enforces; there is no pointer whose lifetime has to be described in prose.

- **`STANDARD.md`'s definition of `ack_bits`** (C 1.4.2) — the spec is vendored here
  verbatim and a CI job fails if it drifts from upstream. The copy in this repo already
  matches upstream at 1.4.5, and the port's ack-bit generation was never the behaviour
  the wording had described loosely.

- **64-bit fragment arithmetic, 64-bit bandwidth totals, and eight bytes of tail slack
  on the reassembly buffer** (C 1.4.3) — the C fix widened `int` arithmetic that sized a
  packet, widened the byte totals the bandwidth statistics accumulate, and padded the
  reassembly buffer so a reader loading an eight-byte window stays inside it. The port
  computes packet lengths in `usize` and copies through bounds-checked slices; it has no
  eight-byte window read to protect, and a length mistake is a panic, not an over-read.
  Its bandwidth totals already accumulate in `u64` (`bytes_sent` in
  `smoothed_bandwidth_kbps`), which is what C widened to. The part of that release the
  port did need — the `packet_header_size` refusals — is carried above.

- **Fragment reassembly survives an allocator that returns NULL** (C 1.4.4) — the C
  receive path guarded its reassembly allocation with an assert alone and dereferenced
  null in release builds. Rust allocation cannot return NULL to safe code: `vec![0; n]`
  either yields a buffer or aborts, so there is no null to dereference and no partially
  built reassembly entry to unwind. The counter widening that came with the C fix
  (`NUM_FRAGMENTS_INVALID` also counting a local allocation failure) has no Rust
  analogue for the same reason.

- **A reassembled packet never carries stale heap bytes** (C 1.4.5) — the port's
  reassembly buffer is already zero-initialised at allocation:
  `reassembly_data.packet_data = vec![0; packet_buffer_size];`
  (`src/endpoint.rs:614`). A hole left by a future logic error would deliver zeros
  here too, never the contents of freed memory.

- **A caller's unterminated endpoint name cannot be over-read** (C 1.4.5) — the C fix
  forces a NUL onto the endpoint's copy of `reliable_config_t.name` and bounds the
  twelve create-time rejection logs with `%.*s`. `Config::name` is a `String`
  (`src/endpoint.rs:14`): Rust strings are length-delimited, carry no terminator, and
  every log that prints the name prints exactly its bytes. There is nothing to run off
  the end of.

Publishing to crates.io is done by hand from Glenn's account.

## 1.3.4 (2026-07-12)

Initial Rust port of [reliable](https://github.com/mas-bandwidth/reliable) 1.3.4.

Wire compatible with the C library: packets written by one implementation are read by
the other, byte for byte. See the README for how the C API maps onto the Rust API.
