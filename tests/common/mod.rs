#![allow(dead_code)]

/// Small deterministic xorshift64* PRNG, so the test harnesses are reproducible and the
/// crate needs no rand dependency.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn next_u8(&mut self) -> u8 {
        (self.next_u64() >> 56) as u8
    }

    pub fn next_usize(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// Random integer in `[a, b]` inclusive.
    pub fn range(&mut self, a: usize, b: usize) -> usize {
        a + self.next_usize(b - a + 1)
    }
}
