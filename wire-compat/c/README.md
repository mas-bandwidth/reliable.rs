# Vendored C reference implementation

`reliable.c` and `reliable.h` are vendored verbatim from
[mas-bandwidth/reliable](https://github.com/mas-bandwidth/reliable) at version 1.4.5
(commit `e4e70927d1656ffcd822309fc2aa7af004c58f6a`), under the BSD 3-Clause licence in
this directory.

This is the reference implementation the wire-compatibility tests run the Rust port
against. It is pinned deliberately: the compatibility target is the reliable 1.4.5 wire
format, which is the same wire format as 1.3.4 — none of the releases between them
changed a byte on the wire. When upstream releases a new version, update this vendored
copy and the port together.

To update: copy the two files straight out of the tag and check they are byte-identical
to the blobs it names, then run `cargo test --manifest-path wire-compat/Cargo.toml`.

```
git -C <reliable> show vX.Y.Z:reliable.c > wire-compat/c/reliable.c
git -C <reliable> show vX.Y.Z:reliable.h > wire-compat/c/reliable.h
git -C <reliable> show vX.Y.Z:LICENCE    > wire-compat/c/LICENCE
```

`shim.c` is a small local shim that builds `reliable_config_t` inside C, so the Rust
test crate never has to mirror the struct layout.
