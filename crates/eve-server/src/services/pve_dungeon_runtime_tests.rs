//! Unit and integration tests for `pve_dungeon_runtime`.

use super::*;
use crate::router::ServerContext;
use crate::services::pve_dungeon_editor::{with_dungeon_editor_store, GmActiveEditorState};
use crate::services::pve_exploration::{with_pve_exploration_reg, PveSiteInstance};
use eve_db::clock::VirtualServerClock;
use eve_db::{BookmarkRecord, CosmicSignatureRecord, WorldRepository};
use eve_sde::{SdeRegistry, SolarSystemId};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn find_non_archetype51_dungeon(sde: &SdeRegistry) -> u32 {
    let mut cands: Vec<u32> = sde
        .dungeons
        .values()
        .filter(|d| !matches!(d.archetype_id, Some(3 | 4 | 6 | 8 | 24 | 51)))
        .map(|d| d.dungeon_id)
        .collect();
    cands.sort_unstable();
    cands.into_iter().next().unwrap_or(1)
}

fn find_archetype51_dungeon(sde: &SdeRegistry) -> Option<u32> {
    sde.dungeons
        .values()
        .find(|d| d.archetype_id == Some(51))
        .map(|d| d.dungeon_id)
}

fn make_ctx(sys_id: SolarSystemId, char_id: i64) -> ServerContext {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("dungeon_runner", "pw", char_id, "DungeonRunner");
    let uid = db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.user_id).unwrap_or(1));
    let _ = db.acquire_online_lock_with_computer_hash(
        uid,
        char_id as u64,
        Some(char_id),
        None,
        true,
        1_775_100_000,
    );
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = 1_020_000_001;
        }
    });
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_100_000)));
    ctx
}

fn sample_instance(inst_id: i64, did: u32, sys_id: SolarSystemId, char_id: i64, disallow_mwd: bool) -> DungeonRuntimeInstance {
    let r1_pos = [1_000_000.0, 0.0, 0.0];
    let r2_pos = [2_000_000.0, 0.0, 0.0];
    let r3_pos = [3_000_000.0, 0.0, 0.0];
    DungeonRuntimeInstance {
        instance_id: inst_id,
        dungeon_id: did,
        dungeon_name: "Test Complex".into(),
        category: "Expedition".into(),
        archetype: "Deadspace".into(),
        solar_system_id: sys_id,
        owner_char_id: char_id,
        base_position: r1_pos,
        disallow_mwd,
        entry_gate_ball_id: None,
        entry_gate_pos: None,
        entry_gate_locked: false,
        current_room_index: 1,
        rooms: vec![
            DungeonRuntimeRoom {
                room_index: 1,
                room_id: 5001,
                room_name: "Outer Pocket".into(),
                position: r1_pos,
                gate_destination_pos: r2_pos,
                gated: false,
                disallow_mwd: None,
                gate_ball_id: None,
                gate_locked: true,
                objects: vec![DungeonRuntimeObject {
                    ball_id: 0,
                    type_id: DEFAULT_CONTAINER_TYPE_ID,
                    name: "Overseer Cache".into(),
                    position: [r1_pos[0] + 1_200.0, 0.0, 0.0],
                    radius: 250.0,
                    is_container: true,
                    loot_items: vec![(34, 500)],
                    spawned: false,
                }],
                npc_groups: vec![
                    DungeonNpcGroup {
                        group_id: 1,
                        wave: 1,
                        npc_specs: vec![(48_090, 1)],
                        npc_ball_ids: Vec::new(),
                        spawn_immediately: true,
                        spawned: false,
                        cleared: false,
                    },
                    DungeonNpcGroup {
                        group_id: 2,
                        wave: 2,
                        npc_specs: vec![(48_090, 1)],
                        npc_ball_ids: Vec::new(),
                        spawn_immediately: false,
                        spawned: false,
                        cleared: false,
                    },
                ],
                triggers: vec![
                    DungeonTrigger {
                        trigger_id: 1,
                        kind: DungeonTriggerKind::OnKill { target_npc_group: Some(1), target_ball_id: None },
                        action: DungeonTriggerAction::SpawnNpcGroup(2),
                        fired: false,
                    },
                    DungeonTrigger {
                        trigger_id: 2,
                        kind: DungeonTriggerKind::OnKill { target_npc_group: Some(2), target_ball_id: None },
                        action: DungeonTriggerAction::UnlockGate,
                        fired: false,
                    },
                ],
                spawned: false,
                cleared: false,
            },
            DungeonRuntimeRoom {
                room_index: 2,
                room_id: 5002,
                room_name: "Inner Sanctuary".into(),
                position: r2_pos,
                gate_destination_pos: r3_pos,
                gated: true,
                disallow_mwd: Some(true),
                gate_ball_id: None,
                gate_locked: false,
                objects: Vec::new(),
                npc_groups: vec![DungeonNpcGroup {
                    group_id: 1,
                    wave: 1,
                    npc_specs: vec![(48_090, 1)],
                    npc_ball_ids: Vec::new(),
                    spawn_immediately: true,
                    spawned: false,
                    cleared: false,
                }],
                triggers: vec![
                    DungeonTrigger {
                        trigger_id: 10,
                        kind: DungeonTriggerKind::OnKill { target_npc_group: None, target_ball_id: None },
                        action: DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 42 },
                        fired: false,
                    },
                    DungeonTrigger {
                        trigger_id: 11,
                        kind: DungeonTriggerKind::OnKill { target_npc_group: None, target_ball_id: None },
                        action: DungeonTriggerAction::CompleteDungeon,
                        fired: false,
                    },
                ],
                spawned: false,
                cleared: false,
            },
            DungeonRuntimeRoom {
                room_index: 3,
                room_id: 5003,
                room_name: "Ungated Observation Pocket".into(),
                position: r3_pos,
                gate_destination_pos: r3_pos,
                gated: false,
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
        created_unix: 1_775_100_000,
    }
}

#[test]
fn deadspace_mwd_and_warp_restrictions_and_targets() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_020_001;
    let ctx = make_ctx(sys_id, char_id);
    let safe_did = find_non_archetype51_dungeon(&ctx.sde);

    assert!(!is_ship_in_deadspace_pocket(&ctx, sys_id, [1_000_000.0, 0.0, 0.0]));
    check_deadspace_mwd_allowed(&ctx, sys_id, [1_000_000.0, 0.0, 0.0]).expect("open space mwd ok");

    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(950_001, sample_instance(950_001, safe_did, sys_id, char_id, false));
    });

    assert!(is_ship_in_deadspace_pocket(&ctx, sys_id, [1_010_000.0, 0.0, 0.0]));
    check_deadspace_mwd_allowed(&ctx, sys_id, [1_010_000.0, 0.0, 0.0]).expect("room 1 mwd ok");
    let r2_mwd_err = check_deadspace_mwd_allowed(&ctx, sys_id, [2_005_000.0, 0.0, 0.0])
        .expect_err("room 2 mwd blocked");
    assert_eq!(r2_mwd_err.code, "ModuleNotAllowedInDeadspace");

    check_deadspace_warp_allowed(&ctx, sys_id, [0.0; 3], [1_000_000.0, 0.0, 0.0], [1_000_000.0, 0.0, 0.0])
        .expect("warp to ungated room 1 ok");
    let gated_warp = check_deadspace_warp_allowed(&ctx, sys_id, [0.0; 3], [2_000_000.0, 0.0, 0.0], [2_000_000.0, 0.0, 0.0])
        .expect_err("cannot warp directly into gated room 2");
    assert_eq!(gated_warp.code, "NaturalPhenomenaDisruptingWarp");
    let cross_pocket = check_deadspace_warp_allowed(&ctx, sys_id, [1_005_000.0, 0.0, 0.0], [3_000_000.0, 0.0, 0.0], [3_000_000.0, 0.0, 0.0])
        .expect_err("cannot warp across pockets in same complex");
    assert_eq!(cross_pocket.code, "NaturalPhenomenaDisruptingWarp");

    let p_inst = resolve_deadspace_or_escalation_warp_target(&ctx, char_id, sys_id, "dungeon", 950_001, None)
        .expect("resolve instance")
        .expect("room 1 pos");
    assert!((p_inst.x - 1_000_000.0).abs() < 1.0);

    let p_str = resolve_deadspace_or_escalation_warp_target(&ctx, char_id, sys_id, "location", 0, Some("DNG-INST-950001"))
        .expect("resolve DNG-INST string")
        .expect("room 1 pos");
    assert!((p_str.x - 1_000_000.0).abs() < 1.0);

    let gated_room_res = resolve_deadspace_or_escalation_warp_target(&ctx, char_id, sys_id, "dungeon", 5002, None)
        .expect_err("gated room target denied");
    assert!(!gated_room_res.code.is_empty());

    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            77_001,
            BookmarkRecord {
                bookmark_id: 77_001,
                owner_id: char_id,
                label: "Esc".into(),
                solar_system_id: sys_id,
                x: 4_500_000.0,
                y: 100.0,
                z: -200.0,
                item_id: Some(88_001),
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    let bm_pos = resolve_deadspace_or_escalation_warp_target(&ctx, char_id, sys_id, "epinstance", 88_001, None)
        .expect("bookmark resolve")
        .expect("bookmark coords");
    assert_eq!(bm_pos, DVec3::new(4_500_000.0, 100.0, -200.0));

    with_dungeon_editor_store(&ctx, |ed| {
        ed.dungeon_disallow_mwd.insert(safe_did as i64, true);
        ed.active_editors.insert(
            char_id,
            GmActiveEditorState {
                solar_system_id: Some(sys_id),
                dungeon_id: Some(safe_did as i64),
                room_id: None,
                ..Default::default()
            },
        );
    });
    let ed_mwd = check_deadspace_mwd_allowed(&ctx, sys_id, [10_000.0, 0.0, 0.0])
        .expect_err("editor disallow_mwd");
    assert_eq!(ed_mwd.code, "ModuleNotAllowedInDeadspace");

    if let Some(a51_did) = find_archetype51_dungeon(&ctx.sde) {
        with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
                site_id: "EXP-A51".into(),
                solar_system_id: sys_id,
                site_category: "Deadspace".into(),
                dungeon_id: a51_did,
                dungeon_name: "DED 10/10".into(),
                position: [9_000_000.0, 0.0, 0.0],
                current_room: 2,
                total_rooms: 3,
                ..Default::default()
            });
        });
        assert_eq!(
            check_deadspace_mwd_allowed(&ctx, sys_id, [9_010_000.0, 0.0, 0.0])
                .expect_err("archetype 51 mwd")
                .code,
            "ModuleNotAllowedInDeadspace"
        );
        assert_eq!(
            check_deadspace_warp_allowed(&ctx, sys_id, [0.0; 3], [9_500_000.0, 0.0, 0.0], [9_500_000.0, 0.0, 0.0])
                .expect_err("multi-room exploration site room 2 blocked")
                .code,
            "NaturalPhenomenaDisruptingWarp"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_materialization_wave_triggers_gate_unlock_and_completion_despawn() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_020_002;
    let ctx = make_ctx(sys_id, char_id);
    let safe_did = find_non_archetype51_dungeon(&ctx.sde);

    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(950_010, sample_instance(950_010, safe_did, sys_id, char_id, false));
    });
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
            site_id: "DNG-SIG-1".into(),
            solar_system_id: sys_id,
            site_category: "Deadspace".into(),
            dungeon_id: safe_did,
            dungeon_name: "Test Complex".into(),
            position: [1_000_000.0, 0.0, 0.0],
            current_room: 1,
            total_rooms: 3,
            ..Default::default()
        });
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.entry(sys_id).or_default().push(CosmicSignatureRecord {
            signature_id: "DNG-SIG-1".into(),
            solar_system_id: sys_id,
            sig_group: "Combat Site".into(),
            type_name: "Test Complex".into(),
            position: [1_000_000.0, 0.0, 0.0],
            signal_strength: 100.0,
            wormhole_type_id: None,
            destination_system_id: None,
            created_unix: 1_775_100_000,
            lifetime_seconds: 86_400,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
        });
    });

    on_warp_landed_at_dungeon_pocket_if_present(&ctx, char_id, sys_id, [1_000_000.0, 0.0, 0.0]).await;
    assert_eq!(find_active_dungeon_id_at_position(&ctx, sys_id, [1_000_000.0, 0.0, 0.0]), Some(safe_did));

    let (w1_npc_id, cont_id) = with_dungeon_runtime_store(&ctx, |store| {
        let inst = store.instances.get(&950_010).unwrap();
        let r1 = &inst.rooms[0];
        assert!(r1.spawned);
        assert!(r1.gate_ball_id.is_some());
        (r1.npc_groups[0].npc_ball_ids[0], r1.objects[0].ball_id)
    });
    ctx.db.with_snapshot(|snap| {
        let loot_inside = snap.items.values().any(|i| i.location_id == cont_id && i.type_id == 34 && i.quantity == 500);
        assert!(loot_inside, "container should contain seeded loot item");
    });

    on_dungeon_npc_killed(&ctx, sys_id, w1_npc_id, char_id).await;
    let w2_npc_id = with_dungeon_runtime_store(&ctx, |store| {
        let r1 = &store.instances.get(&950_010).unwrap().rooms[0];
        assert!(r1.npc_groups[1].spawned);
        assert_eq!(r1.npc_groups[1].npc_ball_ids.len(), 1);
        assert!(r1.gate_locked);
        r1.npc_groups[1].npc_ball_ids[0]
    });

    on_dungeon_npc_killed(&ctx, sys_id, w2_npc_id, char_id).await;
    with_dungeon_runtime_store(&ctx, |store| {
        let r1 = &store.instances.get(&950_010).unwrap().rooms[0];
        assert!(!r1.gate_locked);
        assert!(r1.cleared);
    });

    on_warp_start_leave_dungeon_if_needed(&ctx, char_id, sys_id, [1_000_000.0, 0.0, 0.0], [50_000_000.0, 0.0, 0.0]);
    let notes = ctx.drain_notifications_for_char(char_id);
    assert!(notes.iter().any(|n| n.event == "OnExitingDungeon"));

    let (_, r2_npcs, _) = materialize_dungeon_room_entities(&ctx, sys_id, 950_010, 1)
        .await
        .expect("materialize room 2");
    assert_eq!(r2_npcs.len(), 1);

    on_dungeon_npc_killed(&ctx, sys_id, r2_npcs[0], char_id).await;
    with_dungeon_runtime_store(&ctx, |store| {
        let inst = store.instances.get(&950_010).unwrap();
        assert!(inst.completed);
        assert!(inst.despawned);
    });
    ctx.db.with_snapshot(|snap| {
        assert!(snap.cosmic_signatures[&sys_id][0].collapsed);
    });
    let done_notes = ctx.drain_notifications_for_char(char_id);
    assert!(done_notes.iter().any(|n| n.event == "OnDungeonCompleted"));
}
