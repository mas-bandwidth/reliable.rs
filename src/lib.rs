/*
    reliable.rs

    Copyright © 2017 - 2026, Más Bandwidth LLC

    Redistribution and use in source and binary forms, with or without modification, are permitted provided that the following conditions are met:

        1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following disclaimer.

        2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the following disclaimer
           in the documentation and/or other materials provided with the distribution.

        3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote products derived
           from this software without specific prior written permission.

    THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
    INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
    DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
    SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
    SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
    WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
    USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
*/

//! **reliable** is a simple packet acknowledgement system for UDP-based protocols.
//!
//! It's useful in situations where you need to know which UDP packets you sent were
//! received by the other side.
//!
//! It has the following features:
//!
//! 1. Acknowledgement when packets are received
//! 2. Packet fragmentation and reassembly
//! 3. RTT, jitter and packet loss estimates
//! 4. Duplicate packets are detected and dropped
//!
//! This crate is a faithful port of the C library
//! [reliable](https://github.com/mas-bandwidth/reliable) and is wire compatible with it.
//!
//! # Usage
//!
//! reliable is designed to operate with your own network socket library. Create one
//! [`Endpoint`] per connection: a client has one, a server has one per client slot.
//!
//! ```
//! use reliable::{Config, Endpoint};
//!
//! let mut endpoint = Endpoint::new(Config::default(), 0.0);
//!
//! // sending hands the framed packet (or its fragments) to your transmit closure
//! endpoint.send_packet(&[1, 2, 3], |_sequence, data| {
//!     // send data over your UDP socket
//!     # let _ = data;
//! });
//!
//! // call this for each packet received from your UDP socket
//! # let incoming: &[u8] = &[];
//! # if !incoming.is_empty() {
//! endpoint.receive_packet(incoming, |_sequence, data| {
//!     // process the packet contents here.
//!     // return false if the packet should not be acked
//!     # let _ = data;
//!     true
//! });
//! # }
//!
//! // once per-frame, update stats and process acks
//! endpoint.update(0.01);
//! for &acked_sequence in endpoint.acks() {
//!     // the packet you sent with this sequence number was received by the other side
//!     # let _ = acked_sequence;
//! }
//! endpoint.clear_acks();
//! ```
//!
//! In place of the C library's process-wide log level and printf function, this crate
//! logs through the [`log`] crate facade: install any logger implementation and enable
//! the `Debug` level to see per-packet detail.

mod endpoint;
mod packet;
mod sequence_buffer;

pub use endpoint::{Bandwidth, Config, Counters, Endpoint};

/// The version of the C library this crate is a port of (and wire compatible with).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The maximum size of a packet header in bytes.
pub const MAX_PACKET_HEADER_BYTES: usize = 9;

/// The size of a fragment header in bytes.
pub const FRAGMENT_HEADER_BYTES: usize = 5;

/// Returns true if sequence number `s1` is greater than `s2`, taking wrap-around into account.
pub fn sequence_greater_than(s1: u16, s2: u16) -> bool {
    ((s1 > s2) && (s1 - s2 <= 32768)) || ((s1 < s2) && (s2 - s1 > 32768))
}

/// Returns true if sequence number `s1` is less than `s2`, taking wrap-around into account.
pub fn sequence_less_than(s1: u16, s2: u16) -> bool {
    sequence_greater_than(s2, s1)
}
