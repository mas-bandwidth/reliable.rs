use log::{debug, error};

use crate::packet::{read_fragment_header, read_packet_header, write_packet_header};
use crate::sequence_buffer::SequenceBuffer;
use crate::{FRAGMENT_HEADER_BYTES, MAX_PACKET_HEADER_BYTES};

/// Configuration for an [`Endpoint`].
///
/// [`Config::default`] returns sensible defaults for a client/server game exchanging
/// packets at 60HZ.
#[derive(Debug, Clone)]
pub struct Config {
    /// Name of the endpoint. Used in log output.
    pub name: String,
    /// Maximum packet size that can be sent or received (bytes).
    pub max_packet_size: usize,
    /// Packets larger than this many bytes are sent as fragments.
    pub fragment_above: usize,
    /// Maximum number of fragments per-packet. 256 max. Must cover max_packet_size / fragment_size.
    pub max_fragments: usize,
    /// Size of each fragment (bytes).
    pub fragment_size: usize,
    /// Maximum number of acks buffered between calls to [`Endpoint::clear_acks`].
    pub ack_buffer_size: usize,
    /// Number of sent packets tracked for acks, packet loss and bandwidth stats.
    pub sent_packets_buffer_size: usize,
    /// Number of received packets tracked. Also the window for stale and duplicate packet rejection.
    pub received_packets_buffer_size: usize,
    /// Number of packets that can be under reassembly from fragments at the same time.
    pub fragment_reassembly_buffer_size: usize,
    /// Exponential smoothing factor for the rtt moving average.
    pub rtt_smoothing_factor: f32,
    /// Number of rtt samples kept for min/max/avg rtt and jitter.
    pub rtt_history_size: usize,
    /// Exponential smoothing factor for packet loss.
    pub packet_loss_smoothing_factor: f32,
    /// Exponential smoothing factor for bandwidth.
    pub bandwidth_smoothing_factor: f32,
    /// Assumed network header overhead per-packet, used only for bandwidth stats. 28 = IPv4 + UDP.
    pub packet_header_size: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: "endpoint".to_string(),
            max_packet_size: 16 * 1024,
            fragment_above: 1024,
            max_fragments: 16,
            fragment_size: 1024,
            ack_buffer_size: 256,
            sent_packets_buffer_size: 256,
            received_packets_buffer_size: 256,
            fragment_reassembly_buffer_size: 64,
            rtt_smoothing_factor: 0.0025,
            rtt_history_size: 512,
            packet_loss_smoothing_factor: 0.1,
            bandwidth_smoothing_factor: 0.1,
            packet_header_size: 28, // note: UDP over IPv4 = 20 + 8 bytes, UDP over IPv6 = 40 + 8 bytes
        }
    }
}

/// Counters tracked by an [`Endpoint`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Counters {
    pub num_packets_sent: u64,
    pub num_packets_received: u64,
    pub num_packets_acked: u64,
    pub num_packets_stale: u64,
    pub num_packets_invalid: u64,
    pub num_packets_too_large_to_send: u64,
    pub num_packets_too_large_to_receive: u64,
    pub num_fragments_sent: u64,
    pub num_fragments_received: u64,
    pub num_fragments_invalid: u64,
    pub num_packets_duplicate: u64,
}

/// Bandwidth statistics, in kilobits per-second.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Bandwidth {
    pub sent_kbps: f32,
    pub received_kbps: f32,
    pub acked_kbps: f32,
}

#[derive(Clone, Copy, Default)]
struct SentPacketData {
    time: f64,
    acked: bool,
    packet_bytes: u32,
}

#[derive(Clone, Copy, Default)]
struct ReceivedPacketData {
    time: f64,
    packet_bytes: u32,
}

struct FragmentReassemblyData {
    num_fragments_received: usize,
    num_fragments_total: usize,
    packet_data: Vec<u8>,
    packet_bytes: usize,
    packet_header_bytes: usize,
    fragment_received: [bool; 256],
}

impl Default for FragmentReassemblyData {
    fn default() -> Self {
        Self {
            num_fragments_received: 0,
            num_fragments_total: 0,
            packet_data: Vec::new(),
            packet_bytes: 0,
            packet_header_bytes: 0,
            fragment_received: [false; 256],
        }
    }
}

impl FragmentReassemblyData {
    fn store_fragment(
        &mut self,
        sequence: u16,
        ack: u16,
        ack_bits: u32,
        fragment_id: usize,
        fragment_size: usize,
        mut fragment_data: &[u8],
    ) {
        if fragment_id == 0 {
            let mut packet_header = [0u8; MAX_PACKET_HEADER_BYTES];
            let packet_header_bytes =
                write_packet_header(&mut packet_header, sequence, ack, ack_bits);
            self.packet_header_bytes = packet_header_bytes;
            self.packet_data
                [MAX_PACKET_HEADER_BYTES - packet_header_bytes..MAX_PACKET_HEADER_BYTES]
                .copy_from_slice(&packet_header[..packet_header_bytes]);
            fragment_data = &fragment_data[packet_header_bytes..];
        }

        if fragment_id == self.num_fragments_total - 1 {
            self.packet_bytes =
                (self.num_fragments_total - 1) * fragment_size + fragment_data.len();
        }

        let offset = MAX_PACKET_HEADER_BYTES + fragment_id * fragment_size;
        let end_offset = offset + fragment_data.len();
        if end_offset > self.packet_data.len() {
            debug!(
                "[reliable] invalid fragment size {} (would write past {}/{})",
                fragment_data.len(),
                end_offset,
                self.packet_data.len()
            );
            return;
        }

        self.packet_data[offset..end_offset].copy_from_slice(fragment_data);
    }
}

fn smoothed_bandwidth_kbps<T: Default>(
    buffer: &SequenceBuffer<T>,
    current_kbps: f32,
    smoothing_factor: f32,
    sample: impl Fn(&T) -> Option<(f64, u32)>,
) -> f32 {
    let base_sequence = buffer.sequence().wrapping_sub(buffer.num_entries() as u16);
    let num_samples = buffer.num_entries() / 2;
    let mut bytes_sent: u64 = 0;
    let mut start_time: Option<f64> = None;
    let mut finish_time = 0.0;
    for i in 0..num_samples {
        let Some(entry) = buffer.find(base_sequence.wrapping_add(i as u16)) else {
            continue;
        };
        let Some((time, packet_bytes)) = sample(entry) else {
            continue;
        };
        bytes_sent += u64::from(packet_bytes);
        if start_time.is_none_or(|start_time| time < start_time) {
            start_time = Some(time);
        }
        if time > finish_time {
            finish_time = time;
        }
    }
    if let Some(start_time) = start_time
        && finish_time > start_time
    {
        let kbps = (bytes_sent as f64 / (finish_time - start_time) * 8.0 / 1000.0) as f32;
        if (current_kbps - kbps).abs() > 0.00001 {
            current_kbps + (kbps - current_kbps) * smoothing_factor
        } else {
            kbps
        }
    } else {
        current_kbps
    }
}

/// A reliable endpoint.
///
/// One endpoint per connection: a client has one, a server has one per client slot.
/// There is no locking inside: every operation takes `&mut self`, so the borrow checker
/// enforces the C library's caveat that an endpoint must not be used concurrently.
pub struct Endpoint {
    config: Config,
    time: f64,
    rtt: f32,
    rtt_min: f32,
    rtt_max: f32,
    rtt_avg: f32,
    jitter_avg_vs_min_rtt: f32,
    jitter_max_vs_min_rtt: f32,
    jitter_stddev_vs_avg_rtt: f32,
    packet_loss: f32,
    sent_bandwidth_kbps: f32,
    received_bandwidth_kbps: f32,
    acked_bandwidth_kbps: f32,
    acks: Vec<u16>,
    sequence: u16,
    rtt_history_buffer: Vec<f32>,
    // scratch buffer for outgoing packets, so the send path doesn't allocate.
    // sized for whichever is larger: a regular packet or a fragment
    transmit_buffer: Vec<u8>,
    sent_packets: SequenceBuffer<SentPacketData>,
    received_packets: SequenceBuffer<ReceivedPacketData>,
    fragment_reassembly: SequenceBuffer<FragmentReassemblyData>,
    counters: Counters,
}

impl Endpoint {
    /// Creates an endpoint with the given config and current time in seconds.
    ///
    /// # Panics
    ///
    /// Panics if the config is invalid: zero sizes, or `max_fragments > 256`.
    pub fn new(config: Config, time: f64) -> Self {
        assert!(config.max_packet_size > 0);
        assert!(config.fragment_above > 0);
        assert!(config.max_fragments > 0);
        assert!(config.max_fragments <= 256);
        assert!(config.fragment_size > 0);
        assert!(config.ack_buffer_size > 0);
        assert!(config.sent_packets_buffer_size > 0);
        assert!(config.received_packets_buffer_size > 0);
        assert!(config.rtt_history_size > 0);

        let transmit_buffer_size = (config.max_packet_size + MAX_PACKET_HEADER_BYTES)
            .max(FRAGMENT_HEADER_BYTES + MAX_PACKET_HEADER_BYTES + config.fragment_size);

        Self {
            time,
            rtt: 0.0,
            rtt_min: 0.0,
            rtt_max: 0.0,
            rtt_avg: 0.0,
            jitter_avg_vs_min_rtt: 0.0,
            jitter_max_vs_min_rtt: 0.0,
            jitter_stddev_vs_avg_rtt: 0.0,
            packet_loss: 0.0,
            sent_bandwidth_kbps: 0.0,
            received_bandwidth_kbps: 0.0,
            acked_bandwidth_kbps: 0.0,
            acks: Vec::with_capacity(config.ack_buffer_size),
            sequence: 0,
            rtt_history_buffer: vec![-1.0; config.rtt_history_size],
            transmit_buffer: vec![0; transmit_buffer_size],
            sent_packets: SequenceBuffer::new(config.sent_packets_buffer_size),
            received_packets: SequenceBuffer::new(config.received_packets_buffer_size),
            fragment_reassembly: SequenceBuffer::new(config.fragment_reassembly_buffer_size),
            counters: Counters::default(),
            config,
        }
    }

    /// The endpoint's configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Returns the sequence number the next sent packet will have. Use it to map acked
    /// sequence numbers back to the contents of packets you sent.
    pub fn next_packet_sequence(&self) -> u16 {
        self.sequence
    }

    /// Sends a packet. The packet is handed to the `transmit` closure as
    /// `(sequence, packet_data)`, split into fragments first if larger than
    /// [`Config::fragment_above`]. The closure must not send packets on the same
    /// endpoint (the endpoint's transmit scratch buffer is in use while it runs).
    ///
    /// # Panics
    ///
    /// Panics if `packet_data` is empty.
    pub fn send_packet(&mut self, packet_data: &[u8], mut transmit: impl FnMut(u16, &[u8])) {
        assert!(!packet_data.is_empty());

        let packet_bytes = packet_data.len();

        if packet_bytes > self.config.max_packet_size {
            error!(
                "[{}] packet too large to send. packet is {} bytes, maximum is {}",
                self.config.name, packet_bytes, self.config.max_packet_size
            );
            self.counters.num_packets_too_large_to_send += 1;
            return;
        }

        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);

        let (ack, ack_bits) = self.received_packets.generate_ack_bits();

        debug!("[{}] sending packet {}", self.config.name, sequence);

        let time = self.time;
        let sent_packet_bytes = (self.config.packet_header_size + packet_bytes) as u32;
        let sent_packet_data = self
            .sent_packets
            .insert(sequence)
            .expect("sent packet data");
        sent_packet_data.time = time;
        sent_packet_data.packet_bytes = sent_packet_bytes;
        sent_packet_data.acked = false;

        if packet_bytes <= self.config.fragment_above {
            // regular packet

            debug!(
                "[{}] sending packet {} without fragmentation",
                self.config.name, sequence
            );

            let packet_header_bytes =
                write_packet_header(&mut self.transmit_buffer, sequence, ack, ack_bits);

            self.transmit_buffer[packet_header_bytes..packet_header_bytes + packet_bytes]
                .copy_from_slice(packet_data);

            transmit(
                sequence,
                &self.transmit_buffer[..packet_header_bytes + packet_bytes],
            );
        } else {
            // fragmented packet

            let mut packet_header = [0u8; MAX_PACKET_HEADER_BYTES];
            let packet_header_bytes =
                write_packet_header(&mut packet_header, sequence, ack, ack_bits);

            let num_fragments = packet_bytes.div_ceil(self.config.fragment_size);

            debug!(
                "[{}] sending packet {} as {} fragments",
                self.config.name, sequence, num_fragments
            );

            assert!(num_fragments >= 1);
            assert!(num_fragments <= self.config.max_fragments);

            let mut q = 0;

            for fragment_id in 0..num_fragments {
                let mut p = 0;

                self.transmit_buffer[p] = 1;
                p += 1;
                self.transmit_buffer[p..p + 2].copy_from_slice(&sequence.to_le_bytes());
                p += 2;
                self.transmit_buffer[p] = fragment_id as u8;
                p += 1;
                self.transmit_buffer[p] = (num_fragments - 1) as u8;
                p += 1;

                if fragment_id == 0 {
                    self.transmit_buffer[p..p + packet_header_bytes]
                        .copy_from_slice(&packet_header[..packet_header_bytes]);
                    p += packet_header_bytes;
                }

                let bytes_to_copy = self.config.fragment_size.min(packet_bytes - q);

                self.transmit_buffer[p..p + bytes_to_copy]
                    .copy_from_slice(&packet_data[q..q + bytes_to_copy]);
                p += bytes_to_copy;
                q += bytes_to_copy;

                transmit(sequence, &self.transmit_buffer[..p]);

                self.counters.num_fragments_sent += 1;
            }
        }

        self.counters.num_packets_sent += 1;
    }

    /// Call this for each packet received from your socket. Valid packets are passed to
    /// the `process` closure as `(sequence, packet_data)`; return true to accept and ack
    /// the packet, false to reject it (rejected packets are not acked and may be
    /// processed again if they arrive again). Stale and duplicate packets are dropped.
    ///
    /// # Panics
    ///
    /// Panics if `packet_data` is empty.
    pub fn receive_packet(
        &mut self,
        packet_data: &[u8],
        mut process: impl FnMut(u16, &[u8]) -> bool,
    ) {
        self.receive_packet_internal(packet_data, &mut process);
    }

    fn receive_packet_internal(
        &mut self,
        packet_data: &[u8],
        process: &mut dyn FnMut(u16, &[u8]) -> bool,
    ) {
        assert!(!packet_data.is_empty());

        let packet_bytes = packet_data.len();

        if packet_bytes
            > self.config.max_packet_size + MAX_PACKET_HEADER_BYTES + FRAGMENT_HEADER_BYTES
        {
            debug!(
                "[{}] packet too large to receive. packet is at least {} bytes, maximum is {}",
                self.config.name,
                packet_bytes - (MAX_PACKET_HEADER_BYTES + FRAGMENT_HEADER_BYTES),
                self.config.max_packet_size
            );
            self.counters.num_packets_too_large_to_receive += 1;
            return;
        }

        let prefix_byte = packet_data[0];

        if (prefix_byte & 1) == 0 {
            // regular packet

            self.counters.num_packets_received += 1;

            let Some(header) = read_packet_header(&self.config.name, packet_data) else {
                debug!(
                    "[{}] ignoring invalid packet. could not read packet header",
                    self.config.name
                );
                self.counters.num_packets_invalid += 1;
                return;
            };

            debug_assert!(header.bytes <= packet_bytes);

            let packet_payload_bytes = packet_bytes - header.bytes;

            if packet_payload_bytes > self.config.max_packet_size {
                error!(
                    "[{}] packet too large to receive. packet is at {} bytes, maximum is {}",
                    self.config.name, packet_payload_bytes, self.config.max_packet_size
                );
                self.counters.num_packets_too_large_to_receive += 1;
                return;
            }

            if !self.received_packets.can_insert(header.sequence) {
                debug!(
                    "[{}] ignoring stale packet {}",
                    self.config.name, header.sequence
                );
                self.counters.num_packets_stale += 1;
                return;
            }

            if self.received_packets.exists(header.sequence) {
                debug!(
                    "[{}] ignoring duplicate packet {}",
                    self.config.name, header.sequence
                );
                self.counters.num_packets_duplicate += 1;
                return;
            }

            debug!(
                "[{}] processing packet {}",
                self.config.name, header.sequence
            );

            if process(header.sequence, &packet_data[header.bytes..]) {
                debug!(
                    "[{}] process packet {} successful",
                    self.config.name, header.sequence
                );

                let time = self.time;
                let received_packet_bytes = (self.config.packet_header_size + packet_bytes) as u32;
                let received_packet_data = self
                    .received_packets
                    .insert(header.sequence)
                    .expect("received packet data");
                received_packet_data.time = time;
                received_packet_data.packet_bytes = received_packet_bytes;

                self.fragment_reassembly.advance(header.sequence);

                let mut ack_bits = header.ack_bits;
                for i in 0..32u16 {
                    if (ack_bits & 1) != 0 {
                        let ack_sequence = header.ack.wrapping_sub(i);

                        if let Some(sent_packet_data) = self.sent_packets.find_mut(ack_sequence)
                            && !sent_packet_data.acked
                        {
                            if self.acks.len() < self.config.ack_buffer_size {
                                debug!("[{}] acked packet {}", self.config.name, ack_sequence);
                                self.acks.push(ack_sequence);
                                self.counters.num_packets_acked += 1;
                                sent_packet_data.acked = true;

                                let rtt = ((self.time - sent_packet_data.time) as f32) * 1000.0;
                                debug_assert!(rtt >= 0.0);

                                let index = ack_sequence as usize % self.config.rtt_history_size;
                                self.rtt_history_buffer[index] = rtt;

                                if (self.rtt == 0.0 && rtt > 0.0)
                                    || (self.rtt - rtt).abs() < 0.00001
                                {
                                    self.rtt = rtt;
                                } else {
                                    self.rtt += (rtt - self.rtt) * self.config.rtt_smoothing_factor;
                                }
                            } else {
                                error!(
                                    "[{}] ack buffer is full. dropped ack for packet {}. make sure you call clear_acks",
                                    self.config.name, ack_sequence
                                );
                            }
                        }
                    }
                    ack_bits >>= 1;
                }
            } else {
                error!("[{}] process packet failed", self.config.name);
            }
        } else {
            // fragment packet

            let Some(fragment_header) = read_fragment_header(
                &self.config.name,
                packet_data,
                self.config.max_fragments,
                self.config.fragment_size,
            ) else {
                debug!(
                    "[{}] ignoring invalid fragment. could not read fragment header",
                    self.config.name
                );
                self.counters.num_fragments_invalid += 1;
                return;
            };

            if self.received_packets.exists(fragment_header.sequence) {
                debug!(
                    "[{}] ignoring fragment {} of packet {}. packet already received",
                    self.config.name, fragment_header.fragment_id, fragment_header.sequence
                );
                return;
            }

            if !self.fragment_reassembly.exists(fragment_header.sequence) {
                let Some(reassembly_data) = self
                    .fragment_reassembly
                    .insert_advance_first(fragment_header.sequence)
                else {
                    error!(
                        "[{}] ignoring invalid fragment. could not insert in reassembly buffer (stale)",
                        self.config.name
                    );
                    self.counters.num_fragments_invalid += 1;
                    return;
                };

                let packet_buffer_size = MAX_PACKET_HEADER_BYTES
                    + fragment_header.num_fragments * self.config.fragment_size;

                reassembly_data.num_fragments_total = fragment_header.num_fragments;
                reassembly_data.packet_data = vec![0; packet_buffer_size];

                self.received_packets.advance(fragment_header.sequence);
            }

            let reassembly_data = self
                .fragment_reassembly
                .find_mut(fragment_header.sequence)
                .expect("reassembly data");

            if fragment_header.num_fragments != reassembly_data.num_fragments_total {
                error!(
                    "[{}] ignoring invalid fragment. fragment count mismatch. expected {}, got {}",
                    self.config.name,
                    reassembly_data.num_fragments_total,
                    fragment_header.num_fragments
                );
                self.counters.num_fragments_invalid += 1;
                return;
            }

            if reassembly_data.fragment_received[fragment_header.fragment_id] {
                error!(
                    "[{}] ignoring fragment {} of packet {}. fragment already received",
                    self.config.name, fragment_header.fragment_id, fragment_header.sequence
                );
                return;
            }

            debug!(
                "[{}] received fragment {} of packet {} ({}/{})",
                self.config.name,
                fragment_header.fragment_id,
                fragment_header.sequence,
                reassembly_data.num_fragments_received + 1,
                fragment_header.num_fragments
            );

            reassembly_data.num_fragments_received += 1;
            reassembly_data.fragment_received[fragment_header.fragment_id] = true;

            reassembly_data.store_fragment(
                fragment_header.sequence,
                fragment_header.ack,
                fragment_header.ack_bits,
                fragment_header.fragment_id,
                self.config.fragment_size,
                &packet_data[fragment_header.bytes..],
            );

            if reassembly_data.num_fragments_received == reassembly_data.num_fragments_total {
                debug!(
                    "[{}] completed reassembly of packet {}",
                    self.config.name, fragment_header.sequence
                );

                let packet_header_bytes = reassembly_data.packet_header_bytes;
                let reassembled_packet_bytes = reassembly_data.packet_bytes;
                let reassembled_packet_data = std::mem::take(&mut reassembly_data.packet_data);

                self.receive_packet_internal(
                    &reassembled_packet_data[MAX_PACKET_HEADER_BYTES - packet_header_bytes
                        ..MAX_PACKET_HEADER_BYTES + reassembled_packet_bytes],
                    process,
                );

                self.fragment_reassembly.remove(fragment_header.sequence);
            }

            self.counters.num_fragments_received += 1;
        }
    }

    /// The sequence numbers of sent packets acked since the last call to
    /// [`clear_acks`](Self::clear_acks).
    pub fn acks(&self) -> &[u16] {
        &self.acks
    }

    /// Clears the ack array. Call this once per-frame after processing acks. If you
    /// don't, the ack buffer fills up and new acks are dropped.
    pub fn clear_acks(&mut self) {
        self.acks.clear();
    }

    /// Drains the ack array: yields the sequence numbers of sent packets acked since the
    /// last clear or drain, leaving the array empty. A one-call alternative to
    /// [`acks`](Self::acks) followed by [`clear_acks`](Self::clear_acks) that makes the
    /// clear impossible to forget.
    pub fn drain_acks(&mut self) -> impl Iterator<Item = u16> + '_ {
        self.acks.drain(..)
    }

    /// Resets the endpoint to its initial state: acks, counters, sequence number and all
    /// tracking buffers are cleared.
    pub fn reset(&mut self) {
        self.acks.clear();
        self.sequence = 0;
        self.counters = Counters::default();
        self.sent_packets.reset();
        self.received_packets.reset();
        self.fragment_reassembly.reset();
    }

    /// Updates rtt, jitter, packet loss and bandwidth stats. Call once per-frame with
    /// the current time in seconds.
    pub fn update(&mut self, time: f64) {
        self.time = time;

        // calculate min, max and average rtt
        {
            let mut min_rtt = 10000.0_f32;
            let mut max_rtt = 0.0_f32;
            let mut sum_rtt = 0.0_f32;
            let mut count = 0;
            for &rtt in &self.rtt_history_buffer {
                if rtt >= 0.0 {
                    if rtt < min_rtt {
                        min_rtt = rtt;
                    }
                    if rtt > max_rtt {
                        max_rtt = rtt;
                    }
                    sum_rtt += rtt;
                    count += 1;
                }
            }
            if min_rtt == 10000.0 {
                min_rtt = 0.0;
            }
            self.rtt_min = min_rtt;
            self.rtt_max = max_rtt;
            self.rtt_avg = if count > 0 {
                sum_rtt / count as f32
            } else {
                0.0
            };
        }

        // calculate average and max jitter vs. min rtt
        {
            let mut sum = 0.0_f32;
            let mut max = 0.0_f32;
            let mut count = 0;
            for &rtt in &self.rtt_history_buffer {
                if rtt >= 0.0 {
                    let difference = rtt - self.rtt_min;
                    sum += difference;
                    if difference > max {
                        max = difference;
                    }
                    count += 1;
                }
            }
            self.jitter_avg_vs_min_rtt = if count > 0 { sum / count as f32 } else { 0.0 };
            self.jitter_max_vs_min_rtt = max;
        }

        // calculate stddev jitter vs. avg rtt
        {
            let mut sum = 0.0_f32;
            let mut count = 0;
            for &rtt in &self.rtt_history_buffer {
                if rtt >= 0.0 {
                    let deviation = rtt - self.rtt_avg;
                    sum += deviation * deviation;
                    count += 1;
                }
            }
            self.jitter_stddev_vs_avg_rtt = if count > 0 {
                (sum / count as f32).sqrt()
            } else {
                0.0
            };
        }

        // calculate packet loss over the oldest half of the sent packet buffer,
        // where packets have had enough time to be acked
        {
            let base_sequence = self
                .sent_packets
                .sequence()
                .wrapping_sub(self.config.sent_packets_buffer_size as u16);
            let num_samples = self.config.sent_packets_buffer_size / 2;
            let mut num_sent = 0;
            let mut num_dropped = 0;
            for i in 0..num_samples {
                let sequence = base_sequence.wrapping_add(i as u16);
                if let Some(sent_packet_data) = self.sent_packets.find(sequence) {
                    num_sent += 1;
                    if !sent_packet_data.acked {
                        num_dropped += 1;
                    }
                }
            }
            if num_sent > 0 {
                let packet_loss = num_dropped as f32 / num_sent as f32 * 100.0;
                if (self.packet_loss - packet_loss).abs() > 0.00001 {
                    self.packet_loss +=
                        (packet_loss - self.packet_loss) * self.config.packet_loss_smoothing_factor;
                } else {
                    self.packet_loss = packet_loss;
                }
            } else {
                self.packet_loss = 0.0;
            }
        }

        // calculate sent, received and acked bandwidth

        self.sent_bandwidth_kbps = smoothed_bandwidth_kbps(
            &self.sent_packets,
            self.sent_bandwidth_kbps,
            self.config.bandwidth_smoothing_factor,
            |packet| Some((packet.time, packet.packet_bytes)),
        );

        self.received_bandwidth_kbps = smoothed_bandwidth_kbps(
            &self.received_packets,
            self.received_bandwidth_kbps,
            self.config.bandwidth_smoothing_factor,
            |packet| Some((packet.time, packet.packet_bytes)),
        );

        self.acked_bandwidth_kbps = smoothed_bandwidth_kbps(
            &self.sent_packets,
            self.acked_bandwidth_kbps,
            self.config.bandwidth_smoothing_factor,
            |packet| packet.acked.then_some((packet.time, packet.packet_bytes)),
        );
    }

    /// Round trip time exponentially smoothed moving average, in milliseconds.
    pub fn rtt(&self) -> f32 {
        self.rtt
    }

    /// Minimum round trip time over the rtt history window, in milliseconds.
    pub fn rtt_min(&self) -> f32 {
        self.rtt_min
    }

    /// Maximum round trip time over the rtt history window, in milliseconds.
    pub fn rtt_max(&self) -> f32 {
        self.rtt_max
    }

    /// Average round trip time over the rtt history window, in milliseconds.
    pub fn rtt_avg(&self) -> f32 {
        self.rtt_avg
    }

    /// Average jitter relative to the minimum rtt, in milliseconds.
    pub fn jitter_avg_vs_min_rtt(&self) -> f32 {
        self.jitter_avg_vs_min_rtt
    }

    /// Maximum jitter relative to the minimum rtt, in milliseconds.
    pub fn jitter_max_vs_min_rtt(&self) -> f32 {
        self.jitter_max_vs_min_rtt
    }

    /// Standard deviation of jitter relative to the average rtt, in milliseconds.
    pub fn jitter_stddev_vs_avg_rtt(&self) -> f32 {
        self.jitter_stddev_vs_avg_rtt
    }

    /// Packet loss as a percentage.
    pub fn packet_loss(&self) -> f32 {
        self.packet_loss
    }

    /// Sent, received and acked bandwidth in kilobits per-second.
    pub fn bandwidth(&self) -> Bandwidth {
        Bandwidth {
            sent_kbps: self.sent_bandwidth_kbps,
            received_kbps: self.received_bandwidth_kbps,
            acked_kbps: self.acked_bandwidth_kbps,
        }
    }

    /// The endpoint's counters.
    pub fn counters(&self) -> &Counters {
        &self.counters
    }
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("name", &self.config.name)
            .field("time", &self.time)
            .field("next_packet_sequence", &self.sequence)
            .field("num_acks", &self.acks.len())
            .field("counters", &self.counters)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[allow(clippy::needless_range_loop)]
mod tests {
    use super::*;

    const TEST_ACKS_NUM_ITERATIONS: usize = 256;
    const TEST_MAX_PACKET_BYTES: usize = 4 * 1024;

    fn test_config(name: &str) -> Config {
        Config {
            name: name.to_string(),
            ..Config::default()
        }
    }

    fn generate_packet_data_with_size(sequence: u16, packet_bytes: usize) -> Vec<u8> {
        assert!(packet_bytes >= 2);
        assert!(packet_bytes <= TEST_MAX_PACKET_BYTES);
        let mut packet_data = vec![0u8; packet_bytes];
        packet_data[0] = (sequence & 0xFF) as u8;
        packet_data[1] = ((sequence >> 8) & 0xFF) as u8;
        for i in 2..packet_bytes {
            packet_data[i] = ((i + sequence as usize) % 256) as u8;
        }
        packet_data
    }

    fn generate_packet_data(sequence: u16) -> Vec<u8> {
        let packet_bytes = (sequence as usize * 1023) % (TEST_MAX_PACKET_BYTES - 2) + 2;
        generate_packet_data_with_size(sequence, packet_bytes)
    }

    fn validate_packet_data(packet_data: &[u8]) {
        assert!(packet_data.len() >= 2);
        assert!(packet_data.len() <= TEST_MAX_PACKET_BYTES);
        let sequence = u16::from_le_bytes([packet_data[0], packet_data[1]]);
        assert_eq!(
            packet_data.len(),
            (sequence as usize * 1023) % (TEST_MAX_PACKET_BYTES - 2) + 2
        );
        for i in 2..packet_data.len() {
            assert_eq!(packet_data[i], ((i + sequence as usize) % 256) as u8);
        }
    }

    #[test]
    fn acks() {
        let mut time = 100.0;

        let mut sender = Endpoint::new(test_config("sender"), time);
        let mut receiver = Endpoint::new(test_config("receiver"), time);

        let delta_time = 0.01;
        let dummy_packet = [0u8; 8];

        for _ in 0..TEST_ACKS_NUM_ITERATIONS {
            sender.send_packet(&dummy_packet, |_, data| {
                receiver.receive_packet(data, |_, _| true);
            });
            receiver.send_packet(&dummy_packet, |_, data| {
                sender.receive_packet(data, |_, _| true);
            });

            sender.update(time);
            receiver.update(time);

            time += delta_time;
        }

        let mut sender_acked_packet = [false; TEST_ACKS_NUM_ITERATIONS];
        for &ack in sender.acks() {
            if (ack as usize) < TEST_ACKS_NUM_ITERATIONS {
                sender_acked_packet[ack as usize] = true;
            }
        }
        for i in 0..TEST_ACKS_NUM_ITERATIONS / 2 {
            assert!(sender_acked_packet[i]);
        }

        let mut receiver_acked_packet = [false; TEST_ACKS_NUM_ITERATIONS];
        for &ack in receiver.acks() {
            if (ack as usize) < TEST_ACKS_NUM_ITERATIONS {
                receiver_acked_packet[ack as usize] = true;
            }
        }
        for i in 0..TEST_ACKS_NUM_ITERATIONS / 2 {
            assert!(receiver_acked_packet[i]);
        }
    }

    #[test]
    fn acks_packet_loss() {
        let mut time = 100.0;

        let mut sender = Endpoint::new(test_config("sender"), time);
        let mut receiver = Endpoint::new(test_config("receiver"), time);

        let delta_time = 0.1;
        let dummy_packet = [0u8; 8];

        for i in 0..TEST_ACKS_NUM_ITERATIONS {
            let drop_packet = (i % 2) != 0;

            sender.send_packet(&dummy_packet, |_, data| {
                if !drop_packet {
                    receiver.receive_packet(data, |_, _| true);
                }
            });
            receiver.send_packet(&dummy_packet, |_, data| {
                if !drop_packet {
                    sender.receive_packet(data, |_, _| true);
                }
            });

            sender.update(time);
            receiver.update(time);

            time += delta_time;
        }

        let mut sender_acked_packet = [false; TEST_ACKS_NUM_ITERATIONS];
        for &ack in sender.acks() {
            if (ack as usize) < TEST_ACKS_NUM_ITERATIONS {
                sender_acked_packet[ack as usize] = true;
            }
        }
        for i in 0..TEST_ACKS_NUM_ITERATIONS / 2 {
            assert_eq!(sender_acked_packet[i], (i + 1) % 2 == 1);
        }

        let mut receiver_acked_packet = [false; TEST_ACKS_NUM_ITERATIONS];
        for &ack in receiver.acks() {
            if (ack as usize) < TEST_ACKS_NUM_ITERATIONS {
                receiver_acked_packet[ack as usize] = true;
            }
        }
        for i in 0..TEST_ACKS_NUM_ITERATIONS / 2 {
            assert_eq!(receiver_acked_packet[i], (i + 1) % 2 == 1);
        }
    }

    #[test]
    fn duplicate_packets() {
        const NUM_ITERATIONS: usize = 16;

        let mut sender = Endpoint::new(test_config("sender"), 100.0);
        let mut receiver = Endpoint::new(test_config("receiver"), 100.0);

        let mut num_processed = 0;
        let dummy_packet = [0u8; 8];

        for _ in 0..NUM_ITERATIONS {
            // deliver each packet to the receiver twice, simulating duplication on the network
            sender.send_packet(&dummy_packet, |_, data| {
                receiver.receive_packet(data, |_, _| {
                    num_processed += 1;
                    true
                });
                receiver.receive_packet(data, |_, _| {
                    num_processed += 1;
                    true
                });
            });
        }

        assert_eq!(num_processed, NUM_ITERATIONS);
        assert_eq!(
            receiver.counters().num_packets_received,
            2 * NUM_ITERATIONS as u64
        );
        assert_eq!(
            receiver.counters().num_packets_duplicate,
            NUM_ITERATIONS as u64
        );

        // duplicate fragments arriving after their packet was delivered must not restart reassembly

        let fragmented_sequence = sender.next_packet_sequence();

        let large_packet = [0u8; 2048];
        sender.send_packet(&large_packet, |_, data| {
            receiver.receive_packet(data, |_, _| {
                num_processed += 1;
                true
            });
            receiver.receive_packet(data, |_, _| {
                num_processed += 1;
                true
            });
        });

        assert_eq!(num_processed, NUM_ITERATIONS + 1);
        assert!(!receiver.fragment_reassembly.exists(fragmented_sequence));
    }

    #[test]
    fn stale_packets() {
        const NUM_ITERATIONS: usize = 300;

        let mut sender = Endpoint::new(test_config("sender"), 100.0);
        let mut receiver = Endpoint::new(test_config("receiver"), 100.0);

        let mut first_packet: Vec<u8> = Vec::new();
        let mut num_processed = 0;
        let dummy_packet = [0u8; 8];

        // send enough packets that sequence 0 falls out of the receive window (256 entries)

        for _ in 0..NUM_ITERATIONS {
            sender.send_packet(&dummy_packet, |sequence, data| {
                if sequence == 0 && first_packet.is_empty() {
                    first_packet = data.to_vec();
                }
                receiver.receive_packet(data, |_, _| {
                    num_processed += 1;
                    true
                });
            });
        }

        assert_eq!(num_processed, NUM_ITERATIONS);
        assert!(!first_packet.is_empty());

        // replaying the first packet must be rejected as stale, not processed

        receiver.receive_packet(&first_packet, |_, _| {
            num_processed += 1;
            true
        });

        assert_eq!(num_processed, NUM_ITERATIONS);
        assert_eq!(receiver.counters().num_packets_stale, 1);
    }

    #[test]
    fn ack_buffer_overflow() {
        const NUM_PACKETS: usize = 32;
        const BUFFER_SIZE: usize = 16;

        // undersized ack buffer on the sender, so a single received packet acking 32 sent packets overflows it

        let mut sender = Endpoint::new(
            Config {
                ack_buffer_size: BUFFER_SIZE,
                ..test_config("sender")
            },
            100.0,
        );
        let mut receiver = Endpoint::new(test_config("receiver"), 100.0);

        let dummy_packet = [0u8; 8];

        for _ in 0..NUM_PACKETS {
            sender.send_packet(&dummy_packet, |_, data| {
                receiver.receive_packet(data, |_, _| true);
            });
        }

        // one packet back from the receiver acks all 32, but only 16 fit in the ack buffer. the rest are dropped

        receiver.send_packet(&dummy_packet, |_, data| {
            sender.receive_packet(data, |_, _| true);
        });

        assert_eq!(sender.acks().len(), BUFFER_SIZE);
        assert_eq!(sender.counters().num_packets_acked, BUFFER_SIZE as u64);

        // once the caller clears acks, the dropped acks are reported on the next packet that covers them

        sender.clear_acks();

        receiver.send_packet(&dummy_packet, |_, data| {
            sender.receive_packet(data, |_, _| true);
        });

        assert_eq!(sender.acks().len(), NUM_PACKETS - BUFFER_SIZE);
        assert_eq!(sender.counters().num_packets_acked, NUM_PACKETS as u64);
    }

    #[test]
    fn packets() {
        let mut time = 100.0;

        let mut sender = Endpoint::new(
            Config {
                fragment_above: 500,
                ..test_config("sender")
            },
            time,
        );
        let mut receiver = Endpoint::new(
            Config {
                fragment_above: 500,
                ..test_config("receiver")
            },
            time,
        );

        let delta_time = 0.1;

        for _ in 0..16 {
            for _ in 0..2 {
                let sequence = sender.next_packet_sequence();
                let packet_data = generate_packet_data(sequence);
                sender.send_packet(&packet_data, |_, data| {
                    receiver.receive_packet(data, |_, data| {
                        validate_packet_data(data);
                        true
                    });
                });
            }

            sender.update(time);
            receiver.update(time);

            sender.clear_acks();
            receiver.clear_acks();

            time += delta_time;
        }
    }

    fn generate_packet_data_large() -> Vec<u8> {
        let data_bytes = TEST_MAX_PACKET_BYTES - 2;
        let mut packet_data = vec![0u8; data_bytes + 2];
        packet_data[0] = (data_bytes & 0xFF) as u8;
        packet_data[1] = ((data_bytes >> 8) & 0xFF) as u8;
        for i in 2..data_bytes {
            packet_data[i] = (i % 256) as u8;
        }
        packet_data
    }

    fn validate_packet_data_large(packet_data: &[u8]) {
        assert!(packet_data.len() >= 2);
        assert!(packet_data.len() <= TEST_MAX_PACKET_BYTES);
        let data_bytes = u16::from_le_bytes([packet_data[0], packet_data[1]]) as usize;
        assert_eq!(packet_data.len(), data_bytes + 2);
        for i in 2..data_bytes {
            assert_eq!(packet_data[i], (i % 256) as u8);
        }
    }

    #[test]
    fn large_packets() {
        let time = 100.0;

        let large_config = |name: &str| Config {
            max_packet_size: TEST_MAX_PACKET_BYTES,
            fragment_above: TEST_MAX_PACKET_BYTES,
            ..test_config(name)
        };

        let mut sender = Endpoint::new(large_config("sender"), time);
        let mut receiver = Endpoint::new(large_config("receiver"), time);

        let packet_data = generate_packet_data_large();
        assert_eq!(packet_data.len(), TEST_MAX_PACKET_BYTES);
        sender.send_packet(&packet_data, |_, data| {
            receiver.receive_packet(data, |_, data| {
                validate_packet_data_large(data);
                true
            });
        });

        sender.update(time);
        receiver.update(time);

        sender.clear_acks();
        receiver.clear_acks();

        assert_eq!(receiver.counters().num_packets_too_large_to_receive, 0);
        assert_eq!(receiver.counters().num_packets_received, 1);
    }

    #[test]
    fn sequence_buffer_rollover() {
        let mut sender = Endpoint::new(
            Config {
                fragment_above: 500,
                ..test_config("sender")
            },
            100.0,
        );
        let mut receiver = Endpoint::new(
            Config {
                fragment_above: 500,
                ..test_config("receiver")
            },
            100.0,
        );

        let packet_data = [0u8; 16];

        let mut num_packets_sent: u64 = 0;
        for _ in 0..=32767 {
            sender.send_packet(&packet_data, |_, data| {
                receiver.receive_packet(data, |_, _| true);
            });
            num_packets_sent += 1;
        }

        sender.send_packet(&packet_data, |_, data| {
            receiver.receive_packet(data, |_, _| true);
        });
        num_packets_sent += 1;

        assert_eq!(receiver.counters().num_packets_received, num_packets_sent);
        assert_eq!(receiver.counters().num_fragments_invalid, 0);
    }

    #[test]
    fn fragment_cleanup() {
        let mut time = 100.0;

        let mut sender = Endpoint::new(test_config("sender"), time);
        let mut receiver = Endpoint::new(
            Config {
                fragment_reassembly_buffer_size: 4,
                ..test_config("receiver")
            },
            time,
        );

        let delta_time = 0.1;

        let fragment_size = sender.config().fragment_size;
        let packet_sizes = [fragment_size + fragment_size / 2, 10, 10, 10, 10];

        // send more packets than the reassembly buffer holds, so the buffer wraps around.
        // only one transmit per send is allowed through, so the fragmented packet is only
        // partially delivered and its reassembly never completes.

        assert!(packet_sizes.len() > receiver.config().fragment_reassembly_buffer_size);

        for packet_size in packet_sizes {
            let mut allow_packets = 1;

            let sequence = sender.next_packet_sequence();
            let packet_data = generate_packet_data_with_size(sequence, packet_size);
            sender.send_packet(&packet_data, |_, data| {
                if allow_packets > 0 {
                    allow_packets -= 1;
                    receiver.receive_packet(data, |_, _| true);
                }
            });

            sender.update(time);
            receiver.update(time);

            sender.clear_acks();
            receiver.clear_acks();

            time += delta_time;
        }

        // the in-progress reassembly of packet 0 was evicted when the buffer wrapped
        // around, and eviction must have dropped its packet data (the Rust analog of the
        // C test's tracking-allocator leak check)

        assert!(!receiver.fragment_reassembly.exists(0));
        for index in 0..receiver.fragment_reassembly.num_entries() {
            let (entry_sequence, entry) = receiver.fragment_reassembly.raw_entry(index);
            if entry_sequence.is_none() {
                assert!(entry.packet_data.is_empty());
            }
        }
    }

    #[test]
    fn endpoint_reset() {
        let mut time = 100.0;

        let mut sender = Endpoint::new(
            Config {
                fragment_above: 500,
                ..test_config("sender")
            },
            time,
        );
        let mut receiver = Endpoint::new(
            Config {
                fragment_above: 500,
                ..test_config("receiver")
            },
            time,
        );

        let dummy_packet = [0u8; 8];

        // exchange packets both ways so acks and counters accumulate

        for _ in 0..8 {
            sender.send_packet(&dummy_packet, |_, data| {
                receiver.receive_packet(data, |_, _| true);
            });
            receiver.send_packet(&dummy_packet, |_, data| {
                sender.receive_packet(data, |_, _| true);
            });

            sender.update(time);
            receiver.update(time);

            time += 0.01;
        }

        assert!(!sender.acks().is_empty());
        assert!(sender.counters().num_packets_sent > 0);

        // leave a fragment reassembly in progress on the receiver by delivering only the
        // first fragment of a large packet

        let mut allow_packets = 1;
        let large_packet = [0u8; 1500];
        sender.send_packet(&large_packet, |_, data| {
            if allow_packets > 0 {
                allow_packets -= 1;
                receiver.receive_packet(data, |_, _| true);
            }
        });

        sender.reset();
        receiver.reset();

        assert_eq!(sender.next_packet_sequence(), 0);
        assert_eq!(receiver.next_packet_sequence(), 0);

        assert!(sender.acks().is_empty());

        assert_eq!(*sender.counters(), Counters::default());
        assert_eq!(*receiver.counters(), Counters::default());

        // reset must have dropped the in-progress reassembly data (the Rust analog of
        // the C test's tracking-allocator leak check)

        for index in 0..receiver.fragment_reassembly.num_entries() {
            let (entry_sequence, entry) = receiver.fragment_reassembly.raw_entry(index);
            assert_eq!(entry_sequence, None);
            assert!(entry.packet_data.is_empty());
        }

        // the endpoints must work normally after reset

        for _ in 0..8 {
            sender.send_packet(&dummy_packet, |_, data| {
                receiver.receive_packet(data, |_, _| true);
            });
            receiver.send_packet(&dummy_packet, |_, data| {
                sender.receive_packet(data, |_, _| true);
            });

            sender.update(time);
            receiver.update(time);

            time += 0.01;
        }

        assert!(!sender.acks().is_empty());
        assert!(receiver.counters().num_packets_received > 0);
    }
}
