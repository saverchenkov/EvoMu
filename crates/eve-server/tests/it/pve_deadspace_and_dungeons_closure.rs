//! Integration tests for Naturally Seeded SDE Deadspace Pocket Rules, Data-Driven MWD
//! Disallowance, and Multi-Room Dungeon Triggers (`OnApproach`, `OnKill`, `OnLoot`, `CompleteDungeon`)
//! without any GM-spawned room fixtures (`feat:pve.deadspace-pocket-rules`, `feat:pve.dungeon-system`).

#![forbid(unsafe_code)]

use eve_db::service_roles::ROLE_GM_DEFAULT;
use eve_db::{BookmarkRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::{
    ensure_system_pve_sites_seeded, with_pve_exploration_reg,
};
use glam::DVec3;
use std::collections::HashMap;

const PILOT_1: i64 = 90_000_001;
const JITA_SYS: u32 = 30_000_142;
const TYPE_RIFTER: u32 = 587;
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
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

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
        snap.items.insert(
            ship_id,
            make_item(ship_id, ship_type_id, char_id, i64::from(sys_id), InventoryFlag::None, 1, true),
        );
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

async fn step_until_warp_landed(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, ship_id: i64) {
    let kw = HashMap::new();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for _ in 0..120 {
        let _ = sys_handle.step_manual(1.0).await;
        if let Ok(Some((_, b))) = sys_handle.get_ball_snapshot(ship_id).await {
            if matches!(b.mode, MovementMode::Stop) {
                break;
            }
        }
    }
    ctx.dispatch_rpc(sess, "beyonce", None, "UpdateStateRequest", &[], &kw, None)
        .await
        .expect("UpdateStateRequest after warp");
}

// ============================================================================
// 1. Naturally Seeded Deadspace Pocket Rules & Data-Driven MWD Restriction
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
    let mut sess = setup_space_pilot(&ctx, 9203, PILOT_1, ship_id, TYPE_RIFTER, JITA_SYS, DVec3::ZERO).await;

    // Fit 5MN MWD (434) and 1MN Afterburner (439) + propulsion skills (only MWD online initially due to maxGroupOnline = 1)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mwd_id, make_item(mwd_id, TYPE_MWD_5MN, PILOT_1, ship_id, InventoryFlag::MedSlot0, 1, true));
        snap.items.insert(ab_id, make_item(ab_id, TYPE_AB_1MN, PILOT_1, ship_id, InventoryFlag::MedSlot1, 1, true));
        snap.module_online_states.insert(mwd_id, true);
        snap.module_online_states.insert(ab_id, false);
        let sk = snap.skills.entry(PILOT_1).or_default();
        for sid in [3413_u32, 3426, 3449, 3450, 3454, 3455] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    // 1. Seed system exploration sites naturally -> discovers DSG-30000142 (SDE dungeon 98: Guristas Scout Outpost)
    ensure_system_pve_sites_seeded(&ctx, JITA_SYS).await;
    let (dsg_pos, dsg_inst_id, dsg_did) = with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system[&JITA_SYS]
            .iter()
            .find(|s| s.site_id == "DSG-30000142" && s.site_category == "Deadspace")
            .expect("Naturally seeded DSG-30000142 deadspace combat signature");
        (s.position, s.expedition_instance_id.expect("runtime dungeon instance"), s.dungeon_id)
    });
    assert_eq!(dsg_did, 98, "Caldari high-sec deadspace signature must use SDE Guristas Scout Outpost (98)");

    // Scan DSG-30000142 to 100% via scanMgr.RequestScans and warp to it via beyonce.CmdWarpToStuff
    let probes: Vec<PyRep> = (0..8)
        .map(|_| PyRep::List(vec![PyRep::Real(dsg_pos[0]), PyRep::Real(dsg_pos[1]), PyRep::Real(dsg_pos[2])]))
        .collect();
    ctx.dispatch_rpc(
        &mut sess,
        "scanMgr",
        None,
        "RequestScans",
        &[PyRep::Real(600_000_000_000.0), PyRep::Real(120.0), PyRep::List(probes)],
        &kw,
        None,
    )
    .await
    .expect("RequestScans pinpoints DSG-30000142");

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        None,
        "CmdWarpToStuff",
        &[PyRep::String("scan".into()), PyRep::String("DSG-30000142".into())],
        &kw,
        None,
    )
    .await
    .expect("Warp to scanned DSG-30000142 entry gate");
    step_until_warp_landed(&ctx, &mut sess, JITA_SYS, ship_id).await;

    let inst = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, dsg_inst_id).unwrap();
    let entry_gate_id = inst.dict_get("entryGateBallID").and_then(|v| v.as_i64()).unwrap();
    let PyRep::List(rooms) = inst.dict_get("rooms").unwrap() else { panic!("Expected rooms") };
    let room1_id = rooms[0].dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
    let room2_id = rooms[1].dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
    let PyRep::Tuple(r2_pos_py) = rooms[1].dict_get("position").unwrap() else { panic!() };
    let r2_pos = [
        r2_pos_py[0].as_f64().unwrap(),
        r2_pos_py[1].as_f64().unwrap(),
        r2_pos_py[2].as_f64().unwrap(),
    ];

    // 2. Direct warp into gated Room 1 (by roomID) or gated Room 2 (by bookmark) is refused
    let warp_r1_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("dungeon".into()), PyRep::Integer(room1_id)],
            &kw,
            None,
        )
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
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("bookmark".into()), PyRep::Integer(bm_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Direct bookmark warp into gated Room 2 must fail");
    assert_eq!(warp_bm_err.code, WARP_DISRUPTED_ERR);

    // 3. Activate entry acceleration gate from warp landing spot -> lands in Room 1; MWD allowed in Room 1
    let sys_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let eg_pos = sys_handle.get_ball_snapshot(entry_gate_id).await.unwrap().unwrap().1.position;
    let ship_landed_pos = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position;
    assert!(ship_landed_pos.distance(eg_pos) <= 2_500.0, "Warp lands within 2.5km of entry gate");

    let gate1_res = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(entry_gate_id)], &kw, None)
        .await
        .expect("Activate entry acceleration gate");
    assert_eq!(gate1_res.dict_get("disallowMWD"), Some(&PyRep::Bool(false)));

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect("MWD allowed in outer Room 1");
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Deactivate",
            &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())],
            &kw,
            None,
        )
        .await;

    // Clear Room 1 guards to unlock the Room 1 -> Room 2 gate, approach gate via CmdFollowBall, and enter Room 2
    let PyRep::List(r1_npcs) = gate1_res.dict_get("npcBallIDs").unwrap() else { panic!() };
    for nid in r1_npcs {
        let _ = sys_handle
            .apply_damage_with_attacker(
                nid.as_i64().unwrap(),
                DamageVector { em: 1.0e6, thermal: 1.0e6, kinetic: 1.0e6, explosive: 1.0e6 },
                Some(PILOT_1),
            )
            .await;
    }
    let r1_gate_id = gate1_res.dict_get("nextGateBallID").and_then(|v| v.as_i64()).unwrap();
    let beyonce_oid = ctx.bind_object(&sess, "beyonce", i64::from(JITA_SYS)).unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &kw, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(r1_gate_id), PyRep::Real(500.0)], &kw, None)
        .await
        .expect("CmdFollowBall to Room 1 -> Room 2 gate");
    for _ in 0..60 {
        let _ = sys_handle.step_manual(1.0).await;
    }
    let gate2_res = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
        .await
        .expect("Activate Room 1 -> Room 2 gate");
    assert_eq!(gate2_res.dict_get("disallowMWD"), Some(&PyRep::Bool(true)));

    // 4. In inner Room 2 (disallowMWD == true): MWD refused with ModuleNotAllowedInDeadspace, Afterburner allowed
    let mwd_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect_err("MWD must be refused in MWD-disallowed deadspace room");
    assert_eq!(mwd_err.code, "ModuleNotAllowedInDeadspace");

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mwd_id, false);
        snap.module_online_states.insert(ab_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(ab_id), PyRep::String("moduleBonusAfterburner".into()), PyRep::None, PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect("Afterburner must remain allowed in MWD-disallowed deadspace room");

    // 5. Data-driven dungeon editor flags persist in world state
    sess.role |= ROLE_GM_DEFAULT;
    let mut edit_room_kw = HashMap::new();
    edit_room_kw.insert("disallowMWD".into(), PyRep::Bool(true));
    let created_room = ctx
        .dispatch_rpc(&mut sess, "dungeon", None, "EditRoom", &[PyRep::Integer(98), PyRep::String("Restricted Core".into())], &edit_room_kw, None)
        .await
        .expect("dungeon.EditRoom with disallowMWD");
    let created_room_id = created_room.dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
    assert!(eve_server::services::pve_dungeon_editor::is_dungeon_or_room_mwd_disallowed(&ctx, 98, Some(created_room_id)));
    eve_server::services::pve_dungeon_editor::set_dungeon_disallow_mwd(&ctx, 98, true);
    assert!(eve_server::services::pve_dungeon_editor::is_dungeon_or_room_mwd_disallowed(&ctx, 98, Some(room2_id)));
}

// ============================================================================
// 2. SDE-Seeded Multi-Room Dungeon System: Approach/Kill/Loot Triggers & Despawn
// ============================================================================
#[tokio::test]
async fn test_dungeon_system_rooms_triggers_and_despawn_on_completion() {
    // parity: feat:pve.dungeon-system
    eve_testkit::attest(&["feat:pve.dungeon-system"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let ship_id = 750_004_001i64;
    let mut sess = setup_space_pilot(&ctx, 9204, PILOT_1, ship_id, TYPE_RIFTER, JITA_SYS, DVec3::ZERO).await;

    // 1. Discover naturally seeded DSG-30000142, scan via RequestScans, warp via CmdWarpToStuff, enter Room 1
    ensure_system_pve_sites_seeded(&ctx, JITA_SYS).await;
    let (dsg_pos, inst_id) = with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system[&JITA_SYS]
            .iter()
            .find(|s| s.site_id == "DSG-30000142")
            .unwrap();
        (s.position, s.expedition_instance_id.unwrap())
    });
    let probes: Vec<PyRep> = (0..8)
        .map(|_| PyRep::List(vec![PyRep::Real(dsg_pos[0]), PyRep::Real(dsg_pos[1]), PyRep::Real(dsg_pos[2])]))
        .collect();
    ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[PyRep::Real(600_000_000_000.0), PyRep::Real(120.0), PyRep::List(probes)], &kw, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("scan".into()), PyRep::String("DSG-30000142".into())], &kw, None)
        .await
        .unwrap();
    step_until_warp_landed(&ctx, &mut sess, JITA_SYS, ship_id).await;

    let init_state = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id).unwrap();
    let entry_gate_id = init_state.dict_get("entryGateBallID").and_then(|v| v.as_i64()).unwrap();
    let gate1_res = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(entry_gate_id)], &kw, None)
        .await
        .expect("Enter Room 1 via entry gate");

    let r1_gate_id = gate1_res.dict_get("nextGateBallID").and_then(|v| v.as_i64()).unwrap();
    let PyRep::List(r1_objs) = gate1_res.dict_get("objectBallIDs").unwrap() else { panic!() };
    let r1_beacon_id = r1_objs[0].as_i64().unwrap();
    let PyRep::List(r1_npcs) = gate1_res.dict_get("npcBallIDs").unwrap() else { panic!() };
    assert_eq!(r1_npcs.len(), 2, "Group 1 spawns 2 SDE Guristas frigate guards immediately");
    let g1_npc_ids: Vec<i64> = r1_npcs.iter().map(|v| v.as_i64().unwrap()).collect();

    let sys_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let (_, g1_ball) = sys_handle.get_ball_snapshot(g1_npc_ids[0]).await.unwrap().unwrap();
    assert_ne!(g1_ball.type_id, 10_043, "Dungeon NPCs must come from SDE faction spawn groups, not hardcoded 10043");

    let beyonce_oid = ctx.bind_object(&sess, "beyonce", i64::from(JITA_SYS)).unwrap();
    let ship_inv_oid = ctx.bind_object(&sess, "invbroker", ship_id).unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &kw, None)
        .await
        .unwrap();

    // 2. Approach Room 1->2 gate first: locked gate refuses activation with GateLocked
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(r1_gate_id), PyRep::Real(500.0)], &kw, None)
        .await
        .unwrap();
    for _ in 0..60 {
        let _ = sys_handle.step_manual(1.0).await;
    }
    let locked_err = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
        .await
        .expect_err("Locked gate must refuse activation before Group 1 is cleared");
    assert_eq!(locked_err.code, "GateLocked");

    // 3. Approach Deadspace Relay Beacon via client CmdFollowBall + UpdateStateRequest -> OnApproach spawns Group 2 cruiser!
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(r1_beacon_id), PyRep::Real(500.0)], &kw, None)
        .await
        .unwrap();
    for _ in 0..40 {
        let _ = sys_handle.step_manual(1.0).await;
    }
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "UpdateStateRequest", &[], &kw, None)
        .await
        .unwrap();

    let after_approach = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id).unwrap();
    let PyRep::List(rooms_app) = after_approach.dict_get("rooms").unwrap() else { panic!() };
    let PyRep::List(r1_npcs_after) = rooms_app[0].dict_get("npcBallIDs").unwrap() else { panic!() };
    assert_eq!(r1_npcs_after.len(), 3, "OnApproach trigger must spawn Group 2 reinforcement NPC");
    let r1_reinf_id = r1_npcs_after[2].as_i64().unwrap();

    // 4. Destroy Group 1 guards -> OnKill unlocks Room 1->2 gate (leaving Group 2 alive to verify despawn later)
    for nid in g1_npc_ids {
        let _ = sys_handle
            .apply_damage_with_attacker(
                nid,
                DamageVector { em: 1.0e6, thermal: 1.0e6, kinetic: 1.0e6, explosive: 1.0e6 },
                Some(PILOT_1),
            )
            .await;
    }
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(r1_gate_id), PyRep::Real(500.0)], &kw, None)
        .await
        .unwrap();
    for _ in 0..40 {
        let _ = sys_handle.step_manual(1.0).await;
    }
    let enter_r2 = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
        .await
        .expect("Unlocked gate enters Room 2");

    let PyRep::List(r2_objs) = enter_r2.dict_get("objectBallIDs").unwrap() else { panic!() };
    let strongbox_id = r2_objs[0].as_i64().unwrap();
    let PyRep::List(r2_npcs) = enter_r2.dict_get("npcBallIDs").unwrap() else { panic!() };
    let r2_boss_id = r2_npcs[0].as_i64().unwrap();

    // 5. Approach & loot Overseer Strongbox via invbroker.LootAll -> OnLoot spawns Group 2 ambush defender!
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &kw, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(strongbox_id), PyRep::Real(500.0)], &kw, None)
        .await
        .unwrap();
    for _ in 0..55 {
        let _ = sys_handle.step_manual(1.0).await;
    }
    ctx.dispatch_rpc(&mut sess, "invbroker", Some(ship_inv_oid), "LootAll", &[PyRep::Integer(strongbox_id)], &kw, None)
        .await
        .expect("LootAll on Overseer Strongbox");

    let after_loot = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id).unwrap();
    let PyRep::List(rooms_loot) = after_loot.dict_get("rooms").unwrap() else { panic!() };
    let PyRep::List(r2_npcs_after_loot) = rooms_loot[1].dict_get("npcBallIDs").unwrap() else { panic!() };
    assert_eq!(r2_npcs_after_loot.len(), 2, "OnLoot trigger must spawn Room 2 ambush NPC");
    let r2_ambush_id = r2_npcs_after_loot[1].as_i64().unwrap();

    // 6. Destroy Room 2 Overseer commander -> OnKill fires CompleteDungeon, despawns all entities & collapses signature!
    let _ = sys_handle
        .apply_damage_with_attacker(
            r2_boss_id,
            DamageVector { em: 1.0e7, thermal: 1.0e7, kinetic: 1.0e7, explosive: 1.0e7 },
            Some(PILOT_1),
        )
        .await;

    let final_state = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id).unwrap();
    assert_eq!(final_state.dict_get("completed"), Some(&PyRep::Bool(true)));
    assert_eq!(final_state.dict_get("despawned"), Some(&PyRep::Bool(true)));
    assert!(sys_handle.get_ball_snapshot(entry_gate_id).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(r1_beacon_id).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(r1_gate_id).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(r1_reinf_id).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(r2_ambush_id).await.unwrap().is_none());
    assert!(
        with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system[&JITA_SYS]
                .iter()
                .find(|s| s.site_id == "DSG-30000142")
                .unwrap()
                .cleared
        }),
        "Completing deadspace dungeon must mark DSG-30000142 cleared"
    );
}
