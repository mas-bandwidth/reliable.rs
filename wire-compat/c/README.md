# Vendored C reference implementation

`reliable.c` and `reliable.h` are vendored verbatim from
[mas-bandwidth/reliable](https://github.com/mas-bandwidth/reliable) at version 1.3.4
(commit `e00e11f587efca418820544cccaf085296155834`), under the BSD 3-Clause licence in
this directory.

This is the reference implementation the wire-compatibility tests run the Rust port
against. It is pinned deliberately: the compatibility target is the reliable 1.3.4 wire
format. When upstream releases a new wire-affecting version, update this vendored copy
and the port together.

`shim.c` is a small local shim that builds `reliable_config_t` inside C, so the Rust
test crate never has to mirror the struct layout.
