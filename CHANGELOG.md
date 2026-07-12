# Changelog

Versions track the upstream C library.

## 1.3.4 (2026-07-12)

Initial Rust port of [reliable](https://github.com/mas-bandwidth/reliable) 1.3.4.

Wire compatible with the C library: packets written by one implementation are read by
the other, byte for byte. See the README for how the C API maps onto the Rust API.
