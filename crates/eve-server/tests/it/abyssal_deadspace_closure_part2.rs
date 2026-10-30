//! End-to-end integration tests for Abyssal in-pocket restrictions, spherical 75km
//! boundary damage, pocket clearing rules, automated pylons, caches, clouds, and weathers.

#![forbid(unsafe_code)]

use crate::abyssal_deadspace_closure::{
    login_pilot_in_space, move_pilot_to_system, seed_cargo_item, FIL_T0_ELEC, FIL_T1_ELEC,
    FIL_T3_ELEC, SYS_08_LASHESIH, SYS_09_JITA,
};
use crate::common;
use eve_db::{BookmarkRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_proto::quasar::{
    encode_string_field, EvePublicRequest, GoogleAny, GoogleTimestamp, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_FORBIDDEN;
use eve_server::router::ServerContext;
use eve_server::services::pve_special_sites::with_pve_special_reg;
use eve_server::services::pve_special_sites_abyssal::{
    apply_abyssal_weather_dogma_modifiers_to_ship, apply_abyssal_weather_to_combat_and_nav,
    evacuate_abyssal_pockets_for_downtime, evaluate_abyssal_spatial_clouds_for_ship,
    pulse_abyssal_automated_pylons, restore_abyssal_weather_dogma_modifiers,
    step_abyssal_hazards_and_pylons_in_ballpark, AbyssalWeatherKind,
    AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID, AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID,
    AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, BIOLUMINESCENT_CLOUD_TYPE_ID, FILAMENT_CLOUD_TYPE_ID,
    MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID, MULTIBODY_TRACKING_PYLON_TYPE_ID,
    SPATIAL_CLOUD_RADIUS_M, TACHYON_CLOUD_TYPE_ID,
};
use glam::DVec3;
use std::collections::HashMap;

// parity: feat:pve.abyssal-in-pocket-restrictions, feat:pve.abyssal-boundary-damage, feat:pve.abyssal-pocket-clearing-rules
#[tokio::test]
async fn test_abyssal_in_pocket_restrictions_boundary_damage_and_clearing() {
    eve_testkit::attest(&[
        "feat:pve.abyssal-in-pocket-restrictions",
        "feat:pve.abyssal-boundary-damage",
        "feat:pve.abyssal-pocket-clearing-rules",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let sys_id = SYS_08_LASHESIH;
    let char1 = 90_000_001_i64;
    let ship1 = 630_000_001_i64;
    let mut sess1 = login_pilot_in_space(&ctx, 887_201, "demo", "demo-secret", char1, ship1, 621, sys_id, DVec3::new(0.0, 2.0e9, 0.0)).await;

    let cloak_id = 730_100_001_i64;
    let depot_id = 730_100_002_i64;
    let fil_id = 730_100_003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(cloak_id, InventoryItemRecord {
            item_id: cloak_id, type_id: 11_578, owner_id: char1, location_id: ship1, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.module_online_states.insert(cloak_id, true);
        snap.items.insert(depot_id, InventoryItemRecord {
            item_id: depot_id, type_id: 33_474, owner_id: char1, location_id: ship1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
    });
    seed_cargo_item(&ctx, fil_id, FIL_T1_ELEC, char1, ship1, 1);

    let dep = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fil_id)], &kw, None).await.expect("dep");
    let tr1 = dep.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    let ent = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr1)], &kw, None).await.expect("ent");
    let abyss_sys = ent.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    let pid = ent.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
    let abyss_h = ctx.systems.get_or_spawn(abyss_sys);

    // 1. Pocket clearing rules: spawn ~50 km from gate, gate locked until NPCs cleared, no generic NPC wrecks
    let r1_gate = 1_895_000_000 + pid * 10 + 1;
    let gate_pos = abyss_h.get_ball_snapshot(r1_gate).await.unwrap().unwrap().1.position;
    let ship_pos = abyss_h.get_ball_snapshot(ship1).await.unwrap().unwrap().1.position;
    let dist_to_gate = ship_pos.distance(gate_pos);
    assert!((35_000.0..=70_000.0).contains(&dist_to_gate), "Arriving ship spawns ~50km from gate (got {dist_to_gate})");

    let locked_err = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(r1_gate)], &kw, None).await.unwrap_err();
    assert_eq!(locked_err.code, "GateLocked");

    // 2. In-pocket restrictions: warp, bookmark align, cloak, mobile depot launch/anchor, Local chat, D-scan, star map
    let bm_id = 730_300_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(bm_id, BookmarkRecord {
            bookmark_id: bm_id,
            owner_id: char1,
            label: "Abyss BM".into(),
            solar_system_id: abyss_sys,
            x: gate_pos.x + 200_000.0,
            y: gate_pos.y,
            z: gate_pos.z,
            item_id: None,
            memo: String::new(),
            folder_id: None,
            is_shared: false,
        });
    });
    let beyonce_oid = ctx.bind_object(&sess1, "beyonce", abyss_sys as i64).expect("bind beyonce");
    let e_warp = ctx.dispatch_rpc(&mut sess1, "beyonce", Some(beyonce_oid), "CmdGotoBookmark", &[PyRep::Integer(bm_id)], &kw, None).await.unwrap_err();
    assert_eq!(e_warp.code, "CannotWarpInAbyssalSpace");

    let mut align_kw = HashMap::new();
    align_kw.insert("bookmarkID".into(), PyRep::Integer(bm_id));
    let e_align = ctx.dispatch_rpc(&mut sess1, "beyonce", Some(beyonce_oid), "CmdAlignTo", &[], &align_kw, None).await.unwrap_err();
    assert_eq!(e_align.code, "CannotWarpInAbyssalSpace");

    let e_cloak = ctx.dispatch_rpc(&mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(cloak_id), PyRep::String("cloaking".into())], &kw, None).await.unwrap_err();
    assert_eq!(e_cloak.code, "CannotCloakInAbyssalSpace");

    let drop_arg = PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(depot_id), PyRep::Integer(1)])]);
    let e_drop = ctx.dispatch_rpc(&mut sess1, "ship", None, "Drop", &[drop_arg], &kw, None).await.unwrap_err();
    assert_eq!(e_drop.code, "CannotDeployInAbyssalSpace");

    let e_anchor = ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_id)], &kw, None).await.unwrap_err();
    assert_eq!(e_anchor.code, "CannotDeployInAbyssalSpace");

    let local_chan = format!("solarsystemid2_{abyss_sys}");
    let e_lsc = ctx.dispatch_rpc(&mut sess1, "LSC", None, "JoinChannel", &[PyRep::List(vec![PyRep::String(local_chan)])], &kw, None).await.unwrap_err();
    assert_eq!(e_lsc.code, "LocalChannelDisabledInAbyssalSpace");

    let mut q_bytes = Vec::new();
    encode_string_field(1, "blocked in abyss", &mut q_bytes);
    let q_req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: b"abyss-chat-01".to_vec(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.BroadcastMessageRequest"), value: q_bytes }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let q_rsp = ctx.quasar_gateway.handle_public_request(&sess1, &q_req, None, false, ctx.now_unix());
    assert_eq!(q_rsp.status_code, STATUS_FORBIDDEN);

    let e_dscan = ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "DirectionalScan", &[PyRep::Real(360.0), PyRep::Real(1e12)], &kw, None).await.unwrap_err();
    assert_eq!(e_dscan.code, "CannotDscanInThisSystem");

    let e_map = ctx.dispatch_rpc(&mut sess1, "map", None, "GetHistory", &[PyRep::Integer(3), PyRep::Integer(24)], &kw, None).await.unwrap_err();
    assert_eq!(e_map.code, "MapDisabledInAbyssalSpace");

    // Clear Room 1 NPCs -> zero generic NPC wrecks (26557), transfer gate unlocks & spawns 50km from Room 2 gate
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, char1, 0).await;
    assert!(!abyss_h.list_balls().await.unwrap().iter().any(|b| b.type_id == 26_557), "No generic NPC wrecks in Abyssal room");
    let _ = abyss_h.set_ball_position(ship1, gate_pos + DVec3::new(100.0, 0.0, 0.0)).await;
    ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(r1_gate)], &kw, None).await.expect("R1->R2");
    let r2_gate = 1_895_000_000 + pid * 10 + 2;
    let r2_gate_pos = abyss_h.get_ball_snapshot(r2_gate).await.unwrap().unwrap().1.position;
    let r2_ship_pos = abyss_h.get_ball_snapshot(ship1).await.unwrap().unwrap().1.position;
    assert!((35_000.0..=70_000.0).contains(&r2_ship_pos.distance(r2_gate_pos)), "Room 2 arrival ~50km from gate");

    // 3. Spherical 75 km room boundary damage: escalates, resets inside <= 75km, destroys probe, ejects pod & kills pod
    let room2_center = with_pve_special_reg(&ctx, |reg| {
        DVec3::from_array(reg.abyssal_pockets[&char1].grid_offset) + DVec3::new(0.0, 0.0, 50_000.0)
    });
    let probe_id = 730_400_001_i64;
    let probe_ball = Ball {
        id: probe_id, type_id: 30_028, position: room2_center + DVec3::new(80_000.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 10.0, radius: 5.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    abyss_h.try_spawn_ball_with_meta(
        probe_ball,
        ctx.build_type_combat_state(30_028),
        ctx.build_ship_slim_meta(char1, 30_028),
    );

    if let Ok(Some(mut cs)) = abyss_h.get_combat_state(ship1).await {
        cs.max_structure_hp = 50_000.0;
        cs.structure_hp = 50_000.0;
        let _ = abyss_h.set_combat_state(ship1, cs).await;
    }
    let _ = abyss_h.set_ball_position(ship1, room2_center + DVec3::new(80_000.0, 0.0, 0.0)).await;
    let hp0 = abyss_h.get_combat_state(ship1).await.unwrap().unwrap().structure_hp;
    {
        let mut bp = abyss_h.ballpark.write().unwrap();
        step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&ctx.sde), Some(&ctx.db), abyss_sys);
    }
    assert!(abyss_h.get_ball_snapshot(probe_id).await.unwrap().is_none(), "Out-of-bounds probe destroyed");
    let hp1 = abyss_h.get_combat_state(ship1).await.unwrap().unwrap().structure_hp;
    let dmg1 = hp0 - hp1;
    assert!(dmg1 > 0.0, "Boundary damage applied outside 75km");

    {
        let mut bp = abyss_h.ballpark.write().unwrap();
        step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&ctx.sde), Some(&ctx.db), abyss_sys);
    }
    let hp2 = abyss_h.get_combat_state(ship1).await.unwrap().unwrap().structure_hp;
    let dmg2 = hp1 - hp2;
    assert!(dmg2 > dmg1, "Boundary damage escalates per tick ({dmg2} > {dmg1})");

    let _ = abyss_h.set_ball_position(ship1, room2_center + DVec3::new(10_000.0, 0.0, 0.0)).await;
    {
        let mut bp = abyss_h.ballpark.write().unwrap();
        step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&ctx.sde), Some(&ctx.db), abyss_sys);
    }
    assert_eq!(abyss_h.get_combat_state(ship1).await.unwrap().unwrap().structure_hp, hp2, "No boundary damage inside 75km");
    let _ = abyss_h.set_ball_position(ship1, room2_center + DVec3::new(80_000.0, 0.0, 0.0)).await;
    {
        let mut bp = abyss_h.ballpark.write().unwrap();
        step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&ctx.sde), Some(&ctx.db), abyss_sys);
    }
    let hp3 = abyss_h.get_combat_state(ship1).await.unwrap().unwrap().structure_hp;
    assert!((hp2 - hp3 - dmg1).abs() < 1e-3, "Escalation counter reset after returning inside 75km");

    if let Ok(Some(mut cs)) = abyss_h.get_combat_state(ship1).await {
        cs.structure_hp = 300.0;
        let _ = abyss_h.set_combat_state(ship1, cs).await;
    }
    for _ in 0..40 {
        if abyss_h.get_ball_snapshot(ship1).await.unwrap().is_none() {
            break;
        }
        let mut bp = abyss_h.ballpark.write().unwrap();
        step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&ctx.sde), Some(&ctx.db), abyss_sys);
    }
    let pod_id = ctx.db.with_snapshot(|s| s.characters[&char1].active_ship_id);
    assert_ne!(pod_id, ship1, "Capsule ejected when ship destroyed by boundary");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&pod_id].type_id), 670);

    for _ in 0..20 {
        if ctx.db.with_snapshot(|s| s.characters[&char1].station_id.is_some()) {
            break;
        }
        let mut bp = abyss_h.ballpark.write().unwrap();
        step_abyssal_hazards_and_pylons_in_ballpark(&mut bp, Some(&ctx.sde), Some(&ctx.db), abyss_sys);
    }
    assert!(ctx.db.with_snapshot(|s| s.characters[&char1].station_id.is_some()), "Pilot respawned at home station after pod boundary death");

    // 4. Downtime evacuation: preserves DroneBay drones, destroys deployed drones in space, restores origin Local chat
    let char2 = 90_000_002_i64;
    let ship2 = 630_000_002_i64;
    let mut sess2 = login_pilot_in_space(&ctx, 887_202, "other", "other-secret", char2, ship2, 621, sys_id, DVec3::new(30_000_000.0, 2.0e9, 0.0)).await;
    let bay_drone = 730_500_001_i64;
    let space_drone = 730_500_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(bay_drone, InventoryItemRecord {
            item_id: bay_drone, type_id: 2456, owner_id: char2, location_id: ship2, flag: InventoryFlag::DroneBay, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.items.insert(space_drone, InventoryItemRecord {
            item_id: space_drone, type_id: 2456, owner_id: char2, location_id: abyss_sys as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
    });
    seed_cargo_item(&ctx, 730_500_003, FIL_T1_ELEC, char2, ship2, 1);
    let dep2 = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(730_500_003)], &kw, None).await.expect("dep2");
    let tr2 = dep2.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    let ent2 = ctx.dispatch_rpc(&mut sess2, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(tr2)], &kw, None).await.expect("ent2");
    let p2_sys = ent2.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(space_drone, eve_server::services::combat_crimewatch::DroneSpaceState {
            ball_id: space_drone,
            drone_item_id: space_drone,
            type_id: 2456,
            group_id: 100,
            owner_char_id: char2,
            original_owner_char_id: char2,
            parent_ship_id: ship2,
            solar_system_id: p2_sys,
            target_id: None,
            activity_state: 0,
            assist_char_id: None,
            guard_char_id: None,
            is_abandoned: false,
            is_sentry: false,
            bandwidth_used: 5.0,
            max_velocity: 400.0,
            optimal_range: 5_000.0,
            damage_vector: DamageVector::default(),
            shield_repair_amount: 0.0,
            armor_repair_amount: 0.0,
            hull_repair_amount: 0.0,
            web_speed_factor: 1.0,
            neut_amount: 0.0,
            ecm_strength: 0.0,
            tp_sig_bonus_pct: 0.0,
        });
    });

    let evac_count = evacuate_abyssal_pockets_for_downtime(&ctx);
    assert!(evac_count >= 1, "Downtime evacuation rescued active pocket");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char2].solar_system_id, sys_id, "Pilot returned to origin system");
        assert!(snap.items.contains_key(&bay_drone), "Drone in DroneBay preserved");
        assert!(!snap.items.contains_key(&space_drone), "Deployed drone in space destroyed");
        let origin_chan = format!("solarsystemid2:{sys_id}");
        assert!(snap.chat_channels.get(&origin_chan).is_some_and(|c| c.members.contains(&char2)), "Origin Local chat restored");
    });
}

// parity: feat:abyssal.automated-pylons, feat:abyssal.bioadaptive-cache-rewards, feat:abyssal.spatial-hazard-clouds, feat:abyssal.weather-effects-and-penalties
#[tokio::test]
async fn test_abyssal_pylons_caches_hazard_clouds_and_weather_matrix() {
    eve_testkit::attest(&[
        "feat:abyssal.automated-pylons",
        "feat:abyssal.bioadaptive-cache-rewards",
        "feat:abyssal.spatial-hazard-clouds",
        "feat:abyssal.weather-effects-and-penalties",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let abyss_sys = 32_000_005_u32;
    let sys_h = ctx.systems.get_or_spawn(abyss_sys);

    // 1. Automated Pylons: Small (15km), Medium (25km), Large (40km) Suppressors & Multibody Tracking Pylons (ships + sentry drones)
    let ship_id = 740_000_001_i64;
    let sentry_id = 740_000_002_i64;
    let drone_id = 740_000_003_i64;
    let missile_id = 740_000_004_i64;
    let fighter_id = 740_000_005_i64;
    let rogue_npc_id = 740_000_006_i64;
    let trig_npc_id = 740_000_007_i64;

    for (bid, tid, pos, max_vel) in [
        (ship_id, 621_u32, DVec3::new(10_000.0, 0.0, 0.0), 250.0),
        (sentry_id, 23_559, DVec3::new(10_500.0, 0.0, 0.0), 0.0), // Warden I Sentry Drone (group 100)
        (drone_id, 2456, DVec3::new(20_000.0, 0.0, 0.0), 400.0), // Hobgoblin I at 20km
        (missile_id, 204, DVec3::new(20_000.0, 100.0, 0.0), 2_000.0), // Inferno Cruise Missile at 20km
        (fighter_id, 23_057, DVec3::new(20_000.0, 200.0, 0.0), 800.0), // Dragonfly I Fighter at 20km
        (rogue_npc_id, 47_847, DVec3::new(20_000.0, 300.0, 0.0), 500.0), // Sparkneedle Tessella (group 1997) at 20km
        (trig_npc_id, 48_092, DVec3::new(5_000.0, 0.0, 0.0), 400.0), // Striking Damavik (group 1982) at 5km
    ] {
        let mut cs = ctx.build_type_combat_state(tid);
        cs.shield_hp = 5_000.0;
        cs.max_shield_hp = 5_000.0;
        cs.structure_hp = 5_000.0;
        cs.max_structure_hp = 5_000.0;
        let _ = sys_h.spawn_ball_with_combat(Ball {
            id: bid, type_id: tid, position: pos, velocity: DVec3::ZERO, max_velocity: max_vel,
            inertia: 1.0, mass: 1_000.0, radius: 25.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }, cs).await;
    }

    let _ = sys_h.spawn_ball(Ball {
        id: 740_010_001, type_id: AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: 15_000.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    let _ = pulse_abyssal_automated_pylons(&ctx, abyss_sys).await;
    assert_eq!(sys_h.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp, 5_000.0, "Small suppressor (15km) spares drone at 20km");
    assert_eq!(sys_h.get_combat_state(trig_npc_id).await.unwrap().unwrap().shield_hp, 5_000.0, "Suppressor spares Triglavian NPC at 5km");
    assert_eq!(sys_h.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp, 5_000.0, "Suppressor spares player ship at 10km");
    sys_h.try_remove_ball(740_010_001);

    assert_eq!(AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID, 47_438);
    assert_eq!(AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID, 48_254);
    let _ = sys_h.spawn_ball(Ball {
        id: 740_010_002, type_id: AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: 25_000.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    let _ = pulse_abyssal_automated_pylons(&ctx, abyss_sys).await;
    assert!(sys_h.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp < 5_000.0, "Medium suppressor (25km) damages drone at 20km");
    assert!(sys_h.get_combat_state(missile_id).await.unwrap().unwrap().shield_hp < 5_000.0, "Medium suppressor damages missile at 20km");
    assert!(sys_h.get_combat_state(fighter_id).await.unwrap().unwrap().shield_hp < 5_000.0, "Medium suppressor damages fighter at 20km");
    assert!(sys_h.get_combat_state(rogue_npc_id).await.unwrap().unwrap().shield_hp < 5_000.0, "Medium suppressor damages rogue drone NPC at 20km");

    let _ = sys_h.set_ball_position(drone_id, DVec3::new(35_000.0, 0.0, 0.0)).await;
    let hp_35 = sys_h.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp;
    let _ = pulse_abyssal_automated_pylons(&ctx, abyss_sys).await;
    assert_eq!(sys_h.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp, hp_35, "Medium suppressor (25km) spares drone at 35km");
    sys_h.try_remove_ball(740_010_002);

    let _ = sys_h.spawn_ball(Ball {
        id: 740_010_003, type_id: AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: 40_000.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    let _ = pulse_abyssal_automated_pylons(&ctx, abyss_sys).await;
    assert!(sys_h.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp < hp_35, "Large suppressor (40km) damages drone at 35km");
    sys_h.try_remove_ball(740_010_003);

    assert_eq!(MULTIBODY_TRACKING_PYLON_TYPE_ID, 47_469);
    assert_eq!(MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID, 47_470);
    let _ = sys_h.spawn_ball(Ball {
        id: 740_010_004, type_id: MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: 30_000.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    let _ = pulse_abyssal_automated_pylons(&ctx, abyss_sys).await;
    assert!((sys_h.get_combat_state(ship_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.8).abs() < 1e-6);
    assert!((sys_h.get_combat_state(sentry_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.8).abs() < 1e-6);

    // 2. Bioadaptive Cache (47951) vs Extraction Subnode (49662): T0 has NO Extraction Subnode; T1+ spawns both & 48120 wreck on destroy
    let char1 = 90_000_001_i64;
    let c_ship = 640_000_020_i64;
    let mut sess1 = login_pilot_in_space(&ctx, 887_301, "demo", "demo-secret", char1, c_ship, 621, SYS_09_JITA, DVec3::new(0.0, 2.0e9, 0.0)).await;
    seed_cargo_item(&ctx, 740_100_010, FIL_T0_ELEC, char1, c_ship, 1);
    let d0 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(740_100_010)], &kw, None).await.expect("dep T0");
    let t0 = d0.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    let e0 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(t0)], &kw, None).await.expect("ent T0");
    let pid0 = e0.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
    let subnode0_id = 1_896_000_000 + pid0 * 10 + 1 + 500_000;
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&subnode0_id)), "T0 Tranquil pocket must NOT spawn Extraction Subnode");
    for r in 0..3 {
        crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, char1, r).await;
        let gid = 1_895_000_000 + pid0 * 10 + (r as i64 + 1);
        if r < 2 {
            ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(gid)], &kw, None).await.expect("g");
        } else {
            ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEndGateActivation", &[PyRep::Integer(gid)], &kw, None).await.expect("exit");
        }
    }

    move_pilot_to_system(&ctx, &mut sess1, char1, c_ship, 621, SYS_08_LASHESIH, DVec3::new(5_000_000.0, 2.0e9, 0.0)).await;
    seed_cargo_item(&ctx, 740_100_011, FIL_T3_ELEC, char1, c_ship, 1);
    let d3 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(740_100_011)], &kw, None).await.expect("dep T3");
    let t3 = d3.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    let e3 = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(t3)], &kw, None).await.expect("ent T3");
    let pid3 = e3.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
    let p3_sys = e3.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    let cache3_id = 1_896_000_000 + pid3 * 10 + 1;
    let subnode3_id = cache3_id + 500_000;
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&cache3_id].type_id, 47_951, "Bioadaptive Cache type 47951");
        assert_eq!(s.items[&subnode3_id].type_id, 49_662, "Extraction Subnode type 49662");
    });
    eve_server::services::pve_special_sites_abyssal::on_abyssal_cache_destroyed_core(&ctx.systems, &ctx.db, p3_sys, cache3_id, Some(char1)).await;
    ctx.db.with_snapshot(|s| {
        let wreck = s.items.values().find(|i| i.type_id == 48_120 && i.location_id == p3_sys as i64).expect("Triglavian Bioadaptive Wreck 48120 spawned");
        assert!(s.items.values().any(|i| i.location_id == wreck.item_id && i.quantity >= 4), "Loot transferred to wreck");
    });

    // 3. Spatial hazard clouds (Bioluminescence, Filament, Tachyon) & all 5 weather types across T0/T3/T6
    let _ = sys_h.spawn_ball(Ball {
        id: 740_020_001, type_id: BIOLUMINESCENT_CLOUD_TYPE_ID, position: DVec3::new(0.0, 50_000.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: SPATIAL_CLOUD_RADIUS_M, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    let _ = sys_h.spawn_ball(Ball {
        id: 740_020_002, type_id: FILAMENT_CLOUD_TYPE_ID, position: DVec3::new(50_000.0, 50_000.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: SPATIAL_CLOUD_RADIUS_M, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    let _ = sys_h.spawn_ball(Ball {
        id: 740_020_003, type_id: TACHYON_CLOUD_TYPE_ID, position: DVec3::new(-50_000.0, 50_000.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: SPATIAL_CLOUD_RADIUS_M, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    let _ = sys_h.set_ball_position(ship_id, DVec3::new(0.0, 50_000.0, 0.0)).await;
    assert_eq!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyss_sys, ship_id).await, Some("BioluminescentCloud"));
    let _ = sys_h.set_ball_position(ship_id, DVec3::new(50_000.0, 50_000.0, 0.0)).await;
    assert_eq!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyss_sys, ship_id).await, Some("FilamentCloud"));
    let _ = sys_h.set_ball_position(ship_id, DVec3::new(-50_000.0, 50_000.0, 0.0)).await;
    assert_eq!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyss_sys, ship_id).await, Some("TachyonCloud"));
    let _ = sys_h.set_ball_position(ship_id, DVec3::new(0.0, 0.0, 0.0)).await;
    assert!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyss_sys, ship_id).await.is_none());

    let w_ship = 640_000_030_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(w_ship, InventoryItemRecord {
            item_id: w_ship, type_id: 621, owner_id: char1, location_id: SYS_08_LASHESIH as i64,
            flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
    });
    for pct in [30.0_f64, 50.0, 70.0] {
        for weather in [
            AbyssalWeatherKind::Electrical,
            AbyssalWeatherKind::Dark,
            AbyssalWeatherKind::Exotic,
            AbyssalWeatherKind::Firestorm,
            AbyssalWeatherKind::Gamma,
        ] {
            let mut cs = ctx.build_type_combat_state(621);
            let (mut vel, mut opt) = (250.0, 15_000.0);
            apply_abyssal_weather_to_combat_and_nav(weather, pct, pct, &mut cs, &mut vel, &mut opt);
            let saved = apply_abyssal_weather_dogma_modifiers_to_ship(&ctx, w_ship, weather, pct, pct);
            assert!(!saved.is_empty());
            restore_abyssal_weather_dogma_modifiers(&ctx, w_ship, &saved);
            assert!(ctx.db.with_snapshot(|s| s.items[&w_ship].dynamic_attributes.is_empty()));
        }
    }
}
