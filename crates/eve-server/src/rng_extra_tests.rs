//! Additional unit and mutation-killing tests for `rng`.

#![forbid(unsafe_code)]

use super::*;
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
fn splitmix64_and_u64_to_unit_f64_boundary_and_bit_precision() {
    assert_eq!(TEST_MODE_DEFAULT_SEED, 0x00C5_5EED_0000_0001);

    // State 0 advances by SPLITMIX64_GAMMA and mixes deterministically.
    let mut s0 = 0u64;
    let out0 = splitmix64_next(&mut s0);
    assert_eq!(s0, 0x9E37_79B9_7F4A_7C15);
    let mut manual = s0;
    manual = (manual ^ (manual >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    manual = (manual ^ (manual >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    manual ^= manual >> 31;
    assert_eq!(out0, manual);

    // Wrapping at u64::MAX
    let mut smax = u64::MAX;
    let out_max = splitmix64_next(&mut smax);
    assert_eq!(smax, u64::MAX.wrapping_add(0x9E37_79B9_7F4A_7C15));
    assert_ne!(out_max, 0);

    // u64_to_unit_f64 discards bottom 11 bits and scales by 2^-53.
    assert_eq!(u64_to_unit_f64(0), 0.0);
    assert_eq!(u64_to_unit_f64((1u64 << 11) - 1), 0.0);
    let ulp53 = 1.0 / ((1u64 << 53) as f64);
    assert_eq!(u64_to_unit_f64(1u64 << 11), ulp53);
    assert_eq!(u64_to_unit_f64((1u64 << 11) | 0x7FF), ulp53);
    assert_eq!(u64_to_unit_f64(1u64 << 63), 0.5);

    let max_unit = u64_to_unit_f64(u64::MAX);
    assert!(max_unit < 1.0);
    assert_eq!(max_unit, (((1u64 << 53) - 1) as f64) * ulp53);
}

#[test]
fn mechanic_rng_roll_boundaries_and_poisoned_mutex_recovery() {
    let rng = MechanicRng::seeded(314_159);
    let ref_rng = MechanicRng::seeded(314_159);

    // Non-positive and non-finite probabilities return false without advancing state.
    assert!(!rng.roll(0.0));
    assert!(!rng.roll(-0.0));
    assert!(!rng.roll(-1e-15));
    assert!(!rng.roll(-100.0));
    assert!(!rng.roll(f64::NEG_INFINITY));
    assert!(!rng.roll(f64::NAN));
    assert!(!rng.roll(f64::INFINITY));

    // Finite probabilities >= 1.0 return true without advancing state.
    assert!(rng.roll(1.0));
    assert!(rng.roll(1.000_000_1));
    assert!(rng.roll(f64::MAX));

    // Verify no draw was consumed by any boundary roll above.
    let expected_first = ref_rng.try_unit_f64().unwrap();
    let actual_first = rng.try_unit_f64().unwrap();
    assert_eq!(actual_first, expected_first);

    // Strict `< prob` comparison on interior probability in (0, 1).
    let r_eq = MechanicRng::seeded(271_828);
    let u = MechanicRng::seeded(271_828).try_unit_f64().unwrap();
    assert!(!r_eq.roll(u), "roll(u) must be false when draw == prob");

    let r_gt = MechanicRng::seeded(271_828);
    assert!(r_gt.roll(u + 1e-15), "roll(u + eps) must be true");

    let r_lt = MechanicRng::seeded(271_828);
    assert!(!r_lt.roll(u - 1e-15), "roll(u - eps) must be false");

    // OS mode works and produces valid unit floats.
    let os_rng = MechanicRng::os();
    assert!(!os_rng.is_seeded());
    let u_os = os_rng.try_unit_f64().expect("OS CSPRNG unit f64");
    assert!((0.0..1.0).contains(&u_os));

    // Poisoned mutex recovery via unwrap_or_else(|e| e.into_inner()).
    let poisoned = Arc::new(MechanicRng::seeded(77));
    let p_clone = Arc::clone(&poisoned);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _guard = p_clone.seeded_state.lock().unwrap();
        panic!("intentional poison");
    }));
    assert!(poisoned.is_seeded());
    assert!(poisoned.try_next_u64().is_some());
    poisoned.set_seed(None);
    assert!(!poisoned.is_seeded());
}

#[test]
fn server_context_rng_seed_and_invention_roll_overrides() {
    let ctx = test_ctx().with_seeded_rng(12_345);
    assert!(ctx.mechanic_rng.is_seeded());

    let expected_seq = MechanicRng::seeded(12_345);
    let u1 = expected_seq.try_unit_f64().unwrap();
    let u2 = expected_seq.try_unit_f64().unwrap();

    // Override takes precedence and clamps to [0.0, 0.999_999] without consuming mechanic_rng.
    ctx.set_invention_roll_override(Some(0.42));
    assert_eq!(ctx.next_invention_roll(100, 200, 300), 0.42);

    ctx.set_invention_roll_override(Some(-5.0));
    assert_eq!(ctx.next_invention_roll(100, 200, 300), 0.0);

    ctx.set_invention_roll_override(Some(0.0));
    assert_eq!(ctx.next_invention_roll(100, 200, 300), 0.0);

    ctx.set_invention_roll_override(Some(0.999_999));
    assert_eq!(ctx.next_invention_roll(100, 200, 300), 0.999_999);

    ctx.set_invention_roll_override(Some(1.0));
    assert_eq!(ctx.next_invention_roll(100, 200, 300), 0.999_999);

    ctx.set_invention_roll_override(Some(99.0));
    assert_eq!(ctx.next_invention_roll(100, 200, 300), 0.999_999);

    // Clearing override falls through to mechanic_rng stream (still at first draw u1).
    ctx.set_invention_roll_override(None);
    assert_eq!(ctx.next_invention_roll(100, 200, 300), u1);
    assert_eq!(ctx.next_invention_roll(100, 200, 300), u2);

    // Re-seeding via set_mechanic_rng_seed resets stream; None switches to OS mode.
    ctx.set_mechanic_rng_seed(Some(12_345));
    assert!(ctx.mechanic_rng.is_seeded());
    assert_eq!(ctx.next_invention_roll(1, 1, 1), u1);

    ctx.set_mechanic_rng_seed(None);
    assert!(!ctx.mechanic_rng.is_seeded());
    let os_roll = ctx.next_invention_roll(1, 1, 1);
    assert!((0.0..1.0).contains(&os_roll));
}
