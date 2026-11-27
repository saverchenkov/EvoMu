//! Mission pocket dungeon engine bridging `MissionInstance` lifecycle (`#1496` Track 6) with
//! canonical `DungeonRuntimeInstance` orchestration (`PVE-008`, `PVE-019`, `PVE-021`, `PVE-151`, `R11`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::with_mut_combat_runtime;
use crate::services::pve_dungeon_content::build_wiki_deadspace_rooms;
use crate::services::pve_dungeon_runtime::{
    apply_finish_dungeon_instance_snap, instance_is_finished, materialize_dungeon_room_entities,
    materialize_dungeon_room_entities_sync, on_dungeon_room_entered, on_dungeon_room_entered_sync,
    prune_stale_despawned_instances_in_snap, reconcile_and_rematerialize_dungeon_instance,
    reconcile_and_rematerialize_dungeon_instance_sync, wake_dungeon_system_handle,
    DungeonInstanceId, DungeonNpcGroup, DungeonNpcSpawnEntry, DungeonObjective,
    DungeonRuntimeInstance, DungeonRuntimeRoom, DungeonTemplateId, DungeonTrigger,
    DungeonTriggerAction, DungeonTriggerKind,
};
use eve_db::{
    MissionInstance, MissionInstanceId, MissionState, WorldRepository, WorldStateSnapshot,
};
use glam::DVec3;
use std::collections::HashSet;

#[derive(Clone, Copy)]
enum MissionPocketAction {
    Materialize {
        mission_iid: MissionInstanceId,
        inst_id: i64,
        is_wiki_dungeon: bool,
    },
    Reconcile {
        mission_iid: MissionInstanceId,
        inst_id: i64,
    },
}

impl MissionPocketAction {
    fn mission_iid(self) -> MissionInstanceId {
        match self {
            Self::Materialize { mission_iid, .. } | Self::Reconcile { mission_iid, .. } => {
                mission_iid
            }
        }
    }

    fn inst_id(self) -> i64 {
        match self {
            Self::Materialize { inst_id, .. } | Self::Reconcile { inst_id, .. } => inst_id,
        }
    }
}

fn mission_npc_bounty_override(ctx: &ServerContext, npc_type_id: u32) -> i64 {
    ctx.sde
        .types
        .get(&npc_type_id)
        .and_then(|t| {
            t.default_attributes
                .get(&eve_sde::attr_ids::ENTITY_KILL_BOUNTY)
                .copied()
        })
        .unwrap_or(25_000.0)
        .max(10_000.0)
        .round() as i64
}

fn select_wave_types_for_mission(
    ctx: &ServerContext,
    mission: &MissionInstance,
    remaining_count: usize,
    default_npc_tid: u32,
) -> Vec<u32> {
    let enc = mission.encounter();
    let initial_types = enc.map(|e| e.initial_wave_types.as_slice()).unwrap_or(&[]);
    if !initial_types.is_empty() {
        return if initial_types.len() >= remaining_count {
            initial_types[initial_types.len() - remaining_count..].to_vec()
        } else {
            (0..remaining_count)
                .map(|idx| initial_types[idx % initial_types.len()])
                .collect()
        };
    }

    let sde_pool: Vec<u32> = ctx
        .sde
        .missions
        .get(&mission.template_id)
        .and_then(|tmpl| {
            ctx.sde.agents.get(&mission.agent_id).map(|agent| {
                super::agents_missions_helpers::resolve_security_mission_encounter(
                    ctx,
                    tmpl,
                    agent,
                    mission.solar_system_id,
                )
                .npc_type_ids
            })
        })
        .unwrap_or_default();

    (0..remaining_count)
        .map(|idx| {
            if !sde_pool.is_empty() {
                sde_pool[idx % sde_pool.len()]
            } else {
                default_npc_tid
            }
        })
        .collect()
}

fn build_wiki_for_mission(
    ctx: &ServerContext,
    sys_id: u32,
    sde_dungeon_id: u32,
    mission_title: &str,
    pocket_pos: [f64; 3],
) -> Option<crate::services::pve_dungeon_content::WikiDungeonBuild> {
    let dname = ctx
        .sde
        .dungeons
        .get(&sde_dungeon_id)
        .map(|d| d.name.as_str())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or(mission_title);
    build_wiki_deadspace_rooms(
        ctx,
        sys_id,
        DungeonTemplateId(sde_dungeon_id),
        dname,
        pocket_pos,
        false,
    )
    .filter(|w| !w.rooms.is_empty())
}

pub(super) fn count_wiki_dungeon_initial_npcs(
    ctx: &ServerContext,
    sys_id: u32,
    sde_dungeon_id: u32,
    mission_title: &str,
) -> Option<u32> {
    let wiki = build_wiki_for_mission(ctx, sys_id, sde_dungeon_id, mission_title, [0.0; 3])?;
    let sum = wiki
        .rooms
        .iter()
        .flat_map(|r| r.npc_groups.iter())
        .map(|g| u32::try_from(g.effective_entries().iter().map(|e| e.count.max(1)).sum::<usize>()).unwrap_or(u32::MAX))
        .fold(0u32, u32::saturating_add);
    Some(sum.max(1))
}

fn build_mission_dungeon_instance(
    ctx: &ServerContext,
    mission: &MissionInstance,
    remaining_count: usize,
    pocket_origin: DVec3,
    now_unix: u64,
    default_npc_tid: u32,
) -> (DungeonRuntimeInstance, bool) {
    let (char_id, sys_id, mission_id) = (mission.character_id, mission.solar_system_id, mission.template_id);
    let pocket_pos = pocket_origin.to_array();
    let tmpl_opt = ctx.sde.missions.get(&mission_id);
    let mission_title = tmpl_opt
        .map(|t| t.title.clone())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| format!("Mission {mission_id}"));
    let sde_dungeon_id_opt = tmpl_opt.and_then(|t| t.dungeon_id).filter(|&d| d > 0);
    let dungeon_id = sde_dungeon_id_opt.unwrap_or(mission_id);
    let editor_disallow = crate::services::pve_dungeon_editor::is_dungeon_or_room_mwd_disallowed(
        ctx,
        i64::from(dungeon_id),
        None,
    );
    let sde_disallow = sde_dungeon_id_opt.is_some_and(|did| {
        ctx.sde
            .dungeons
            .get(&did)
            .is_some_and(|d| matches!(d.archetype_id, Some(3 | 4 | 6 | 8 | 24 | 51)))
    });
    let disallow_mwd = editor_disallow || sde_disallow;

    let enc = mission.encounter();
    let has_explicit_types = enc.is_some_and(|e| !e.initial_wave_types.is_empty());
    let matches_full_initial = enc.is_some_and(|e| e.initial_wave_count.is_none_or(|init| remaining_count >= init as usize));

    let wiki_opt = if !has_explicit_types && matches_full_initial {
        sde_dungeon_id_opt.and_then(|did| build_wiki_for_mission(ctx, sys_id, did, &mission_title, pocket_pos))
    } else {
        None
    };

    let (eff_dungeon_id, current_room_index, rooms, objectives, active_objective_id, is_wiki_dungeon) =
        if let Some(wiki) = wiki_opt {
            let active_obj = wiki.objectives.first().map(|o| o.objective_id).unwrap_or(1);
            let first_room = wiki.rooms.first().map(|r| r.room_index).unwrap_or(1);
            (dungeon_id, first_room, wiki.rooms, wiki.objectives, active_obj, true)
        } else {
            let wave_types = select_wave_types_for_mission(ctx, mission, remaining_count, default_npc_tid);
            let total_wave = wave_types.len().max(1);
            let npc_entries: Vec<DungeonNpcSpawnEntry> = wave_types
                .iter()
                .enumerate()
                .map(|(idx, &npc_tid)| {
                    let angle = (idx as f64) * std::f64::consts::TAU / (total_wave as f64);
                    let offset_m = [(angle.cos() * 4_500.0).round() as i64, 0, (angle.sin() * 4_500.0).round() as i64];
                    DungeonNpcSpawnEntry {
                        type_id: npc_tid,
                        count: 1,
                        relative_offsets_m: vec![offset_m],
                        bounty_isk_override: Some(mission_npc_bounty_override(ctx, npc_tid)),
                        destructible: Some(true),
                        ..Default::default()
                    }
                })
                .collect();
            let room = DungeonRuntimeRoom {
                room_index: 0,
                room_id: i64::from(mission_id).saturating_mul(100).saturating_add(1),
                room_name: format!("{mission_title} - Encounter"),
                position: pocket_pos,
                entered: true,
                entered_unix: Some(now_unix),
                last_actor_char_id: char_id,
                npc_groups: vec![DungeonNpcGroup { group_id: 1, wave: 1, npc_entries, spawn_immediately: true, ..Default::default() }],
                triggers: vec![DungeonTrigger {
                    trigger_id: 1,
                    kind: DungeonTriggerKind::OnKill { target_npc_group: Some(1), target_ball_id: None },
                    action: DungeonTriggerAction::CompleteDungeon,
                    ..Default::default()
                }],
                ..Default::default()
            };
            let obj = DungeonObjective {
                objective_id: 1,
                key: format!("mission:{mission_id}:encounter"),
                title: "Clear Mission Hostiles".to_string(),
                kind: "ClearRoom".to_string(),
                room_index: Some(0),
                target_count: wave_types.len() as u32,
                ..Default::default()
            };
            (dungeon_id, 0, vec![room], vec![obj], 1, false)
        };

    let inst = DungeonRuntimeInstance {
        instance_id: 0,
        dungeon_id: eff_dungeon_id,
        dungeon_name: mission_title,
        category: "Mission".to_string(),
        archetype: "Mission".to_string(),
        solar_system_id: sys_id,
        owner_char_id: char_id,
        last_actor_char_id: char_id,
        base_position: pocket_pos,
        disallow_mwd,
        current_room_index,
        rooms,
        objectives,
        active_objective_id,
        created_unix: now_unix,
        ..Default::default()
    };
    (inst, is_wiki_dungeon)
}

enum InstLinkState { Active, Finished, MissingOrStale }

fn inspect_mission_dungeon_link(snap: &WorldStateSnapshot, inst_id: i64, sys_id: u32) -> InstLinkState {
    let Some(inst) = snap.dungeon_runtime.instances.get(&inst_id) else {
        return InstLinkState::MissingOrStale;
    };
    if !inst.despawned && !instance_is_finished(inst) && inst.solar_system_id == sys_id {
        InstLinkState::Active
    } else if instance_is_finished(inst) {
        InstLinkState::Finished
    } else {
        InstLinkState::MissingOrStale
    }
}

fn pre_live_npcs_for_inst(
    snap: &WorldStateSnapshot,
    inst_id: i64,
    view: &crate::system_actor::SystemView,
) -> HashSet<i64> {
    snap.dungeon_runtime
        .instances
        .get(&inst_id)
        .map(|inst| dungeon_mission_encounter_view(inst).1.into_iter().filter(|&bid| view.contains_ball(bid)).collect())
        .unwrap_or_default()
}

enum PocketPlan<T> {
    ClearAwaiting(MissionInstanceId),
    SyncFinished(i64),
    Reconcile { mission_iid: MissionInstanceId, inst_id: i64 },
    Create(T),
}

struct PendingMissionDungeon {
    mission_iid: MissionInstanceId,
    clear_stale_link: bool,
    mission: MissionInstance,
    remaining_count: usize,
}

struct BuiltMissionDungeon {
    mission_iid: MissionInstanceId,
    clear_stale_link: bool,
    inst: DungeonRuntimeInstance,
    is_wiki_dungeon: bool,
}

fn prepare_mission_pocket_actions(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    char_id: i64,
    sys_id: u32,
    target_mission_id: Option<u32>,
    pocket_origin: DVec3,
) -> Vec<(MissionPocketAction, HashSet<i64>)> {
    let now_unix = ctx.now_unix();
    let default_npc_tid = super::agents_missions_helpers::resolve_mission_npc_type(ctx).max(10_019);
    let view = sys_handle.view();

    let pending_builds: Vec<PocketPlan<PendingMissionDungeon>> = ctx.db.with_snapshot(|snap| {
        let mut out = Vec::new();
        for m_snapshot in snap.missions.iter_by_character(char_id) {
            let Some(enc) = m_snapshot.encounter() else { continue };
            if m_snapshot.state != MissionState::Accepted || m_snapshot.solar_system_id != sys_id {
                continue;
            }
            let matches_target = match target_mission_id {
                Some(mid) => {
                    m_snapshot.template_id == mid
                        && super::agents_missions_warp::resolve_mission_pocket_origin_in_snap(
                            ctx, snap, None, m_snapshot, sys_id, now_unix, false,
                        )
                        .is_ok()
                }
                None => {
                    let Ok(known_origin) =
                        super::agents_missions_warp::resolve_mission_pocket_origin_in_snap(
                            ctx, snap, None, m_snapshot, sys_id, now_unix,
                            m_snapshot.site_origin_m.is_some() || m_snapshot.bookmark_id.is_some(),
                        )
                    else { continue };
                    known_origin.is_none_or(|pos| pos.is_finite() && pocket_origin.distance(pos) <= 200_000.0)
                }
            };
            if !matches_target {
                continue;
            }
            if let Some(inst_id) = m_snapshot.dungeon_instance_id {
                match inspect_mission_dungeon_link(snap, inst_id, sys_id) {
                    InstLinkState::Active => {
                        out.push(PocketPlan::Reconcile { mission_iid: m_snapshot.instance_id, inst_id });
                        continue;
                    }
                    InstLinkState::Finished => {
                        let needs_sync = snap.dungeon_runtime.instances.get(&inst_id).is_some_and(|inst| {
                            if inst.failed {
                                !enc.spawned_npc_ball_ids.is_empty()
                                    || enc.awaiting_downtime_respawn
                                    || enc.npcs_remaining == 0
                            } else {
                                enc.npcs_remaining > 0 || enc.awaiting_downtime_respawn
                            }
                        });
                        if needs_sync {
                            out.push(PocketPlan::SyncFinished(inst_id));
                        }
                        continue;
                    }
                    InstLinkState::MissingOrStale => {}
                }
            }
            if enc.npcs_remaining == 0 {
                if enc.awaiting_downtime_respawn {
                    out.push(PocketPlan::ClearAwaiting(m_snapshot.instance_id));
                }
                continue;
            }
            if !enc.awaiting_downtime_respawn
                && !enc.spawned_npc_ball_ids.is_empty()
                && enc.spawned_npc_ball_ids.iter().any(|&bid| view.contains_ball(bid))
            {
                continue;
            }
            out.push(PocketPlan::Create(PendingMissionDungeon {
                mission_iid: m_snapshot.instance_id,
                clear_stale_link: m_snapshot.dungeon_instance_id.is_some(),
                mission: m_snapshot.clone(),
                remaining_count: enc.npcs_remaining as usize,
            }));
        }
        out
    });

    if pending_builds.is_empty() {
        return Vec::new();
    }

    // Build Wiki/SDE dungeon layouts OUTSIDE any DB snapshot lock (`pve_dungeon_editor` acquires rank 40).
    let built: Vec<PocketPlan<BuiltMissionDungeon>> = pending_builds
        .into_iter()
        .map(|cand| match cand {
            PocketPlan::ClearAwaiting(iid) => PocketPlan::ClearAwaiting(iid),
            PocketPlan::SyncFinished(inst_id) => PocketPlan::SyncFinished(inst_id),
            PocketPlan::Reconcile { mission_iid, inst_id } => PocketPlan::Reconcile { mission_iid, inst_id },
            PocketPlan::Create(p) => {
                let (inst, is_wiki_dungeon) = build_mission_dungeon_instance(
                    ctx, &p.mission, p.remaining_count, pocket_origin, now_unix, default_npc_tid,
                );
                PocketPlan::Create(BuiltMissionDungeon {
                    mission_iid: p.mission_iid,
                    clear_stale_link: p.clear_stale_link,
                    inst,
                    is_wiki_dungeon,
                })
            }
        })
        .collect();

    let mut actions = Vec::new();
    ctx.db.with_mut_snapshot(|snap| {
        prune_stale_despawned_instances_in_snap(snap, now_unix);

        for plan in built {
            match plan {
                PocketPlan::ClearAwaiting(iid) => {
                    let _ = snap.missions.update(iid, |m| {
                        if let Some(e) = m.encounter_mut() { e.awaiting_downtime_respawn = false; }
                    });
                }
                PocketPlan::SyncFinished(inst_id) => sync_mission_from_dungeon_in_snap(snap, inst_id),
                PocketPlan::Reconcile { mission_iid, inst_id } => {
                    if matches!(inspect_mission_dungeon_link(snap, inst_id, sys_id), InstLinkState::Active) {
                        let pre = pre_live_npcs_for_inst(snap, inst_id, &view);
                        actions.push((MissionPocketAction::Reconcile { mission_iid, inst_id }, pre));
                    }
                }
                PocketPlan::Create(BuiltMissionDungeon { mission_iid, clear_stale_link, mut inst, is_wiki_dungeon }) => {
                    let Some(cur) = snap.missions.get(mission_iid) else { continue };
                    if cur.state != MissionState::Accepted || cur.solar_system_id != sys_id || cur.npcs_remaining() == 0 {
                        continue;
                    }
                    if let Some(cur_inst_id) = cur.dungeon_instance_id {
                        match inspect_mission_dungeon_link(snap, cur_inst_id, sys_id) {
                            InstLinkState::Active => {
                                let pre = pre_live_npcs_for_inst(snap, cur_inst_id, &view);
                                actions.push((MissionPocketAction::Reconcile { mission_iid, inst_id: cur_inst_id }, pre));
                                continue;
                            }
                            InstLinkState::Finished => {
                                sync_mission_from_dungeon_in_snap(snap, cur_inst_id);
                                continue;
                            }
                            InstLinkState::MissingOrStale => {}
                        }
                    }
                    if clear_stale_link {
                        let _ = snap.missions.update(mission_iid, |m| { m.dungeon_instance_id = None; });
                    }
                    let derived_room_id = u32::try_from(inst.current_room_index.max(1)).unwrap_or(1);
                    let inst_id = DungeonInstanceId::new(snap.alloc_entity_id()).as_i64();
                    inst.instance_id = inst_id;
                    snap.dungeon_runtime.instances.insert(inst_id, inst);
                    let _ = snap.missions.update(mission_iid, |m| {
                        m.current_room_id = derived_room_id;
                        m.dungeon_instance_id = Some(inst_id);
                        if let Some(e) = m.encounter_mut() {
                            e.spawned_npc_ball_ids.clear();
                            e.awaiting_downtime_respawn = false;
                        }
                    });
                    actions.push((MissionPocketAction::Materialize { mission_iid, inst_id, is_wiki_dungeon }, HashSet::new()));
                }
            }
        }
    });

    actions
}

fn dungeon_mission_encounter_view(inst: &DungeonRuntimeInstance) -> (u32, Vec<i64>, Option<u32>) {
    let derived_room_id = u32::try_from(inst.current_room_index.max(1)).unwrap_or(1);
    let live_ids: Vec<i64> = if instance_is_finished(inst) {
        Vec::new()
    } else {
        inst.rooms
            .iter()
            .filter(|r| r.spawned)
            .flat_map(|r| {
                r.npc_groups
                    .iter()
                    .filter(|g| g.spawned && !g.cleared)
                    .flat_map(|g| g.npc_ball_ids.iter().copied())
            })
            .collect()
    };
    let total_remaining = if inst.completed && !inst.failed {
        Some(0)
    } else if inst.failed || inst.despawned {
        None
    } else {
        let mut total = 0u32;
        let mut has_non_npc_room_req = false;
        for r in &inst.rooms {
            if r.cleared {
                continue;
            }
            if r.npc_groups.is_empty()
                || r.triggers.iter().any(|t| {
                    !t.fired
                        && !matches!(
                            t.kind,
                            DungeonTriggerKind::OnKill { .. }
                                | DungeonTriggerKind::OnKillTagged { .. }
                                | DungeonTriggerKind::OnKillCount { .. }
                        )
                        && t.all_actions().any(|a| {
                            matches!(
                                a,
                                DungeonTriggerAction::UnlockGate
                                    | DungeonTriggerAction::UnlockEntryGate
                                    | DungeonTriggerAction::CompleteDungeon
                                    | DungeonTriggerAction::SpawnNpcGroup(_)
                                    | DungeonTriggerAction::SetObjective { .. }
                            )
                        })
                })
            {
                has_non_npc_room_req = true;
            }
            for g in &r.npc_groups {
                if g.cleared {
                    continue;
                }
                let cnt = if g.spawned {
                    g.npc_ball_ids.len()
                } else {
                    g.effective_entries()
                        .iter()
                        .map(|e| e.count.max(1))
                        .sum()
                };
                total = total.saturating_add(u32::try_from(cnt).unwrap_or(u32::MAX));
            }
        }
        // Keep encounter non-completable while un-cleared NPC-free rooms (`waves: []`)
        // or unfired non-kill progression triggers (`OnLoot`/`OnApproach`/etc.) remain.
        Some(if has_non_npc_room_req { total.max(1) } else { total })
    };
    (derived_room_id, live_ids, total_remaining)
}

pub(crate) fn sync_mission_from_dungeon_in_snap(snap: &mut WorldStateSnapshot, inst_id: i64) {
    let mission_iids: Vec<MissionInstanceId> = snap
        .missions
        .iter()
        .filter(|m| m.state == MissionState::Accepted && m.dungeon_instance_id == Some(inst_id))
        .map(|m| m.instance_id)
        .collect();
    if mission_iids.is_empty() {
        return;
    }
    let Some(inst) = snap.dungeon_runtime.instances.get(&inst_id) else {
        return;
    };
    let inst_failed = inst.failed;
    let (derived_room_id, live_ids, total_remaining) = dungeon_mission_encounter_view(inst);
    for iid in mission_iids {
        let _ = snap.missions.update(iid, |m| {
            m.current_room_id = derived_room_id;
            if let Some(enc) = m.encounter_mut() {
                enc.spawned_npc_ball_ids = live_ids.clone();
                if inst_failed {
                    enc.awaiting_downtime_respawn = false;
                    enc.npcs_remaining = enc.npcs_remaining.max(1);
                } else if let Some(rem) = total_remaining {
                    enc.npcs_remaining = rem;
                    if rem == 0 {
                        enc.awaiting_downtime_respawn = false;
                    }
                }
            }
        });
    }
}

fn sync_missions_after_dungeon_materialization(
    ctx: &ServerContext,
    completed_actions: &[(MissionPocketAction, HashSet<i64>, bool)],
) -> usize {
    if completed_actions.is_empty() {
        return 0;
    }
    let mut total_spawned = 0usize;
    ctx.db.with_mut_snapshot(|snap| {
        for (action, pre_live_in_space, ok) in completed_actions {
            let _ = snap.missions.update(action.mission_iid(), |m| {
                m.dungeon_instance_id = Some(action.inst_id());
                if let Some(enc) = m.encounter_mut() {
                    enc.awaiting_downtime_respawn = false;
                }
            });
            sync_mission_from_dungeon_in_snap(snap, action.inst_id());
            if *ok {
                let newly_spawned = snap
                    .missions
                    .get(action.mission_iid())
                    .and_then(|m| m.encounter())
                    .map_or(0, |e| {
                        e.spawned_npc_ball_ids
                            .iter()
                            .filter(|id| !pre_live_in_space.contains(id))
                            .count()
                    });
                total_spawned += newly_spawned;
            }
        }
    });
    total_spawned
}

pub(crate) async fn materialize_or_reconcile_mission_dungeon(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    char_id: i64,
    sys_id: u32,
    target_mission_id: Option<u32>,
    pocket_origin: DVec3,
) -> usize {
    if !pocket_origin.is_finite() {
        return 0;
    }
    let prepared = prepare_mission_pocket_actions(
        ctx, sys_handle, char_id, sys_id, target_mission_id, pocket_origin,
    );
    let mut results = Vec::with_capacity(prepared.len());
    for (action, pre_live_in_space) in prepared {
        let ok = match action {
            MissionPocketAction::Materialize { inst_id, is_wiki_dungeon, .. } => {
                let mat = materialize_dungeon_room_entities(ctx, sys_id, inst_id, 0).await;
                if is_wiki_dungeon && mat.is_ok() {
                    on_dungeon_room_entered(ctx, sys_id, inst_id, 0, char_id).await;
                }
                mat.is_ok()
            }
            MissionPocketAction::Reconcile { inst_id, .. } => {
                let _ = reconcile_and_rematerialize_dungeon_instance(ctx, sys_id, inst_id).await;
                true
            }
        };
        results.push((action, pre_live_in_space, ok));
    }
    sync_missions_after_dungeon_materialization(ctx, &results)
}

pub(crate) fn materialize_or_reconcile_mission_dungeon_sync(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    target_mission_id: Option<u32>,
    pocket_origin: DVec3,
) -> usize {
    if !pocket_origin.is_finite() {
        return 0;
    }
    let sys_handle = wake_dungeon_system_handle(&ctx.systems, sys_id);
    let prepared = prepare_mission_pocket_actions(
        ctx, &sys_handle, char_id, sys_id, target_mission_id, pocket_origin,
    );
    let mut results = Vec::with_capacity(prepared.len());
    for (action, pre_live_in_space) in prepared {
        let ok = match action {
            MissionPocketAction::Materialize { inst_id, is_wiki_dungeon, .. } => {
                let mat = materialize_dungeon_room_entities_sync(ctx, sys_id, inst_id, 0);
                if is_wiki_dungeon && mat.is_ok() {
                    on_dungeon_room_entered_sync(ctx, sys_id, inst_id, 0, char_id);
                }
                mat.is_ok()
            }
            MissionPocketAction::Reconcile { inst_id, .. } => {
                let _ = reconcile_and_rematerialize_dungeon_instance_sync(ctx, sys_id, inst_id);
                true
            }
        };
        results.push((action, pre_live_in_space, ok));
    }
    sync_missions_after_dungeon_materialization(ctx, &results)
}

fn mark_dungeon_npc_killed_for_mission_sync(
    inst: &mut DungeonRuntimeInstance,
    npc_id: i64,
    now_unix: u64,
) -> bool {
    if instance_is_finished(inst) {
        return false;
    }
    for (r_idx, room) in inst.rooms.iter_mut().enumerate() {
        let mut found: Option<(u8, Option<String>, u32)> = None;
        for grp in &mut room.npc_groups {
            if let Some(pos) = grp.npc_ball_ids.iter().position(|&id| id == npc_id) {
                let meta = grp.npc_ball_meta.remove(&npc_id);
                let tag = grp.npc_ball_tags.remove(&npc_id);
                grp.npc_ball_names.remove(&npc_id);
                grp.npc_ball_bounties.remove(&npc_id);
                let entries = grp.effective_entries();
                let tid = meta
                    .as_ref()
                    .map(|m| m.type_id)
                    .or_else(|| {
                        entries
                            .iter()
                            .flat_map(|e| std::iter::repeat(e.type_id).take(e.count.max(1)))
                            .nth(pos)
                    })
                    .or_else(|| grp.npc_specs.first().map(|&(t, _)| t))
                    .unwrap_or(0);
                grp.npc_ball_ids.remove(pos);
                if !grp.dead_npc_ball_ids.contains(&npc_id) {
                    grp.dead_npc_ball_ids.push(npc_id);
                }
                grp.kills_recorded = grp.kills_recorded.saturating_add(1);
                if grp.npc_ball_ids.is_empty() && grp.spawned {
                    grp.cleared = true;
                    room.armed_timers
                        .entry(format!("__group_cleared:{}", grp.group_id))
                        .or_insert(now_unix);
                }
                found = Some((grp.group_id, tag, tid));
                break;
            }
        }
        if let Some((gid, tag_opt, tid)) = found {
            let tag_str = tag_opt.unwrap_or_default();
            room.armed_timers.insert(
                format!("__mission_kill:{r_idx}:{npc_id}:{gid}:{tid}:{tag_str}"),
                now_unix,
            );
            return true;
        }
    }
    false
}

pub(crate) fn on_mission_npc_destroyed_sync(db: &WorldRepository, sys_id: u32, npc_id: i64) {
    let linked_inst_for_npc = |snap: &WorldStateSnapshot| -> Option<i64> {
        snap.missions.iter().find_map(|m| {
            (m.state == MissionState::Accepted && m.solar_system_id == sys_id)
                .then_some(m.dungeon_instance_id)
                .flatten()
                .filter(|&inst_id| {
                    snap.dungeon_runtime.instances.get(&inst_id).is_some_and(|inst| {
                        !instance_is_finished(inst)
                            && inst.solar_system_id == sys_id
                            && inst.rooms.iter().any(|r| {
                                r.npc_groups.iter().any(|g| g.npc_ball_ids.contains(&npc_id))
                            })
                    })
                })
        })
    };

    let has_hit = db.with_snapshot(|snap| {
        snap.missions
            .get_by_npc_ball(npc_id)
            .is_some_and(|m| m.state == MissionState::Accepted && m.solar_system_id == sys_id)
            || linked_inst_for_npc(snap).is_some()
    });
    if !has_hit {
        return;
    }

    let now_unix = db.clock().now_unix_secs();
    db.with_mut_snapshot(|snap| {
        let indexed_mission = snap
            .missions
            .get_by_npc_ball(npc_id)
            .filter(|m| m.state == MissionState::Accepted && m.solar_system_id == sys_id)
            .map(|m| (m.instance_id, m.dungeon_instance_id));
        let _ = snap.missions.on_npc_destroyed(sys_id, npc_id);
        let fallback_inst_id = if indexed_mission.is_none() {
            linked_inst_for_npc(snap)
        } else {
            None
        };
        let target_inst_id = indexed_mission
            .and_then(|(_, d)| d)
            .or(fallback_inst_id);

        if let Some(inst_id) = target_inst_id {
            if let Some(inst) = snap.dungeon_runtime.instances.get_mut(&inst_id) {
                if inst.solar_system_id == sys_id {
                    let _ = mark_dungeon_npc_killed_for_mission_sync(inst, npc_id, now_unix);
                }
            }
            sync_mission_from_dungeon_in_snap(snap, inst_id);
        }
    });
}

fn push_unique_downtime_ball(
    removed: &mut Vec<(u32, i64)>,
    sys_id: u32,
    bid: i64,
) {
    if bid > 0 && !removed.iter().any(|&(s, b)| s == sys_id && b == bid) {
        removed.push((sys_id, bid));
    }
}

pub(crate) fn reset_missions_on_downtime_in_snap(
    snap: &mut WorldStateSnapshot,
    removed_mission_npcs: &mut Vec<(u32, i64)>,
) -> usize {
    let mut missions_reset_count = 0usize;
    let downtime_ids: Vec<MissionInstanceId> = snap
        .missions
        .iter()
        .filter(|m| {
            m.state == MissionState::Accepted
                || m.encounter().is_some_and(|enc| {
                    enc.initial_wave_count.is_some()
                        || !enc.initial_wave_types.is_empty()
                        || enc.awaiting_downtime_respawn
                })
        })
        .map(|m| m.instance_id)
        .collect();

    for iid in downtime_ids {
        let linked_failed = snap
            .missions
            .get(iid)
            .and_then(|m| m.dungeon_instance_id)
            .and_then(|inst_id| snap.dungeon_runtime.instances.get(&inst_id))
            .is_some_and(|inst| inst.failed);
        let mut linked_inst_id: Option<i64> = None;
        let mut linked_sys_id: u32 = 0;
        let _ = snap.missions.update(iid, |mission| {
            if mission.state != MissionState::Accepted {
                if let Some(enc) = mission.encounter_mut() {
                    enc.initial_wave_count = None;
                    enc.initial_wave_types.clear();
                    enc.awaiting_downtime_respawn = false;
                }
                return;
            }
            let sys_id = mission.solar_system_id;
            linked_sys_id = sys_id;
            linked_inst_id = mission.dungeon_instance_id;
            mission.current_room_id = 1;
            let Some(enc) = mission.encounter_mut() else {
                return;
            };
            for &old_bid in &enc.spawned_npc_ball_ids {
                removed_mission_npcs.push((sys_id, old_bid));
            }
            enc.spawned_npc_ball_ids.clear();
            if linked_failed {
                enc.npcs_remaining = enc.npcs_remaining.max(1);
                enc.awaiting_downtime_respawn = false;
            } else if enc.npcs_remaining > 0 {
                // `PVE-019`: preserve exact surviving kill progress across downtime instead of
                // resetting upward to `initial_wave_count` or `.max(4)`.
                enc.awaiting_downtime_respawn = true;
                missions_reset_count += 1;
            } else {
                enc.npcs_remaining = 0;
                enc.awaiting_downtime_respawn = false;
            }
        });

        if let Some(inst_id) = linked_inst_id {
            if let Some(inst) = snap.dungeon_runtime.instances.get_mut(&inst_id) {
                let sys_id = if inst.solar_system_id > 0 {
                    inst.solar_system_id
                } else {
                    linked_sys_id
                };
                if !instance_is_finished(inst) {
                    inst.current_room_index = inst.rooms.first().map(|r| r.room_index).unwrap_or(1);
                    if let Some(eg) = inst.entry_gate_ball_id {
                        push_unique_downtime_ball(removed_mission_npcs, sys_id, eg);
                    }
                }
                for r in &inst.rooms {
                    for g in &r.npc_groups {
                        for &bid in &g.npc_ball_ids {
                            push_unique_downtime_ball(removed_mission_npcs, sys_id, bid);
                        }
                    }
                    if !instance_is_finished(inst) {
                        if let Some(gid) = r.gate_ball_id {
                            push_unique_downtime_ball(removed_mission_npcs, sys_id, gid);
                        }
                        for gl in &r.gate_links {
                            if let Some(gid) = gl.gate_ball_id {
                                push_unique_downtime_ball(removed_mission_npcs, sys_id, gid);
                            }
                        }
                        for obj in &r.objects {
                            if obj.spawned && !obj.destroyed {
                                push_unique_downtime_ball(removed_mission_npcs, sys_id, obj.ball_id);
                            }
                        }
                    }
                }
            }
        }
    }

    missions_reset_count
}

pub(super) fn finish_mission_dungeon_in_snap(
    snap: &mut WorldStateSnapshot,
    inst_id: Option<i64>,
    char_id: i64,
    is_fail: bool,
    now_unix: u64,
    ball_ids: &mut Vec<i64>,
) {
    let Some(iid) = inst_id else {
        return;
    };
    if let Some(done) =
        apply_finish_dungeon_instance_snap(snap, iid, char_id, is_fail, true, now_unix)
    {
        for bid in done
            .npc_balls_to_remove
            .into_iter()
            .chain(done.non_npc_balls_to_remove)
        {
            if bid > 0 && !ball_ids.contains(&bid) {
                ball_ids.push(bid);
            }
        }
    }
}

pub(super) async fn despawn_mission_cleanup_balls(
    ctx: &ServerContext,
    sys_id: u32,
    ast_id: Option<i64>,
    ball_ids: &[i64],
) {
    if sys_id == 0 || (ball_ids.is_empty() && ast_id.is_none()) {
        return;
    }
    if let Some(handle) = ctx.systems.get_handle(sys_id) {
        for &bid in ball_ids {
            let _ = handle.remove_ball(bid).await;
        }
        if let Some(aid) = ast_id {
            let _ = handle.remove_ball(aid).await;
        }
    }
    if !ball_ids.is_empty() {
        with_mut_combat_runtime(ctx, |rt| {
            for bid in ball_ids {
                rt.npc_entities.remove(bid);
            }
        });
    }
}

#[cfg(test)]
#[path = "agents_missions_dungeon_tests.rs"]
mod agents_missions_dungeon_tests;
