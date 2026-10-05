//! The only source of randomness in mint.
//!
//! Bytes come from the operating system CSPRNG through `getrandom`
//! (`getentropy` on macOS, `getrandom(2)` on Linux, `ProcessPrng` on Windows).
//! Uniform choice uses rejection sampling against a power-of-two mask, so no
//! remainder operation and no bias enter the sampling path. A test scans this
//! file and fails if the remainder operator appears in code.

use zeroize::Zeroize;

const BUF_LEN: usize = 256;

/// Buffered reader over the OS CSPRNG. Consumed bytes are wiped from the
/// buffer, and the whole buffer is wiped on drop.
pub struct SecureRng {
    buf: [u8; BUF_LEN],
    pos: usize,
}

impl SecureRng {
    pub fn new() -> Self {
        Self { buf: [0; BUF_LEN], pos: BUF_LEN }
    }

    fn next_u32(&mut self) -> u32 {
        if self.pos + 4 > BUF_LEN {
            // An OS random source failure is unrecoverable; stopping beats
            // producing a weak password.
            getrandom::fill(&mut self.buf).expect("the operating system random source failed");
            self.pos = 0;
        }
        let bytes = &mut self.buf[self.pos..self.pos + 4];
        let value = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        bytes.zeroize();
        self.pos += 4;
        value
    }

    /// A uniformly distributed integer in `0..n`.
    ///
    /// Draws 32 random bits, masks them to the smallest power of two that
    /// covers `n`, and rejects values `>= n`. Every accepted value is equally
    /// likely; at most half of the draws are rejected.
    pub fn below(&mut self, n: usize) -> usize {
        assert!(n > 0 && n <= u32::MAX as usize, "range must be 1..=u32::MAX");
        if n == 1 {
            return 0;
        }
        let max = (n - 1) as u32;
        let mask = u32::MAX >> max.leading_zeros();
        loop {
            let candidate = self.next_u32() & mask;
            if candidate <= max {
                return candidate as usize;
            }
        }
    }

    /// Uniformly chooses one element.
    pub fn choose<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    /// Fisher–Yates shuffle driven by the same CSPRNG.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }
}

impl Default for SecureRng {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SecureRng {
    fn drop(&mut self) {
        self.buf.zeroize();
    }
}
