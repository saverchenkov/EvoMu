//! Extra unit and mutation-killing tests for `services::pve_dungeon_runtime_rpc`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_dungeon_runtime::{
    with_dungeon_runtime_store, DungeonNpcGroup, DungeonRuntimeInstance, DungeonRuntimeObject,
    DungeonRuntimeRoom, DungeonTrigger, DungeonTriggerAction, DungeonTriggerKind,
    DEFAULT_CONTAINER_TYPE_ID,
};
use crate::services::pve_dungeon_runtime_rpc::*;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{DungeonTemplate, InvType, SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx_and_session(sys_id: SolarSystemId, char_id: i64, ship_id: i64) -> (ServerContext, SessionState) {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("dng_rpc_ext", "pw", char_id, "DngRpcExt");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_785_000_000)));
    let mut sess = SessionState::new(401);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    (ctx, sess)
}

#[test]
fn build_sde_rooms_factions_difficulty_progression_events_and_notify_actions() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id = 90_040_001i64;
    let (mut ctx, _sess) = make_ctx_and_session(sys_id, char_id, 1_040_000_001);

    // Inject synthetic dungeons covering all 5 pirate factions and difficulty < 4 vs >= 4
    let mut sde = (*ctx.sde).clone();
    for (did, fid, diff, arch) in [
        (990_001u32, Some(500_011u32), Some(2u32), Some(3u32)),  // Angel Cartel, diff < 4
        (990_002u32, Some(500_012u32), Some(4u32), Some(4u32)),  // Blood Raiders, diff >= 4
        (990_003u32, Some(500_019u32), Some(5u32), Some(51u32)), // Sansha, diff >= 4, arch 51
        (990_004u32, Some(500_020u32), Some(3u32), None),        // Serpentis, diff < 4
        (990_005u32, Some(500_010u32), Some(4u32), Some(24u32)), // Guristas, diff >= 4
        (990_006u32, None, None, None),                          // Fallback regional faction + default diff 3
    ] {
        sde.dungeons.insert(
            did,
            DungeonTemplate {
                dungeon_id: did,
                name: format!("Test Dungeon {did}"),
                archetype_id: arch,
                faction_id: fid,
                difficulty: diff,
            },
        );
    }
    ctx.sde = Arc::new(sde);

    let base = [10_000.0, 20_000.0, 30_000.0];
    // sys_id == 0 uses fallback loot 19_419; has_entry_gate = true offsets r1_pos by +600_000
    let rooms_gated = build_sde_deadspace_rooms(&ctx, 0, 990_001, "Angel Den", base, true);
    assert_eq!(rooms_gated.len(), 2);
    assert!(rooms_gated[0].gated);
    assert_eq!(rooms_gated[0].position[0], base[0] + 600_000.0);
    assert_eq!(rooms_gated[0].gate_destination_pos[0], rooms_gated[0].position[0] - 10_000.0);
    assert_eq!(rooms_gated[1].objects[0].loot_items, vec![(19_419, 1)]);

    // sys_id > 0 with each faction and difficulty
    for did in [990_002, 990_003, 990_004, 990_005, 990_006] {
        let rooms = build_sde_deadspace_rooms(&ctx, sys_id, did, "Faction Complex", base, false);
        assert_eq!(rooms.len(), 2);
        assert!(!rooms[0].gated);
        assert_eq!(rooms[0].position, base);
        assert_eq!(rooms[1].position[0], base[0] + 600_000.0);
        assert!(!rooms[0].npc_groups[0].npc_specs.is_empty());
        assert!(!rooms[1].npc_groups[0].npc_specs.is_empty());
    }

    // record_dungeon_progression_events deduplicates task IDs and updates active_objective_id
    let op_id = register_sde_operation_dungeon_instance(&ctx, char_id, sys_id, 990_003);
    with_dungeon_runtime_store(&ctx, |store| {
        let inst = store.instances.get_mut(&op_id).unwrap();
        assert!(inst.disallow_mwd);
        record_dungeon_progression_events(inst, &[(101, 201), (101, 202), (102, 203)]);
        assert_eq!(inst.completed_task_ids, vec![101, 102]);
        assert_eq!(inst.last_completed_task_id, Some(102));
        assert_eq!(inst.active_objective_id, 203);
    });

    // dispatch_dungeon_notify_actions records notifications in pending_notifications
    dispatch_dungeon_notify_actions(
        &ctx.db,
        Some(&ctx.pending_notifications),
        Some(&ctx.notify_tx),
        char_id,
        &[(101, 201)],
    );
    let queued = ctx.pending_notifications.read().unwrap();
    assert!(!queued.get(&char_id).unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_activation_errors_rig_restriction_dng_inst_tag_and_progression_advance() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id = 90_040_002i64;
    let ship_id = 1_040_000_002i64;
    let (mut ctx, mut sess) = make_ctx_and_session(sys_id, char_id, ship_id);

    // Inject difficulty=1 dungeon and a ship type with rigSize (attr 1547) = 2.0 but low mass
    let mut sde = (*ctx.sde).clone();
    sde.dungeons.insert(
        990_010,
        DungeonTemplate {
            dungeon_id: 990_010,
            name: "Frigate Only Complex".into(),
            archetype_id: Some(51),
            faction_id: Some(500_010),
            difficulty: Some(1),
        },
    );
    let mut cruiser_type: InvType = sde.types.get(&587).cloned().unwrap();
    cruiser_type.type_id = 990_587;
    cruiser_type.default_attributes.insert(1547, 2.0); // Medium rig -> restricted in diff 1!
    sde.types.insert(990_587, cruiser_type);
    ctx.sde = Arc::new(sde);

    // Spawn custom dungeon using Tuple of rooms via args[1], default object/group/trigger fields
    let custom_rooms = PyRep::Tuple(vec![
        PyRep::keyval([
            ("objects", PyRep::Tuple(vec![PyRep::keyval([("isContainer", PyRep::Bool(false))])])),
            (
                "npcGroups",
                PyRep::Tuple(vec![
                    PyRep::keyval([("wave", PyRep::Integer(1)), ("npcs", PyRep::Tuple(vec![]))]),
                    PyRep::keyval([("wave", PyRep::Integer(2))]),
                ]),
            ),
            (
                "triggers",
                PyRep::Tuple(vec![
                    PyRep::keyval([("kind", PyRep::String("approach".into()))]),
                    PyRep::keyval([("kind", PyRep::String("loot".into()))]),
                    PyRep::keyval([("kind", PyRep::String("kill".into()))]),
                ]),
            ),
        ]),
        PyRep::keyval([("disallowMWD", PyRep::Bool(false))]),
    ]);
    let spawned = spawn_dungeon_instance(
        &ctx,
        char_id,
        Some(sys_id),
        &[PyRep::Integer(990_010), custom_rooms],
        &HashMap::new(),
    )
    .await
    .expect("spawn with positional args");
    let inst_id = spawned.dict_get("instanceID").unwrap().as_i64().unwrap();
    with_dungeon_runtime_store(&ctx, |store| {
        let inst = store.instances.get(&inst_id).unwrap();
        assert!(inst.disallow_mwd);
        assert_eq!(inst.rooms[0].objects[0].type_id, DEFAULT_CONTAINER_TYPE_ID);
        assert_eq!(inst.rooms[0].npc_groups[0].npc_specs, vec![(10_043, 1)]);
        assert!(inst.rooms[0].npc_groups[0].spawn_immediately);
        assert!(!inst.rooms[0].npc_groups[1].spawn_immediately);
    });

    // Empty rooms list falls back to build_sde_deadspace_rooms
    let mut kw_empty = HashMap::new();
    kw_empty.insert("rooms".into(), PyRep::List(vec![]));
    let fb_rep = spawn_dungeon_instance(&ctx, char_id, Some(sys_id), &[PyRep::Integer(990_010)], &kw_empty)
        .await
        .unwrap();
    let fb_id = fb_rep.dict_get("instanceID").unwrap().as_i64().unwrap();

    // 1. Unauthenticated session -> error
    let dng_tag = PyRep::String(format!("DNG-INST-{inst_id}"));
    let mut unauth_sess = SessionState::new(402);
    assert!(try_activate_dungeon_runtime_gate(&ctx, &mut unauth_sess, sys_id, &[dng_tag.clone()])
        .await
        .unwrap()
        .is_err());

    // 2. Docked in structure -> NotInSpace
    sess.structure_id = Some(1_040_999_001);
    assert_eq!(
        try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &[dng_tag.clone()])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInSpace"
    );
    sess.structure_id = None;

    // 3. Ship ball not spawned yet -> ShipNotInSpace
    assert_eq!(
        try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &[dng_tag.clone()])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // 4. Spawn ship ball with cruiser type 990_587 (rigSize 2.0, mass 1_000_000 <= 15M) near Room 1 gate
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let r1_gate_pos = DVec3::new(120_005_000.0, 0.0, 80_000_000.0);
    sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 990_587,
            position: r1_gate_pos,
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    assert_eq!(
        try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &[dng_tag.clone()])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipRestrictedByDungeonGate"
    );

    // 5. Switch dungeon_id to unrestricted, set last_completed_task_id, and activate via DNG-INST-{id}
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.remove(&fb_id);
        let inst = store.instances.get_mut(&inst_id).unwrap();
        inst.dungeon_id = 990_004; // difficulty 3
        inst.last_completed_task_id = Some(777);
    });
    let ok_kv = try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &[dng_tag])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ok_kv.dict_get("roomIndex").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(ok_kv.dict_get("disallowMWD").and_then(|v| v.as_bool()), Some(false));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approach_explicit_target_loot_wildcard_and_mwd_warp_boundary_checks() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id = 90_040_003i64;
    let ship_id = 1_040_000_003i64;
    let (ctx, _sess) = make_ctx_and_session(sys_id, char_id, ship_id);

    // Before store is initialized, approach and loot are no-ops
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);
    on_dungeon_item_looted(&ctx, char_id, Some(sys_id), 500, 34).await;

    let r1_pos = [50_000_000.0, 0.0, 0.0];
    let r2_pos = [60_000_000.0, 0.0, 0.0];
    let inst_id = 975_001i64;
    let beacon_ball_id = 880_001i64;
    let box_ball_id = 880_002i64;
    let r2_gate_ball_id = 880_003i64;

    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(
            inst_id,
            DungeonRuntimeInstance {
                instance_id: inst_id,
                dungeon_id: 2001,
                dungeon_name: "Boundary Complex".into(),
                category: "Deadspace".into(),
                archetype: "Deadspace".into(),
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: r1_pos,
                disallow_mwd: true,
                entry_gate_ball_id: None,
                entry_gate_pos: None,
                entry_gate_locked: false,
                current_room_index: 1,
                rooms: vec![
                    DungeonRuntimeRoom {
                        room_index: 1,
                        room_id: 200_101,
                        room_name: "Room 1".into(),
                        position: r1_pos,
                        gate_destination_pos: [r1_pos[0] - 10_000.0, 0.0, 0.0],
                        gated: false,
                        disallow_mwd: None, // inherits inst.disallow_mwd = true without explicit room flag
                        gate_ball_id: Some(r2_gate_ball_id),
                        gate_locked: true,
                        objects: vec![
                            DungeonRuntimeObject {
                                ball_id: beacon_ball_id,
                                type_id: 23,
                                name: "Beacon".into(),
                                position: r1_pos,
                                radius: 100.0,
                                is_container: false,
                                loot_items: vec![],
                                spawned: true,
                                ..Default::default()
                            },
                            DungeonRuntimeObject {
                                ball_id: box_ball_id,
                                type_id: 23,
                                name: "Box".into(),
                                position: r1_pos,
                                radius: 100.0,
                                is_container: true,
                                loot_items: vec![],
                                spawned: true,
                                ..Default::default()
                            },
                        ],
                        npc_groups: vec![DungeonNpcGroup {
                            group_id: 1,
                            wave: 1,
                            npc_specs: vec![],
                            npc_ball_ids: vec![],
                            spawn_immediately: true,
                            spawned: true,
                            cleared: true,
                            ..Default::default()
                        }],
                        triggers: vec![
                            DungeonTrigger {
                                trigger_id: 1,
                                kind: DungeonTriggerKind::OnApproach {
                                    target_ball_id: Some(beacon_ball_id),
                                    target_object_index: None,
                                    anchor_pos: r1_pos,
                                    radius_m: 2_500.0,
                                },
                                action: DungeonTriggerAction::UnlockGate,
                                fired: false,
                                ..Default::default()
                            },
                            DungeonTrigger {
                                trigger_id: 2,
                                kind: DungeonTriggerKind::OnLoot {
                                    container_ball_id: None,
                                    target_object_index: None,
                                    item_type_id: None, // wildcard item_type_id + room_has_cont check
                                },
                                action: DungeonTriggerAction::CompleteDungeon,
                                fired: false,
                                ..Default::default()
                            },
                        ],
                        spawned: true,
                        cleared: false,
                        entered: true,
                        ..Default::default()
                    },
                    DungeonRuntimeRoom {
                        room_index: 2,
                        room_id: 200_102,
                        room_name: "Room 2".into(),
                        position: r2_pos,
                        gate_destination_pos: [r2_pos[0] - 10_000.0, 0.0, 0.0],
                        gated: true,
                        disallow_mwd: Some(true),
                        gate_ball_id: Some(880_004),
                        gate_locked: false,
                        objects: vec![DungeonRuntimeObject {
                            ball_id: 880_005,
                            type_id: 23,
                            name: "R2 Obj".into(),
                            position: r2_pos,
                            radius: 100.0,
                            is_container: false,
                            loot_items: vec![],
                            spawned: false,
                            ..Default::default()
                        }],
                        npc_groups: vec![],
                        triggers: vec![],
                        spawned: false,
                        cleared: false,
                        ..Default::default()
                    },
                ],
                completed: false,
                despawned: false,
                active_objective_id: 0,
                last_completed_task_id: None,
                completed_task_ids: vec![],
                created_unix: 1_785_000_000,
                ..Default::default()
            },
        );
    });

    // Warp target blocked by target_ball_id in gated Room 2 even when target_pos is [0,0,0]
    assert_eq!(
        check_warp_target_blocked_by_gated_dungeon_room(&ctx, sys_id, DVec3::ZERO, Some(880_005))
            .unwrap_err()
            .code,
        "WarpBlockedByDeadspacePocket"
    );
    assert_eq!(
        check_warp_target_blocked_by_gated_dungeon_room(&ctx, sys_id, DVec3::ZERO, Some(880_004))
            .unwrap_err()
            .code,
        "WarpBlockedByDeadspacePocket"
    );
    // Warp target blocked by Room 2 gate_destination_pos within 150km
    assert_eq!(
        check_warp_target_blocked_by_gated_dungeon_room(
            &ctx,
            sys_id,
            DVec3::new(r2_pos[0] - 10_000.0, 140_000.0, 0.0),
            None,
        )
        .unwrap_err()
        .code,
        "WarpBlockedByDeadspacePocket"
    );

    // MWD pocket distance boundary: 250_000m is inside, 250_001m from both room_pos and gate_dest is outside
    assert!(is_ship_in_mwd_disallowed_dungeon_pocket(
        &ctx,
        sys_id,
        DVec3::new(r1_pos[0] + 250_000.0, 0.0, 0.0),
    ));
    assert!(!is_ship_in_mwd_disallowed_dungeon_pocket(
        &ctx,
        sys_id,
        DVec3::new(r1_pos[0] + 250_001.0, 0.0, 0.0),
    ));

    // Spawn ship at 4_000m from r1_pos:
    // - on_dungeon_approach with target_id_opt = None does NOT fire (4_000 > 2_500)
    // - on_dungeon_approach with target_id_opt = Some(beacon_ball_id) DOES fire (4_000 <= 5_000 explicit_target_close!)
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(r1_pos[0] + 4_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    // check_deadspace_mwd_disallowed when room.disallow_mwd is None delegates to check_deadspace_mwd_allowed
    assert_eq!(
        check_deadspace_mwd_disallowed(&ctx, &sys_handle, sys_id, ship_id)
            .await
            .unwrap_err()
            .code,
        "ModuleNotAllowedInDeadspace"
    );

    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);
    assert_eq!(
        on_dungeon_approach(&ctx, char_id, sys_id, ship_id, Some(beacon_ball_id)).await,
        1
    );
    with_dungeon_runtime_store(&ctx, |store| {
        assert!(!store.instances[&inst_id].rooms[0].gate_locked);
    });

    // Wildcard OnLoot trigger: wrong system or non-room container ignored, room container fires
    on_dungeon_item_looted(&ctx, char_id, Some(30_000_144), box_ball_id, 999).await;
    on_dungeon_item_looted(&ctx, char_id, None, 999_999, 999).await;
    with_dungeon_runtime_store(&ctx, |store| {
        assert!(!store.instances[&inst_id].completed);
    });
    on_dungeon_item_looted(&ctx, char_id, None, box_ball_id, 999).await;
    with_dungeon_runtime_store(&ctx, |store| {
        assert!(store.instances[&inst_id].completed);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a2_i4_locked_entry_gate_final_room_none_and_branching_gate_links() {
    use crate::services::pve_dungeon_runtime::{
        apply_dungeon_trigger_actions, has_runtime_gate_target, resolve_runtime_gate_in_snap, DungeonGateLink,
    };
    use crate::services::pve_exploration::handle_pve_exploration;

    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_222;
    let ship_id: i64 = 1_040_000_222;
    let (mut ctx, mut sess) = make_ctx_and_session(sys_id, char_id, ship_id);
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    sess.user_id = Some(uid);
    let _ = ctx.db.acquire_online_lock_with_computer_hash(uid, char_id as u64, Some(char_id), None, true, 1_785_000_000);
    let mut sde = (*ctx.sde).clone();
    sde.dungeons.insert(990_222, DungeonTemplate {
        dungeon_id: 990_222, name: "Branching I4 Sanctum".into(), faction_id: Some(500_010), archetype_id: Some(20), difficulty: Some(2),
    });
    ctx.sde = Arc::new(sde);

    let (inst_id, entry_gid, gate_a_id, gate_b_id) = (980_222i64, 88_900_001i64, 88_900_002i64, 88_900_003i64);
    let (entry_pos, r1_pos, r2_pos, r2_land, r3_pos, r3_land, ga_pos, gb_pos) = (
        [10_000_000.0, 0.0, 0.0], [11_000_000.0, 0.0, 0.0], [12_000_000.0, 0.0, 0.0], [12_010_000.0, 500.0, 0.0],
        [13_000_000.0, 0.0, 0.0], [13_020_000.0, 0.0, -500.0], [11_005_000.0, 0.0, 0.0], [11_009_000.0, 0.0, 0.0],
    );
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(inst_id, DungeonRuntimeInstance {
            instance_id: inst_id, dungeon_id: 990_222, dungeon_name: "Branching I4 Sanctum".into(),
            category: "Deadspace".into(), archetype: "Deadspace".into(), solar_system_id: sys_id, owner_char_id: char_id,
            base_position: r1_pos, entry_gate_ball_id: Some(entry_gid), entry_gate_pos: Some(entry_pos),
            entry_gate_locked: true, current_room_index: 0,
            rooms: vec![
                DungeonRuntimeRoom {
                    room_index: 1, room_id: 9221, room_name: "Fork Chamber".into(), position: r1_pos,
                    gate_destination_pos: [11_001_000.0, 0.0, 0.0], gated: true, gate_ball_id: Some(gate_a_id),
                    gate_locked: false, entered: true, spawned: true,
                    gate_links: vec![
                        DungeonGateLink { gate_ball_id: Some(gate_a_id), gate_position: ga_pos, target_room_index: 1, destination_pos: r2_land, locked: false, label: "East".into(), spawned: true, ..Default::default() },
                        DungeonGateLink { gate_ball_id: Some(gate_b_id), gate_position: gb_pos, target_room_index: 2, destination_pos: r3_land, locked: true, label: "West".into(), spawned: true, ..Default::default() },
                    ],
                    ..Default::default()
                },
                DungeonRuntimeRoom { room_index: 2, room_id: 9222, room_name: "East".into(), position: r2_pos, gate_destination_pos: r2_land, gated: true, ..Default::default() },
                DungeonRuntimeRoom { room_index: 3, room_id: 9223, room_name: "West Final".into(), position: r3_pos, gate_destination_pos: r3_land, gated: true, ..Default::default() },
            ],
            created_unix: 1_785_000_000, ..Default::default()
        });
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (bid, tid, pos) in [(ship_id, 587u32, entry_pos), (entry_gid, 17831, entry_pos), (gate_a_id, 17831, ga_pos), (gate_b_id, 17831, gb_pos)] {
        sys_handle.spawn_ball(Ball {
            id: bid, type_id: tid, position: DVec3::from_array(pos), velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await.unwrap();
    }

    // OWN-R5-03 + I4: locked entry gate rejects DNG-INST-<id> through ActivateAccelerationGate even if Room 1 gate_locked == false.
    let dng_tag = format!("DNG-INST-{inst_id}");
    assert!(has_runtime_gate_target(&ctx, sys_id, None, Some(&dng_tag)));
    assert_eq!(handle_pve_exploration(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::String(dng_tag.clone())], &HashMap::new()).await.unwrap().unwrap_err().code, "GateLocked");

    assert_eq!(apply_dungeon_trigger_actions(&ctx, sys_id, inst_id, 0, vec![(1, DungeonTriggerAction::UnlockEntryGate)], char_id).await, 1);
    let r1 = try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &[PyRep::String(dng_tag.clone())]).await.unwrap().unwrap();
    assert_eq!(r1.dict_get("roomIndex").and_then(|v| v.as_i64()), Some(1));

    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(gb_pos)).await;
    assert_eq!(try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &[PyRep::Integer(gate_b_id)]).await.unwrap().unwrap_err().code, "GateLocked");
    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(ga_pos)).await;
    let ra = try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &[PyRep::Integer(gate_a_id)]).await.unwrap().unwrap();
    assert_eq!(ra.dict_get("roomIndex").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(ra.dict_get("destinationX").and_then(|v| v.as_f64()), Some(r2_land[0]));

    assert_eq!(apply_dungeon_trigger_actions(&ctx, sys_id, inst_id, 0, vec![(2, DungeonTriggerAction::UnlockGate)], char_id).await, 1);
    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(gb_pos)).await;
    let rb = handle_pve_exploration(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_b_id)], &HashMap::new()).await.unwrap().unwrap();
    assert_eq!(rb.dict_get("roomIndex").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(rb.dict_get("destinationX").and_then(|v| v.as_f64()), Some(r3_land[0]));

    // I4: final room DNG-INST-<id> returns None instead of fabricating a phantom gate.
    assert!(!has_runtime_gate_target(&ctx, sys_id, None, Some(&dng_tag)));
    assert!(ctx.db.with_snapshot(|s| resolve_runtime_gate_in_snap(s, sys_id, None, Some(&dng_tag))).is_none());
    assert!(try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &[PyRep::String(dng_tag)]).await.is_none());
}
