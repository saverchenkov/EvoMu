//! Integration tests for Bosonic/GTFO/Reaper Doomsdays, Ice Refinery Heist,
//! and Deadspace Pocket Rules:
//! - `feat:modules.doomsday-bosonic-field`
//! - `feat:modules.doomsday-gtfo-reaper`
//! - `feat:pve.insurgency-ice-heist`
//! - `feat:pve.deadspace-pocket-rules`

use crate::common::*;
use eve_db::{InventoryFlag, InventoryItemRecord, PersistedInsurgencyCampaign};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
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
    let highsec_sys = 30_000_142; // Jita (0.95)
    let sys_id = 30_002_537; // Amamake (0.44 low-sec)
    let char_id = 90_001;
    let titan_id = 88_100_001;
    let dd_mod_id = 88_100_002; // 40633 Bosonic Field Generator (doomsdayConeDOT)
    let gtfo_mod_id = 88_100_003; // 40634 Gravitational Transportation Field Oscillator (doomsdayHOG)
    let reaper_mod_id = 88_100_004; // 40632 'Divine Harvest' Electromagnetic Reaper (doomsdaySlash)
    let helium_id = 88_100_005;
    let stront_id = 88_100_006;

    // 1. Negative: non-Titan hull refused
    seed_ship_in_space(&ctx, char_id, titan_id, 587, sys_id, DVec3::ZERO).await;
    ctx.db.with_mut_snapshot(|snap| {
        for (mid, tid, flg) in [
            (dd_mod_id, 40_633, InventoryFlag::HiSlot0),
            (gtfo_mod_id, 40_634, InventoryFlag::HiSlot1),
            (reaper_mod_id, 40_632, InventoryFlag::HiSlot2),
        ] {
            snap.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: titan_id,
                    flag: flg,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(mid, true);
        }
    });
    let mut sess = make_sess(char_id, titan_id, sys_id, None);
    let err = call_svc(
        &ctx,
        &mut sess,
        "superWeaponMgr",
        "ActivateSlashModule",
        vec![PyRep::Integer(dd_mod_id), PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "ShipCannotActivateSuperweapon");

    // 2. Negative: high-sec empire space refused (`disallowInEmpireSpace`), Green safety refused, missing fuel refused
    seed_ship_in_space(&ctx, char_id, titan_id, 11_567, highsec_sys, DVec3::ZERO).await;
    let mut hs_sess = make_sess(char_id, titan_id, highsec_sys, None);
    let hs_err = call_svc(
        &ctx,
        &mut hs_sess,
        "superWeaponMgr",
        "ActivateSlashModule",
        vec![PyRep::Integer(dd_mod_id), PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(hs_err.code, "CannotActivateInEmpireSpace");

    seed_ship_in_space(&ctx, char_id, titan_id, 11_567, sys_id, DVec3::ZERO).await;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
    });
    let safety_err = call_svc(
        &ctx,
        &mut sess,
        "superWeaponMgr",
        "ActivateSlashModule",
        vec![PyRep::Integer(dd_mod_id), PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap_err();
    assert!(safety_err.message.contains("CantDoInCurrentSafetyLevel") || safety_err.code.contains("CantDoInCurrentSafetyLevel"));
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
    });

    let err_fuel = call_svc(
        &ctx,
        &mut sess,
        "superWeaponMgr",
        "ActivateSlashModule",
        vec![PyRep::Integer(dd_mod_id), PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_fuel.code, "InsufficientFuel");

    // 3. Seed SDE Strontium Clathrates (16275) & Helium Isotopes (16274), plus targets (including tethered & Upwell structure)
    ctx.db.with_mut_snapshot(|snap| {
        for (fid, tid, qty) in [(stront_id, 16_275, 10_000), (helium_id, 16_274, 100_000)] {
            snap.items.insert(
                fid,
                InventoryItemRecord {
                    item_id: fid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: titan_id,
                    flag: InventoryFlag::Cargo,
                    quantity: qty,
                    ..Default::default()
                },
            );
        }
    });

    let cone_hit_id = 88_100_010;
    let cone_miss_id = 88_100_011;
    let cone_tethered_id = 88_100_014;
    let gtfo_hit_id = 88_100_012;
    let gtfo_tethered_id = 88_100_015;
    let gtfo_struct_id = 88_100_016;
    let reaper_hit_id = 88_100_013;
    seed_ship_in_space(&ctx, 90_002, cone_hit_id, 587, sys_id, DVec3::new(20_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_003, cone_miss_id, 587, sys_id, DVec3::new(0.0, 20_000.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_006, cone_tethered_id, 587, sys_id, DVec3::new(15_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_004, gtfo_hit_id, 587, sys_id, DVec3::new(-40_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_007, gtfo_tethered_id, 587, sys_id, DVec3::new(-38_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_005, reaper_hit_id, 587, sys_id, DVec3::new(0.0, -20_000.0, 10_000.0)).await;

    let sys = ctx.systems.get_or_spawn(sys_id);
    for tid in [cone_tethered_id, gtfo_tethered_id] {
        let mut cs = sys.get_combat_state(tid).await.unwrap().unwrap();
        cs.is_tethered = true;
        let _ = sys.set_combat_state(tid, cs).await;
    }
    let _ = sys.spawn_ball(make_site_npc_ball(gtfo_struct_id, 35_832, DVec3::new(-37_000.0, 0.0, 0.0))).await;

    // 4. Activate Bosonic Field Generator (Cone, 30km SDE maxRange)
    let pre_hit_cs = sys.get_combat_state(cone_hit_id).await.unwrap().unwrap();
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
    assert_eq!(cone_kv.get("fuelConsumed").and_then(|v| v.as_i64()), Some(2_000));
    let hit_ids: Vec<i64> = match cone_kv.get("hitTargetIDs") {
        Some(PyRep::List(l)) => l.iter().filter_map(|x| x.as_i64()).collect(),
        _ => Vec::new(),
    };
    assert!(hit_ids.contains(&cone_hit_id) && !hit_ids.contains(&cone_miss_id));

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

    let (_, titan_ball) = sys.get_ball_snapshot(titan_id).await.unwrap().unwrap();
    assert_eq!(titan_ball.max_velocity, 0.0);
    assert!(sys.get_combat_state(titan_id).await.unwrap().unwrap().disallow_assistance);
    assert_eq!(sys.get_combat_state(cone_hit_id).await.unwrap().unwrap().shield_hp, pre_hit_cs.shield_hp);

    // Advance 10s -> warmup completes and Bosonic tick damages cone_hit_id while respecting tether on cone_tethered_id
    ctx.advance_secs(10);
    let _ = sys.tick().await;
    assert!(sys.get_combat_state(cone_hit_id).await.unwrap().is_none_or(|c| c.shield_hp < pre_hit_cs.shield_hp));
    assert_eq!(
        sys.get_combat_state(cone_tethered_id).await.unwrap().unwrap().shield_hp,
        pre_tether_cs.shield_hp,
        "Bosonic AoE tick must respect tether invulnerability"
    );

    // 5. Negative GTFO out-of-range & NaN checks, then GTFO teleport within 10km SDE sphere (skipping tethered & structure)
    let oor_err = call_svc(
        &ctx,
        &mut sess,
        "superWeaponMgr",
        "ActivateSinglePointTargetedModule",
        vec![PyRep::Integer(gtfo_mod_id), PyRep::Real(900_000.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(oor_err.code, "TargetOutOfRange");

    let nan_err = call_svc(
        &ctx,
        &mut sess,
        "superWeaponMgr",
        "ActivateSinglePointTargetedModule",
        vec![PyRep::Integer(gtfo_mod_id), PyRep::Real(f64::NAN), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(nan_err.code, "InvalidTargetPoint");

    let gtfo_res = call_svc(
        &ctx,
        &mut sess,
        "superWeaponMgr",
        "ActivateSinglePointTargetedModule",
        vec![PyRep::Integer(gtfo_mod_id), PyRep::Real(-35_000.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap();
    let gtfo_kv = py_keyval_to_map(&gtfo_res);
    let tp_ids: Vec<i64> = match gtfo_kv.get("teleportedShipIDs") {
        Some(PyRep::List(l)) => l.iter().filter_map(|x| x.as_i64()).collect(),
        _ => Vec::new(),
    };
    assert!(tp_ids.contains(&gtfo_hit_id), "Eligible player ship inside 10km GTFO sphere must teleport");
    assert!(!tp_ids.contains(&gtfo_tethered_id) && !tp_ids.contains(&gtfo_struct_id), "Tethered ships and structures must not teleport");
    let (_, tp_ball) = sys.get_ball_snapshot(gtfo_hit_id).await.unwrap().unwrap();
    assert!(tp_ball.position.distance(DVec3::new(-40_000.0, 0.0, 0.0)) > 500_000.0);

    // 6. Activate Reaper ('Divine Harvest' Electromagnetic Reaper 40632, consumes 50,000 Helium Isotopes)
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
    let reaper_hits: Vec<i64> = match reaper_kv.get("hitTargetIDs") {
        Some(PyRep::List(l)) => l.iter().filter_map(|x| x.as_i64()).collect(),
        _ => Vec::new(),
    };
    assert!(reaper_hits.contains(&reaper_hit_id));

    // Advance past 30s immobilize window -> Titan mobility & assistance restored
    ctx.advance_secs(30);
    let _ = sys.tick().await;
    assert!(sys.get_ball_snapshot(titan_id).await.unwrap().unwrap().1.max_velocity > 0.0);
    assert!(!sys.get_combat_state(titan_id).await.unwrap().unwrap().disallow_assistance);

    // 7. Negative abuse checks: offline module & unfitted cargo module refused
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
    });
    let mut sess = make_sess(char_id, ship_id, sys_id, None);
    sess.role = 1 << 29; // ROLE_QA without ROLE_GM_MASK

    // 0. Negative: subrole ROLE_QA without ROLE_GM_MASK is refused on slash.SlashCmd with InsufficientRole + GM audit
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

    // 2. Seed corruption stage 3 (600 pts) and spawn via live 1 Hz system tick
    ctx.db.with_mut_snapshot(|snap| {
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
                corruption_points: HashMap::from([(sys_id, 600)]),
                suppression_points: HashMap::new(),
            },
        );
    });

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
    let tanker_cs = sys.get_combat_state(tanker_id).await.unwrap().unwrap();
    assert!(tanker_cs.warp_core_strength >= 100 && tanker_cs.is_web_immune);

    // Spawn a pirate ship within 25km of Upwell Security Thunderchild guard (150_000, 0, 190_000)
    let pirate_char_id = 90_002;
    let pirate_ship_id = 88_200_002;
    seed_ship_in_space(&ctx, pirate_char_id, pirate_ship_id, 587, sys_id, DVec3::new(150_000.0, 0.0, 195_000.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pirate_char_id) {
            ch.war_faction_id = Some(500_010);
        }
    });

    // 3. Escort all 10 tankers strictly via live 1 Hz ballpark physics (no teleporting)
    for _ in 0..80 {
        let _ = sys.tick().await;
    }
    assert!(sys.get_ball_snapshot(pirate_ship_id).await.unwrap().is_none(), "Thunderchild guard must destroy nearby pirate");

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
