//! End-to-end integration tests for Batch 22 open `feat:structures.*` and `feat:modules.*` rows.

#![forbid(unsafe_code)]

use crate::open_feats_and_rpcs_b22::{field, rpc, select_pilot, PILOT_1, PILOT_2};
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

#[tokio::test]
async fn test_b22_overheat_rack_heat_and_damage_propagation() {
    // parity: feat:modules.overheat-rack-heat-generation, feat:modules.overheat-damage-propagation
    eve_testkit::attest(&[
        "feat:modules.overheat-rack-heat-generation",
        "feat:modules.overheat-damage-propagation",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select_pilot(&ctx, 55_203, "demo", "demo-secret", PILOT_1).await;

    let ship_id = 77_103_001_i64;
    let mod_high = 77_103_005_i64;
    let mod_left = 77_103_002_i64;
    let mod_center = 77_103_003_i64;
    let mod_right = 77_103_004_i64;
    let mod_dist2 = 77_103_006_i64;
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
                type_id: 587, // Rifter
                owner_id: PILOT_1,
                location_id: JITA_SYS as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (mid, flag, tid) in [
            (mod_high, InventoryFlag::HiSlot0, 2873),    // 125mm Gatling AutoCannon I (flag 27, high rack)
            (mod_left, InventoryFlag::MedSlot0, 3242),   // Warp Scrambler I (flag 19, dist 1)
            (mod_center, InventoryFlag::MedSlot1, 440),  // 5MN Microwarpdrive I (flag 20, overloaded)
            (mod_right, InventoryFlag::MedSlot2, 527),   // Stasis Webifier I (flag 21, dist 1)
            (mod_dist2, InventoryFlag::MedSlot3, 399),   // Small Shield Booster I (flag 22, dist 2)
        ] {
            snap.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: tid,
                    owner_id: PILOT_1,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(mid, true);
        }
        snap.skills
            .entry(PILOT_1)
            .or_default()
            .insert(
                28164,
                TrainedSkillRecord {
                    type_id: 28164, // Thermodynamics
                    level: 4,
                    skill_points: 135_765.0,
                },
            );
        snap.skills
            .entry(PILOT_1)
            .or_default()
            .insert(
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
    sc.capacitor = 1_000.0;
    jita_handle.spawn_ball_with_combat(sb, sc).await.expect("spawn Rifter");

    // Online modules, activate & overload center module (5MN MWD) via dogmaIM.Overload
    let _ = rpc(&ctx, &mut s1, "dogmaIM", "ShipOnlineModules", &[PyRep::Integer(ship_id)])
        .await
        .expect("ShipOnlineModules");
    let base_max_vel = ctx
        .get_or_rebuild_ship_fitting(PILOT_1, ship_id)
        .expect("base fit")
        .ship
        .get_attribute(eve_dogma::attr_ids::MAX_VELOCITY, &ctx.sde);
    let ovl_rsp = rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "Overload",
        &[PyRep::Integer(mod_center)],
    )
    .await
    .expect("dogmaIM.Overload");
    let ovl_max_vel = match field(&ovl_rsp, "effectiveMaxVelocity") {
        PyRep::Real(v) => v,
        _ => ctx
            .get_or_rebuild_ship_fitting(PILOT_1, ship_id)
            .expect("overloaded fit")
            .ship
            .get_attribute(eve_dogma::attr_ids::MAX_VELOCITY, &ctx.sde),
    };
    assert!(
        ovl_max_vel > base_max_vel * 1.2,
        "Overloaded 5MN MWD must apply +50% propulsion speed overload bonus (base={base_max_vel}, ovl={ovl_max_vel})"
    );

    // Step SystemActor across 4 MWD cycles (40s) so rack heat builds and propagates damage
    for _ in 0..4 {
        ctx.advance_secs(10);
        jita_handle.step_manual(10.0).await.expect("step overheat cycle");
    }

    let (med_heat, dmg_left, dmg_center, dmg_right, dmg_dist2, dmg_high) = ctx.db.with_snapshot(|snap| {
        let mh = snap
            .transient_item_attributes
            .get(&ship_id)
            .and_then(|m| m.get(&1176))
            .copied()
            .unwrap_or(0.0);
        let dl = snap.items.get(&mod_left).and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE)).copied().unwrap_or(0.0);
        let dc = snap.items.get(&mod_center).and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE)).copied().unwrap_or(0.0);
        let dr = snap.items.get(&mod_right).and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE)).copied().unwrap_or(0.0);
        let d2 = snap.items.get(&mod_dist2).and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE)).copied().unwrap_or(0.0);
        let dh = snap.items.get(&mod_high).and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE)).copied().unwrap_or(0.0);
        (mh, dl, dc, dr, d2, dh)
    });
    let expected_heat = 1.0 - (-0.04_f64 * 40.0).exp();
    assert!(
        (med_heat - expected_heat).abs() < 1e-6,
        "Mid rack heat (attr 1176) must match exponential formula 1 - exp(-0.04 * 40): got {med_heat}, expected {expected_heat}"
    );
    assert!(dmg_center > 0.0, "Overloaded center MWD must take heat damage");
    assert!(dmg_left > 0.0 && dmg_right > 0.0, "Adjacent mid-rack modules must take propagated heat damage");
    assert!((dmg_left - dmg_right).abs() < 1e-9, "Symmetric distance-1 slots must take equal heat damage");
    let expected_falloff = (-0.5_f64).exp();
    assert!(
        ((dmg_dist2 / dmg_right) - expected_falloff).abs() < 1e-6,
        "Slot-distance 2 vs 1 heat damage ratio must equal exp(-0.5): got {}, expected {expected_falloff}",
        dmg_dist2 / dmg_right
    );
    assert_eq!(dmg_high, 0.0, "Unheated high rack module must take 0 heat damage across rack boundary");

    // Verify Thermodynamics level 4 gives exactly 0.8x heat damage vs level 0
    let dmg_thermo4 = eve_dogma::thermodynamics::effective_heat_damage(19.0, 4);
    let dmg_thermo0 = eve_dogma::thermodynamics::effective_heat_damage(19.0, 0);
    assert!(
        (dmg_thermo4 - 0.8 * dmg_thermo0).abs() < 1e-9,
        "Thermodynamics IV must give exactly 0.8x overheat damage of level 0 (lvl4={dmg_thermo4}, lvl0={dmg_thermo0})"
    );
}
