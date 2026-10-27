//! MN-05..MN-07 mining bonuses through SDE dogma: Mining Laser Upgrades, the Pioneer hull, the
//! Highwall implant, Type A/B/C crystal residue and Mining Foreman Burst dbuffs (fleet boosts from
//! `sde_dbuff_collections`).
//!
//! Sources: wiki:ORE_Basic_Ship_and_Skill_Overview (MLU +5/+9% yield and a CPU penalty on the
//! mining modules, "not subject to stacking penalties"), wiki:Pioneer (Mining Destroyer +10%/level
//! yield and +20%/level range, role +50% yield, 8,000 m3 mining hold), wiki:Mining_crystals (Type A/B/C
//! residue probability and multiplier columns), wiki:Command_Bursts (Mining Foreman Burst charges),
//! supervisor ruling 863a6ea (residue multiplier follows SDE effect 1200 ModAdd 3159 onto 3153).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::mining_cycle_engine::{activate, advance, attr, lock, pilot, rock, rpc, Pilot, CHAR_ID, JITA, MINER_I, SKILL_MINING, SKILL_MINING_FRIGATE, VELDSPAR, VENTURE};
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;

const RETRIEVER: u32 = 17_478;
const PIONEER: u32 = 89_240;
const BIFROST: u32 = 37_480;
const STRIP_MINER_I: u32 = 17_482;
const MOD_STRIP_II: u32 = 17_912;
const MLU_II: u32 = 28_576;
const HIGHWALL_MX1005: u32 = 22_535;
const BURST_II: u32 = 43_551;
const CHARGE_FIELD: u32 = 42_829;
const CHARGE_OPTIMIZATION: u32 = 42_830;
const CHARGE_EFFICIENCY: u32 = 90_733;
const BOOSTER_ID: i64 = 90_000_002;

/// Barge pilot skills: Mining V, Astrogeology V, Mining Barge V, Mining Upgrades IV, prerequisites,
/// Simple Ore Processing III (crystals) and Target Management V.
const BARGE_SKILLS: [(u32, u8); 10] =
    [(3_386, 5), (3_410, 5), (17_940, 5), (22_578, 4), (32_918, 5), (3_380, 5), (3_327, 5), (60_377, 3), (3_429, 5), (3_436, 5)];

fn insert_item(p: &Pilot, item_id: i64, type_id: u32, location: i64, flag: InventoryFlag, qty: i32) {
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id: CHAR_ID,
                location_id: location,
                flag,
                quantity: qty,
                singleton: qty == 1,
                version: 1,
                ..Default::default()
            },
        );
    });
}

fn module_attr(p: &Pilot, module: i64, attr_id: u16) -> f64 {
    let fit = p.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p.ship_id).unwrap();
    fit.get_module_effective_attribute(module, attr_id, &p.ctx.sde)
}

async fn deactivate(p: &mut Pilot, module: i64) {
    rpc(p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(module), PyRep::String("miningLaser".into())]).await.unwrap();
}

/// Undocks a second pilot in a Bifrost (command destroyer without mining-burst hull bonuses) with a
/// Mining Foreman Burst II, puts both pilots in one fleet and returns `(session, ship, burst module)`.
async fn booster(p: &Pilot) -> (SessionState, i64, i64) {
    let (b_ship, burst) = (9_400_000_001_i64, 9_400_000_002_i64);
    p.ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&CHAR_ID].clone();
        ch.character_id = BOOSTER_ID;
        ch.name = "Burst Booster".into();
        ch.station_id = Some(60_003_760);
        ch.structure_id = None;
        ch.solar_system_id = JITA;
        ch.active_ship_id = b_ship;
        snap.characters.insert(BOOSTER_ID, ch);
        snap.items.insert(
            b_ship,
            InventoryItemRecord {
                item_id: b_ship,
                type_id: BIFROST,
                owner_id: BOOSTER_ID,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Leadership V, Mining Foreman V (+10%/level buff duration), Mining Director V (+10%/level strength).
        let skills = [(3_348_u32, 5_u8), (22_536, 5), (22_552, 5)]
            .iter()
            .map(|&(id, level)| (id, TrainedSkillRecord { type_id: id, level, skill_points: 1_000_000.0 }))
            .collect();
        snap.skills.insert(BOOSTER_ID, skills);
        snap.fleets.insert(
            940_400,
            FleetRecord { fleet_id: 940_400, boss_id: BOOSTER_ID, members: vec![BOOSTER_ID, CHAR_ID], free_move: true, ..Default::default() },
        );
    });
    // The booster flies next to the miner. Its session is the miner's authenticated session with the
    // booster's character and ship (one account cannot hold two logged-in sessions).
    p.ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&BOOSTER_ID).unwrap();
        ch.station_id = None;
        let ship = snap.items.get_mut(&b_ship).unwrap();
        ship.location_id = i64::from(JITA);
        ship.flag = InventoryFlag::None;
    });
    let ball = eve_destiny::Ball {
        id: b_ship,
        type_id: BIFROST,
        position: p.ship_pos + DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    p.ctx.systems.get_or_spawn(JITA).spawn_ball(ball).await.unwrap();
    let mut sess = p.sess.clone();
    sess.character_id = Some(BOOSTER_ID);
    sess.ship_id = Some(b_ship);
    sess.station_id = None;
    sess.solar_system_id = Some(JITA);
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != b_ship);
        snap.items.insert(
            burst,
            InventoryItemRecord {
                item_id: burst,
                type_id: BURST_II,
                owner_id: BOOSTER_ID,
                location_id: b_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    p.ctx.invalidate_ship_fitting(b_ship);
    (sess, b_ship, burst)
}

/// Loads `charge` into the burst and activates it with `dogmaIM.Activate` (one burst pulse).
async fn pulse(p: &Pilot, sess: &mut SessionState, b_ship: i64, burst: i64, charge: u32) {
    let kw = HashMap::new();
    let _ = p.ctx.dispatch_rpc(sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst)], &kw, None).await;
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| !(i.location_id == b_ship && i.item_id != burst));
        snap.items.insert(
            9_400_000_003,
            InventoryItemRecord {
                item_id: 9_400_000_003,
                type_id: charge,
                owner_id: BOOSTER_ID,
                location_id: b_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_charges.remove(&burst);
    });
    p.ctx.invalidate_ship_fitting(b_ship);
    p.ctx
        .dispatch_rpc(sess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None)
        .await
        .expect("activate Mining Foreman Burst II");
}

/// Tops the miner's capacitor up (tests do not run the grid's passive recharge between cycles).
async fn refill(p: &Pilot) {
    let sys = p.ctx.systems.get_or_spawn(JITA);
    let max = sys.get_combat_state(p.ship_id).await.unwrap().unwrap().max_capacitor;
    sys.set_capacitor(p.ship_id, max).await.unwrap();
}

/// Seed whose first three draws satisfy `preds` (crit roll, residue roll, crystal volatility roll).
fn seed3(preds: [&dyn Fn(f64) -> bool; 3]) -> u64 {
    (1u64..)
        .find(|&s| {
            let mut st = s;
            preds.iter().all(|f| f(eve_server::rng::u64_to_unit_f64(eve_server::rng::splitmix64_next(&mut st))))
        })
        .unwrap()
}

#[tokio::test]
async fn test_mn07_mining_upgrades_stack_without_penalty_and_add_cpu_per_upgrade() {
    // parity: feat:mining.mining-upgrades
    eve_testkit::attest(&["feat:mining.mining-upgrades"]);
    let strip = 800_500_001;
    let mut p = pilot(RETRIEVER, &BARGE_SKILLS, &[(strip, STRIP_MINER_I, InventoryFlag::HiSlot0)]).await;
    let rock_id = 750_500_001;
    rock(&p, rock_id, VELDSPAR, 1_000_000, 4_000.0).await;
    lock(&mut p, rock_id).await;
    // SDE: MLU II miningAmountBonus 9%, cpuPenaltyPercent 12.5%; Mining Upgrades -5%/level of the
    // penalty (IV here: 12.5 x 0.80 = 10%). miningAmount (77) and cpu (50) are stackable=1.
    assert_eq!((attr(&p.ctx, MLU_II, 434), attr(&p.ctx, MLU_II, 1082), attr(&p.ctx, 22_578, 927)), (9.0, 12.5, -5.0));
    let base_m3 = 150.0 * 1.25 * 1.25 * 1.15 * 1.10; // 296.484375 (wiki:Retriever)
    let base_cpu = attr(&p.ctx, STRIP_MINER_I, 50);
    assert_eq!(base_cpu, 60.0);
    let mut expected_m3 = base_m3;
    let mut expected_cpu = base_cpu;
    for n in 0..=2_i64 {
        if n > 0 {
            insert_item(&p, 800_500_010 + n, MLU_II, p.ship_id, if n == 1 { InventoryFlag::LoSlot0 } else { InventoryFlag::LoSlot1 }, 1);
            p.ctx.invalidate_ship_fitting(p.ship_id);
            expected_m3 *= 1.09;
            expected_cpu *= 1.10;
        }
        let cpu = module_attr(&p, strip, 50);
        assert!((cpu - expected_cpu).abs() < 1e-9, "{n} MLU: strip miner cpu {cpu} vs {expected_cpu}");
        activate(&mut p, strip, rock_id).await.expect("Activate Strip Miner I");
        p.ctx.set_mechanic_rng_seed(Some(crate::mining_cycle_engine::seed_for(&[false], 0.01)));
        let out = advance(&p, 45).await;
        assert!((out[0].mining_amount_m3 - expected_m3).abs() < 1e-9, "{n} MLU: {} vs {expected_m3}", out[0].mining_amount_m3);
        assert_eq!(out[0].units_removed, (expected_m3 / 0.1 + 1e-9).floor() as i64);
        deactivate(&mut p, strip).await;
    }
    // Two MLU IIs: 296.484375 x 1.09 x 1.09 = 352.2531 m3 (no stacking penalty) and 60 x 1.1 x 1.1 = 72.6 TF.
    assert!((expected_m3 - 352.253_085_937_5).abs() < 1e-9);
    assert!((expected_cpu - 72.6).abs() < 1e-9);
}

#[tokio::test]
async fn test_mn07_pioneer_hull_bonuses_and_highwall_implant() {
    // parity: feat:mining.mining-destroyers, feat:mining.mining-skills, feat:mining.ore-ships
    eve_testkit::attest(&[
        "feat:mining.mining-destroyers",
        "feat:mining.mining-skills",
        "feat:mining.ore-ships",
    ]);
    let laser = 800_500_101;
    let skills = [
        (SKILL_MINING, 5),
        (89_241, 5), // Mining Destroyer
        (33_856, 5), // Expedition Frigates
        (16_281, 5), // Ice Harvesting
        (25_544, 5), // Gas Cloud Harvesting
        (17_940, 5), // Mining Barge
        (22_551, 5), // Exhumers
        (29_637, 5), // Industrial Command Ships
        (28_374, 5), // Capital Industrial Ships
        (22_578, 5), // Mining Upgrades
        (22_536, 5), // Mining Foreman
        (22_552, 5), // Mining Director
        (3_429, 5),
        (3_411, 5),
    ];
    let mut p = pilot(PIONEER, &skills, &[(laser, MINER_I, InventoryFlag::HiSlot0)]).await;
    // wiki:Pioneer: Mining Destroyer +10% yield and +20% range per level, role +50% yield, 8,000 m3 hold.
    assert_eq!((attr(&p.ctx, PIONEER, 5820), attr(&p.ctx, PIONEER, 5821), attr(&p.ctx, PIONEER, 5986)), (10.0, 20.0, 50.0));
    let fit = p.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p.ship_id).unwrap();
    assert_eq!(fit.ship.get_attribute(1556, &p.ctx.sde), 8_000.0);
    let rock_id = 750_500_101;
    rock(&p, rock_id, VELDSPAR, 1_000_000, 15_000.0).await;
    lock(&mut p, rock_id).await;
    // Range: Miner I 10 km x (1 + 5 x 20%) = 20 km, so a rock 15 km away is minable.
    assert_eq!(module_attr(&p, laser, 54), 20_000.0);
    let act = activate(&mut p, laser, rock_id).await.expect("Activate at 15 km");
    assert_eq!(act.dict_get("maxRange").and_then(|v| v.as_f64()), Some(20_000.0));
    p.ctx.set_mechanic_rng_seed(Some(crate::mining_cycle_engine::seed_for(&[false], 0.01)));
    let out = advance(&p, 15).await;
    // Yield: 10 m3 x Mining V 1.25 x Mining Destroyer V 1.5 x role 1.5 = 28.125 m3 = 281 units.
    assert_eq!((out[0].mining_amount_m3, out[0].units_removed), (28.125, 281));
    assert_eq!(out[0].destination_flag, InventoryFlag::SpecializedOreHold);
    deactivate(&mut p, laser).await;

    // Highwall 'MX-1005' (22535) and Michi's Excavation Augmentor (20700): +5% each to mining yield.
    assert_eq!((attr(&p.ctx, HIGHWALL_MX1005, 434), attr(&p.ctx, 20_700, 434)), (5.0, 5.0));
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.entry(CHAR_ID).or_default().extend([HIGHWALL_MX1005, 20_700]);
    });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    activate(&mut p, laser, rock_id).await.expect("Activate with implants");
    p.ctx.set_mechanic_rng_seed(Some(crate::mining_cycle_engine::seed_for(&[false], 0.01)));
    let out = advance(&p, 15).await;
    assert!((out[0].mining_amount_m3 - 28.125 * 1.05 * 1.05).abs() < 1e-9, "{}", out[0].mining_amount_m3);
    assert_eq!(out[0].units_removed, 310);
    deactivate(&mut p, laser).await;

    // Astrogeology V (3410): +5%/level = +25% yield on top of Mining V, hull, and implants.
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(CHAR_ID).or_default().insert(
            3_410,
            TrainedSkillRecord { type_id: 3_410, level: 5, skill_points: 256_000.0 },
        );
    });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    activate(&mut p, laser, rock_id).await.expect("Activate with Astrogeology V");
    p.ctx.set_mechanic_rng_seed(Some(crate::mining_cycle_engine::seed_for(&[false], 0.01)));
    let out = advance(&p, 15).await;
    assert!((out[0].mining_amount_m3 - 28.125 * 1.05 * 1.05 * 1.25).abs() < 1e-9, "{}", out[0].mining_amount_m3);
    assert_eq!(out[0].units_removed, 387);
    deactivate(&mut p, laser).await;
    p.ctx.db.with_mut_snapshot(|snap| snap.active_implants.remove(&CHAR_ID));

    // Perseverance (91174): 21,000 m3 hold, +100% role ice crit chance, +10%/lvl ice crit chance and
    // +5%/lvl ice crit bonus yield per Mining Destroyer, +20%/lvl ice range per Ice Harvesting.
    const PERSEVERANCE: u32 = 91_174;
    const ICE_LASER_II: u32 = 37_451;
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&p.ship_id).unwrap().type_id = PERSEVERANCE;
        snap.items.get_mut(&laser).unwrap().type_id = ICE_LASER_II;
    });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    let fit = p.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p.ship_id).unwrap();
    assert_eq!(fit.ship.get_attribute(1556, &p.ctx.sde), 21_000.0);
    assert_eq!(module_attr(&p, laser, 54), 16_000.0); // 8 km x (1 + 5 x 20%)
    assert!((module_attr(&p, laser, 5967) - 0.03).abs() < 1e-9); // 0.01 x 2.0 (role) x 1.5 (MD V)
    assert!((module_attr(&p, laser, 5969) - 2.5).abs() < 1e-9); // 2.0 x 1.25 (MD V)
    let ice_rock = 750_500_102;
    rock(&p, ice_rock, 16_262, 100, 14_000.0).await;
    lock(&mut p, ice_rock).await;
    refill(&p).await;
    activate(&mut p, laser, ice_rock).await.expect("Activate Ice Mining Laser II on Perseverance at 14 km");
    // Roll in [0.015, 0.025): crits at Perseverance's 3% chance, would miss at base 1%.
    let crit_seed = (1u64..)
        .find(|&s| {
            let mut st = s;
            let u = eve_server::rng::u64_to_unit_f64(eve_server::rng::splitmix64_next(&mut st));
            (0.015..0.025).contains(&u)
        })
        .unwrap();
    p.ctx.set_mechanic_rng_seed(Some(crit_seed));
    let dur_s = (module_attr(&p, laser, 73) / 1000.0).ceil() as u64;
    let out = advance(&p, dur_s).await;
    assert!(out[0].crit, "Perseverance 3% ice crit chance must trigger on 2% roll");
    assert_eq!(out[0].bonus_crit_units, (out[0].units_removed as f64 * 2.5).floor() as i64);
    deactivate(&mut p, laser).await;

    // ORE hull progression & mining hold capacities across Frigates, Barges, Exhumers, and Command Ships:
    for (hull_tid, expected_hold) in [
        (VENTURE, 5_000.0),
        (33_697, 12_500.0),     // Prospect
        (RETRIEVER, 34_375.0),   // 27,500 x (1 + 5 x 5%)
        (22_548, 44_296.875),   // Mackinaw: 31,500 x 1.25 x 1.125
        (22_544, 11_500.0),     // Hulk
        (42_244, 62_500.0),     // Porpoise: 50,000 x (1 + 5 x 5%)
        (28_606, 187_500.0),    // Orca: 150,000 x (1 + 5 x 5%)
        (28_352, 300_000.0),    // Rorqual
    ] {
        p.ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&p.ship_id).unwrap().type_id = hull_tid;
        });
        p.ctx.invalidate_ship_fitting(p.ship_id);
        let fit = p.ctx.get_or_rebuild_ship_fitting(CHAR_ID, p.ship_id).unwrap();
        assert!(
            (fit.ship.get_attribute(1556, &p.ctx.sde) - expected_hold).abs() < 1e-6,
            "hull {hull_tid} hold {} vs {expected_hold}",
            fit.ship.get_attribute(1556, &p.ctx.sde)
        );
    }
}

#[tokio::test]
async fn test_mn06_type_abc_crystal_residue_and_modifiers_compose_with_skills_and_bursts() {
    // parity: feat:mining.mining-residue, feat:mining.crystal-type-abc-modifiers
    eve_testkit::attest(&["feat:mining.mining-residue", "feat:mining.crystal-type-abc-modifiers"]);
    let strip = 800_500_201;
    let crystal = 800_500_202;
    let mut p = pilot(RETRIEVER, &BARGE_SKILLS, &[(strip, MOD_STRIP_II, InventoryFlag::HiSlot0)]).await;
    let rock_id = 750_500_201;
    rock(&p, rock_id, VELDSPAR, 10_000_000, 3_000.0).await;
    lock(&mut p, rock_id).await;
    // Modulated Strip Miner II: miningWasteProbability 34%, miningWastedVolumeMultiplier 1.0.
    assert_eq!((attr(&p.ctx, MOD_STRIP_II, 3154), attr(&p.ctx, MOD_STRIP_II, 3153)), (34.0, 1.0));
    let base_m3 = 120.0 * 1.25 * 1.25 * 1.15 * 1.10; // skills + Retriever bonuses
    // (crystal, yield x782, duration x3161, residue probability %, residue multiplier): probability
    // = 34 + 3160 and multiplier = 1.0 + 3159 (SDE effect 1200 ModAdd), e.g. Type C I 1.0 + 18 = 19.
    let table: [(u32, f64, f64, f64, f64); 6] = [
        (60_276, 1.5, 1.0, 34.0, 1.0),
        (60_281, 1.8, 1.0, 37.6, 1.0),
        (60_279, 1.5, 0.9, 54.0, 1.0),
        (60_283, 1.8, 0.8, 64.0, 1.0),
        (60_280, 0.25, 1.0, 74.0, 19.0),
        (60_284, 0.2, 1.0, 93.0, 29.0),
    ];
    for (ct, y, d, prob, mult) in table {
        p.ctx.db.with_mut_snapshot(|snap| snap.items.remove(&crystal));
        insert_item(&p, crystal, ct, p.ship_id, InventoryFlag::HiSlot0, 1);
        p.ctx.invalidate_ship_fitting(p.ship_id);
        assert!((module_attr(&p, strip, 3154) - prob).abs() < 1e-9, "type {ct} residue probability");
        assert!((module_attr(&p, strip, 3153) - mult).abs() < 1e-9, "type {ct} residue multiplier");
        refill(&p).await;
        let act = activate(&mut p, strip, rock_id).await.expect("Activate with crystal");
        assert_eq!(act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some((45_000.0 * d) as i64), "type {ct} duration");
        // No crit, residue roll just under the probability (hit), no volatility damage.
        let p_hit = prob / 100.0;
        p.ctx.set_mechanic_rng_seed(Some(seed3([&|u| u >= 0.01, &|u| u < p_hit && u > p_hit - 0.02, &|u| u >= 0.075])));
        let before = crate::mining_cycle_engine::rock_left(&p, rock_id);
        let out = advance(&p, (45.0 * d).ceil() as u64).await;
        let units = (base_m3 * y / 0.1 + 1e-9).floor() as i64;
        assert!((out[0].mining_amount_m3 - base_m3 * y).abs() < 1e-9, "type {ct} yield {}", out[0].mining_amount_m3);
        assert_eq!(out[0].units_removed, units, "type {ct}");
        assert_eq!(out[0].residue_units, (units as f64 * mult + 1e-9).floor() as i64, "type {ct} residue");
        assert_eq!(out[0].units_delivered, units, "residue is never delivered");
        assert_eq!(crate::mining_cycle_engine::rock_left(&p, rock_id), before - units - out[0].residue_units);
        deactivate(&mut p, strip).await;
    }

    // A roll above the probability wastes nothing (Type B I, 54%: a 60% roll misses).
    p.ctx.db.with_mut_snapshot(|snap| snap.items.remove(&crystal));
    insert_item(&p, crystal, 60_279, p.ship_id, InventoryFlag::HiSlot0, 1);
    p.ctx.invalidate_ship_fitting(p.ship_id);
    refill(&p).await;
    activate(&mut p, strip, rock_id).await.unwrap();
    p.ctx.set_mechanic_rng_seed(Some(seed3([&|u| u >= 0.01, &|u| (0.55..0.65).contains(&u), &|u| u >= 0.075])));
    let out = advance(&p, 41).await;
    assert_eq!(out[0].residue_units, 0);
    deactivate(&mut p, strip).await;

    // Bursts compose: a Mining Laser Efficiency Charge from a Mining Foreman Burst II with Mining
    // Director V lowers residue probability by 15% x 1.25 x 1.5 = 28.125%: Type C I 74% -> 53.1875%.
    p.ctx.db.with_mut_snapshot(|snap| snap.items.remove(&crystal));
    insert_item(&p, crystal, 60_280, p.ship_id, InventoryFlag::HiSlot0, 1);
    p.ctx.invalidate_ship_fitting(p.ship_id);
    let (mut bsess, b_ship, burst) = booster(&p).await;
    pulse(&p, &mut bsess, b_ship, burst, CHARGE_EFFICIENCY).await;
    refill(&p).await;
    activate(&mut p, strip, rock_id).await.unwrap();
    // A 60% roll would waste ore at 74% but not at 53.1875%.
    p.ctx.set_mechanic_rng_seed(Some(seed3([&|u| u >= 0.02, &|u| (0.55..0.65).contains(&u), &|u| u >= 0.075])));
    let out = advance(&p, 45).await;
    assert_eq!(out[0].residue_units, 0, "Efficiency burst lowers Type C residue probability");
    assert!((out[0].mining_amount_m3 - base_m3 * 0.25).abs() < 1e-9);
}

#[tokio::test]
async fn test_mn07_mining_foreman_burst_charges_apply_their_own_dbuffs_in_range() {
    // parity: feat:mining.foreman-burst-charge-effects, feat:mining.mining-fleet-boosts
    eve_testkit::attest(&[
        "feat:mining.foreman-burst-charge-effects",
        "feat:mining.mining-fleet-boosts",
    ]);
    let laser = 800_500_301;
    let scanner = 800_500_302;
    let skills = [(SKILL_MINING, 5), (SKILL_MINING_FRIGATE, 5), (3_429, 5), (3_402, 5)];
    let mut p = pilot(
        VENTURE,
        &skills,
        &[
            (laser, MINER_I, InventoryFlag::HiSlot0),
            (scanner, 444, InventoryFlag::MedSlot0), // Survey Scanner I (15 km base scanRange)
        ],
    )
    .await;
    let rock_id = 750_500_301;
    let far_scan_rock = 750_500_302;
    rock(&p, rock_id, VELDSPAR, 1_000_000, 2_000.0).await;
    rock(&p, far_scan_rock, VELDSPAR, 50_000, 22_000.0).await;
    lock(&mut p, rock_id).await;
    let (mut bsess, b_ship, burst) = booster(&p).await;
    // Burst II warfareBuff1Value 1.25 (T2) x Mining Director V (+50%); Optimization charge -15%.
    assert_eq!((attr(&p.ctx, BURST_II, 2469), attr(&p.ctx, CHARGE_OPTIMIZATION, 2596)), (1.25, -15.0));
    let strength = 1.25 * 1.5;

    // 1. Optimization: duration and capacitor need -28.125% (5.0 GJ -> 3.59375 GJ).
    let sys = p.ctx.systems.get_or_spawn(JITA);
    let max_cap = sys.get_combat_state(p.ship_id).await.unwrap().unwrap().max_capacitor;
    sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
    pulse(&p, &mut bsess, b_ship, burst, CHARGE_OPTIMIZATION).await;
    let act = activate(&mut p, laser, rock_id).await.expect("Activate under Optimization");
    let dur = (15_000.0 * (1.0 - 0.15 * strength)) as i64;
    assert_eq!(act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some(dur));
    assert_eq!(dur, 10_781);
    assert_eq!(act.dict_get("maxRange").and_then(|v| v.as_f64()), Some(10_000.0), "Optimization grants no range");
    let cap_after = sys.get_combat_state(p.ship_id).await.unwrap().unwrap().capacitor;
    assert!(((max_cap - cap_after) - 5.0 * (1.0 - 0.15 * strength)).abs() < 1e-6, "Optimization reduces cap need");
    deactivate(&mut p, laser).await;
    // Buff lasts buffDuration 60 s x Mining Foreman V (+50%) = 90 s.
    p.ctx.advance_virtual_clock(91);
    let act = activate(&mut p, laser, rock_id).await.unwrap();
    assert_eq!(act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some(15_000), "dbuff expired after 90 s");
    deactivate(&mut p, laser).await;

    // 2. Field Enhancement: +40% x strength = +75% mining and survey scanner range (15 km -> 26.25 km).
    let scan_before = rpc(&mut p, "miningScanMgr", None, "perform_scan", &[PyRep::Integer(scanner)]).await.unwrap();
    let PyRep::List(list_before) = scan_before.dict_get("asteroids").unwrap() else { panic!() };
    assert!(
        !list_before.iter().any(|e| e.dict_get("asteroidID").and_then(|v| v.as_i64()) == Some(far_scan_rock)),
        "22 km rock is outside 15 km base survey scanner range"
    );
    pulse(&p, &mut bsess, b_ship, burst, CHARGE_FIELD).await;
    let act = activate(&mut p, laser, rock_id).await.unwrap();
    assert_eq!(act.dict_get("maxRange").and_then(|v| v.as_f64()), Some(17_500.0));
    assert_eq!(act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some(15_000));
    deactivate(&mut p, laser).await;
    let scan_boosted = rpc(&mut p, "miningScanMgr", None, "perform_scan", &[PyRep::Integer(scanner)]).await.unwrap();
    let PyRep::List(list_boosted) = scan_boosted.dict_get("asteroids").unwrap() else { panic!() };
    assert!(
        list_boosted.iter().any(|e| e.dict_get("asteroidID").and_then(|v| v.as_i64()) == Some(far_scan_rock)),
        "Field Enhancement (+75%) extends survey scanner range to 26.25 km so 22 km rock is scanned"
    );

    // 3. Efficiency: crit chance +50% x strength = +93.75% (0.01 -> 0.019375), plus residue & crystal volatility reductions.
    p.ctx.advance_virtual_clock(91);
    pulse(&p, &mut bsess, b_ship, burst, CHARGE_EFFICIENCY).await;
    refill(&p).await;
    activate(&mut p, laser, rock_id).await.unwrap();
    let eff_crit_seed = (1u64..)
        .find(|&s| {
            let mut st = s;
            let u = eve_server::rng::u64_to_unit_f64(eve_server::rng::splitmix64_next(&mut st));
            (0.012..0.018).contains(&u)
        })
        .unwrap();
    p.ctx.set_mechanic_rng_seed(Some(eff_crit_seed));
    let out = advance(&p, 15).await;
    assert!(out[0].crit, "Efficiency burst raises crit chance from 1% to 1.9375%");
    deactivate(&mut p, laser).await;

    // 4. Out of burst range (Burst II maxRange 15 km): a miner 40 km away receives nothing.
    p.ctx.advance_virtual_clock(91);
    sys.set_ball_position(p.ship_id, p.ship_pos + DVec3::new(40_000.0, 0.0, 0.0)).await;
    pulse(&p, &mut bsess, b_ship, burst, CHARGE_OPTIMIZATION).await;
    sys.set_ball_position(p.ship_id, p.ship_pos).await;
    let act = activate(&mut p, laser, rock_id).await.unwrap();
    assert_eq!(act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some(15_000));
    deactivate(&mut p, laser).await;

    // 5. T1 vs T2 burst module & Mining Foreman Mindlink (+25%) scaling:
    // T1 burst (42528, warfareBuff1Value 1.0) vs T2 burst (43551, 1.25).
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&burst).unwrap().type_id = 42_528;
    });
    p.ctx.invalidate_ship_fitting(b_ship);
    pulse(&p, &mut bsess, b_ship, burst, CHARGE_OPTIMIZATION).await;
    let act = activate(&mut p, laser, rock_id).await.unwrap();
    assert_eq!(act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some((15_000.0 * (1.0 - 0.15 * 1.5)) as i64)); // 11,625 ms
    deactivate(&mut p, laser).await;

    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&burst).unwrap().type_id = BURST_II;
        snap.active_implants.entry(BOOSTER_ID).or_default().push(22_559);
    });
    p.ctx.invalidate_ship_fitting(b_ship);
    assert_eq!(attr(&p.ctx, 22_559, 884), 25.0);
    pulse(&p, &mut bsess, b_ship, burst, CHARGE_OPTIMIZATION).await;
    let act = activate(&mut p, laser, rock_id).await.unwrap();
    assert_eq!(act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some((15_000.0 * (1.0 - 0.15 * strength * 1.25)) as i64));
    assert_eq!(act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some(9_726));
    deactivate(&mut p, laser).await;

    // 6. Active Medium Industrial Core II (62591, attr 2587 = +7.5% command burst strength via effect 8119) on Porpoise (42244):
    let core_id = 9_400_000_004_i64;
    assert_eq!(attr(&p.ctx, 62_591, 2587), 7.5);
    let kw = HashMap::new();
    let _ = p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst)], &kw, None).await;
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&b_ship).unwrap().type_id = 42_244; // Porpoise
        let b_sk = snap.skills.entry(BOOSTER_ID).or_default();
        b_sk.insert(29_637, TrainedSkillRecord { type_id: 29_637, level: 5, skill_points: 1_000_000.0 });
        b_sk.insert(58_956, TrainedSkillRecord { type_id: 58_956, level: 4, skill_points: 500_000.0 });
        snap.items.insert(
            core_id,
            InventoryItemRecord {
                item_id: core_id,
                type_id: 62_591,
                owner_id: BOOSTER_ID,
                location_id: b_ship,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(core_id, true);
        snap.module_active_states.insert(core_id);
        snap.module_active_charges.remove(&burst);
    });
    p.ctx.invalidate_ship_fitting(b_ship);
    p.ctx
        .dispatch_rpc(&mut bsess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None)
        .await
        .expect("activate Mining Foreman Burst II with active Industrial Core II");
    let act = activate(&mut p, laser, rock_id).await.unwrap();
    // Porpoise (+2%/lvl Industrial Command Ships V = 1.10x) + active Medium Industrial Core II (1.075x):
    assert_eq!(
        act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()),
        Some((15_000.0 * (1.0 - 0.15 * strength * 1.25 * 1.10 * 1.075)) as i64)
    );
    assert_eq!(act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some(8_764));
    deactivate(&mut p, laser).await;
}

#[tokio::test]
async fn test_mn08_ice_harvester_cycle_duration_modifiers() {
    // parity: feat:mining.ice-harvester-cycle-modifiers
    eve_testkit::attest(&["feat:mining.ice-harvester-cycle-modifiers"]);
    const ICE_HARV_I: u32 = 16_278;
    const ICE_HARV_II: u32 = 22_229;
    const IHU_II: u32 = 28_578;
    const ICE_RIG_I: u32 = 32_819;
    const RETRIEVER: u32 = 17_478;
    const HULK: u32 = 22_544;
    const MACKINAW: u32 = 22_548;
    const CLEAR_ICICLE: u32 = 16_262;

    let harv = 800_500_401;
    let skills = [
        (SKILL_MINING, 5),
        (16_281, 5), // Ice Harvesting V (-5%/lvl = 0.75x)
        (17_940, 5), // Mining Barge V (-2%/lvl on Retriever = 0.90x, -3%/lvl on Hulk = 0.85x, -4%/lvl on Mackinaw = 0.80x)
        (22_551, 5), // Exhumers V (-4%/lvl on Hulk = 0.80x)
        (22_578, 4), // Mining Upgrades IV (-20% CPU penalty -> 10% per IHU II)
        (3_402, 5),
        (3_380, 5),
        (3_327, 5),
        (3_429, 5),
        (26_252, 5), // Jury Rigging
        (26_253, 5), // Astronautics Rigging
    ];
    let mut p = pilot(MACKINAW, &skills, &[(harv, ICE_HARV_I, InventoryFlag::HiSlot0)]).await;
    let ice_rock = 750_500_401;
    rock(&p, ice_rock, CLEAR_ICICLE, 500, 3_000.0).await;
    lock(&mut p, ice_rock).await;

    // Base Ice Harvester I duration 240,000 ms, Ice Harvester II 200,000 ms; both yield 1,000 m3 (1 ice block).
    assert_eq!((attr(&p.ctx, ICE_HARV_I, 73), attr(&p.ctx, ICE_HARV_I, 77)), (240_000.0, 1_000.0));
    assert_eq!((attr(&p.ctx, ICE_HARV_II, 73), attr(&p.ctx, ICE_HARV_II, 77)), (200_000.0, 1_000.0));

    let swap_ship = |p: &Pilot, ship_type_id: u32| {
        p.ctx.db.with_mut_snapshot(|snap| {
            if let Some(ship) = snap.items.get_mut(&p.ship_id) {
                ship.type_id = ship_type_id;
            }
        });
        p.ctx.invalidate_ship_fitting(p.ship_id);
    };
    let set_skill = |p: &Pilot, skill_id: u32, level: u8| {
        p.ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(CHAR_ID).or_default().insert(
                skill_id,
                TrainedSkillRecord { type_id: skill_id, level, skill_points: 256_000.0 },
            );
        });
        p.ctx.invalidate_ship_fitting(p.ship_id);
    };

    // 1. Mining Barge (Retriever 17478): Ice Harvesting V (0.75) x Mining Barge V (-2%/lvl = 0.90) x role (-12.5% = 0.875) = 141,750 ms.
    swap_ship(&p, RETRIEVER);
    assert!((module_attr(&p, harv, 73) - 240_000.0 * 0.75 * 0.90 * 0.875).abs() < 1e-6);

    // 2. Exhumer (Hulk 22544): Ice Harvesting V (0.75) x Mining Barge V (-3%/lvl = 0.85) x Exhumers V (-4%/lvl = 0.80) x role (-30% = 0.70) = 85,680 ms.
    swap_ship(&p, HULK);
    assert!((module_attr(&p, harv, 73) - 240_000.0 * 0.75 * 0.85 * 0.80 * 0.70).abs() < 1e-6);

    // 3. Exhumer (Mackinaw 22548): Ice Harvesting V (0.75) x Mining Barge V (-4%/lvl = 0.80) x role (-12.5% = 0.875) = 126,000 ms.
    swap_ship(&p, MACKINAW);
    let base_mack_dur = 240_000.0 * 0.75 * 0.80 * 0.875;
    assert!((module_attr(&p, harv, 73) - base_mack_dur).abs() < 1e-6);

    // 4. Ice Harvesting skill per-level check (level 3 = 0.85x vs level 5 = 0.75x):
    set_skill(&p, 16_281, 3);
    assert!((module_attr(&p, harv, 73) - 240_000.0 * 0.85 * 0.80 * 0.875).abs() < 1e-6);
    set_skill(&p, 16_281, 5);

    // 5. Fit two Ice Harvester Upgrade IIs (-9% duration each, +10% CPU each with Mining Upgrades IV) and
    // Medium Ice Harvester Accelerator I rig (-12% duration):
    insert_item(&p, 800_500_410, IHU_II, p.ship_id, InventoryFlag::LoSlot0, 1);
    insert_item(&p, 800_500_411, IHU_II, p.ship_id, InventoryFlag::LoSlot1, 1);
    insert_item(&p, 800_500_412, ICE_RIG_I, p.ship_id, InventoryFlag::RigSlot0, 1);
    p.ctx.invalidate_ship_fitting(p.ship_id);
    let expected_dur = base_mack_dur * 0.91 * 0.91 * 0.88;
    assert!((module_attr(&p, harv, 73) - expected_dur).abs() < 1e-6);
    assert!((module_attr(&p, harv, 50) - 60.0 * 1.10 * 1.10).abs() < 1e-6);

    // 6. Run a cycle and verify 1 whole ice unit (1,000 m3) is delivered to the mining hold.
    p.ctx.set_mechanic_rng_seed(Some(crate::mining_cycle_engine::seed_for(&[false], 0.01)));
    let act = activate(&mut p, harv, ice_rock).await.expect("Activate Ice Harvester I");
    assert_eq!(act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some(expected_dur as i64));
    let out = advance(&p, (expected_dur / 1000.0).ceil() as u64).await;
    assert_eq!((out[0].mining_amount_m3, out[0].units_removed, out[0].units_delivered), (1_000.0, 1, 1));
    assert_eq!(out[0].destination_flag, InventoryFlag::SpecializedOreHold);
    deactivate(&mut p, harv).await;
}

#[tokio::test]
async fn test_mn09_mining_drones_and_excavators() {
    // parity: feat:drones.mining-drones, feat:mining.mining-drones-yield, feat:mining.excavator-drones, rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID, rpc:entity.CmdMineRepeatedly, rpc:entity.CmdReturnBay, rpc:ship.LaunchDrones, rpc:ship.Undock
    eve_testkit::attest(&[
        "feat:drones.mining-drones",
        "feat:mining.mining-drones-yield",
        "feat:mining.excavator-drones",
    ]);
    let p = pilot(RETRIEVER, &BARGE_SKILLS, &[]).await;
    let kw = HashMap::new();
    let mut session = p.sess;
    let ship_id = p.ship_id;
    let jita = p.ctx.systems.get_or_spawn(JITA);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    let ast_id = 750_000_112i64;
    let ice_ast_id = 750_000_113i64;
    p.ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
        asteroid_id: ast_id,
        solar_system_id: JITA,
        belt_id: 40_000_001,
        type_id: 1230, // Veldspar (0.1 m3)
        quantity: 120_000,
        initial_quantity: 120_000,
        position: (ship_ball.position.x + 400.0, ship_ball.position.y, ship_ball.position.z),
        radius: 60.0,
    });
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: ast_id,
            belt_id: 40_000_001,
            type_id: 1230,
            initial_units: 120_000,
            remaining_units: 120_000,
            unit_volume: 0.1,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: ast_id,
            type_id: 1230,
            position: ship_ball.position + DVec3::new(400.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e10,
            radius: 60.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();
    p.ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
        asteroid_id: ice_ast_id,
        solar_system_id: JITA,
        belt_id: 40_000_001,
        type_id: 16262, // Clear Icicle (1000 m3, group 465)
        quantity: 200,
        initial_quantity: 200,
        position: (ship_ball.position.x + 450.0, ship_ball.position.y, ship_ball.position.z),
        radius: 100.0,
    });
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: ice_ast_id,
            belt_id: 40_000_001,
            type_id: 16262,
            initial_units: 200,
            remaining_units: 200,
            unit_volume: 1_000.0,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: ice_ast_id,
            type_id: 16262,
            position: ship_ball.position + DVec3::new(450.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e10,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();

    // 1. Mining Drone II (type 10250, group 101, base yield 33 m3 = 330 Veldspar units)
    let drone_id = 800_000_120i64;
    p.ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 17478; // Retriever (has SpecializedOreHold attr 1556)
        }
        snap.items.insert(
            drone_id,
            InventoryItemRecord {
                item_id: drone_id,
                type_id: 10250,
                owner_id: CHAR_ID,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let err_in_bay = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Mining drone in DroneBay cannot mine until launched");
    assert_eq!(err_in_bay.code, "DroneNotInSpace");

    p.ctx
        .dispatch_rpc(
            &mut session,
            "ship",
            None,
            "LaunchDrones",
            &[PyRep::List(vec![PyRep::Integer(drone_id)])],
            &kw,
            None,
        )
        .await
        .expect("ship.LaunchDrones");

    // 1a. Low skills: Mining Drone Operation I (3438 = 1 -> 1.05x), no Drone Interfacing (3436), no Specialization (22541), no rig
    // Expected yield: floor(33.0 * 1.05 / 0.1) = floor(346.5) = 346 units delivered to SpecializedOreHold (134)
    p.ctx.db.with_mut_snapshot(|snap| {
        if let Some(skills) = snap.skills.get_mut(&CHAR_ID) {
            skills.insert(3438, TrainedSkillRecord { type_id: 3438, level: 1, skill_points: 1000.0 });
            skills.remove(&3439);
            skills.remove(&3436);
            skills.remove(&22541);
        }
    });
    let _low_drone_res = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdMineRepeatedly low skills");
    let next_tick = eve_server::services::combat_crimewatch::with_combat_runtime(&p.ctx, |rt| {
        rt.active_drone_miners.values().map(|m| m.next_cycle_unix).max().unwrap()
    });
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&p.ctx, next_tick).await;
    let has_ore = p.ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|it| it.location_id == ship_id && it.flag == InventoryFlag::SpecializedOreHold && it.type_id == 1230 && it.quantity == 346)
    });
    assert!(has_ore, "Mining Drone II with Mining Drone Operation I must yield floor(33*1.05/0.1) = 346 Veldspar units");
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&p.ctx, |rt| {
        rt.active_drone_miners.clear();
    });

    // 1b. Switch ship hull to Vexor (626, cruiser with no SpecializedOreHold) -> ore delivered to Cargo (5)
    p.ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 626; // Vexor
        }
        snap.items.retain(|_, it| it.flag != InventoryFlag::SpecializedOreHold && it.flag != InventoryFlag::Cargo);
    });
    p.ctx.invalidate_ship_fitting(ship_id);
    let _cargo_drone_res = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdMineRepeatedly on Vexor");
    let next_tick_cargo = eve_server::services::combat_crimewatch::with_combat_runtime(&p.ctx, |rt| {
        rt.active_drone_miners.values().map(|m| m.next_cycle_unix).max().unwrap()
    });
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&p.ctx, next_tick_cargo).await;
    let has_cargo_ore = p.ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .any(|it| it.location_id == ship_id && it.flag == InventoryFlag::Cargo && it.type_id == 1230 && it.quantity == 346)
    });
    assert!(has_cargo_ore, "Vexor Cargo hold must contain the 346 Veldspar units mined by drone");
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&p.ctx, |rt| {
        rt.active_drone_miners.clear();
    });

    // 1c. High skills + Medium Drone Mining Augmentor I rig (25918, +10% attr 434):
    // 33.0 * 1.25 (3438 V) * 1.50 (3436 V) * 1.10 (22541 V) * 1.10 (25918 rig) = 74.86875 m3 -> floor(748.6875) = 748 units
    let rig_id = 800_000_122i64;
    p.ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 17478; // Retriever
        }
        let skills = snap.skills.entry(CHAR_ID).or_default();
        skills.insert(3438, TrainedSkillRecord { type_id: 3438, level: 5, skill_points: 500_000.0 });
        skills.insert(3436, TrainedSkillRecord { type_id: 3436, level: 5, skill_points: 500_000.0 });
        skills.insert(22541, TrainedSkillRecord { type_id: 22541, level: 5, skill_points: 500_000.0 });
        snap.items.insert(
            rig_id,
            InventoryItemRecord {
                item_id: rig_id,
                type_id: 25918,
                owner_id: CHAR_ID,
                location_id: ship_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    p.ctx.invalidate_ship_fitting(ship_id);

    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| it.flag != InventoryFlag::SpecializedOreHold && it.flag != InventoryFlag::Cargo);
    });
    let _drone_res = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdMineRepeatedly high skills + rig");
    let next_tick_high = eve_server::services::combat_crimewatch::with_combat_runtime(&p.ctx, |rt| {
        rt.active_drone_miners.values().map(|m| m.next_cycle_unix).max().unwrap()
    });
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&p.ctx, next_tick_high).await;
    let has_ore_748 = p.ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|it| it.location_id == ship_id && it.flag == InventoryFlag::SpecializedOreHold && it.type_id == 1230 && it.quantity == 748)
    });
    assert!(has_ore_748, "Mining Drone II with V/V/V skills + 10% rig must yield floor(74.86875 / 0.1) = 748 units");

    p.ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdReturnBay",
            &[PyRep::List(vec![PyRep::Integer(drone_id)])],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdReturnBay");

    // 2. Rorqual Excavator Drones (`feat:mining.excavator-drones`):
    // Both 'Excavator' Mining Drone (41030, ore) and 'Excavator' Ice Harvesting Drone (43681, ice)
    let exc_ore_drone_id = 800_000_121i64;
    let exc_ice_drone_id = 800_000_124i64;
    p.ctx.db.with_mut_snapshot(|snap| {
        for (did, dtid) in [(exc_ore_drone_id, 41030u32), (exc_ice_drone_id, 43681u32)] {
            snap.items.insert(
                did,
                InventoryItemRecord {
                    item_id: did,
                    type_id: dtid,
                    owner_id: CHAR_ID,
                    location_id: ship_id,
                    flag: InventoryFlag::DroneBay,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    p.ctx
        .dispatch_rpc(
            &mut session,
            "ship",
            None,
            "LaunchDrones",
            &[PyRep::List(vec![PyRep::Integer(exc_ore_drone_id), PyRep::Integer(exc_ice_drone_id)])],
            &kw,
            None,
        )
        .await
        .expect("Launch Excavator ore and ice drones");

    let err_non_rorq = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(exc_ore_drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Excavator must require a Rorqual hull");
    assert_eq!(err_non_rorq.code, "InvalidShipForExcavator");

    let ind_core_id = 800_000_123i64;
    p.ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 28352; // Rorqual
        }
        snap.skills.entry(CHAR_ID).or_default().insert(
            28374, // Capital Industrial Ships V (+10%/lvl = +50% ore drone yield)
            TrainedSkillRecord { type_id: 28374, level: 5, skill_points: 500_000.0 },
        );
        snap.items.retain(|_, item| item.location_id != ship_id || item.flag == InventoryFlag::DroneBay);
        snap.items.insert(
            ind_core_id,
            InventoryItemRecord {
                item_id: ind_core_id,
                type_id: 28583, // Capital Industrial Core I (+170% ore drone yield attr 2585, -75% ice drone cycle attr 2586)
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
    p.ctx.invalidate_ship_fitting(ship_id);

    let err_no_core = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(exc_ore_drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Excavator must require active Industrial Core on Rorqual");
    assert_eq!(err_no_core.code, "IndustrialCoreRequired");
    let err_ice_no_core = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(exc_ice_drone_id)]), PyRep::Integer(ice_ast_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Ice Excavator must also require active Industrial Core on Rorqual");
    assert_eq!(err_ice_no_core.code, "IndustrialCoreRequired");

    p.ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(ind_core_id), PyRep::String("industrialCore".into())],
            &kw,
            None,
        )
        .await
        .expect("Activate Capital Industrial Core I");

    // 'Excavator' Mining Drone (41030): base 80 m3 * 1.25 (3438 V) * 1.50 (3436 V) * 1.10 (22541 V) * 1.50 (Rorqual V) * 2.70 (Industrial Core I +170%) = 668.25 m3 = 6,682 Veldspar units
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| it.flag != InventoryFlag::SpecializedOreHold && it.flag != InventoryFlag::Cargo);
    });
    let _exc_ore_res = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(exc_ore_drone_id)]), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdMineRepeatedly with Excavator Mining Drone on active-core Rorqual");
    let next_tick_exc_ore = eve_server::services::combat_crimewatch::with_combat_runtime(&p.ctx, |rt| {
        rt.active_drone_miners.values().map(|m| m.next_cycle_unix).max().unwrap()
    });
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&p.ctx, next_tick_exc_ore).await;
    let has_exc_ore = p.ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|it| it.location_id == ship_id && it.flag == InventoryFlag::SpecializedOreHold && it.type_id == 1230 && it.quantity == 6_682)
    });
    assert!(has_exc_ore, "'Excavator' Mining Drone on active-core Rorqual V must yield floor(668.25 / 0.1) = 6,682 Veldspar units in SpecializedOreHold");

    // 'Excavator' Ice Harvesting Drone (43681): base 1000 m3 * 4.0 (Industrial Core I -75% ice drone cycle time) = 4,000 m3 = 4 Clear Icicle units
    let _exc_ice_res = p
        .ctx
        .dispatch_rpc(
            &mut session,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[PyRep::List(vec![PyRep::Integer(exc_ice_drone_id)]), PyRep::Integer(ice_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdMineRepeatedly with Excavator Ice Harvesting Drone on active-core Rorqual");
    let next_tick_exc_ice = eve_server::services::combat_crimewatch::with_combat_runtime(&p.ctx, |rt| {
        rt.active_drone_miners.values().map(|m| m.next_cycle_unix).max().unwrap()
    });
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&p.ctx, next_tick_exc_ice).await;
    let has_exc_ice = p.ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|it| it.location_id == ship_id && it.flag == InventoryFlag::SpecializedOreHold && it.type_id == 16262 && it.quantity == 4)
    });
    assert!(has_exc_ice, "'Excavator' Ice Harvesting Drone on active-core Rorqual must harvest 4 Clear Icicle units (4,000 m3)");
}
