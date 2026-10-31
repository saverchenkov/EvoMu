//! Integration tests for Pirate FOBs, Pochven Observatory Flashpoint & World Ark,
//! Abyssal Deadspace Weather Dogma Modifiers, and Deathless Shipcaster to FOB:
//! - `feat:pve.pirate-fob-forward-operating-base`
//! - `feat:pve.triglavian-world-ark-observatory`
//! - `feat:navigation.deathless-shipcaster-fob`

use crate::common::*;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::pve_abyssal_hazards::{
    apply_abyssal_weather_dogma_modifiers_to_ship, restore_abyssal_weather_dogma_modifiers,
};
use eve_server::services::pve_incursion_sites::make_site_npc_ball;
use eve_server::services::pve_special_sites_abyssal::AbyssalWeatherKind;
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::collections::HashMap;

fn make_sess(char_id: i64, ship_id: i64, sys_id: u32, station_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        solar_system_id: Some(sys_id),
        station_id,
        ..Default::default()
    }
}

async fn call_svc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, &args, &HashMap::new(), None)
        .await
}

async fn seed_ship_in_space(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&char_id) {
            let mut template = snap.characters.get(&90_001).or_else(|| snap.characters.values().next()).cloned().unwrap();
            template.character_id = char_id;
            template.name = format!("Pilot{char_id}");
            template.war_faction_id = None;
            template.loyalty_points.clear();
            snap.characters.insert(char_id, template);
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::new(),
                ..Default::default()
            },
        );
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            make_site_npc_ball(ship_id, ship_type_id, pos),
            ctx.build_type_combat_state(ship_type_id),
        )
        .await;
}

#[tokio::test]
async fn test_pirate_fob_spawn_defense_scaling_and_loot_bounty() {
    // parity: feat:pve.pirate-fob-forward-operating-base
    eve_testkit::attest(&["feat:pve.pirate-fob-forward-operating-base"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let sys_id = 30_002_537; // Amamake (low-sec 0.44 -> Blood Raiders Stronghold 46364)
    let char_id = 90_001;
    let ship_id = 88_500_001;
    let fob_grid = DVec3::new(200_000.0, 0.0, -200_000.0);

    // Negative: invalid faction and null-sec system refused
    assert_eq!(
        eve_server::services::pve_fob_observatory::spawn_pirate_fob(&ctx, sys_id, 500_001)
            .await
            .unwrap_err()
            .code,
        "InvalidPirateFaction"
    );
    assert_eq!(
        eve_server::services::pve_fob_observatory::spawn_pirate_fob(&ctx, 30_000_157, 500_012)
            .await
            .unwrap_err()
            .code,
        "InvalidSystemSecurity"
    );

    // 1. Seed on-grid attacking fleet (1 Titan=4, 1 BS=3, 1 Cruiser=2 -> weight 9) + 1 off-grid ship, and spawn FOB via live cluster tick
    seed_ship_in_space(&ctx, char_id, ship_id, 11_567, sys_id, fob_grid + DVec3::new(0.0, 0.0, 4_000.0)).await;
    seed_ship_in_space(&ctx, 90_002, 88_500_002, 638, sys_id, fob_grid + DVec3::new(8_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_003, 88_500_003, 621, sys_id, fob_grid + DVec3::new(10_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_004, 88_500_004, 587, sys_id, DVec3::new(2_000_000.0, 0.0, 0.0)).await;

    tick_background_cluster(&ctx).await;

    let fob_id = ctx.db.with_snapshot(|snap| {
        snap.structures
            .values()
            .find(|s| s.solar_system_id == sys_id && s.type_id == 46_364)
            .map(|s| s.structure_id)
    })
    .expect("Living-world cluster tick must spawn Blood Raiders Stronghold (46364) in inhabited low-sec");

    let sys = ctx.systems.get_or_spawn(sys_id);
    let init_balls = sys.list_balls().await.unwrap_or_default();
    assert_eq!(init_balls.iter().filter(|b| b.type_id == 24_000).count(), 3, "FOB must launch 3 Corpatis Bishop raiding cruisers");

    // 2. Fit Smartbomb (3995) & Reapers (40632, 41443) + isotopes, and attack FOB via live client RPCs
    let sb_mod_id = 88_500_010;
    let reaper1_id = 88_500_011;
    let reaper2_id = 88_500_012;
    ctx.db.with_mut_snapshot(|snap| {
        for (mid, tid, flg) in [
            (sb_mod_id, 3_995, InventoryFlag::HiSlot0),
            (reaper1_id, 40_632, InventoryFlag::HiSlot1),
            (reaper2_id, 41_443, InventoryFlag::HiSlot2),
        ] {
            snap.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: flg,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(mid, true);
        }
        for (fid, tid) in [(88_500_020, 16_274), (88_500_021, 17_887)] {
            snap.items.insert(
                fid,
                InventoryItemRecord {
                    item_id: fid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 100_000,
                    ..Default::default()
                },
            );
        }
        if let Some(ch) = snap.characters.get_mut(&90_004) {
            ch.balance = IskCents::ZERO;
        }
    });

    let mut sess = make_sess(char_id, ship_id, sys_id, None);
    call_svc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(sb_mod_id)])
        .await
        .expect("Smartbomb activation against FOB");

    let mid_balls = sys.list_balls().await.unwrap_or_default();
    let defender_bs_count = mid_balls.iter().filter(|b| b.type_id == 16_934).count();
    assert_eq!(defender_bs_count, 9, "FOB defense response must scale to on-grid fleet composition (4+3+2=9 Corpus Prophets, excluding off-grid ship)");

    // 3. Destroy FOB via live superweapon RPCs -> drops Dark Blood faction loot (13791) & pays 100M ISK to each of the 3 on-grid pilots
    let bal_before = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| c.balance.to_wire_f64()).unwrap_or(0.0)
    });
    for rid in [reaper1_id, reaper2_id] {
        call_svc(
            &ctx,
            &mut sess,
            "superWeaponMgr",
            "ActivateSlashModule",
            vec![
                PyRep::Integer(rid),
                PyRep::Real(200_000.0), PyRep::Real(0.0), PyRep::Real(-205_000.0),
                PyRep::Real(200_000.0), PyRep::Real(0.0), PyRep::Real(-198_000.0),
            ],
        )
        .await
        .unwrap();
    }

    let loot_cont_id = fob_id + 90;
    let (st_state, loot_type, bal_after, offgrid_bal) = ctx.db.with_snapshot(|snap| {
        let state = snap.structures.get(&fob_id).map(|s| s.state.clone()).unwrap_or_default();
        let loot = snap.items.values().find(|i| i.location_id == loot_cont_id).map(|i| i.type_id);
        let bal = snap.characters.get(&char_id).map(|c| c.balance.to_wire_f64()).unwrap_or(0.0);
        let off = snap.characters.get(&90_004).map(|c| c.balance.to_wire_f64()).unwrap_or(-1.0);
        (state, loot, bal, off)
    });
    assert_eq!(st_state, "Destroyed");
    assert_eq!(loot_type, Some(13_791), "Blood Raider FOB must drop SDE Dark Blood Dual Heavy Pulse Laser (13791)");
    assert!((bal_after - bal_before - 100_000_000.0).abs() < 1.0, "3 on-grid attackers split 300M ISK FOB bounty");
    assert_eq!(offgrid_bal, 0.0, "Off-grid pilot must not receive FOB bounty");
}

#[tokio::test]
async fn test_pochven_observatory_flashpoint_and_world_ark() {
    // parity: feat:pve.triglavian-world-ark-observatory
    eve_testkit::attest(&["feat:pve.triglavian-world-ark-observatory"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let pochven_sys_id = 30_000_157; // Otela in Pochven (region 10000070)
    let highsec_sys_id = 30_000_142;
    let char_id = 90_001;
    let ship_id = 88_600_001;

    // 1. Negative: non-Pochven system refused
    let err_non_poch = eve_server::services::pve_fob_observatory::spawn_observatory_flashpoint(&ctx, highsec_sys_id)
        .await
        .unwrap_err();
    assert_eq!(err_non_poch.code, "NotInPochven");

    // 2. Spawn Observatory Flashpoint in Otela via live cluster tick
    seed_ship_in_space(&ctx, char_id, ship_id, 587, pochven_sys_id, DVec3::new(-350_000.0, 0.0, 300_000.0)).await;
    tick_background_cluster(&ctx).await;

    let site_id = 1_899_000_000 + i64::from(pochven_sys_id);
    let gate_id = site_id + 2;
    let world_ark_id = site_id + 1;
    let sys = ctx.systems.get_or_spawn(pochven_sys_id);
    let balls = sys.list_balls().await.unwrap_or_default();
    assert!(balls.iter().any(|b| b.id == world_ark_id && b.type_id == 52_361), "World Ark (Xordazh-class, 52361) must spawn");
    assert_eq!(
        balls.iter().filter(|b| matches!(b.type_id, 52_338 | 52_184 | 52_187)).count(),
        3,
        "Observatory Flashpoint must spawn 3 SDE Triglavian defense wave NPCs"
    );

    let mut sess = make_sess(char_id, ship_id, pochven_sys_id, None);

    // 3. Negative: under-sized fleet (< 3 pilots) refused on keeper.ActivateAccelerationGate
    let err_fleet = call_svc(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate_id)])
        .await
        .unwrap_err();
    assert_eq!(err_fleet.code, "InsufficientFleetComposition");

    // 4. Negative: hostile Triglavian standing (< 0.0) refused on keeper.ActivateAccelerationGate
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(500_026, -2.5);
        }
        snap.fleets.insert(
            777_001,
            eve_db::FleetRecord {
                fleet_id: 777_001,
                boss_id: char_id,
                members: vec![char_id, 90_002, 90_003],
                ..Default::default()
            },
        );
    });
    let err_stand = call_svc(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate_id)])
        .await
        .unwrap_err();
    assert_eq!(err_stand.code, "StandingAccessDenied");

    // 5. Seed 18 on-grid fleet ships + 1 off-grid fleet ship in Pochven and enter via keeper.ActivateAccelerationGate
    let mut fleet_ids = Vec::new();
    for idx in 0..18_i64 {
        let cid = 95_000 + idx;
        let sid = 88_610_000 + idx;
        let hull = if idx == 0 { 11_567 } else { 587 };
        fleet_ids.push(cid);
        seed_ship_in_space(
            &ctx,
            cid,
            sid,
            hull,
            pochven_sys_id,
            DVec3::new(-300_000.0 + (idx as f64) * 500.0, 0.0, 295_000.0),
        )
        .await;
    }
    let offgrid_cid = 95_099_i64;
    seed_ship_in_space(&ctx, offgrid_cid, 88_610_099, 587, pochven_sys_id, DVec3::new(5_000_000.0, 0.0, 0.0)).await;
    let mut all_members = vec![offgrid_cid];
    all_members.extend_from_slice(&fleet_ids);

    let reaper_id = 88_620_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        for &cid in &all_members {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.standings.insert(500_026, 1.0);
                ch.balance = IskCents::ZERO;
                ch.loyalty_points.clear();
            }
        }
        snap.fleets.insert(
            777_001,
            eve_db::FleetRecord {
                fleet_id: 777_001,
                boss_id: fleet_ids[0],
                members: all_members.clone(),
                ..Default::default()
            },
        );
        snap.items.insert(
            reaper_id,
            InventoryItemRecord {
                item_id: reaper_id,
                type_id: 40_632,
                owner_id: fleet_ids[0],
                location_id: 88_610_000,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(reaper_id, true);
        snap.items.insert(
            88_620_002,
            InventoryItemRecord {
                item_id: 88_620_002,
                type_id: 16_274,
                owner_id: fleet_ids[0],
                location_id: 88_610_000,
                flag: InventoryFlag::Cargo,
                quantity: 100_000,
                ..Default::default()
            },
        );
    });

    let mut leader_sess = make_sess(fleet_ids[0], 88_610_000, pochven_sys_id, None);
    let _ = call_svc(
        &ctx,
        &mut leader_sess,
        "keeper",
        "ActivateAccelerationGate",
        vec![PyRep::Integer(gate_id)],
    )
    .await
    .expect("Observatory acceleration gate must admit qualified fleet");

    // 6. Destroy World Ark boss (52361) via live weapon RPC -> pays 15 on-grid pilots (skipping off-grid & 16th+)
    call_svc(
        &ctx,
        &mut leader_sess,
        "superWeaponMgr",
        "ActivateSlashModule",
        vec![
            PyRep::Integer(reaper_id),
            PyRep::Real(-300_000.0), PyRep::Real(0.0), PyRep::Real(298_000.0),
            PyRep::Real(-300_000.0), PyRep::Real(0.0), PyRep::Real(302_000.0),
        ],
    )
    .await
    .expect("Live weapon sweep kills World Ark boss");

    let (off_isk, p15_isk, p15_lp, p16_isk, site_exists) = ctx.db.with_snapshot(|snap| {
        let coff = snap.characters.get(&offgrid_cid).unwrap();
        let c15 = snap.characters.get(&fleet_ids[14]).unwrap();
        let c16 = snap.characters.get(&fleet_ids[15]).unwrap();
        (
            coff.balance.to_wire_f64(),
            c15.balance.to_wire_f64(),
            c15.loyalty_points.get(&1_000_137).copied().unwrap_or(0),
            c16.balance.to_wire_f64(),
            snap.service_state_blobs.contains_key(&format!("observatory_site:{site_id}")),
        )
    });
    assert_eq!(off_isk, 0.0, "Off-grid fleet member must not consume an Observatory payout slot");
    assert!((p15_isk - 100_000_000.0).abs() < 1.0);
    assert_eq!(p15_lp, 10_000);
    assert_eq!(p16_isk, 0.0, "16th on-grid fleet member must receive 0 payout");
    assert!(!site_exists, "observatory_site:<site_id> must be deleted on completion");
}

#[tokio::test]
async fn test_abyssal_weather_dogma_modifiers_all_five_storms() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let sys_id = 32_000_001;
    let char_id = 90_001;
    let ship_id = 88_700_001;
    seed_ship_in_space(&ctx, char_id, ship_id, 587, sys_id, DVec3::ZERO).await;

    for weather in [
        AbyssalWeatherKind::Electrical,
        AbyssalWeatherKind::Dark,
        AbyssalWeatherKind::Exotic,
        AbyssalWeatherKind::Firestorm,
        AbyssalWeatherKind::Gamma,
    ] {
        let (orig_267, orig_269) = ctx.sde.types.get(&587).map(|t| (
            t.default_attributes.get(&267).copied().unwrap_or(0.5),
            t.default_attributes.get(&269).copied().unwrap_or(0.65),
        )).unwrap_or((0.5, 0.65));
        let saved = apply_abyssal_weather_dogma_modifiers_to_ship(&ctx, ship_id, weather, 50.0, 50.0);
        assert!(!saved.is_empty());
        ctx.db.with_snapshot(|snap| {
            let ship = snap.items.get(&ship_id).unwrap();
            match weather {
                AbyssalWeatherKind::Electrical => {
                    assert!(ship.dynamic_attributes[&271] > 0.5);
                    assert!(ship.dynamic_attributes[&267] > orig_267);
                    assert!(ship.dynamic_attributes[&55] < 250_000.0);
                }
                AbyssalWeatherKind::Dark => {
                    assert!(ship.dynamic_attributes[&37] > 250.0);
                    assert!((ship.dynamic_attributes[&54] - 7_500.0).abs() < 1e-6);
                }
                AbyssalWeatherKind::Exotic => {
                    assert!(ship.dynamic_attributes[&273] > 0.5);
                    assert!(ship.dynamic_attributes[&269] > orig_269);
                    assert!(ship.dynamic_attributes[&564] > 250.0);
                }
                AbyssalWeatherKind::Firestorm => {
                    assert!(ship.dynamic_attributes[&265] > 500.0);
                    assert!(ship.dynamic_attributes[&274] > 0.5);
                }
                AbyssalWeatherKind::Gamma => {
                    assert!(ship.dynamic_attributes[&263] > 500.0);
                    assert!(ship.dynamic_attributes[&272] > 0.5);
                }
            }
        });
        restore_abyssal_weather_dogma_modifiers(&ctx, ship_id, &saved);
    }

    // Enter an Abyssal pocket (Electrical T1 filament 47765) and verify V24 SDE Electrical weather modifiers on NPC
    let mut sess = make_sess(char_id, ship_id, 30_000_142, None);
    let (_pid, r1_npcs) = eve_server::services::pve_special_sites_abyssal::enter_abyssal_pocket_with_trace(
        &ctx,
        &mut sess,
        char_id,
        47_765,
        1,
        30_000_142,
        None,
    )
    .await;
    assert!(!r1_npcs.is_empty());
    let abyss_sys = ctx.systems.get_or_spawn(32_000_001);
    let (_, npc_ball) = abyss_sys.get_ball_snapshot(r1_npcs[0]).await.unwrap().unwrap();
    let base_npc_cs = ctx.build_type_combat_state(npc_ball.type_id);
    let npc_cs = abyss_sys.get_combat_state(r1_npcs[0]).await.unwrap().unwrap();
    assert!(
        npc_cs.armor_resonance[0] >= base_npc_cs.armor_resonance[0]
            && npc_cs.capacitor_recharge_time_ms < base_npc_cs.capacitor_recharge_time_ms,
        "Abyssal NPCs must receive V24 SDE Electrical EM resonance penalty and faster capacitor recharge"
    );
}

#[tokio::test]
async fn test_deathless_shipcaster_zarzakh_to_fob() {
    // parity: feat:navigation.deathless-shipcaster-fob
    eve_testkit::attest(&["feat:navigation.deathless-shipcaster-fob"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let zarzakh_id = 30_100_000;
    let fob_sys_id = 30_000_142;
    let char_id = 90_001;
    let ship_id = 88_400_001;
    let launcher_id = 88_400_010;
    let guristas_pad_id = 88_400_011;

    seed_ship_in_space(&ctx, char_id, ship_id, 587, zarzakh_id, DVec3::new(10_000.0, 0.0, 10_000.0)).await;
    let sys = ctx.systems.get_or_spawn(zarzakh_id);
    let mut l_ball = make_site_npc_ball(launcher_id, 77_262, DVec3::new(10_500.0, 0.0, 10_000.0));
    l_ball.mode = eve_destiny::MovementMode::Rigid;
    let _ = sys.spawn_ball(l_ball).await;

    eve_server::services::cluster_macho_misc_services::with_misc_state(&ctx, |st| {
        st.launchers.push(eve_server::services::cluster_macho_misc_services::LauncherRecord {
            item_id: launcher_id,
            type_id: 77_262,
            solar_system_id: zarzakh_id,
            faction_id: 0,
            position: DVec3::new(10_500.0, 0.0, 10_000.0),
            linked_pad_ids: vec![guristas_pad_id],
        });
        st.landing_pads.push(eve_server::services::cluster_macho_misc_services::LandingPadRecord {
            item_id: guristas_pad_id,
            type_id: 77_263,
            solar_system_id: fob_sys_id,
            faction_id: 500_010, // Guristas
            position: DVec3::new(50_000.0, 0.0, 50_000.0),
            is_built: true,
            is_disrupted: false,
            link_timestamp: 1,
        });
    });

    let mut sess = make_sess(char_id, ship_id, zarzakh_id, None);

    // 1. Negative: non-pirate character refused
    let err_non_pirate = call_svc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        vec![PyRep::Integer(launcher_id), PyRep::Integer(i64::from(fob_sys_id)), PyRep::Integer(guristas_pad_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_non_pirate.code, "NonPirateRefused");

    // 2. Negative: Angel Cartel (500011) refused on Guristas (500010) FOB pad
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_011);
        }
    });
    let err_mismatch = call_svc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        vec![PyRep::Integer(launcher_id), PyRep::Integer(i64::from(fob_sys_id)), PyRep::Integer(guristas_pad_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_mismatch.code, "FobFactionMismatch");

    // 3. Positive: Guristas (500010) jumps to FOB system
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_010);
        }
    });
    call_svc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        vec![PyRep::Integer(launcher_id), PyRep::Integer(i64::from(fob_sys_id)), PyRep::Integer(guristas_pad_id)],
    )
    .await
    .unwrap();
    assert_eq!(sess.solar_system_id, Some(fob_sys_id));

    // 4. Negative: per-target cooldown blocks immediate repeat jump
    seed_ship_in_space(&ctx, char_id, ship_id, 587, zarzakh_id, DVec3::new(10_000.0, 0.0, 10_000.0)).await;
    sess.solar_system_id = Some(zarzakh_id);
    let err_cd = call_svc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        vec![PyRep::Integer(launcher_id), PyRep::Integer(i64::from(fob_sys_id)), PyRep::Integer(guristas_pad_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_cd.code, "ShipcasterTargetOnCooldown");
}
