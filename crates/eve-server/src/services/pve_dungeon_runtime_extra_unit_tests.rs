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
            },
        ],
        completed: false,
        despawned: false,
        active_objective_id: 0,
        last_completed_task_id: None,
        completed_task_ids: Vec::new(),
        created_unix: 1_780_000_000,
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
            }],
            npc_groups: vec![DungeonNpcGroup {
                group_id: 1,
                wave: 1,
                npc_specs: vec![(48_090, 2)],
                npc_ball_ids: Vec::new(),
                spawn_immediately: true,
                spawned: false,
                cleared: false,
            }],
            triggers: Vec::new(), // no explicit triggers -> tests auto-clear & auto-complete on last NPC killed!
            spawned: false,
            cleared: false,
        }],
        completed: false,
        despawned: false,
        active_objective_id: 0,
        last_completed_task_id: None,
        completed_task_ids: Vec::new(),
        created_unix: 1_780_000_000,
    };

    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(960_010, inst);
    });

    // Out-of-bounds room_vec_idx fails with DungeonRoomNotFound
    assert!(materialize_dungeon_room_entities(&ctx, sys_id, 960_010, 5).await.is_err());

    // Materialize room 0 -> spawns entry gate, container (owner 500_010, type 23), and 2 NPCs
    let (obj_ids, npc_ids, gate_opt) =
        materialize_dungeon_room_entities(&ctx, sys_id, 960_010, 0).await.unwrap();
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
        });
    });

    on_dungeon_npc_killed(&ctx, sys_id, npc_ids[0], char_id).await;
    with_dungeon_runtime_store(&ctx, |store| {
        let inst = &store.instances[&960_010];
        assert!(!inst.rooms[0].gate_locked, "killing target_ball_id should unlock gate");
        assert!(!inst.completed, "1 NPC still alive");
    });

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
