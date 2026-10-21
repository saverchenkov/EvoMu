//! Injectable game-mechanic RNG (seeded test RNG).
//!
//! Every *game-mechanic* random roll (mining crits, ECM jam rolls, moon-chunk jackpots, residue
//! waste, invention/mutaplasmid/turret rolls) draws from a [`MechanicRng`] owned by
//! [`crate::router::ServerContext`] and shared with [`crate::system_actor::SystemRegistry`].
//!
//! - **Production default** ([`MechanicRng::os`]): every draw reads 8 bytes from the OS CSPRNG
//!   (`/dev/urandom`), exactly as the former per-call-site helpers did. A failed read yields `None`
//!   and each call site keeps its previous fallback.
//! - **Seeded mode** ([`MechanicRng::seeded`]): a deterministic SplitMix64 stream (Steele, Lea &
//!   Flood, "Fast Splittable Pseudorandom Number Generators", OOPSLA 2014), so tests can assert
//!   exact outcomes by value.
//!
//! Security-sensitive randomness (password salts, JWT signing secrets, PKCE codes, refresh tokens,
//! session keys) does NOT use this type and stays on the OS CSPRNG in `eve-db::accounts`.

use std::sync::Mutex;

/// SplitMix64 increment (golden-ratio constant `0x9E37_79B9_7F4A_7C15`).
const SPLITMIX64_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// Seed used by the test/demo constructor `ServerContext::new()` (only compiled with `cfg(test)` or
/// the `demo-seed` feature). Production constructors never seed.
pub const TEST_MODE_DEFAULT_SEED: u64 = 0x00C5_5EED_0000_0001;

/// One SplitMix64 step: advances `state` and returns the mixed 64-bit output.
pub fn splitmix64_next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(SPLITMIX64_GAMMA);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Maps a 64-bit draw to a uniform `f64` in `[0, 1)` using its top 53 bits.
pub fn u64_to_unit_f64(raw: u64) -> f64 {
    ((raw >> 11) as f64) * (1.0 / ((1u64 << 53) as f64))
}

/// Game-mechanic random source: OS CSPRNG by default, SplitMix64 when seeded.
#[derive(Debug, Default)]
pub struct MechanicRng {
    /// `None` = OS CSPRNG; `Some(state)` = SplitMix64 state.
    seeded_state: Mutex<Option<u64>>,
}

impl MechanicRng {
    /// Production RNG: every draw comes from the OS CSPRNG.
    pub fn os() -> Self {
        Self::default()
    }

    /// Deterministic RNG seeded with `seed`.
    pub fn seeded(seed: u64) -> Self {
        Self {
            seeded_state: Mutex::new(Some(seed)),
        }
    }

    /// Switches to a SplitMix64 stream at `seed` (`Some`) or back to the OS CSPRNG (`None`).
    pub fn set_seed(&self, seed: Option<u64>) {
        *self.seeded_state.lock().unwrap_or_else(|e| e.into_inner()) = seed;
    }

    /// Returns `true` when draws are deterministic (seeded mode).
    pub fn is_seeded(&self) -> bool {
        self.seeded_state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Next raw 64-bit draw; `None` only if the OS CSPRNG cannot be read.
    pub fn try_next_u64(&self) -> Option<u64> {
        {
            let mut guard = self.seeded_state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(state) = guard.as_mut() {
                return Some(splitmix64_next(state));
            }
        }
        let mut buf = [0u8; 8];
        getrandom::getrandom(&mut buf).ok()?;
        Some(u64::from_le_bytes(buf))
    }

    /// Next uniform draw in `[0, 1)`; `None` only if the OS CSPRNG cannot be read.
    pub fn try_unit_f64(&self) -> Option<f64> {
        self.try_next_u64().map(u64_to_unit_f64)
    }

    /// Bernoulli roll: `true` with probability `prob`. Non-positive/non-finite → `false`,
    /// `>= 1` → `true` (no draw consumed in either case), CSPRNG read failure → `false`.
    pub fn roll(&self, prob: f64) -> bool {
        if prob <= 0.0 || !prob.is_finite() {
            return false;
        }
        if prob >= 1.0 {
            return true;
        }
        self.try_unit_f64().is_some_and(|u| u < prob)
    }
}

impl crate::router::ServerContext {
    /// Builder: replaces the mechanic RNG stream with a deterministic SplitMix64 seeded at `seed`.
    pub fn with_seeded_rng(self, seed: u64) -> Self {
        self.mechanic_rng.set_seed(Some(seed));
        self
    }

    /// Re-seeds the shared mechanic RNG (`Some`) or restores the OS CSPRNG (`None`).
    pub fn set_mechanic_rng_seed(&self, seed: Option<u64>) {
        self.mechanic_rng.set_seed(seed);
    }

    pub fn set_invention_roll_override(&self, roll: Option<f64>) {
        *self
            .invention_roll_override
            .write()
            .unwrap_or_else(|e| e.into_inner()) = roll.map(|r| r.clamp(0.0, 0.999_999));
    }

    pub fn next_invention_roll(&self, job_id: u64, installer_id: u64, now_unix: u64) -> f64 {
        if let Some(r) = *self
            .invention_roll_override
            .read()
            .unwrap_or_else(|e| e.into_inner())
        {
            return r;
        }
        if let Some(u) = self.mechanic_rng.try_unit_f64() {
            return u;
        }
        let ticks = (self.now_blueos_filetime_ticks() as u64).max(now_unix);
        let mut z = job_id
            .wrapping_add(installer_id.wrapping_mul(0x517c_c1b7_2722_0a95))
            .wrapping_add(ticks);
        u64_to_unit_f64(splitmix64_next(&mut z))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix64_matches_reference_vector() {
        // Reference outputs of SplitMix64 seeded with 1234567 (Vigna's splitmix64.c test vector).
        let mut s = 1_234_567u64;
        assert_eq!(splitmix64_next(&mut s), 6_457_827_717_110_365_317);
        assert_eq!(splitmix64_next(&mut s), 3_203_168_211_198_807_973);
        assert_eq!(splitmix64_next(&mut s), 9_817_491_932_198_370_423);
    }

    #[test]
    fn seeded_streams_repeat_and_reseed() {
        let a = MechanicRng::seeded(42);
        let b = MechanicRng::seeded(42);
        let xs: Vec<u64> = (0..8).map(|_| a.try_next_u64().unwrap()).collect();
        let ys: Vec<u64> = (0..8).map(|_| b.try_next_u64().unwrap()).collect();
        assert_eq!(xs, ys);
        a.set_seed(Some(42));
        assert_eq!(a.try_next_u64(), Some(xs[0]));
        assert!(a.is_seeded());
        a.set_seed(None);
        assert!(!a.is_seeded());
        assert!(MechanicRng::os().try_next_u64().is_some());
    }

    #[test]
    fn roll_edges_consume_no_draw() {
        let r = MechanicRng::seeded(7);
        let reference = MechanicRng::seeded(7);
        assert!(!r.roll(0.0));
        assert!(!r.roll(f64::NAN));
        assert!(r.roll(1.0));
        assert_eq!(r.try_next_u64(), reference.try_next_u64());
        let u = MechanicRng::seeded(9).try_unit_f64().unwrap();
        assert!((0.0..1.0).contains(&u));
        assert_eq!(MechanicRng::seeded(9).roll(u + 1e-12), true);
        assert_eq!(MechanicRng::seeded(9).roll(u), false);
    }
}
