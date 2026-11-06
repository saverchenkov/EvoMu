//! Deep branch-coverage and mutation-killing unit tests for `rng` (`crates/eve-server/src/rng.rs`).

#![forbid(unsafe_code)]

use crate::rng::*;
use crate::router::ServerContext;
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[test]
fn splitmix64_exact_avalanche_vectors_and_unit_f64_mantissa_bits() {
    // Verify exact SplitMix64 outputs across multiple distinct initial states
    let mut state = TEST_MODE_DEFAULT_SEED;
    let first = splitmix64_next(&mut state);
    assert_eq!(
        state,
        TEST_MODE_DEFAULT_SEED.wrapping_add(0x9E37_79B9_7F4A_7C15)
    );
    let second = splitmix64_next(&mut state);
    assert_ne!(first, second);

    // Verify each bit position in the top 53 bits of u64_to_unit_f64 contributes 2^-(64 - bit)
    for bit in 11..64 {
        let raw = 1u64 << bit;
        let expected = ((1u64 << (bit - 11)) as f64) / ((1u64 << 53) as f64);
        assert_eq!(u64_to_unit_f64(raw), expected, "bit {bit} mismatch");
    }
    // Low 11 bits (0..11) are completely masked out
    for low_bit in 0..11 {
        assert_eq!(u64_to_unit_f64(1u64 << low_bit), 0.0);
    }
}

#[test]
fn mechanic_rng_modes_bernoulli_rolls_and_context_invention_rolls() {
    // Default is unseeded OS mode
    let default_rng = MechanicRng::default();
    assert!(!default_rng.is_seeded());
    assert!(default_rng.try_next_u64().is_some());
    assert!(default_rng.try_unit_f64().is_some_and(|u| (0.0..1.0).contains(&u)));

    // Switching from OS -> seeded -> OS
    default_rng.set_seed(Some(999));
    assert!(default_rng.is_seeded());
    let draw_a = default_rng.try_next_u64().unwrap();
    default_rng.set_seed(Some(999));
    assert_eq!(default_rng.try_next_u64(), Some(draw_a));
    default_rng.set_seed(None);
    assert!(!default_rng.is_seeded());

    // Bernoulli roll statistical & exact deterministic sequence check
    let seeded = MechanicRng::seeded(2026);
    let mut mirror_state = 2026u64;
    for prob in [0.1, 0.25, 0.5, 0.75, 0.9] {
        let expected_u = u64_to_unit_f64(splitmix64_next(&mut mirror_state));
        assert_eq!(seeded.roll(prob), expected_u < prob);
    }

    // ServerContext invention roll override boundary clamping and RwLock poison recovery
    let ctx = test_ctx().with_seeded_rng(555_555);
    ctx.set_invention_roll_override(Some(0.123_456));
    assert!((ctx.next_invention_roll(1, 2, 3) - 0.123_456).abs() < 1e-12);

    // Poison invention_roll_override RwLock and verify both write and read recover cleanly
    let ctx_arc = Arc::new(ctx);
    let ctx_clone = Arc::clone(&ctx_arc);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _guard = ctx_clone.invention_roll_override.write().unwrap();
        panic!("poison invention_roll_override");
    }));
    assert!((ctx_arc.next_invention_roll(1, 2, 3) - 0.123_456).abs() < 1e-12);
    ctx_arc.set_invention_roll_override(None);
    let seeded_roll = ctx_arc.next_invention_roll(10, 20, 30);
    assert_eq!(
        seeded_roll,
        MechanicRng::seeded(555_555).try_unit_f64().unwrap()
    );
}
