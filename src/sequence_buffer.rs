use crate::{sequence_greater_than, sequence_less_than};

/// A circular buffer of entries indexed by u16 sequence number. This is the core data
/// structure used to track sent packets, received packets and packets under reassembly.
/// (Where the C library marks empty slots with a 0xFFFFFFFF sentinel in a u32 array,
/// this port stores `Option<u16>` — the same four bytes per slot, without the sentinel.)
///
/// The capacity is rounded up to a power of two, so each slot corresponds to the same
/// sequence number on every pass through the sequence space.
pub(crate) struct SequenceBuffer<T> {
    sequence: u16,
    entry_sequence: Vec<Option<u16>>,
    entries: Vec<T>,
}

impl<T: Default> SequenceBuffer<T> {
    pub fn new(num_entries: usize) -> Self {
        assert!(num_entries > 0);
        let num_entries = num_entries.next_power_of_two();
        Self {
            sequence: 0,
            entry_sequence: vec![None; num_entries],
            entries: std::iter::repeat_with(T::default)
                .take(num_entries)
                .collect(),
        }
    }

    /// The most recent sequence number inserted, plus one.
    pub fn sequence(&self) -> u16 {
        self.sequence
    }

    pub fn num_entries(&self) -> usize {
        self.entries.len()
    }

    fn is_stale(&self, sequence: u16) -> bool {
        // a buffer covering the whole sequence space holds every sequence number, so
        // nothing is stale (its capacity does not fit the u16 used for the window)
        self.entries.len() < 65536
            && sequence_less_than(
                sequence,
                self.sequence.wrapping_sub(self.entries.len() as u16),
            )
    }

    pub fn reset(&mut self) {
        self.sequence = 0;
        self.entry_sequence.fill(None);
        for entry in &mut self.entries {
            *entry = T::default();
        }
    }

    fn remove_entries(&mut self, start_sequence: u16, finish_sequence: u16) {
        let num_entries = self.entries.len();
        let start = start_sequence as usize;
        let mut finish = finish_sequence as usize;
        if finish < start {
            finish += 65536;
        }
        if finish - start < num_entries {
            for sequence in start..=finish {
                self.entry_sequence[sequence % num_entries] = None;
                self.entries[sequence % num_entries] = T::default();
            }
        } else {
            for index in 0..num_entries {
                self.entry_sequence[index] = None;
                self.entries[index] = T::default();
            }
        }
    }

    /// Returns false if the sequence number is too old to be inserted (stale).
    pub fn can_insert(&self, sequence: u16) -> bool {
        !self.is_stale(sequence)
    }

    /// Inserts an entry, advancing the buffer if the sequence number is newer than the
    /// most recent. Returns None if the sequence number is stale. The returned entry is
    /// reset to its default value.
    pub fn insert(&mut self, sequence: u16) -> Option<&mut T> {
        if self.is_stale(sequence) {
            return None;
        }
        if sequence_greater_than(sequence.wrapping_add(1), self.sequence) {
            self.remove_entries(self.sequence, sequence);
            self.sequence = sequence.wrapping_add(1);
        }
        let index = sequence as usize % self.entries.len();
        self.entry_sequence[index] = Some(sequence);
        self.entries[index] = T::default();
        Some(&mut self.entries[index])
    }

    /// Like [`insert`](Self::insert), but the advance check runs before the stale check:
    /// a sequence number far enough ahead to be ambiguous under wrap-around advances the
    /// buffer instead of being rejected as stale. This matches the C library, where the
    /// fragment reassembly buffer inserts with this variant (insert_with_cleanup) and the
    /// sent/received packet buffers insert with the other.
    pub fn insert_advance_first(&mut self, sequence: u16) -> Option<&mut T> {
        if sequence_greater_than(sequence.wrapping_add(1), self.sequence) {
            self.remove_entries(self.sequence, sequence);
            self.sequence = sequence.wrapping_add(1);
        } else if self.is_stale(sequence) {
            return None;
        }
        let index = sequence as usize % self.entries.len();
        self.entry_sequence[index] = Some(sequence);
        self.entries[index] = T::default();
        Some(&mut self.entries[index])
    }

    /// Advances the buffer to the given sequence number without inserting an entry,
    /// dropping any entries that fall out of the window.
    pub fn advance(&mut self, sequence: u16) {
        if sequence_greater_than(sequence.wrapping_add(1), self.sequence) {
            self.remove_entries(self.sequence, sequence);
            self.sequence = sequence.wrapping_add(1);
        }
    }

    pub fn remove(&mut self, sequence: u16) {
        let index = sequence as usize % self.entries.len();
        if self.entry_sequence[index].is_some() {
            self.entry_sequence[index] = None;
            self.entries[index] = T::default();
        }
    }

    pub fn exists(&self, sequence: u16) -> bool {
        self.entry_sequence[sequence as usize % self.entries.len()] == Some(sequence)
    }

    pub fn find(&self, sequence: u16) -> Option<&T> {
        let index = sequence as usize % self.entries.len();
        if self.entry_sequence[index] == Some(sequence) {
            Some(&self.entries[index])
        } else {
            None
        }
    }

    pub fn find_mut(&mut self, sequence: u16) -> Option<&mut T> {
        let index = sequence as usize % self.entries.len();
        if self.entry_sequence[index] == Some(sequence) {
            Some(&mut self.entries[index])
        } else {
            None
        }
    }

    /// Generates the (ack, ack_bits) pair describing the most recent received packet and
    /// the 32 packets preceding it, for inclusion in an outgoing packet header.
    pub fn generate_ack_bits(&self) -> (u16, u32) {
        let ack = self.sequence.wrapping_sub(1);
        let mut ack_bits: u32 = 0;
        let mut mask: u32 = 1;
        for i in 0..32u16 {
            if self.exists(ack.wrapping_sub(i)) {
                ack_bits |= mask;
            }
            mask <<= 1;
        }
        (ack, ack_bits)
    }

    #[cfg(test)]
    pub fn raw_entry(&self, index: usize) -> (Option<u16>, &T) {
        (self.entry_sequence[index], &self.entries[index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestSequenceData {
        sequence: u16,
    }

    const TEST_SEQUENCE_BUFFER_SIZE: usize = 256;

    #[test]
    fn sequence_buffer() {
        let mut sequence_buffer =
            SequenceBuffer::<TestSequenceData>::new(TEST_SEQUENCE_BUFFER_SIZE);

        assert_eq!(sequence_buffer.sequence(), 0);
        assert_eq!(sequence_buffer.num_entries(), TEST_SEQUENCE_BUFFER_SIZE);

        for i in 0..TEST_SEQUENCE_BUFFER_SIZE {
            assert!(sequence_buffer.find(i as u16).is_none());
        }

        for i in 0..=TEST_SEQUENCE_BUFFER_SIZE * 4 {
            let entry = sequence_buffer.insert(i as u16).expect("insert");
            entry.sequence = i as u16;
            assert_eq!(sequence_buffer.sequence(), (i + 1) as u16);
        }

        for i in 0..=TEST_SEQUENCE_BUFFER_SIZE {
            assert!(sequence_buffer.insert(i as u16).is_none());
        }

        let mut index = TEST_SEQUENCE_BUFFER_SIZE * 4;
        for _ in 0..TEST_SEQUENCE_BUFFER_SIZE {
            let entry = sequence_buffer.find(index as u16).expect("find");
            assert_eq!(entry.sequence, index as u16);
            index -= 1;
        }

        sequence_buffer.reset();

        assert_eq!(sequence_buffer.sequence(), 0);
        assert_eq!(sequence_buffer.num_entries(), TEST_SEQUENCE_BUFFER_SIZE);

        for i in 0..TEST_SEQUENCE_BUFFER_SIZE {
            assert!(sequence_buffer.find(i as u16).is_none());
        }
    }

    #[test]
    fn sequence_buffer_rounds_capacity_to_power_of_two() {
        let sequence_buffer = SequenceBuffer::<TestSequenceData>::new(255);
        assert_eq!(sequence_buffer.num_entries(), 256);
    }

    #[test]
    fn sequence_buffer_supports_full_sequence_space() {
        let mut sequence_buffer = SequenceBuffer::<TestSequenceData>::new(65535);
        assert_eq!(sequence_buffer.num_entries(), 65536);

        sequence_buffer.insert(u16::MAX).expect("insert");
        sequence_buffer.insert(0).expect("insert");

        assert!(sequence_buffer.exists(u16::MAX));
        assert!(sequence_buffer.exists(0));
    }

    #[test]
    fn sequence_buffer_retains_recent_entry_across_wrap() {
        let mut sequence_buffer = SequenceBuffer::<TestSequenceData>::new(255);

        sequence_buffer.insert(u16::MAX).expect("insert");
        sequence_buffer.insert(0).expect("insert");

        assert!(sequence_buffer.exists(u16::MAX));
        assert!(sequence_buffer.exists(0));

        let (ack, ack_bits) = sequence_buffer.generate_ack_bits();
        assert_eq!(ack, 0);
        assert_eq!(ack_bits & 0b11, 0b11);
    }

    #[test]
    fn sequence_buffer_insert_advance_first_retains_entry_across_wrap() {
        let mut sequence_buffer = SequenceBuffer::<TestSequenceData>::new(255);

        sequence_buffer
            .insert_advance_first(u16::MAX)
            .expect("insert");
        sequence_buffer.insert_advance_first(0).expect("insert");

        assert!(sequence_buffer.exists(u16::MAX));
        assert!(sequence_buffer.exists(0));

        let (ack, ack_bits) = sequence_buffer.generate_ack_bits();
        assert_eq!(ack, 0);
        assert_eq!(ack_bits & 0b11, 0b11);
    }

    #[test]
    fn generate_ack_bits() {
        let mut sequence_buffer =
            SequenceBuffer::<TestSequenceData>::new(TEST_SEQUENCE_BUFFER_SIZE);

        let (ack, ack_bits) = sequence_buffer.generate_ack_bits();
        assert_eq!(ack, 0xFFFF);
        assert_eq!(ack_bits, 0);

        for i in 0..=TEST_SEQUENCE_BUFFER_SIZE {
            sequence_buffer.insert(i as u16).expect("insert");
        }

        let (ack, ack_bits) = sequence_buffer.generate_ack_bits();
        assert_eq!(ack as usize, TEST_SEQUENCE_BUFFER_SIZE);
        assert_eq!(ack_bits, 0xFFFF_FFFF);

        sequence_buffer.reset();

        for input_ack in [1u16, 5, 9, 11] {
            sequence_buffer.insert(input_ack).expect("insert");
        }

        let (ack, ack_bits) = sequence_buffer.generate_ack_bits();
        assert_eq!(ack, 11);
        assert_eq!(
            ack_bits,
            1 | (1 << (11 - 9)) | (1 << (11 - 5)) | (1 << (11 - 1))
        );
    }
}
