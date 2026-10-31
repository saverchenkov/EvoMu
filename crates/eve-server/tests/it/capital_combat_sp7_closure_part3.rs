//! Integration tests for Bosonic/GTFO/Reaper Doomsdays, Ice Refinery Heist,
//! Deadspace Pocket Rules, and Deathless Shipcaster to FOB:
//! - `feat:modules.doomsday-bosonic-field`
//! - `feat:modules.doomsday-gtfo-reaper`
//! - `feat:pve.insurgency-ice-heist`
//! - `feat:pve.deadspace-pocket-rules`
//! - `feat:navigation.deathless-shipcaster-fob`

use crate::common::*;
use eve_db::{InventoryFlag, InventoryItemRecord, PersistedInsurgencyCampaign};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::cluster_macho_misc_services::{
    with_misc_state, LandingPadRecord, LauncherRecord,
};
use eve_server::services::pve_dungeon_runtime::{
    with_dungeon_runtime_store, DungeonRuntimeInstance, DungeonRuntimeObject, DungeonRuntimeRoom,
};
use eve_server::services::pve_incursion_sites::make_site_npc_ball;
use glam::DVec3;
use std::collections::HashMap;

fn make_sess(char_id: i64, ship_id: i64, sys_id: u32, station_id: Option<i64>) -> SessionState {
    let mut sess = SessionState::default();
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = station_id;
    sess
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
            let mut template = snap.characters.values().next().cloned().unwrap();
            template.character_id = char_id;
            template.name = format!("Pilot{char_id}");
            template.war_faction_id = None;
            snap.characters.insert(char_id, template);
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
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
    let sys_id = 30_002_537;
    let char_id = 90_001;
    let titan_id = 88_100_001;
    let dd_mod_id = 88_100_002;
    let fuel_id = 88_100_005;

    // 1. Negative: non-Titan hull refused
    seed_ship_in_space(&ctx, char_id, titan_id, 587, sys_id, DVec3::ZERO).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            dd_mod_id,
            InventoryItemRecord {
                item_id: dd_mod_id,
                type_id: 35_695,
                owner_id: char_id,
                location_id: titan_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::new(),
                ..Default::default()
            },
        );
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

    // 2. Upgrade to Avatar Titan (671, group 30) without fuel -> InsufficientFuel
    seed_ship_in_space(&ctx, char_id, titan_id, 671, sys_id, DVec3::ZERO).await;
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

    // 3. Seed 150,000 Helium Isotopes (16274), GTFO/Reaper modules, and targets
    let gtfo_mod_id = 88_100_003;
    let reaper_mod_id = 88_100_004;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fuel_id,
            InventoryItemRecord {
                item_id: fuel_id,
                type_id: 16_274,
                owner_id: char_id,
                location_id: titan_id,
                flag: InventoryFlag::Cargo,
                quantity: 150_000,
                singleton: false,
                dynamic_attributes: HashMap::new(),
                ..Default::default()
            },
        );
        for (mid, tid) in [(gtfo_mod_id, 35_699), (reaper_mod_id, 40_633)] {
            snap.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: titan_id,
                    flag: InventoryFlag::HiSlot1,
                    quantity: 1,
                    singleton: true,
                    dynamic_attributes: HashMap::new(),
                    ..Default::default()
                },
            );
        }
    });

    let cone_hit_id = 88_100_010;
    let cone_miss_id = 88_100_011;
    let gtfo_hit_id = 88_100_012;
    let reaper_hit_id = 88_100_013;
    seed_ship_in_space(&ctx, 90_002, cone_hit_id, 587, sys_id, DVec3::new(50_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_003, cone_miss_id, 587, sys_id, DVec3::new(0.0, 50_000.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_004, gtfo_hit_id, 587, sys_id, DVec3::new(-40_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_005, reaper_hit_id, 587, sys_id, DVec3::new(0.0, -40_000.0, 20_000.0)).await;

    // 4. ActivateSlashModule with Bosonic Field Generator (Cone)
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
    assert_eq!(cone_kv.get("tetherBlocked").and_then(|v| match v { PyRep::Bool(b) => Some(*b), _ => None }), Some(true));
    let hit_ids: Vec<i64> = match cone_kv.get("hitTargetIDs") {
        Some(PyRep::List(l)) => l.iter().filter_map(|x| x.as_i64()).collect(),
        _ => Vec::new(),
    };
    assert!(hit_ids.contains(&cone_hit_id));
    assert!(!hit_ids.contains(&cone_miss_id));

    // Verify Titan immobilized & tether-blocked
    let sys = ctx.systems.get_or_spawn(sys_id);
    let (_, titan_ball) = sys.get_ball_snapshot(titan_id).await.unwrap().unwrap();
    assert_eq!(titan_ball.max_velocity, 0.0);
    let titan_cs = sys.get_combat_state(titan_id).await.unwrap().unwrap();
    assert!(titan_cs.disallow_assistance);

    // 5. ActivateSinglePointTargetedModule with GTFO (Spatial)
    let gtfo_res = call_svc(
        &ctx,
        &mut sess,
        "superWeaponMgr",
        "ActivateSinglePointTargetedModule",
        vec![PyRep::Integer(gtfo_mod_id), PyRep::Real(-40_000.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap();
    let gtfo_kv = py_keyval_to_map(&gtfo_res);
    let tp_ids: Vec<i64> = match gtfo_kv.get("teleportedShipIDs") {
        Some(PyRep::List(l)) => l.iter().filter_map(|x| x.as_i64()).collect(),
        _ => Vec::new(),
    };
    assert!(tp_ids.contains(&gtfo_hit_id));
    let (_, tp_ball) = sys.get_ball_snapshot(gtfo_hit_id).await.unwrap().unwrap();
    assert!(tp_ball.position.distance(DVec3::new(-40_000.0, 0.0, 0.0)) > 500_000.0);

    // 6. ActivateSlashModule with Reaper (MultiPoint)
    let reaper_res = call_svc(
        &ctx,
        &mut sess,
        "superWeaponMgr",
        "ActivateSlashModule",
        vec![
            PyRep::Integer(reaper_mod_id),
            PyRep::Real(0.0), PyRep::Real(-40_000.0), PyRep::Real(0.0),
            PyRep::Real(0.0), PyRep::Real(-40_000.0), PyRep::Real(50_000.0),
        ],
    )
    .await
    .unwrap();
    let reaper_kv = py_keyval_to_map(&reaper_res);
    let reaper_hits: Vec<i64> = match reaper_kv.get("hitTargetIDs") {
        Some(PyRep::List(l)) => l.iter().filter_map(|x| x.as_i64()).collect(),
        _ => Vec::new(),
    };
    assert!(reaper_hits.contains(&reaper_hit_id));
}

#[tokio::test]
async fn test_insurgency_ice_heist_lifecycle() {
    // parity: feat:pve.insurgency-ice-heist
    eve_testkit::attest(&["feat:pve.insurgency-ice-heist"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let sys_id = 30_000_142;
    let char_id = 90_001;
    let ship_id = 88_200_001;
    seed_ship_in_space(&ctx, char_id, ship_id, 587, sys_id, DVec3::ZERO).await;
    let mut sess = make_sess(char_id, ship_id, sys_id, None);
    sess.role = u64::MAX;

    // 1. Negative: corruption stage < 3 refused
    let err = call_svc(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String(format!("/iceheist spawn {sys_id}"))],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "CorruptionStageOutOfRange");

    // 2. Seed corruption stage 3 (600 pts)
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

    let inst_rep = call_svc(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String(format!("/iceheist spawn {sys_id}"))],
    )
    .await
    .unwrap();
    let inst_id = inst_rep.as_i64().unwrap();

    // Verify listed by GetPirateInsurgencyIceHeistInstances and tanker is web-immune
    let list_rep = call_svc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetPirateInsurgencyIceHeistInstances",
        vec![PyRep::Integer(i64::from(sys_id))],
    )
    .await
    .unwrap();
    let PyRep::List(items) = list_rep else { panic!("expected list") };
    assert_eq!(items.len(), 1);

    let sys = ctx.systems.get_or_spawn(sys_id);
    let tanker_id = 1_896_000_000 + inst_id * 10 + 1;
    let tanker_cs = sys.get_combat_state(tanker_id).await.unwrap().unwrap();
    assert!(tanker_cs.warp_core_strength >= 100);

    // Spawn a pirate ship near the Thunderchild guard (155_000, 0, 150_000) and verify destruction
    let pirate_char_id = 90_002;
    let pirate_ship_id = 88_200_002;
    seed_ship_in_space(&ctx, pirate_char_id, pirate_ship_id, 587, sys_id, DVec3::new(156_000.0, 0.0, 150_000.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pirate_char_id) {
            ch.war_faction_id = Some(500_010);
        }
    });

    // 3. Escort 10 tankers -> wins 150,000 LP and destroys pirate near Thunderchild guard
    for step in 1..=10 {
        let res = call_svc(
            &ctx,
            &mut sess,
            "slash",
            "SlashCmd",
            vec![PyRep::String(format!("/iceheist escort {inst_id} empire"))],
        )
        .await
        .unwrap();
        let kv = py_keyval_to_map(&res);
        if step == 1 {
            assert!(sys.get_ball_snapshot(pirate_ship_id).await.unwrap().is_none());
        }
        if step == 10 {
            assert_eq!(kv.get("completed").and_then(|v| match v { PyRep::Bool(b) => Some(*b), _ => None }), Some(true));
            assert_eq!(kv.get("lpAwarded").and_then(|v| v.as_i64()), Some(150_000));
        }
    }

    let lp_bal = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .and_then(|c| c.loyalty_points.get(&1_000_137).copied())
            .unwrap_or(0)
    });
    assert!(lp_bal >= 150_000);

    // 4. Verify 4h respawn cooldown blocks immediate respawn
    let err_cd = call_svc(
        &ctx,
        &mut sess,
        "slash",
        "SlashCmd",
        vec![PyRep::String(format!("/iceheist spawn {sys_id}"))],
    )
    .await
    .unwrap_err();
    assert_eq!(err_cd.code, "IceHeistRespawnCooldown");
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
                dynamic_attributes: HashMap::new(),
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

    // 2. ActivateAccelerationGate into Room 1 -> lands at room1_dest
    call_svc(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        vec![PyRep::Integer(gate1_id)],
    )
    .await
    .unwrap();
    let (_, b1) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(b1.position.distance(DVec3::from_array(room1_dest)) < 5_000.0);

    // 3. Move to gate2 and activate into MWD-disallowed Room 2
    let mut b_at_g2 = b1.clone();
    b_at_g2.position = DVec3::new(room1_dest[0] + 5_000.0, room1_dest[1], room1_dest[2]);
    let _ = sys.spawn_ball(b_at_g2).await;
    call_svc(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        vec![PyRep::Integer(gate2_id)],
    )
    .await
    .unwrap();

    // 4. Negative: MWD activation refused in Room 2 where disallow_mwd == true
    let err_mwd = call_svc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        vec![PyRep::Integer(mwd_id), PyRep::String("online".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err_mwd.code, "MicroWarpdriveDisallowedInDeadspace");
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
    l_ball.mode = MovementMode::Rigid;
    let _ = sys.spawn_ball(l_ball).await;

    with_misc_state(&ctx, |st| {
        st.launchers.push(LauncherRecord {
            item_id: launcher_id,
            type_id: 77_262,
            solar_system_id: zarzakh_id,
            faction_id: 0,
            position: DVec3::new(10_500.0, 0.0, 10_000.0),
            linked_pad_ids: vec![guristas_pad_id],
        });
        st.landing_pads.push(LandingPadRecord {
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
