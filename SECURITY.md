# Security Policy

reliable.rs implements packet acknowledgement, fragmentation and reassembly over UDP. It
parses untrusted data straight off the wire, so a malformed or hostile packet must be
rejected rather than trusted.

## Reporting a vulnerability

**Please do not report security issues in public GitHub issues or pull requests.**

Report privately through either channel:

- **GitHub private vulnerability reporting** (preferred): on this repository, go to the
  **Security** tab → **Report a vulnerability**. This opens a private advisory visible only
  to the maintainers.
- **Email**: glenn@mas-bandwidth.com.

Please include enough detail to reproduce: the affected version or commit, a description of
the flaw, and — where possible — a proof-of-concept input or a small patch.

We will acknowledge your report, keep you updated on our assessment, and coordinate
disclosure timing with you. We prefer coordinated disclosure and will credit reporters who
wish to be named.

## Scope

In scope — bugs in this crate, above all on the RECEIVE path: a packet that causes a read or
write out of bounds, a panic, unbounded memory growth through fragment reassembly, or a
fragment accepted outside its declared bounds.

The crate is `#![forbid(unsafe_code)]`, so the interesting class is a hostile packet that is
accepted when it should be rejected, or that panics or exhausts memory.

Note the deliberate asymmetry, the same as the C library's: on SEND the caller is
responsible for passing sane sizes; on RECEIVE the library checks at runtime, because that
is where untrusted data arrives.

The wire format is specified in `STANDARD.md`. **A flaw in the *specification* — as opposed
to this implementation of it — is in scope and is more valuable to us**, because it affects
every implementation of reliable rather than one. Report those the same way.

## No known vulnerabilities

reliable has no published security advisories. The AEAD nonce-reuse issue that affected the
netcode family in July 2026 does not apply here: this library does not handle keys or
encryption.

## Supported versions

Security fixes land on the latest release. We do not backport to older release lines.
