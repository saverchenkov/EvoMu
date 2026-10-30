//! End-to-end integration tests for Abyssal in-pocket restrictions, spherical 75km
//! boundary damage, pocket clearing rules, automated pylons, caches, clouds, and weathers.

#![forbid(unsafe_code)]

use crate::abyssal_deadspace_closure::{
    login_pilot_in_space, move_pilot_to_system, seed_cargo_item, FIL_T0_ELEC, FIL_T1_ELEC,
    FIL_T3_ELEC, SYS_08_LASHESIH, SYS_09_JITA, SYS_LOW_TAMA,
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
    evacuate_abyssal_pockets_for_downtime, evaluate_abyssal_spatial_clouds_for_ship,
    pulse_abyssal_automated_pylons, roll_abyssal_weather_penalty_pct, spatial_cloud_radius_m,
    step_abyssal_hazards_and_pylons_in_ballpark, suppressor_radius_m, tracking_pylon_radius_and_mult,
    AbyssalWeatherKind, AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID, AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID,
    AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, BIOLUMINESCENT_CLOUD_LARGE_TYPE_ID,
    BIOLUMINESCENT_CLOUD_SMALL_TYPE_ID, BIOLUMINESCENT_CLOUD_TYPE_ID,
    FILAMENT_CLOUD_LARGE_TYPE_ID, FILAMENT_CLOUD_SMALL_TYPE_ID, FILAMENT_CLOUD_TYPE_ID,
    MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID, MULTIBODY_TRACKING_PYLON_TYPE_ID,
    PROVING_TACHYON_CLOUD_TYPE_ID, SPATIAL_CLOUD_RADIUS_M, TACHYON_CLOUD_TYPE_ID,
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
            bookmark_id: bm_id, owner_id: char1, label: "Abyss BM".into(), solar_system_id: abyss_sys,
            x: gate_pos.x + 200_000.0, y: gate_pos.y, z: gate_pos.z, item_id: None, memo: String::new(), folder_id: None, is_shared: false,
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

    // Clear Room 1 NPCs -> zero wrecks of any type, zero bounty, zero faction standing change, 300 HP Bioadaptive Cache
    let r1_cache_id = 1_896_000_000 + pid * 10 + 1;
    let cache_cs = abyss_h.get_combat_state(r1_cache_id).await.unwrap().expect("Bioadaptive Cache combat state");
    let cache_total_hp = cache_cs.shield_hp + cache_cs.armor_hp + cache_cs.structure_hp;
    assert!((cache_total_hp - 300.0).abs() < 1e-6, "Triglavian Bioadaptive Cache must have 300 total HP (got {cache_total_hp})");

    let (pre_balance, pre_standings, pre_standing_tx) = ctx.db.with_snapshot(|snap| {
        (
            snap.characters[&char1].balance,
            snap.characters[&char1].standings.clone(),
            snap.standing_transactions.get(&char1).cloned(),
        )
    });
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, char1, 0).await;
    assert!(
        !abyss_h.list_balls().await.unwrap().iter().any(|b| ctx.sde.wrecks.values().any(|&w| w == b.type_id) || ctx.sde.types.get(&b.type_id).is_some_and(|t| t.group_id == 186)),
        "Abyssal NPCs must leave zero wrecks of any type in space"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char1].balance, pre_balance, "Abyssal NPCs must pay zero bounty");
        assert_eq!(snap.characters[&char1].standings, pre_standings, "Abyssal NPCs must cause zero faction standing change");
        assert_eq!(snap.standing_transactions.get(&char1).cloned(), pre_standing_tx, "Abyssal NPCs must record zero standing transactions");
        assert!(
            !snap.items.values().any(|i| i.location_id == abyss_sys as i64 && (ctx.sde.wrecks.values().any(|&w| w == i.type_id) || ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186))),
            "Abyssal NPCs must leave zero wreck items in DB"
        );
    });
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
    // Position clock at 10:55 UTC (5m before 11:00 UTC downtime) so the 20m Abyssal timer stays active across the window boundary
    let sec_init = ctx.now_unix() % 86_400;
    let target_pre_dt = 11 * 3_600 - 300;
    ctx.advance_virtual_clock(if sec_init <= target_pre_dt { target_pre_dt - sec_init } else { 86_400 - sec_init + target_pre_dt });
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
            ball_id: space_drone, drone_item_id: space_drone, type_id: 2456, group_id: 100, owner_char_id: char2,
            original_owner_char_id: char2, parent_ship_id: ship2, solar_system_id: p2_sys, target_id: None, activity_state: 0,
            assist_char_id: None, guard_char_id: None, is_abandoned: false, is_sentry: false, bandwidth_used: 5.0,
            max_velocity: 400.0, optimal_range: 5_000.0, damage_vector: DamageVector::default(), shield_repair_amount: 0.0,
            armor_repair_amount: 0.0, hull_repair_amount: 0.0, web_speed_factor: 1.0, neut_amount: 0.0, ecm_strength: 0.0, tp_sig_bonus_pct: 0.0,
        });
    });

    // Downtime outside 11:00 UTC window (10:55 UTC) must NOT evacuate active pocket; inside 11:00 UTC window (11:01 UTC) evacuates & destroys deployed drones
    let dt_non_window = eve_server::services::wallet_serverops::run_daily_downtime_and_jobs(&ctx);
    assert_eq!(dt_non_window.get("is1100UtcWindow"), Some(&0));
    assert_eq!(dt_non_window.get("evacuatedAbyssalPilots"), Some(&0), "Outside 11:00 UTC window must not evacuate pocket");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char2].solar_system_id), p2_sys);

    ctx.advance_virtual_clock(360); // 10:55 UTC -> 11:01 UTC (6m elapsed, well within 20m Abyssal timer)
    let dt_window = eve_server::services::wallet_serverops::run_daily_downtime_and_jobs(&ctx);
    assert_eq!(dt_window.get("is1100UtcWindow"), Some(&1));
    assert!(dt_window.get("evacuatedAbyssalPilots").copied().unwrap_or(0) >= 1, "11:00 UTC downtime rescues active pocket");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char2].solar_system_id, sys_id, "Pilot returned to origin system");
        assert!(snap.items.contains_key(&bay_drone), "Drone in DroneBay preserved");
        assert!(!snap.items.contains_key(&space_drone), "Deployed drone in space destroyed");
        let origin_chan = format!("solarsystemid2:{sys_id}");
        assert!(snap.chat_channels.get(&origin_chan).is_some_and(|c| c.members.contains(&char2)), "Origin Local chat restored");
    });

    // Verify Triglavian Extraction SubNode (49662) rolls limited loot pool and may be empty (~35% chance)
    let mut empty_seed = None;
    let mut full_seed = None;
    for s in 1_u64..200 {
        ctx.set_mechanic_rng_seed(Some(s));
        let _ = ctx.mechanic_rng.roll(0.5); // consumed by roll_abyssal_weather_penalty_pct
        let u = ctx.mechanic_rng.try_unit_f64().unwrap();
        if u < 0.35 && empty_seed.is_none() { empty_seed = Some(s); }
        if u >= 0.35 && full_seed.is_none() { full_seed = Some(s); }
        if empty_seed.is_some() && full_seed.is_some() { break; }
    }
    for (idx, (seed, expect_loot)) in [(empty_seed.unwrap(), false), (full_seed.unwrap(), true)].into_iter().enumerate() {
        let pos = DVec3::new((idx as f64 + 1.0) * 8_000_000.0, 2.0e9, 0.0);
        move_pilot_to_system(&ctx, &mut sess1, char1, ship1, 621, sys_id, pos).await;
        ctx.set_mechanic_rng_seed(Some(seed));
        let fiid = 730_500_000 + idx as i64;
        seed_cargo_item(&ctx, fiid, FIL_T1_ELEC, char1, ship1, 1);
        let d_sub = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fiid)], &kw, None).await.expect("dep");
        let t_sub = d_sub.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
        let e_sub = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(t_sub)], &kw, None).await.expect("ent");
        let pid_sub = e_sub.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
        let sub_id = 1_896_000_000 + pid_sub * 10 + 1 + 500_000;
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.items[&sub_id].type_id, 49_662, "Extraction SubNode spawned in T1");
            let child_count = snap.items.values().filter(|i| i.location_id == sub_id).count();
            if expect_loot {
                assert!(child_count > 0, "Non-empty subnode roll must contain limited loot items");
            } else {
                assert_eq!(child_count, 0, "Empty subnode roll (~35%) must contain 0 items");
            }
        });
        let _ = evacuate_abyssal_pockets_for_downtime(&ctx);
    }
}

// parity: feat:abyssal.automated-pylons
#[tokio::test]
async fn test_abyssal_pylons_caches_hazard_clouds_and_weather_matrix() {
    eve_testkit::attest(&[
        "feat:abyssal.automated-pylons",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let abyss_sys = 32_000_005_u32;
    let sys_h = ctx.systems.get_or_spawn(abyss_sys);

    // 1. Automated Pylons (V24 SDE): Suppressors (47437=15km, 47438=40km, 48254=40km; spare Sentry Drones & Tesseras)
    // and Multibody Tracking Pylons (47469=15km 1.8x, 47470=40km 1.6x; buff player ships, Sentry Drones, and NPC ships)
    assert_eq!(suppressor_radius_m(Some(&ctx.sde), AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID, 0.0), 15_000.0);
    assert_eq!(suppressor_radius_m(Some(&ctx.sde), AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID, 0.0), 40_000.0);
    assert_eq!(suppressor_radius_m(Some(&ctx.sde), AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID, 0.0), 40_000.0);
    assert_eq!(tracking_pylon_radius_and_mult(Some(&ctx.sde), MULTIBODY_TRACKING_PYLON_TYPE_ID, 0.0), (15_000.0, 1.8));
    assert_eq!(tracking_pylon_radius_and_mult(Some(&ctx.sde), MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID, 0.0), (40_000.0, 1.6));

    let ship_id = 740_000_001_i64;
    let sentry_id = 740_000_002_i64;
    let drone_id = 740_000_003_i64;
    let missile_id = 740_000_004_i64;
    let fighter_id = 740_000_005_i64;
    let rogue_npc_id = 740_000_006_i64;
    let trig_npc_id = 740_000_007_i64;
    let tessera_id = 740_000_008_i64;

    for (bid, tid, pos, max_vel) in [
        (ship_id, 621_u32, DVec3::new(10_000.0, 0.0, 0.0), 250.0),
        (sentry_id, 23_559, DVec3::new(10_500.0, 0.0, 0.0), 0.0), // Warden I Sentry Drone (group 100)
        (drone_id, 2456, DVec3::new(35_000.0, 0.0, 0.0), 400.0), // Hobgoblin I at 35km
        (missile_id, 204, DVec3::new(35_000.0, 100.0, 0.0), 2_000.0), // Inferno Cruise Missile at 35km
        (fighter_id, 23_057, DVec3::new(35_000.0, 200.0, 0.0), 800.0), // Dragonfly I Fighter at 35km
        (rogue_npc_id, 47_847, DVec3::new(35_000.0, 300.0, 0.0), 500.0), // Sparkneedle Tessella (group 1997) at 35km
        (trig_npc_id, 48_092, DVec3::new(12_000.0, 0.0, 0.0), 400.0), // Striking Damavik NPC (id >= 700M) at 12km
        (tessera_id, 48_262, DVec3::new(10_000.0, 200.0, 0.0), 300.0), // Blastlance Tessera cruiser spared at 10km
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
    assert_eq!(sys_h.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp, 5_000.0, "Small suppressor (15km) spares drone at 35km");
    assert_eq!(sys_h.get_combat_state(sentry_id).await.unwrap().unwrap().shield_hp, 5_000.0, "Suppressor spares stationary Sentry Drone at 10.5km");
    assert_eq!(sys_h.get_combat_state(tessera_id).await.unwrap().unwrap().shield_hp, 5_000.0, "Suppressor spares Tessera cruiser at 10km");
    assert_eq!(sys_h.get_combat_state(trig_npc_id).await.unwrap().unwrap().shield_hp, 5_000.0, "Suppressor spares Triglavian NPC at 12km");
    assert_eq!(sys_h.get_combat_state(ship_id).await.unwrap().unwrap().shield_hp, 5_000.0, "Suppressor spares player ship at 10km");
    sys_h.try_remove_ball(740_010_001);

    let _ = sys_h.spawn_ball(Ball {
        id: 740_010_002, type_id: AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: 40_000.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    let _ = pulse_abyssal_automated_pylons(&ctx, abyss_sys).await;
    assert!(sys_h.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp < 5_000.0, "Medium suppressor (SDE 40km) damages drone at 35km");
    assert!(sys_h.get_combat_state(missile_id).await.unwrap().unwrap().shield_hp < 5_000.0, "Medium suppressor damages missile at 35km");
    assert!(sys_h.get_combat_state(fighter_id).await.unwrap().unwrap().shield_hp < 5_000.0, "Medium suppressor damages fighter at 35km");
    assert!(sys_h.get_combat_state(rogue_npc_id).await.unwrap().unwrap().shield_hp < 5_000.0, "Medium suppressor damages rogue drone NPC at 35km");
    assert_eq!(sys_h.get_combat_state(sentry_id).await.unwrap().unwrap().shield_hp, 5_000.0, "Medium suppressor still spares Sentry Drone");

    let _ = sys_h.set_ball_position(drone_id, DVec3::new(42_000.0, 0.0, 0.0)).await;
    let hp_42 = sys_h.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp;
    let _ = pulse_abyssal_automated_pylons(&ctx, abyss_sys).await;
    assert_eq!(sys_h.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp, hp_42, "Medium suppressor (40km) spares drone at 42km");
    sys_h.try_remove_ball(740_010_002);

    let _ = sys_h.set_ball_position(drone_id, DVec3::new(38_000.0, 0.0, 0.0)).await;
    let _ = sys_h.spawn_ball(Ball {
        id: 740_010_003, type_id: AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: 40_000.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    let _ = pulse_abyssal_automated_pylons(&ctx, abyss_sys).await;
    assert!(sys_h.get_combat_state(drone_id).await.unwrap().unwrap().shield_hp < hp_42, "Large suppressor (40km) damages drone at 38km");
    sys_h.try_remove_ball(740_010_003);

    // Medium-Range Tracking Pylon (47470, 40km, +60% -> 1.6x) & Short-Range Tracking Pylon (47469, 15km, +80% -> 1.8x)
    let _ = sys_h.spawn_ball(Ball {
        id: 740_010_004, type_id: MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: 40_000.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    let _ = pulse_abyssal_automated_pylons(&ctx, abyss_sys).await;
    assert!((sys_h.get_combat_state(ship_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.6).abs() < 1e-6);
    assert!((sys_h.get_combat_state(sentry_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.6).abs() < 1e-6);
    assert!((sys_h.get_combat_state(trig_npc_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.6).abs() < 1e-6, "NPC ship receives 47470 tracking buff");
    let _ = sys_h.spawn_ball(Ball {
        id: 740_010_005, type_id: MULTIBODY_TRACKING_PYLON_TYPE_ID, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: 15_000.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    let _ = pulse_abyssal_automated_pylons(&ctx, abyss_sys).await;
    assert!((sys_h.get_combat_state(ship_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.8).abs() < 1e-6);
    assert!((sys_h.get_combat_state(trig_npc_id).await.unwrap().unwrap().tracking_speed_multiplier - 1.8).abs() < 1e-6, "NPC ship receives 47469 1.8x tracking buff");

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
    // BF-04: Suppressor killing last room Rogue Drone NPC (47847) unlocks Room 1 conduit & cleans npc_entities
    let p3_h = ctx.systems.get_or_spawn(p3_sys);
    let r1_npcs = with_pve_special_reg(&ctx, |reg| reg.abyssal_pockets[&char1].rooms[0].npc_ball_ids.clone());
    for &nid in &r1_npcs[1..] {
        let _ = p3_h.apply_damage_with_attacker(nid, DamageVector { em: 1e7, thermal: 1e7, kinetic: 1e7, explosive: 1e7 }, Some(char1)).await;
    }
    let last_npc = r1_npcs[0];
    let sup_pos = p3_h.get_ball_snapshot(1_899_000_000 + pid3 * 100 + 11).await.unwrap().unwrap().1.position;
    {
        let mut bp = p3_h.ballpark.write().unwrap();
        if let Some(b) = bp.balls.get_mut(&last_npc) { b.type_id = 47_847; b.position = sup_pos; }
        if let Some(cs) = bp.combat_states.get_mut(&last_npc) { cs.shield_hp = 0.0; cs.armor_hp = 0.0; cs.structure_hp = 10.0; }
    }
    let _ = pulse_abyssal_automated_pylons(&ctx, p3_sys).await;
    assert!(with_pve_special_reg(&ctx, |reg| reg.abyssal_pockets[&char1].rooms[0].cleared), "Suppressor kill unlocks room conduit");
    assert!(!eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| rt.npc_entities.contains_key(&last_npc)), "Suppressor kill cleans npc_entities");

    eve_server::services::pve_special_sites_abyssal::on_abyssal_cache_destroyed_core(&ctx.systems, &ctx.db, p3_sys, cache3_id, Some(char1)).await;
    ctx.db.with_snapshot(|s| {
        let wreck = s.items.values().find(|i| i.type_id == 48_120 && i.location_id == p3_sys as i64).expect("Triglavian Bioadaptive Wreck 48120 spawned");
        assert!(s.items.values().any(|i| i.location_id == wreck.item_id && i.quantity >= 4), "Loot transferred to wreck");
    });
    let _ = evacuate_abyssal_pockets_for_downtime(&ctx);

    // 3. Spatial hazard clouds (Bioluminescence, Filament Small/Medium/Large, Tachyon): SDE radii, numeric modifiers & exit restoration
    assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), BIOLUMINESCENT_CLOUD_SMALL_TYPE_ID, 0.0), 5_000.0);
    assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), BIOLUMINESCENT_CLOUD_TYPE_ID, 0.0), 15_000.0);
    assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), BIOLUMINESCENT_CLOUD_LARGE_TYPE_ID, 0.0), 40_000.0);
    assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), FILAMENT_CLOUD_SMALL_TYPE_ID, 0.0), 5_000.0);
    assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), FILAMENT_CLOUD_TYPE_ID, 0.0), 15_000.0);
    assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), FILAMENT_CLOUD_LARGE_TYPE_ID, 0.0), 40_000.0);
    assert_eq!(spatial_cloud_radius_m(Some(&ctx.sde), PROVING_TACHYON_CLOUD_TYPE_ID, 0.0), 40_000.0);

    let default_vel = ctx.sde.types.get(&621).and_then(|t| t.default_attributes.get(&37).copied()).unwrap_or(250.0);
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
    assert!((sys_h.get_combat_state(ship_id).await.unwrap().unwrap().signature_radius - 500.0).abs() < 1e-3);
    assert!((sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.max_velocity - default_vel).abs() < 1e-3);

    let _ = sys_h.set_ball_position(ship_id, DVec3::new(50_000.0, 50_000.0, 0.0)).await;
    assert_eq!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyss_sys, ship_id).await, Some("FilamentCloud"));
    let fil_cs = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((fil_cs.shield_boost_multiplier - 0.6).abs() < 1e-6);
    assert!((fil_cs.signature_radius - 125.0).abs() < 1e-3);

    let _ = sys_h.set_ball_position(ship_id, DVec3::new(-50_000.0, 50_000.0, 0.0)).await;
    assert_eq!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyss_sys, ship_id).await, Some("TachyonCloud"));
    let (_, tach_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((tach_ball.max_velocity - default_vel * 4.0).abs() < 1e-3);
    assert!((tach_ball.inertia - 0.25).abs() < 1e-3);
    assert!((sys_h.get_combat_state(ship_id).await.unwrap().unwrap().signature_radius - 500.0).abs() < 1e-3);

    let _ = sys_h.set_ball_position(ship_id, DVec3::new(0.0, 0.0, 0.0)).await;
    assert!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, abyss_sys, ship_id).await.is_none());
    let (_, exit_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let exit_cs = sys_h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((exit_ball.max_velocity - default_vel).abs() < 1e-3);
    assert!((exit_ball.inertia - 0.5).abs() < 1e-3);
    assert!((exit_cs.signature_radius - 125.0).abs() < 1e-3);
    assert!((exit_cs.shield_boost_multiplier - 1.0).abs() < 1e-6);

    // 4. Weather penalty RNG distribution (30%/50% for T0..=T3, 50%/70% for T4..=T6) & all 5 weather types across player & NPC ships
    let mut low_seed = None;
    let mut high_seed = None;
    for s in 1_u64..100 {
        ctx.set_mechanic_rng_seed(Some(s));
        if ctx.mechanic_rng.roll(0.5) {
            if high_seed.is_none() { high_seed = Some(s); }
        } else if low_seed.is_none() {
            low_seed = Some(s);
        }
        if low_seed.is_some() && high_seed.is_some() { break; }
    }
    let (low_seed, high_seed) = (low_seed.unwrap(), high_seed.unwrap());
    ctx.set_mechanic_rng_seed(Some(low_seed));
    assert_eq!(roll_abyssal_weather_penalty_pct(&ctx, 1), 30.0);
    ctx.set_mechanic_rng_seed(Some(high_seed));
    assert_eq!(roll_abyssal_weather_penalty_pct(&ctx, 1), 50.0);
    ctx.set_mechanic_rng_seed(Some(low_seed));
    assert_eq!(roll_abyssal_weather_penalty_pct(&ctx, 4), 50.0);
    ctx.set_mechanic_rng_seed(Some(high_seed));
    assert_eq!(roll_abyssal_weather_penalty_pct(&ctx, 4), 70.0);

    let base_ship_cs = ctx.build_ship_combat_state(char1, c_ship, 621);
    let fitted_base_vel = eve_server::services::pve_abyssal_pylons::build_fitted_ship_space_ball(&ctx, char1, c_ship, 621, DVec3::ZERO).max_velocity;
    let base_opt = ctx.sde.types.get(&621).and_then(|t| t.default_attributes.get(&54).copied()).unwrap_or(15_000.0);
    let base_scan = ctx.sde.types.get(&621).and_then(|t| t.default_attributes.get(&564).copied()).unwrap_or(250.0);
    let weather_cases = [
        (47_765_u32, AbyssalWeatherKind::Electrical, low_seed, 30.0_f64),
        (47_762, AbyssalWeatherKind::Dark, high_seed, 50.0),
        (47_761, AbyssalWeatherKind::Exotic, low_seed, 30.0),
        (47_763, AbyssalWeatherKind::Firestorm, high_seed, 50.0),
        (47_764, AbyssalWeatherKind::Gamma, low_seed, 30.0),
        (47_906, AbyssalWeatherKind::Electrical, high_seed, 70.0),
        (47_894, AbyssalWeatherKind::Dark, high_seed, 70.0),
        (47_890, AbyssalWeatherKind::Exotic, high_seed, 70.0),
        (47_898, AbyssalWeatherKind::Firestorm, high_seed, 70.0),
        (47_902, AbyssalWeatherKind::Gamma, high_seed, 70.0),
    ];
    for (idx, (fil_tid, weather, seed, exp_pen)) in weather_cases.into_iter().enumerate() {
        move_pilot_to_system(&ctx, &mut sess1, char1, c_ship, 621, SYS_LOW_TAMA, DVec3::new((idx as f64 + 1.0) * 6_000_000.0, 2.0e9, 0.0)).await;
        ctx.set_mechanic_rng_seed(Some(seed));
        let fiid = 740_200_000 + idx as i64;
        seed_cargo_item(&ctx, fiid, fil_tid, char1, c_ship, 1);
        let d_w = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(fiid)], &kw, None).await.expect("dep w");
        let t_w = d_w.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
        let e_w = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(t_w)], &kw, None).await.expect("ent w");
        let w_sys = e_w.dict_get("solarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
        let w_h = ctx.systems.get_or_spawn(w_sys);
        let npc_id = with_pve_special_reg(&ctx, |reg| {
            let p = &reg.abyssal_pockets[&char1];
            assert_eq!(p.weather, weather);
            assert_eq!(p.weather_penalty_pct, exp_pen);
            p.rooms[0].npc_ball_ids[0]
        });
        let (_, s_ball) = w_h.get_ball_snapshot(c_ship).await.unwrap().unwrap();
        let s_cs = w_h.get_combat_state(c_ship).await.unwrap().unwrap();
        let (_, n_ball) = w_h.get_ball_snapshot(npc_id).await.unwrap().unwrap();
        let n_cs = w_h.get_combat_state(npc_id).await.unwrap().unwrap();
        let n_base_cs = ctx.build_type_combat_state(n_ball.type_id);
        let n_base_vel = ctx.sde.types.get(&n_ball.type_id).and_then(|t| t.default_attributes.get(&37).copied()).filter(|&v| v > 0.0).unwrap_or(380.0);
        let pen = exp_pen / 100.0;
        let dyn_attrs = ctx.db.with_snapshot(|s| s.items[&c_ship].dynamic_attributes.clone());
        let pen_res = |r: f64| (r * (1.0 + pen)).clamp(0.0, 1.0);
        match weather {
            AbyssalWeatherKind::Electrical => {
                assert!((s_cs.shield_resonance[0] - pen_res(base_ship_cs.shield_resonance[0])).abs() < 1e-4);
                assert!((s_cs.armor_resonance[0] - pen_res(base_ship_cs.armor_resonance[0])).abs() < 1e-4);
                assert!((s_cs.structure_resonance[0] - pen_res(base_ship_cs.structure_resonance[0])).abs() < 1e-4);
                assert!((s_cs.capacitor_recharge_time_ms - (base_ship_cs.capacitor_recharge_time_ms * 0.5).max(1_000.0)).abs() < 1.0);
                assert!((n_cs.armor_resonance[0] - pen_res(n_base_cs.armor_resonance[0])).abs() < 1e-4);
            }
            AbyssalWeatherKind::Dark => {
                assert!((s_ball.max_velocity - fitted_base_vel * 1.5).abs() < 1e-3);
                assert!((dyn_attrs[&54] - base_opt * (1.0 - pen)).abs() < 1.0);
                assert!((n_ball.max_velocity - n_base_vel * 1.5).abs() < 1e-3);
            }
            AbyssalWeatherKind::Exotic => {
                assert!((s_cs.shield_resonance[2] - pen_res(base_ship_cs.shield_resonance[2])).abs() < 1e-4);
                assert!((s_cs.armor_resonance[2] - pen_res(base_ship_cs.armor_resonance[2])).abs() < 1e-4);
                assert!((s_cs.structure_resonance[2] - pen_res(base_ship_cs.structure_resonance[2])).abs() < 1e-4);
                assert!((dyn_attrs[&564] - base_scan * 1.5).abs() < 1e-3);
                assert!((n_cs.armor_resonance[2] - pen_res(n_base_cs.armor_resonance[2])).abs() < 1e-4);
            }
            AbyssalWeatherKind::Firestorm => {
                assert!((s_cs.max_armor_hp - base_ship_cs.max_armor_hp * 1.5).abs() < 1.0);
                assert!((s_cs.armor_resonance[1] - pen_res(base_ship_cs.armor_resonance[1])).abs() < 1e-4);
                assert!((s_cs.structure_resonance[1] - pen_res(base_ship_cs.structure_resonance[1])).abs() < 1e-4);
                assert!((n_cs.armor_resonance[1] - pen_res(n_base_cs.armor_resonance[1])).abs() < 1e-4);
            }
            AbyssalWeatherKind::Gamma => {
                assert!((s_cs.max_shield_hp - base_ship_cs.max_shield_hp * 1.5).abs() < 1.0);
                assert!((s_cs.shield_resonance[3] - pen_res(base_ship_cs.shield_resonance[3])).abs() < 1e-4);
                assert!((s_cs.structure_resonance[3] - pen_res(base_ship_cs.structure_resonance[3])).abs() < 1e-4);
                assert!((n_cs.shield_resonance[3] - pen_res(n_base_cs.shield_resonance[3])).abs() < 1e-4);
            }
        }
        let _ = evacuate_abyssal_pockets_for_downtime(&ctx);
        assert!(ctx.db.with_snapshot(|s| s.items[&c_ship].dynamic_attributes.is_empty()), "Ship attributes restored on exit");
    }

    // BF-03: Live Gunnery turret range penalty in Dark weather (-50%: ~44.9km skilled -> ~22.4km, so 30km fails in Dark and succeeds after exit)
    move_pilot_to_system(&ctx, &mut sess1, char1, c_ship, 624, SYS_LOW_TAMA, DVec3::new(90_000_000.0, 2.0e9, 0.0)).await;
    let (turret_id, ammo_id) = (740_300_001_i64, 740_300_002_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(turret_id, InventoryItemRecord { item_id: turret_id, type_id: 487, owner_id: char1, location_id: c_ship, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default() });
        snap.items.insert(ammo_id, InventoryItemRecord { item_id: ammo_id, type_id: 182, owner_id: char1, location_id: c_ship, flag: InventoryFlag::HiSlot0, quantity: 40, singleton: false, version: 1, ..Default::default() });
        snap.module_online_states.insert(turret_id, true);
    });
    ctx.invalidate_ship_fitting(c_ship);
    ctx.set_mechanic_rng_seed(Some(low_seed)); // T5 Dark -> -50% penalty
    seed_cargo_item(&ctx, 740_300_003, 47_894, char1, c_ship, 1);
    let d_dk = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(740_300_003)], &kw, None).await.expect("dep dark");
    let e_dk = ctx.dispatch_rpc(&mut sess1, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(d_dk.dict_get("traceBallID").unwrap().as_i64().unwrap())], &kw, None).await.expect("ent dark");
    let dk_sys = e_dk.dict_get("solarSystemID").unwrap().as_i64().unwrap() as u32;
    let dk_npc = e_dk.dict_get("npcBallIDs").and_then(|v| if let PyRep::List(l) = v { l[0].as_i64() } else { None }).unwrap();
    let dk_h = ctx.systems.get_or_spawn(dk_sys);
    let _ = dk_h.set_ball_position(c_ship, DVec3::ZERO).await;
    let _ = dk_h.set_ball_position(dk_npc, DVec3::new(30_000.0, 0.0, 0.0)).await;
    let _ = dk_h.add_target_lock(c_ship, eve_destiny::TargetLock { target_id: dk_npc, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await;
    let oor = eve_server::services::combat_crimewatch::dispatch_combat_rpc(&ctx, &mut sess1, "dogmaIM", "Activate", &[PyRep::Integer(turret_id), PyRep::String("projectileFired".into()), PyRep::Integer(dk_npc)], &kw).await.unwrap_err();
    assert_eq!(oor.code, "TargetOutOfRange", "Dark weather -50% turret optimal/falloff blocks shot at 30km");
    let _ = ctx.dispatch_rpc(&mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(turret_id), PyRep::String("projectileFired".into()), PyRep::Integer(dk_npc)], &kw, None).await;
    let _ = dk_h.try_step_full_tick(1.0);
    assert!(!dk_h.has_module_cycle(c_ship, turret_id).await, "Module cycle auto-deactivates beyond Dark-penalized range");
    let _ = evacuate_abyssal_pockets_for_downtime(&ctx);
    assert!(ctx.db.with_snapshot(|s| s.items[&turret_id].dynamic_attributes.is_empty()), "Turret dynamic_attributes restored on exit");
    move_pilot_to_system(&ctx, &mut sess1, char1, c_ship, 624, SYS_LOW_TAMA, DVec3::ZERO).await;
    let tama_h = ctx.systems.get_or_spawn(SYS_LOW_TAMA);
    let dummy_t = 740_300_099_i64;
    let _ = tama_h.spawn_ball_with_combat(Ball { id: dummy_t, type_id: 48_090, position: DVec3::new(30_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_type_combat_state(48_090)).await;
    let _ = tama_h.add_target_lock(c_ship, eve_destiny::TargetLock { target_id: dummy_t, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }).await;
    eve_server::services::combat_crimewatch::dispatch_combat_rpc(&ctx, &mut sess1, "dogmaIM", "Activate", &[PyRep::Integer(turret_id), PyRep::String("projectileFired".into()), PyRep::Integer(dummy_t)], &kw).await.expect("Restored turret hits 30km outside Dark weather");
}
