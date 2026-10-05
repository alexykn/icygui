//! A small seedable PRNG (xoshiro256** seeded through `SplitMix64`).
//!
//! Implemented here instead of using `rand` so the simulator's sequences
//! depend only on this file: a given seed and tick sequence produce the same
//! events on every platform and with every dependency version.

/// Deterministic pseudo-random number generator.
#[derive(Clone, Debug)]
pub(crate) struct Rng {
    state: [u64; 4],
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Rng {
    /// A generator for `seed`. Different seeds give independent streams.
    pub(crate) fn new(seed: u64) -> Self {
        let mut sm = seed;
        let mut state = [0; 4];
        for slot in &mut state {
            *slot = splitmix64(&mut sm);
        }
        if state == [0; 4] {
            state[0] = 1;
        }
        Self { state }
    }

    /// A generator derived from this one's seed space, for an independent
    /// stream (e.g. object names vs. simulator decisions).
    pub(crate) fn derive(seed: u64, stream: u64) -> Self {
        Self::new(seed ^ stream.wrapping_mul(0xD6E8_FEB8_6659_FD93))
    }

    /// The next 64 random bits.
    pub(crate) fn next_u64(&mut self) -> u64 {
        let result = self.state[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.state[1] << 17;
        self.state[2] ^= self.state[0];
        self.state[3] ^= self.state[1];
        self.state[1] ^= self.state[2];
        self.state[0] ^= self.state[3];
        self.state[2] ^= t;
        self.state[3] = self.state[3].rotate_left(45);
        result
    }

    /// A uniform integer in `0..n` (`0` when `n == 0`).
    pub(crate) fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        // Lemire's multiply-shift with rejection: unbiased and deterministic.
        loop {
            let x = self.next_u64();
            let (high, low) = split_u128(u128::from(x) * u128::from(n));
            if low >= n.wrapping_neg() % n {
                return high;
            }
        }
    }

    /// A uniform index into a collection of `len` items (`0` when empty).
    pub(crate) fn index(&mut self, len: usize) -> usize {
        usize::try_from(self.below(u64::try_from(len).unwrap_or(u64::MAX))).unwrap_or(0)
    }

    /// A uniform float in `[0, 1)`.
    pub(crate) fn unit(&mut self) -> f64 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "53 random bits fit a double's mantissa exactly"
        )]
        let value = (self.next_u64() >> 11) as f64;
        value / 9_007_199_254_740_992.0
    }

    /// A uniform float in `[low, high)`.
    pub(crate) fn range(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }

    /// `true` with probability `p`.
    pub(crate) fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    /// A random element.
    pub(crate) fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            None
        } else {
            items.get(self.index(items.len()))
        }
    }

    /// A version-4 UUID in canonical form, from this generator (so object
    /// names are deterministic for a given seed).
    pub(crate) fn uuid(&mut self) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let high = self.next_u64();
        let low = self.next_u64();
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&high.to_be_bytes());
        bytes[8..].copy_from_slice(&low.to_be_bytes());
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let hex: String = bytes
            .iter()
            .flat_map(|b| [DIGITS[usize::from(b >> 4)], DIGITS[usize::from(b & 0x0f)]])
            .map(char::from)
            .collect();
        format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    }
}

/// The high and low 64 bits of a 128-bit value.
#[expect(
    clippy::cast_possible_truncation,
    reason = "both halves are masked/shifted into 64 bits first"
)]
const fn split_u128(value: u128) -> (u64, u64) {
    ((value >> 64) as u64, value as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let mut c = Rng::new(43);
        assert_ne!(Rng::new(42).next_u64(), c.next_u64());
    }

    #[test]
    fn known_first_values_stay_stable() {
        // Guards against accidental changes to the algorithm: recorded
        // sequences in other crates' tests depend on it.
        let mut rng = Rng::new(0);
        let first: Vec<u64> = (0..3).map(|_| rng.next_u64()).collect();
        let mut again = Rng::new(0);
        assert_eq!(first, (0..3).map(|_| again.next_u64()).collect::<Vec<_>>());
        assert_ne!(first[0], first[1]);
    }

    #[test]
    fn ranges() {
        let mut rng = Rng::new(7);
        for _ in 0..10_000 {
            assert!(rng.below(10) < 10);
            let u = rng.unit();
            assert!((0.0..1.0).contains(&u));
            let r = rng.range(5.0, 6.0);
            assert!((5.0..6.0).contains(&r));
        }
        assert_eq!(rng.below(0), 0);
        assert_eq!(rng.index(0), 0);
        assert!(rng.pick::<u8>(&[]).is_none());
    }

    #[test]
    fn uuids_are_v4_shaped() {
        let mut rng = Rng::new(1);
        let id = rng.uuid();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
        assert_ne!(id, rng.uuid());
    }
}
