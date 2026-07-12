# CLAUDE.md

## What this is

**reliable.rs** is the Rust port of the C library
[reliable](https://github.com/mas-bandwidth/reliable) — a packet acknowledgement system
for UDP-based protocols (acks, fragmentation/reassembly, rtt/jitter/packet-loss/bandwidth
stats). Published on crates.io as `reliable`. The library is ~1,600 lines across
`src/lib.rs` (constants, sequence math), `src/sequence_buffer.rs`, `src/packet.rs`
(header codec), and `src/endpoint.rs` (the endpoint, plus the C test suite ported
one-for-one at the bottom).

Build and test:

```
cargo test                                       # unit tests + bounded soak/fuzz harnesses + doctests
cargo test --manifest-path wire-compat/Cargo.toml  # wire compatibility vs the vendored C reference
cargo +nightly fuzz run fuzz_endpoint            # coverage-guided fuzzing (needs cargo-fuzz)
cargo clippy --all-targets -- -D warnings        # includes missing_docs via crate lint
```

MSRV is 1.88 (`rust-version` in Cargo.toml, verified by CI). `#![forbid(unsafe_code)]`
in the library; the only unsafe in the repo is FFI in the `wire-compat` test crate.

## Invariants — do not break these

1. **Wire compatibility with C reliable 1.3.4 is the defining property.** `wire-compat/`
   vendors the C reference (pinned at 1.3.4, commit `e00e11f`) and CI cross-feeds
   traffic both directions and requires byte-identical transcripts, on every push and
   PR, on Linux/macOS/Windows. Branch protection on `main` requires these checks (plus
   the rest of CI) for PR merges; force pushes are blocked. Any wire-format change is a
   coordinated event with upstream, never a refactor side effect.
2. **Crate versions track the upstream C library** (see CHANGELOG.md). When upstream
   ships a new version, update the vendored copy in `wire-compat/c/` and the port
   together.
3. **Behavior matches the C source, deliberately** — check ordering (stale before
   duplicate, size checks before both), counter increment points, log levels, the two
   sequence-buffer insert orderings (`insert` = stale-check first for sent/received,
   `insert_advance_first` = advance-check first for reassembly), and the stats
   smoothing formulas are all faithful ports. The ported tests in `endpoint.rs` are
   kept line-for-line diffable against upstream's; don't "modernize" them.

## Design contract (carried over from the C library — do not "fix")

- Config errors are programmer bugs: `Endpoint::new` panics via asserts, no `Result`.
- `receive_packet` is fire-and-forget; counters and `log` output are the diagnostics.
  No outcome enum.
- The caller owns time as f64 seconds (game-loop convention; tests drive synthetic
  time). No `Instant`/`Duration`.
- Wrapping sequence comparison is non-transitive, so `sequence_greater_than`/
  `sequence_less_than` stay free functions — never implement `Ord`/`PartialOrd` for
  sequence numbers.
- The rtt history keeps its `-1.0` empty sentinel (`Option<f32>` would double the
  buffer); the sequence buffer uses `Option<u16>` (free — niche-less either way).
- The transmit closure must not send on the same endpoint (shared transmit scratch
  buffer); documented in `send_packet`.

A longer record of accepted/declined idiom decisions is in the 2026-07-12 red/blue
review session.

## CI

`.github/workflows/ci.yml`: test (3 OS, debug+release), wire-compat (3 OS), rustfmt +
clippy `-D warnings`, docs (`RUSTDOCFLAGS=-D warnings`), msrv (1.88), `cargo publish
--dry-run`, `cargo audit`, 60s fuzz smoke (nightly; the fuzz target triple is pinned to
`x86_64-unknown-linux-gnu` because the prebuilt cargo-fuzz binary is musl-linked and
would otherwise pick a musl target incompatible with ASan). `fuzz.yml` runs 30 minutes
of coverage-guided fuzzing weekly. Dependabot watches actions + cargo.

## Releasing

Tag `vX.Y.Z` + `gh release create`, then `cargo publish` (Glenn's crates.io credentials
via `cargo login` on his machine). The `reliable` crate name is owned by Glenn's
account. Deliberately not yet set up (offered 2026-07-12, deferred): crates.io trusted
publishing via GitHub OIDC, a second crate owner, and a cross-link from the C repo's
README to this port.

## Status (2026-07-12)

v1.3.4 released: repo public, crate on crates.io, docs.rs built, CI fully green,
branch protection active, topics set. No open issues.
