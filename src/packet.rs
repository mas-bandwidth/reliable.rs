use log::debug;

use crate::{FRAGMENT_HEADER_BYTES, MAX_PACKET_HEADER_BYTES};

pub(crate) struct PacketHeader {
    pub bytes: usize,
    pub sequence: u16,
    pub ack: u16,
    pub ack_bits: u32,
}

pub(crate) struct FragmentHeader {
    pub bytes: usize,
    pub fragment_id: usize,
    pub num_fragments: usize,
    pub sequence: u16,
    pub ack: u16,
    pub ack_bits: u32,
}

/// Writes a packet header with variable-length encoding: ack_bits bytes that are all 1s
/// are implied rather than sent, and the ack is sent as a one byte offset from the
/// sequence number when they are close together. Returns the number of bytes written.
pub(crate) fn write_packet_header(
    packet_data: &mut [u8],
    sequence: u16,
    ack: u16,
    ack_bits: u32,
) -> usize {
    let mut prefix_byte: u8 = 0;

    if (ack_bits & 0x0000_00FF) != 0x0000_00FF {
        prefix_byte |= 1 << 1;
    }
    if (ack_bits & 0x0000_FF00) != 0x0000_FF00 {
        prefix_byte |= 1 << 2;
    }
    if (ack_bits & 0x00FF_0000) != 0x00FF_0000 {
        prefix_byte |= 1 << 3;
    }
    if (ack_bits & 0xFF00_0000) != 0xFF00_0000 {
        prefix_byte |= 1 << 4;
    }

    let sequence_difference = sequence.wrapping_sub(ack);
    if sequence_difference <= 255 {
        prefix_byte |= 1 << 5;
    }

    let mut p = 0;

    packet_data[p] = prefix_byte;
    p += 1;

    packet_data[p..p + 2].copy_from_slice(&sequence.to_le_bytes());
    p += 2;

    if sequence_difference <= 255 {
        packet_data[p] = sequence_difference as u8;
        p += 1;
    } else {
        packet_data[p..p + 2].copy_from_slice(&ack.to_le_bytes());
        p += 2;
    }

    if (ack_bits & 0x0000_00FF) != 0x0000_00FF {
        packet_data[p] = (ack_bits & 0xFF) as u8;
        p += 1;
    }
    if (ack_bits & 0x0000_FF00) != 0x0000_FF00 {
        packet_data[p] = ((ack_bits >> 8) & 0xFF) as u8;
        p += 1;
    }
    if (ack_bits & 0x00FF_0000) != 0x00FF_0000 {
        packet_data[p] = ((ack_bits >> 16) & 0xFF) as u8;
        p += 1;
    }
    if (ack_bits & 0xFF00_0000) != 0xFF00_0000 {
        packet_data[p] = ((ack_bits >> 24) & 0xFF) as u8;
        p += 1;
    }

    debug_assert!(p <= MAX_PACKET_HEADER_BYTES);

    p
}

pub(crate) fn read_packet_header(name: &str, packet_data: &[u8]) -> Option<PacketHeader> {
    let packet_bytes = packet_data.len();

    if packet_bytes < 3 {
        debug!("[{name}] packet too small for packet header (1)");
        return None;
    }

    let prefix_byte = packet_data[0];

    if (prefix_byte & 1) != 0 {
        debug!("[{name}] prefix byte does not indicate a regular packet");
        return None;
    }

    let sequence = u16::from_le_bytes([packet_data[1], packet_data[2]]);
    let mut p = 3;

    let ack = if (prefix_byte & (1 << 5)) != 0 {
        if packet_bytes < 3 + 1 {
            debug!("[{name}] packet too small for packet header (2)");
            return None;
        }
        let sequence_difference = packet_data[p];
        p += 1;
        sequence.wrapping_sub(sequence_difference as u16)
    } else {
        if packet_bytes < 3 + 2 {
            debug!("[{name}] packet too small for packet header (3)");
            return None;
        }
        let ack = u16::from_le_bytes([packet_data[p], packet_data[p + 1]]);
        p += 2;
        ack
    };

    let expected_bytes = (1..=4).filter(|i| (prefix_byte & (1 << i)) != 0).count();
    if packet_bytes < p + expected_bytes {
        debug!("[{name}] packet too small for packet header (4)");
        return None;
    }

    let mut ack_bits: u32 = 0xFFFF_FFFF;

    if (prefix_byte & (1 << 1)) != 0 {
        ack_bits &= 0xFFFF_FF00;
        ack_bits |= packet_data[p] as u32;
        p += 1;
    }
    if (prefix_byte & (1 << 2)) != 0 {
        ack_bits &= 0xFFFF_00FF;
        ack_bits |= (packet_data[p] as u32) << 8;
        p += 1;
    }
    if (prefix_byte & (1 << 3)) != 0 {
        ack_bits &= 0xFF00_FFFF;
        ack_bits |= (packet_data[p] as u32) << 16;
        p += 1;
    }
    if (prefix_byte & (1 << 4)) != 0 {
        ack_bits &= 0x00FF_FFFF;
        ack_bits |= (packet_data[p] as u32) << 24;
        p += 1;
    }

    Some(PacketHeader {
        bytes: p,
        sequence,
        ack,
        ack_bits,
    })
}

pub(crate) fn read_fragment_header(
    name: &str,
    packet_data: &[u8],
    max_fragments: usize,
    fragment_size: usize,
) -> Option<FragmentHeader> {
    let packet_bytes = packet_data.len();

    if packet_bytes < FRAGMENT_HEADER_BYTES {
        debug!("[{name}] packet is too small to read fragment header");
        return None;
    }

    let prefix_byte = packet_data[0];
    if prefix_byte != 1 {
        debug!("[{name}] prefix byte is not a fragment");
        return None;
    }

    let sequence = u16::from_le_bytes([packet_data[1], packet_data[2]]);
    let fragment_id = packet_data[3] as usize;
    let num_fragments = packet_data[4] as usize + 1;

    if num_fragments > max_fragments {
        debug!(
            "[{name}] num fragments {num_fragments} outside of range of max fragments {max_fragments}"
        );
        return None;
    }

    if fragment_id >= num_fragments {
        debug!(
            "[{name}] fragment id {fragment_id} outside of range of num fragments {num_fragments}"
        );
        return None;
    }

    let mut fragment_bytes = packet_bytes - FRAGMENT_HEADER_BYTES;
    let mut ack = 0;
    let mut ack_bits = 0;

    if fragment_id == 0 {
        let Some(packet_header) = read_packet_header(name, &packet_data[FRAGMENT_HEADER_BYTES..])
        else {
            debug!("[{name}] bad packet header in fragment");
            return None;
        };

        if packet_header.sequence != sequence {
            debug!(
                "[{name}] bad packet sequence in fragment. expected {sequence}, got {}",
                packet_header.sequence
            );
            return None;
        }

        // the packet header is re-encoded canonically during reassembly, so a non-canonical
        // header would shift where the fragment payload lands. reject it here instead.

        let mut canonical_header = [0u8; MAX_PACKET_HEADER_BYTES];
        let canonical_header_bytes = write_packet_header(
            &mut canonical_header,
            packet_header.sequence,
            packet_header.ack,
            packet_header.ack_bits,
        );
        if canonical_header_bytes != packet_header.bytes
            || canonical_header[..canonical_header_bytes]
                != packet_data
                    [FRAGMENT_HEADER_BYTES..FRAGMENT_HEADER_BYTES + canonical_header_bytes]
        {
            debug!("[{name}] non-canonical packet header in fragment");
            return None;
        }

        ack = packet_header.ack;
        ack_bits = packet_header.ack_bits;
        fragment_bytes = packet_bytes - packet_header.bytes - FRAGMENT_HEADER_BYTES;
    }

    if fragment_bytes > fragment_size {
        debug!("[{name}] fragment bytes {fragment_bytes} > fragment size {fragment_size}");
        return None;
    }

    if fragment_id != num_fragments - 1 && fragment_bytes != fragment_size {
        debug!(
            "[{name}] fragment {fragment_id} is {fragment_bytes} bytes, which is not the expected fragment size {fragment_size}"
        );
        return None;
    }

    Some(FragmentHeader {
        bytes: FRAGMENT_HEADER_BYTES,
        fragment_id,
        num_fragments,
        sequence,
        ack,
        ack_bits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_header_roundtrip_random() {
        // randomized roundtrip: any (sequence, ack, ack_bits) triple must survive
        // write/read exactly, whichever variable-length encoding it takes
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut packet_data = [0u8; MAX_PACKET_HEADER_BYTES];
        for _ in 0..100_000 {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let random = state.wrapping_mul(0x2545_F491_4F6C_DD1D);

            let sequence = random as u16;
            let ack = (random >> 16) as u16;
            let ack_bits = (random >> 32) as u32;

            let bytes_written = write_packet_header(&mut packet_data, sequence, ack, ack_bits);
            let header = read_packet_header("roundtrip", &packet_data[..bytes_written])
                .expect("read packet header");
            assert_eq!(header.bytes, bytes_written);
            assert_eq!(header.sequence, sequence);
            assert_eq!(header.ack, ack);
            assert_eq!(header.ack_bits, ack_bits);
        }
    }

    #[test]
    fn packet_header() {
        let mut packet_data = [0u8; MAX_PACKET_HEADER_BYTES];

        // worst case, sequence and ack are far apart, no packets acked

        let write_sequence = 10000;
        let write_ack = 100;
        let write_ack_bits = 0;

        let bytes_written =
            write_packet_header(&mut packet_data, write_sequence, write_ack, write_ack_bits);
        assert_eq!(bytes_written, MAX_PACKET_HEADER_BYTES);

        let header = read_packet_header("packet_header", &packet_data[..bytes_written])
            .expect("read packet header");
        assert_eq!(header.bytes, bytes_written);
        assert_eq!(header.sequence, write_sequence);
        assert_eq!(header.ack, write_ack);
        assert_eq!(header.ack_bits, write_ack_bits);

        // rare case. sequence and ack are far apart, significant # of acks are missing

        let write_sequence = 10000;
        let write_ack = 100;
        let write_ack_bits = 0xFEFE_FFFE;

        let bytes_written =
            write_packet_header(&mut packet_data, write_sequence, write_ack, write_ack_bits);
        assert_eq!(bytes_written, 1 + 2 + 2 + 3);

        let header = read_packet_header("packet_header", &packet_data[..bytes_written])
            .expect("read packet header");
        assert_eq!(header.bytes, bytes_written);
        assert_eq!(header.sequence, write_sequence);
        assert_eq!(header.ack, write_ack);
        assert_eq!(header.ack_bits, write_ack_bits);

        // common case under packet loss. sequence and ack are close together, some acks are missing

        let write_sequence = 200;
        let write_ack = 100;
        let write_ack_bits = 0xFFFE_FFFF;

        let bytes_written =
            write_packet_header(&mut packet_data, write_sequence, write_ack, write_ack_bits);
        assert_eq!(bytes_written, 1 + 2 + 1 + 1);

        let header = read_packet_header("packet_header", &packet_data[..bytes_written])
            .expect("read packet header");
        assert_eq!(header.bytes, bytes_written);
        assert_eq!(header.sequence, write_sequence);
        assert_eq!(header.ack, write_ack);
        assert_eq!(header.ack_bits, write_ack_bits);

        // ideal case. no packet loss

        let write_sequence = 200;
        let write_ack = 100;
        let write_ack_bits = 0xFFFF_FFFF;

        let bytes_written =
            write_packet_header(&mut packet_data, write_sequence, write_ack, write_ack_bits);
        assert_eq!(bytes_written, 1 + 2 + 1);

        let header = read_packet_header("packet_header", &packet_data[..bytes_written])
            .expect("read packet header");
        assert_eq!(header.bytes, bytes_written);
        assert_eq!(header.sequence, write_sequence);
        assert_eq!(header.ack, write_ack);
        assert_eq!(header.ack_bits, write_ack_bits);
    }
}
