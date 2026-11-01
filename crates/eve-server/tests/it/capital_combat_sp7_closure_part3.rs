//! Integration tests for Bosonic/GTFO/Reaper Doomsdays, Ice Refinery Heist,
//! and Deadspace Pocket Rules:
//! - `feat:modules.doomsday-bosonic-field`
//! - `feat:modules.doomsday-gtfo-reaper`
//! - `feat:pve.insurgency-ice-heist`
//! - `feat:pve.deadspace-pocket-rules`

use crate::common::*;
use eve_db::{InventoryFlag, InventoryItemRecord, PersistedInsurgencyCampaign};
use eve_destiny::{MovementMode, WarpPhase};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::pve_dungeon_runtime::{
    with_dungeon_runtime_store, DungeonRuntimeInstance, DungeonRuntimeObject, DungeonRuntimeRoom,
};
use eve_server::services::pve_incursion_sites::make_site_npc_ball;
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

fn py_keyval_to_map(rep: &PyRep) -> HashMap<String, PyRep> {
    let mut out = HashMap::new();
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            out.insert(k.clone(), v.clone());
        }
    }
    out
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
async fn test_doomsday_bosonic_cone_and_gtfo_reaper() {
    // parity: feat:modules.doomsday-bosonic-field, feat:modules.doomsday-gtfo-reaper
    eve_testkit::attest(&[
        "feat:modules.doomsday-bosonic-field",
        "feat:modules.doomsday-gtfo-reaper",
    ]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let highsec_sys = 30_000_142; // Jita (0.95 highsec)
    let lowsec_sys = 30_002_537; // Amamake (0.44 lowsec)
    let nullsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0 && s.solar_system_id < 31_000_000 && s.region_id != 10_000_070)
        .map(|s| s.solar_system_id)
        .expect("SDE must contain a K-space nullsec system");
    let char_id = 90_001;
    let titan_id = 88_100_001;
    let dd_mod_id = 88_100_002; // 40633 Bosonic Field Generator (doomsdayConeDOT)
    let gtfo_mod_id = 88_100_003; // 40634 Gravitational Transportation Field Oscillator (doomsdayHOG)
    let reaper_mod_id = 88_100_004; // 40632 'Divine Harvest' Electromagnetic Reaper (doomsdaySlash)
    let helium_id = 88_100_005;
    let stront_id = 88_100_006;

    let slash_vec = |mid: i64, x: f64, y: f64, z: f64| {
        vec![PyRep::Integer(mid), PyRep::Real(x), PyRep::Real(y), PyRep::Real(z)]
    };

    // 1. Negative: non-Titan hull refused
    seed_ship_in_space(&ctx, char_id, titan_id, 587, nullsec_sys, DVec3::ZERO).await;
    ctx.db.with_mut_snapshot(|snap| {
        for (mid, tid, flg) in [
            (dd_mod_id, 40_633, InventoryFlag::HiSlot0),
            (gtfo_mod_id, 40_634, InventoryFlag::HiSlot1),
            (reaper_mod_id, 40_632, InventoryFlag::HiSlot2),
        ] {
            snap.items.insert(mid, InventoryItemRecord {
                item_id: mid, type_id: tid, owner_id: char_id, location_id: titan_id,
                flag: flg, quantity: 1, singleton: true, ..Default::default()
            });
            snap.module_online_states.insert(mid, true);
        }
    });
    let mut sess = make_sess(char_id, titan_id, nullsec_sys, None);
    let err = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSlashModule", slash_vec(dd_mod_id, 1.0, 0.0, 0.0)).await.unwrap_err();
    assert_eq!(err.code, "ShipCannotActivateSuperweapon");

    // 2. Negative: BOTH Highsec (0.95) AND Lowsec (0.44) Empire space refused (`disallowInEmpireSpace`)
    for emp_sys in [highsec_sys, lowsec_sys] {
        seed_ship_in_space(&ctx, char_id, titan_id, 11_567, emp_sys, DVec3::ZERO).await;
        let mut emp_sess = make_sess(char_id, titan_id, emp_sys, None);
        let emp_err = call_svc(&ctx, &mut emp_sess, "superWeaponMgr", "ActivateSlashModule", slash_vec(dd_mod_id, 1.0, 0.0, 0.0)).await.unwrap_err();
        assert_eq!(emp_err.code, "CannotActivateInEmpireSpace");
    }

    // 3. Negative: docked, cloaked, warping, and missing-fuel checks in Nullsec
    seed_ship_in_space(&ctx, char_id, titan_id, 11_567, nullsec_sys, DVec3::ZERO).await;
    ctx.db.with_mut_snapshot(|s| { if let Some(c) = s.characters.get_mut(&char_id) { c.station_id = Some(60_003_760); } });
    let mut docked_sess = make_sess(char_id, titan_id, nullsec_sys, Some(60_003_760));
    let docked_err = call_svc(&ctx, &mut docked_sess, "superWeaponMgr", "ActivateSlashModule", slash_vec(dd_mod_id, 1.0, 0.0, 0.0)).await.unwrap_err();
    assert_eq!(docked_err.code, "CantDoThatWhileDocked");
    ctx.db.with_mut_snapshot(|s| { if let Some(c) = s.characters.get_mut(&char_id) { c.station_id = None; } });

    let sys = ctx.systems.get_or_spawn(nullsec_sys);
    let mut t_cs = sys.get_combat_state(titan_id).await.unwrap().unwrap();
    t_cs.is_cloaked = true;
    let _ = sys.set_combat_state(titan_id, t_cs.clone()).await;
    let cloak_err = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSlashModule", slash_vec(dd_mod_id, 1.0, 0.0, 0.0)).await.unwrap_err();
    assert_eq!(cloak_err.code, "CantActivateWhileCloaked");
    t_cs.is_cloaked = false;
    let _ = sys.set_combat_state(titan_id, t_cs).await;

    let _ = sys.set_movement_mode(titan_id, MovementMode::Warp {
        origin: DVec3::ZERO, destination: DVec3::new(1.0e7, 0.0, 0.0), warp_speed_au_s: 1.5,
        total_distance: 1.0e7, phase: WarpPhase::Accelerating, elapsed_warp_time: 0.0,
    }).await;
    let warp_err = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSlashModule", slash_vec(dd_mod_id, 1.0, 0.0, 0.0)).await.unwrap_err();
    assert_eq!(warp_err.code, "CantActivateInWarp");
    let _ = sys.set_movement_mode(titan_id, MovementMode::Stop).await;

    let err_fuel = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSlashModule", slash_vec(dd_mod_id, 1.0, 0.0, 0.0)).await.unwrap_err();
    assert_eq!(err_fuel.code, "InsufficientFuel");

    // 4. Seed SDE Strontium Clathrates (16275) & Helium Isotopes (16274) + targets
    ctx.db.with_mut_snapshot(|snap| {
        for (fid, tid, qty) in [(stront_id, 16_275, 10_000), (helium_id, 16_274, 150_000)] {
            snap.items.insert(fid, InventoryItemRecord {
                item_id: fid, type_id: tid, owner_id: char_id, location_id: titan_id,
                flag: InventoryFlag::Cargo, quantity: qty, ..Default::default()
            });
        }
    });

    let neut_ship_id = 88_100_009;
    let cone_frig_id = 88_100_010;
    let cone_cap_id = 88_100_017;
    let cone_miss_id = 88_100_011;
    let cone_tethered_id = 88_100_014;
    let gtfo_hit_id = 88_100_012;
    let gtfo_cap_id = 88_100_018;
    let gtfo_tethered_id = 88_100_015;
    let gtfo_struct_id = 88_100_016;
    let reaper_hit_id = 88_100_013;

    seed_ship_in_space(&ctx, 90_011, neut_ship_id, 638, nullsec_sys, DVec3::new(0.0, 8_000.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_002, cone_frig_id, 587, nullsec_sys, DVec3::new(20_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_012, cone_cap_id, 19_720, nullsec_sys, DVec3::new(22_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_003, cone_miss_id, 587, nullsec_sys, DVec3::new(0.0, 20_000.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_006, cone_tethered_id, 587, nullsec_sys, DVec3::new(15_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_004, gtfo_hit_id, 587, nullsec_sys, DVec3::new(-40_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_013, gtfo_cap_id, 19_720, nullsec_sys, DVec3::new(-39_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_007, gtfo_tethered_id, 587, nullsec_sys, DVec3::new(-38_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_005, reaper_hit_id, 638, nullsec_sys, DVec3::new(0.0, -20_000.0, 18_000.0)).await;

    for tid in [cone_frig_id, cone_cap_id] {
        let mut cs = sys.get_combat_state(tid).await.unwrap().unwrap();
        cs.shield_hp = 500_000.0;
        cs.max_shield_hp = 500_000.0;
        cs.shield_resonance = [1.0, 1.0, 1.0, 1.0];
        let _ = sys.set_combat_state(tid, cs).await;
    }
    for tid in [cone_tethered_id, gtfo_tethered_id] {
        let mut cs = sys.get_combat_state(tid).await.unwrap().unwrap();
        cs.is_tethered = true;
        let _ = sys.set_combat_state(tid, cs).await;
    }
    let mut struct_ball = make_site_npc_ball(gtfo_struct_id, 35_832, DVec3::new(-37_000.0, 0.0, 0.0));
    struct_ball.max_velocity = 0.0;
    let _ = sys.spawn_ball(struct_ball).await;

    // 5. Activate Bosonic Field Generator -> 10km 35k GJ neut pulse, 10s warmup (0 dmg at t=0), then sig-scaled DoT
    assert!(sys.get_combat_state(neut_ship_id).await.unwrap().unwrap().capacitor > 0.0);
    let pre_tether_cs = sys.get_combat_state(cone_tethered_id).await.unwrap().unwrap();
    let cone_res = call_svc(
        &ctx,
        &mut sess,
        "superWeaponMgr",
        "ActivateSlashModule",
        vec![PyRep::Integer(dd_mod_id), PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap();
    let cone_kv = py_keyval_to_map(&cone_res);
    assert_eq!(cone_kv.get("mode").and_then(|v| v.as_str()), Some("Cone"));
    assert_eq!(cone_kv.get("status").and_then(|v| v.as_str()), Some("WarmingUp"));
    assert_eq!(cone_kv.get("fuelConsumed").and_then(|v| v.as_i64()), Some(2_000));
    assert_eq!(sys.get_combat_state(neut_ship_id).await.unwrap().unwrap().capacitor, 0.0, "10km superweapon neut pulse must drain nearby ship capacitor");
    assert_eq!(sys.get_combat_state(cone_frig_id).await.unwrap().unwrap().shield_hp, 500_000.0, "No Bosonic damage before 10s warmup");

    let refire_err = call_svc(
        &ctx,
        &mut sess,
        "superWeaponMgr",
        "ActivateSlashModule",
        vec![PyRep::Integer(dd_mod_id), PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(refire_err.code, "ModuleInReactivationDelay");
    assert_eq!(sys.get_ball_snapshot(titan_id).await.unwrap().unwrap().1.max_velocity, 0.0);
    assert!(sys.get_combat_state(titan_id).await.unwrap().unwrap().disallow_assistance);

    ctx.advance_secs(10);
    let _ = sys.tick().await;
    let frig_dmg = 500_000.0 - sys.get_combat_state(cone_frig_id).await.unwrap().unwrap().shield_hp;
    let cap_dmg = 500_000.0 - sys.get_combat_state(cone_cap_id).await.unwrap().unwrap().shield_hp;
    assert!(frig_dmg > 0.0 && cap_dmg > frig_dmg * 5.0, "Bosonic damage must scale with target signatureRadius (cap={cap_dmg}, frig={frig_dmg})");
    assert_eq!(sys.get_combat_state(cone_tethered_id).await.unwrap().unwrap().shield_hp, pre_tether_cs.shield_hp);
    ctx.advance_secs(20);
    let _ = sys.tick().await;

    // 6. GTFO: range/NaN checks, 15s spool delay, subcapital-only filter (Dreadnought/tethered/structure excluded), and 60s warp suppression
    let oor_err = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSinglePointTargetedModule", slash_vec(gtfo_mod_id, 900_000.0, 0.0, 0.0)).await.unwrap_err();
    assert_eq!(oor_err.code, "TargetOutOfRange");
    let nan_err = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSinglePointTargetedModule", slash_vec(gtfo_mod_id, f64::NAN, 0.0, 0.0)).await.unwrap_err();
    assert_eq!(nan_err.code, "InvalidTargetPoint");
    let gtfo_res = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSinglePointTargetedModule", slash_vec(gtfo_mod_id, -35_000.0, 0.0, 0.0)).await.unwrap();
    let gtfo_kv = py_keyval_to_map(&gtfo_res);
    assert_eq!(gtfo_kv.get("status").and_then(|v| v.as_str()), Some("WarmingUp"));
    assert!(sys.get_ball_snapshot(gtfo_hit_id).await.unwrap().unwrap().1.position.distance(DVec3::new(-40_000.0, 0.0, 0.0)) < 1.0);

    ctx.advance_secs(15);
    let _ = sys.tick().await;
    assert!(sys.get_ball_snapshot(gtfo_hit_id).await.unwrap().unwrap().1.position.distance(DVec3::new(-40_000.0, 0.0, 0.0)) > 500_000.0);
    assert!(sys.get_combat_state(gtfo_hit_id).await.unwrap().unwrap().warp_scramble_strength >= 25, "GTFO must apply 60s warp suppression");
    assert!(sys.get_ball_snapshot(gtfo_cap_id).await.unwrap().unwrap().1.position.distance(DVec3::new(-39_000.0, 0.0, 0.0)) < 1.0, "Capital ships must not be teleported by GTFO");
    assert!(sys.get_ball_snapshot(gtfo_tethered_id).await.unwrap().unwrap().1.position.distance(DVec3::new(-38_000.0, 0.0, 0.0)) < 1.0);
    assert!(sys.get_ball_snapshot(gtfo_struct_id).await.unwrap().unwrap().1.position.distance(DVec3::new(-37_000.0, 0.0, 0.0)) < 1.0);

    ctx.advance_secs(61);
    let _ = sys.tick().await;
    assert_eq!(sys.get_combat_state(gtfo_hit_id).await.unwrap().unwrap().warp_scramble_strength, 0, "GTFO warp suppression must expire after 60s");

    // 7. Reaper (40632): >100km arc cap rejected, then 10s warmup + 10s progressive sweep
    let arc_err = call_svc(
        &ctx,
        &mut sess,
        "superWeaponMgr",
        "ActivateSlashModule",
        vec![
            PyRep::Integer(reaper_mod_id),
            PyRep::Real(0.0), PyRep::Real(-20_000.0), PyRep::Real(-60_000.0),
            PyRep::Real(0.0), PyRep::Real(-20_000.0), PyRep::Real(60_000.0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(arc_err.code, "SweepArcTooLong");

    let pre_reaper_hp = sys.get_combat_state(reaper_hit_id).await.unwrap().unwrap().shield_hp;
    let reaper_res = call_svc(
        &ctx,
        &mut sess,
        "superWeaponMgr",
        "ActivateSlashModule",
        vec![
            PyRep::Integer(reaper_mod_id),
            PyRep::Real(0.0), PyRep::Real(-20_000.0), PyRep::Real(0.0),
            PyRep::Real(0.0), PyRep::Real(-20_000.0), PyRep::Real(20_000.0),
        ],
    )
    .await
    .unwrap();
    let reaper_kv = py_keyval_to_map(&reaper_res);
    assert_eq!(reaper_kv.get("fuelConsumed").and_then(|v| v.as_i64()), Some(50_000));
    assert_eq!(sys.get_combat_state(reaper_hit_id).await.unwrap().unwrap().shield_hp, pre_reaper_hp, "Reaper must not deal instant damage before warmup");

    ctx.advance_secs(12);
    let _ = sys.tick().await;
    assert_eq!(
        sys.get_combat_state(reaper_hit_id).await.unwrap().unwrap().shield_hp,
        pre_reaper_hp,
        "Target near end of Reaper arc (z=18km) must not be hit during early sweep ticks (z=0..6km)"
    );
    ctx.advance_secs(8);
    let _ = sys.tick().await;
    assert!(sys.get_combat_state(reaper_hit_id).await.unwrap().is_none_or(|c| c.shield_hp < pre_reaper_hp));

    ctx.advance_secs(10);
    let _ = sys.tick().await;
    assert!(sys.get_ball_snapshot(titan_id).await.unwrap().unwrap().1.max_velocity > 0.0);
    assert!(!sys.get_combat_state(titan_id).await.unwrap().unwrap().disallow_assistance);

    // 8. Negative abuse checks: offline module & unfitted cargo module refused
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(reaper_mod_id, false); });
    let offline_err = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSlashModule", vec![PyRep::Integer(reaper_mod_id), PyRep::Real(0.0), PyRep::Real(-20_000.0), PyRep::Real(0.0)]).await.unwrap_err();
    assert_eq!(offline_err.code, "ModuleNotOnline");

    ctx.db.with_mut_snapshot(|snap| { if let Some(m) = snap.items.get_mut(&reaper_mod_id) { m.flag = InventoryFlag::Cargo; } });
    let cargo_err = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSlashModule", vec![PyRep::Integer(reaper_mod_id), PyRep::Real(0.0), PyRep::Real(-20_000.0), PyRep::Real(0.0)]).await.unwrap_err();
    assert_eq!(cargo_err.code, "ModuleNotFitted");
}

#[tokio::test]
async fn test_insurgency_ice_heist_lifecycle() {
    // parity: feat:pve.insurgency-ice-heist
    eve_testkit::attest(&["feat:pve.insurgency-ice-heist"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let sys_id = 30_002_537;
    let char_id = 90_001;
    let ship_id = 88_200_001;
    let bystander_id = 90_009;
    let bystander_ship = 88_200_009;
    seed_ship_in_space(&ctx, char_id, ship_id, 587, sys_id, DVec3::new(155_000.0, 0.0, 150_000.0)).await;
    seed_ship_in_space(&ctx, bystander_id, bystander_ship, 587, sys_id, DVec3::new(155_000.0, 0.0, 152_000.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_001); // Caldari State Protectorate
        }
        snap.insurgency_runtime.campaigns.insert(
            1,
            PersistedInsurgencyCampaign {
                campaign_id: 1,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: 0,
                forecasting_end_unix: 0,
                active_end_unix: 100_000,
                winddown_end_unix: 200_000,
                corruption_points: HashMap::from([(sys_id, 0)]),
                suppression_points: HashMap::new(),
            },
        );
    });
    let mut sess = make_sess(char_id, ship_id, sys_id, None);
    sess.role = 1 << 29; // ROLE_QA without ROLE_GM_MASK

    // 0. Negative: subrole ROLE_QA without ROLE_GM_MASK is refused on slash.SlashCmd with InsufficientRole
    let slash_err = call_svc(&ctx, &mut sess, "slash", "SlashCmd", vec![PyRep::String("/heal".into())])
        .await
        .unwrap_err();
    assert_eq!(slash_err.code, "InsufficientRole");

    // 1. Negative: corruption stage < 3 does not spawn Ice Heist on 1 Hz tick
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.tick().await;
    let PyRep::List(pre_list) = call_svc(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", vec![])
        .await
        .unwrap()
    else {
        panic!("expected List");
    };
    assert!(pre_list.is_empty(), "Ice Heist must not spawn below corruption stage 3");

    // 2. Advance corruption to stage 3 (600 pts) via live `add_system_corruption` and verify 1 Hz tick materializes balls
    eve_server::services::insurgency_services::add_system_corruption(&ctx, 1, sys_id, 600);
    let _ = sys.tick().await;
    let PyRep::List(active_list) = call_svc(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", vec![])
        .await
        .unwrap()
    else {
        panic!("expected List");
    };
    assert_eq!(active_list.len(), 1);
    let inst_id = active_list[0].dict_get("instanceID").and_then(|v| v.as_i64()).unwrap();

    let tanker_id = 1_896_000_000 + inst_id * 10 + 1;
    let guard_id = 1_896_000_000 + inst_id * 10 + 2;
    let tanker_cs = sys.get_combat_state(tanker_id).await.unwrap().expect("Tanker ball must materialize in space");
    assert!(tanker_cs.warp_core_strength >= 100 && tanker_cs.is_web_immune);
    assert!(sys.get_ball_snapshot(guard_id).await.unwrap().is_some(), "Thunderchild guard (79986) must materialize in space");

    // Spawn a pirate cruiser (621) and pirate capsule (670) within 25km of Thunderchild guard (150_000, 0, 190_000)
    let pirate_char_id = 90_002;
    let pirate_ship_id = 88_200_002;
    let pod_char_id = 90_014;
    let pirate_pod_id = 88_200_014;
    seed_ship_in_space(&ctx, pirate_char_id, pirate_ship_id, 621, sys_id, DVec3::new(150_000.0, 0.0, 195_000.0)).await;
    seed_ship_in_space(&ctx, pod_char_id, pirate_pod_id, 670, sys_id, DVec3::new(150_000.0, 0.0, 196_000.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        for pid in [pirate_char_id, pod_char_id] {
            if let Some(ch) = snap.characters.get_mut(&pid) {
                ch.war_faction_id = Some(500_010);
            }
        }
    });
    let pre_pirate_hp = sys.get_combat_state(pirate_ship_id).await.unwrap().unwrap().shield_hp;

    // 3. Verify 1 tick deals weapon damage (NOT instant 1-tick deletion), then 80 ticks destroy pirate cruiser while sparing capsule
    let _ = sys.tick().await;
    let after_one_cs = sys.get_combat_state(pirate_ship_id).await.unwrap().expect("Thunderchild guard must apply weapon DPS, not 1-tick instant deletion");
    assert!(after_one_cs.shield_hp < pre_pirate_hp);

    for _ in 0..80 {
        let _ = sys.tick().await;
    }
    assert!(sys.get_ball_snapshot(pirate_ship_id).await.unwrap().is_none(), "Thunderchild guard weapon DPS must destroy nearby pirate ship");
    assert!(sys.get_ball_snapshot(pirate_pod_id).await.unwrap().is_some(), "Thunderchild guard must not execute capsules (670)");

    let PyRep::List(post_list) = call_svc(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", vec![])
        .await
        .unwrap()
    else {
        panic!("expected List");
    };
    assert_eq!(post_list[0].dict_get("completed"), Some(&PyRep::Bool(true)));
    assert_eq!(post_list[0].dict_get("empireTankersSaved"), Some(&PyRep::Integer(10)));

    let (militia_lp, bystander_lp) = ctx.db.with_snapshot(|snap| {
        (
            snap.characters.get(&char_id).and_then(|c| c.loyalty_points.get(&1_000_180).copied()).unwrap_or(0),
            snap.characters.get(&bystander_id).map(|c| c.loyalty_points.values().sum::<i64>()).unwrap_or(0),
        )
    });
    assert_eq!(militia_lp, 150_000, "Winning militia escort must earn 150,000 LP");
    assert_eq!(bystander_lp, 0, "Unaligned bystander must not earn Ice Heist LP");

    // 4. Verify 4h respawn cooldown blocks new Ice Heist on 1 Hz tick until 14,400s elapse
    let _ = sys.tick().await;
    let PyRep::List(cd_list) = call_svc(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", vec![])
        .await
        .unwrap()
    else {
        panic!("expected List");
    };
    assert_eq!(cd_list.len(), 1, "4-hour cooldown must block immediate Ice Heist respawn");
    ctx.advance_secs(14_401);
    let _ = sys.tick().await;
    let PyRep::List(respawn_list) = call_svc(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", vec![])
        .await
        .unwrap()
    else {
        panic!("expected List");
    };
    assert_eq!(respawn_list.len(), 2, "New Ice Heist instance must spawn after 4h cooldown expires");
}

#[tokio::test]
async fn test_deadspace_pocket_rules_and_mwd_disallowance() {
    // parity: feat:pve.deadspace-pocket-rules
    eve_testkit::attest(&["feat:pve.deadspace-pocket-rules"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let sys_id = 30_000_142;
    let char_id = 90_001;
    let ship_id = 88_300_001;
    let mwd_id = 88_300_002;
    let gate1_id = 88_300_010;
    let gate2_id = 88_300_011;
    let room2_obj_id = 88_300_012;

    let entry_gate_pos = [500_000.0, 0.0, 500_000.0];
    let room1_dest = [600_000.0, 0.0, 600_000.0];
    let room2_dest = [800_000.0, 0.0, 800_000.0];

    seed_ship_in_space(&ctx, char_id, ship_id, 587, sys_id, DVec3::from_array(entry_gate_pos)).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mwd_id,
            InventoryItemRecord {
                item_id: mwd_id,
                type_id: 434, // 5MN Microwarpdrive I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(
            777,
            DungeonRuntimeInstance {
                instance_id: 777,
                dungeon_id: 9001,
                dungeon_name: "Gated Deadspace Pocket".into(),
                archetype: "Combat".into(),
                category: "Combat".into(),
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: entry_gate_pos,
                entry_gate_ball_id: Some(gate1_id),
                entry_gate_pos: Some(entry_gate_pos),
                entry_gate_locked: false,
                active_objective_id: 0,
                completed_task_ids: Vec::new(),
                last_completed_task_id: None,
                current_room_index: 0,
                disallow_mwd: false,
                completed: false,
                despawned: false,
                created_unix: 0,
                rooms: vec![
                    DungeonRuntimeRoom {
                        room_index: 1,
                        room_id: 1,
                        room_name: "Room 1".into(),
                        position: room1_dest,
                        gate_destination_pos: room1_dest,
                        gate_ball_id: Some(gate2_id),
                        gate_locked: false,
                        disallow_mwd: Some(false),
                        gated: true,
                        cleared: true,
                        spawned: true,
                        npc_groups: Vec::new(),
                        objects: Vec::new(),
                        triggers: Vec::new(),
                    },
                    DungeonRuntimeRoom {
                        room_index: 2,
                        room_id: 2,
                        room_name: "Room 2 (No MWD)".into(),
                        position: room2_dest,
                        gate_destination_pos: room2_dest,
                        gate_ball_id: None,
                        gate_locked: false,
                        disallow_mwd: Some(true),
                        gated: true,
                        cleared: false,
                        spawned: true,
                        npc_groups: Vec::new(),
                        objects: vec![DungeonRuntimeObject {
                            ball_id: room2_obj_id,
                            type_id: 23,
                            name: "Overseer Structure".into(),
                            position: room2_dest,
                            radius: 100.0,
                            is_container: true,
                            spawned: true,
                            loot_items: Vec::new(),
                        }],
                        triggers: Vec::new(),
                    },
                ],
            },
        );
    });

    let mut sess = make_sess(char_id, ship_id, sys_id, None);
    sess.role = u64::MAX;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut r2_ball = make_site_npc_ball(room2_obj_id, 23, DVec3::from_array(room2_dest));
    r2_ball.mode = MovementMode::Rigid;
    let _ = sys.spawn_ball(r2_ball).await;

    // 1. Negative: direct warp into gated Room 2 refused
    let err_warp = call_svc(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdWarpToStuff",
        vec![PyRep::String("item".into()), PyRep::Integer(room2_obj_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_warp.code, "WarpBlockedByDeadspacePocket");

    // 2. ActivateAccelerationGate into Room 1 -> lands at room1_dest, and MWD activation succeeds
    call_svc(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate1_id)])
        .await
        .unwrap();
    let (_, b1) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(b1.position.distance(DVec3::from_array(room1_dest)) < 5_000.0);

    call_svc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(mwd_id), PyRep::String("online".into())])
        .await
        .expect("MWD online must succeed in Room 1");
    call_svc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        vec![PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())],
    )
    .await
    .expect("MWD propulsion activation must succeed in MWD-allowed Room 1");
    let _ = call_svc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Deactivate",
        vec![PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())],
    )
    .await;

    // 3. Move to gate2 and activate into MWD-disallowed Room 2
    let mut b_at_g2 = b1.clone();
    b_at_g2.position = DVec3::new(room1_dest[0] + 5_000.0, room1_dest[1], room1_dest[2]);
    let _ = sys.spawn_ball(b_at_g2).await;
    call_svc(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate2_id)])
        .await
        .unwrap();

    // 4. Negative: MWD propulsion activation refused in Room 2 where disallow_mwd == true
    let err_mwd = call_svc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        vec![PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err_mwd.code, "MicroWarpdriveDisallowedInDeadspace");
}
