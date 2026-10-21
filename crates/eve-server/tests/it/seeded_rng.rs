//! Deterministic, seedable mechanic RNG (seeded test RNG): seeded contexts force both mining-crit outcomes
//! by value, and production constructors stay on the OS CSPRNG.

use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::rng::MechanicRng;
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

const CHAR_ID: i64 = 90_000_001;
const MINER_I: u32 = 483;
const VELDSPAR: u32 = 1230;
const MODULE_ID: i64 = 100_000_951;

fn dict_i64(res: &PyRep, key: &str) -> i64 {
    dict_val(res, key).and_then(|v| v.as_i64()).unwrap_or_else(|| panic!("{key} missing"))
}

fn dict_val<'a>(res: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match res {
        PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| matches!(k, PyRep::String(s) if s == key))
            .map(|(_, v)| v),
        _ => None,
    }
}

/// First seed in `1..` whose first SplitMix64 unit draw satisfies `pred`.
fn find_seed(pred: impl Fn(f64) -> bool) -> u64 {
    (1u64..100_000)
        .find(|&s| pred(MechanicRng::seeded(s).try_unit_f64().unwrap()))
        .expect("seed search")
}

/// Logs in the demo pilot, trains mining skills, undocks, fits Miner I and returns
/// `(session, ship_id, solar_system_id, ship_position)`.
async fn undock_with_miner(ctx: &ServerContext) -> (SessionState, i64, u32, (f64, f64, f64)) {
    let kw = HashMap::new();
    let mut sess = SessionState::new(951);
    let login = [PyRep::String("demo".into()), PyRep::String("demo-secret".into())];
    ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &login, &kw, None)
        .await
        .expect("Login");
    let sel = [PyRep::Integer(CHAR_ID)];
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &sel, &kw, None)
        .await
        .expect("SelectCharacterID");
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(CHAR_ID).or_default();
        for skill_id in [3386u32, 3410] {
            let rec = TrainedSkillRecord { type_id: skill_id, level: 5, skill_points: 256_000.0 };
            skills.insert(skill_id, rec);
        }
    });
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let ship_id = sess.ship_id.expect("active ship");
    let sys_id = sess.solar_system_id.expect("solar system");
    let (_, ball) = ctx
        .systems
        .get_or_spawn(sys_id)
        .get_ball_snapshot(ship_id)
        .await
        .expect("ball snapshot")
        .expect("ship ball");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&100_000_005);
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
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    (sess, ship_id, sys_id, (ball.position.x + 1_000.0, ball.position.y, ball.position.z))
}

fn spawn_rock(ctx: &ServerContext, id: i64, sys_id: u32, qty: i64, pos: (f64, f64, f64)) {
    ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
        asteroid_id: id,
        solar_system_id: sys_id,
        belt_id: 1001,
        type_id: VELDSPAR,
        quantity: qty,
        initial_quantity: qty,
        position: pos,
        radius: 100.0,
    });
}

fn ore_in_ship(ctx: &ServerContext, ship_id: i64) -> i64 {
    ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == ship_id && i.type_id == VELDSPAR)
            .map(|i| i.quantity as i64)
            .sum()
    })
}

async fn activate(ctx: &ServerContext, sess: &mut SessionState, rock: i64) -> PyRep {
    let args = [PyRep::Integer(MODULE_ID), PyRep::Integer(rock)];
    ctx.dispatch_rpc(sess, "dogmaIM", None, "Activate", &args, &HashMap::new(), None)
        .await
        .expect("Activate Miner I")
}

#[tokio::test]
async fn test_c5_seeded_mining_crit_forces_both_outcomes_by_value() {
    // parity: feat:mining.mining-crits
    eve_testkit::attest(&["feat:mining.mining-crits"]);
    let ctx = ServerContext::new();
    let miner = ctx.sde.types.get(&MINER_I).expect("Miner I in SDE");
    let crit_chance = miner.default_attributes.get(&5967).copied().expect("Miner I miningCritChance (5967) must be in the SDE");
    let crit_bonus = miner.default_attributes.get(&5969).copied().expect("Miner I miningCritBonusYield (5969) must be in the SDE");
    assert!(crit_chance > 0.001 && crit_chance < 0.999, "SDE miningCritChance {crit_chance}");
    // A first draw below 0.001 must crit; above 0.999 must not (ship multipliers stay in range).
    let crit_seed = find_seed(|u| u < 0.001);
    let normal_seed = find_seed(|u| u > 0.999);

    let (mut sess, ship_id, sys_id, pos) = undock_with_miner(&ctx).await;

    // Normal cycle: 3-unit rock is fully extracted with no bonus units.
    spawn_rock(&ctx, 1_000_000_951, sys_id, 3, pos);
    let hold_before = ore_in_ship(&ctx, ship_id);
    ctx.set_mechanic_rng_seed(Some(normal_seed));
    let res = activate(&ctx, &mut sess, 1_000_000_951).await;
    assert_eq!(dict_val(&res, "isCrit"), Some(&PyRep::Bool(false)));
    assert_eq!(dict_i64(&res, "normalUnits"), 3);
    assert_eq!(dict_i64(&res, "bonusCritUnits"), 0);
    assert_eq!(dict_i64(&res, "yieldUnits"), 3);
    assert_eq!(ore_in_ship(&ctx, ship_id) - hold_before, 3);
    assert_eq!(ctx.db.get_asteroid(1_000_000_951).map(|a| a.quantity).unwrap_or(0), 0);

    // Critical cycle: same 3 rock units, plus round(3 * (bonus - 1)) free bonus units in the hold.
    let expected_bonus = ((3.0 * (crit_bonus - 1.0)).round() as i64).max(1);
    spawn_rock(&ctx, 1_000_000_952, sys_id, 3, pos);
    let hold_before = ore_in_ship(&ctx, ship_id);
    ctx.set_mechanic_rng_seed(Some(crit_seed));
    let res = activate(&ctx, &mut sess, 1_000_000_952).await;
    assert_eq!(dict_val(&res, "isCrit"), Some(&PyRep::Bool(true)));
    assert_eq!(dict_i64(&res, "normalUnits"), 3);
    assert_eq!(dict_i64(&res, "bonusCritUnits"), expected_bonus);
    assert_eq!(dict_i64(&res, "yieldUnits"), 3 + expected_bonus);
    assert_eq!(ore_in_ship(&ctx, ship_id) - hold_before, 3 + expected_bonus);
    assert_eq!(ctx.db.get_asteroid(1_000_000_952).map(|a| a.quantity).unwrap_or(0), 0);
}

#[tokio::test]
async fn test_c5_same_seed_replays_identical_mining_sequence() {
    // Two contexts on the same seed replay the same 40-cycle outcome sequence, and every
    // non-crit cycle on the large rock yields the same normal unit count.
    async fn run(ctx: &ServerContext, cycles: usize) -> Vec<(bool, i64)> {
        let (mut sess, _ship, sys_id, pos) = undock_with_miner(ctx).await;
        spawn_rock(ctx, 1_000_000_960, sys_id, 1_000_000, pos);
        ctx.set_mechanic_rng_seed(Some(0xC5));
        let mut out = Vec::new();
        for _ in 0..cycles {
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.retain(|_, i| i.type_id != VELDSPAR);
            });
            let res = activate(ctx, &mut sess, 1_000_000_960).await;
            let crit = dict_val(&res, "isCrit") == Some(&PyRep::Bool(true));
            out.push((crit, dict_i64(&res, "yieldUnits")));
        }
        out
    }
    let a = run(&ServerContext::new(), 40).await;
    let b = run(&ServerContext::new(), 40).await;
    assert_eq!(a.len(), 40);
    assert_eq!(a, b);
    let normal = a.iter().find(|(c, _)| !c).map(|(_, y)| *y).expect("a normal cycle");
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
