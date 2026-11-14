//! Deterministic, seedable mechanic RNG (seeded test RNG): seeded contexts force both mining-crit outcomes
//! by value, and production constructors stay on the OS CSPRNG.

use crate::common;
use crate::mining_cycle_engine::{activate, advance, attr, hold, lock, rock, rock_left, Pilot, CHAR_ID, JITA, MINER_I, SKILL_MINING, SKILL_MINING_FRIGATE, VELDSPAR, VENTURE};
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::rng::MechanicRng;
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

const MODULE_ID: i64 = 100_000_951;

fn fast_demo_ctx() -> ServerContext {
    static BASE: OnceLock<(Arc<eve_sde::SdeRegistry>, eve_db::WorldStateSnapshot)> = OnceLock::new();
    let (sde, snap) = BASE.get_or_init(|| {
        let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
        let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
        let _ = eve_server::world_seeder::seed_dual_anchor_markets(&db, &sde);
        (sde, db.with_snapshot(|s| s.clone()))
    });
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    db.with_mut_snapshot(|s| *s = snap.clone());
    ServerContext::with_sde_and_repository(Arc::clone(sde), db)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED)
}

/// First seed in `1..` whose first SplitMix64 unit draw satisfies `pred`.
fn find_seed(pred: impl Fn(f64) -> bool) -> u64 {
    (1u64..100_000)
        .find(|&s| pred(MechanicRng::seeded(s).try_unit_f64().expect("unit f64 draw")))
        .expect("seed search")
}

/// Undocks the demo pilot in a Venture with one Miner I (MN-04 cycle engine).
async fn venture_with_miner() -> Pilot {
    let ctx = fast_demo_ctx();
    common::pin_clock(&ctx);
    let mut sess = SessionState::new(1001);
    let kw = HashMap::new();
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    common::borrow_wire_harness(&ctx, &mut sess)
        .call_ok("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(CHAR_ID)], &kw)
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        let map = [(SKILL_MINING, 5), (SKILL_MINING_FRIGATE, 5)]
            .into_iter()
            .map(|(id, level)| (id, TrainedSkillRecord { type_id: id, level, skill_points: 1_000_000.0 }))
            .collect();
        snap.skills.insert(CHAR_ID, map);
    });
    let undock = common::borrow_wire_harness(&ctx, &mut sess)
        .call_ok("stationSvc", None, "Undock", &[], &kw)
        .await;
    let PyRep::KeyVal(kv) = &undock else { panic!("Undock returns KeyVal") };
    let ship_id = kv["shipID"].as_i64().expect("shipID");
    let beyonce_id = kv["beyonceBoundID"].as_i64().expect("beyonceBoundID") as u64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).expect("ship item").type_id = VENTURE;
        snap.items.retain(|_, i| i.location_id != ship_id);
        snap.items.insert(
            MODULE_ID,
            InventoryItemRecord {
                item_id: MODULE_ID,
                type_id: MINER_I,
                owner_id: CHAR_ID,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);
    let sys = ctx.systems.get_or_spawn(JITA);
    let (_, ball) = sys.get_ball_snapshot(ship_id).await.expect("get_ball_snapshot").expect("ship ball after undock");
    Pilot { ctx, sess, ship_id, beyonce_id, ship_pos: ball.position }
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
    // Two contexts on the same seed replay the same 8-cycle outcome sequence, and every
    // non-crit cycle on the large rock yields the same normal unit count.
    async fn run(cycles: usize) -> Vec<(bool, i64)> {
        let mut p = venture_with_miner().await;
        rock(&p, 1_000_000_960, VELDSPAR, 1_000_000, 2_000.0).await;
        lock(&mut p, 1_000_000_960).await;
        p.ctx.set_mechanic_rng_seed(Some(0xC5));
        activate(&mut p, MODULE_ID, 1_000_000_960).await.expect("Activate Miner I");
        advance(&p, 15 * cycles as u64)
            .await
            .into_iter()
            .map(|o| (o.crit, o.units_delivered))
            .collect()
    }
    let a = run(8).await;
    let b = run(8).await;
    assert_eq!(a.len(), 8);
    assert_eq!(a, b);
    let normal = a.iter().find(|(c, _)| !c).map(|(_, y)| *y).expect("a normal cycle");
    assert_eq!(normal, 312);
    assert!(a.iter().filter(|(c, _)| !c).all(|(_, y)| *y == normal));
}

#[tokio::test]
async fn test_c5_production_constructors_stay_on_os_csprng() {
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let prod = ServerContext::with_repository(db);
    common::pin_clock(&prod);
    assert!(!prod.mechanic_rng.is_seeded(), "production context must use the OS CSPRNG");
    let first = prod.mechanic_rng.try_next_u64().expect("OS CSPRNG readable");
    let second = prod.mechanic_rng.try_next_u64().expect("OS CSPRNG readable");
    assert_ne!(first, second);

    let test_ctx = ServerContext::new();
    common::pin_clock(&test_ctx);
    assert!(test_ctx.mechanic_rng.is_seeded(), "test constructor is seeded");
    let reference = MechanicRng::seeded(eve_server::rng::TEST_MODE_DEFAULT_SEED);
    assert_eq!(test_ctx.mechanic_rng.try_next_u64(), reference.try_next_u64());

    let seeded = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::new_empty()))
        .with_seeded_rng(77);
    common::pin_clock(&seeded);
    assert_eq!(seeded.mechanic_rng.try_next_u64(), MechanicRng::seeded(77).try_next_u64());
    seeded.set_mechanic_rng_seed(None);
    assert!(!seeded.mechanic_rng.is_seeded());

    // Invention rolls share the stream; the explicit override still wins.
    let inv = ServerContext::new().with_seeded_rng(5);
    common::pin_clock(&inv);
    let expected = MechanicRng::seeded(5).try_unit_f64().expect("unit f64 draw");
    assert_eq!(inv.next_invention_roll(1, 2, 3), expected);
    inv.set_invention_roll_override(Some(0.25));
    assert_eq!(inv.next_invention_roll(1, 2, 3), 0.25);
}
