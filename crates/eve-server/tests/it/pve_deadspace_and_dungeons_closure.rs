//! Integration tests for Deadspace Pocket Rules & Data-Driven MWD Disallowance
//! and Multi-Room Dungeon System (`feat:pve.deadspace-pocket-rules`, `feat:pve.dungeon-system`).

#![forbid(unsafe_code)]

use eve_db::service_roles::ROLE_GM_DEFAULT;
use eve_db::{BookmarkRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_1: i64 = 90_000_001;
const JITA_SYS: u32 = 30_000_142;
const TYPE_RIFTER: u32 = 587;
const TYPE_CARACAL: u32 = 621;
const TYPE_MWD_5MN: u32 = 434;
const TYPE_AB_1MN: u32 = 439;
const WARP_DISRUPTED_ERR: &str = "NaturalPhenomenaDisruptingWarp";

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        ..Default::default()
    }
}

async fn setup_space_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sess_id);
    ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await.expect("Login");
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");

    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, make_item(ship_id, ship_type_id, char_id, i64::from(sys_id), InventoryFlag::None, 1, true));
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 1.0,
                mass: 1_200_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
            ctx.build_ship_slim_meta(char_id, ship_type_id),
        )
        .await;
    sess
}

// ============================================================================
// 1. Deadspace Pocket Rules (Acceleration Gate Landing, No Direct Warp, Data-Driven MWD)
// ============================================================================
#[tokio::test]
async fn test_deadspace_pocket_rules_and_mwd_restriction() {
    // parity: feat:pve.deadspace-pocket-rules
    eve_testkit::attest(&["feat:pve.deadspace-pocket-rules"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let ship_id = 750_003_001i64;
    let mwd_id = 750_003_010i64;
    let ab_id = 750_003_011i64;
    let mut sess = setup_space_pilot(&ctx, 9203, PILOT_1, ship_id, TYPE_CARACAL, JITA_SYS, DVec3::ZERO).await;
    sess.role |= ROLE_GM_DEFAULT;

    // Fit 5MN MWD (434) and 1MN Afterburner (439) + propulsion skills
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mwd_id, make_item(mwd_id, TYPE_MWD_5MN, PILOT_1, ship_id, InventoryFlag::MedSlot0, 1, true));
        snap.items.insert(ab_id, make_item(ab_id, TYPE_AB_1MN, PILOT_1, ship_id, InventoryFlag::MedSlot1, 1, true));
        snap.module_online_states.insert(mwd_id, true);
        snap.module_online_states.insert(ab_id, true);
        let sk = snap.skills.entry(PILOT_1).or_default();
        for sid in [3413_u32, 3426, 3449, 3450, 3454, 3455] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    // Spawn 2-room gated deadspace dungeon: Room 1 allows MWD; Room 2 disallows MWD via room data
    let mut spawn_kw = HashMap::new();
    spawn_kw.insert("solarSystemID".into(), PyRep::Integer(i64::from(JITA_SYS)));
    spawn_kw.insert("dungeonID".into(), PyRep::Integer(43));
    spawn_kw.insert("dungeonName".into(), PyRep::String("Deadspace Relay Complex".into()));
    spawn_kw.insert("hasEntryGate".into(), PyRep::Bool(true));
    spawn_kw.insert("disallowMWD".into(), PyRep::Bool(false));
    spawn_kw.insert("position".into(), PyRep::Tuple(vec![PyRep::Real(250_000_000.0), PyRep::Real(0.0), PyRep::Real(150_000_000.0)]));
    spawn_kw.insert(
        "rooms".into(),
        PyRep::List(vec![
            PyRep::keyval([("roomName", PyRep::String("Outer Perimeter".into())), ("disallowMWD", PyRep::Bool(false)), ("gateLocked", PyRep::Bool(false))]),
            PyRep::keyval([("roomName", PyRep::String("Inner Magnetic Core".into())), ("disallowMWD", PyRep::Bool(true)), ("gateLocked", PyRep::Bool(false))]),
        ]),
    );
    let inst = eve_server::services::pve_dungeon_runtime::spawn_dungeon_instance(&ctx, PILOT_1, Some(JITA_SYS), &[], &spawn_kw).await.expect("spawn_dungeon_instance");
    let entry_gate_id = inst.dict_get("entryGateBallID").and_then(|v| v.as_i64()).unwrap();
    let PyRep::List(rooms) = inst.dict_get("rooms").unwrap() else { panic!("Expected rooms list") };
    let room1_id = rooms[0].dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
    let room2_id = rooms[1].dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
    let PyRep::Tuple(r2_pos_py) = rooms[1].dict_get("position").unwrap() else { panic!() };
    let r2_pos = [r2_pos_py[0].as_f64().unwrap(), r2_pos_py[1].as_f64().unwrap(), r2_pos_py[2].as_f64().unwrap()];

    // 1. Direct warp into gated Room 1 or Room 2 (by dungeon roomID or bookmark) is impossible
    let warp_r1_err = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("dungeon".into()), PyRep::Integer(room1_id)], &kw, None)
        .await
        .expect_err("Direct warp into gated Room 1 must fail");
    assert_eq!(warp_r1_err.code, WARP_DISRUPTED_ERR);

    let bm_id = 880_001_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            bm_id,
            BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: PILOT_1,
                label: "Inner Room Bookmark".into(),
                solar_system_id: JITA_SYS,
                x: r2_pos[0],
                y: r2_pos[1],
                z: r2_pos[2],
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    let warp_bm_err = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(bm_id)], &kw, None)
        .await
        .expect_err("Direct bookmark warp into gated Room 2 must fail");
    assert_eq!(warp_bm_err.code, WARP_DISRUPTED_ERR);

    // 2. Activate entry acceleration gate -> lands at Room 1 gateDestinationPos; MWD succeeds in Room 1
    let sys_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let eg_pos = sys_handle.get_ball_snapshot(entry_gate_id).await.unwrap().unwrap().1.position;
    let _ = sys_handle.set_ball_position(ship_id, eg_pos).await;
    let gate1_res = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(entry_gate_id)], &kw, None)
        .await
        .expect("Activate entry acceleration gate");
    assert_eq!(gate1_res.dict_get("success"), Some(&PyRep::Bool(true)));
    let landed_r1 = DVec3::new(
        gate1_res.dict_get("destinationX").and_then(|v| v.as_f64()).unwrap(),
        gate1_res.dict_get("destinationY").and_then(|v| v.as_f64()).unwrap(),
        gate1_res.dict_get("destinationZ").and_then(|v| v.as_f64()).unwrap(),
    );
    let ship_pos_r1 = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position;
    assert!(ship_pos_r1.distance(landed_r1) < 1.0, "Ship must land at Room 1 gate destination");

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
        .await
        .expect("MWD allowed in Room 1");
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())], &kw, None).await;

    // 3. Activate Room 1 -> Room 2 gate -> lands in Room 2 where disallowMWD == true; MWD refused, AB allowed
    let r1_gate_id = gate1_res.dict_get("nextGateBallID").and_then(|v| v.as_i64()).unwrap();
    let r1_gate_pos = sys_handle.get_ball_snapshot(r1_gate_id).await.unwrap().unwrap().1.position;
    let _ = sys_handle.set_ball_position(ship_id, r1_gate_pos).await;
    let gate2_res = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
        .await
        .expect("Activate Room 1 -> Room 2 gate");
    assert_eq!(gate2_res.dict_get("disallowMWD"), Some(&PyRep::Bool(true)));

    let mwd_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
        .await
        .expect_err("MWD must be refused in MWD-disallowed deadspace room");
    assert_eq!(mwd_err.code, "ModuleNotAllowedInDeadspace");

    // Propulsion modules share maxGroupOnline = 1; offline MWD and online Afterburner
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mwd_id, false);
        snap.module_online_states.insert(ab_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(ab_id), PyRep::String("moduleBonusAfterburner".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
        .await
        .expect("Afterburner must remain allowed in MWD-disallowed deadspace room");

    // 4. Data-driven dungeon editor flags (dungeon.EditRoom with disallowMWD + dungeon-level flag)
    let mut edit_room_kw = HashMap::new();
    edit_room_kw.insert("disallowMWD".into(), PyRep::Bool(true));
    let created_room = ctx
        .dispatch_rpc(&mut sess, "dungeon", None, "EditRoom", &[PyRep::Integer(43), PyRep::String("Restricted Core".into())], &edit_room_kw, None)
        .await
        .expect("dungeon.EditRoom with disallowMWD");
    let created_room_id = created_room.dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
    assert!(eve_server::services::pve_dungeon_editor::is_dungeon_or_room_mwd_disallowed(&ctx, 43, Some(created_room_id)));
    eve_server::services::pve_dungeon_editor::set_dungeon_disallow_mwd(&ctx, 43, true);
    assert!(eve_server::services::pve_dungeon_editor::is_dungeon_or_room_mwd_disallowed(&ctx, 43, Some(room2_id)));
}

// ============================================================================
// 2. Multi-Room Dungeon System (Objects, NPC Waves, Kill/Approach/Loot Triggers & Despawn)
// ============================================================================
#[tokio::test]
async fn test_dungeon_system_rooms_triggers_and_despawn_on_completion() {
    // parity: feat:pve.dungeon-system
    eve_testkit::attest(&["feat:pve.dungeon-system"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let ship_id = 750_004_001i64;
    let mut sess = setup_space_pilot(&ctx, 9204, PILOT_1, ship_id, TYPE_RIFTER, JITA_SYS, DVec3::new(350_000_000.0, 0.0, 200_000_000.0)).await;

    let mut spawn_kw = HashMap::new();
    spawn_kw.insert("solarSystemID".into(), PyRep::Integer(i64::from(JITA_SYS)));
    spawn_kw.insert("dungeonID".into(), PyRep::Integer(2005));
    spawn_kw.insert("dungeonName".into(), PyRep::String("Guristas Forward Outpost".into()));
    spawn_kw.insert("hasEntryGate".into(), PyRep::Bool(false));
    spawn_kw.insert("position".into(), PyRep::Tuple(vec![PyRep::Real(350_000_000.0), PyRep::Real(0.0), PyRep::Real(200_000_000.0)]));
    spawn_kw.insert(
        "rooms".into(),
        PyRep::List(vec![
            PyRep::keyval([
                ("roomName", PyRep::String("Staging Courtyard".into())),
                ("gateLocked", PyRep::Bool(true)),
                ("objects", PyRep::List(vec![PyRep::keyval([
                    ("typeID", PyRep::Integer(23)),
                    ("name", PyRep::String("Comm Relay".into())),
                    ("offset", PyRep::Tuple(vec![PyRep::Real(3_000.0), PyRep::Real(0.0), PyRep::Real(0.0)])),
                ])])),
                ("npcGroups", PyRep::List(vec![
                    PyRep::keyval([("groupID", PyRep::Integer(1)), ("wave", PyRep::Integer(1)), ("spawnImmediately", PyRep::Bool(true)), ("npcs", PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(10_043), PyRep::Integer(1)])]))]),
                    PyRep::keyval([("groupID", PyRep::Integer(2)), ("wave", PyRep::Integer(2)), ("spawnImmediately", PyRep::Bool(false)), ("npcs", PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(10_043), PyRep::Integer(1)])]))]),
                ])),
                ("triggers", PyRep::List(vec![
                    PyRep::keyval([("kind", PyRep::String("approach".into())), ("targetObjectIndex", PyRep::Integer(0)), ("radius", PyRep::Real(5_000.0)), ("action", PyRep::String("SpawnNpcGroup".into())), ("actionGroup", PyRep::Integer(2))]),
                    PyRep::keyval([("kind", PyRep::String("kill".into())), ("targetGroup", PyRep::Integer(1)), ("action", PyRep::String("UnlockGate".into()))]),
                ])),
            ]),
            PyRep::keyval([
                ("roomName", PyRep::String("Overseer Vault".into())),
                ("gateLocked", PyRep::Bool(false)),
                ("objects", PyRep::List(vec![PyRep::keyval([
                    ("typeID", PyRep::Integer(23)),
                    ("name", PyRep::String("Overseer Cargo Vault".into())),
                    ("isContainer", PyRep::Bool(true)),
                    ("lootItems", PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(50)])])),
                    ("offset", PyRep::Tuple(vec![PyRep::Real(2_000.0), PyRep::Real(0.0), PyRep::Real(0.0)])),
                ])])),
                ("npcGroups", PyRep::List(vec![
                    PyRep::keyval([("groupID", PyRep::Integer(1)), ("wave", PyRep::Integer(1)), ("spawnImmediately", PyRep::Bool(true)), ("npcs", PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(10_043), PyRep::Integer(1)])]))]),
                ])),
                ("triggers", PyRep::List(vec![
                    PyRep::keyval([("kind", PyRep::String("loot".into())), ("targetObjectIndex", PyRep::Integer(0)), ("itemTypeID", PyRep::Integer(34)), ("action", PyRep::String("CompleteDungeon".into()))]),
                ])),
            ]),
        ]),
    );

    let inst = eve_server::services::pve_dungeon_runtime::spawn_dungeon_instance(&ctx, PILOT_1, Some(JITA_SYS), &[], &spawn_kw).await.expect("spawn_dungeon_instance");
    let inst_id = inst.dict_get("instanceID").and_then(|v| v.as_i64()).unwrap();
    let PyRep::List(rooms) = inst.dict_get("rooms").unwrap() else { panic!("Expected rooms list") };
    let r1_gate_id = rooms[0].dict_get("gateBallID").and_then(|v| v.as_i64()).unwrap();
    let PyRep::List(r1_objs) = rooms[0].dict_get("objectBallIDs").unwrap() else { panic!() };
    let r1_beacon_id = r1_objs[0].as_i64().unwrap();
    let PyRep::List(r1_npcs) = rooms[0].dict_get("npcBallIDs").unwrap() else { panic!() };
    assert_eq!(r1_npcs.len(), 1, "Only Group 1 spawns immediately in Room 1");
    let r1_npc1_id = r1_npcs[0].as_i64().unwrap();

    let sys_handle = ctx.systems.get_or_spawn(JITA_SYS);
    assert!(sys_handle.get_ball_snapshot(r1_beacon_id).await.unwrap().is_some());
    assert!(sys_handle.get_ball_snapshot(r1_npc1_id).await.unwrap().is_some());

    // 1. Locked gate refuses activation until OnKill unlocks it
    let gate_pos = sys_handle.get_ball_snapshot(r1_gate_id).await.unwrap().unwrap().1.position;
    let _ = sys_handle.set_ball_position(ship_id, gate_pos).await;
    let locked_err = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
        .await
        .expect_err("Locked gate must refuse activation");
    assert_eq!(locked_err.code, "GateLocked");

    // 2. approach trigger fires when ship moves within 5,000m of Comm Relay -> spawns Group 2 reinforcement NPC
    let beacon_pos = sys_handle.get_ball_snapshot(r1_beacon_id).await.unwrap().unwrap().1.position;
    let _ = sys_handle.set_ball_position(ship_id, beacon_pos).await;
    let trig_count = eve_server::services::pve_dungeon_runtime::check_dungeon_triggers(&ctx, PILOT_1, JITA_SYS, ship_id).await;
    assert_eq!(trig_count, 1);

    let state_after_approach = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id).unwrap();
    let PyRep::List(rooms_app) = state_after_approach.dict_get("rooms").unwrap() else { panic!() };
    let PyRep::List(r1_npcs_after) = rooms_app[0].dict_get("npcBallIDs").unwrap() else { panic!() };
    assert_eq!(r1_npcs_after.len(), 2, "approach trigger must spawn Group 2 NPCs");
    let r1_npc2_id = r1_npcs_after[1].as_i64().unwrap();

    // 3. kill trigger fires when Group 1 NPC dies -> unlocks Room 1 gate (leaving Group 2 alive to verify despawn later)
    let _ = sys_handle
        .apply_damage_with_attacker(r1_npc1_id, DamageVector { em: 1.0e7, thermal: 1.0e7, kinetic: 1.0e7, explosive: 1.0e7 }, Some(PILOT_1))
        .await;
    eve_server::services::pve_dungeon_runtime::on_dungeon_npc_killed(&ctx, JITA_SYS, r1_npc1_id, PILOT_1).await;

    let _ = sys_handle.set_ball_position(ship_id, gate_pos).await;
    let enter_r2 = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
        .await
        .expect("Unlocked gate enters Room 2");
    let PyRep::List(r2_objs) = enter_r2.dict_get("objectBallIDs").unwrap() else { panic!() };
    let r2_container_id = r2_objs[0].as_i64().unwrap();
    let PyRep::List(r2_npcs) = enter_r2.dict_get("npcBallIDs").unwrap() else { panic!() };
    let r2_npc_id = r2_npcs[0].as_i64().unwrap();

    // 4. loot trigger fires when player loots Overseer Cargo Vault -> completes dungeon & despawns all dungeon entities
    let cont_pos = sys_handle.get_ball_snapshot(r2_container_id).await.unwrap().unwrap().1.position;
    let _ = sys_handle.set_ball_position(ship_id, cont_pos).await;
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "LootAll", &[PyRep::Integer(r2_container_id)], &kw, None).await.expect("LootAll on Overseer Cargo Vault");

    let final_state = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id).unwrap();
    assert_eq!(final_state.dict_get("completed"), Some(&PyRep::Bool(true)));
    assert_eq!(final_state.dict_get("despawned"), Some(&PyRep::Bool(true)));
    assert!(sys_handle.get_ball_snapshot(r1_beacon_id).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(r1_gate_id).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(r1_npc2_id).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(r2_npc_id).await.unwrap().is_none());
}
