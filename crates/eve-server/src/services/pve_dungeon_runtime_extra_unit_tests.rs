//! Extra unit and mutation-killing tests for `services::pve_dungeon_runtime`
//! covering room-level MWD override precedence, intra-pocket vs cross-pocket warp checks,
//! escalation & FW complex warp target resolution, entry gate materialization,
//! all `DungeonTriggerAction` variants, `target_ball_id` specific `OnKill` triggers,
//! and final-room auto-clear/auto-completion without explicit triggers.

use crate::router::ServerContext;
use crate::services::pve_dungeon_editor::{
    with_dungeon_editor_store, DungeonEditorRoom, GmActiveEditorState,
};
use crate::services::pve_dungeon_runtime::*;
use crate::services::pve_exploration::{
    with_pve_exploration_reg, EscalatingPathEntry, PveSiteInstance,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{SystemDynamicState, WorldRepository};
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_runtime_ctx(sys_id: SolarSystemId, char_id: i64) -> ServerContext {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("dng_extra", "pw", char_id, "DngExtra");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = 1_030_000_001;
        }
    });
    let uid = db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id).unwrap_or(1));
    let _ = db.acquire_online_lock_with_computer_hash(
        uid,
        char_id as u64,
        Some(char_id),
        None,
        true,
        1_780_000_000,
    );
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_780_000_000)));
    ctx
}

#[test]
fn mwd_override_precedence_intra_pocket_warp_and_escalation_fw_warp_targets() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id = 90_030_001i64;
    let ctx = make_runtime_ctx(sys_id, char_id);

    // 1. Verify dist3, unix_to_filetime_ticks, and default store id
    assert!((dist3([0.0, 0.0, 0.0], [3.0, 4.0, 12.0]) - 13.0).abs() < 1e-9);
    assert_eq!(unix_to_filetime_ticks(0), 116_444_736_000_000_000);
    assert_eq!(DungeonRuntimeStore::default().next_instance_id, 950_001);

    // 2. Instance has disallow_mwd = true, but Room 1 explicitly sets disallow_mwd = Some(false) -> MWD allowed!
    //    Room 2 has disallow_mwd = None -> falls back to inst.disallow_mwd = true -> MWD blocked!
    let r1_pos = [1_000_000.0, 0.0, 0.0];
    let r2_pos = [2_000_000.0, 0.0, 0.0];
    let entry_pos = [500_000.0, 0.0, 0.0];
    let inst = DungeonRuntimeInstance {
        instance_id: 960_001,
        dungeon_id: 1,
        dungeon_name: "Override Complex".into(),
        category: "Deadspace".into(),
        archetype: "Deadspace".into(),
        solar_system_id: sys_id,
        owner_char_id: char_id,
        base_position: r1_pos,
        disallow_mwd: true,
        entry_gate_ball_id: None,
        entry_gate_pos: Some(entry_pos),
        entry_gate_locked: false,
        current_room_index: 1,
        rooms: vec![
            DungeonRuntimeRoom {
                room_index: 1,
                room_id: 6001,
                room_name: "Room 1".into(),
                position: r1_pos,
                gate_destination_pos: r2_pos,
                gated: false,
                disallow_mwd: Some(false),
                gate_ball_id: None,
                gate_locked: false,
                objects: Vec::new(),
                npc_groups: Vec::new(),
                triggers: Vec::new(),
                spawned: false,
                cleared: false,
                ..Default::default()
            },
            DungeonRuntimeRoom {
                room_index: 2,
                room_id: 6002,
                room_name: "Room 2".into(),
                position: r2_pos,
                gate_destination_pos: r2_pos,
                gated: true,
                disallow_mwd: None,
                gate_ball_id: None,
                gate_locked: false,
                objects: Vec::new(),
                npc_groups: Vec::new(),
                triggers: Vec::new(),
                spawned: false,
                cleared: false,
                ..Default::default()
            },
        ],
        completed: false,
        despawned: false,
        active_objective_id: 0,
        last_completed_task_id: None,
        completed_task_ids: Vec::new(),
        created_unix: 1_780_000_000,
        ..Default::default()
    };

    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(960_001, inst);
    });

    // Room 1: disallow_mwd = Some(false) overrides inst.disallow_mwd = true
    assert!(check_deadspace_mwd_allowed(&ctx, sys_id, [1_050_000.0, 0.0, 0.0]).is_ok());
    // Room 2: disallow_mwd = None inherits inst.disallow_mwd = true
    assert_eq!(
        check_deadspace_mwd_allowed(&ctx, sys_id, [2_050_000.0, 0.0, 0.0]).unwrap_err().code,
        "ModuleNotAllowedInDeadspace"
    );

    // Warping within the SAME ungated pocket (Room 1 -> Room 1) is allowed
    assert!(check_deadspace_warp_allowed(
        &ctx,
        sys_id,
        [1_010_000.0, 0.0, 0.0],
        [1_050_000.0, 0.0, 0.0],
        [1_050_000.0, 0.0, 0.0]
    )
    .is_ok());

    // find_active_dungeon_id_at_position matches entry_gate_pos within 150km
    assert_eq!(find_active_dungeon_id_at_position(&ctx, sys_id, [550_000.0, 0.0, 0.0]), Some(1));
    assert_eq!(find_active_dungeon_id_at_position(&ctx, sys_id, [99_000_000.0, 0.0, 0.0]), None);

    // 3. resolve_deadspace_or_escalation_warp_target:
    // - Instance with entry_gate_pos returns entry_pos
    let ep_res = resolve_deadspace_or_escalation_warp_target(&ctx, char_id, sys_id, "dungeon", 960_001, None)
        .unwrap()
        .unwrap();
    assert_eq!(ep_res, DVec3::from_array(entry_pos));

    // - Ungated room_id 6001 returns r1_pos
    let r1_res = resolve_deadspace_or_escalation_warp_target(&ctx, char_id, sys_id, "dungeon", 6001, None)
        .unwrap()
        .unwrap();
    assert_eq!(r1_res, DVec3::from_array(r1_pos));

    // - "epinstance" resolves active escalation site EXP-{instance_id}-{step_index}
    with_pve_exploration_reg(&ctx, |reg| {
        reg.escalations_by_char.entry(char_id).or_default().push(EscalatingPathEntry {
            instance_id: 77_701,
            character_id: char_id,
            source_dungeon_id: 10,
            expedition_dungeon_id: 20,
            expedition_name: "Escalation Step 2".into(),
            destination_solar_system_id: sys_id,
            step_index: 2,
            total_steps: 4,
            journal_text: String::new(),
            created_at_sec: 1_780_000_000,
            expires_at_sec: 1_780_086_400,
            dungeons_entered: Vec::new(),
            journal_entry_template_id: 0,
        });
        reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
            site_id: "EXP-77701-2".into(),
            solar_system_id: sys_id,
            site_category: "Expedition".into(),
            dungeon_id: 20,
            dungeon_name: "Escalation Step 2".into(),
            position: [7_000_000.0, 100.0, 0.0],
            ..Default::default()
        });
    });
    let esc_pos = resolve_deadspace_or_escalation_warp_target(&ctx, char_id, sys_id, "epinstance", 77_701, None)
        .unwrap()
        .unwrap();
    assert_eq!(esc_pos, DVec3::new(7_000_000.0, 100.0, 0.0));

    // - FW complex warp target resolution via fw_runtime.fw_complexes + system_states.fw_complex_position
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime
            .fw_complexes
            .insert(88_801, (sys_id, "Medium".into(), 500_001, 100.0, false));
        let mut st = SystemDynamicState::default();
        st.fw_complex_position = Some([12_000_000.0, 0.0, 0.0]);
        snap.system_states.insert(sys_id, st);
    });
    let fw_pos = resolve_deadspace_or_escalation_warp_target(&ctx, char_id, sys_id, "location", 88_801, None)
        .unwrap()
        .unwrap();
    assert_eq!(fw_pos, DVec3::new(12_000_000.0, 0.0, 0.0));

    // - Unknown stuff_type returns Ok(None)
    assert!(
        resolve_deadspace_or_escalation_warp_target(&ctx, char_id, sys_id, "unknown_type", 960_001, None)
            .unwrap()
            .is_none()
    );

    // 4. GM editor room-level disallow_mwd check at origin [0, 0, 0]
    with_dungeon_editor_store(&ctx, |ed| {
        ed.rooms_by_dungeon.insert(
            555,
            vec![DungeonEditorRoom {
                room_id: 555_01,
                room_name: "EdRoom".into(),
                dungeon_id: 555,
                disallow_mwd: true,
                objects: Vec::new(),
            }],
        );
        ed.active_editors.insert(
            char_id,
            GmActiveEditorState {
                solar_system_id: Some(sys_id),
                dungeon_id: Some(555),
                room_id: Some(555_01),
                ..Default::default()
            },
        );
    });
    assert_eq!(
        check_deadspace_mwd_allowed(&ctx, sys_id, [1_000.0, 0.0, 0.0]).unwrap_err().code,
        "ModuleNotAllowedInDeadspace"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn materialize_errors_entry_gate_all_trigger_actions_and_boss_kill_auto_clear() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id = 90_030_002i64;
    let ctx = make_runtime_ctx(sys_id, char_id);

    // 1. materialize_dungeon_room_entities error branches: DungeonInstanceNotFound
    assert_eq!(
        materialize_dungeon_room_entities(&ctx, sys_id, 999_999_999, 0)
            .await
            .unwrap_err()
            .code,
        "DungeonInstanceNotFound"
    );

    // 2. Build a 1-room dungeon with entry_gate_pos, owner_char_id = 0 (fallback container owner 500_010),
    //    type_id = 0 (fallback DEFAULT_CONTAINER_TYPE_ID = 23), and a boss-specific OnKill trigger
    let r1_pos = [3_000_000.0, 0.0, 0.0];
    let inst = DungeonRuntimeInstance {
        instance_id: 960_010,
        dungeon_id: 20,
        dungeon_name: "Boss Pocket".into(),
        category: "Expedition".into(),
        archetype: "Deadspace".into(),
        solar_system_id: sys_id,
        owner_char_id: 0,
        base_position: r1_pos,
        disallow_mwd: false,
        entry_gate_ball_id: None,
        entry_gate_pos: Some([2_500_000.0, 0.0, 0.0]),
        entry_gate_locked: false,
        current_room_index: 1,
        rooms: vec![DungeonRuntimeRoom {
            room_index: 1,
            room_id: 7001,
            room_name: "Boss Room".into(),
            position: r1_pos,
            gate_destination_pos: r1_pos,
            gated: false,
            disallow_mwd: None,
            gate_ball_id: None,
            gate_locked: false,
            objects: vec![DungeonRuntimeObject {
                ball_id: 0,
                type_id: 0, // falls back to DEFAULT_CONTAINER_TYPE_ID (23)
                name: "Faction Box".into(),
                position: r1_pos,
                radius: 50.0,
                is_container: true,
                loot_items: vec![(34, 100)],
                spawned: false,
                ..Default::default()
            }],
            npc_groups: vec![DungeonNpcGroup {
                group_id: 1,
                wave: 1,
                npc_specs: vec![(48_090, 2)],
                npc_ball_ids: Vec::new(),
                spawn_immediately: true,
                spawned: false,
                cleared: false,
                ..Default::default()
            }],
            triggers: Vec::new(), // no explicit triggers -> tests auto-clear & auto-complete on last NPC killed!
            spawned: false,
            cleared: false,
            ..Default::default()
        }],
        completed: false,
        despawned: false,
        active_objective_id: 0,
        last_completed_task_id: None,
        completed_task_ids: Vec::new(),
        created_unix: 1_780_000_000,
        ..Default::default()
    };

    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(960_010, inst);
    });

    // Out-of-bounds room_vec_idx fails with DungeonRoomNotFound
    assert!(materialize_dungeon_room_entities(&ctx, sys_id, 960_010, 5).await.is_err());

    // Materialize room 0 -> spawns entry gate, container (owner 500_010, type 23), and 2 NPCs
    let MaterializedDungeonRoomEntities {
        object_ids: obj_ids,
        npc_ids,
        next_gate_id: gate_opt,
    } = materialize_dungeon_room_entities(&ctx, sys_id, 960_010, 0)
        .await
        .unwrap();
    assert_eq!(obj_ids.len(), 1);
    assert_eq!(npc_ids.len(), 2);
    assert!(gate_opt.is_none(), "single-room dungeon has no internal next-room gate");

    ctx.db.with_snapshot(|snap| {
        let cont = &snap.items[&obj_ids[0]];
        assert_eq!(cont.type_id, DEFAULT_CONTAINER_TYPE_ID);
        assert_eq!(cont.owner_id, 500_010);
    });
    with_dungeon_runtime_store(&ctx, |store| {
        assert!(store.instances[&960_010].entry_gate_ball_id.is_some());
    });
    on_dungeon_room_entered(&ctx, sys_id, 960_010, 0, char_id).await;

    // 3. Test apply_dungeon_trigger_actions with all presentation trigger actions:
    //    CameraInterest, TriggerAmbientSound, TriggerAudio, TriggerMessage, TriggerMusic
    let fired = apply_dungeon_trigger_actions(
        &ctx,
        sys_id,
        960_010,
        0,
        vec![
            (
                1,
                DungeonTriggerAction::CameraInterest {
                    item_id: obj_ids[0],
                    speed_bits: 1.0f64.to_bits(),
                    duration_bits: 5.0f64.to_bits(),
                },
            ),
            (
                2,
                DungeonTriggerAction::TriggerAmbientSound {
                    sound_id: 1001,
                    entity_item_id: obj_ids[0],
                    emitter_id: 2001,
                },
            ),
            (
                3,
                DungeonTriggerAction::TriggerAudio {
                    audio_url: "wise:/dungeon_intro".into(),
                },
            ),
            (
                4,
                DungeonTriggerAction::TriggerMessage {
                    message_type: 1,
                    message_id: 500,
                },
            ),
            (
                5,
                DungeonTriggerAction::TriggerMusic {
                    music_trigger_id: 900,
                },
            ),
        ],
        char_id,
    )
    .await;
    assert_eq!(fired, 5);

    // 4. Add a target_ball_id-specific OnKill trigger for npc_ids[0] (UnlockGate), kill npc_ids[0], then kill npc_ids[1] -> auto-completes dungeon!
    with_dungeon_runtime_store(&ctx, |store| {
        let r = &mut store.instances.get_mut(&960_010).unwrap().rooms[0];
        r.gate_locked = true;
        r.triggers.push(DungeonTrigger {
            trigger_id: 50,
            kind: DungeonTriggerKind::OnKill {
                target_npc_group: None,
                target_ball_id: Some(npc_ids[0]),
            },
            action: DungeonTriggerAction::UnlockGate,
            fired: false,
            ..Default::default()
        });
    });

    on_dungeon_npc_killed(&ctx, sys_id, npc_ids[0], char_id).await;
    with_dungeon_runtime_store(&ctx, |store| {
        let inst = &store.instances[&960_010];
        assert!(!inst.rooms[0].gate_locked, "killing target_ball_id should unlock gate");
        assert!(!inst.completed, "1 NPC still alive");
    });

    // Loot container before final NPC kill so completion triggers immediate full despawn rather
    // than entering loot-grace retention (`PVE-207`).
    ctx.db
        .with_mut_snapshot(|snap| snap.items.retain(|_, item| item.location_id != obj_ids[0]));

    // Kill final NPC -> all spawned dead, all triggers fired, final room -> auto-completes and despawns!
    on_dungeon_npc_killed(&ctx, sys_id, npc_ids[1], char_id).await;
    with_dungeon_runtime_store(&ctx, |store| {
        let inst = &store.instances[&960_010];
        assert!(inst.completed);
        assert!(inst.despawned);
    });

    // Materializing an already completed dungeon fails with DungeonAlreadyCompleted
    assert!(materialize_dungeon_room_entities(&ctx, sys_id, 960_010, 0).await.is_err());

    // 5. Landing at PveExplorationRegistry site pocket updates escalationungeons_entered and emits room entry
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
            site_id: "EXP-SITE-99".into(),
            solar_system_id: sys_id,
            site_category: "Expedition".into(),
            dungeon_id: 20,
            dungeon_name: "Escalation Site".into(),
            position: [15_000_000.0, 0.0, 0.0],
            current_wave: 1,
            total_waves: 3,
            created_unix: 1_780_000_000,
            lifetime_seconds: 0,
            expedition_instance_id: Some(77_701),
            ..Default::default()
        });
        reg.escalations_by_char.entry(char_id).or_default().push(EscalatingPathEntry {
            instance_id: 77_701,
            character_id: char_id,
            source_dungeon_id: 10,
            expedition_dungeon_id: 20,
            expedition_name: "Escalation Site".into(),
            destination_solar_system_id: sys_id,
            step_index: 1,
            total_steps: 3,
            journal_text: String::new(),
            created_at_sec: 1_780_000_000,
            expires_at_sec: 1_780_086_400,
            dungeons_entered: Vec::new(),
            journal_entry_template_id: 0,
        });
    });
    on_warp_landed_at_dungeon_pocket_if_present(&ctx, char_id, sys_id, [15_000_000.0, 0.0, 0.0]).await;
    with_pve_exploration_reg(&ctx, |reg| {
        assert!(reg.escalations_by_char[&char_id][0].dungeons_entered.contains(&20));
    });
}

#[test]
fn warp_guards_leave_suppression_bookmarks_and_exploration_site_targets() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id = 90_030_101i64;
    let other_char = 90_030_102i64;
    let ctx = make_runtime_ctx(sys_id, char_id);

    let r1_pos = [10_000_000.0, 0.0, 0.0];
    let r2_pos = [20_000_000.0, 0.0, 0.0];
    let inst = DungeonRuntimeInstance {
        instance_id: 970_001,
        dungeon_id: 42,
        dungeon_name: "Gated Start".into(),
        category: "Combat".into(),
        archetype: "Deadspace".into(),
        solar_system_id: sys_id,
        owner_char_id: char_id,
        base_position: r1_pos,
        disallow_mwd: false,
        entry_gate_ball_id: None,
        entry_gate_pos: None,
        entry_gate_locked: false,
        current_room_index: 1,
        rooms: vec![
            DungeonRuntimeRoom {
                room_index: 1,
                room_id: 8001,
                room_name: "Gated Entry".into(),
                position: r1_pos,
                gate_destination_pos: r2_pos,
                gated: true,
                disallow_mwd: None,
                gate_ball_id: None,
                gate_locked: false,
                objects: Vec::new(),
                npc_groups: Vec::new(),
                triggers: Vec::new(),
                spawned: false,
                cleared: false,
                ..Default::default()
            },
            DungeonRuntimeRoom {
                room_index: 2,
                room_id: 8002,
                room_name: "Open Exit".into(),
                position: r2_pos,
                gate_destination_pos: r2_pos,
                gated: false,
                disallow_mwd: None,
                gate_ball_id: None,
                gate_locked: false,
                objects: Vec::new(),
                npc_groups: Vec::new(),
                triggers: Vec::new(),
                spawned: false,
                cleared: false,
                ..Default::default()
            },
        ],
        completed: false,
        despawned: false,
        active_objective_id: 0,
        last_completed_task_id: None,
        completed_task_ids: Vec::new(),
        created_unix: 1_780_100_000,
        ..Default::default()
    };
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(970_001, inst);
    });

    // 1. Exact radius boundary (200_000.0 blocked vs 200_000.1 allowed) + anchor_pos check
    assert_eq!(
        check_deadspace_warp_allowed(
            &ctx,
            sys_id,
            [0.0; 3],
            [0.0; 3],
            [10_200_000.0, 0.0, 0.0],
        )
        .unwrap_err()
        .code,
        "NaturalPhenomenaDisruptingWarp"
    );
    assert_eq!(
        check_deadspace_warp_allowed(
            &ctx,
            sys_id,
            [0.0; 3],
            [10_200_000.0, 0.0, 0.0], // anchor in gated room even if dest outside
            [50_000_000.0, 0.0, 0.0],
        )
        .unwrap_err()
        .code,
        "NaturalPhenomenaDisruptingWarp"
    );
    assert!(check_deadspace_warp_allowed(
        &ctx,
        sys_id,
        [0.0; 3],
        [10_200_000.1, 0.0, 0.0],
        [10_200_000.1, 0.0, 0.0],
    )
    .is_ok());

    // 2. Resolving instance/dungeon whose first_room.gated == true & entry_gate_pos == None fails!
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(&ctx, char_id, sys_id, "dungeon", 42, None)
            .unwrap_err()
            .code,
        "NaturalPhenomenaDisruptingWarp" // rpc-lint: allow test fixture
    );

    // 3. on_warp_start_leave_dungeon_if_needed: intra-dungeon warp & open-space-to-dungeon suppress notice; leaving emits once
    let _ = ctx.drain_notifications_for_char(char_id);
    on_warp_start_leave_dungeon_if_needed(&ctx, char_id, sys_id, r1_pos, [10_010_000.0, 0.0, 0.0]);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty(), "same pocket/dungeon warp must not emit OnExitingDungeon");

    on_warp_start_leave_dungeon_if_needed(&ctx, char_id, sys_id, [50_000_000.0, 0.0, 0.0], r2_pos);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty(), "entering from open space must not emit OnExitingDungeon");

    on_warp_start_leave_dungeon_if_needed(&ctx, char_id, sys_id, r2_pos, [50_000_000.0, 0.0, 0.0]);
    let exit_notes = ctx.drain_notifications_for_char(char_id);
    assert!(exit_notes.iter().any(|n| n.event == "OnExitingDungeon"));

    // Completing/despawning instance stops blocking warps into that coordinate
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.get_mut(&970_001).unwrap().despawned = true;
    });
    assert!(check_deadspace_warp_allowed(&ctx, sys_id, [0.0; 3], r1_pos, r1_pos).is_ok());

    // 4. Bookmark system/owner filtering and PveExplorationRegistry string site lookup (ignoring cleared)
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            77_101,
            eve_db::BookmarkRecord {
                bookmark_id: 77_101,
                owner_id: other_char,
                label: "WrongOwner".into(),
                solar_system_id: sys_id,
                x: 1.0,
                y: 2.0,
                z: 3.0,
                item_id: Some(99_901),
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            77_102,
            eve_db::BookmarkRecord {
                bookmark_id: 77_102,
                owner_id: char_id,
                label: "WrongSys".into(),
                solar_system_id: 30_000_144,
                x: 4.0,
                y: 5.0,
                z: 6.0,
                item_id: Some(99_901),
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    assert!(resolve_deadspace_or_escalation_warp_target(&ctx, char_id, sys_id, "epinstance", 99_901, None)
        .unwrap()
        .is_none());

    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(sys_id).or_default().extend([
            PveSiteInstance {
                site_id: "EXP-CLEARED".into(),
                solar_system_id: sys_id,
                site_category: "Deadspace".into(),
                dungeon_id: 777,
                position: [1.0, 2.0, 3.0],
                cleared: true,
                ..Default::default()
            },
            PveSiteInstance {
                site_id: "EXP-LIVE".into(),
                solar_system_id: sys_id,
                site_category: "Deadspace".into(),
                dungeon_id: 778,
                position: [8_000_000.0, -100.0, 250.0],
                cleared: false,
                ..Default::default()
            },
        ]);
    });
    assert!(resolve_deadspace_or_escalation_warp_target(&ctx, char_id, sys_id, "dungeon", 0, Some("EXP-CLEARED"))
        .unwrap()
        .is_none());
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(&ctx, char_id, sys_id, "dungeon", 0, Some("EXP-LIVE"))
            .unwrap(),
        Some(DVec3::new(8_000_000.0, -100.0, 250.0))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn materialize_idempotency_static_objects_and_despawn_cleanup_guards() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id = 90_030_201i64;
    let ctx = make_runtime_ctx(sys_id, char_id);

    // Nonexistent despawn is clean no-op
    despawn_completed_dungeon(&ctx, sys_id, 999_111_222, char_id).await;
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    let r1_pos = [30_000_000.0, 0.0, 0.0];
    let r2_pos = [31_000_000.0, 0.0, 0.0];
    let inst = DungeonRuntimeInstance {
        instance_id: 970_010,
        dungeon_id: 313,
        dungeon_name: "Static Monument Complex".into(),
        category: "Combat".into(),
        archetype: "Deadspace".into(),
        solar_system_id: sys_id,
        owner_char_id: char_id,
        base_position: r1_pos,
        disallow_mwd: false,
        entry_gate_ball_id: None,
        entry_gate_pos: Some([29_500_000.0, 0.0, 0.0]),
        entry_gate_locked: false,
        current_room_index: 1,
        rooms: vec![
            DungeonRuntimeRoom {
                room_index: 1,
                room_id: 8101,
                room_name: "Atrium".into(),
                position: r1_pos,
                gate_destination_pos: r2_pos,
                gated: false,
                disallow_mwd: None,
                gate_ball_id: None,
                gate_locked: false,
                objects: vec![
                    DungeonRuntimeObject {
                        ball_id: 0,
                        type_id: 17_831,
                        name: "NonContainerMonument".into(),
                        position: [r1_pos[0] + 500.0, 0.0, 0.0],
                        radius: 150.0,
                        is_container: false, // NOT a container -> ball only, no snap.items!
                        loot_items: Vec::new(),
                        spawned: false,
                        ..Default::default()
                    },
                    DungeonRuntimeObject {
                        ball_id: 0,
                        type_id: DEFAULT_CONTAINER_TYPE_ID,
                        name: "LootCan".into(),
                        position: [r1_pos[0] + 900.0, 0.0, 0.0],
                        radius: 100.0,
                        is_container: true,
                        loot_items: vec![(34, 250)],
                        spawned: false,
                        ..Default::default()
                    },
                ],
                npc_groups: vec![DungeonNpcGroup {
                    group_id: 1,
                    wave: 1,
                    npc_specs: vec![(48_090, 2)],
                    npc_ball_ids: Vec::new(),
                    spawn_immediately: true,
                    spawned: false,
                    cleared: false,
                    ..Default::default()
                }],
                triggers: Vec::new(),
                spawned: false,
                cleared: false,
                ..Default::default()
            },
            DungeonRuntimeRoom {
                room_index: 2,
                room_id: 8102,
                room_name: "Sanctum".into(),
                position: r2_pos,
                gate_destination_pos: r2_pos,
                gated: true,
                disallow_mwd: None,
                gate_ball_id: None,
                gate_locked: false,
                objects: Vec::new(),
                npc_groups: Vec::new(),
                triggers: Vec::new(),
                spawned: false,
                cleared: false,
                ..Default::default()
            },
        ],
        completed: false,
        despawned: false,
        active_objective_id: 0,
        last_completed_task_id: None,
        completed_task_ids: Vec::new(),
        created_unix: 1_780_100_000,
        ..Default::default()
    };
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(970_010, inst);
    });
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
            site_id: "DNG-STATIC-313".into(),
            solar_system_id: sys_id,
            site_category: "Deadspace".into(),
            dungeon_id: 313,
            position: r1_pos,
            ..Default::default()
        });
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures
            .entry(sys_id)
            .or_default()
            .push(eve_db::CosmicSignatureRecord {
                signature_id: "DNG-STATIC-313".into(),
                solar_system_id: sys_id,
                sig_group: "Combat Site".into(),
                type_name: "Static Monument Complex".into(),
                position: r1_pos,
                signal_strength: 100.0,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: 1_780_100_000,
                lifetime_seconds: 86_400,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            });
    });

    // First materialization
    let MaterializedDungeonRoomEntities {
        object_ids: objs1,
        npc_ids: npcs1,
        next_gate_id: gate1,
    } = materialize_dungeon_room_entities(&ctx, sys_id, 970_010, 0)
        .await
        .unwrap();
    assert_eq!(objs1.len(), 2);
    assert_eq!(npcs1.len(), 2);
    assert!(gate1.is_some());

    // Non-container object must NOT be in snap.items, while container + loot MUST be in snap.items
    let items_after_first = ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&objs1[0]));
        assert!(snap.items.contains_key(&objs1[1]));
        snap.items.len()
    });

    // Second materialization of same room -> idempotent! No new items/NPCs/balls
    let MaterializedDungeonRoomEntities {
        object_ids: objs2,
        npc_ids: npcs2,
        next_gate_id: gate2,
    } = materialize_dungeon_room_entities(&ctx, sys_id, 970_010, 0)
        .await
        .unwrap();
    assert_eq!(objs2, objs1);
    assert_eq!(npcs2, npcs1);
    assert_eq!(gate2, gate1);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.len(), items_after_first, "rematerialize must not duplicate loot");
    });

    let entry_gate_id = with_dungeon_runtime_store(&ctx, |store| store.instances[&970_010].entry_gate_ball_id.unwrap());
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert!(sys_handle.get_ball_snapshot(entry_gate_id).await.unwrap().is_some());
    assert!(sys_handle.get_ball_snapshot(gate1.unwrap()).await.unwrap().is_some());
    assert!(sys_handle.get_ball_snapshot(objs1[0]).await.unwrap().is_some());
    assert!(sys_handle.get_ball_snapshot(npcs1[0]).await.unwrap().is_some());

    // Explicit despawn cleans all balls, marks exploration site cleared, collapses cosmic signature, emits OnDungeonCompleted ONCE
    let _ = ctx.drain_notifications_for_char(char_id);
    despawn_completed_dungeon(&ctx, sys_id, 970_010, char_id).await;
    assert!(sys_handle.get_ball_snapshot(entry_gate_id).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(gate1.unwrap()).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(objs1[0]).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(npcs1[0]).await.unwrap().is_none());
    ctx.db.with_snapshot(|snap| {
        assert!(snap.cosmic_signatures[&sys_id][0].collapsed);
    });
    with_pve_exploration_reg(&ctx, |reg| {
        assert!(reg.sites_by_system[&sys_id][0].cleared);
    });
    assert_eq!(
        ctx.drain_notifications_for_char(char_id)
            .iter()
            .filter(|n| n.event == "OnDungeonCompleted")
            .count(),
        1
    );

    // Calling despawn again on already despawned instance must NOT emit duplicate OnDungeonCompleted
    despawn_completed_dungeon(&ctx, sys_id, 970_010, char_id).await;
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn materialize_room_preserves_positive_ball_id_and_increments_next_entity_id_monotonically() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id = 90_030_301i64;
    let ctx = make_runtime_ctx(sys_id, char_id);
    let base_id = ctx
        .db
        .with_snapshot(|snap| snap.next_entity_id)
        .max(250_000_000);
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id = base_id;
    });

    let r1_pos = [40_000_000.0, 0.0, 0.0];
    let r2_pos = [41_000_000.0, 0.0, 0.0];
    let inst = DungeonRuntimeInstance {
        instance_id: 970_020,
        dungeon_id: 314,
        dungeon_name: "Sequential ID Test Complex".into(),
        category: "Combat".into(),
        archetype: "Deadspace".into(),
        solar_system_id: sys_id,
        owner_char_id: char_id,
        base_position: r1_pos,
        disallow_mwd: false,
        entry_gate_ball_id: None,
        entry_gate_pos: Some([39_500_000.0, 0.0, 0.0]),
        entry_gate_locked: false,
        current_room_index: 1,
        rooms: vec![
            DungeonRuntimeRoom {
                room_index: 1,
                room_id: 8201,
                room_name: "Room One".into(),
                position: r1_pos,
                gate_destination_pos: r2_pos,
                gated: false,
                disallow_mwd: None,
                gate_ball_id: None,
                gate_locked: false,
                objects: vec![
                    DungeonRuntimeObject {
                        ball_id: 555_001,
                        type_id: 17_831,
                        name: "PresetMonument".into(),
                        position: [r1_pos[0] + 400.0, 0.0, 0.0],
                        radius: 150.0,
                        is_container: false,
                        loot_items: Vec::new(),
                        spawned: false,
                        ..Default::default()
                    },
                    DungeonRuntimeObject {
                        ball_id: 0,
                        type_id: DEFAULT_CONTAINER_TYPE_ID,
                        name: "AutoIdCan".into(),
                        position: [r1_pos[0] + 800.0, 0.0, 0.0],
                        radius: 100.0,
                        is_container: true,
                        loot_items: vec![(34, 100), (35, 50)],
                        spawned: false,
                        ..Default::default()
                    },
                ],
                npc_groups: vec![DungeonNpcGroup {
                    group_id: 1,
                    wave: 1,
                    npc_specs: vec![(48_090, 2)],
                    npc_ball_ids: Vec::new(),
                    spawn_immediately: true,
                    spawned: false,
                    cleared: false,
                    ..Default::default()
                }],
                triggers: Vec::new(),
                spawned: false,
                cleared: false,
                ..Default::default()
            },
            DungeonRuntimeRoom {
                room_index: 2,
                room_id: 8202,
                room_name: "Room Two".into(),
                position: r2_pos,
                gate_destination_pos: r2_pos,
                gated: true,
                disallow_mwd: None,
                gate_ball_id: None,
                gate_locked: false,
                objects: Vec::new(),
                npc_groups: Vec::new(),
                triggers: Vec::new(),
                spawned: false,
                cleared: false,
                ..Default::default()
            },
        ],
        completed: false,
        despawned: false,
        active_objective_id: 0,
        last_completed_task_id: None,
        completed_task_ids: Vec::new(),
        created_unix: 1_780_100_000,
        ..Default::default()
    };
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(970_020, inst);
    });

    let res = materialize_dungeon_room_entities(&ctx, sys_id, 970_020, 0)
        .await
        .unwrap();

    // Exact strictly increasing allocation order:
    // +1 entry gate, preset object keeps 555_001, +2 container, +3..+4 loot items, +5 internal gate, +6..+7 NPCs
    let entry_gate_id = with_dungeon_runtime_store(&ctx, |store| store.instances[&970_020].entry_gate_ball_id);
    assert_eq!(entry_gate_id, Some(base_id + 1));
    assert_eq!(res.object_ids, vec![555_001, base_id + 2]);
    assert_eq!(res.next_gate_id, Some(base_id + 5));
    assert_eq!(res.npc_ids, vec![base_id + 6, base_id + 7]);

    ctx.db.with_snapshot(|snap| {
        let mut loot_ids: Vec<i64> = snap
            .items
            .values()
            .filter(|it| it.location_id == base_id + 2)
            .map(|it| it.item_id)
            .collect();
        loot_ids.sort_unstable();
        assert_eq!(loot_ids, vec![base_id + 3, base_id + 4]);
        assert_eq!(snap.next_entity_id, base_id + 7);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a2_repeatable_waves_same_tx_spawn_despawn_and_r5_actions() {
    use crate::services::combat_crimewatch::with_combat_runtime;
    use eve_destiny::{Ball, MovementMode};

    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_030_501;
    let ctx = make_runtime_ctx(sys_id, char_id);
    let r1_pos = [30_000_000.0, 0.0, 0.0];
    let r2_pos = [31_000_000.0, 0.0, 0.0];
    let ship_id = 1_030_000_001i64;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.spawn_ball(Ball {
        id: ship_id, type_id: 587, position: DVec3::from_array(r1_pos), velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();

    // Part A: Repeatable wave respawn on last NPC kill (`OWN-R5-02`), LockGate, SpawnObject + GrantLoot (`R5 P2-4`),
    // same-tx `[SpawnNpcGroup, DespawnNpcGroup]`, and multi-room auto-complete guard.
    let inst_id = 980_501i64;
    let inst = DungeonRuntimeInstance {
        instance_id: inst_id, dungeon_id: 1, dungeon_name: "Wave Crucible".into(), category: "Deadspace".into(),
        archetype: "Deadspace".into(), solar_system_id: sys_id, owner_char_id: char_id, base_position: r1_pos,
        rooms: vec![
            DungeonRuntimeRoom {
                room_index: 1, room_id: 9501, room_name: "Wave Room".into(), position: r1_pos, gate_destination_pos: r2_pos,
                gated: true, gate_ball_id: Some(78_500_001), gate_locked: true,
                objects: vec![DungeonRuntimeObject {
                    ball_id: 0, type_id: DEFAULT_CONTAINER_TYPE_ID, name: "Triggered Cache".into(),
                    position: [r1_pos[0] + 1_200.0, 0.0, 0.0], radius: 150.0, is_container: true,
                    spawn_on_trigger: true, ..Default::default()
                }],
                npc_groups: vec![
                    DungeonNpcGroup { group_id: 1, wave: 1, npc_specs: vec![(48_090, 1)], spawn_immediately: true, repeatable: true, ..Default::default() },
                    DungeonNpcGroup { group_id: 2, wave: 2, npc_specs: vec![(48_090, 1)], spawn_immediately: false, ..Default::default() },
                ],
                triggers: vec![DungeonTrigger {
                    trigger_id: 1, kind: DungeonTriggerKind::OnKill { target_npc_group: Some(1), target_ball_id: None },
                    action: DungeonTriggerAction::SpawnNpcGroup(1), repeatable: true, ..Default::default()
                }],
                ..Default::default()
            },
            DungeonRuntimeRoom {
                room_index: 2, room_id: 9502, room_name: "Boss Chamber".into(), position: r2_pos, gate_destination_pos: r2_pos,
                gated: true, npc_groups: vec![DungeonNpcGroup { group_id: 1, wave: 1, npc_specs: vec![(48_090, 1)], spawn_immediately: true, ..Default::default() }],
                ..Default::default()
            },
        ],
        created_unix: 1_780_000_000, ..Default::default()
    };
    with_dungeon_runtime_store(&ctx, |store| { store.instances.insert(inst_id, inst); });

    let mat0 = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0).await.unwrap();
    assert!(mat0.object_ids.is_empty());
    on_dungeon_room_entered(&ctx, sys_id, inst_id, 0, char_id).await;

    // R5 P2-4: GrantLoot targeting unspawned object index 0 adds nothing until SpawnObject runs.
    assert_eq!(apply_dungeon_trigger_actions(&ctx, sys_id, inst_id, 0, vec![(20, DungeonTriggerAction::GrantLoot { items: vec![(34, 50)], target_object_index: Some(0) })], char_id).await, 1);
    ctx.db.with_snapshot(|s| assert!(!s.items.values().any(|i| i.type_id == 34 && i.quantity == 50)));
    assert_eq!(apply_dungeon_trigger_actions(&ctx, sys_id, inst_id, 0, vec![
        (21, DungeonTriggerAction::SpawnObject { object_index: 0 }),
        (22, DungeonTriggerAction::GrantLoot { items: vec![(34, 50)], target_object_index: Some(0) }),
    ], char_id).await, 2);
    let cache_bid = read_dungeon_runtime_store(&ctx, |s| s.instances[&inst_id].rooms[0].objects[0].ball_id);
    assert!(sys_handle.get_ball_snapshot(cache_bid).await.unwrap().is_some());
    ctx.db.with_snapshot(|s| assert!(s.items.values().any(|i| i.location_id == cache_bid && i.type_id == 34 && i.quantity == 50)));

    // Kill Wave 1 -> respawns Wave 2 (`room.cleared == false`, `gate_locked == true`, live ball + ExtendedNpcEntity).
    on_dungeon_npc_killed(&ctx, sys_id, mat0.npc_ids[0], char_id).await;
    let wave2_npc = read_dungeon_runtime_store(&ctx, |s| {
        let rt = &s.instances[&inst_id];
        assert!(!rt.rooms[0].cleared && rt.rooms[0].gate_locked && !rt.completed);
        assert_eq!(rt.rooms[0].npc_groups[0].spawn_count, 2);
        rt.rooms[0].npc_groups[0].npc_ball_ids[0]
    });
    assert!(sys_handle.get_ball_snapshot(wave2_npc).await.unwrap().is_some());
    assert!(with_combat_runtime(&ctx, |rt| rt.npc_entities.contains_key(&wave2_npc)));

    on_dungeon_npc_killed(&ctx, sys_id, wave2_npc, char_id).await;
    let wave3_npc = read_dungeon_runtime_store(&ctx, |s| {
        assert_eq!(s.instances[&inst_id].rooms[0].npc_groups[0].spawn_count, 3);
        s.instances[&inst_id].rooms[0].npc_groups[0].npc_ball_ids[0]
    });

    // Same-tx SpawnNpcGroup(2) + DespawnNpcGroup(2) leaves 0 orphaned balls & 0 ExtendedNpcEntity rows.
    let next_before = ctx.db.with_snapshot(|s| s.next_entity_id);
    assert_eq!(apply_dungeon_trigger_actions(&ctx, sys_id, inst_id, 0, vec![
        (30, DungeonTriggerAction::SpawnNpcGroup(2)),
        (31, DungeonTriggerAction::DespawnNpcGroup(2)),
    ], char_id).await, 2);
    let grp2_dead = read_dungeon_runtime_store(&ctx, |s| {
        let g = &s.instances[&inst_id].rooms[0].npc_groups[1];
        assert!(g.cleared && g.npc_ball_ids.is_empty() && g.dead_npc_ball_ids[0] > next_before);
        g.dead_npc_ball_ids.clone()
    });
    for bid in &grp2_dead {
        assert!(sys_handle.get_ball_snapshot(*bid).await.unwrap().is_none());
        assert!(!with_combat_runtime(&ctx, |rt| rt.npc_entities.contains_key(bid)));
    }

    // OWN-R5-02: clearing Room 2 without Room 1 must NOT auto-complete instance.
    let mat1 = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 1).await.unwrap();
    on_dungeon_room_entered(&ctx, sys_id, inst_id, 1, char_id).await;
    on_dungeon_npc_killed(&ctx, sys_id, mat1.npc_ids[0], char_id).await;
    read_dungeon_runtime_store(&ctx, |s| {
        assert!(s.instances[&inst_id].rooms[1].cleared && !s.instances[&inst_id].rooms[0].cleared && !s.instances[&inst_id].completed);
    });

    assert_eq!(apply_dungeon_trigger_actions(&ctx, sys_id, inst_id, 0, vec![
        (40, DungeonTriggerAction::LockGate),
        (41, DungeonTriggerAction::CompleteDungeon),
    ], char_id).await, 2);
    assert!(sys_handle.get_ball_snapshot(wave3_npc).await.unwrap().is_none());
    assert!(!with_combat_runtime(&ctx, |rt| rt.npc_entities.contains_key(&wave3_npc)));

    // Part B: Same-tx SpawnNpcGroup + FailDungeon leaves 0 orphaned balls/ExtendedNpcEntity and emits OnDungeonExited.
    let fail_id = 980_502i64;
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(fail_id, DungeonRuntimeInstance {
            instance_id: fail_id, dungeon_id: 1, dungeon_name: "Fail Test".into(), category: "Deadspace".into(),
            archetype: "Deadspace".into(), solar_system_id: sys_id, owner_char_id: char_id, base_position: r1_pos,
            rooms: vec![DungeonRuntimeRoom {
                room_index: 1, room_id: 9511, room_name: "Fail R1".into(), position: r1_pos, gate_destination_pos: r1_pos,
                npc_groups: vec![DungeonNpcGroup { group_id: 2, wave: 1, npc_specs: vec![(48_090, 1)], spawn_immediately: false, ..Default::default() }],
                ..Default::default()
            }],
            created_unix: 1_780_000_000, ..Default::default()
        });
    });
    let _ = materialize_dungeon_room_entities(&ctx, sys_id, fail_id, 0).await.unwrap();
    on_dungeon_room_entered(&ctx, sys_id, fail_id, 0, char_id).await;
    let _ = ctx.drain_notifications_for_char(char_id);
    let before_fail_id = ctx.db.with_snapshot(|s| s.next_entity_id);
    assert_eq!(apply_dungeon_trigger_actions(&ctx, sys_id, fail_id, 0, vec![
        (1, DungeonTriggerAction::SpawnNpcGroup(2)),
        (2, DungeonTriggerAction::FailDungeon { reason: "TimedOut".into() }),
    ], char_id).await, 2);
    let after_fail_id = ctx.db.with_snapshot(|s| s.next_entity_id);
    for bid in (before_fail_id + 1)..=after_fail_id {
        assert!(sys_handle.get_ball_snapshot(bid).await.unwrap().is_none());
        assert!(!with_combat_runtime(&ctx, |rt| rt.npc_entities.contains_key(&bid)));
    }
    read_dungeon_runtime_store(&ctx, |s| assert!(s.instances[&fail_id].failed && s.instances[&fail_id].despawned && !s.instances[&fail_id].completed));
    assert!(ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnDungeonExited"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a2_public_dungeon_delayed_reward_escalation_and_notifications() {
    use crate::services::pve_dungeon_runtime_rpc::on_dungeon_approach;
    use eve_db::types::IskCents;
    use eve_destiny::{Ball, MovementMode};

    let sys_id: SolarSystemId = 30_000_142;
    let pilot_a: i64 = 90_030_601;
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("pub_pilot", "pw", pilot_a, "PubPilot");
    let mut sde_mut = (*shared_sde()).clone();
    let esc_did = 990_601u32;
    sde_mut.dungeons.insert(esc_did, eve_sde::DungeonTemplate {
        dungeon_id: esc_did, name: "Default SDE Escalation".into(), faction_id: Some(500_010), archetype_id: Some(20), difficulty: Some(2),
    });
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde_mut), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_790_000_000)));
    let ship_id = 1_030_000_601i64;
    let r1_pos = [60_000_000.0, 0.0, 0.0];
    let bal_before = ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&pilot_a).unwrap();
        ch.solar_system_id = sys_id; ch.active_ship_id = ship_id; ch.balance
    });
    let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_a].user_id);
    let _ = ctx.db.acquire_online_lock_with_computer_hash(uid, pilot_a as u64, Some(pilot_a), None, true, 1_790_000_000);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.spawn_ball(Ball {
        id: ship_id, type_id: 587, position: DVec3::from_array(r1_pos), velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();

    let inst_id = 980_601i64;
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(inst_id, DungeonRuntimeInstance {
            instance_id: inst_id, dungeon_id: 1, dungeon_name: "Public Nexus".into(), category: "Deadspace".into(),
            archetype: "Deadspace".into(), solar_system_id: sys_id, owner_char_id: 0, base_position: r1_pos,
            rooms: vec![DungeonRuntimeRoom {
                room_index: 1, room_id: 9601, room_name: "Nexus Hub".into(), position: r1_pos, gate_destination_pos: r1_pos,
                spawned: true, entered: true,
                triggers: vec![
                    DungeonTrigger {
                        trigger_id: 1,
                        kind: DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: r1_pos, radius_m: 3_000.0 },
                        action: DungeonTriggerAction::GrantReward { isk: 250_000, lp: 500, corp_id: 1_000_002, faction_id: 500_002, standing_milli: 250 },
                        extra_actions: vec![
                            DungeonTriggerAction::StartEscalation { target_dungeon_id: Some(esc_did), chance_bps: 10_000, target_title: Some("Custom Storm Citadel".into()) },
                            DungeonTriggerAction::SetObjective { objective_id: 8801, objective_key: "pub_esc".into(), title: "Press the Assault".into(), completed: true },
                            DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 9002 },
                        ],
                        delay_secs: 5, ..Default::default()
                    },
                    DungeonTrigger {
                        trigger_id: 2,
                        kind: DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: [r1_pos[0] + 20_000.0, 0.0, 0.0], radius_m: 3_000.0 },
                        action: DungeonTriggerAction::GrantReward { isk: i64::MAX, lp: 0, corp_id: 0, faction_id: 0, standing_milli: 0 },
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            created_unix: 1_790_000_000, ..Default::default()
        });
    });

    let _ = ctx.drain_notifications_for_char(pilot_a);
    // P2: arming delayed-only trigger (batch empty) must record room.last_actor_char_id cleanly.
    assert_eq!(on_dungeon_approach(&ctx, pilot_a, sys_id, ship_id, None).await, 1);
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(r1_pos[0] + 20_000.0, 0.0, 0.0)).await;
    assert_eq!(on_dungeon_approach(&ctx, pilot_a, sys_id, ship_id, None).await, 1);
    assert_eq!(on_dungeon_approach(&ctx, pilot_a, sys_id, ship_id, None).await, 0);
    assert_eq!(tick_due_delayed_dungeon_triggers(&ctx).await, 0);
    read_dungeon_runtime_store(&ctx, |s| {
        let rt = &s.instances[&inst_id];
        assert_eq!(rt.last_actor_char_id, pilot_a);
        assert_eq!(rt.rooms[0].last_actor_char_id, pilot_a);
        assert_eq!(rt.rooms[0].triggers[0].armed_by_char_id, Some(pilot_a));
    });
    ctx.db.with_snapshot(|s| assert_eq!(s.characters[&pilot_a].balance, bal_before));
    let _ = ctx.drain_notifications_for_char(pilot_a);

    ctx.advance_secs(5);
    assert_eq!(tick_due_delayed_dungeon_triggers(&ctx).await, 1);
    ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&pilot_a];
        assert_eq!(ch.balance, bal_before.checked_add(IskCents::from_isk(250_000)).unwrap());
        assert_eq!(ch.loyalty_points.get(&1_000_002).copied(), Some(500));
        assert!(ch.standings.get(&500_002).copied().unwrap_or(0.0) > 0.0);
        assert!(s.wallet_journal.iter().any(|j| j.character_id == pilot_a && j.amount == IskCents::from_isk(250_000)));
        assert!(!s.standing_transactions[&pilot_a].is_empty());
    });
    with_pve_exploration_reg(&ctx, |reg| {
        let escs = &reg.escalations_by_char[&pilot_a];
        assert_eq!(escs.len(), 1);
        assert_eq!(escs[0].expedition_dungeon_id, esc_did);
        assert_eq!(escs[0].expedition_name, "Custom Storm Citadel");
    });
    read_dungeon_runtime_store(&ctx, |s| {
        let rt = &s.instances[&inst_id];
        assert!(rt.rooms[0].triggers[0].fired && rt.escalation_instance_id.is_some());
        assert_eq!(rt.objectives[0].status, DungeonObjectiveStatus::Completed);
        assert_eq!(rt.active_objective_id, 8801);
        assert_eq!(rt.last_completed_task_id, Some(8801));
        assert_eq!(rt.completed_task_ids, vec![960102, 8801]);
    });
    let notes = ctx.drain_notifications_for_char(pilot_a);
    for ev in ["OnEscalatingPathChange", "OnEscalatingPathMessage", "OnDungeonTriggerMessage", "OnProgressionTaskCompleted"] {
        assert!(notes.iter().any(|n| n.event == ev), "missing {ev}");
    }
    let prog: Vec<(&str, Vec<eve_proto::PyRep>)> = notes
        .iter()
        .filter(|n| matches!(n.event.as_str(), "OnProgressionTaskCompleted" | "OnProgressionAdvanced"))
        .map(|n| (n.event.as_str(), n.args.clone()))
        .collect();
    assert_eq!(
        prog,
        vec![
            ("OnProgressionTaskCompleted", vec![eve_proto::PyRep::Integer(8801)]),
            ("OnProgressionAdvanced", vec![eve_proto::PyRep::Integer(8801), eve_proto::PyRep::Integer(8801)]),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a2_r1_lock_gate_pirate_archetype_despawn_purge_and_zero_based_cross_room() {
    use crate::services::combat_crimewatch::with_combat_runtime;
    use crate::services::pve_dungeon_runtime_rpc::on_dungeon_approach;
    use eve_destiny::{Ball, MovementMode};

    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_030_701;
    let ctx = make_runtime_ctx(sys_id, char_id);
    let r1_pos = [70_000_000.0, 0.0, 0.0];
    let r2_pos = [71_000_000.0, 0.0, 0.0];
    let r3_pos = [72_000_000.0, 0.0, 0.0];
    let ship_id = 1_030_000_701i64;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.spawn_ball(Ball {
        id: ship_id, type_id: 587, position: DVec3::from_array(r1_pos), velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();

    let inst_id = 980_701i64;
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(inst_id, DungeonRuntimeInstance {
            instance_id: inst_id, dungeon_id: 1, dungeon_name: "R1 Audit Crucible".into(), category: "Deadspace".into(),
            archetype: "Deadspace".into(), solar_system_id: sys_id, owner_char_id: char_id, base_position: r1_pos,
            rooms: vec![
                DungeonRuntimeRoom {
                    room_index: 1, room_id: 9701, room_name: "Room One".into(), position: r1_pos, gate_destination_pos: r2_pos,
                    gated: true, gate_ball_id: Some(79_700_001), gate_locked: false,
                    gate_links: vec![DungeonGateLink { gate_ball_id: Some(79_700_001), gate_position: [r1_pos[0] + 5_000.0, 0.0, 0.0], target_room_index: 1, destination_pos: r2_pos, locked: false, ..Default::default() }],
                    npc_groups: vec![DungeonNpcGroup { group_id: 1, wave: 1, npc_specs: vec![(10_019, 1)], spawn_immediately: true, ..Default::default() }],
                    triggers: vec![
                        DungeonTrigger {
                            trigger_id: 1,
                            kind: DungeonTriggerKind::OnKill { target_npc_group: None, target_ball_id: None },
                            action: DungeonTriggerAction::LockGate, ..Default::default()
                        },
                        DungeonTrigger {
                            trigger_id: 2,
                            kind: DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: r1_pos, radius_m: 3_000.0 },
                            action: DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 1 }, repeatable: true, ..Default::default()
                        },
                        DungeonTrigger {
                            trigger_id: 3,
                            kind: DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: [r1_pos[0] + 15_000.0, 0.0, 0.0], radius_m: 3_000.0 },
                            action: DungeonTriggerAction::UnlockGate, repeatable: true, target_room_index: Some(99), ..Default::default()
                        },
                        DungeonTrigger {
                            trigger_id: 4,
                            kind: DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: [r1_pos[0] + 30_000.0, 0.0, 0.0], radius_m: 3_000.0 },
                            action: DungeonTriggerAction::DespawnNpcGroup(1), repeatable: true, target_room_index: Some(1), ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
                DungeonRuntimeRoom {
                    room_index: 2, room_id: 9702, room_name: "Room Two".into(), position: r2_pos, gate_destination_pos: r3_pos,
                    gated: true, gate_ball_id: Some(79_700_002), gate_locked: true,
                    gate_links: vec![DungeonGateLink { gate_ball_id: Some(79_700_002), gate_position: [r2_pos[0] + 5_000.0, 0.0, 0.0], target_room_index: 2, destination_pos: r3_pos, locked: true, ..Default::default() }],
                    npc_groups: vec![DungeonNpcGroup { group_id: 1, wave: 1, npc_specs: vec![(48_090, 1)], spawn_immediately: true, ..Default::default() }],
                    ..Default::default()
                },
                DungeonRuntimeRoom {
                    room_index: 3, room_id: 9703, room_name: "Room Three".into(), position: r3_pos, gate_destination_pos: r3_pos,
                    gated: true, npc_groups: vec![DungeonNpcGroup { group_id: 1, wave: 1, npc_specs: vec![(10_019, 1)], spawn_immediately: true, ..Default::default() }],
                    ..Default::default()
                },
            ],
            created_unix: 1_795_000_000, ..Default::default()
        });
    });

    // BF-03: SDE-derived Pirate archetype ("Pirate", not overwritten by "Dungeon").
    let m0 = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0).await.unwrap();
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&m0.npc_ids[0]].archetype.clone()), "Pirate");
    on_dungeon_room_entered(&ctx, sys_id, inst_id, 0, char_id).await;

    // BF-02: Real OnKill -> LockGate survives unlock_room_gates_on_clear_in_inst and subsequent OnApproach triggers.
    on_dungeon_npc_killed(&ctx, sys_id, m0.npc_ids[0], char_id).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let r0 = &s.instances[&inst_id].rooms[0];
        assert!(r0.cleared && r0.gate_locked && r0.gate_links[0].locked, "LockGate on OnKill must persist");
    });
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    read_dungeon_runtime_store(&ctx, |s| {
        let r0 = &s.instances[&inst_id].rooms[0];
        assert!(r0.gate_locked && r0.gate_links[0].locked, "subsequent OnApproach must not unlock LockGate");
    });

    // BF-05: Out-of-bounds target_room_index=99 is a no-op; 0-based target_room_index=1 clears Room Two & unlocks its gate.
    let m1 = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 1).await.unwrap();
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(r1_pos[0] + 15_000.0, 0.0, 0.0)).await;
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    read_dungeon_runtime_store(&ctx, |s| assert!(s.instances[&inst_id].rooms[0].gate_locked));

    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(r1_pos[0] + 30_000.0, 0.0, 0.0)).await;
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    assert!(!with_combat_runtime(&ctx, |rt| rt.npc_entities.contains_key(&m1.npc_ids[0])));
    read_dungeon_runtime_store(&ctx, |s| {
        let rt = &s.instances[&inst_id];
        assert!(rt.rooms[0].gate_locked, "0-based Some(1) must target rooms[1], never rooms[0]");
        assert!(rt.rooms[1].cleared && !rt.rooms[1].gate_locked && !rt.rooms[1].gate_links[0].locked);
    });

    // BF-04: despawn_completed_dungeon purges ctx.combat_ephemeral.npc_entities cleanly.
    let m2 = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 2).await.unwrap();
    assert!(with_combat_runtime(&ctx, |rt| rt.npc_entities.contains_key(&m2.npc_ids[0])));
    despawn_completed_dungeon(&ctx, sys_id, inst_id, char_id).await;
    assert!(sys_handle.get_ball_snapshot(m2.npc_ids[0]).await.unwrap().is_none());
    assert!(!with_combat_runtime(&ctx, |rt| rt.npc_entities.contains_key(&m2.npc_ids[0])));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a2_r2_set_objective_progression_final_room_lock_short_circuit_and_sde_reward() {
    use crate::services::pve_dungeon_runtime_rpc::on_dungeon_approach;
    use eve_db::types::IskCents;
    use eve_destiny::{Ball, MovementMode};
    use eve_proto::PyRep;

    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_030_801;
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("r2_pilot", "pw", char_id, "R2Pilot");
    let mut sde_mut = (*shared_sde()).clone();
    let (src_did, sde_reward_did, esc_did) = (990_801u32, 990_802u32, 990_803u32);
    sde_mut.factions.insert(500_801, eve_sde::FactionInfo {
        faction_id: 500_801, name: "SDE R2 Syndicate".into(), corporation_id: Some(1_000_801),
        militia_corporation_id: None, solar_system_id: Some(sys_id), size_factor: 1.0, description: String::new(),
    });
    sde_mut.dungeons.insert(src_did, eve_sde::DungeonTemplate {
        dungeon_id: src_did, name: "Hard Source".into(), faction_id: Some(500_801), archetype_id: Some(20), difficulty: Some(4),
    });
    sde_mut.dungeons.insert(sde_reward_did, eve_sde::DungeonTemplate {
        dungeon_id: sde_reward_did, name: "Reward Site".into(), faction_id: Some(500_801), archetype_id: Some(20), difficulty: Some(2),
    });
    sde_mut.dungeons.insert(esc_did, eve_sde::DungeonTemplate {
        dungeon_id: esc_did, name: "Target Citadel".into(), faction_id: Some(500_801), archetype_id: Some(31), difficulty: Some(2),
    });
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde_mut), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_796_000_000)));
    let ship_id = 1_030_000_801i64;
    let r1_pos = [80_000_000.0, 0.0, 0.0];
    let r2_pos = [81_000_000.0, 0.0, 0.0];
    let bal_before = ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = sys_id; ch.active_ship_id = ship_id; ch.balance
    });
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx.db.acquire_online_lock_with_computer_hash(uid, char_id as u64, Some(char_id), None, true, 1_796_000_000);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.spawn_ball(Ball {
        id: ship_id, type_id: 587, position: DVec3::from_array(r1_pos), velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();

    // Part A: OWN-P1-01 (primary completed=false, primary completed=true, extra_actions SetObjective) + P2-4/P2-5 + P2-1 final-room LockGate.
    let inst_a = 980_801i64;
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(inst_a, DungeonRuntimeInstance {
            instance_id: inst_a, dungeon_id: src_did, dungeon_name: "R2 Obj".into(), category: "Deadspace".into(),
            archetype: "Deadspace".into(), solar_system_id: sys_id, owner_char_id: char_id, base_position: r1_pos,
            rooms: vec![DungeonRuntimeRoom {
                room_index: 1, room_id: 9801, room_name: "Final Lock Chamber".into(), position: r1_pos, gate_destination_pos: r1_pos,
                gated: false, gate_locked: false,
                npc_groups: vec![DungeonNpcGroup { group_id: 1, wave: 1, npc_specs: vec![(48_090, 1)], spawn_immediately: true, ..Default::default() }],
                triggers: vec![
                    DungeonTrigger {
                        trigger_id: 1, kind: DungeonTriggerKind::OnKill { target_npc_group: None, target_ball_id: None },
                        action: DungeonTriggerAction::LockGate, ..Default::default()
                    },
                    DungeonTrigger {
                        trigger_id: 2, kind: DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: r1_pos, radius_m: 3_000.0 },
                        action: DungeonTriggerAction::SetObjective { objective_id: 9101, objective_key: "step1".into(), title: "Active Primary".into(), completed: false },
                        repeatable: true, ..Default::default()
                    },
                    DungeonTrigger {
                        trigger_id: 3, kind: DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: [r1_pos[0] + 10_000.0, 0.0, 0.0], radius_m: 3_000.0 },
                        action: DungeonTriggerAction::SetObjective { objective_id: 9102, objective_key: "step2".into(), title: "Done Primary".into(), completed: true },
                        ..Default::default()
                    },
                    DungeonTrigger {
                        trigger_id: 4, kind: DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: [r1_pos[0] + 20_000.0, 0.0, 0.0], radius_m: 3_000.0 },
                        action: DungeonTriggerAction::GrantReward { isk: 0, lp: 200, corp_id: 0, faction_id: 0, standing_milli: 150 },
                        extra_actions: vec![DungeonTriggerAction::SetObjective { objective_id: 9103, objective_key: "step3".into(), title: "Extra Active".into(), completed: false }],
                        ..Default::default()
                    },
                    DungeonTrigger {
                        trigger_id: 5, kind: DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: [r1_pos[0] + 30_000.0, 0.0, 0.0], radius_m: 3_000.0 },
                        action: DungeonTriggerAction::StartEscalation { target_dungeon_id: Some(esc_did), chance_bps: 10_000, target_title: None },
                        extra_actions: vec![DungeonTriggerAction::SetObjective { objective_id: 9104, objective_key: "step4".into(), title: "Extra Done".into(), completed: true }],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            created_unix: 1_796_000_000, ..Default::default()
        });
    });

    let m0 = materialize_dungeon_room_entities(&ctx, sys_id, inst_a, 0).await.unwrap();
    on_dungeon_room_entered(&ctx, sys_id, inst_a, 0, char_id).await;
    let _ = ctx.drain_notifications_for_char(char_id);

    let drain_prog = || -> Vec<(String, Vec<PyRep>)> {
        ctx.drain_notifications_for_char(char_id)
            .into_iter()
            .filter(|n| matches!(n.event.as_str(), "OnProgressionTaskCompleted" | "OnProgressionAdvanced"))
            .map(|n| (n.event, n.args))
            .collect()
    };

    // Case 1a: primary SetObjective { completed: false } from fresh state emits ONLY OnProgressionAdvanced(9101, 0).
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    read_dungeon_runtime_store(&ctx, |s| {
        let rt = &s.instances[&inst_a];
        assert_eq!(rt.active_objective_id, 9101);
        assert_eq!(rt.last_completed_task_id, None);
        assert!(rt.completed_task_ids.is_empty());
    });
    assert_eq!(drain_prog(), vec![("OnProgressionAdvanced".into(), vec![PyRep::Integer(9101), PyRep::Integer(0)])]);

    // Case 1b + P2-1: OnKill -> LockGate sets 980101; exit + re-enter repeatable OnApproach preserves final-room LockGate & 980101 across tx (I3 latch).
    on_dungeon_npc_killed(&ctx, sys_id, m0.npc_ids[0], char_id).await;
    let _ = ctx.drain_notifications_for_char(char_id);
    assert!(sys_handle.set_ball_position(ship_id, DVec3::new(r1_pos[0] + 5_000.0, 0.0, 0.0)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);
    assert!(sys_handle.set_ball_position(ship_id, DVec3::from_array(r1_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    read_dungeon_runtime_store(&ctx, |s| {
        let rt = &s.instances[&inst_a];
        assert!(!rt.completed && rt.rooms[0].gate_locked, "P2-1: final room must not auto-complete while gate_locked");
        assert_eq!(rt.active_objective_id, 9101);
        assert_eq!(rt.last_completed_task_id, Some(980101));
        assert_eq!(rt.completed_task_ids, vec![980101]);
    });
    assert_eq!(drain_prog(), vec![("OnProgressionAdvanced".into(), vec![PyRep::Integer(9101), PyRep::Integer(980101)])]);

    // Case 2: primary SetObjective { completed: true } emits single TaskCompleted + Advanced and updates completed_task_ids.
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(r1_pos[0] + 10_000.0, 0.0, 0.0)).await;
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    read_dungeon_runtime_store(&ctx, |s| {
        let rt = &s.instances[&inst_a];
        assert_eq!(rt.active_objective_id, 9102);
        assert_eq!(rt.last_completed_task_id, Some(9102));
        assert_eq!(rt.completed_task_ids, vec![980101, 9102]);
    });
    assert_eq!(drain_prog(), vec![
        ("OnProgressionTaskCompleted".into(), vec![PyRep::Integer(9102)]),
        ("OnProgressionAdvanced".into(), vec![PyRep::Integer(9102), PyRep::Integer(9102)]),
    ]);

    // Case 3a: SetObjective { completed: false } in extra_actions + P2-4 SDE faction LP/standing (zero Caldari fabrication!).
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(r1_pos[0] + 20_000.0, 0.0, 0.0)).await;
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    read_dungeon_runtime_store(&ctx, |s| {
        let rt = &s.instances[&inst_a];
        assert_eq!(rt.active_objective_id, 9103);
        assert_eq!(rt.last_completed_task_id, Some(9102));
        assert_eq!(rt.completed_task_ids, vec![980101, 9102]);
    });
    assert_eq!(drain_prog(), vec![("OnProgressionAdvanced".into(), vec![PyRep::Integer(9103), PyRep::Integer(9102)])]);
    ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&char_id];
        assert_eq!(ch.loyalty_points.get(&1_000_801).copied(), Some(200));
        assert_eq!(ch.loyalty_points.get(&1_000_002).copied(), None);
        assert!(ch.standings.get(&500_801).copied().unwrap_or(0.0) > 0.0);
        assert_eq!(ch.standings.get(&500_001).copied(), None);
    });

    // Case 3b: SetObjective { completed: true } in extra_actions + P2-5 explicit target_dungeon_id chain total_steps/template.
    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(r1_pos[0] + 30_000.0, 0.0, 0.0)).await;
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    read_dungeon_runtime_store(&ctx, |s| {
        let rt = &s.instances[&inst_a];
        assert!(rt.rooms[0].cleared && rt.rooms[0].gate_locked && rt.rooms[0].gate_explicitly_locked && !rt.completed);
        assert_eq!(rt.active_objective_id, 9104);
        assert_eq!(rt.last_completed_task_id, Some(9104));
        assert_eq!(rt.completed_task_ids, vec![980101, 9102, 9104]);
    });
    assert_eq!(drain_prog(), vec![
        ("OnProgressionTaskCompleted".into(), vec![PyRep::Integer(9104)]),
        ("OnProgressionAdvanced".into(), vec![PyRep::Integer(9104), PyRep::Integer(9104)]),
    ]);
    with_pve_exploration_reg(&ctx, |reg| {
        let e = &reg.escalations_by_char[&char_id][0];
        assert_eq!(e.expedition_dungeon_id, esc_did);
        assert_eq!(e.total_steps, 3);
        assert_eq!(e.journal_entry_template_id, esc_did);
    });
    // Subsequent repeatable OnApproach after room.cleared=true still preserves final-room LockGate & !completed (P2-1).
    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(r1_pos)).await;
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    read_dungeon_runtime_store(&ctx, |s| {
        let rt = &s.instances[&inst_a];
        assert!(rt.rooms[0].cleared && rt.rooms[0].gate_locked && !rt.completed);
    });

    // P2-4 unknown dungeon with corp_id=0, faction_id=0 adds zero Caldari LP/standing.
    with_dungeon_runtime_store(&ctx, |s| { s.instances.get_mut(&inst_a).unwrap().dungeon_id = 1; });
    assert_eq!(
        apply_dungeon_trigger_actions(&ctx, sys_id, inst_a, 0, vec![(77, DungeonTriggerAction::GrantReward { isk: 0, lp: 50, corp_id: 0, faction_id: 0, standing_milli: 100 })], char_id).await,
        1
    );
    ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&char_id];
        assert_eq!(ch.loyalty_points.get(&1_000_002), None);
        assert_eq!(ch.standings.get(&500_001), None);
    });

    // Part B: P2-2 cross-room delayed short-circuit on FailDungeon.
    let inst_b = 980_802i64;
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(inst_b, DungeonRuntimeInstance {
            instance_id: inst_b, dungeon_id: sde_reward_did, dungeon_name: "Short Circuit".into(), category: "Deadspace".into(),
            archetype: "Deadspace".into(), solar_system_id: sys_id, owner_char_id: char_id, base_position: r1_pos,
            rooms: vec![
                DungeonRuntimeRoom {
                    room_index: 1, room_id: 9811, room_name: "Fail R1".into(), position: r1_pos, gate_destination_pos: r2_pos,
                    spawned: true, entered: true,
                    triggers: vec![DungeonTrigger {
                        trigger_id: 1, kind: DungeonTriggerKind::OnKill { target_npc_group: None, target_ball_id: None },
                        action: DungeonTriggerAction::FailDungeon { reason: "Aborted".into() },
                        delay_secs: 2, armed_at_unix: Some(1_796_000_000), armed_by_char_id: Some(char_id), ..Default::default()
                    }],
                    ..Default::default()
                },
                DungeonRuntimeRoom {
                    room_index: 2, room_id: 9812, room_name: "PostFail R2".into(), position: r2_pos, gate_destination_pos: r2_pos,
                    spawned: true, entered: true,
                    triggers: vec![DungeonTrigger {
                        trigger_id: 2, kind: DungeonTriggerKind::OnKill { target_npc_group: None, target_ball_id: None },
                        action: DungeonTriggerAction::GrantReward { isk: 500_000, lp: 0, corp_id: 0, faction_id: 0, standing_milli: 0 },
                        delay_secs: 2, armed_at_unix: Some(1_796_000_000), armed_by_char_id: Some(char_id), ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            created_unix: 1_796_000_000, ..Default::default()
        });
    });
    ctx.advance_secs(2);
    assert_eq!(tick_due_delayed_dungeon_triggers(&ctx).await, 1);
    read_dungeon_runtime_store(&ctx, |s| {
        let rt = &s.instances[&inst_b];
        assert!(rt.failed && !rt.completed);
        assert!(rt.rooms[0].triggers[0].fired);
        assert!(!rt.rooms[1].triggers[0].fired && rt.rooms[1].triggers[0].armed_at_unix.is_some());
    });
    ctx.db.with_snapshot(|s| assert_eq!(s.characters[&char_id].balance, bal_before, "post-fail room must not grant ISK"));
    let _ = bal_before == IskCents::ZERO;
}
