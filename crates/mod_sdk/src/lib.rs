//! Guest-side SDK for Torse behavior mods.
//!
//! The interface contract lives in `crates/mod_api/wit/behavior.wit`; this
//! crate re-exports the generated bindings so mods never invoke the generator
//! themselves, and adds the deterministic helpers every mod needs. The SDK
//! must not depend on any host-side engine crate.

pub mod bindings {
    wit_bindgen::generate!({
        path: "../mod_api/wit/behavior.wit",
        world: "mod-behavior",
    });
}

pub use bindings::exports::torse::behavior::invoke::Guest;

/// Deterministic integer PRNG seeded from the host-provided `rng-seed`.
///
/// The stream is part of the determinism contract: same seed, same sequence,
/// integer operations only, no platform float or time dependence.
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// splitmix64 step; returns the next value in the stream.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Unbiased value below `bound`; `bound` must be non-zero.
    pub fn below(&mut self, bound: u64) -> u64 {
        assert!(bound > 0, "bound must be non-zero");
        let zone = u64::MAX - (u64::MAX % bound) - 1;
        loop {
            let v = self.next_u64();
            if v <= zone {
                return v % bound;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Rng;

    #[test]
    fn rng_stream_is_reproducible() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..64 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn rng_below_stays_in_range() {
        let mut rng = Rng::new(7);
        for _ in 0..128 {
            assert!(rng.below(10) < 10);
        }
    }
}
