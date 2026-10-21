//! Deterministic, seedable mechanic RNG (seeded test RNG): seeded contexts force both mining-crit outcomes
//! by value, and production constructors stay on the OS CSPRNG.

use crate::mining_cycle_engine::{activate, advance, attr, hold, lock, pilot, rock, rock_left, Pilot, MINER_I, SKILL_MINING, SKILL_MINING_FRIGATE, VELDSPAR, VENTURE};
use eve_db::InventoryFlag;
use eve_server::rng::MechanicRng;
use eve_server::router::ServerContext;
use std::sync::Arc;

const MODULE_ID: i64 = 100_000_951;

/// First seed in `1..` whose first SplitMix64 unit draw satisfies `pred`.
fn find_seed(pred: impl Fn(f64) -> bool) -> u64 {
    (1u64..100_000)
        .find(|&s| pred(MechanicRng::seeded(s).try_unit_f64().unwrap()))
        .expect("seed search")
}

/// Undocks the demo pilot in a Venture with one Miner I (MN-04 cycle engine).
async fn venture_with_miner() -> Pilot {
    pilot(VENTURE, &[(SKILL_MINING, 5), (SKILL_MINING_FRIGATE, 5)], &[(MODULE_ID, MINER_I, InventoryFlag::HiSlot0)]).await
}

#[tokio::test]
async fn test_c5_seeded_mining_crit_forces_both_outcomes_by_value() {
    // parity: feat:mining.mining-crits
    eve_testkit::attest(&["feat:mining.mining-crits"]);
    let mut p = venture_with_miner().await;
    let crit_chance = attr(&p.ctx, MINER_I, 5967);
    let crit_bonus = attr(&p.ctx, MINER_I, 5969);
    assert_eq!((crit_chance, crit_bonus), (0.01, 2.0));
    // A first draw below 0.001 must crit; above 0.999 must not.
    let crit_seed = find_seed(|u| u < 0.001);
    let normal_seed = find_seed(|u| u > 0.999);
    let ore = InventoryFlag::SpecializedOreHold;

    // Normal cycle: a 3-unit rock is fully extracted with no bonus units.
    rock(&p, 1_000_000_951, VELDSPAR, 3, 2_000.0).await;
    lock(&mut p, 1_000_000_951).await;
    activate(&mut p, MODULE_ID, 1_000_000_951).await.expect("Activate Miner I");
    p.ctx.set_mechanic_rng_seed(Some(normal_seed));
    let out = advance(&p, 15).await;
    assert_eq!(out.len(), 1);
    assert!(!out[0].crit);
    assert_eq!((out[0].units_removed, out[0].bonus_crit_units, out[0].units_delivered), (3, 0, 3));
    assert_eq!(hold(&p, ore, VELDSPAR), 3);
    assert_eq!(rock_left(&p, 1_000_000_951), 0);

    // Critical cycle: same 3 rock units plus 3 x miningCritBonusYield = 6 free bonus units.
    rock(&p, 1_000_000_952, VELDSPAR, 3, 2_000.0).await;
    lock(&mut p, 1_000_000_952).await;
    activate(&mut p, MODULE_ID, 1_000_000_952).await.expect("Activate Miner I");
    p.ctx.set_mechanic_rng_seed(Some(crit_seed));
    let out = advance(&p, 15).await;
    assert!(out[0].crit);
    assert_eq!((out[0].units_removed, out[0].bonus_crit_units, out[0].units_delivered), (3, 6, 9));
    assert_eq!(hold(&p, ore, VELDSPAR), 12);
    assert_eq!(rock_left(&p, 1_000_000_952), 0);
}

#[tokio::test]
async fn test_c5_same_seed_replays_identical_mining_sequence() {
    // Two contexts on the same seed replay the same 40-cycle outcome sequence, and every
    // non-crit cycle on the large rock yields the same normal unit count.
    async fn run(cycles: usize) -> Vec<(bool, i64)> {
        let mut p = venture_with_miner().await;
        rock(&p, 1_000_000_960, VELDSPAR, 1_000_000, 2_000.0).await;
        lock(&mut p, 1_000_000_960).await;
        p.ctx.set_mechanic_rng_seed(Some(0xC5));
        let mut out = Vec::new();
        for _ in 0..cycles {
            p.ctx.db.with_mut_snapshot(|snap| {
                snap.items.retain(|_, i| i.type_id != VELDSPAR);
            });
            if out.is_empty() {
                activate(&mut p, MODULE_ID, 1_000_000_960).await.expect("Activate Miner I");
            }
            let res = advance(&p, 15).await;
            assert_eq!(res.len(), 1);
            out.push((res[0].crit, res[0].units_delivered));
        }
        out
    }
    let a = run(40).await;
    let b = run(40).await;
    assert_eq!(a.len(), 40);
    assert_eq!(a, b);
    let normal = a.iter().find(|(c, _)| !c).map(|(_, y)| *y).expect("a normal cycle");
    assert_eq!(normal, 312);
    assert!(a.iter().filter(|(c, _)| !c).all(|(_, y)| *y == normal));
}

#[tokio::test]
async fn test_c5_production_constructors_stay_on_os_csprng() {
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let prod = ServerContext::with_repository(db);
    assert!(!prod.mechanic_rng.is_seeded(), "production context must use the OS CSPRNG");
    let first = prod.mechanic_rng.try_next_u64().expect("OS CSPRNG readable");
    let second = prod.mechanic_rng.try_next_u64().expect("OS CSPRNG readable");
    assert_ne!(first, second);

    let test_ctx = ServerContext::new();
    assert!(test_ctx.mechanic_rng.is_seeded(), "test constructor is seeded");
    let reference = MechanicRng::seeded(eve_server::rng::TEST_MODE_DEFAULT_SEED);
    assert_eq!(test_ctx.mechanic_rng.try_next_u64(), reference.try_next_u64());

    let seeded = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::new_empty()))
        .with_seeded_rng(77);
    assert_eq!(seeded.mechanic_rng.try_next_u64(), MechanicRng::seeded(77).try_next_u64());
    seeded.set_mechanic_rng_seed(None);
    assert!(!seeded.mechanic_rng.is_seeded());

    // Invention rolls share the stream; the explicit override still wins.
    let inv = ServerContext::new().with_seeded_rng(5);
    let expected = MechanicRng::seeded(5).try_unit_f64().unwrap();
    assert_eq!(inv.next_invention_roll(1, 2, 3), expected);
    inv.set_invention_roll_override(Some(0.25));
    assert_eq!(inv.next_invention_roll(1, 2, 3), 0.25);
}
