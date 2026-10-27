//! MN-04 mining-module cycle engine through the V24 `dogmaIM.Activate` / `dogmaIM.Deactivate` calls.
//!
//! Every expected number is derived from SDE dogma attributes and cross-checked against the EVE
//! University wiki: wiki:Venture (Mining Frigate +5%/level ore yield, role bonus +100%),
//! wiki:Retriever (Mining Barge +3%/level strip-miner yield, role +10%, +5%/level ore hold),
//! wiki:Mining_yield (Mining and Astrogeology +5%/level; ore is delivered at the end of a cycle;
//! short-cycling delivers the elapsed share), wiki:ORE_Basic_Ship_and_Skill_Overview (Mining Laser
//! Upgrade II +9%, "not subject to stacking penalties") and wiki:Mining "Critical yield" (bonus
//! = 200% of the cycle's yield without reducing the asteroid).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use eve_db::{AsteroidRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, VirtualServerClock};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::mining_cycle::{process_mining_cycles, MiningCycleOutcome, MiningStopReason};
use eve_server::system_actor::AsteroidState;
use glam::DVec3;

pub(crate) const CHAR_ID: i64 = 90_000_001;
pub(crate) const JITA: u32 = 30_000_142;
pub(crate) const VENTURE: u32 = 32_880;
const RETRIEVER: u32 = 17_478;
pub(crate) const MINER_I: u32 = 483;
const STRIP_MINER_I: u32 = 17_482;
const ICE_HARVESTER_I: u32 = 16_278;
const DEEP_CORE_LASER_I: u32 = 12_108;
const MLU_II: u32 = 28_576;
pub(crate) const VELDSPAR: u32 = 1_230;
const MERCOXIT: u32 = 11_396;
pub(crate) const SKILL_MINING: u32 = 3_386;
const SKILL_ASTROGEOLOGY: u32 = 3_410;
pub(crate) const SKILL_MINING_FRIGATE: u32 = 32_918;
const SKILL_MINING_BARGE: u32 = 17_940;
const SKILL_DEEP_CORE: u32 = 11_395;
const SKILL_TARGET_MANAGEMENT: u32 = 3_429;

pub(crate) struct Pilot {
    pub(crate) ctx: ServerContext,
    pub(crate) sess: SessionState,
    pub(crate) ship_id: i64,
    pub(crate) beyonce_id: u64,
    pub(crate) ship_pos: DVec3,
}

pub(crate) fn attr(ctx: &ServerContext, type_id: u32, attr_id: u16) -> f64 {
    ctx.sde.types[&type_id].default_attributes[&attr_id]
}

pub(crate) async fn rpc(p: &mut Pilot, service: &str, bound: Option<u64>, method: &str, args: &[PyRep]) -> Result<PyRep, String> {
    let kw = HashMap::new();
    p.ctx.dispatch_rpc(&mut p.sess, service, bound, method, args, &kw, None).await.map_err(|e| e.code)
}

/// Logs in, undocks from Jita and refits the active ship as `hull` with `modules` (item, type, flag).
pub(crate) async fn pilot(hull: u32, skills: &[(u32, u8)], modules: &[(i64, u32, InventoryFlag)]) -> Pilot {
    let ctx = ServerContext::new();
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let mut sess = SessionState::new(1001);
    let kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None)
        .await
        .expect("Login");
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(CHAR_ID)], &kw, None)
        .await
        .expect("SelectCharacterID");
    ctx.db.with_mut_snapshot(|snap| {
        let map = skills
            .iter()
            .map(|&(id, level)| (id, TrainedSkillRecord { type_id: id, level, skill_points: 1_000_000.0 }))
            .collect();
        snap.skills.insert(CHAR_ID, map);
    });
    let undock = ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let PyRep::KeyVal(kv) = &undock else { panic!("Undock returns KeyVal") };
    let ship_id = kv["shipID"].as_i64().unwrap();
    let beyonce_id = kv["beyonceBoundID"].as_i64().unwrap() as u64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = hull;
        snap.items.retain(|_, i| i.location_id != ship_id);
        for &(item_id, type_id, flag) in modules {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: CHAR_ID,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let sys = ctx.systems.get_or_spawn(JITA);
    let (_, ball) = sys.get_ball_snapshot(ship_id).await.unwrap().expect("ship ball after undock");
    Pilot { ctx, sess, ship_id, beyonce_id, ship_pos: ball.position }
}

/// Spawns an asteroid both in the world state and as a ball on grid, `offset` metres from the ship.
pub(crate) async fn rock(p: &Pilot, id: i64, type_id: u32, quantity: i64, offset: f64) {
    let pos = p.ship_pos + DVec3::new(offset, 0.0, 0.0);
    p.ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: id,
        solar_system_id: JITA,
        belt_id: 40_000_001,
        type_id,
        quantity,
        initial_quantity: quantity,
        position: (pos.x, pos.y, pos.z),
        radius: 100.0,
    });
    let unit_volume = p.ctx.sde.types[&type_id].volume;
    let state = AsteroidState {
        asteroid_id: id,
        belt_id: 40_000_001,
        type_id,
        initial_units: quantity as u64,
        remaining_units: quantity as u64,
        unit_volume,
        depleted: false,
        respawn_at_tick: None,
    };
    let ball = Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 0.0,
        mass: 1.0e10,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    p.ctx.systems.get_or_spawn(JITA).spawn_asteroid(state, ball).await.unwrap();
}

/// `dogmaIM.AddTarget`, then steps the grid until the lock completes.
pub(crate) async fn lock(p: &mut Pilot, target: i64) {
    rpc(p, "dogmaIM", None, "AddTarget", &[PyRep::Integer(target)]).await.expect("AddTarget");
    let sys = p.ctx.systems.get_or_spawn(JITA);
    for _ in 0..30 {
        let locks = sys.get_target_locks(p.ship_id).await.unwrap();
        if locks.iter().any(|l| l.target_id == target && l.locked) {
            return;
        }
        sys.step_manual(1.0).await.unwrap();
    }
    panic!("target {target} never locked");
}

pub(crate) async fn activate(p: &mut Pilot, module: i64, target: i64) -> Result<PyRep, String> {
    rpc(p, "dogmaIM", None, "Activate", &[PyRep::Integer(module), PyRep::String("miningLaser".into()), PyRep::Integer(target)]).await
}

pub(crate) async fn advance(p: &Pilot, secs: u64) -> Vec<MiningCycleOutcome> {
    p.ctx.advance_virtual_clock(secs);
    process_mining_cycles(&p.ctx).await
}

pub(crate) fn hold(p: &Pilot, flag: InventoryFlag, type_id: u32) -> i64 {
    p.ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|i| i.location_id == p.ship_id && i.flag == flag && i.type_id == type_id)
            .map(|i| i64::from(i.quantity))
            .sum()
    })
}

pub(crate) fn rock_left(p: &Pilot, id: i64) -> i64 {
    p.ctx.db.get_asteroid(id).map(|a| a.quantity).unwrap()
}

pub(crate) fn running(p: &Pilot, module: i64) -> bool {
    p.ctx.mining_cycles.read().unwrap().contains_key(&module)
}

/// Runs exactly one full mining cycle for an already-logged-in, undocked session: makes sure the
/// target asteroid has a ball on grid, locks it (`dogmaIM.AddTarget`), activates the module
/// (`dogmaIM.Activate`), advances the server clock past one cycle, resolves it and deactivates the
/// module again. Returns the activation error code, or the resolved cycle.
pub(crate) async fn mine_once(
    ctx: &ServerContext,
    sess: &mut SessionState,
    module_id: i64,
    target_id: i64,
) -> Result<MiningCycleOutcome, String> {
    let kw = HashMap::new();
    let sys_id = sess.solar_system_id.expect("in space");
    let ship_id = sess.ship_id.expect("active ship");
    let sys = ctx.systems.get_or_spawn(sys_id);
    if let Some(a) = ctx.db.get_asteroid(target_id) {
        if sys.get_ball_snapshot(target_id).await.unwrap().is_none() {
            let state = AsteroidState {
                asteroid_id: target_id,
                belt_id: a.belt_id,
                type_id: a.type_id,
                initial_units: a.initial_quantity.max(0) as u64,
                remaining_units: a.quantity.max(0) as u64,
                unit_volume: ctx.sde.types.get(&a.type_id).map(|t| t.volume).unwrap_or(0.1),
                depleted: false,
                respawn_at_tick: None,
            };
            let ball = Ball {
                id: target_id,
                type_id: a.type_id,
                position: DVec3::new(a.position.0, a.position.1, a.position.2),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e10,
                radius: a.radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            };
            sys.spawn_asteroid(state, ball).await.unwrap();
        }
    }
    let locked = |locks: &[eve_destiny::TargetLock]| locks.iter().any(|l| l.target_id == target_id && l.locked);
    if !locked(&sys.get_target_locks(ship_id).await.unwrap()) {
        ctx.dispatch_rpc(sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(target_id)], &kw, None)
            .await
            .map_err(|e| e.code)?;
        for _ in 0..30 {
            if locked(&sys.get_target_locks(ship_id).await.unwrap()) {
                break;
            }
            sys.step_manual(1.0).await.unwrap();
        }
    }
    let args = [PyRep::Integer(module_id), PyRep::String("miningLaser".into()), PyRep::Integer(target_id)];
    let act = ctx.dispatch_rpc(sess, "dogmaIM", None, "Activate", &args, &kw, None).await.map_err(|e| e.code)?;
    let cycle_ms = act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()).expect("cycleTimeMs") as u64;
    ctx.advance_virtual_clock(cycle_ms.div_ceil(1000));
    let mut out = process_mining_cycles(ctx).await;
    if !out.iter().any(|o| o.module_id == module_id) && ctx.mining_cycles.read().unwrap().contains_key(&module_id) {
        // Wall-anchored clocks carry sub-second jitter; one more second completes the cycle.
        ctx.advance_virtual_clock(1);
        out = process_mining_cycles(ctx).await;
    }
    if ctx.mining_cycles.read().unwrap().contains_key(&module_id) {
        // Drop the lock first so the stop does not deliver a short-cycle share.
        ctx.dispatch_rpc(sess, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(target_id)], &kw, None)
            .await
            .map_err(|e| e.code)?;
        let deact = [PyRep::Integer(module_id), PyRep::String("miningLaser".into())];
        ctx.dispatch_rpc(sess, "dogmaIM", None, "Deactivate", &deact, &kw, None).await.map_err(|e| e.code)?;
    }
    out.into_iter().find(|o| o.module_id == module_id).ok_or_else(|| "NoCycleResolved".to_string())
}

/// A seed whose consecutive SplitMix64 draws produce exactly `crits` for a `chance` roll each.
pub(crate) fn seed_for(crits: &[bool], chance: f64) -> u64 {
    (1u64..).find(|&seed| {
        let mut state = seed;
        crits.iter().all(|&c| {
            let u = eve_server::rng::u64_to_unit_f64(eve_server::rng::splitmix64_next(&mut state));
            (u < chance) == c
        })
    })
    .unwrap()
}

#[tokio::test]
async fn test_mn04_venture_miner_i_cycles_deliver_exact_sde_yield_until_hold_full_or_depleted() {
    // parity: feat:mining.mining-laser-cycle, feat:mining.mining-crits, plan:MN-03, plan:MN-04
    eve_testkit::attest(&["feat:mining.mining-laser-cycle", "feat:mining.mining-crits", "plan:MN-03", "plan:MN-04"]);
    let laser = 800_400_001;
    let skills = [(SKILL_MINING, 5), (SKILL_MINING_FRIGATE, 5)];
    let mut p = pilot(VENTURE, &skills, &[(laser, MINER_I, InventoryFlag::HiSlot0)]).await;

    // Yield per cycle from SDE: Miner I miningAmount x (1 + 5 x Mining bonus) x (1 + 5 x Venture
    // Mining Frigate bonus) x Venture role multiplier = 10 x 1.25 x 1.25 x 2 = 31.25 m3 (wiki:Venture).
    let expected_m3 = attr(&p.ctx, MINER_I, 77)
        * (1.0 + 5.0 * attr(&p.ctx, SKILL_MINING, 434) / 100.0)
        * (1.0 + 5.0 * attr(&p.ctx, VENTURE, 1842) / 100.0)
        * attr(&p.ctx, VENTURE, 207);
    assert_eq!(expected_m3, 31.25);
    let veldspar_m3 = p.ctx.sde.types[&VELDSPAR].volume;
    assert_eq!(veldspar_m3, 0.1);
    let per_cycle = 312; // floor(31.25 / 0.1)
    let ore_hold = InventoryFlag::SpecializedOreHold;

    // 1) Three full cycles: ore arrives only at each cycle end, into the general mining hold.
    let rock_a = 750_400_001;
    rock(&p, rock_a, VELDSPAR, 50_000, 3_000.0).await;
    lock(&mut p, rock_a).await;
    p.ctx.drain_notifications_for_char(CHAR_ID);
    let act = activate(&mut p, laser, rock_a).await.expect("Activate Miner I");
    assert_eq!(act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some(15_000));
    assert!(running(&p, laser));
    assert!(advance(&p, 14).await.is_empty(), "no ore before the 15 s cycle completes");
    assert_eq!(hold(&p, ore_hold, VELDSPAR), 0);
    p.ctx.set_mechanic_rng_seed(Some(seed_for(&[false, false, false], 0.01)));
    let out = advance(&p, 31).await; // t = 45 s: cycles end at 15, 30 and 45 s
    assert_eq!(out.len(), 3);
    for o in &out {
        assert_eq!((o.mining_amount_m3, o.units_removed, o.units_delivered, o.crit), (31.25, per_cycle, per_cycle, false));
        assert_eq!(o.destination_flag, ore_hold);
        assert_eq!(o.stop, None);
    }
    assert_eq!(hold(&p, ore_hold, VELDSPAR), 3 * per_cycle);
    assert_eq!(rock_left(&p, rock_a), 50_000 - 3 * per_cycle);
    let mined_notes: Vec<_> = p.ctx.drain_notifications_for_char(CHAR_ID).into_iter().filter(|n| n.event == "OnOreMined").collect();
    assert_eq!(mined_notes.len(), 3);
    assert_eq!(
        mined_notes[0].args,
        vec![
            PyRep::Integer(i64::from(VELDSPAR)),
            PyRep::Integer(per_cycle),
            PyRep::Integer(per_cycle),
            PyRep::Bool(false),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(rock_a),
            PyRep::Integer(laser),
        ]
    );

    // 2) Critical success (seeded mechanic RNG, miningCritChance 0.01): the hold gains
    //    miningCritBonusYield (2.0) x the cycle's yield on top, the asteroid loses only the yield.
    assert_eq!((attr(&p.ctx, MINER_I, 5967), attr(&p.ctx, MINER_I, 5969)), (0.01, 2.0));
    p.ctx.set_mechanic_rng_seed(Some(seed_for(&[true], 0.01)));
    let crit = advance(&p, 15).await;
    assert_eq!(crit.len(), 1);
    assert!(crit[0].crit);
    assert_eq!((crit[0].units_removed, crit[0].bonus_crit_units, crit[0].units_delivered), (per_cycle, 624, 936));
    assert_eq!(hold(&p, ore_hold, VELDSPAR), 3 * per_cycle + 936);
    assert_eq!(rock_left(&p, rock_a), 50_000 - 4 * per_cycle);

    // 3) Short cycle: deactivating 6 s into a cycle delivers 6/15 of 31.25 m3 = 125 units.
    p.ctx.advance_virtual_clock(6);
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(laser), PyRep::String("miningLaser".into())])
        .await
        .expect("Deactivate");
    assert!(!running(&p, laser));
    let after_short = 3 * per_cycle + 936 + 125;
    assert_eq!(hold(&p, ore_hold, VELDSPAR), after_short);
    assert_eq!(rock_left(&p, rock_a), 50_000 - 4 * per_cycle - 125);

    // 4) Full hold: Venture generalMiningHoldCapacity 5000 m3. Leave room for 400 units: one full
    //    cycle (312) then a 88-unit cycle that fills the hold and deactivates the module.
    assert_eq!(attr(&p.ctx, VENTURE, 1556), 5_000.0);
    p.ctx.db.with_mut_snapshot(|snap| {
        let stack = snap.items.values_mut().find(|i| i.location_id == p.ship_id && i.flag == ore_hold).unwrap();
        stack.quantity = 49_600;
    });
    let before = rock_left(&p, rock_a);
    activate(&mut p, laser, rock_a).await.expect("Activate again");
    p.ctx.set_mechanic_rng_seed(Some(seed_for(&[false, false], 0.01)));
    let full = advance(&p, 30).await;
    assert_eq!(full.iter().map(|o| o.units_delivered).collect::<Vec<_>>(), vec![per_cycle, 88]);
    assert_eq!(full[1].stop, Some(MiningStopReason::HoldFull));
    assert_eq!(hold(&p, ore_hold, VELDSPAR), 50_000);
    assert_eq!(rock_left(&p, rock_a), before - 400);
    assert!(!running(&p, laser), "module deactivates when the hold is full");
    assert!(advance(&p, 15).await.is_empty());

    // 5) Depletion: a 500-unit rock gives a full cycle then a partial 188-unit final cycle, after
    //    which the asteroid ball is removed and the module stops.
    p.ctx.db.with_mut_snapshot(|snap| snap.items.retain(|_, i| !(i.location_id == p.ship_id && i.flag == ore_hold)));
    let rock_b = 750_400_002;
    rock(&p, rock_b, VELDSPAR, 500, 2_500.0).await;
    lock(&mut p, rock_b).await;
    activate(&mut p, laser, rock_b).await.expect("Activate on small rock");
    p.ctx.set_mechanic_rng_seed(Some(seed_for(&[false, false], 0.01)));
    let dep = advance(&p, 30).await;
    assert_eq!(dep.iter().map(|o| o.units_removed).collect::<Vec<_>>(), vec![per_cycle, 188]);
    assert_eq!(dep[1].stop, Some(MiningStopReason::AsteroidDepleted));
    assert_eq!((rock_left(&p, rock_b), hold(&p, ore_hold, VELDSPAR)), (0, 500));
    assert!(p.ctx.systems.get_or_spawn(JITA).get_ball_snapshot(rock_b).await.unwrap().is_none());
    assert!(!running(&p, laser));
}

#[tokio::test]
async fn test_mn04_activation_requirements_and_cycle_interruptions() {
    // parity: plan:MN-04
    eve_testkit::attest(&["plan:MN-04"]);
    let (laser, ice, deep) = (800_400_011, 800_400_012, 800_400_013);
    let skills = [(SKILL_MINING, 5), (SKILL_MINING_FRIGATE, 5), (SKILL_DEEP_CORE, 1), (SKILL_ASTROGEOLOGY, 5), (SKILL_TARGET_MANAGEMENT, 5)];
    let mods = [(laser, MINER_I, InventoryFlag::HiSlot0), (ice, ICE_HARVESTER_I, InventoryFlag::HiSlot1), (deep, DEEP_CORE_LASER_I, InventoryFlag::HiSlot2)];
    let mut p = pilot(VENTURE, &skills, &mods).await;
    let (near, far, merc) = (750_400_011, 750_400_012, 750_400_013);
    rock(&p, near, VELDSPAR, 50_000, 3_000.0).await;
    rock(&p, far, VELDSPAR, 50_000, 12_500.0).await;
    rock(&p, merc, MERCOXIT, 1_000, 3_500.0).await;

    // Target must be locked.
    assert_eq!(activate(&mut p, laser, near).await.unwrap_err(), "TargetNotLocked");
    lock(&mut p, near).await;
    lock(&mut p, far).await;
    lock(&mut p, merc).await;
    // Range: Miner I maxRange 10 000 m against the surface distance (12 500 m centre offset).
    assert_eq!(attr(&p.ctx, MINER_I, 54), 10_000.0);
    assert_eq!(activate(&mut p, laser, far).await.unwrap_err(), "TargetOutOfRange");
    // Minable type: the asteroid's requiredSkill1 must be one of the module's required skills.
    assert_eq!(activate(&mut p, ice, near).await.unwrap_err(), "TargetNotMinable");
    assert_eq!(activate(&mut p, laser, merc).await.unwrap_err(), "TargetNotMinable");
    assert!(activate(&mut p, deep, merc).await.is_ok(), "Deep Core Mining Laser mines Mercoxit");
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(deep), PyRep::String("miningLaser".into())]).await.unwrap();
    // Capacitor: Miner I capacitorNeed 5 GJ.
    let sys = p.ctx.systems.get_or_spawn(JITA);
    let full_cap = sys.get_combat_state(p.ship_id).await.unwrap().expect("combat state").capacitor;
    sys.set_capacitor(p.ship_id, 4.0).await.unwrap();
    assert_eq!(activate(&mut p, laser, near).await.unwrap_err(), "NotEnoughCapacitor");
    sys.set_capacitor(p.ship_id, full_cap).await.unwrap();
    // Cloaked ships cannot mine.
    let mut cs = sys.get_combat_state(p.ship_id).await.unwrap().unwrap();
    cs.is_cloaked = true;
    sys.set_combat_state(p.ship_id, cs.clone()).await.unwrap();
    assert_eq!(activate(&mut p, laser, near).await.unwrap_err(), "CannotActivateWhileCloaked");
    cs.is_cloaked = false;
    sys.set_combat_state(p.ship_id, cs).await.unwrap();
    // Online module required.
    p.ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(laser, false);
    });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    assert_eq!(activate(&mut p, laser, near).await.unwrap_err(), "ModuleOffline");
    p.ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(laser, true);
    });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    // Activation pays capacitorNeed for the first cycle.
    activate(&mut p, laser, near).await.expect("valid activation");
    let cap_after = sys.get_combat_state(p.ship_id).await.unwrap().unwrap().capacitor;
    assert!((full_cap - cap_after - 5.0).abs() < 1e-6, "cap {full_cap} -> {cap_after}");

    // Interruptions stop the cycle without delivering ore.
    let ore = |p: &Pilot| hold(p, InventoryFlag::SpecializedOreHold, VELDSPAR);
    // a) lost lock
    rpc(&mut p, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(near)]).await.unwrap();
    assert!(advance(&p, 15).await.is_empty());
    assert!(!running(&p, laser));
    assert_eq!(ore(&p), 0);
    // b) out of range (ship moved 20 km away mid-cycle)
    lock(&mut p, near).await;
    activate(&mut p, laser, near).await.unwrap();
    sys.set_ball_position(p.ship_id, p.ship_pos - DVec3::new(20_000.0, 0.0, 0.0)).await;
    assert!(advance(&p, 15).await.is_empty());
    assert!(!running(&p, laser));
    sys.set_ball_position(p.ship_id, p.ship_pos).await;
    // c) docked
    activate(&mut p, laser, near).await.unwrap();
    p.ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&CHAR_ID).unwrap().station_id = Some(60_003_760));
    assert!(advance(&p, 15).await.is_empty());
    assert!(!running(&p, laser));
    p.ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&CHAR_ID).unwrap().station_id = None);
    // d) warping (beyonce.CmdWarpToStuff to a Jita stargate)
    activate(&mut p, laser, near).await.unwrap();
    let bid = p.beyonce_id;
    rpc(&mut p, "beyonce", Some(bid), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(50_001_249)])
        .await
        .expect("CmdWarpToStuff");
    assert!(advance(&p, 15).await.is_empty());
    assert!(!running(&p, laser));
    assert_eq!(ore(&p), 0);
    assert_eq!(rock_left(&p, near), 50_000);
}

#[tokio::test]
async fn test_mn04_retriever_strip_miner_yield_matches_wiki_with_barge_and_upgrade_bonuses() {
    // parity: feat:mining.strip-miner, plan:MN-07
    eve_testkit::attest(&["feat:mining.strip-miner", "plan:MN-07"]);
    let (strip, mlu_a, mlu_b) = (800_400_021, 800_400_022, 800_400_023);
    let skills = [(SKILL_MINING, 5), (SKILL_ASTROGEOLOGY, 5), (SKILL_MINING_BARGE, 5), (SKILL_MINING_FRIGATE, 5), (22_578, 4), (3_380, 5), (3_327, 5)];
    let mods = [(strip, STRIP_MINER_I, InventoryFlag::HiSlot0), (mlu_a, MLU_II, InventoryFlag::LoSlot0), (mlu_b, MLU_II, InventoryFlag::LoSlot1)];
    let mut p = pilot(RETRIEVER, &skills, &mods).await;
    let rock_id = 750_400_021;
    rock(&p, rock_id, VELDSPAR, 1_000_000, 4_000.0).await;
    lock(&mut p, rock_id).await;

    // wiki:Retriever: Strip Miner I 150 m3 x Mining V 1.25 x Astrogeology V 1.25 x Mining Barge V
    // (3%/level) 1.15 x role 1.10 x two MLU II (+9% each, no stacking penalty) = 352.2531 m3.
    let published = 150.0 * 1.25 * 1.25 * 1.15 * 1.10 * 1.09 * 1.09;
    assert_eq!(attr(&p.ctx, STRIP_MINER_I, 77), 150.0);
    assert_eq!((attr(&p.ctx, RETRIEVER, 3181), attr(&p.ctx, RETRIEVER, 3177), attr(&p.ctx, MLU_II, 434)), (3.0, 10.0, 9.0));
    let act = activate(&mut p, strip, rock_id).await.expect("Activate Strip Miner I");
    assert_eq!(act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some(45_000));
    p.ctx.set_mechanic_rng_seed(Some(seed_for(&[false], 0.01)));
    assert!(advance(&p, 44).await.is_empty(), "strip miner cycle is 45 s");
    let out = advance(&p, 1).await;
    assert_eq!(out.len(), 1);
    assert!((out[0].mining_amount_m3 - published).abs() / published < 0.001, "{} vs {published}", out[0].mining_amount_m3);
    assert_eq!(out[0].units_delivered, 3_522); // floor(352.2531 / 0.1)
    // Retriever generalMiningHoldCapacity 27 500 m3 + 5%/level Mining Barge = 34 375 m3 (wiki).
    let fit = p.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p.ship_id).unwrap();
    assert_eq!(fit.ship.get_attribute(1556, &p.ctx.sde), 34_375.0);

    // Without the upgrades the same skills give 296.484375 m3 (2 964 units).
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(strip), PyRep::String("miningLaser".into())]).await.unwrap();
    p.ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&mlu_a);
        s.items.remove(&mlu_b);
    });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    activate(&mut p, strip, rock_id).await.unwrap();
    p.ctx.set_mechanic_rng_seed(Some(seed_for(&[false], 0.01)));
    let bare = advance(&p, 45).await;
    assert_eq!((bare[0].mining_amount_m3, bare[0].units_delivered), (296.484375, 2_964));
}

#[tokio::test]
async fn test_mn04_deactivate_requires_the_cycle_owner() {
    // Review R-MN04-1: another pilot's dogmaIM.Deactivate must not stop (or short-cycle) this cycle.
    let laser = 800_400_031;
    let mut p = pilot(VENTURE, &[(SKILL_MINING, 5), (SKILL_MINING_FRIGATE, 5)], &[(laser, MINER_I, InventoryFlag::HiSlot0)]).await;
    let rock_id = 750_400_031;
    rock(&p, rock_id, VELDSPAR, 50_000, 3_000.0).await;
    lock(&mut p, rock_id).await;
    activate(&mut p, laser, rock_id).await.expect("Activate Miner I");

    // Second pilot in space with their own ship, on the same authenticated connection.
    let (other_char, other_ship) = (90_000_002_i64, 9_400_100_001_i64);
    p.ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&CHAR_ID].clone();
        ch.character_id = other_char;
        ch.active_ship_id = other_ship;
        snap.characters.insert(other_char, ch);
        snap.items.insert(
            other_ship,
            InventoryItemRecord {
                item_id: other_ship,
                type_id: VENTURE,
                owner_id: other_char,
                location_id: i64::from(JITA),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut other = p.sess.clone();
    other.character_id = Some(other_char);
    other.ship_id = Some(other_ship);
    p.ctx.advance_virtual_clock(6);
    let kw = HashMap::new();
    let deact = [PyRep::Integer(laser), PyRep::String("miningLaser".into())];
    let err = p.ctx.dispatch_rpc(&mut other, "dogmaIM", None, "Deactivate", &deact, &kw, None).await.unwrap_err();
    assert_eq!(err.code, "ModuleNotFound");
    assert!(running(&p, laser), "the owner's cycle keeps running");
    assert_eq!(hold(&p, InventoryFlag::SpecializedOreHold, VELDSPAR), 0, "no short-cycle delivery for the intruder");
    p.ctx.set_mechanic_rng_seed(Some(seed_for(&[false], 0.01)));
    let out = advance(&p, 9).await; // t = 15 s
    assert_eq!(out.len(), 1);
    assert_eq!((out[0].module_id, out[0].units_delivered), (laser, 312));
    assert_eq!(rock_left(&p, rock_id), 50_000 - 312);
}
