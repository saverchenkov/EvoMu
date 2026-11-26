//! Track 2a-3 verification tests for `#1495`: 10-variant trigger matrix (`I1`–`I3`),
//! `OWN-R5-01` multi-pilot isolation (`PVE-212`), `OWN-R5-04` shared/compound timers,
//! `OWN-R5-05` subwarp `OnApproach` + public tick attribution, and `R5 P2-1` read-purity.

use super::super::*;
use crate::router::ServerContext;
use crate::services::pve_dungeon_runtime_rpc::{on_dungeon_approach, spawn_dungeon_instance};
use crate::services::pve_exploration::with_pve_exploration_reg;
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, IskCents, WorldRepository};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{DungeonTemplate, FactionInfo, SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

async fn make_t2a3_ctx(
    sde: Arc<SdeRegistry>,
    repo: Arc<WorldRepository>,
    sys_id: SolarSystemId,
    char_id: i64,
    user_tag: &str,
    pos: DVec3,
) -> (ServerContext, SessionState, i64) {
    repo.provision_gm_account(user_tag, "pw", char_id, user_tag);
    let (uid, ship_id) = repo.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        let sid = ch.active_ship_id;
        let uid = ch.user_id;
        if let Some(it) = snap.items.get_mut(&sid) {
            it.location_id = i64::from(sys_id);
            it.flag = InventoryFlag::None;
        }
        (uid, sid)
    });
    let _ = repo.acquire_online_lock_with_computer_hash(uid, char_id as u64, Some(char_id), None, true, 1_785_100_000);
    let ctx = ServerContext::with_sde_and_repository(sde, repo);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_785_100_000)));
    ctx.systems.get_or_spawn(sys_id).spawn_ball(Ball {
        id: ship_id, type_id: 587, position: pos, velocity: DVec3::ZERO,
        max_velocity: 2_500.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    let mut sess = SessionState::new(char_id as u64);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    (ctx, sess, ship_id)
}

fn t2_room(room_index: usize, room_id: i64, pos: [f64; 3], triggers: Vec<DungeonTrigger>) -> DungeonRuntimeRoom {
    DungeonRuntimeRoom {
        room_index, room_id, room_name: format!("Room {room_index}"),
        position: pos, gate_destination_pos: pos, gated: room_index > 1, gate_locked: true, triggers, ..Default::default()
    }
}

fn t2_trig(trigger_id: u32, kind: DungeonTriggerKind, action: DungeonTriggerAction, repeatable: bool) -> DungeonTrigger {
    DungeonTrigger { trigger_id, kind, action, repeatable, ..Default::default() }
}

async fn spawn_extra_pilot(ctx: &ServerContext, sys_id: SolarSystemId, char_id: i64, tag: &str, pos: DVec3) -> i64 {
    ctx.db.provision_gm_account(tag, "pw", char_id, tag);
    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id
    });
    ctx.systems.get_or_spawn(sys_id).spawn_ball(Ball {
        id: ship_id, type_id: 587, position: pos, velocity: DVec3::ZERO,
        max_velocity: 2_500.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    ship_id
}

async fn hit_npc_below_pct(
    sys_h: &crate::system_actor::SystemHandle,
    npc_id: i64,
    attacker: i64,
    target_pct: u8,
) {
    for _ in 0..16 {
        let step_dmg = {
            let view = sys_h.view();
            let Some(cs) = view.combat_states.get(&npc_id) else { return };
            let max_hp = (cs.max_shield_hp + cs.max_armor_hp + cs.max_structure_hp).max(1.0);
            let cur_hp = (cs.shield_hp + cs.armor_hp + cs.structure_hp).max(0.0);
            if ((cur_hp / max_hp) * 100.0).round() as u8 <= target_pct {
                return;
            }
            (max_hp * 0.12).max(1.0)
        };
        let _ = sys_h
            .apply_damage_with_attacker(
                npc_id,
                DamageVector { em: step_dmg, thermal: step_dmg, kinetic: step_dmg, explosive: step_dmg },
                Some(attacker),
            )
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a3_ten_trigger_kinds_and_invariants_matrix() {
    let (sys_id, char_id, inst_id) = (30_000_142u32, 90_050_001i64, 991_001i64);
    let tmp_db = std::env::temp_dir().join(format!("evomu_t2a3_matrix_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&tmp_db);
    let repo1 = Arc::new(WorldRepository::open_sqlite(&tmp_db).unwrap());
    let (r1_pos, entry_pos) = ([40_000_000.0, 0.0, 0.0], [39_900_000.0, 0.0, 0.0]);
    let (ctx1, mut sess, ship_id) = make_t2a3_ctx(shared_sde(), repo1.clone(), sys_id, char_id, "t2a3_mat", DVec3::from_array(entry_pos)).await;
    let sys1 = ctx1.systems.get_or_spawn(sys_id);
    let entry_gid = ctx1.db.with_mut_snapshot(|s| s.alloc_entity_id());
    sys1.spawn_ball(Ball {
        id: entry_gid, type_id: ACCELERATION_GATE_TYPE_ID, position: DVec3::from_array(entry_pos),
        velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1e7, radius: 1_500.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();

    // Cover all 10 DungeonTriggerKind variants via production RPC/damage/tick/loot paths + SQLite reopen (`I1`-`I3`).
    let mut r1 = t2_room(1, 9101, r1_pos, vec![
        t2_trig(1, DungeonTriggerKind::OnRoomEntered { target_room_index: None }, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 1001 }, false),
        t2_trig(2, DungeonTriggerKind::OnNpcDamaged { target_npc_group: Some(1), target_tag: None, target_ball_id: None, hull_or_shield_below_pct: 99, on_first_attack: true }, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 1002 }, false),
        t2_trig(3, DungeonTriggerKind::OnNpcDamaged { target_npc_group: Some(1), target_tag: Some("officer".into()), target_ball_id: None, hull_or_shield_below_pct: 60, on_first_attack: false }, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 1003 }, false),
        t2_trig(4, DungeonTriggerKind::OnKillTagged { target_tag: "officer".into(), required_kills: Some(2), type_ids: vec![48_090] }, DungeonTriggerAction::SpawnObject { object_index: 0 }, false),
        t2_trig(5, DungeonTriggerKind::OnKillCount { target_npc_group: Some(1), required_kills: 2 }, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 1005 }, true),
        t2_trig(6, DungeonTriggerKind::OnKill { target_npc_group: Some(1), target_ball_id: None }, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 1006 }, false),
        t2_trig(7, DungeonTriggerKind::OnTimer { delay_secs: 3, timer_id: None, after_group_cleared: Some(1) }, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 1007 }, false),
        t2_trig(8, DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: Some(0), anchor_pos: r1_pos, radius_m: 3_000.0 }, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 1008 }, false),
        t2_trig(9, DungeonTriggerKind::OnLoot { container_ball_id: None, target_object_index: Some(0), item_type_id: Some(34) }, DungeonTriggerAction::StartTimer { timer_id: "fin".into(), delay_secs: 2 }, false),
        t2_trig(10, DungeonTriggerKind::CompoundOr { clauses: vec![
            DungeonTriggerKind::OnTimer { delay_secs: 2, timer_id: Some("fin".into()), after_group_cleared: None },
            DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: [r1_pos[0] + 50_000.0, 0.0, 0.0], radius_m: 2_500.0 },
        ]}, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 1010 }, false),
        t2_trig(11, DungeonTriggerKind::CompoundAnd { clauses: vec![
            DungeonTriggerKind::OnRoomEntered { target_room_index: None },
            DungeonTriggerKind::OnKillCount { target_npc_group: Some(1), required_kills: 3 },
        ]}, DungeonTriggerAction::CompleteDungeon, false),
    ]);
    r1.objects = vec![DungeonRuntimeObject {
        type_id: DEFAULT_CONTAINER_TYPE_ID, name: "Delayed Cache".into(),
        position: [r1_pos[0] + 1_000.0, 0.0, 0.0], radius: 150.0, is_container: true, loot_items: vec![(34, 5)],
        spawn_on_trigger: true, ..Default::default()
    }];
    r1.npc_groups = vec![DungeonNpcGroup {
        group_id: 1, wave: 1, spawn_immediately: true,
        npc_entries: vec![
            DungeonNpcSpawnEntry { type_id: 48_090, count: 2, tag: Some("officer".into()), ..Default::default() },
            DungeonNpcSpawnEntry { type_id: 48_090, count: 1, tag: Some("escort".into()), ..Default::default() },
        ],
        ..Default::default()
    }];
    let inst = DungeonRuntimeInstance {
        instance_id: inst_id, dungeon_id: 1, dungeon_name: "10-Matrix".into(), category: "Deadspace".into(),
        archetype: "Deadspace".into(), solar_system_id: sys_id, owner_char_id: char_id, base_position: r1_pos,
        entry_gate_ball_id: Some(entry_gid), entry_gate_pos: Some(entry_pos), rooms: vec![r1], created_unix: 1_785_100_000, ..Default::default()
    };
    with_dungeon_runtime_store(&ctx1, |s| { s.instances.insert(inst_id, inst); });

    // Gate activation -> room entered (`I1`: OnRoomEntered fires, CompoundAnd clause 0 latched, unspawned approach `0`).
    let _ = ctx1.drain_notifications_for_char(char_id);
    assert!(try_activate_dungeon_runtime_gate(&ctx1, &mut sess, sys_id, &[PyRep::Integer(entry_gid)]).await.unwrap().is_ok());
    let (npcs, cont_pos) = read_dungeon_runtime_store(&ctx1, |s| {
        let rt = &s.instances[&inst_id];
        assert!(rt.rooms[0].triggers[0].fired && !rt.rooms[0].triggers[10].fired);
        (rt.rooms[0].npc_groups[0].npc_ball_ids.clone(), rt.rooms[0].objects[0].position)
    });
    assert_eq!(on_dungeon_approach(&ctx1, char_id, sys_id, ship_id, None).await, 0, "unspawned object[0] must not fire");

    // Non-lethal damage drives OnNpcDamaged (first_attack + threshold) on 1 Hz tick; then lethal kill drives OnKillCount & OnKillTagged.
    hit_npc_below_pct(&sys1, npcs[0], char_id, 60).await;
    sys1.step_manual(1.0).await.unwrap();
    read_dungeon_runtime_store(&ctx1, |s| {
        assert!(s.instances[&inst_id].rooms[0].triggers[1].fired);
        assert!(s.instances[&inst_id].rooms[0].triggers[2].fired);
    });
    // Wrong-tag NPC kill (#2 escort) advances OnKillCount=1 without firing OnKillTagged.
    let lethal = DamageVector { em: 500_000.0, thermal: 500_000.0, kinetic: 500_000.0, explosive: 500_000.0 };
    let _ = sys1.apply_damage_with_attacker(npcs[2], lethal, Some(char_id)).await.unwrap();
    let _ = sys1.apply_damage_with_attacker(npcs[0], lethal, Some(char_id)).await.unwrap();
    read_dungeon_runtime_store(&ctx1, |s| {
        let trigs = &s.instances[&inst_id].rooms[0].triggers;
        assert!(!trigs[3].fired, "OnKillTagged needs 2 officers");
        assert!(s.instances[&inst_id].rooms[0].armed_timers.values().count() >= 1);
    });

    // Reopen SQLite & reconcile (`PVE-210`) while CompoundAnd + OnKillTagged progress is mid-flight!
    drop(sys1);
    drop(ctx1);
    drop(repo1);
    let repo2 = Arc::new(WorldRepository::open_sqlite(&tmp_db).unwrap());
    let (ctx2, _, _) = make_t2a3_ctx(shared_sde(), repo2.clone(), sys_id, char_id, "t2a3_mat", DVec3::from_array(cont_pos)).await;
    assert_eq!(reconcile_and_rematerialize_dungeon_instance(&ctx2, sys_id, inst_id).await, 2); // ship + remaining officer? entry_gid + officer
    let sys2 = ctx2.systems.get_or_spawn(sys_id);
    let _ = sys2.apply_damage_with_attacker(npcs[1], lethal, Some(char_id)).await.unwrap();
    read_dungeon_runtime_store(&ctx2, |s| {
        let r0 = &s.instances[&inst_id].rooms[0];
        assert!(r0.triggers[3].fired && r0.objects[0].spawned, "2nd officer kill spawns Delayed Cache");
        assert!(r0.triggers[5].fired && s.instances[&inst_id].completed, "OnKill + CompoundAnd(3 kills) fire");
    });
    // Re-trigger checks while completed (`I2`) must no-op; verify timer/loot/approach on a clean live room below!
    assert_eq!(on_dungeon_approach(&ctx2, char_id, sys_id, ship_id, None).await, 0);
    drop(sys2);
    drop(ctx2);
    drop(repo2);
    let _ = std::fs::remove_file(&tmp_db);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a3_approach_loot_after_group_timer_and_compound_or_e2e() {
    let (sys_id, char_id, inst_id) = (30_000_142u32, 90_050_002i64, 991_002i64);
    let (ctx, _, ship_id) = make_t2a3_ctx(shared_sde(), Arc::new(WorldRepository::new_empty()), sys_id, char_id, "t2a3_app_loot", DVec3::new(41_000_000.0, 0.0, 0.0)).await;
    let r1_pos = [41_000_000.0, 0.0, 0.0];
    let mut r1 = t2_room(1, 9201, r1_pos, vec![
        t2_trig(1, DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: Some(0), anchor_pos: r1_pos, radius_m: 3_000.0 }, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 201 }, false),
        t2_trig(2, DungeonTriggerKind::OnLoot { container_ball_id: None, target_object_index: Some(0), item_type_id: Some(34) }, DungeonTriggerAction::StartTimer { timer_id: "chain".into(), delay_secs: 2 }, false),
        t2_trig(3, DungeonTriggerKind::OnTimer { delay_secs: 3, timer_id: None, after_group_cleared: Some(1) }, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 203 }, false),
        t2_trig(4, DungeonTriggerKind::CompoundOr { clauses: vec![
            DungeonTriggerKind::OnTimer { delay_secs: 2, timer_id: Some("chain".into()), after_group_cleared: None },
            DungeonTriggerKind::OnLoot { container_ball_id: None, target_object_index: Some(0), item_type_id: Some(99) },
        ]}, DungeonTriggerAction::UnlockGate, false),
    ]);
    r1.entered = true;
    r1.entered_unix = Some(1_785_100_000);
    r1.objects = vec![DungeonRuntimeObject {
        type_id: DEFAULT_CONTAINER_TYPE_ID, name: "Loot Box".into(), position: r1_pos,
        radius: 150.0, is_container: true, loot_items: vec![(34, 5)], ..Default::default()
    }];
    r1.npc_groups = vec![DungeonNpcGroup { group_id: 1, wave: 1, npc_specs: vec![(48_090, 1)], spawn_immediately: true, ..Default::default() }];
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_id, DungeonRuntimeInstance {
            instance_id: inst_id, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: char_id,
            base_position: r1_pos, rooms: vec![r1], created_unix: 1_785_100_000, ..Default::default()
        });
    });
    let mat = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0).await.unwrap();
    let (cont_id, npc_id) = (mat.object_ids[0], mat.npc_ids[0]);
    let sys_h = ctx.systems.get_or_spawn(sys_id);

    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, Some(cont_id)).await, 1);
    on_dungeon_item_looted(&ctx, char_id, Some(sys_id), cont_id, 35).await;
    read_dungeon_runtime_store(&ctx, |s| assert!(!s.instances[&inst_id].rooms[0].triggers[1].fired));
    on_dungeon_item_looted(&ctx, char_id, Some(sys_id), cont_id, 34).await;
    on_dungeon_npc_killed(&ctx, sys_id, npc_id, char_id).await;
    ctx.advance_secs(2);
    sys_h.step_manual(1.0).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        let trigs = &s.instances[&inst_id].rooms[0].triggers;
        assert!(trigs[1].fired && trigs[3].fired && !trigs[2].fired, "chain named timer fires at +2s; after_group_cleared waits +3s");
    });
    ctx.advance_secs(1);
    sys_h.step_manual(1.0).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        let rt = &s.instances[&inst_id];
        assert!(rt.rooms[0].triggers[2].fired && !rt.rooms[0].gate_locked && rt.completed);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a3_multipilot_on_approach_isolated_latches_and_read_purity() {
    let (sys_id, pilot_a, pilot_b, inst_id) = (30_000_142u32, 90_050_010i64, 90_050_011i64, 991_003i64);
    let r1_pos = [42_000_000.0, 0.0, 0.0];
    let (ctx, _, ship_a) = make_t2a3_ctx(shared_sde(), Arc::new(WorldRepository::new_empty()), sys_id, pilot_a, "pilot_a", DVec3::from_array(r1_pos)).await;
    let ship_b = spawn_extra_pilot(&ctx, sys_id, pilot_b, "pilot_b", DVec3::new(0.0, 0.0, 0.0)).await;
    let mut r1 = t2_room(1, 9301, r1_pos, vec![
        t2_trig(1, DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: r1_pos, radius_m: 3_000.0 }, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 301 }, true),
    ]);
    r1.spawned = true;
    r1.entered = true;
    r1.entered_unix = Some(1_785_100_000);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_id, DungeonRuntimeInstance {
            instance_id: inst_id, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: pilot_a,
            base_position: r1_pos, rooms: vec![r1], created_unix: 1_785_100_000, ..Default::default()
        });
    });

    assert_eq!(on_dungeon_approach(&ctx, pilot_a, sys_id, ship_a, None).await, 1);
    let tx0 = ctx.db.tx_commit_count();
    assert_eq!(on_dungeon_approach(&ctx, pilot_a, sys_id, ship_a, None).await, 0, "Pilot A stays latched inside radius");
    assert_eq!(on_dungeon_approach(&ctx, pilot_b, sys_id, ship_b, None).await, 0, "Pilot B outside pocket must not fire or reset A");
    ctx.systems.get_or_spawn(sys_id).step_manual(1.0).await.unwrap();
    assert_eq!(ctx.db.tx_commit_count(), tx0, "idle re-approaches and unrelated sys traffic must commit 0 DB txs (PVE-212)");

    // Pilot A stays latched if queried again, then exits & re-enters cleanly (`I3`).
    assert_eq!(on_dungeon_approach(&ctx, pilot_a, sys_id, ship_a, None).await, 0);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    assert!(sys_h.set_ball_position(ship_a, DVec3::new(r1_pos[0] + 10_000.0, 0.0, 0.0)).await);
    assert_eq!(on_dungeon_approach(&ctx, pilot_a, sys_id, ship_a, None).await, 0);
    assert!(sys_h.set_ball_position(ship_a, DVec3::from_array(r1_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, pilot_a, sys_id, ship_a, None).await, 1, "re-entering radius fires 2nd occurrence");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a3_subwarp_flight_into_on_approach_fires_on_tick() {
    let (sys_id, char_id, inst_id) = (30_000_142u32, 90_050_020i64, 991_004i64);
    let r1_pos = [43_000_000.0, 0.0, 0.0];
    let (ctx, _, ship_id) = make_t2a3_ctx(shared_sde(), Arc::new(WorldRepository::new_empty()), sys_id, char_id, "subwarp_a", DVec3::new(r1_pos[0] + 7_500.0, 0.0, 0.0)).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h.remove_ball(ship_id).await;
    sys_h.spawn_ball(Ball {
        id: ship_id, type_id: 587, position: DVec3::new(r1_pos[0] + 7_500.0, 0.0, 0.0),
        velocity: DVec3::new(-2_000.0, 0.0, 0.0), max_velocity: 2_000.0, inertia: 1.0, mass: 1_000_000.0,
        radius: 40.0, speed_fraction: 1.0, mode: MovementMode::GotoDirection { direction: DVec3::new(-1.0, 0.0, 0.0) },
    }).await.unwrap();
    let mut r1 = t2_room(1, 9401, r1_pos, vec![
        t2_trig(1, DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: r1_pos, radius_m: 3_000.0 }, DungeonTriggerAction::UnlockGate, false),
    ]);
    r1.spawned = true;
    r1.entered = true;
    r1.entered_unix = Some(1_785_100_000);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_id, DungeonRuntimeInstance {
            instance_id: inst_id, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: char_id,
            base_position: r1_pos, rooms: vec![r1], created_unix: 1_785_100_000, ..Default::default()
        });
    });

    let tx0 = ctx.db.tx_commit_count();
    crate::services::pve_dungeon_runtime::tick_dungeon_runtime_system_step(&ctx, sys_id).await;
    assert_eq!(ctx.db.tx_commit_count(), tx0, "tick at 7_500m must stay read-pure (R5 P2-1)");
    for _ in 0..2 {
        sys_h.step_manual(1.0).await.unwrap();
    }
    read_dungeon_runtime_store(&ctx, |s| {
        let rm = &s.instances[&inst_id].rooms[0];
        assert!(rm.gate_locked && rm.armed_timers.is_empty(), "outside 3_000m before 3rd tick");
    });
    let tx_mid = ctx.db.tx_commit_count();
    crate::services::pve_dungeon_runtime::tick_dungeon_runtime_system_step(&ctx, sys_id).await;
    assert_eq!(ctx.db.tx_commit_count(), tx_mid, "tick_dungeon_runtime_system_step outside radius must stay read-pure (R5 P2-1)");

    sys_h.step_manual(1.0).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| assert!(!s.instances[&inst_id].rooms[0].gate_locked && s.instances[&inst_id].rooms[0].triggers[0].fired, "subwarp arrival inside radius fires OnApproach on tick"));
    let tx_after = ctx.db.tx_commit_count();
    crate::services::pve_dungeon_runtime::tick_dungeon_runtime_system_step(&ctx, sys_id).await;
    assert_eq!(ctx.db.tx_commit_count(), tx_after, "latched OnApproach tick must stay read-pure (R5 P2-1)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a3_shared_named_timer_and_compound_or_early_approach_own_r5_04() {
    let (sys_id, char_id, inst_a, inst_b) = (30_000_142u32, 90_050_030i64, 991_005i64, 991_006i64);
    let r_pos = [44_000_000.0, 0.0, 0.0];
    let (ctx, _, ship_id) = make_t2a3_ctx(shared_sde(), Arc::new(WorldRepository::new_empty()), sys_id, char_id, "r5_04", DVec3::from_array(r_pos)).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);

    // Instance A: Two triggers sharing one named timer_id ("boom") must BOTH fire on the same tick.
    let mut ra = t2_room(1, 9501, r_pos, vec![
        t2_trig(1, DungeonTriggerKind::OnRoomEntered { target_room_index: None }, DungeonTriggerAction::StartTimer { timer_id: "boom".into(), delay_secs: 4 }, false),
        t2_trig(2, DungeonTriggerKind::OnTimer { delay_secs: 4, timer_id: Some("boom".into()), after_group_cleared: None }, DungeonTriggerAction::UnlockGate, true),
        t2_trig(3, DungeonTriggerKind::OnTimer { delay_secs: 4, timer_id: Some("boom".into()), after_group_cleared: None }, DungeonTriggerAction::SpawnNpcGroup(2), true),
    ]);
    ra.spawned = true;
    ra.npc_groups = vec![DungeonNpcGroup { group_id: 2, wave: 2, npc_specs: vec![(48_090, 1)], spawn_immediately: false, ..Default::default() }];
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_a, DungeonRuntimeInstance {
            instance_id: inst_a, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: char_id,
            base_position: r_pos, rooms: vec![ra], created_unix: 1_785_100_000, ..Default::default()
        });
    });
    on_dungeon_room_entered(&ctx, sys_id, inst_a, 0, char_id).await;
    ctx.advance_secs(3);
    let tx_wait = ctx.db.tx_commit_count();
    sys_h.step_manual(1.0).await.unwrap();
    assert_eq!(ctx.db.tx_commit_count(), tx_wait, "unexpired timer ticks stay read-pure");
    ctx.advance_secs(1);
    sys_h.step_manual(1.0).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_a].rooms[0];
        assert!(!r.gate_locked && r.npc_groups[0].spawned && !r.armed_timers.contains_key("boom"), "both shared named timer triggers fire and then clear 'boom'");
    });
    let tx_after = ctx.db.tx_commit_count();
    sys_h.step_manual(1.0).await.unwrap();
    assert_eq!(ctx.db.tx_commit_count(), tx_after);

    // Instance B: Repeatable CompoundOr(OnTimer, OnApproach) firing early on approach preserves unexpired named/unnamed timers.
    let rb_pos = [45_000_000.0, 0.0, 0.0];
    let mut rb = t2_room(1, 9502, rb_pos, vec![
        t2_trig(1, DungeonTriggerKind::OnRoomEntered { target_room_index: None }, DungeonTriggerAction::StartTimer { timer_id: "early_t".into(), delay_secs: 30 }, false),
        t2_trig(2, DungeonTriggerKind::CompoundOr { clauses: vec![
            DungeonTriggerKind::OnTimer { delay_secs: 30, timer_id: Some("early_t".into()), after_group_cleared: None },
            DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: rb_pos, radius_m: 3_000.0 },
        ]}, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 501 }, true),
        t2_trig(3, DungeonTriggerKind::CompoundOr { clauses: vec![
            DungeonTriggerKind::OnTimer { delay_secs: 20, timer_id: None, after_group_cleared: None },
            DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: rb_pos, radius_m: 3_000.0 },
        ]}, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 502 }, true),
    ]);
    rb.spawned = true;
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_b, DungeonRuntimeInstance {
            instance_id: inst_b, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: char_id,
            base_position: rb_pos, rooms: vec![rb], created_unix: 1_785_100_004, ..Default::default()
        });
    });
    on_dungeon_room_entered(&ctx, sys_id, inst_b, 0, char_id).await;
    ctx.advance_secs(5);
    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(rb_pos)).await);
    let _ = ctx.drain_notifications_for_char(char_id);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 2);
    read_dungeon_runtime_store(&ctx, |s| {
        let at = &s.instances[&inst_b].rooms[0].armed_timers;
        assert_eq!(at.get("early_t").copied(), Some(1_785_100_004 + 30), "early approach must not delete unexpired named timer");
        assert!(!at.keys().any(|k| k.starts_with("__rep_timer:")), "early approach must not reset unexpired unnamed timer");
    });
    assert!(sys_h.set_ball_position(ship_id, DVec3::new(0.0, 0.0, 0.0)).await);
    let _ = ctx.drain_notifications_for_char(char_id);
    ctx.advance_secs(15); // total +20s
    sys_h.step_manual(1.0).await.unwrap();
    let notes_20 = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notes_20.iter().filter(|n| n.event == "OnDungeonTriggerMessage").count(), 1, "unnamed 20s timer still fires at original deadline");
    ctx.advance_secs(10); // total +30s
    sys_h.step_manual(1.0).await.unwrap();
    let notes_30 = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notes_30.iter().filter(|n| n.event == "OnDungeonTriggerMessage").count(), 1, "named 30s timer still fires at original deadline");
    read_dungeon_runtime_store(&ctx, |s| assert!(!s.instances[&inst_b].rooms[0].armed_timers.contains_key("early_t")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a3_public_dungeon_tick_delivery_reward_escalation_and_message() {
    let (sys_id, char_id, inst_id, esc_did) = (30_000_142u32, 90_050_040i64, 991_007i64, 991_888u32);
    let r_pos = [46_000_000.0, 0.0, 0.0];
    let mut sde_mut = (*shared_sde()).clone();
    sde_mut.factions.insert(500_002, FactionInfo {
        faction_id: 500_002, name: "Pub Esc Corp".into(), corporation_id: Some(1_000_002),
        militia_corporation_id: None, solar_system_id: Some(sys_id), size_factor: 1.0, description: String::new(),
    });
    sde_mut.dungeons.insert(esc_did, DungeonTemplate {
        dungeon_id: esc_did, name: "Public Citadel".into(), faction_id: Some(500_002), archetype_id: Some(20), difficulty: Some(3),
    });
    let (ctx, _, _) = make_t2a3_ctx(Arc::new(sde_mut), Arc::new(WorldRepository::new_empty()), sys_id, char_id, "pub_t2a3", DVec3::from_array(r_pos)).await;
    let bal0 = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);

    // Public dungeon (`owner_char_id == 0`, `last_actor_char_id == 0`) with pilot physically in pocket.
    let mut delayed_msg = t2_trig(2, DungeonTriggerKind::OnNpcDamaged { target_npc_group: Some(1), target_tag: None, target_ball_id: None, hull_or_shield_below_pct: 99, on_first_attack: true }, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 7777 }, false);
    delayed_msg.delay_secs = 3;
    let mut r1 = t2_room(1, 9601, r_pos, vec![
        t2_trig(1, DungeonTriggerKind::OnNpcDamaged { target_npc_group: Some(1), target_tag: None, target_ball_id: None, hull_or_shield_below_pct: 99, on_first_attack: true }, DungeonTriggerAction::GrantReward { isk: 250_000, lp: 500, corp_id: 1_000_002, faction_id: 500_002, standing_milli: 250 }, false),
        delayed_msg,
        t2_trig(3, DungeonTriggerKind::OnTimer { delay_secs: 5, timer_id: None, after_group_cleared: None }, DungeonTriggerAction::StartEscalation { target_dungeon_id: Some(esc_did), chance_bps: 10_000, target_title: Some("Public Citadel".into()) }, false),
    ]);
    r1.entered = true;
    r1.entered_unix = Some(1_785_100_000);
    r1.npc_groups = vec![DungeonNpcGroup { group_id: 1, wave: 1, npc_specs: vec![(48_090, 1)], spawn_immediately: true, ..Default::default() }];
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_id, DungeonRuntimeInstance {
            instance_id: inst_id, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: 0,
            base_position: r_pos, rooms: vec![r1], created_unix: 1_785_100_000, ..Default::default()
        });
    });
    let npc_id = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0).await.unwrap().npc_ids[0];
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = ctx.drain_notifications_for_char(char_id);
    hit_npc_below_pct(&sys_h, npc_id, char_id, 90).await;
    sys_h.step_manual(1.0).await.unwrap();
    ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&char_id];
        assert_eq!(ch.balance, bal0.checked_add(IskCents::from_isk(250_000)).unwrap());
        assert_eq!(ch.loyalty_points.get(&1_000_002).copied(), Some(500));
        assert!(ch.standings.get(&500_002).copied().unwrap_or(0.0) > 0.0);
    });
    ctx.advance_secs(3);
    sys_h.step_manual(1.0).await.unwrap();
    ctx.advance_secs(2);
    sys_h.step_manual(1.0).await.unwrap();
    with_pve_exploration_reg(&ctx, |reg| {
        assert_eq!(reg.escalations_by_char.get(&char_id).map(|v| v.len()), Some(1));
    });
    let notes = ctx.drain_notifications_for_char(char_id);
    assert!(notes.iter().any(|n| n.event == "OnDungeonTriggerMessage"));
    assert!(notes.iter().any(|n| n.event == "OnEscalatingPathChange"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a3_pyrep_parser_all_10_kinds_start_timer_and_bounds() {
    let (sys_id, char_id) = (30_000_142u32, 90_050_050i64);
    let (ctx, _, _) = make_t2a3_ctx(shared_sde(), Arc::new(WorldRepository::new_empty()), sys_id, char_id, "pyrep_parse", DVec3::ZERO).await;
    let deep_and = (0..6).fold(PyRep::keyval([("kind", PyRep::String("kill".into())), ("targetGroup", PyRep::Integer(2))]), |inner, _| {
        PyRep::keyval([("kind", PyRep::String("and".into())), ("clauses", PyRep::List(vec![inner]))])
    });
    let room_rep = PyRep::keyval([
        ("roomName", PyRep::String("Parser Room".into())),
        ("npcGroups", PyRep::List(vec![PyRep::keyval([
            ("groupID", PyRep::Integer(1)),
            ("npcEntries", PyRep::List(vec![PyRep::keyval([("typeID", PyRep::Integer(48_090)), ("count", PyRep::Integer(2)), ("tag", PyRep::String("alpha".into()))])])),
        ])])),
        ("triggers", PyRep::List(vec![
            PyRep::keyval([("kind", PyRep::String("room_entered".into())), ("targetRoomIndex", PyRep::Integer(0)), ("action", PyRep::String("StartTimer".into())), ("timerID", PyRep::String("t1".into())), ("delaySecs", PyRep::Integer(5))]),
            PyRep::keyval([("kind", PyRep::String("kill_tagged".into())), ("targetTag", PyRep::String("alpha".into())), ("requiredKills", PyRep::Integer(2)), ("typeIDs", PyRep::List(vec![PyRep::Integer(48_090)]))]),
            PyRep::keyval([("kind", PyRep::String("kill_count".into())), ("targetGroup", PyRep::Integer(1)), ("requiredKills", PyRep::Integer(3))]),
            PyRep::keyval([("kind", PyRep::String("timer".into())), ("timerID", PyRep::String("t1".into())), ("afterGroupCleared", PyRep::Integer(1)), ("delaySecs", PyRep::Integer(7))]),
            PyRep::keyval([("kind", PyRep::String("npc_damaged".into())), ("targetGroup", PyRep::Integer(1)), ("targetTag", PyRep::String("alpha".into())), ("belowPct", PyRep::Integer(45)), ("onFirstAttack", PyRep::Bool(false))]),
            PyRep::keyval([("kind", PyRep::String("or".into())), ("clauses", PyRep::List(vec![PyRep::keyval([("kind", PyRep::String("approach".into()))]), PyRep::keyval([("kind", PyRep::String("loot".into()))])]))]),
            deep_and,
            PyRep::keyval([("kind", PyRep::String("timer".into())), ("timerID", PyRep::String("__reserved".into())), ("delaySecs", PyRep::Integer(4)), ("triggerDelaySecs", PyRep::Integer(2)), ("action", PyRep::String("StartTimer".into())), ("actionTargetRoomIndex", PyRep::Integer(1))]),
            PyRep::keyval([("kind", PyRep::String("timer".into())), ("timerID", PyRep::String("__internal_probe".into())), ("delaySecs", PyRep::Integer(1)), ("action", PyRep::String("TriggerMessage".into())), ("messageID", PyRep::Integer(9909))]),
            PyRep::keyval([("kind", PyRep::String("approach".into())), ("delaySecs", PyRep::Integer(3)), ("timerID", PyRep::String("p_extra".into())), ("extraActions", PyRep::List(vec![PyRep::String("StartTimer".into())]))]),
        ])),
    ]);
    let res = spawn_dungeon_instance(&ctx, char_id, Some(sys_id), &[PyRep::Integer(1), PyRep::List(vec![room_rep])], &HashMap::new()).await.unwrap();
    let inst_id = res.dict_get("instanceID").unwrap().as_i64().unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert_eq!(r.npc_groups[0].npc_entries[0].tag.as_deref(), Some("alpha"));
        assert!(matches!(r.triggers[0].kind, DungeonTriggerKind::OnRoomEntered { target_room_index: Some(0) }));
        assert!(matches!(&r.triggers[0].action, DungeonTriggerAction::StartTimer { timer_id, delay_secs: 5 } if timer_id == "t1"));
        assert_eq!(r.triggers[0].delay_secs, 0, "BF-02: StartTimer delaySecs must not leak into wrapper delay_secs");
        assert_eq!(r.triggers[0].target_room_index, None, "BF-02: OnRoomEntered targetRoomIndex must not leak into action target_room_index");
        assert!(matches!(&r.triggers[1].kind, DungeonTriggerKind::OnKillTagged { target_tag, required_kills: Some(2), type_ids } if target_tag == "alpha" && type_ids == &[48_090]));
        assert!(matches!(r.triggers[2].kind, DungeonTriggerKind::OnKillCount { target_npc_group: Some(1), required_kills: 3 }));
        assert!(matches!(&r.triggers[3].kind, DungeonTriggerKind::OnTimer { delay_secs: 7, timer_id: Some(t), after_group_cleared: Some(1) } if t == "t1"));
        assert_eq!(r.triggers[3].delay_secs, 0, "BF-02: OnTimer delaySecs must not leak into wrapper delay_secs");
        assert!(matches!(&r.triggers[4].kind, DungeonTriggerKind::OnNpcDamaged { hull_or_shield_below_pct: 45, on_first_attack: false, .. }));
        assert!(matches!(&r.triggers[5].kind, DungeonTriggerKind::CompoundOr { clauses } if clauses.len() == 2));
        let mut cur = &r.triggers[6].kind;
        for _ in 0..4 {
            cur = match cur {
                DungeonTriggerKind::CompoundAnd { clauses } => &clauses[0],
                other => panic!("expected CompoundAnd depth cap, got {other:?}"),
            };
        }
        assert!(matches!(cur, DungeonTriggerKind::OnKill { .. }), "depth > 4 must cap safely to OnKill");
        assert!(matches!(&r.triggers[7].kind, DungeonTriggerKind::OnTimer { delay_secs: 4, timer_id: Some(t), .. } if t.is_empty()), "BF-R2-01: __-prefixed OnTimer timerID must keep inert empty sentinel");
        assert!(matches!(&r.triggers[7].action, DungeonTriggerAction::StartTimer { timer_id, delay_secs: 4 } if timer_id.is_empty()), "P2-3: __-prefixed StartTimer timerID must be stripped");
        assert_eq!(r.triggers[7].delay_secs, 2, "explicit triggerDelaySecs must still work");
        assert_eq!(r.triggers[7].target_room_index, Some(1), "explicit actionTargetRoomIndex must still work");
        assert!(matches!(&r.triggers[8].kind, DungeonTriggerKind::OnTimer { delay_secs: 1, timer_id: Some(t), .. } if t.is_empty()));
        assert_eq!(r.triggers[9].delay_secs, 0, "P2-01: extraActions StartTimer shorthand must consume wrapper delaySecs");
    });

    // BF-R2-01 runtime proof: __-prefixed OnTimer never falls back to unnamed room-entry timer nor touches internal __ keys.
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.get_mut(&inst_id).unwrap().rooms[0].armed_timers.insert("__internal_probe".into(), 0);
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = ctx.drain_notifications_for_char(char_id);
    ctx.advance_secs(5);
    sys_h.step_manual(1.0).await.unwrap();
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnDungeonTriggerMessage"), "BF-R2-01: __-prefixed OnTimer must never fire after delaySecs");
    read_dungeon_runtime_store(&ctx, |s| {
        let r = &s.instances[&inst_id].rooms[0];
        assert!(!r.triggers[7].fired && r.triggers[7].armed_at_unix.is_none());
        assert!(!r.triggers[8].fired && r.triggers[8].armed_at_unix.is_none());
        assert_eq!(r.armed_timers.get("__internal_probe").copied(), Some(0), "BF-R2-01: internal __ keys must stay untouched");
    });

    // BF-02 E2E: PyRep-parsed StartTimer(3s), OnTimer(3s), and OnRoomEntered(targetRoomIndex: 1) execute without 2x delay or misrouting.
    let exec_npc = PyRep::List(vec![PyRep::keyval([("groupID", PyRep::Integer(1)), ("npcs", PyRep::List(vec![PyRep::List(vec![PyRep::Integer(48_090), PyRep::Integer(1)])]))])]);
    let exec_r0 = PyRep::keyval([
        ("roomName", PyRep::String("Exec 0".into())),
        ("gateLocked", PyRep::Bool(true)),
        ("npcGroups", exec_npc.clone()),
        ("triggers", PyRep::List(vec![
            PyRep::keyval([("kind", PyRep::String("room_entered".into())), ("targetRoomIndex", PyRep::Integer(0)), ("action", PyRep::String("StartTimer".into())), ("timerID", PyRep::String("p_t1".into())), ("delaySecs", PyRep::Integer(3))]),
            PyRep::keyval([("kind", PyRep::String("timer".into())), ("timerID", PyRep::String("p_t1".into())), ("delaySecs", PyRep::Integer(3)), ("action", PyRep::String("TriggerMessage".into())), ("messageID", PyRep::Integer(9101))]),
            PyRep::keyval([("kind", PyRep::String("room_entered".into())), ("targetRoomIndex", PyRep::Integer(1)), ("action", PyRep::String("UnlockGate".into()))]),
        ])),
    ]);
    let exec_r1 = PyRep::keyval([("roomName", PyRep::String("Exec 1".into())), ("gateLocked", PyRep::Bool(true)), ("npcGroups", exec_npc)]);
    let t0 = ctx.db.clock().now_unix_secs();
    let exec_res = spawn_dungeon_instance(&ctx, char_id, Some(sys_id), &[PyRep::Integer(1), PyRep::List(vec![exec_r0, exec_r1])], &HashMap::new()).await.unwrap();
    let exec_iid = exec_res.dict_get("instanceID").unwrap().as_i64().unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        let r0 = &s.instances[&exec_iid].rooms[0];
        assert_eq!(r0.armed_timers.get("p_t1").copied(), Some(t0 + 3), "StartTimer must arm immediately without wrapper delay");
        assert!(r0.gate_locked);
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    ctx.advance_secs(2);
    sys_h.step_manual(1.0).await.unwrap();
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnDungeonTriggerMessage"));
    ctx.advance_secs(1); // exact +3s, NOT +6s or +9s
    sys_h.step_manual(1.0).await.unwrap();
    assert!(ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnDungeonTriggerMessage"), "OnTimer must fire at +3s without 2x delay");
    on_dungeon_room_entered(&ctx, sys_id, exec_iid, 1, char_id).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let inst = &s.instances[&exec_iid];
        assert!(!inst.rooms[0].gate_locked, "OnRoomEntered action must target trigger's own Room 0");
        assert!(inst.rooms[1].gate_locked, "OnRoomEntered targetRoomIndex must not redirect UnlockGate to Room 1");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a3_compound_and_partially_latched_read_purity_bf01() {
    let (sys_id, pilot_a, pilot_b, inst_id) = (30_000_142u32, 90_050_060i64, 90_050_061i64, 991_008i64);
    let r_pos = [47_000_000.0, 0.0, 0.0];
    let (ctx, _, ship_a) = make_t2a3_ctx(shared_sde(), Arc::new(WorldRepository::new_empty()), sys_id, pilot_a, "bf01_a", DVec3::from_array(r_pos)).await;
    let ship_b = spawn_extra_pilot(&ctx, sys_id, pilot_b, "bf01_b", DVec3::new(0.0, 0.0, 0.0)).await;
    let bal0 = ctx.db.with_snapshot(|s| s.characters[&pilot_a].balance);
    let sys_h = ctx.systems.get_or_spawn(sys_id);

    let mut r0 = t2_room(1, 9701, r_pos, vec![
        t2_trig(1, DungeonTriggerKind::CompoundAnd { clauses: vec![
            DungeonTriggerKind::OnRoomEntered { target_room_index: None },
            DungeonTriggerKind::OnTimer { delay_secs: 60, timer_id: None, after_group_cleared: None },
        ]}, DungeonTriggerAction::UnlockGate, false),
        t2_trig(2, DungeonTriggerKind::CompoundAnd { clauses: vec![
            DungeonTriggerKind::OnRoomEntered { target_room_index: None },
            DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: [r_pos[0] + 15_000.0, 0.0, 0.0], radius_m: 3_000.0 },
        ]}, DungeonTriggerAction::GrantReward { isk: 100_000, lp: 0, corp_id: 0, faction_id: 0, standing_milli: 0 }, false),
    ]);
    r0.spawned = true;
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_id, DungeonRuntimeInstance {
            instance_id: inst_id, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: pilot_a,
            base_position: r_pos, rooms: vec![r0], created_unix: 1_785_100_000, ..Default::default()
        });
    });
    on_dungeon_room_entered(&ctx, sys_id, inst_id, 0, pilot_a).await;
    let (ts_and0, ts_and1) = read_dungeon_runtime_store(&ctx, |s| {
        let at = &s.instances[&inst_id].rooms[0].armed_timers;
        (at["__and:0:0:root.0"], at["__and:0:1:root.0"])
    });
    ctx.advance_secs(10);
    for _ in 0..3 {
        ctx.advance_secs(1);
        let tx_step = ctx.db.tx_commit_count();
        tick_dungeon_runtime_system_step(&ctx, sys_id).await;
        assert_eq!(ctx.db.tx_commit_count(), tx_step, "BF-01: idle 1Hz ticks with partially latched CompoundAnd must do 0 DB writes");
    }
    let tx_app = ctx.db.tx_commit_count();
    assert_eq!(on_dungeon_approach(&ctx, pilot_b, sys_id, ship_b, None).await, 0);
    assert_eq!(ctx.db.tx_commit_count(), tx_app, "BF-01: unrelated pilot approach must do 0 DB writes");
    read_dungeon_runtime_store(&ctx, |s| {
        let at = &s.instances[&inst_id].rooms[0].armed_timers;
        assert_eq!((at["__and:0:0:root.0"], at["__and:0:1:root.0"]), (ts_and0, ts_and1), "BF-01: __and timestamps must stay immutable");
    });

    ctx.advance_secs(47); // total +60s
    sys_h.step_manual(1.0).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| assert!(!s.instances[&inst_id].rooms[0].gate_locked));
    for _ in 0..2 {
        ctx.advance_secs(1);
        let tx_step = ctx.db.tx_commit_count();
        tick_dungeon_runtime_system_step(&ctx, sys_id).await;
        assert_eq!(ctx.db.tx_commit_count(), tx_step, "BF-01: CompoundAnd([OnRoomEntered, OnApproach]) tick must stay read-pure");
    }
    let tx_app2 = ctx.db.tx_commit_count();
    assert_eq!(on_dungeon_approach(&ctx, pilot_b, sys_id, ship_b, None).await, 0);
    assert_eq!(ctx.db.tx_commit_count(), tx_app2, "BF-01: CompoundAnd([OnRoomEntered, OnApproach]) approach must stay read-pure");

    assert!(sys_h.set_ball_position(ship_a, DVec3::new(r_pos[0] + 15_000.0, 0.0, 0.0)).await);
    assert_eq!(on_dungeon_approach(&ctx, pilot_a, sys_id, ship_a, None).await, 1);
    ctx.db.with_snapshot(|s| assert_eq!(s.characters[&pilot_a].balance, bal0.checked_add(IskCents::from_isk(100_000)).unwrap()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a3_shared_named_timer_non_timer_events_and_delayed_tick_bf03() {
    let (sys_id, char_id, inst_a, inst_b) = (30_000_142u32, 90_050_070i64, 991_009i64, 991_010i64);
    let r_pos = [48_000_000.0, 0.0, 0.0];
    let (ctx, _, ship_id) = make_t2a3_ctx(shared_sde(), Arc::new(WorldRepository::new_empty()), sys_id, char_id, "bf03", DVec3::ZERO).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let t0 = ctx.db.clock().now_unix_secs();

    // (a) Approach at now >= due must neither delete shared named timer nor advance repeatable unnamed timer before TimerTick.
    let mut ra = t2_room(1, 9801, r_pos, vec![
        t2_trig(1, DungeonTriggerKind::CompoundOr { clauses: vec![
            DungeonTriggerKind::OnTimer { delay_secs: 10, timer_id: Some("shared_a".into()), after_group_cleared: None },
            DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: r_pos, radius_m: 3_000.0 },
        ]}, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 801 }, false),
        t2_trig(2, DungeonTriggerKind::OnTimer { delay_secs: 10, timer_id: Some("shared_a".into()), after_group_cleared: None }, DungeonTriggerAction::UnlockGate, false),
        t2_trig(3, DungeonTriggerKind::CompoundOr { clauses: vec![
            DungeonTriggerKind::OnTimer { delay_secs: 10, timer_id: None, after_group_cleared: None },
            DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: r_pos, radius_m: 3_000.0 },
        ]}, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 802 }, true),
    ]);
    ra.spawned = true;
    ra.entered = true;
    ra.entered_unix = Some(t0);
    ra.armed_timers.insert("shared_a".into(), t0 + 10);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_a, DungeonRuntimeInstance {
            instance_id: inst_a, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: char_id,
            base_position: r_pos, rooms: vec![ra], created_unix: t0, ..Default::default()
        });
    });
    ctx.advance_secs(10);
    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(r_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 2);
    read_dungeon_runtime_store(&ctx, |s| {
        let rm = &s.instances[&inst_a].rooms[0];
        assert!(rm.gate_locked && rm.armed_timers.contains_key("shared_a"), "BF-03: Approach must not delete shared_a");
        assert!(!rm.armed_timers.keys().any(|k| k.starts_with("__rep_timer:")), "BF-03: Approach must not bump __rep_timer");
    });
    assert!(sys_h.set_ball_position(ship_id, DVec3::ZERO).await);
    let _ = ctx.drain_notifications_for_char(char_id);
    sys_h.step_manual(1.0).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        let rm = &s.instances[&inst_a].rooms[0];
        assert!(!rm.gate_locked && !rm.armed_timers.contains_key("shared_a"), "BF-03: TimerTick fires peer OnTimer then clears shared_a");
    });
    assert_eq!(ctx.drain_notifications_for_char(char_id).iter().filter(|n| n.event == "OnDungeonTriggerMessage").count(), 1, "BF-03: TimerTick fires unnamed 10s OnTimer");

    // (b) Delayed trigger firing via tick_due_delayed_dungeon_triggers must not delete shared_b before peer OnTimer runs.
    let rb_pos = [49_000_000.0, 0.0, 0.0];
    let mut delayed_t = t2_trig(1, DungeonTriggerKind::CompoundOr { clauses: vec![
        DungeonTriggerKind::OnTimer { delay_secs: 5, timer_id: Some("shared_b".into()), after_group_cleared: None },
        DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: rb_pos, radius_m: 3_000.0 },
    ]}, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 803 }, false);
    delayed_t.delay_secs = 5;
    let mut rb = t2_room(1, 9802, rb_pos, vec![
        delayed_t,
        t2_trig(2, DungeonTriggerKind::OnTimer { delay_secs: 5, timer_id: Some("shared_b".into()), after_group_cleared: None }, DungeonTriggerAction::UnlockGate, false),
    ]);
    rb.spawned = true;
    rb.entered = true;
    rb.entered_unix = Some(t0 + 10);
    rb.armed_timers.insert("shared_b".into(), t0 + 15);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_b, DungeonRuntimeInstance {
            instance_id: inst_b, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: char_id,
            base_position: rb_pos, rooms: vec![rb], created_unix: t0 + 10, ..Default::default()
        });
    });
    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(rb_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    assert!(sys_h.set_ball_position(ship_id, DVec3::ZERO).await);
    ctx.advance_secs(5);
    let _ = ctx.drain_notifications_for_char(char_id);
    assert_eq!(tick_due_delayed_dungeon_triggers(&ctx).await, 1);
    read_dungeon_runtime_store(&ctx, |s| {
        let rm = &s.instances[&inst_b].rooms[0];
        assert!(rm.gate_locked && rm.armed_timers.contains_key("shared_b"), "BF-03: delayed tick must not delete shared_b");
    });
    sys_h.step_manual(1.0).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        let rm = &s.instances[&inst_b].rooms[0];
        assert!(!rm.gate_locked && !rm.armed_timers.contains_key("shared_b"), "BF-03: peer OnTimer still fires on TimerTick");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a3_p2_empty_system_tick_prekill_group0_and_reserved_timer_id() {
    let (sys_id, empty_sys_id, char_id, inst_id) = (30_000_142u32, 30_000_144u32, 90_050_080i64, 991_011i64);
    let r_pos = [50_000_000.0, 0.0, 0.0];
    let (ctx, _, _) = make_t2a3_ctx(shared_sde(), Arc::new(WorldRepository::new_empty()), sys_id, char_id, "p2_clean", DVec3::ZERO).await;
    let mut r0 = t2_room(1, 9901, r_pos, vec![
        t2_trig(1, DungeonTriggerKind::OnRoomEntered { target_room_index: None }, DungeonTriggerAction::StartTimer { timer_id: "__bad".into(), delay_secs: 5 }, false),
        t2_trig(2, DungeonTriggerKind::OnKillCount { target_npc_group: Some(0), required_kills: 1 }, DungeonTriggerAction::UnlockGate, false),
    ]);
    r0.npc_groups = vec![DungeonNpcGroup { group_id: 0, wave: 1, npc_specs: vec![(48_090, 1)], spawn_immediately: true, ..Default::default() }];
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_id, DungeonRuntimeInstance {
            instance_id: inst_id, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: char_id,
            base_position: r_pos, rooms: vec![r0], created_unix: 1_785_100_000, ..Default::default()
        });
    });
    let npc_id = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0).await.unwrap().npc_ids[0];
    let tx0 = ctx.db.tx_commit_count();
    tick_dungeon_runtime_system_step(&ctx, empty_sys_id).await;
    assert_eq!(ctx.db.tx_commit_count(), tx0, "empty system tick must short-circuit cleanly");

    on_dungeon_npc_killed(&ctx, sys_id, npc_id, char_id).await;
    on_dungeon_room_entered(&ctx, sys_id, inst_id, 0, char_id).await;
    read_dungeon_runtime_store(&ctx, |s| {
        let rm = &s.instances[&inst_id].rooms[0];
        assert!(!rm.gate_locked, "group_id == 0 pre-entry kill replay must match OnKillCount(Some(0))");
        assert!(!rm.armed_timers.contains_key("__bad"), "__-prefixed StartTimer must be ignored");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a3_delayed_repeatable_named_timer_cleanup_and_rearm_bf_r2_02() {
    let (sys_id, char_id, inst_id) = (30_000_142u32, 90_050_090i64, 991_012i64);
    let r_pos = [51_000_000.0, 0.0, 0.0];
    let (ctx, _, ship_id) = make_t2a3_ctx(shared_sde(), Arc::new(WorldRepository::new_empty()), sys_id, char_id, "r2_02", DVec3::ZERO).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let t0 = ctx.db.clock().now_unix_secs();

    let mut del_t = t2_trig(1, DungeonTriggerKind::OnTimer { delay_secs: 2, timer_id: Some("rep_del".into()), after_group_cleared: None }, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 9201 }, true);
    del_t.delay_secs = 4;
    let mut r0 = t2_room(1, 9911, r_pos, vec![
        del_t,
        t2_trig(2, DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: r_pos, radius_m: 3_000.0 }, DungeonTriggerAction::StartTimer { timer_id: "rep_del".into(), delay_secs: 2 }, true),
    ]);
    r0.spawned = true;
    r0.entered = true;
    r0.entered_unix = Some(t0);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_id, DungeonRuntimeInstance {
            instance_id: inst_id, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: char_id,
            base_position: r_pos, rooms: vec![r0], created_unix: t0, ..Default::default()
        });
    });

    for cycle in 1..=2 {
        assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(r_pos)).await);
        assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
        assert!(sys_h.set_ball_position(ship_id, DVec3::ZERO).await);
        assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);
        ctx.advance_secs(2);
        sys_h.step_manual(1.0).await.unwrap();
        read_dungeon_runtime_store(&ctx, |s| {
            let rm = &s.instances[&inst_id].rooms[0];
            assert!(rm.triggers[0].armed_at_unix.is_some() && !rm.armed_timers.contains_key("rep_del"), "cycle {cycle}: rep_del cleaned up on arming");
        });
        let _ = ctx.drain_notifications_for_char(char_id);
        ctx.advance_secs(4);
        assert_eq!(tick_due_delayed_dungeon_triggers(&ctx).await, 1, "cycle {cycle}: delayed repeatable OnTimer fires");
        assert_eq!(ctx.drain_notifications_for_char(char_id).iter().filter(|n| n.event == "OnDungeonTriggerMessage").count(), 1);
        read_dungeon_runtime_store(&ctx, |s| {
            assert!(!s.instances[&inst_id].rooms[0].armed_timers.contains_key("__latch:0:0:root:timer:rep_del"));
        });
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a3_npc_damaged_shield_only_depletion_bf_r2_03() {
    let (sys_id, char_id, inst_id) = (30_000_142u32, 90_050_100i64, 991_013i64);
    let r_pos = [52_000_000.0, 0.0, 0.0];
    let (ctx, _, _) = make_t2a3_ctx(shared_sde(), Arc::new(WorldRepository::new_empty()), sys_id, char_id, "r2_03", DVec3::from_array(r_pos)).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let t0 = ctx.db.clock().now_unix_secs();

    let mut r0 = t2_room(1, 9921, r_pos, vec![
        t2_trig(1, DungeonTriggerKind::OnNpcDamaged { target_npc_group: Some(1), target_tag: None, target_ball_id: None, hull_or_shield_below_pct: 50, on_first_attack: false }, DungeonTriggerAction::UnlockGate, false),
    ]);
    r0.entered = true;
    r0.entered_unix = Some(t0);
    r0.npc_groups = vec![DungeonNpcGroup { group_id: 1, wave: 1, npc_specs: vec![(48_090, 1)], spawn_immediately: true, ..Default::default() }];
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_id, DungeonRuntimeInstance {
            instance_id: inst_id, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: char_id,
            base_position: r_pos, rooms: vec![r0], created_unix: t0, ..Default::default()
        });
    });
    let npc_id = materialize_dungeon_room_entities(&ctx, sys_id, inst_id, 0).await.unwrap().npc_ids[0];
    for _ in 0..32 {
        let (shield_pct, step) = {
            let view = sys_h.view();
            let cs = view.combat_states.get(&npc_id).expect("npc combat state");
            assert!(cs.max_shield_hp > 0.0);
            (((cs.shield_hp / cs.max_shield_hp) * 100.0).round() as u8, (cs.max_shield_hp * 0.08).max(1.0))
        };
        if shield_pct <= 35 { break; }
        let _ = sys_h.apply_damage_with_attacker(npc_id, DamageVector { em: step, thermal: step, kinetic: step, explosive: step }, Some(char_id)).await.unwrap();
    }
    {
        let view = sys_h.view();
        let cs = view.combat_states.get(&npc_id).expect("npc combat state");
        let total_max = (cs.max_shield_hp + cs.max_armor_hp + cs.max_structure_hp).max(1.0);
        let total_pct = (((cs.shield_hp + cs.armor_hp + cs.structure_hp) / total_max) * 100.0).round() as u8;
        assert!(cs.shield_hp / cs.max_shield_hp <= 0.5 && cs.shield_hp > 0.5 && cs.armor_hp == cs.max_armor_hp && cs.structure_hp == cs.max_structure_hp && total_pct > 50);
    }
    sys_h.step_manual(1.0).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        let rm = &s.instances[&inst_id].rooms[0];
        assert!(rm.triggers[0].fired && !rm.gate_locked, "BF-R2-03: shield-only depletion <= 50% must fire OnNpcDamaged");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a3_compound_and_repeatable_named_timer_and_shared_peer_bf_r3_01() {
    let (sys_id, char_id, inst_id) = (30_000_142u32, 90_050_110i64, 991_014i64);
    let (r_pos, p2_pos, arm_pos) = ([53_000_000.0, 0.0, 0.0], [53_010_000.0, 0.0, 0.0], [53_020_000.0, 0.0, 0.0]);
    let (ctx, _, ship_id) = make_t2a3_ctx(shared_sde(), Arc::new(WorldRepository::new_empty()), sys_id, char_id, "r3_01", DVec3::ZERO).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let t0 = ctx.db.clock().now_unix_secs();

    let mut r0 = t2_room(1, 9931, r_pos, vec![
        t2_trig(1, DungeonTriggerKind::CompoundAnd { clauses: vec![
            DungeonTriggerKind::OnTimer { delay_secs: 0, timer_id: Some("t1".into()), after_group_cleared: None },
            DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: r_pos, radius_m: 3_000.0 },
        ]}, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 9301 }, true),
        t2_trig(2, DungeonTriggerKind::CompoundAnd { clauses: vec![
            DungeonTriggerKind::OnTimer { delay_secs: 0, timer_id: Some("t1".into()), after_group_cleared: None },
            DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: p2_pos, radius_m: 3_000.0 },
        ]}, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 9302 }, true),
        t2_trig(3, DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: arm_pos, radius_m: 3_000.0 }, DungeonTriggerAction::StartTimer { timer_id: "t1".into(), delay_secs: 3 }, true),
    ]);
    r0.spawned = true;
    r0.entered = true;
    r0.entered_unix = Some(t0);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_id, DungeonRuntimeInstance {
            instance_id: inst_id, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: char_id,
            base_position: r_pos, rooms: vec![r0], created_unix: t0, ..Default::default()
        });
    });

    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(arm_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    assert!(sys_h.set_ball_position(ship_id, DVec3::ZERO).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);

    ctx.advance_secs(3);
    sys_h.step_manual(1.0).await.unwrap();
    read_dungeon_runtime_store(&ctx, |s| {
        let at = &s.instances[&inst_id].rooms[0].armed_timers;
        assert!(!at.contains_key("t1") && at.contains_key("__and:0:0:root.0") && at.contains_key("__and:0:1:root.0"));
    });
    ctx.advance_secs(1);
    let tx_wait = ctx.db.tx_commit_count();
    tick_dungeon_runtime_system_step(&ctx, sys_id).await;
    assert_eq!(ctx.db.tx_commit_count(), tx_wait, "BF-R3-01: idle TimerTick with latched OnTimer must stay read-pure");

    let _ = ctx.drain_notifications_for_char(char_id);
    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(r_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(p2_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    assert_eq!(ctx.drain_notifications_for_char(char_id).iter().filter(|n| n.event == "OnDungeonTriggerMessage").count(), 2);

    assert!(sys_h.set_ball_position(ship_id, DVec3::ZERO).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);
    for _ in 0..2 {
        ctx.advance_secs(1);
        let tx_idle = ctx.db.tx_commit_count();
        tick_dungeon_runtime_system_step(&ctx, sys_id).await;
        assert_eq!(ctx.db.tx_commit_count(), tx_idle, "BF-R3-01: completed CompoundAnd stays read-pure");
    }
    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(r_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);

    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(arm_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    assert!(sys_h.set_ball_position(ship_id, DVec3::ZERO).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);
    ctx.advance_secs(3);
    sys_h.step_manual(1.0).await.unwrap();
    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(p2_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a3_repeatable_compound_and_two_timers_preserves_rearmed_bf_r4_01() {
    let (sys_id, char_id, inst_id) = (30_000_142u32, 90_050_120i64, 991_015i64);
    let (r_pos, p1_pos, p2_pos) = ([54_000_000.0, 0.0, 0.0], [54_010_000.0, 0.0, 0.0], [54_020_000.0, 0.0, 0.0]);
    let (ctx, _, ship_id) = make_t2a3_ctx(shared_sde(), Arc::new(WorldRepository::new_empty()), sys_id, char_id, "r4_01a", DVec3::ZERO).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let t0 = ctx.db.clock().now_unix_secs();

    let mut r0 = t2_room(1, 9941, r_pos, vec![
        t2_trig(1, DungeonTriggerKind::CompoundAnd { clauses: vec![
            DungeonTriggerKind::OnTimer { delay_secs: 0, timer_id: Some("t1".into()), after_group_cleared: None },
            DungeonTriggerKind::OnTimer { delay_secs: 0, timer_id: Some("t2".into()), after_group_cleared: None },
        ]}, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 9401 }, true),
        t2_trig(2, DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: p1_pos, radius_m: 3_000.0 }, DungeonTriggerAction::StartTimer { timer_id: "t1".into(), delay_secs: 2 }, true),
        t2_trig(3, DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: p2_pos, radius_m: 3_000.0 }, DungeonTriggerAction::StartTimer { timer_id: "t2".into(), delay_secs: 5 }, true),
    ]);
    r0.spawned = true;
    r0.entered = true;
    r0.entered_unix = Some(t0);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_id, DungeonRuntimeInstance {
            instance_id: inst_id, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: char_id,
            base_position: r_pos, rooms: vec![r0], created_unix: t0, ..Default::default()
        });
    });

    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(p1_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(p2_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    assert!(sys_h.set_ball_position(ship_id, DVec3::ZERO).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);

    ctx.advance_secs(2);
    sys_h.step_manual(1.0).await.unwrap();
    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(p1_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    assert!(sys_h.set_ball_position(ship_id, DVec3::ZERO).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);

    let _ = ctx.drain_notifications_for_char(char_id);
    ctx.advance_secs(3);
    sys_h.step_manual(1.0).await.unwrap();
    assert_eq!(ctx.drain_notifications_for_char(char_id).iter().filter(|n| n.event == "OnDungeonTriggerMessage").count(), 1);
    read_dungeon_runtime_store(&ctx, |s| {
        let at = &s.instances[&inst_id].rooms[0].armed_timers;
        assert!(at.contains_key("t1") && !at.contains_key("t2"), "re-armed t1 must stay intact when t2 finishes cycle 1");
    });

    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(p2_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    assert!(sys_h.set_ball_position(ship_id, DVec3::ZERO).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);
    ctx.advance_secs(1);
    sys_h.step_manual(1.0).await.unwrap();
    ctx.advance_secs(4);
    sys_h.step_manual(1.0).await.unwrap();
    assert_eq!(ctx.drain_notifications_for_char(char_id).iter().filter(|n| n.event == "OnDungeonTriggerMessage").count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a3_delayed_timer_step_manual_preserves_rearmed_cycle2_bf_r4_01() {
    let (sys_id, char_id, inst_id) = (30_000_142u32, 90_050_130i64, 991_016i64);
    let (r_pos, p1_pos, p2_pos) = ([55_000_000.0, 0.0, 0.0], [55_010_000.0, 0.0, 0.0], [55_020_000.0, 0.0, 0.0]);
    let (ctx, _, ship_id) = make_t2a3_ctx(shared_sde(), Arc::new(WorldRepository::new_empty()), sys_id, char_id, "r4_01b", DVec3::ZERO).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let t0 = ctx.db.clock().now_unix_secs();

    let mut del_rep = t2_trig(1, DungeonTriggerKind::OnTimer { delay_secs: 0, timer_id: Some("rep_del".into()), after_group_cleared: None }, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 9402 }, true);
    del_rep.delay_secs = 4;
    let mut del_once = t2_trig(3, DungeonTriggerKind::CompoundOr { clauses: vec![
        DungeonTriggerKind::OnTimer { delay_secs: 0, timer_id: Some("peer_t".into()), after_group_cleared: None },
        DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: p2_pos, radius_m: 3_000.0 },
    ]}, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 9403 }, false);
    del_once.delay_secs = 3;
    let mut r0 = t2_room(1, 9942, r_pos, vec![
        del_rep,
        t2_trig(2, DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: p1_pos, radius_m: 3_000.0 }, DungeonTriggerAction::StartTimer { timer_id: "rep_del".into(), delay_secs: 2 }, true),
        del_once,
        t2_trig(4, DungeonTriggerKind::OnTimer { delay_secs: 0, timer_id: Some("peer_t".into()), after_group_cleared: None }, DungeonTriggerAction::UnlockGate, false),
        t2_trig(5, DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: p2_pos, radius_m: 3_000.0 }, DungeonTriggerAction::StartTimer { timer_id: "peer_t".into(), delay_secs: 2 }, false),
    ]);
    r0.spawned = true;
    r0.entered = true;
    r0.entered_unix = Some(t0);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_id, DungeonRuntimeInstance {
            instance_id: inst_id, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: char_id,
            base_position: r_pos, rooms: vec![r0], created_unix: t0, ..Default::default()
        });
    });

    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(p1_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    assert!(sys_h.set_ball_position(ship_id, DVec3::ZERO).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);
    ctx.advance_secs(2);
    sys_h.step_manual(1.0).await.unwrap();

    ctx.advance_secs(1);
    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(p1_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(p2_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 2);
    assert!(sys_h.set_ball_position(ship_id, DVec3::ZERO).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);

    let _ = ctx.drain_notifications_for_char(char_id);
    ctx.advance_secs(3);
    sys_h.step_manual(1.0).await.unwrap();
    assert_eq!(ctx.drain_notifications_for_char(char_id).iter().filter(|n| n.event == "OnDungeonTriggerMessage").count(), 2);
    read_dungeon_runtime_store(&ctx, |s| {
        let rm = &s.instances[&inst_id].rooms[0];
        assert!(!rm.gate_locked && rm.triggers[2].fired && rm.armed_timers.contains_key("rep_del"));
    });

    ctx.advance_secs(1);
    sys_h.step_manual(1.0).await.unwrap();
    ctx.advance_secs(4);
    sys_h.step_manual(1.0).await.unwrap();
    assert_eq!(ctx.drain_notifications_for_char(char_id).iter().filter(|n| n.event == "OnDungeonTriggerMessage").count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a3_delayed_compound_or_unnamed_timer_not_bumped_bf_r4_01() {
    let (sys_id, char_id, inst_id) = (30_000_142u32, 90_050_140i64, 991_017i64);
    let r_pos = [56_000_000.0, 0.0, 0.0];
    let (ctx, _, ship_id) = make_t2a3_ctx(shared_sde(), Arc::new(WorldRepository::new_empty()), sys_id, char_id, "r4_01c", DVec3::ZERO).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let t0 = ctx.db.clock().now_unix_secs();

    let mut del_or = t2_trig(1, DungeonTriggerKind::CompoundOr { clauses: vec![
        DungeonTriggerKind::OnTimer { delay_secs: 10, timer_id: None, after_group_cleared: None },
        DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: r_pos, radius_m: 3_000.0 },
    ]}, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 9404 }, true);
    del_or.delay_secs = 5;
    let mut r0 = t2_room(1, 9943, r_pos, vec![del_or]);
    r0.spawned = true;
    r0.entered = true;
    r0.entered_unix = Some(t0);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_id, DungeonRuntimeInstance {
            instance_id: inst_id, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: char_id,
            base_position: r_pos, rooms: vec![r0], created_unix: t0, ..Default::default()
        });
    });

    ctx.advance_secs(6);
    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(r_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    assert!(sys_h.set_ball_position(ship_id, DVec3::ZERO).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);

    let _ = ctx.drain_notifications_for_char(char_id);
    ctx.advance_secs(5);
    sys_h.step_manual(1.0).await.unwrap();
    assert_eq!(ctx.drain_notifications_for_char(char_id).iter().filter(|n| n.event == "OnDungeonTriggerMessage").count(), 1);
    read_dungeon_runtime_store(&ctx, |s| {
        assert!(!s.instances[&inst_id].rooms[0].armed_timers.contains_key("__rep_timer:0:0:root.0"));
    });

    ctx.advance_secs(1);
    sys_h.step_manual(1.0).await.unwrap();
    ctx.advance_secs(5);
    sys_h.step_manual(1.0).await.unwrap();
    assert_eq!(ctx.drain_notifications_for_char(char_id).iter().filter(|n| n.event == "OnDungeonTriggerMessage").count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t2a3_nested_compound_or_in_compound_and_read_pure_bf_r4_02() {
    let (sys_id, char_id, inst_id) = (30_000_142u32, 90_050_150i64, 991_018i64);
    let (r_pos, a_pos, b_pos, arm_pos) = ([57_000_000.0, 0.0, 0.0], [57_010_000.0, 0.0, 0.0], [57_020_000.0, 0.0, 0.0], [57_030_000.0, 0.0, 0.0]);
    let (ctx, _, ship_id) = make_t2a3_ctx(shared_sde(), Arc::new(WorldRepository::new_empty()), sys_id, char_id, "r4_02", DVec3::ZERO).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let t0 = ctx.db.clock().now_unix_secs();

    let mut r0 = t2_room(1, 9944, r_pos, vec![
        t2_trig(1, DungeonTriggerKind::CompoundAnd { clauses: vec![
            DungeonTriggerKind::CompoundOr { clauses: vec![
                DungeonTriggerKind::OnTimer { delay_secs: 0, timer_id: Some("t1".into()), after_group_cleared: None },
                DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: a_pos, radius_m: 3_000.0 },
            ]},
            DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: b_pos, radius_m: 3_000.0 },
        ]}, DungeonTriggerAction::TriggerMessage { message_type: 1, message_id: 9405 }, true),
        t2_trig(2, DungeonTriggerKind::OnApproach { target_ball_id: None, target_object_index: None, anchor_pos: arm_pos, radius_m: 3_000.0 }, DungeonTriggerAction::StartTimer { timer_id: "t1".into(), delay_secs: 3 }, true),
    ]);
    r0.spawned = true;
    r0.entered = true;
    r0.entered_unix = Some(t0);
    with_dungeon_runtime_store(&ctx, |s| {
        s.instances.insert(inst_id, DungeonRuntimeInstance {
            instance_id: inst_id, dungeon_id: 1, solar_system_id: sys_id, owner_char_id: char_id,
            base_position: r_pos, rooms: vec![r0], created_unix: t0, ..Default::default()
        });
    });

    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(arm_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    assert!(sys_h.set_ball_position(ship_id, DVec3::ZERO).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);

    ctx.advance_secs(3);
    sys_h.step_manual(1.0).await.unwrap();
    for _ in 0..2 {
        ctx.advance_secs(1);
        let tx_idle = ctx.db.tx_commit_count();
        tick_dungeon_runtime_system_step(&ctx, sys_id).await;
        assert_eq!(ctx.db.tx_commit_count(), tx_idle, "BF-R4-02: idle tick after nested CompoundOr latched must stay read-pure");
    }

    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(arm_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    assert!(sys_h.set_ball_position(ship_id, DVec3::ZERO).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);
    ctx.advance_secs(3);
    let tx_rearm = ctx.db.tx_commit_count();
    sys_h.step_manual(1.0).await.unwrap();
    assert_eq!(ctx.db.tx_commit_count(), tx_rearm, "BF-R4-02: re-armed t1 must not trigger DB writes while root.0 is latched");
    read_dungeon_runtime_store(&ctx, |s| {
        assert!(s.instances[&inst_id].rooms[0].armed_timers.contains_key("t1"), "BF-R4-02: re-armed t1 preserved");
    });

    let _ = ctx.drain_notifications_for_char(char_id);
    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(b_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    assert_eq!(ctx.drain_notifications_for_char(char_id).iter().filter(|n| n.event == "OnDungeonTriggerMessage").count(), 1);
    assert!(sys_h.set_ball_position(ship_id, DVec3::ZERO).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 0);
    sys_h.step_manual(1.0).await.unwrap();
    assert!(sys_h.set_ball_position(ship_id, DVec3::from_array(b_pos)).await);
    assert_eq!(on_dungeon_approach(&ctx, char_id, sys_id, ship_id, None).await, 1);
    assert_eq!(ctx.drain_notifications_for_char(char_id).iter().filter(|n| n.event == "OnDungeonTriggerMessage").count(), 1);
}
