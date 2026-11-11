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

    let MaterializedDungeonRoomEntities {
        npc_ids: r2_npcs, ..
    } = materialize_dungeon_room_entities(&ctx, sys_id, 950_010, 1)
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
    inst2.rooms[0].triggers.clear();
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(956_002, inst2);
    });
    let MaterializedDungeonRoomEntities { npc_ids: npcs2, .. } =
        materialize_dungeon_room_entities(&ctx, sys_id, 956_002, 0)
            .await
            .unwrap();
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
