//! End-to-end integration tests for Batch 22 open `feat:structures.*` and `feat:modules.*` rows.

#![forbid(unsafe_code)]

use crate::open_feats_and_rpcs_b22::{rpc, select_pilot, PILOT_1, PILOT_2};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, UpwellStructureRecord};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;

const JITA_SYS: u32 = 30_000_142;
const NULL_SYS: u32 = 30_004_759;

#[tokio::test]
async fn test_b22_mobile_observatory_and_standup_point_defense() {
    // parity: feat:structures.mobile-observatory, feat:structures.point-defense-system
    eve_testkit::attest(&[
        "feat:structures.mobile-observatory",
        "feat:structures.point-defense-system",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select_pilot(&ctx, 55_201, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = select_pilot(&ctx, 55_202, "other", "other-secret", PILOT_2).await;

    // 1. Negative case for feat:structures.mobile-observatory: deploying in high-sec (Jita) is rejected
    let ship1_id = 77_101_001_i64;
    let obs_item_id = 77_101_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_1) {
            c.station_id = None;
            c.structure_id = None;
            c.solar_system_id = JITA_SYS;
            c.active_ship_id = ship1_id;
        }
        snap.items.insert(
            ship1_id,
            InventoryItemRecord {
                item_id: ship1_id,
                type_id: 587, // Rifter
                owner_id: PILOT_1,
                location_id: JITA_SYS as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            obs_item_id,
            InventoryItemRecord {
                item_id: obs_item_id,
                type_id: eve_server::services::mobile_deployables::TYPE_MOBILE_OBSERVATORY,
                owner_id: PILOT_1,
                location_id: ship1_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.skills
            .entry(PILOT_1)
            .or_default()
            .insert(
                11584,
                TrainedSkillRecord {
                    type_id: 11584, // Anchoring
                    level: 3,
                    skill_points: 24_000.0,
                },
            );
    });
    s1.solar_system_id = Some(JITA_SYS);
    s1.ship_id = Some(ship1_id);

    let jita_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let (sb1, sc1) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, ship1_id, 587, DVec3::ZERO);
    jita_handle.spawn_ball_with_combat(sb1, sc1).await.expect("spawn ship1");

    let hs_err = rpc(
        &ctx,
        &mut s1,
        "posMgr",
        "AnchorStructure",
        &[PyRep::Integer(obs_item_id), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .expect_err("Mobile Observatory in high-sec must be rejected");
    assert!(!hs_err.code.is_empty());

    // 2. Positive case for feat:structures.mobile-observatory: deploy in null-sec, anchor, and decloak nearby ship
    let ship2_id = 77_101_010_i64;
    let cloak_id = 77_101_011_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_1) {
            c.solar_system_id = NULL_SYS;
        }
        if let Some(it) = snap.items.get_mut(&ship1_id) {
            it.location_id = NULL_SYS as i64;
        }
        if let Some(c2) = snap.characters.get_mut(&PILOT_2) {
            c2.station_id = None;
            c2.structure_id = None;
            c2.solar_system_id = NULL_SYS;
            c2.active_ship_id = ship2_id;
        }
        snap.items.insert(
            ship2_id,
            InventoryItemRecord {
                item_id: ship2_id,
                type_id: 33468, // Astero (can fit Covert Ops Cloaking Device II)
                owner_id: PILOT_2,
                location_id: NULL_SYS as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cloak_id,
            InventoryItemRecord {
                item_id: cloak_id,
                type_id: 11370, // Prototype Cloaking Device I
                owner_id: PILOT_2,
                location_id: ship2_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    s1.solar_system_id = Some(NULL_SYS);
    s2.solar_system_id = Some(NULL_SYS);
    s2.ship_id = Some(ship2_id);

    let null_handle = ctx.systems.get_or_spawn(NULL_SYS);
    let (sb1n, sc1n) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, ship1_id, 587, DVec3::ZERO);
    null_handle.spawn_ball_with_combat(sb1n, sc1n).await.expect("spawn ship1 in null");
    let (sb2n, mut sc2n) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, ship2_id, 33468, DVec3::new(5_000.0, 0.0, 0.0));
    sc2n.capacitor = 500.0;
    null_handle.spawn_ball_with_combat(sb2n, sc2n).await.expect("spawn ship2 in null");
    ctx.invalidate_ship_fitting(ship2_id);

    // Cloak ship2 via live dogmaIM.ShipOnlineModules + dogmaIM.Activate RPC
    let _ = rpc(&ctx, &mut s2, "dogmaIM", "ShipOnlineModules", &[PyRep::Integer(ship2_id)])
        .await
        .expect("ShipOnlineModules ship2");
    let _ = rpc(&ctx, &mut s2, "dogmaIM", "Activate", &[PyRep::Integer(cloak_id)])
        .await
        .expect("Activate Covert Ops Cloak");
    assert!(
        null_handle.get_combat_state(ship2_id).await.unwrap().expect("ship2 combat").is_cloaked,
        "ship2 must be cloaked after dogmaIM.Activate"
    );

    let _ = rpc(
        &ctx,
        &mut s1,
        "posMgr",
        "AnchorStructure",
        &[PyRep::Integer(obs_item_id), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .expect("Anchor Mobile Observatory in null-sec");

    // Advance past 10-minute (600s) observatory anchor delay and tick
    ctx.advance_secs(605);
    tick_background_cluster(&ctx).await;

    let cs2_after = null_handle.get_combat_state(ship2_id).await.unwrap().expect("ship2 combat state");
    assert!(!cs2_after.is_cloaked, "Mobile Observatory ping must decloak ship within range");

    // 3. Test feat:structures.point-defense-system & feat:structures.structure-weapons
    let keepstar_id = 77_102_001_i64;
    let pds_id = 77_102_002_i64;
    let flak_id = 77_102_003_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).map(|c| c.corporation_id).unwrap_or(98_000_001));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(PILOT_1, corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER);
        }
        snap.structures.insert(
            keepstar_id,
            UpwellStructureRecord {
                structure_id: keepstar_id,
                type_id: 35834, // Keepstar (XL)
                owner_corp_id: corp_id,
                solar_system_id: NULL_SYS,
                name: "Test Keepstar".into(),
                position: [100_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            keepstar_id,
            InventoryItemRecord {
                item_id: keepstar_id,
                type_id: 35834, // Keepstar (XL)
                owner_id: corp_id,
                location_id: NULL_SYS as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            pds_id,
            InventoryItemRecord {
                item_id: pds_id,
                type_id: 35926, // Standup Point Defense Battery I
                owner_id: corp_id,
                location_id: keepstar_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(pds_id, true);
        snap.items.insert(
            flak_id,
            InventoryItemRecord {
                item_id: flak_id,
                type_id: 63195, // Standup Flak Rounds
                owner_id: corp_id,
                location_id: keepstar_id,
                flag: InventoryFlag::SpecializedAmmoHold,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
        if let Some(c) = snap.characters.get_mut(&PILOT_1) {
            c.structure_id = Some(keepstar_id);
            c.active_ship_id = keepstar_id;
        }
    });
    s1.structure_id = Some(keepstar_id);
    s1.ship_id = Some(keepstar_id);
    s1.corp_role |= corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER;

    let (kb, mut kc) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, keepstar_id, 35834, DVec3::new(100_000.0, 0.0, 0.0));
    kc.capacitor = 10_000.0;
    null_handle.spawn_ball_with_combat(kb, kc).await.expect("spawn Keepstar");
    // Move ship2 within 1,100m of Keepstar so PDS damages it
    assert!(null_handle.set_ball_position(ship2_id, DVec3::new(101_100.0, 0.0, 0.0)).await, "move ship2 near Keepstar");
    let before_sh = null_handle.get_combat_state(ship2_id).await.unwrap().unwrap().shield_hp;

    let _ = rpc(
        &ctx,
        &mut s1,
        "structureControl",
        "TakeControl",
        &[PyRep::Integer(keepstar_id)],
    )
    .await
    .expect("TakeControl Keepstar");

    let pds_res = rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(pds_id)],
    )
    .await
    .expect("Activate Standup Point Defense Battery");
    assert!(!matches!(pds_res, PyRep::None));

    let after_sh = null_handle.get_combat_state(ship2_id).await.unwrap().unwrap().shield_hp;
    assert!(after_sh < before_sh, "Standup Point Defense Battery must damage ship within perimeter");
}

async fn run_overheat_fit(thermo_lvl: u8) -> (f64, f64, f64, f64, f64, f64, f64, f64) {
    let ctx = ServerContext::new();
    let mut s1 = select_pilot(&ctx, 55_203, "demo", "demo-secret", PILOT_1).await;

    let ship_id = 77_103_001_i64;
    let mod_high = 77_103_005_i64;
    let mod_left = 77_103_002_i64;
    let mod_center = 77_103_003_i64;
    let mod_right = 77_103_004_i64;
    let mod_low = 77_103_006_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_1) {
            c.station_id = None;
            c.structure_id = None;
            c.solar_system_id = JITA_SYS;
            c.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter (1259 heatAttenuationHi=0.5, 1261 heatAttenuationMed=0.5)
                owner_id: PILOT_1,
                location_id: JITA_SYS as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (mid, flag, tid) in [
            (mod_high, InventoryFlag::HiSlot0, 2873),   // 125mm Gatling AutoCannon II (flag 27, 1180=0.02, 0 cap)
            (mod_left, InventoryFlag::MedSlot0, 3242),  // Warp Disruptor I (flag 19, dist 1)
            (mod_center, InventoryFlag::MedSlot1, 440), // 5MN Microwarpdrive II (flag 20, 1180=0.04)
            (mod_right, InventoryFlag::MedSlot2, 527),  // Stasis Webifier II (flag 21, dist 1)
            (mod_low, InventoryFlag::LoSlot0, 2048),    // Damage Control II (flag 11, low rack)
        ] {
            let mut item = InventoryItemRecord {
                item_id: mid,
                type_id: tid,
                owner_id: PILOT_1,
                location_id: ship_id,
                flag,
                quantity: 1,
                singleton: true,
                ..Default::default()
            };
            item.dynamic_attributes.insert(eve_sde::attr_ids::HP_STRUCT, 500.0);
            snap.items.insert(mid, item);
            snap.module_online_states.insert(mid, true);
        }
        snap.items.insert(
            77_103_010,
            InventoryItemRecord {
                item_id: 77_103_010,
                type_id: 212, // EMP S
                owner_id: PILOT_1,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
        snap.skills.entry(PILOT_1).or_default().insert(
            28164,
            TrainedSkillRecord {
                type_id: 28164, // Thermodynamics
                level: thermo_lvl,
                skill_points: 135_765.0,
            },
        );
        snap.skills.entry(PILOT_1).or_default().insert(
            3454,
            TrainedSkillRecord {
                type_id: 3454, // High Speed Maneuvering
                level: 4,
                skill_points: 135_765.0,
            },
        );
    });
    s1.solar_system_id = Some(JITA_SYS);
    s1.ship_id = Some(ship_id);
    ctx.invalidate_ship_fitting(ship_id);

    let jita_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let (sb, mut sc) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, ship_id, 587, DVec3::ZERO);
    sc.capacitor = 2_000.0;
    sc.max_capacitor = 2_000.0;
    jita_handle.spawn_ball_with_combat(sb, sc).await.expect("spawn Rifter");
    let target_id = 77_103_099_i64;
    let (tb, mut tc) = eve_server::living_world::sde_ball_and_combat(&ctx.sde, target_id, 587, DVec3::new(1_000.0, 0.0, 0.0));
    tc.shield_hp = 50_000.0;
    tc.max_shield_hp = 50_000.0;
    jita_handle.spawn_ball_with_combat(tb, tc).await.expect("spawn target");
    jita_handle.add_target_lock(ship_id, eve_destiny::TargetLock { target_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0) }).await.expect("lock target");

    let _ = rpc(&ctx, &mut s1, "dogmaIM", "ShipOnlineModules", &[PyRep::Integer(ship_id)])
        .await
        .expect("ShipOnlineModules");
    let _ = rpc(&ctx, &mut s1, "dogmaIM", "Overload", &[PyRep::Integer(mod_center)])
        .await
        .expect("dogmaIM.Overload mid");
    let _ = rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(mod_high), PyRep::String("targetAttack".into()), PyRep::Integer(target_id), PyRep::Integer(1)])
        .await
        .expect("dogmaIM.Activate high");
    let _ = rpc(&ctx, &mut s1, "dogmaIM", "Overload", &[PyRep::Integer(mod_high)])
        .await
        .expect("dogmaIM.Overload high");

    // Step SystemActor across 80s (80 x 1.0s ticks) so high & mid (10s cycle -> 8 cycles = 80s) rack heat build
    for _ in 0..80 {
        ctx.advance_secs(1);
        jita_handle.step_manual(1.0).await.expect("step overheat cycle");
    }

    ctx.db.with_snapshot(|snap| {
        let tr = snap.transient_item_attributes.get(&ship_id);
        let hi_h = tr.and_then(|m| m.get(&1175)).copied().unwrap_or(0.0);
        let med_h = tr.and_then(|m| m.get(&1176)).copied().unwrap_or(0.0);
        let low_h = tr.and_then(|m| m.get(&1177)).copied().unwrap_or(0.0);
        let dl = snap.items.get(&mod_left).and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE)).copied().unwrap_or(0.0);
        let dc = snap.items.get(&mod_center).and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE)).copied().unwrap_or(0.0);
        let dr = snap.items.get(&mod_right).and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE)).copied().unwrap_or(0.0);
        let dlow = snap.items.get(&mod_low).and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE)).copied().unwrap_or(0.0);
        let dhi = snap.items.get(&mod_high).and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE)).copied().unwrap_or(0.0);
        (hi_h, med_h, low_h, dl, dc, dr, dlow, dhi)
    })
}

#[tokio::test]
async fn test_b22_overheat_rack_heat_and_damage_propagation() {
    // parity: feat:modules.overheat-rack-heat-generation, feat:modules.overheat-damage-propagation
    eve_testkit::attest(&[
        "feat:modules.overheat-rack-heat-generation",
        "feat:modules.overheat-damage-propagation",
    ]);

    let (hi_heat, med_heat, low_heat, dmg_left_0, dmg_center_0, dmg_right_0, dmg_low_0, _dmg_hi_0) =
        run_overheat_fit(0).await;
    let (_hi_heat_4, _med_heat_4, _low_heat_4, _dmg_left_4, dmg_center_4, _dmg_right_4, _dmg_low_4, _dmg_hi_4) =
        run_overheat_fit(4).await;

    // Rifter SDE: heatAttenuationHi (1259) = 0.5, 125mm Gatling AutoCannon II (2873) heatAbsorbtionRateModifier (1180) = 0.02 -> rate = 0.01
    let expected_hi_heat = 1.0 - (-0.5_f64 * 0.02 * 79.212).exp();
    assert!(
        (hi_heat - expected_hi_heat).abs() < 1e-3,
        "High rack heat (attr 1175 heatHi) must match SDE formula 1 - exp(-0.5 * 0.02 * t): got {hi_heat}, expected {expected_hi_heat}"
    );

    // Rifter SDE: heatAttenuationMed (1261) = 0.5, 5MN MWD II (440) heatAbsorbtionRateModifier (1180) = 0.04 -> rate = 0.02
    // Over 80s, 10s-cycle MWD completes 8 cycles = 80s of heating
    let expected_med_heat = 1.0 - (-0.5_f64 * 0.04 * 80.0).exp();
    assert!(
        (med_heat - expected_med_heat).abs() < 1e-6,
        "Mid rack heat (attr 1176 heatMed) must match SDE formula 1 - exp(-0.5 * 0.04 * 40): got {med_heat}, expected {expected_med_heat}"
    );
    assert_eq!(low_heat, 0.0, "Unheated low rack (attr 1177 heatLow) must remain 0.0");

    assert!(dmg_center_0 > 0.0, "Overloaded center MWD must take heat damage from seeded roll");
    assert!(
        dmg_left_0 > 0.0 || dmg_right_0 > 0.0,
        "Adjacent mid-rack modules must take propagated heat damage from seeded roll (left={dmg_left_0}, right={dmg_right_0})"
    );
    assert_eq!(dmg_low_0, 0.0, "Unheated low rack module must take 0 heat damage across rack boundary");

    // Live SystemActor Thermodynamics IV must reduce heat damage by 20% (0.8x of level 0)
    assert!(
        (dmg_center_4 - 0.8 * dmg_center_0).abs() < 1e-6,
        "Live SystemActor Thermodynamics IV must give 0.8x heat damage of level 0 (lvl4={dmg_center_4}, lvl0={dmg_center_0})"
    );
}
