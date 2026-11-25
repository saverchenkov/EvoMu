//! Unit and integration tests for `pve_dungeon_runtime`.

use super::*;
use crate::router::ServerContext;
use crate::services::pve_dungeon_editor::{with_dungeon_editor_store, GmActiveEditorState};
use crate::services::pve_exploration::{with_pve_exploration_reg, PveSiteInstance};
use eve_db::clock::VirtualServerClock;
use eve_db::{BookmarkRecord, CosmicSignatureRecord, ServerClock, WorldRepository};
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
        category: "Deadspace".into(),
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
                    ..Default::default()
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
                        ..Default::default()
                    },
                    DungeonNpcGroup {
                        group_id: 2,
                        wave: 2,
                        npc_specs: vec![(48_090, 1)],
                        npc_ball_ids: Vec::new(),
                        spawn_immediately: false,
                        spawned: false,
                        cleared: false,
                        ..Default::default()
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
                ..Default::default()
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
                    ..Default::default()
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
                ..Default::default()
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
                ..Default::default()
            },
        ],
        completed: false,
        despawned: false,
        active_objective_id: 0,
        last_completed_task_id: None,
        completed_task_ids: Vec::new(),
        created_unix: 1_775_100_000,
        ..Default::default()
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

    let MaterializedDungeonRoomEntities {
        npc_ids: r2_npcs, ..
    } = materialize_dungeon_room_entities(&ctx, sys_id, 950_010, 1)
        .await
        .expect("materialize room 2");
    assert_eq!(r2_npcs.len(), 1);
    on_dungeon_room_entered(&ctx, sys_id, 950_010, 1, char_id).await;
    // Empty the Overseer Cache so completion performs immediate full despawn rather than loot-grace retention.
    ctx.db
        .with_mut_snapshot(|snap| snap.items.retain(|_, it| it.location_id != cont_id));

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

#[test]
fn private_mwd_precedence_boundaries_and_gate_ship_restriction_matrix() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_020_101;
    let mut custom_sde = (*shared_sde()).clone();

    // Inject synthetic DungeonTemplates covering all archetypes & difficulties 1..=5 + None
    for (did, arch, diff) in [
        (880_001u32, Some(3u32), Some(1u32)),  // restricted arch 3, diff 1
        (880_002, Some(4), Some(2)),           // restricted arch 4, diff 2
        (880_003, Some(6), Some(3)),           // restricted arch 6, diff 3
        (880_004, Some(8), Some(4)),           // restricted arch 8, diff 4
        (880_005, Some(24), Some(5)),          // restricted arch 24, diff 5
        (880_006, Some(51), None),             // restricted arch 51, no difficulty
        (880_007, Some(99), Some(1)),          // unrestricted arch 99, diff 1
    ] {
        custom_sde.dungeons.insert(
            did,
            eve_sde::DungeonTemplate {
                dungeon_id: did,
                name: format!("Synth_{did}"),
                archetype_id: arch,
                faction_id: Some(500_010),
                difficulty: diff,
            },
        );
    }

    // Inject synthetic ItemTypes covering always-blocked groups, valid/invalid diff groups, rigs, mass
    let base_item = custom_sde.types.values().next().unwrap().clone();
    let mut add_ship = |tid: u32, grp: u32, mass: f64, rig_sz: f64| {
        let mut t = base_item.clone();
        t.type_id = tid;
        t.category_id = 6;
        t.group_id = grp;
        t.mass = mass;
        t.default_attributes.insert(1547, rig_sz);
        custom_sde.types.insert(tid, t);
    };
    add_ship(910_001, 25, 1_000_000.0, 1.0);   // Frigate ok for diff 1,2,3,4,5
    add_ship(910_002, 420, 15_000_000.0, 1.0); // Exact 15M mass destroyer -> diff 1 ok
    add_ship(910_003, 25, 15_000_001.0, 1.0);  // Frigate > 15M -> diff 1 blocked, diff 2 ok
    add_ship(910_004, 25, 1_000_000.0, 1.5);   // Rig > 1.0 -> diff 1 blocked, diff 2 ok
    add_ship(910_005, 26, 10_000_000.0, 2.0);  // Cruiser rig 2.0 -> diff 1 blocked, diff 2..=3 ok
    add_ship(910_006, 963, 50_000_000.0, 2.0); // Strategic Cruiser exact 50M -> diff 2..=3 ok
    add_ship(910_007, 26, 50_000_001.0, 2.0);  // Cruiser > 50M -> diff 2..=3 blocked, diff 4 ok
    add_ship(910_008, 419, 40_000_000.0, 2.0); // Battlecruiser (grp 419 NOT in diff 2..=3) -> diff 2..=3 blocked, diff 4 ok
    add_ship(910_009, 27, 300_000_000.0, 3.0); // Battleship exact 300M rig 3.0 -> diff 4..=5 ok
    add_ship(910_010, 27, 300_000_001.0, 3.0); // > 300M -> diff 4..=5 blocked, None ok (<= 500M)
    add_ship(910_011, 27, 100_000_000.0, 3.1); // Rig > 3.0 -> diff 4..=5 AND None blocked
    add_ship(910_012, 27, 500_000_001.0, 3.0); // > 500M -> None diff blocked

    for (idx, &blk_grp) in [29u32, 30, 485, 513, 547, 659, 883, 902, 941, 1538].iter().enumerate() {
        add_ship(920_000 + idx as u32, blk_grp, 100.0, 1.0);
    }

    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("dng_priv", "pw", char_id, "DngPriv");
    let ctx = ServerContext::with_sde_and_repository(Arc::new(custom_sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_200_000)));

    // 1. Store presence checks, dist3, and unix_to_filetime_ticks
    assert!(!has_dungeon_runtime_store_db(&ctx.db));
    assert!(!has_dungeon_runtime_store(&ctx));
    assert!((dist3([-3.0, -4.0, 0.0], [0.0, 0.0, 12.0]) - 13.0).abs() < 1e-9);
    assert_eq!(unix_to_filetime_ticks(1_000), (1_000 + 11_644_473_600) * 10_000_000);

    // 2. is_pocket_mwd_disallowed precedence + pocket radius exact boundary (200_000.0 vs 200_000.1)
    let mut base_inst = sample_instance(955_001, 880_007, sys_id, char_id, false);
    let mut room = base_inst.rooms[0].clone();
    room.room_id = 88_001;
    room.position = [10_000_000.0, 0.0, 0.0];

    // All false/unrestricted -> MWD allowed
    room.disallow_mwd = None;
    base_inst.disallow_mwd = false;
    base_inst.dungeon_id = 880_007; // arch 99 (unrestricted)
    assert!(!is_pocket_mwd_disallowed(&ctx, &base_inst, &room));

    // SDE archetype 3, 4, 6, 8, 24, 51 -> all disallow MWD when flags are None/false
    for restricted_did in [880_001u32, 880_002, 880_003, 880_004, 880_005, 880_006] {
        base_inst.dungeon_id = restricted_did;
        assert!(is_pocket_mwd_disallowed(&ctx, &base_inst, &room), "arch for {restricted_did}");
    }

    // inst.disallow_mwd = true beats unrestricted arch 99
    base_inst.dungeon_id = 880_007;
    base_inst.disallow_mwd = true;
    assert!(is_pocket_mwd_disallowed(&ctx, &base_inst, &room));

    // Editor dungeon override beats inst.disallow_mwd = true and restricted arch 51
    base_inst.dungeon_id = 880_006;
    with_dungeon_editor_store(&ctx, |ed| {
        ed.dungeon_disallow_mwd.insert(880_006, false);
    });
    assert!(!is_pocket_mwd_disallowed(&ctx, &base_inst, &room));

    // Editor room override beats editor dungeon override
    with_dungeon_editor_store(&ctx, |ed| {
        ed.rooms_by_dungeon.insert(
            880_006,
            vec![crate::services::pve_dungeon_editor::DungeonEditorRoom {
                room_id: 88_001,
                room_name: "EdOverride".into(),
                dungeon_id: 880_006,
                disallow_mwd: true,
                objects: Vec::new(),
            }],
        );
    });
    assert!(is_pocket_mwd_disallowed(&ctx, &base_inst, &room));

    // Room.disallow_mwd = Some(false) beats editor room override (`true`), inst flag (`true`), & SDE arch 51!
    room.disallow_mwd = Some(false);
    assert!(!is_pocket_mwd_disallowed(&ctx, &base_inst, &room));

    // Pocket radius boundary & completed/despawned/other-system filtering in is_ship_in_deadspace_pocket
    base_inst.rooms = vec![room];
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(955_001, base_inst);
    });
    assert!(has_dungeon_runtime_store_db(&ctx.db));
    assert!(is_ship_in_deadspace_pocket(&ctx, sys_id, [10_200_000.0, 0.0, 0.0]));
    assert!(!is_ship_in_deadspace_pocket(&ctx, sys_id, [10_200_000.1, 0.0, 0.0]));
    assert!(!is_ship_in_deadspace_pocket(&ctx, 30_000_144, [10_000_000.0, 0.0, 0.0]));
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.get_mut(&955_001).unwrap().completed = true;
    });
    assert!(!is_ship_in_deadspace_pocket(&ctx, sys_id, [10_000_000.0, 0.0, 0.0]));

    // 3. check_dungeon_runtime_gate_ship_allowed across all difficulty buckets & blocked groups
    for idx in 0..10 {
        assert_eq!(
            check_dungeon_runtime_gate_ship_allowed(&ctx.sde, 880_006, 920_000 + idx, 100.0)
                .unwrap_err()
                .code,
            "ShipRestrictedByDungeonGate"
        );
    }
    // Diff 1 (`880_001`)
    assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, 880_001, 910_001, 1.0).is_ok());
    assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, 880_001, 910_002, 1.0).is_ok());
    assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, 880_001, 910_003, 1.0).is_err());
    assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, 880_001, 910_004, 1.0).is_err());
    assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, 880_001, 910_005, 1.0).is_err());
    assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, 880_001, 999_999, 15_000_000.0).is_ok());
    assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, 880_001, 999_999, 15_000_001.0).is_err());

    // Diff 2 (`880_002`) & Diff 3 (`880_003`)
    for d in [880_002u32, 880_003] {
        assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, d, 910_005, 1.0).is_ok());
        assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, d, 910_006, 1.0).is_ok());
        assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, d, 910_007, 1.0).is_err());
        assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, d, 910_008, 1.0).is_err());
        assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, d, 910_009, 1.0).is_err());
    }
    // Diff 4 (`880_004`) & Diff 5 (`880_005`)
    for d in [880_004u32, 880_005] {
        assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, d, 910_008, 1.0).is_ok());
        assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, d, 910_009, 1.0).is_ok());
        assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, d, 910_010, 1.0).is_err());
        assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, d, 910_011, 1.0).is_err());
    }
    // None / unknown difficulty (`880_006` and `999_999`)
    for d in [880_006u32, 999_999] {
        assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, d, 910_010, 1.0).is_ok());
        assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, d, 910_011, 1.0).is_err());
        assert!(check_dungeon_runtime_gate_ship_allowed(&ctx.sde, d, 910_012, 1.0).is_err());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_dungeon_npc_killed_guards_unfired_triggers_and_owner_fallback_recipient() {
    let sys_id: SolarSystemId = 30_000_142;
    let owner_cid: i64 = 90_020_201;
    let ctx = make_ctx(sys_id, owner_cid);

    // 1. Empty store returns cleanly
    on_dungeon_npc_killed(&ctx, sys_id, 123_456, owner_cid).await;

    // 2. Single-room instance where an unfired OnApproach trigger remains prevents room clear & completion!
    let mut inst = sample_instance(956_001, 1, sys_id, owner_cid, false);
    inst.rooms.truncate(1);
    inst.rooms[0].npc_groups.truncate(1);
    inst.rooms[0].triggers = vec![DungeonTrigger {
        trigger_id: 91,
        kind: DungeonTriggerKind::OnApproach {
            target_ball_id: None,
            target_object_index: None,
            anchor_pos: inst.rooms[0].position,
            radius_m: 1_000.0,
        },
        action: DungeonTriggerAction::UnlockGate,
        fired: false,
    }];
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(956_001, inst);
    });
    let MaterializedDungeonRoomEntities { npc_ids: npcs, .. } =
        materialize_dungeon_room_entities(&ctx, sys_id, 956_001, 0)
            .await
            .unwrap();
    on_dungeon_room_entered(&ctx, sys_id, 956_001, 0, owner_cid).await;
    on_dungeon_npc_killed(&ctx, sys_id, npcs[0], owner_cid).await;
    with_dungeon_runtime_store(&ctx, |store| {
        let s = &store.instances[&956_001];
        assert!(!s.rooms[0].cleared, "unfired OnApproach must block room clear");
        assert!(!s.completed);
    });

    // 3. Remove the blocking trigger, materialize a fresh room with no triggers & kill with killer_char_id = 0 -> owner_cid gets notifications!
    let mut inst2 = sample_instance(956_002, 1, sys_id, owner_cid, false);
    inst2.rooms.truncate(1);
    inst2.rooms[0].npc_groups.truncate(1);
    inst2.rooms[0].objects.clear();
    inst2.rooms[0].triggers.clear();
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(956_002, inst2);
    });
    let MaterializedDungeonRoomEntities { npc_ids: npcs2, .. } =
        materialize_dungeon_room_entities(&ctx, sys_id, 956_002, 0)
            .await
            .unwrap();
    on_dungeon_room_entered(&ctx, sys_id, 956_002, 0, owner_cid).await;
    let _ = ctx.drain_notifications_for_char(owner_cid);
    on_dungeon_npc_killed(&ctx, sys_id, npcs2[0], 0).await;
    with_dungeon_runtime_store(&ctx, |store| {
        let s = &store.instances[&956_002];
        assert!(s.completed);
        assert!(s.despawned);
        assert_eq!(s.completed_task_ids.len(), 1);
    });
    let owner_notes = ctx.drain_notifications_for_char(owner_cid);
    assert!(owner_notes.iter().any(|n| n.event == "OnDungeonCompleted"));
    assert!(owner_notes.iter().any(|n| n.event == "OnProgressionTaskCompleted"));
}

#[test]
fn resolve_deadspace_or_escalation_enforces_probe_scan_expedition_owner_and_drifter_hive_id() {
    let sys_id: SolarSystemId = 30_000_142;
    let owner_char: i64 = 90_020_301;
    let other_char: i64 = 90_020_302;
    let ctx = make_ctx(sys_id, owner_char);
    let ded_pos = [14_000_000.0, 250.0, -500.0];
    let exp_pos = [18_000_000.0, -100.0, 750.0];
    let hive_beacon_id = 1_898_701_110_i64;

    // 1. Seed a probe_required DED combat signature, a private escalation site, an owned
    //    DungeonRuntimeInstance, and an active Drifter Hive in `sys_id`.
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(
            957_001,
            sample_instance(957_001, 4_203, sys_id, owner_char, false),
        );
    });
    with_pve_exploration_reg(&ctx, |reg| {
        reg.escalations_by_char
            .entry(owner_char)
            .or_default()
            .push(crate::services::pve_exploration::EscalatingPathEntry {
                instance_id: 77_801,
                character_id: owner_char,
                source_dungeon_id: 4_200,
                expedition_dungeon_id: 4_202,
                expedition_name: "Private Escalation".into(),
                destination_solar_system_id: sys_id,
                step_index: 1,
                total_steps: 4,
                journal_text: String::new(),
                created_at_sec: 1_775_100_000,
                expires_at_sec: 1_775_186_400,
                dungeons_entered: Vec::new(),
                journal_entry_template_id: 0,
            });
        reg.sites_by_system.entry(sys_id).or_default().extend([
            PveSiteInstance {
                site_id: "DED-SIG-1".into(),
                solar_system_id: sys_id,
                site_category: "CombatSignature".into(),
                dungeon_id: 4_201,
                dungeon_name: "Unscanned DED 4/10".into(),
                position: ded_pos,
                probe_required: true,
                signal_strength: 0.25,
                cleared: false,
                ..Default::default()
            },
            PveSiteInstance {
                site_id: "EXP-77801-1".into(),
                solar_system_id: sys_id,
                site_category: "Expedition".into(),
                dungeon_id: 4_202,
                dungeon_name: "Private Escalation".into(),
                position: exp_pos,
                probe_required: false,
                signal_strength: 1.0,
                expedition_instance_id: Some(77_801),
                cleared: false,
                ..Default::default()
            },
        ]);
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures
            .entry(sys_id)
            .or_default()
            .push(CosmicSignatureRecord {
                signature_id: "DED-SIG-1".into(),
                solar_system_id: sys_id,
                sig_group: "Combat Site".into(),
                type_name: "Unscanned DED 4/10".into(),
                position: ded_pos,
                signal_strength: 0.25,
                collapsed: false,
                ..Default::default()
            });
    });
    crate::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.drifter_hives.insert(
            sys_id,
            crate::services::pve_special_sites::DrifterHiveRecord {
                site_id: "HIVE-TEST-1".into(),
                dungeon_id: 7_011,
                dungeon_name: "Vidette".into(),
                solar_system_id: sys_id,
                element_type_id: 34_575,
                element_name: "Antikythera".into(),
                current_room: 1,
                total_rooms: 3,
                warden_npc_ball_id: 1_898_701_130,
                cleared: false,
                rooms: vec![crate::services::pve_special_sites::DrifterHiveRoom {
                    room_index: 1,
                    room_name: "Perimeter".into(),
                    npc_ball_ids: Vec::new(),
                    npc_type_ids: Vec::new(),
                    beacon_ball_id: hive_beacon_id,
                    cleared: false,
                    destroyed_npc_ball_ids: Vec::new(),
                }],
            },
        );
    });

    // 2. Unscanned probe_required DED site is denied before 100% scan, then allowed ONLY for the scanning pilot.
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(&ctx, owner_char, sys_id, "dungeon", 4_201, None)
            .unwrap(),
        None
    );
    ctx.record_character_scan_progress(owner_char, sys_id, "DED-SIG-1", 1.0);
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(&ctx, owner_char, sys_id, "dungeon", 4_201, None)
            .unwrap(),
        Some(DVec3::from_array(ded_pos))
    );
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(&ctx, other_char, sys_id, "dungeon", 4_201, None)
            .unwrap(),
        None
    );

    // 3. Private escalation site is denied to non-owners and resolved for the owning pilot.
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(&ctx, other_char, sys_id, "dungeon", 4_202, None)
            .unwrap(),
        None
    );
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(&ctx, owner_char, sys_id, "dungeon", 4_202, None)
            .unwrap(),
        Some(DVec3::from_array(exp_pos))
    );

    // 4. Owned DungeonRuntimeInstance is denied to non-owners and resolved for the owning pilot.
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(&ctx, other_char, sys_id, "dungeon", 957_001, None)
            .unwrap(),
        None
    );
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(&ctx, owner_char, sys_id, "dungeon", 957_001, None)
            .unwrap(),
        Some(DVec3::new(1_000_000.0, 0.0, 0.0))
    );

    // 5. Drifter Hive refuses unrelated target_id and resolves only on explicit dungeon_id / beacon / site_id match.
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(&ctx, owner_char, sys_id, "dungeon", 999_999, None)
            .unwrap(),
        None
    );
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(&ctx, owner_char, sys_id, "dungeon", 7_011, None)
            .unwrap(),
        Some(DVec3::new(10_000.0, 0.0, 0.0))
    );
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            owner_char,
            sys_id,
            "location",
            hive_beacon_id,
            None,
        )
        .unwrap(),
        Some(DVec3::new(10_000.0, 0.0, 0.0))
    );
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            owner_char,
            sys_id,
            "dungeon",
            0,
            Some("HIVE-TEST-1"),
        )
        .unwrap(),
        Some(DVec3::new(10_000.0, 0.0, 0.0))
    );
}

#[test]
fn resolve_dsg_probe_gate_by_inst_dungeon_or_site_id_and_filters_expired_escalations() {
    let sys_id: SolarSystemId = 30_000_142;
    let scanner_char = 90_004_101i64;
    let other_char = 90_004_102i64;
    let ctx = make_ctx(sys_id, scanner_char);
    let now_unix = ctx.now_unix();

    let dsg_inst_id = 88_100_001i64;
    let dsg_did = 4_901u32;
    let dsg_sig_id = format!("DSG-{sys_id}");
    let dsg_pos = [2_500_000.0, -500_000.0, 125_000.0];

    let dsg_did_only_inst_id = 88_100_002i64;
    let dsg_did_only_did = 4_902u32;
    let dsg_did_only_sig_id = format!("DSG-{sys_id}-DID");

    let expired_exp_id = 88_200_001i64;
    let expired_exp_did = 4_903u32;
    let active_exp_id = 88_200_002i64;
    let active_exp_did = 4_904u32;
    let timeless_exp_id = 88_200_003i64;
    let timeless_exp_did = 4_905u32;

    with_dungeon_runtime_store(&ctx, |store| {
        let mut dsg_inst = sample_instance(dsg_inst_id, dsg_did, sys_id, 0, false);
        dsg_inst.entry_gate_pos = Some(dsg_pos);
        store.instances.insert(dsg_inst_id, dsg_inst);

        let mut did_only_inst = sample_instance(dsg_did_only_inst_id, dsg_did_only_did, sys_id, 0, false);
        did_only_inst.entry_gate_pos = Some([3_500_000.0, 0.0, 0.0]);
        store.instances.insert(dsg_did_only_inst_id, did_only_inst);
    });

    with_pve_exploration_reg(&ctx, |reg| {
        reg.escalations_by_char.insert(
            scanner_char,
            vec![
                crate::services::pve_expeditions::EscalatingPathEntry {
                    instance_id: expired_exp_id,
                    character_id: scanner_char,
                    source_dungeon_id: 100,
                    expedition_dungeon_id: expired_exp_did,
                    expedition_name: "Expired Escalation".into(),
                    destination_solar_system_id: sys_id,
                    step_index: 1,
                    total_steps: 2,
                    journal_text: String::new(),
                    created_at_sec: now_unix.saturating_sub(90_000),
                    expires_at_sec: now_unix.saturating_sub(60),
                    dungeons_entered: Vec::new(),
                    journal_entry_template_id: 0,
                },
                crate::services::pve_expeditions::EscalatingPathEntry {
                    instance_id: active_exp_id,
                    character_id: scanner_char,
                    source_dungeon_id: 100,
                    expedition_dungeon_id: active_exp_did,
                    expedition_name: "Active Escalation".into(),
                    destination_solar_system_id: sys_id,
                    step_index: 1,
                    total_steps: 2,
                    journal_text: String::new(),
                    created_at_sec: now_unix,
                    expires_at_sec: now_unix + 3_600,
                    dungeons_entered: Vec::new(),
                    journal_entry_template_id: 0,
                },
                crate::services::pve_expeditions::EscalatingPathEntry {
                    instance_id: timeless_exp_id,
                    character_id: scanner_char,
                    source_dungeon_id: 100,
                    expedition_dungeon_id: timeless_exp_did,
                    expedition_name: "Timeless Escalation".into(),
                    destination_solar_system_id: sys_id,
                    step_index: 1,
                    total_steps: 2,
                    journal_text: String::new(),
                    created_at_sec: now_unix,
                    expires_at_sec: 0,
                    dungeons_entered: Vec::new(),
                    journal_entry_template_id: 0,
                },
            ],
        );
        reg.sites_by_system.entry(sys_id).or_default().extend([
            PveSiteInstance {
                site_id: dsg_sig_id.clone(),
                solar_system_id: sys_id,
                dungeon_id: dsg_did,
                dungeon_name: "Seeded DSG Deadspace".into(),
                site_category: "Deadspace".into(),
                probe_required: true,
                cleared: false,
                position: dsg_pos,
                expedition_instance_id: Some(dsg_inst_id),
                ..Default::default()
            },
            PveSiteInstance {
                site_id: dsg_did_only_sig_id.clone(),
                solar_system_id: sys_id,
                dungeon_id: dsg_did_only_did,
                dungeon_name: "Dungeon-Matched DSG Site".into(),
                site_category: "Deadspace".into(),
                probe_required: true,
                cleared: false,
                position: [3_500_000.0, 0.0, 0.0],
                expedition_instance_id: None,
                ..Default::default()
            },
            PveSiteInstance {
                site_id: format!("EXP-{expired_exp_id}-1"),
                solar_system_id: sys_id,
                dungeon_id: expired_exp_did,
                dungeon_name: "Expired Escalation".into(),
                site_category: "Expedition".into(),
                probe_required: false,
                cleared: false,
                position: [4_500_000.0, 0.0, 0.0],
                expedition_instance_id: Some(expired_exp_id),
                ..Default::default()
            },
            PveSiteInstance {
                site_id: format!("EXP-{active_exp_id}-1"),
                solar_system_id: sys_id,
                dungeon_id: active_exp_did,
                dungeon_name: "Active Escalation".into(),
                site_category: "Expedition".into(),
                probe_required: false,
                cleared: false,
                position: [5_500_000.0, 0.0, 0.0],
                expedition_instance_id: Some(active_exp_id),
                ..Default::default()
            },
            PveSiteInstance {
                site_id: format!("EXP-{timeless_exp_id}-1"),
                solar_system_id: sys_id,
                dungeon_id: timeless_exp_did,
                dungeon_name: "Timeless Escalation".into(),
                site_category: "Expedition".into(),
                probe_required: false,
                cleared: false,
                position: [6_500_000.0, 0.0, 0.0],
                expedition_instance_id: Some(timeless_exp_id),
                ..Default::default()
            },
        ]);
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.entry(sys_id).or_default().extend([
            CosmicSignatureRecord {
                signature_id: dsg_sig_id.clone(),
                solar_system_id: sys_id,
                sig_group: "Combat".into(),
                type_name: "Seeded DSG Deadspace".into(),
                position: dsg_pos,
                signal_strength: 0.25,
                created_unix: now_unix,
                lifetime_seconds: 86_400,
                ..Default::default()
            },
            CosmicSignatureRecord {
                signature_id: dsg_did_only_sig_id.clone(),
                solar_system_id: sys_id,
                sig_group: "Combat".into(),
                type_name: "Dungeon-Matched DSG Site".into(),
                position: [3_500_000.0, 0.0, 0.0],
                signal_strength: 0.25,
                created_unix: now_unix,
                lifetime_seconds: 86_400,
                ..Default::default()
            },
        ]);
    });

    // 1. Before 100% probe scan, DSG-* must reject warp by dungeon_id, instance_id, DNG-INST-*, and site_id.
    for &(tid, ref tstr) in &[
        (i64::from(dsg_did), None),
        (dsg_inst_id, None),
        (0, Some(format!("DNG-INST-{dsg_inst_id}"))),
        (0, Some(dsg_sig_id.clone())),
    ] {
        assert_eq!(
            resolve_deadspace_or_escalation_warp_target(
                &ctx,
                scanner_char,
                sys_id,
                "dungeon",
                tid,
                tstr.as_deref(),
            )
            .unwrap(),
            None,
            "unscanned DSG site must be unwarpable for ({tid}, {tstr:?})"
        );
    }
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            scanner_char,
            sys_id,
            "dungeon",
            i64::from(dsg_did_only_did),
            None,
        )
        .unwrap(),
        None
    );

    // 2. After 100% scan for scanner_char, all DSG-* selectors resolve for scanner_char only.
    ctx.record_character_scan_progress(scanner_char, sys_id, &dsg_sig_id, 1.0);
    ctx.record_character_scan_progress(scanner_char, sys_id, &dsg_did_only_sig_id, 1.0);

    for &(tid, ref tstr) in &[
        (i64::from(dsg_did), None),
        (dsg_inst_id, None),
        (0, Some(format!("DNG-INST-{dsg_inst_id}"))),
        (0, Some(dsg_sig_id.clone())),
    ] {
        assert_eq!(
            resolve_deadspace_or_escalation_warp_target(
                &ctx,
                scanner_char,
                sys_id,
                "dungeon",
                tid,
                tstr.as_deref(),
            )
            .unwrap(),
            Some(DVec3::from_array(dsg_pos)),
            "100%-scanned DSG site must resolve for ({tid}, {tstr:?})"
        );
    }
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            scanner_char,
            sys_id,
            "dungeon",
            i64::from(dsg_did_only_did),
            None,
        )
        .unwrap(),
        Some(DVec3::new(3_500_000.0, 0.0, 0.0))
    );
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            other_char,
            sys_id,
            "dungeon",
            0,
            Some(dsg_sig_id.as_str()),
        )
        .unwrap(),
        None
    );
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            other_char,
            sys_id,
            "dungeon",
            dsg_inst_id,
            None,
        )
        .unwrap(),
        None
    );

    // 3. Collapsing the DSG-* signature closes warp access even with prior 100% scan progress.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) {
            if let Some(sig) = sigs.iter_mut().find(|s| s.signature_id == dsg_sig_id) {
                sig.collapsed = true;
            }
        }
    });
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            scanner_char,
            sys_id,
            "dungeon",
            dsg_inst_id,
            None,
        )
        .unwrap(),
        None
    );

    // 4. Expired escalations are filtered out of owned_exp_ids, while active and timeless ones resolve.
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            scanner_char,
            sys_id,
            "dungeon",
            i64::from(expired_exp_did),
            None,
        )
        .unwrap(),
        None
    );
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            scanner_char,
            sys_id,
            "dungeon",
            i64::from(active_exp_did),
            None,
        )
        .unwrap(),
        Some(DVec3::new(5_500_000.0, 0.0, 0.0))
    );
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            scanner_char,
            sys_id,
            "dungeon",
            i64::from(timeless_exp_did),
            None,
        )
        .unwrap(),
        Some(DVec3::new(6_500_000.0, 0.0, 0.0))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_escalation_epinstance_bookmark_and_runtime_instance_are_unwarpable() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_029_901;
    let ctx = make_ctx(sys_id, char_id);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_775_100_000));
    ctx.set_clock(clock.clone());

    let entry = crate::services::pve_expeditions::create_escalation_expedition(
        &ctx,
        char_id,
        sys_id,
        1420,
        "Guristas Scout Outpost",
        [0.0; 3],
    )
    .await
    .expect("create_escalation_expedition must seed escalation");

    let runtime_inst_id = with_dungeon_runtime_store(&ctx, |store| {
        store
            .instances
            .values()
            .find(|inst| {
                inst.owner_char_id == char_id
                    && inst.solar_system_id == entry.destination_solar_system_id
                    && inst.dungeon_id == entry.expedition_dungeon_id
                    && inst.category == "Expedition"
            })
            .map(|inst| inst.instance_id)
    })
    .expect("create_escalation_expedition must spawn owned Expedition DungeonRuntimeInstance");

    // 1. Before expiry, epinstance, dungeon_id, and owned runtime instance all resolve.
    assert!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            char_id,
            entry.destination_solar_system_id,
            "epinstance",
            entry.instance_id,
            None,
        )
        .unwrap()
        .is_some()
    );
    assert!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            char_id,
            entry.destination_solar_system_id,
            "dungeon",
            i64::from(entry.expedition_dungeon_id),
            None,
        )
        .unwrap()
        .is_some()
    );
    assert!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            char_id,
            entry.destination_solar_system_id,
            "dungeon",
            runtime_inst_id,
            None,
        )
        .unwrap()
        .is_some()
    );

    // 2. Advance past expiry WITHOUT calling journal pruning; neither epinstance (even with
    //    bookmark still present in snap.bookmarks) nor the owned runtime instance may resolve.
    clock.advance_secs(86_405);
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            char_id,
            entry.destination_solar_system_id,
            "epinstance",
            entry.instance_id,
            None,
        )
        .unwrap(),
        None,
        "expired escalation must not fall back to bookmark"
    );
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            char_id,
            entry.destination_solar_system_id,
            "dungeon",
            i64::from(entry.expedition_dungeon_id),
            None,
        )
        .unwrap(),
        None
    );
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            char_id,
            entry.destination_solar_system_id,
            "dungeon",
            runtime_inst_id,
            None,
        )
        .unwrap(),
        None
    );
    let inst_str = format!("DNG-INST-{runtime_inst_id}");
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            char_id,
            entry.destination_solar_system_id,
            "location",
            0,
            Some(inst_str.as_str()),
        )
        .unwrap(),
        None
    );

    // 3. Even if the EscalatingPathEntry was removed, an orphaned escalation bookmark or an
    //    expired bookmark_expiries entry cannot be warped via epinstance.
    with_pve_exploration_reg(&ctx, |reg| {
        reg.escalations_by_char.remove(&char_id);
    });
    assert_eq!(
        resolve_deadspace_or_escalation_warp_target(
            &ctx,
            char_id,
            entry.destination_solar_system_id,
            "epinstance",
            entry.instance_id,
            None,
        )
        .unwrap(),
        None
    );
}

async fn spawn_test_ship_ball(ctx: &ServerContext, sys_id: SolarSystemId, ship_id: i64, pos: [f64; 3]) -> crate::system_actor::SystemHandle {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::from_array(pos),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    sys_handle
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dungeon_unentered_room_is_inert_until_on_dungeon_room_entered() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_001;
    let ship_id: i64 = 1_020_000_001;
    let ctx = make_ctx(sys_id, char_id);
    let safe_did = find_non_archetype51_dungeon(&ctx.sde);
    let mut inst = sample_instance(965_001, safe_did, sys_id, char_id, false);
    let beacon_pos = [inst.rooms[0].position[0] + 500.0, 0.0, 0.0];
    inst.rooms.truncate(1);
    inst.rooms[0].npc_groups.truncate(1);
    inst.rooms[0].objects.push(DungeonRuntimeObject {
        ball_id: 0, type_id: 23, name: "Beacon".into(), position: beacon_pos,
        radius: 150.0, is_container: false, loot_items: Vec::new(), spawned: false, ..Default::default()
    });
    inst.rooms[0].triggers = vec![
        DungeonTrigger {
            trigger_id: 1,
            kind: DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: Some(1), anchor_pos: beacon_pos, radius_m: 5_000.0 },
            action: DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 111 },
            fired: false,
        },
        DungeonTrigger {
            trigger_id: 2,
            kind: DungeonTriggerKind::OnLoot { container_ball_id: None, target_object_index: Some(0), item_type_id: Some(34) },
            action: DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 222 },
            fired: false,
        },
        DungeonTrigger {
            trigger_id: 3,
            kind: DungeonTriggerKind::OnKill { target_npc_group: Some(1), target_ball_id: None },
            action: DungeonTriggerAction::UnlockGate,
            fired: false,
        },
    ];
    with_dungeon_runtime_store(&ctx, |store| { store.instances.insert(965_001, inst); });

    let mat = materialize_dungeon_room_entities(&ctx, sys_id, 965_001, 0).await.unwrap();
    let _ = spawn_test_ship_ball(&ctx, sys_id, ship_id, beacon_pos).await;
    let _ = ctx.drain_notifications_for_char(char_id);

    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);
    on_dungeon_item_looted(&ctx, char_id, Some(sys_id), mat.object_ids[0], 34).await;
    let _ = ctx.systems.get_or_spawn(sys_id).remove_ball(mat.npc_ids[0]).await;
    on_dungeon_npc_killed(&ctx, sys_id, mat.npc_ids[0], char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        let r0 = &store.instances[&965_001].rooms[0];
        assert!(r0.spawned && !r0.entered && r0.entered_unix.is_none() && r0.gate_locked && !r0.cleared);
        // F-02: dead NPC ball ID must be removed immediately so reconcile cannot resurrect it or soft-lock entry.
        assert!(r0.npc_groups[0].npc_ball_ids.is_empty());
        assert_eq!(r0.npc_groups[0].kills_recorded, 1);
        assert!(r0.triggers.iter().all(|t| !t.fired));
    });
    assert_eq!(reconcile_and_rematerialize_dungeon_instance(&ctx, sys_id, 965_001).await, 0);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // Entering the room sets entered=true and retroactively fires OnKill for pre-entry kills.
    on_dungeon_room_entered(&ctx, sys_id, 965_001, 0, char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&965_001];
        let r0 = &rt.rooms[0];
        assert!(r0.entered && r0.entered_unix.is_some() && !r0.gate_locked);
        assert_eq!(rt.last_actor_char_id, char_id);
        assert!(r0.triggers[2].fired);
    });
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    on_dungeon_item_looted(&ctx, char_id, Some(sys_id), mat.object_ids[0], 34).await;
    read_dungeon_runtime_store(&ctx, |store| {
        let r0 = &store.instances[&965_001].rooms[0];
        assert!(r0.triggers.iter().all(|t| t.fired));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dungeon_finished_instances_are_inert_across_all_lifecycle_and_rpc_helpers() {
    use crate::services::pve_dungeon_runtime_rpc::{
        check_deadspace_mwd_disallowed, check_warp_target_blocked_by_gated_dungeon_room,
        is_ship_in_mwd_disallowed_dungeon_pocket,
    };
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use glam::DVec3;

    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_002;
    let ship_id: i64 = 1_020_000_001;
    let ctx = make_ctx(sys_id, char_id);
    let safe_did = find_non_archetype51_dungeon(&ctx.sde);
    let (r1_pos, r2_pos) = ([12_000_000.0, 0.0, 0.0], [13_000_000.0, 0.0, 0.0]);
    let (gate_id, npc_id, cont_id) = (77_900_001_i64, 77_900_002_i64, 77_900_003_i64);
    let sys_handle = spawn_test_ship_ball(&ctx, sys_id, ship_id, r1_pos).await;
    let mut session = SessionState::new(902);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);

    for (idx, (completed, failed, despawned)) in
        [(true, false, false), (false, true, false), (false, false, true)].into_iter().enumerate()
    {
        let inst_id = 966_001 + (idx as i64);
        let inst = DungeonRuntimeInstance {
            instance_id: inst_id, dungeon_id: safe_did, dungeon_name: "Finished".into(),
            category: "Deadspace".into(), archetype: "Deadspace".into(), solar_system_id: sys_id,
            owner_char_id: char_id, base_position: r1_pos, disallow_mwd: true,
            current_room_index: 1, completed, failed, despawned,
            rooms: vec![
                DungeonRuntimeRoom {
                    room_index: 1, room_id: 9661, room_name: "R1".into(), position: r1_pos,
                    gate_destination_pos: r1_pos, gated: true, disallow_mwd: Some(true),
                    gate_ball_id: Some(gate_id), spawned: true, entered: true,
                    objects: vec![DungeonRuntimeObject {
                        ball_id: cont_id, type_id: DEFAULT_CONTAINER_TYPE_ID, name: "Cont".into(),
                        position: r1_pos, radius: 100.0, is_container: true,
                        loot_items: vec![(34, 10)], spawned: true, ..Default::default()
                    }],
                    npc_groups: vec![DungeonNpcGroup {
                        group_id: 1, wave: 1, npc_specs: vec![(48_090, 1)],
                        npc_ball_ids: vec![npc_id], spawn_immediately: true, spawned: true,
                        ..Default::default()
                    }],
                    triggers: vec![
                        DungeonTrigger { trigger_id: 1, kind: DungeonTriggerKind::OnKill { target_npc_group: Some(1), target_ball_id: None }, action: DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 1 }, fired: false },
                        DungeonTrigger { trigger_id: 2, kind: DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: Some(0), anchor_pos: r1_pos, radius_m: 5_000.0 }, action: DungeonTriggerAction::UnlockGate, fired: false },
                        DungeonTrigger { trigger_id: 3, kind: DungeonTriggerKind::OnLoot { container_ball_id: Some(cont_id), target_object_index: Some(0), item_type_id: Some(34) }, action: DungeonTriggerAction::UnlockGate, fired: false },
                    ],
                    ..Default::default()
                },
                DungeonRuntimeRoom {
                    room_index: 2, room_id: 9662, room_name: "R2".into(), position: r2_pos,
                    gate_destination_pos: r2_pos, gated: true, disallow_mwd: Some(true), ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(instance_is_finished(&inst));
        with_dungeon_runtime_store(&ctx, |store| { store.instances.clear(); store.instances.insert(inst_id, inst); });

        on_dungeon_npc_killed(&ctx, sys_id, npc_id, char_id).await;
        assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);
        on_dungeon_item_looted(&ctx, char_id, Some(sys_id), cont_id, 34).await;
        on_dungeon_room_entered(&ctx, sys_id, inst_id, 1, char_id).await;
        assert!(materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 1).await.is_err());
        assert!(try_activate_dungeon_runtime_gate(&ctx, &mut session, sys_id, &[PyRep::Integer(gate_id)]).await.is_none());
        assert!(check_deadspace_mwd_allowed(&ctx, sys_id, r1_pos).is_ok());
        assert!(!is_ship_in_mwd_disallowed_dungeon_pocket(&ctx, sys_id, DVec3::from_array(r1_pos)));
        assert!(check_deadspace_mwd_disallowed(&ctx, &sys_handle, sys_id, ship_id).await.is_ok());
        assert!(check_deadspace_warp_allowed(&ctx, sys_id, [0.0; 3], [0.0; 3], r1_pos).is_ok());
        assert!(check_warp_target_blocked_by_gated_dungeon_room(&ctx, sys_id, DVec3::from_array(r1_pos), Some(gate_id)).is_ok());
        assert_eq!(resolve_deadspace_or_escalation_warp_target(&ctx, char_id, sys_id, "dungeon", inst_id, None).unwrap(), None);
        assert_eq!(find_active_dungeon_id_at_position(&ctx, sys_id, r1_pos), None);
        read_dungeon_runtime_store(&ctx, |store| {
            let rt = &store.instances[&inst_id];
            assert!(!rt.rooms[1].entered && !rt.rooms[1].spawned);
            assert_eq!(rt.rooms[0].npc_groups[0].npc_ball_ids, vec![npc_id]);
            assert!(rt.rooms[0].triggers.iter().all(|t| !t.fired));
        });
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dungeon_sqlite_container_position_persistence_and_restart_reconciliation() {
    use glam::DVec3;
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_003;
    let sde = shared_sde();
    let tmp_db = std::env::temp_dir().join(format!(
        "evomu_t2a1_reconcile_{}_{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()
    ));
    let _ = std::fs::remove_file(&tmp_db);

    let repo1 = Arc::new(WorldRepository::open_sqlite(&tmp_db).expect("open sqlite repo1"));
    repo1.provision_gm_account("sqlite_pilot", "pw", char_id, "SqlitePilot");
    let ctx1 = ServerContext::with_sde_and_repository(sde.clone(), repo1.clone());
    ctx1.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_100_000)));
    let safe_did = find_non_archetype51_dungeon(&sde);
    let mut inst = sample_instance(968_001, safe_did, sys_id, char_id, false);
    let expected_cont_pos = inst.rooms[0].objects[0].position;
    inst.entry_gate_pos = Some([inst.base_position[0] - 5_000.0, 0.0, 0.0]);
    with_dungeon_runtime_store(&ctx1, |store| { store.instances.insert(968_001, inst); });
    let mat = materialize_dungeon_room_entities(&ctx1, sys_id, 968_001, 0).await.unwrap();
    let (cont_id, r0_gate_id, npc_id) = (mat.object_ids[0], mat.next_gate_id.unwrap(), mat.npc_ids[0]);
    let (entry_gate_id, items_len_before) = ctx1.db.with_snapshot(|snap| {
        assert_eq!(snap.item_positions.get(&cont_id).copied(), Some(expected_cont_pos));
        (snap.dungeon_runtime.instances[&968_001].entry_gate_ball_id.unwrap(), snap.items.len())
    });
    drop(ctx1);
    drop(repo1);

    let repo2 = Arc::new(WorldRepository::open_sqlite(&tmp_db).expect("reopen sqlite repo2"));
    let ctx2 = ServerContext::with_sde_and_repository(sde.clone(), repo2.clone());
    ctx2.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_100_050)));
    assert_eq!(reconcile_and_rematerialize_dungeon_instance(&ctx2, sys_id, 968_001).await, 4);
    assert_eq!(reconcile_and_rematerialize_dungeon_instance(&ctx2, sys_id, 968_001).await, 0);
    let sys2 = ctx2.systems.get_or_spawn(sys_id);
    assert!(sys2.get_ball_snapshot(entry_gate_id).await.unwrap().is_some());
    assert!(sys2.get_ball_snapshot(r0_gate_id).await.unwrap().is_some());
    assert!(sys2.get_ball_snapshot(npc_id).await.unwrap().is_some());
    let (_, cont_ball) = sys2.get_ball_snapshot(cont_id).await.unwrap().expect("container restored");
    assert!(cont_ball.position.distance(DVec3::from_array(expected_cont_pos)) < 1.0);
    ctx2.db.with_snapshot(|snap| {
        assert_eq!(snap.items.len(), items_len_before);
        assert_eq!(snap.item_positions.get(&cont_id).copied(), Some(expected_cont_pos));
    });

    complete_dungeon_instance_core(&ctx2.systems, &ctx2.db, Some(&ctx2.pending_notifications), Some(&ctx2.notify_tx), sys_id, 968_001, char_id, false).await;
    drop(ctx2);
    drop(repo2);

    let repo3 = Arc::new(WorldRepository::open_sqlite(&tmp_db).expect("reopen sqlite repo3"));
    let ctx3 = ServerContext::with_sde_and_repository(sde, repo3.clone());
    ctx3.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_100_100)));
    assert_eq!(reconcile_and_rematerialize_dungeon_instance(&ctx3, sys_id, 968_001).await, 1);
    let sys3 = ctx3.systems.get_or_spawn(sys_id);
    assert!(sys3.get_ball_snapshot(cont_id).await.unwrap().is_some());
    assert!(sys3.get_ball_snapshot(r0_gate_id).await.unwrap().is_none());
    assert!(sys3.get_ball_snapshot(npc_id).await.unwrap().is_none());
    drop(ctx3);
    drop(repo3);
    let _ = std::fs::remove_file(&tmp_db);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dungeon_completion_loot_grace_and_stale_retention_cleanup() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_004;
    let ctx = make_ctx(sys_id, char_id);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_775_100_000));
    ctx.set_clock(clock.clone());
    let safe_did = find_non_archetype51_dungeon(&ctx.sde);
    with_dungeon_runtime_store(&ctx, |store| { store.instances.insert(969_001, sample_instance(969_001, safe_did, sys_id, char_id, false)); });

    let r0_mat = materialize_dungeon_room_entities(&ctx, sys_id, 969_001, 0).await.unwrap();
    let cont_id = r0_mat.object_ids[0];
    on_dungeon_room_entered(&ctx, sys_id, 969_001, 0, char_id).await;
    let r1_mat = materialize_dungeon_room_entities(&ctx, sys_id, 969_001, 1).await.unwrap();
    on_dungeon_room_entered(&ctx, sys_id, 969_001, 1, char_id).await;

    on_dungeon_npc_killed(&ctx, sys_id, r1_mat.npc_ids[0], char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&969_001];
        assert!(rt.completed && !rt.despawned);
        assert_eq!(rt.loot_grace_until_unix, Some(1_775_100_000 + DUNGEON_COMPLETION_LOOT_GRACE_SECS));
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert!(sys_handle.get_ball_snapshot(cont_id).await.unwrap().is_some());

    clock.advance_secs(DUNGEON_COMPLETION_LOOT_GRACE_SECS - 1);
    assert_eq!(expire_dungeon_loot_grace(&ctx, sys_id).await, 0);
    assert!(sys_handle.get_ball_snapshot(cont_id).await.unwrap().is_some());

    clock.advance_secs(2);
    assert_eq!(expire_dungeon_loot_grace(&ctx, sys_id).await, 1);
    assert!(sys_handle.get_ball_snapshot(cont_id).await.unwrap().is_none());
    ctx.db.with_snapshot(|snap| {
        let rt = &snap.dungeon_runtime.instances[&969_001];
        assert!(rt.despawned && rt.loot_grace_until_unix.is_none());
        assert!(!snap.items.contains_key(&cont_id) && !snap.item_positions.contains_key(&cont_id));
    });

    clock.advance_secs(COMPLETED_INSTANCE_RETENTION_SECS + 1);
    with_dungeon_runtime_store(&ctx, |store| {
        prune_stale_despawned_instances(store, clock.now_unix_secs());
        assert!(!store.instances.contains_key(&969_001));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_dungeon_instance_enforces_bounded_parser_limits_and_finite_coords() {
    use eve_proto::PyRep;
    use std::collections::HashMap;

    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_005;
    let ctx = make_ctx(sys_id, char_id);
    let safe_did = find_non_archetype51_dungeon(&ctx.sde);
    let pair = |a: i64, b: i64| PyRep::List(vec![PyRep::Integer(a), PyRep::Integer(b)]);

    let room0 = PyRep::keyval([
        ("roomName", PyRep::String("Sanitized Room 0".into())),
        ("objects", PyRep::List(vec![PyRep::keyval([
            ("typeID", PyRep::Integer(23)), ("isContainer", PyRep::Bool(true)),
            ("offset", PyRep::List(vec![PyRep::Real(f64::NAN), PyRep::Real(f64::INFINITY), PyRep::Real(1.0e20)])),
            ("radius", PyRep::Real(f64::NAN)),
            ("lootItems", PyRep::List(vec![pair(34, 0), pair(34, -5), pair(34, 7)])),
        ])])),
        ("npcGroups", PyRep::List(vec![PyRep::keyval([("npcs", PyRep::List(vec![pair(48_090, 1)]))])])),
    ]);
    let heavy_loot = PyRep::List((0..70).map(|_| pair(34, 1)).collect());
    let big_objs = PyRep::List((0..70).map(|i| PyRep::keyval([
        ("typeID", PyRep::Integer(23)), ("name", PyRep::String(format!("O{i}"))), ("lootItems", heavy_loot.clone()),
    ])).collect());
    let big_npcs = PyRep::List((0..40).map(|_| pair(48_090, 9_999)).collect());
    let mut big_triggers: Vec<PyRep> = vec![PyRep::keyval([
        ("kind", PyRep::String("approach".into())),
        ("radius", PyRep::Real(1.0e20)),
        ("action", PyRep::String("UnlockGate".into())),
    ])];
    big_triggers.extend((1..70).map(|_| PyRep::keyval([("kind", PyRep::String("kill".into())), ("action", PyRep::String("UnlockGate".into()))])));
    let mut big_links: Vec<PyRep> = (0..15).map(|i| PyRep::keyval([("targetRoomIndex", PyRep::Integer(i))])).collect();
    big_links.push(PyRep::keyval([("targetRoomIndex", PyRep::Integer(999))]));
    big_links.extend((16..20).map(|i| PyRep::keyval([("targetRoomIndex", PyRep::Integer(i % 16))])));
    let room1_heavy = PyRep::keyval([
        ("roomName", PyRep::String("Bounded Room 1".into())),
        ("objects", big_objs),
        ("npcGroups", PyRep::List((0..40).map(|_| PyRep::keyval([("npcs", big_npcs.clone())])).collect())),
        ("triggers", PyRep::List(big_triggers)),
        ("gateLinks", PyRep::List(big_links)),
    ]);
    let mut rooms_vec = vec![room0, room1_heavy];
    rooms_vec.extend((2..20).map(|idx| PyRep::keyval([("roomName", PyRep::String(format!("Extra {idx}")))])));
    let mut kwargs = HashMap::from([
        ("solarSystemID".into(), PyRep::Integer(i64::from(sys_id))),
        ("dungeonID".into(), PyRep::Integer(i64::from(safe_did))),
        ("hasEntryGate".into(), PyRep::Bool(false)),
        ("position".into(), PyRep::List(vec![PyRep::Real(f64::NAN), PyRep::Real(f64::INFINITY), PyRep::Real(0.0)])),
        ("rooms".into(), PyRep::List(rooms_vec)),
    ]);
    kwargs.shrink_to_fit();

    let spawned = spawn_dungeon_instance(&ctx, char_id, Some(sys_id), &[], &kwargs).await.unwrap();
    let inst_id = spawned.dict_get("instanceID").and_then(|v| v.as_i64()).unwrap();
    read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&inst_id];
        assert_eq!(rt.base_position, [120_000_000.0, 0.0, 0.0]);
        assert_eq!(rt.rooms.len(), 16);
        let obj0 = &rt.rooms[0].objects[0];
        assert!(obj0.position.iter().all(|v| v.is_finite()));
        assert_eq!(obj0.position[2], 1.0e15);
        assert_eq!(obj0.radius, 300.0);
        assert_eq!(obj0.loot_items, vec![(34, 7)]);
        let r1 = &rt.rooms[1];
        assert_eq!((r1.objects.len(), r1.objects[0].loot_items.len(), r1.npc_groups.len(), r1.npc_groups[0].npc_specs.len(), r1.triggers.len(), r1.gate_links.len()), (64, 64, 32, 32, 64, 15));
        assert_eq!(r1.npc_groups[0].npc_specs[0], (48_090, 500));
        assert!(matches!(r1.triggers[0].kind, DungeonTriggerKind::OnApproach { radius_m, .. } if radius_m == 1_000_000.0));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_instance_loot_grace_expiry_despawns_without_marking_completed() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_006;
    let ctx = make_ctx(sys_id, char_id);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_775_100_000));
    ctx.set_clock(clock.clone());
    let safe_did = find_non_archetype51_dungeon(&ctx.sde);
    let mut inst = sample_instance(971_001, safe_did, sys_id, char_id, false);
    inst.rooms.truncate(1);
    inst.rooms[0].npc_groups.clear();
    inst.rooms[0].triggers.clear();
    with_dungeon_runtime_store(&ctx, |store| { store.instances.insert(971_001, inst); });

    let mat = materialize_dungeon_room_entities(&ctx, sys_id, 971_001, 0).await.unwrap();
    let cont_id = mat.object_ids[0];
    let grace_due = 1_775_100_000 + 60;
    with_dungeon_runtime_store(&ctx, |store| {
        let rt = store.instances.get_mut(&971_001).unwrap();
        rt.failed = true;
        rt.loot_grace_until_unix = Some(grace_due);
        rt.despawn_after_unix = Some(grace_due);
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    clock.advance_secs(61);
    assert_eq!(expire_dungeon_loot_grace(&ctx, sys_id).await, 1);

    ctx.db.with_snapshot(|snap| {
        let rt = &snap.dungeon_runtime.instances[&971_001];
        assert!(rt.despawned);
        assert!(rt.failed);
        assert!(!rt.completed);
        assert!(!rt.rooms[0].cleared);
        assert!(rt.completed_unix.is_none());
        assert!(rt.loot_grace_until_unix.is_none());
        assert!(!snap.items.contains_key(&cont_id) && !snap.item_positions.contains_key(&cont_id));
    });
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnDungeonCompleted"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ungated_dungeon_room0_lifecycle_clears_and_completes_without_manual_entry_injection() {
    use eve_proto::PyRep;
    use std::collections::HashMap;

    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_007;
    let ship_id: i64 = 1_020_000_001;
    let ctx = make_ctx(sys_id, char_id);
    let safe_did = find_non_archetype51_dungeon(&ctx.sde);
    let pair = |a: i64, b: i64| PyRep::List(vec![PyRep::Integer(a), PyRep::Integer(b)]);
    let pos = [15_000_000.0, 0.0, 0.0];
    let room0 = PyRep::keyval([
        ("roomName", PyRep::String("Open Entry Room".into())),
        ("objects", PyRep::List(vec![PyRep::keyval([("typeID", PyRep::Integer(23)), ("offset", PyRep::List(vec![PyRep::Real(0.0); 3]))])])),
        ("npcGroups", PyRep::List(vec![PyRep::keyval([("npcs", PyRep::List(vec![pair(48_090, 1)]))])])),
        ("triggers", PyRep::List(vec![
            PyRep::keyval([("kind", PyRep::String("approach".into())), ("targetObjectIndex", PyRep::Integer(0)), ("radius", PyRep::Real(5_000.0)), ("action", PyRep::String("UnlockGate".into()))]),
            PyRep::keyval([("kind", PyRep::String("kill".into())), ("targetGroup", PyRep::Integer(1)), ("action", PyRep::String("CompleteDungeon".into()))]),
        ])),
    ]);
    let kwargs = HashMap::from([
        ("solarSystemID".into(), PyRep::Integer(i64::from(sys_id))),
        ("dungeonID".into(), PyRep::Integer(i64::from(safe_did))),
        ("hasEntryGate".into(), PyRep::Bool(false)),
        ("position".into(), PyRep::List(vec![PyRep::Real(pos[0]), PyRep::Real(pos[1]), PyRep::Real(pos[2])])),
        ("rooms".into(), PyRep::List(vec![room0])),
    ]);

    let spawned = spawn_dungeon_instance(&ctx, char_id, Some(sys_id), &[], &kwargs).await.unwrap();
    let inst_id = spawned.dict_get("instanceID").and_then(|v| v.as_i64()).unwrap();
    let npc_id = read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&inst_id];
        assert!(rt.rooms[0].entered && rt.rooms[0].entered_unix.is_some());
        rt.rooms[0].npc_groups[0].npc_ball_ids[0]
    });
    let _ = spawn_test_ship_ball(&ctx, sys_id, ship_id, pos).await;
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    on_dungeon_npc_killed(&ctx, sys_id, npc_id, char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&inst_id];
        assert!(rt.rooms[0].cleared && rt.completed && rt.despawned);
    });

    // Gated Room 0: landing at entry_gate_pos must keep Room 0 unentered until gate activation.
    let mut gated = sample_instance(971_002, safe_did, sys_id, char_id, false);
    gated.base_position = [25_000_000.0, 0.0, 0.0];
    gated.entry_gate_pos = Some(gated.base_position);
    gated.current_room_index = 0;
    gated.rooms[0].position = gated.base_position;
    gated.rooms[0].gated = true;
    with_dungeon_runtime_store(&ctx, |store| { store.instances.insert(971_002, gated); });
    on_warp_landed_at_dungeon_pocket_if_present(&ctx, char_id, sys_id, [25_000_000.0, 0.0, 0.0]).await;
    read_dungeon_runtime_store(&ctx, |store| {
        assert!(!store.instances[&971_002].rooms[0].entered);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dungeon_gate_links_branching_lock_destination_and_oob_validation() {
    use crate::services::pve_exploration::handle_pve_exploration;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use glam::DVec3;
    use std::collections::HashMap;

    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_008;
    let ship_id: i64 = 1_020_000_001;
    let ctx = make_ctx(sys_id, char_id);
    let safe_did = find_non_archetype51_dungeon(&ctx.sde);
    let base = [35_000_000.0, 0.0, 0.0];
    let gate_a_pos = [base[0] + 4_000.0, 0.0, 0.0];
    let gate_b_pos = [base[0] + 7_000.0, 0.0, 0.0];
    let dest_b = [base[0] + 1_234_000.0, 222.0, 333.0];
    let vec3_rep = |p: [f64; 3]| PyRep::List(vec![PyRep::Real(p[0]), PyRep::Real(p[1]), PyRep::Real(p[2])]);

    let room0 = PyRep::keyval([
        ("roomName", PyRep::String("Hub".into())),
        ("gateLinks", PyRep::List(vec![
            PyRep::keyval([("targetRoomIndex", PyRep::Integer(1)), ("gatePosition", vec3_rep(gate_a_pos)), ("locked", PyRep::Bool(true)), ("label", PyRep::String("Locked Path".into()))]),
            PyRep::keyval([("targetRoomIndex", PyRep::Integer(2)), ("gatePosition", vec3_rep(gate_b_pos)), ("destinationPos", vec3_rep(dest_b)), ("locked", PyRep::Bool(false)), ("label", PyRep::String("Shortcut".into()))]),
            PyRep::keyval([("targetRoomIndex", PyRep::Integer(99)), ("label", PyRep::String("Invalid OOB".into()))]),
        ])),
    ]);
    let kwargs = HashMap::from([
        ("solarSystemID".into(), PyRep::Integer(i64::from(sys_id))),
        ("dungeonID".into(), PyRep::Integer(i64::from(safe_did))),
        ("hasEntryGate".into(), PyRep::Bool(false)),
        ("position".into(), vec3_rep(base)),
        ("rooms".into(), PyRep::List(vec![room0, PyRep::keyval([("roomName", PyRep::String("Side".into()))]), PyRep::keyval([("roomName", PyRep::String("Vault".into()))])])),
    ]);

    let spawned = spawn_dungeon_instance(&ctx, char_id, Some(sys_id), &[], &kwargs).await.unwrap();
    let inst_id = spawned.dict_get("instanceID").and_then(|v| v.as_i64()).unwrap();
    let (gate_a_id, gate_b_id) = read_dungeon_runtime_store(&ctx, |store| {
        let links = &store.instances[&inst_id].rooms[0].gate_links;
        assert_eq!(links.len(), 2, "OOB targetRoomIndex=99 must be dropped");
        (links[0].gate_ball_id.unwrap(), links[1].gate_ball_id.unwrap())
    });

    let sys_handle = spawn_test_ship_ball(&ctx, sys_id, ship_id, gate_a_pos).await;
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let mut sess = SessionState::new(908);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();

    let locked_err = handle_pve_exploration(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_a_id)], &kw)
        .await.expect("routed").expect_err("link 0 is locked");
    assert_eq!(locked_err.code, "GateLocked");

    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(gate_b_pos)).await;
    let ok_rep = handle_pve_exploration(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_b_id)], &kw)
        .await.expect("routed").expect("link 1 shortcut succeeds");
    assert_eq!(ok_rep.dict_get("roomIndex").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(ok_rep.dict_get("destinationX").and_then(|v| v.as_f64()), Some(dest_b[0]));
    let (_, ship_after) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(ship_after.position.distance(DVec3::from_array(dest_b)) < 1.0);
    read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&inst_id];
        assert!(!rt.rooms[1].entered && rt.rooms[2].entered);
    });
    assert!(
        is_ship_in_deadspace_pocket(&ctx, sys_id, dest_b),
        "custom gate_links.destination_pos must be covered by deadspace pocket guards"
    );
    assert!(
        crate::services::pve_dungeon_runtime_rpc::check_warp_target_blocked_by_gated_dungeon_room(
            &ctx,
            sys_id,
            DVec3::from_array(dest_b),
            None,
        )
        .is_err()
    );

    // Once marked failed, pve_exploration & try_activate_dungeon_runtime_gate ignore the gate.
    with_dungeon_runtime_store(&ctx, |store| { store.instances.get_mut(&inst_id).unwrap().failed = true; });
    assert!(try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &[PyRep::Integer(gate_b_id)]).await.is_none());
    assert!(handle_pve_exploration(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_b_id)], &kw).await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pre_entry_boss_kill_catchup_handles_unspawned_wave2_and_surviving_escort() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_009;
    let ctx = make_ctx(sys_id, char_id);
    let safe_did = find_non_archetype51_dungeon(&ctx.sde);
    let boss_id = 77_950_001_i64;
    let escort_id = 77_950_002_i64;

    // Case A: unspawned Wave 2 trigger keyed by OnKill(target_ball_id = Some(boss_id)).
    let mut inst_wave = sample_instance(972_001, safe_did, sys_id, char_id, false);
    inst_wave.rooms.truncate(1);
    inst_wave.rooms[0].position = [40_000_000.0, 0.0, 0.0];
    inst_wave.rooms[0].npc_groups = vec![
        DungeonNpcGroup {
            group_id: 1,
            wave: 1,
            npc_specs: vec![(48_090, 1)],
            npc_ball_ids: vec![boss_id],
            spawn_immediately: true,
            spawned: true,
            cleared: false,
            ..Default::default()
        },
        DungeonNpcGroup {
            group_id: 2,
            wave: 2,
            npc_specs: vec![(48_090, 1)],
            npc_ball_ids: Vec::new(),
            spawn_immediately: false,
            spawned: false,
            cleared: false,
            ..Default::default()
        },
    ];
    inst_wave.rooms[0].triggers = vec![
        DungeonTrigger {
            trigger_id: 1,
            kind: DungeonTriggerKind::OnKill {
                target_npc_group: None,
                target_ball_id: Some(boss_id),
            },
            action: DungeonTriggerAction::SpawnNpcGroup(2),
            fired: false,
        },
        DungeonTrigger {
            trigger_id: 2,
            kind: DungeonTriggerKind::OnKill {
                target_npc_group: Some(2),
                target_ball_id: None,
            },
            action: DungeonTriggerAction::CompleteDungeon,
            fired: false,
        },
    ];
    inst_wave.rooms[0].spawned = true;
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(972_001, inst_wave);
    });

    on_dungeon_npc_killed(&ctx, sys_id, boss_id, char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        let r0 = &store.instances[&972_001].rooms[0];
        assert_eq!(r0.npc_groups[0].dead_npc_ball_ids, vec![boss_id]);
        assert!(!r0.triggers[0].fired && !r0.npc_groups[1].spawned);
    });
    on_dungeon_room_entered(&ctx, sys_id, 972_001, 0, char_id).await;
    let w2_id = read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&972_001];
        let r0 = &rt.rooms[0];
        assert!(r0.triggers[0].fired, "pre-entry boss kill must trigger Wave 2 on entry");
        assert!(r0.npc_groups[1].spawned && !r0.cleared && !rt.completed);
        r0.npc_groups[1].npc_ball_ids[0]
    });
    on_dungeon_npc_killed(&ctx, sys_id, w2_id, char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        assert!(store.instances[&972_001].completed);
    });

    // Case B: surviving escort in same group must keep room uncleared until escort also dies.
    let boss2 = 77_950_011_i64;
    let mut inst_escort = sample_instance(972_002, safe_did, sys_id, char_id, false);
    inst_escort.rooms.truncate(1);
    inst_escort.rooms[0].position = [42_000_000.0, 0.0, 0.0];
    inst_escort.rooms[0].npc_groups = vec![DungeonNpcGroup {
        group_id: 1,
        wave: 1,
        npc_specs: vec![(48_090, 2)],
        npc_ball_ids: vec![boss2, escort_id],
        spawn_immediately: true,
        spawned: true,
        cleared: false,
        ..Default::default()
    }];
    inst_escort.rooms[0].triggers = vec![DungeonTrigger {
        trigger_id: 1,
        kind: DungeonTriggerKind::OnKill {
            target_npc_group: None,
            target_ball_id: Some(boss2),
        },
        action: DungeonTriggerAction::UnlockGate,
        fired: false,
    }];
    inst_escort.rooms[0].spawned = true;
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(972_002, inst_escort);
    });

    on_dungeon_npc_killed(&ctx, sys_id, boss2, char_id).await;
    on_dungeon_room_entered(&ctx, sys_id, 972_002, 0, char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&972_002];
        let r0 = &rt.rooms[0];
        assert!(r0.triggers[0].fired && !r0.gate_locked);
        assert_eq!(r0.npc_groups[0].npc_ball_ids, vec![escort_id]);
        assert!(!r0.cleared && !rt.completed);
    });
    on_dungeon_npc_killed(&ctx, sys_id, escort_id, char_id).await;
    read_dungeon_runtime_store(&ctx, |store| {
        assert!(store.instances[&972_002].rooms[0].cleared && store.instances[&972_002].completed);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dng_inst_string_gate_activation_at_room0_uses_entry_gate_not_gate_links() {
    use eve_machonet::SessionState;
    use eve_proto::PyRep;

    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_010;
    let ship_id: i64 = 1_020_000_002;
    let ctx = make_ctx(sys_id, char_id);
    let safe_did = find_non_archetype51_dungeon(&ctx.sde);
    let entry_pos = [45_000_000.0, 0.0, 0.0];
    let r0_pos = [45_020_000.0, 0.0, 0.0];
    let link_gate_pos = [45_025_000.0, 0.0, 0.0];
    let r2_dest = [45_900_000.0, 0.0, 0.0];
    let entry_gate_id = 77_960_001_i64;

    let mut inst = sample_instance(973_001, safe_did, sys_id, char_id, true);
    inst.base_position = entry_pos;
    inst.entry_gate_pos = Some(entry_pos);
    inst.entry_gate_ball_id = Some(entry_gate_id);
    inst.entry_gate_locked = true;
    inst.current_room_index = 0;
    inst.rooms.push(DungeonRuntimeRoom {
        room_index: 3,
        room_id: 9733,
        room_name: "R2".into(),
        position: r2_dest,
        gate_destination_pos: r2_dest,
        gated: true,
        ..Default::default()
    });
    inst.rooms[0].position = r0_pos;
    inst.rooms[0].gate_destination_pos = r0_pos;
    inst.rooms[0].gate_links = vec![DungeonGateLink {
        target_room_index: 2,
        gate_position: link_gate_pos,
        destination_pos: r2_dest,
        locked: false,
        label: "Shortcut".into(),
        ..Default::default()
    }];
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(973_001, inst);
    });

    let sys_handle = spawn_test_ship_ball(&ctx, sys_id, ship_id, entry_pos).await;
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let mut sess = SessionState::new(910);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let arg = [PyRep::String("DNG-INST-973001".into())];

    let locked_err = try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &arg)
        .await
        .expect("resolved")
        .expect_err("entry gate is locked; must not bypass via rooms[0].gate_links[0]");
    assert_eq!(locked_err.code, "GateLocked");

    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.get_mut(&973_001).unwrap().entry_gate_locked = false;
    });
    let enter_r0 = try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &arg)
        .await
        .expect("resolved")
        .expect("entry gate succeeds");
    assert_eq!(enter_r0.dict_get("roomIndex").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(enter_r0.dict_get("gateID").and_then(|v| v.as_i64()), Some(entry_gate_id));
    read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&973_001];
        assert_eq!(rt.current_room_index, 1);
        assert!(rt.rooms[0].entered && !rt.rooms[2].entered);
    });

    let _ = sys_handle.set_ball_position(ship_id, glam::DVec3::from_array(link_gate_pos)).await;
    let enter_r2 = try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &arg)
        .await
        .expect("resolved")
        .expect("from Room 0 DNG-INST now resolves rooms[0].gate_links[0]");
    assert_eq!(enter_r2.dict_get("roomIndex").and_then(|v| v.as_i64()), Some(3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warp_landing_respects_unmaterialized_entry_gate_skipped_rooms_and_remote_expeditions() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_011;
    let ctx = make_ctx(sys_id, char_id);
    let safe_did = find_non_archetype51_dungeon(&ctx.sde);
    let entry_pos = [50_000_000.0, 0.0, 0.0];
    let skipped_pos = [50_600_000.0, 0.0, 0.0];

    let mut inst = sample_instance(974_001, safe_did, sys_id, char_id, false);
    inst.base_position = entry_pos;
    inst.entry_gate_pos = Some(entry_pos);
    inst.entry_gate_ball_id = None;
    inst.current_room_index = 3; // e.g. Room 0 -> Room 2 jump skipped Room 1
    inst.rooms[0].position = entry_pos;
    inst.rooms[0].gated = false; // unmaterialized entry gate with room.gated = false
    inst.rooms[1].position = skipped_pos;
    inst.rooms[1].gated = true;
    inst.rooms[1].entered = false;
    inst.rooms[1].spawned = false;
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(974_001, inst);
    });

    on_warp_landed_at_dungeon_pocket_if_present(&ctx, char_id, sys_id, entry_pos).await;
    read_dungeon_runtime_store(&ctx, |store| {
        let rt = &store.instances[&974_001];
        assert!(rt.entry_gate_ball_id.is_some());
        assert!(!rt.rooms[0].entered, "unmaterialized entry_gate_pos must keep Room 0 unentered");
    });

    on_warp_landed_at_dungeon_pocket_if_present(&ctx, char_id, sys_id, skipped_pos).await;
    read_dungeon_runtime_store(&ctx, |store| {
        let r1 = &store.instances[&974_001].rooms[1];
        assert!(!r1.entered && !r1.spawned, "skipped gated interior Room 1 must stay unmaterialized and unentered");
    });

    // Remote expedition escalation spawns with rooms[0].entered = false until pilot arrives.
    let esc = crate::services::pve_expeditions::create_escalation_expedition(
        &ctx,
        char_id,
        sys_id,
        1420,
        "Guristas Hideaway",
        [60_000_000.0, 0.0, 0.0],
    )
    .await
    .expect("create_escalation_expedition");
    read_dungeon_runtime_store(&ctx, |store| {
        let rt = store
            .instances
            .values()
            .find(|i| i.solar_system_id == esc.destination_solar_system_id && i.category == "Expedition")
            .expect("expedition runtime spawned");
        assert!(!rt.rooms[0].entered, "remote unvisited escalation must remain unentered until pilot arrives");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_cluster_tick_and_loot_out_despawn_completed_and_failed_loot_grace() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_012;
    let ctx = make_ctx(sys_id, char_id);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_775_200_000));
    ctx.set_clock(clock.clone());
    let safe_did = find_non_archetype51_dungeon(&ctx.sde);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // 1. Background cluster tick despawns overdue completed loot-grace container + instance.
    let mut inst1 = sample_instance(975_001, safe_did, sys_id, char_id, false);
    inst1.rooms.truncate(1);
    inst1.rooms[0].triggers.clear();
    inst1.rooms[0].npc_groups.clear();
    with_dungeon_runtime_store(&ctx, |store| { store.instances.insert(975_001, inst1); });
    let mat1 = materialize_dungeon_room_entities(&ctx, sys_id, 975_001, 0).await.unwrap();
    complete_dungeon_instance_core(&ctx.systems, &ctx.db, Some(&ctx.pending_notifications), Some(&ctx.notify_tx), sys_id, 975_001, char_id, false).await;
    clock.advance_secs(DUNGEON_COMPLETION_LOOT_GRACE_SECS + 5);
    crate::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(sys_handle.get_ball_snapshot(mat1.object_ids[0]).await.unwrap().is_none());
    ctx.db.with_snapshot(|snap| {
        assert!(snap.dungeon_runtime.instances[&975_001].despawned);
        assert!(!snap.items.contains_key(&mat1.object_ids[0]));
    });

    // 2. Clock advance sync sweep despawns overdue failed loot-grace instance without marking completed.
    let mut inst2 = sample_instance(975_002, safe_did, sys_id, char_id, false);
    inst2.rooms.truncate(1);
    inst2.rooms[0].triggers.clear();
    inst2.rooms[0].npc_groups.clear();
    with_dungeon_runtime_store(&ctx, |store| { store.instances.insert(975_002, inst2); });
    let mat2 = materialize_dungeon_room_entities(&ctx, sys_id, 975_002, 0).await.unwrap();
    let due2 = clock.now_unix_secs() + 60;
    with_dungeon_runtime_store(&ctx, |store| {
        let rt = store.instances.get_mut(&975_002).unwrap();
        rt.failed = true;
        rt.loot_grace_until_unix = Some(due2);
        rt.despawn_after_unix = Some(due2);
    });
    let prev = clock.now_unix_secs();
    clock.advance_secs(65);
    crate::sp6_lifecycle::on_advance_secs(&ctx, prev, 65);
    assert!(sys_handle.get_ball_snapshot(mat2.object_ids[0]).await.unwrap().is_none());
    ctx.db.with_snapshot(|snap| {
        let rt = &snap.dungeon_runtime.instances[&975_002];
        assert!(rt.despawned && rt.failed && !rt.completed);
    });

    // 3. Looting out the last container of a failed loot-grace instance despawns early.
    let mut inst3 = sample_instance(975_003, safe_did, sys_id, char_id, false);
    inst3.rooms.truncate(1);
    inst3.rooms[0].triggers.clear();
    inst3.rooms[0].npc_groups.clear();
    with_dungeon_runtime_store(&ctx, |store| { store.instances.insert(975_003, inst3); });
    let mat3 = materialize_dungeon_room_entities(&ctx, sys_id, 975_003, 0).await.unwrap();
    let cont3 = mat3.object_ids[0];
    let future_due = clock.now_unix_secs() + 600;
    ctx.db.with_mut_snapshot(|snap| {
        let rt = snap.dungeon_runtime.instances.get_mut(&975_003).unwrap();
        rt.failed = true;
        rt.loot_grace_until_unix = Some(future_due);
        rt.despawn_after_unix = Some(future_due);
        snap.items.retain(|_, item| item.location_id != cont3);
    });
    on_dungeon_item_looted(&ctx, char_id, Some(sys_id), cont3, 34).await;
    assert!(sys_handle.get_ball_snapshot(cont3).await.unwrap().is_none());
    ctx.db.with_snapshot(|snap| {
        let rt = &snap.dungeon_runtime.instances[&975_003];
        assert!(rt.despawned && rt.failed && !rt.completed);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dng_inst_current_room0_without_entry_gate_rejects_observatory_bypass_and_missing_gate() {
    use crate::services::pve_exploration::handle_pve_exploration;
    use crate::services::pve_fob_observatory::{get_observatory_site_state, spawn_observatory_flashpoint};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use glam::DVec3;
    use std::collections::HashMap;

    let pochven_sys = shared_sde()
        .solar_systems
        .values()
        .find(|s| s.region_id == 10_000_070)
        .map(|s| s.solar_system_id)
        .expect("SDE has Pochven solar system");
    let char_id: i64 = 90_040_013;
    let ship_id: i64 = 1_020_000_003;
    let ctx = make_ctx(pochven_sys, char_id);
    let _ = spawn_observatory_flashpoint(&ctx, pochven_sys)
        .await
        .expect("spawn Observatory Flashpoint");
    let obs = get_observatory_site_state(&ctx, pochven_sys).expect("observatory persisted");
    let obs_inst_id = obs.dungeon_instance_id;

    let far_pos = [obs.gate_pos[0] + 100_000.0, obs.gate_pos[1], obs.gate_pos[2]];
    let sys_handle = spawn_test_ship_ball(&ctx, pochven_sys, ship_id, far_pos).await;
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let mut sess = SessionState::new(913);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(pochven_sys);
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();
    let obs_dng_arg = [PyRep::String(format!("DNG-INST-{obs_inst_id}"))];

    assert!(
        matches!(
            handle_pve_exploration(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &obs_dng_arg, &kw).await,
            None | Some(Err(_))
        ),
        "Observatory Flashpoint must reject DNG-INST string gate bypass"
    );
    assert!(
        try_activate_dungeon_runtime_gate(&ctx, &mut sess, pochven_sys, &obs_dng_arg)
            .await
            .is_none(),
        "try_activate_dungeon_runtime_gate must reject Observatory Flashpoint DNG-INST bypass"
    );
    let (_, ship_after_obs) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(ship_after_obs.position.distance(DVec3::from_array(far_pos)) < 1.0);

    // Non-Flashpoint instance with current_room_index == 0 and no entry gate / 0-indexed gate room -> None
    let safe_did = find_non_archetype51_dungeon(&ctx.sde);
    let no_gate = sample_instance(976_001, safe_did, pochven_sys, char_id, false);
    with_dungeon_runtime_store(&ctx, |store| {
        let mut i = no_gate;
        i.current_room_index = 0;
        store.instances.insert(976_001, i);
    });
    assert!(
        try_activate_dungeon_runtime_gate(
            &ctx,
            &mut sess,
            pochven_sys,
            &[PyRep::String("DNG-INST-976001".into())],
        )
        .await
        .is_none(),
        "current_room_index == 0 with no entry gate or 0-indexed gate room must not bypass range/lock"
    );

    // Explicit 0-indexed outer gate room enforces <= 2500m range and lock state via DNG-INST.
    let g0_id = 77_970_001_i64;
    let mut r0_gate_inst = sample_instance(976_002, safe_did, pochven_sys, char_id, false);
    r0_gate_inst.current_room_index = 0;
    r0_gate_inst.rooms[0].room_index = 0;
    r0_gate_inst.rooms[0].position = [80_000_000.0, 0.0, 0.0];
    r0_gate_inst.rooms[0].gate_ball_id = Some(g0_id);
    r0_gate_inst.rooms[0].gate_locked = true;
    r0_gate_inst.rooms[1].gate_destination_pos = [80_500_000.0, 0.0, 0.0];
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(976_002, r0_gate_inst);
    });
    let r0_arg = [PyRep::String("DNG-INST-976002".into())];
    let range_err = try_activate_dungeon_runtime_gate(&ctx, &mut sess, pochven_sys, &r0_arg)
        .await
        .expect("resolved")
        .expect_err("must enforce <= 2500m proximity on 0-indexed gate room");
    assert_eq!(range_err.code, "OutsideGateActivationRange");

    let _ = sys_handle.set_ball_position(ship_id, DVec3::new(80_005_000.0, 0.0, 0.0)).await;
    let locked_err = try_activate_dungeon_runtime_gate(&ctx, &mut sess, pochven_sys, &r0_arg)
        .await
        .expect("resolved")
        .expect_err("must enforce gate_locked on 0-indexed gate room");
    assert_eq!(locked_err.code, "GateLocked");

    // Unmaterialized entry_gate_ball_id with entry_gate_pos = None must fail closed with GateNotFound.
    let mut missing_pos_inst = sample_instance(976_003, safe_did, pochven_sys, char_id, false);
    missing_pos_inst.current_room_index = 0;
    missing_pos_inst.entry_gate_pos = None;
    missing_pos_inst.entry_gate_ball_id = Some(77_970_099);
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(976_003, missing_pos_inst);
    });
    let missing_err = try_activate_dungeon_runtime_gate(
        &ctx,
        &mut sess,
        pochven_sys,
        &[PyRep::String("DNG-INST-976003".into())],
    )
    .await
    .expect("resolved")
    .expect_err("missing physical gate position must fail closed with GateNotFound");
    assert_eq!(missing_err.code, "GateNotFound");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_gate_link_destination_falls_back_to_target_room_gate_destination_pos() {
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use glam::DVec3;

    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_040_014;
    let ship_id: i64 = 1_020_000_004;
    let ctx = make_ctx(sys_id, char_id);
    let safe_did = find_non_archetype51_dungeon(&ctx.sde);
    let r0_pos = [74_000_000.0, 0.0, 0.0];
    let gate_pos = [74_005_000.0, 0.0, 0.0];
    let r1_pos = [75_000_000.0, 0.0, 0.0];
    let r1_landing = [74_990_000.0, 1_000.0, 0.0];
    let link_gate_id = 77_980_001_i64;

    let mut inst = sample_instance(977_001, safe_did, sys_id, char_id, false);
    inst.base_position = r0_pos;
    inst.current_room_index = 1;
    inst.rooms.truncate(2);
    inst.rooms[0].position = r0_pos;
    inst.rooms[0].entered = true;
    inst.rooms[0].gate_links = vec![DungeonGateLink {
        gate_ball_id: Some(link_gate_id),
        gate_position: gate_pos,
        target_room_index: 1,
        locked: false,
        ..Default::default()
    }];
    inst.rooms[1].position = r1_pos;
    inst.rooms[1].gate_destination_pos = r1_landing;
    inst.rooms[1].disallow_mwd = Some(true);
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(977_001, inst);
    });

    let sys_handle = spawn_test_ship_ball(&ctx, sys_id, ship_id, gate_pos).await;
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let mut sess = SessionState::new(914);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    let rep = try_activate_dungeon_runtime_gate(&ctx, &mut sess, sys_id, &[PyRep::Integer(link_gate_id)])
        .await
        .expect("resolved")
        .expect("gate activation succeeds");
    assert_eq!(rep.dict_get("destinationX").and_then(|v| v.as_f64()), Some(r1_landing[0]));
    assert_eq!(rep.dict_get("destinationY").and_then(|v| v.as_f64()), Some(r1_landing[1]));
    let (_, ship_after) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        ship_after.position.distance(DVec3::from_array(r1_landing)) < 1.0,
        "unset DungeonGateLink.destination_pos must fall back to target room gate_destination_pos"
    );
    assert!(is_ship_in_deadspace_pocket(&ctx, sys_id, r1_landing));
    assert_eq!(
        check_deadspace_mwd_allowed(&ctx, sys_id, r1_landing).unwrap_err().code,
        "ModuleNotAllowedInDeadspace"
    );

    // Also verify DNG-INST- path falls back identically without landing at [0, 0, 0].
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.get_mut(&977_001).unwrap().current_room_index = 1;
    });
    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(gate_pos)).await;
    let str_rep = try_activate_dungeon_runtime_gate(
        &ctx,
        &mut sess,
        sys_id,
        &[PyRep::String("DNG-INST-977001".into())],
    )
    .await
    .expect("resolved")
    .expect("DNG-INST gate activation succeeds");
    assert_eq!(str_rep.dict_get("destinationX").and_then(|v| v.as_f64()), Some(r1_landing[0]));
}
