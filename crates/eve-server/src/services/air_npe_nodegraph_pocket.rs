//! `air_npe_nodegraph_pocket` — Tutorial Pocket Spawning, Starter Ship Hangar-to-Pocket
//! Transition, and Pocket/UI Restoration Helpers (`plan:E-7`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::air_npe_nodegraph_runner::{
    load_runner_state, save_runner_state, AIR_NPE_STATE_ACTIVE, NPE_DUNGEON_SCENE_1_ID,
};
use crate::services::operations_manager::{
    can_character_play_tutorial, load_store, save_store,
};
use eve_proto::BridgeRpcError;
use crate::services::pve_dungeon_runtime::{
    with_dungeon_runtime_store, DungeonNpcGroup, DungeonRuntimeInstance, DungeonRuntimeObject,
    DungeonRuntimeRoom, DungeonTrigger, DungeonTriggerAction, DungeonTriggerKind,
};
use crate::services::pve_exploration_seeding::build_type_combat_state_from_sde;
use crate::services::pve_special_sites::make_site_npc_ball;
use eve_destiny::{Ball, MovementMode};
use eve_dogma::attr_ids;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;

/// Anchor coordinates for AIR NPE deadspace pockets in the character's solar system.
pub const NPE_POCKET_ANCHOR_POS: [f64; 3] = [125_000.0, 15_000.0, -75_000.0];
const NPE_WRECKAGE_BEACON_TYPE_ID: u32 = 12_242;
const NPE_HOSTILE_DRONE_TYPE_ID: u32 = 11_747;
const NPE_ACCEL_GATE_TYPE_ID: u32 = 17_831;

pub(crate) fn spawn_or_get_npe_pocket(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    dungeon_id: u32,
) -> (i64, Vec<i64>, Vec<i64>) {
    let existing = with_dungeon_runtime_store(ctx, |store| {
        store.instances.values().find_map(|inst| {
            (!inst.completed
                && !inst.despawned
                && inst.owner_char_id == char_id
                && inst.solar_system_id == sys_id
                && inst.dungeon_id == dungeon_id)
                .then_some(inst.instance_id)
        })
    });
    if let Some(inst_id) = existing {
        return (inst_id, Vec::new(), Vec::new());
    }

    let now = ctx.now_unix();
    let dungeon_name = ctx
        .sde
        .dungeons
        .get(&dungeon_id)
        .map(|d| d.name.trim().to_string())
        .unwrap_or_else(|| format!("AIR NPE Pocket {dungeon_id}"));

    let (beacon_id, npc_id, gate_id) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 3;
        (
            snap.next_entity_id - 2,
            snap.next_entity_id - 1,
            snap.next_entity_id,
        )
    });

    let room_id = i64::from(dungeon_id) * 100 + 1;
    let beacon_pos = [
        NPE_POCKET_ANCHOR_POS[0] + 12_000.0,
        NPE_POCKET_ANCHOR_POS[1],
        NPE_POCKET_ANCHOR_POS[2] + 8_000.0,
    ];
    let npc_pos = [
        NPE_POCKET_ANCHOR_POS[0] + 18_000.0,
        NPE_POCKET_ANCHOR_POS[1] + 1_200.0,
        NPE_POCKET_ANCHOR_POS[2] + 10_000.0,
    ];
    let gate_pos = [
        NPE_POCKET_ANCHOR_POS[0] + 25_000.0,
        NPE_POCKET_ANCHOR_POS[1],
        NPE_POCKET_ANCHOR_POS[2] - 10_000.0,
    ];

    let npc_group = DungeonNpcGroup {
        group_id: 1,
        wave: 1,
        npc_specs: vec![(NPE_HOSTILE_DRONE_TYPE_ID, 1)],
        npc_ball_ids: vec![npc_id],
        spawn_immediately: true,
        spawned: true,
        cleared: false,
    };
    let room_obj = DungeonRuntimeObject {
        ball_id: beacon_id,
        type_id: NPE_WRECKAGE_BEACON_TYPE_ID,
        name: "AIR Research Facility Wreckage".to_string(),
        position: beacon_pos,
        radius: 1_500.0,
        is_container: false,
        loot_items: Vec::new(),
        spawned: true,
    };
    let trigger = DungeonTrigger {
        trigger_id: 1,
        kind: DungeonTriggerKind::OnKill {
            target_npc_group: Some(1),
            target_ball_id: Some(npc_id),
        },
        action: DungeonTriggerAction::UnlockGate,
        fired: false,
    };
    let room = DungeonRuntimeRoom {
        room_index: 1,
        room_id,
        room_name: dungeon_name.clone(),
        position: NPE_POCKET_ANCHOR_POS,
        gate_destination_pos: [
            NPE_POCKET_ANCHOR_POS[0] + 50_000.0,
            NPE_POCKET_ANCHOR_POS[1],
            NPE_POCKET_ANCHOR_POS[2],
        ],
        gated: false,
        disallow_mwd: Some(false),
        gate_ball_id: Some(gate_id),
        gate_locked: false,
        objects: vec![room_obj],
        npc_groups: vec![npc_group],
        triggers: vec![trigger],
        spawned: true,
        cleared: false,
    };

    let inst_id = with_dungeon_runtime_store(ctx, |store| {
        let id = store.next_instance_id;
        store.next_instance_id += 1;
        store.instances.insert(
            id,
            DungeonRuntimeInstance {
                instance_id: id,
                dungeon_id,
                dungeon_name: dungeon_name.clone(),
                category: "AirNpeTutorial".to_string(),
                archetype: dungeon_name.clone(),
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: NPE_POCKET_ANCHOR_POS,
                disallow_mwd: false,
                entry_gate_ball_id: Some(gate_id),
                entry_gate_pos: Some(gate_pos),
                entry_gate_locked: false,
                current_room_index: 1,
                rooms: vec![room],
                completed: false,
                despawned: false,
                created_unix: now,
                active_objective_id: 12,
                completed_task_ids: Vec::new(),
                last_completed_task_id: None,
            },
        );
        id
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let beacon_ball = make_site_npc_ball(
        beacon_id,
        NPE_WRECKAGE_BEACON_TYPE_ID,
        DVec3::from_array(beacon_pos),
    );
    let beacon_cs = build_type_combat_state_from_sde(&ctx.sde, NPE_WRECKAGE_BEACON_TYPE_ID);
    let beacon_meta = ctx.build_ship_slim_meta(500_021, NPE_WRECKAGE_BEACON_TYPE_ID);
    sys_handle.try_spawn_ball_with_meta(beacon_ball, beacon_cs, beacon_meta);

    let npc_ball = make_site_npc_ball(npc_id, NPE_HOSTILE_DRONE_TYPE_ID, DVec3::from_array(npc_pos));
    let npc_cs = build_type_combat_state_from_sde(&ctx.sde, NPE_HOSTILE_DRONE_TYPE_ID);
    let npc_meta = ctx.build_ship_slim_meta(500_021, NPE_HOSTILE_DRONE_TYPE_ID);
    sys_handle.try_spawn_ball_with_meta(npc_ball, npc_cs, npc_meta);

    let gate_ball = make_site_npc_ball(gate_id, NPE_ACCEL_GATE_TYPE_ID, DVec3::from_array(gate_pos));
    let gate_cs = build_type_combat_state_from_sde(&ctx.sde, NPE_ACCEL_GATE_TYPE_ID);
    let gate_meta = ctx.build_ship_slim_meta(500_021, NPE_ACCEL_GATE_TYPE_ID);
    sys_handle.try_spawn_ball_with_meta(gate_ball, gate_cs, gate_meta);

    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store(snap);
        let rec = store.by_character.entry(char_id).or_default();
        rec.active_dungeon_id = Some(dungeon_id);
        rec.active_site_id = Some(inst_id);
        rec.active_operation_system_id = Some(sys_id);
        save_store(snap, &store, now);
    });

    (inst_id, vec![beacon_id, gate_id], vec![npc_id])
}

pub(crate) fn spawn_or_move_ship_in_pocket(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    ship_id: i64,
    ship_type_id: u32,
) {
    let ship_type = ctx.sde.types.get(&ship_type_id);
    let max_vel = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .ok()
        .map(|f| f.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde))
        .or_else(|| {
            ship_type
                .and_then(|t| t.default_attributes.get(&attr_ids::MAX_VELOCITY).copied())
        })
        .unwrap_or(250.0);
    let inertia = ship_type
        .and_then(|t| t.default_attributes.get(&attr_ids::AGILITY).copied())
        .unwrap_or(3.0);
    let mass = ship_type.map(|t| t.mass).unwrap_or(1_200_000.0);
    let radius = ship_type.map(|t| t.radius).unwrap_or(40.0);

    let spawn_pos = DVec3::from_array(NPE_POCKET_ANCHOR_POS);
    let ball = Ball {
        id: ship_id,
        type_id: ship_type_id,
        position: spawn_pos,
        velocity: DVec3::ZERO,
        max_velocity: max_vel,
        inertia,
        mass,
        radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let combat_state = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    let slim_meta = ctx.build_ship_slim_meta(char_id, ship_type_id);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_spawn_ball_with_meta(ball, combat_state, slim_meta);
    sys_handle.try_set_ball_position(ship_id, spawn_pos);
}

pub fn is_starter_or_school_solar_system(ctx: &ServerContext, sys_id: u32) -> bool {
    if !ctx.sde.schools.is_empty() {
        return ctx
            .sde
            .schools
            .values()
            .any(|s| s.solar_system_id == sys_id && s.solar_system_id > 0);
    }
    matches!(
        sys_id,
        30_000_141
            | 30_000_142
            | 30_003_489
            | 30_005_042
            | 30_001_672
            | 30_002_547
            | 30_002_505
            | 30_003_410
            | 30_001_392
            | 30_001_407
            | 30_004_971
            | 30_002_715
            | 30_005_305
    )
}

pub(crate) fn check_npe_snapshot_combat_timers(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    now: u64,
) -> Result<(), BridgeRpcError> {
    let sp6 = crate::sp6_lifecycle::load_sp6_state_from_snap(snap);
    let snap_blocked = sp6.pvp_timers_until.get(&char_id).copied().unwrap_or(0) > now
        || sp6.npc_timers_until.get(&char_id).copied().unwrap_or(0) > now
        || sp6.weapon_timers_until.get(&char_id).copied().unwrap_or(0) > now
        || sp6.criminal_timers_until.get(&char_id).copied().unwrap_or(0) > now
        || snap
            .crimewatch_aggressions
            .iter()
            .any(|((a, v), r)| (*a == char_id || *v == char_id) && r.expires_unix > now);
    if snap_blocked {
        return Err(BridgeRpcError::user_error("NpeBlockedInCombat"));
    }
    Ok(())
}

pub(crate) fn check_npe_live_combat_or_scramble(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    ship_id: i64,
    in_space: bool,
    now: u64,
) -> Result<(), BridgeRpcError> {
    let cw_blocked = ctx
        .crimewatch
        .read()
        .ok()
        .and_then(|cw| {
            cw.states
                .get(&char_id)
                .map(|s| s.is_weapon_timer_active(now) || s.is_pvp_active(now) || s.is_criminal(now))
        })
        .unwrap_or(false);
    if cw_blocked {
        return Err(BridgeRpcError::user_error("NpeBlockedInCombat"));
    }
    let scrambled = in_space
        && ship_id > 0
        && (ctx.systems.is_ball_scrambled_sync(sys_id, ship_id)
            || ctx
                .systems
                .active_system_handles()
                .into_iter()
                .find(|(id, _)| *id == sys_id)
                .is_some_and(|(_, h)| {
                    h.ballpark
                        .try_read()
                        .is_ok_and(|bp| bp.is_ball_scrambled(ship_id))
                }));
    if scrambled {
        return Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::WarpScrambled));
    }
    Ok(())
}

pub(crate) fn validate_npe_start_or_entry(
    ctx: &ServerContext,
    char_id: i64,
) -> Result<(), BridgeRpcError> {
    let now = ctx.now_unix();
    let (sys_id, ship_id, in_space) = ctx.db.with_snapshot(|snap| {
        let Some(ch) = snap.characters.get(&char_id) else {
            return Err(BridgeRpcError::user_error("CharacterNotFound"));
        };
        if !can_character_play_tutorial(snap, char_id) {
            return Err(BridgeRpcError::user_error("TutorialNotActive"));
        }
        if !is_starter_or_school_solar_system(ctx, ch.solar_system_id) {
            return Err(BridgeRpcError::user_error("NpeStarterSystemRequired"));
        }
        check_npe_snapshot_combat_timers(snap, char_id, now)?;
        Ok((
            ch.solar_system_id,
            ch.active_ship_id,
            ch.station_id.is_none() && ch.structure_id.is_none(),
        ))
    })?;
    check_npe_live_combat_or_scramble(ctx, char_id, sys_id, ship_id, in_space, now)
}

fn station_system_info(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    dest_id: i64,
    fallback_const: u32,
    fallback_reg: u32,
) -> Option<(u32, u32, u32, bool)> {
    if let Some(st) = ctx.sde.stations.get(&dest_id) {
        return Some((st.solar_system_id, st.constellation_id, st.region_id, false));
    }
    if let Some(s) = snap.structures.get(&dest_id) {
        let (cid, rid) = ctx
            .sde
            .solar_systems
            .get(&s.solar_system_id)
            .map(|sys| (sys.constellation_id, sys.region_id))
            .unwrap_or((fallback_const, fallback_reg));
        return Some((s.solar_system_id, cid, rid, true));
    }
    None
}

fn resolve_npe_dock_target(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    origin_station: Option<i64>,
    home_station: i64,
    expected_sys: u32,
    fallback_const: u32,
    fallback_reg: u32,
) -> Option<(i64, u32, u32, u32, bool)> {
    let allowed_sys = |sid: u32| sid == expected_sys || is_starter_or_school_solar_system(ctx, sid);
    if let Some(orig) = origin_station.filter(|&id| id > 0) {
        if let Some((sid, cid, rid, is_struct)) =
            station_system_info(ctx, snap, orig, fallback_const, fallback_reg)
        {
            if allowed_sys(sid) {
                return Some((orig, sid, cid, rid, is_struct));
            }
        }
    }
    if home_station > 0 {
        if let Some((sid, cid, rid, is_struct)) =
            station_system_info(ctx, snap, home_station, fallback_const, fallback_reg)
        {
            if allowed_sys(sid) {
                return Some((home_station, sid, cid, rid, is_struct));
            }
        }
    }
    if let Some(st) = ctx
        .sde
        .stations
        .values()
        .filter(|s| s.solar_system_id == expected_sys)
        .min_by_key(|s| s.station_id)
    {
        return Some((
            st.station_id,
            st.solar_system_id,
            st.constellation_id,
            st.region_id,
            false,
        ));
    }
    None
}

pub(crate) fn try_cleanup_npe_pocket_and_restore_ui(
    ctx: &ServerContext,
    mut session: Option<&mut SessionState>,
    char_id: i64,
    require_active_completion: bool,
) -> Result<(Vec<i64>, Option<i64>), BridgeRpcError> {
    let now = ctx.now_unix();
    let live_info = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| {
            (
                c.solar_system_id,
                c.active_ship_id,
                c.station_id.is_none() && c.structure_id.is_none(),
            )
        })
    });
    if let Some((sys_id, ship_id, in_space)) = live_info {
        check_npe_live_combat_or_scramble(ctx, char_id, sys_id, ship_id, in_space, now)?;
    }
    let (stopped_instances, stop_master_by_id, pocket_cleanup, restore_station) =
        ctx.db.try_with_mut_snapshot(|snap| {
            let mut stopped = Vec::new();
            let mut master_by_id = None;
            let mut pocket_info = None;
            let mut station_restore = None;

            let runner_opt = load_runner_state(snap, char_id);
            let ch_info = snap.characters.get(&char_id).map(|c| {
                (
                    c.solar_system_id,
                    c.station_id.is_none() && c.structure_id.is_none(),
                )
            });
            if let Some((sys_id, in_space)) = ch_info {
                check_npe_snapshot_combat_timers(snap, char_id, now)?;
                let in_pocket = runner_opt.as_ref().is_some_and(|r| r.moved_into_pocket);
                if (in_space || in_pocket) && !is_starter_or_school_solar_system(ctx, sys_id) {
                    return Err(BridgeRpcError::user_error("NpeStarterSystemRequired"));
                }
                if require_active_completion {
                    let active_ok = runner_opt.as_ref().is_some_and(|r| {
                        r.state_id == AIR_NPE_STATE_ACTIVE
                            && !r.completed
                            && !r.skipped
                            && (!in_space || r.moved_into_pocket)
                    }) && is_starter_or_school_solar_system(ctx, sys_id);
                    if !active_ok {
                        return Err(BridgeRpcError::user_error("TutorialNotActive"));
                    }
                }
            } else if require_active_completion {
                return Err(BridgeRpcError::user_error("CharacterNotFound"));
            }

            if let Some(mut runner) = runner_opt {
                if let Some(sid) = runner.scene_client_instance_id.take() {
                    stopped.push(sid);
                }
                if let Some(aid) = runner.act_client_instance_id.take() {
                    stopped.push(aid);
                }
                if let Some(mid) = runner.master_client_instance_id.take() {
                    stopped.push(mid);
                    master_by_id = Some(runner.master_client_graph_id);
                }
                if let (Some(inst_id), Some(sys_id)) =
                    (runner.pocket_instance_id, runner.pocket_solar_system_id)
                {
                    pocket_info = Some((
                        inst_id,
                        sys_id,
                        runner.pocket_dungeon_id.unwrap_or(NPE_DUNGEON_SCENE_1_ID),
                    ));
                }
                if runner.moved_into_pocket {
                    let expected_sys = runner
                        .origin_solar_system_id
                        .or(runner.pocket_solar_system_id)
                        .or_else(|| snap.characters.get(&char_id).map(|c| c.solar_system_id))
                        .ok_or_else(|| BridgeRpcError {
                            code: "CharacterNotFound".into(),
                            message: format!("Character {char_id} has no known solar system"),
                            client_error: None,
                        })?;
                    let dock_target = snap.characters.get(&char_id).and_then(|ch| {
                        if ch.station_id.is_none() && ch.structure_id.is_none() {
                            resolve_npe_dock_target(
                                ctx,
                                snap,
                                runner.origin_station_id,
                                ch.home_station_id,
                                expected_sys,
                                ch.constellation_id,
                                ch.region_id,
                            )
                        } else {
                            None
                        }
                    });
                    if let Some((dest_station, new_sys_id, new_const_id, new_reg_id, is_struct)) =
                        dock_target
                    {
                        if let Some((old_sys_id, ship_id, uid, cid_corp, aid)) = snap
                            .characters
                            .get(&char_id)
                            .map(|ch| (ch.solar_system_id, ch.active_ship_id, ch.user_id, ch.corporation_id, ch.alliance_id))
                        {
                            let dest_loc = if is_struct {
                                crate::router::TransitionLocation::Structure {
                                    solar_system_id: Some(new_sys_id),
                                    structure_id: dest_station,
                                }
                            } else {
                                crate::router::TransitionLocation::Station {
                                    solar_system_id: Some(new_sys_id),
                                    station_id: dest_station,
                                }
                            };
                            let _ = crate::router::apply_character_transition_in_snap(
                                snap, &ctx.sde, char_id, dest_loc, None, true, false,
                            );
                            snap.item_positions.remove(&ship_id);
                            station_restore = Some((
                                dest_station,
                                is_struct,
                                old_sys_id,
                                new_sys_id,
                                ship_id,
                                uid,
                                cid_corp,
                                aid,
                                new_const_id,
                                new_reg_id,
                            ));
                        }
                    }
                }
                save_runner_state(snap, &runner);
            }
            Ok((stopped, master_by_id, pocket_info, station_restore))
        })?;

    if let Some((inst_id, sys_id, dungeon_id)) = pocket_cleanup {
        let balls_to_remove = with_dungeon_runtime_store(ctx, |store| {
            let mut balls = Vec::new();
            if let Some(inst) = store.instances.get_mut(&inst_id) {
                inst.completed = true;
                inst.despawned = true;
                if let Some(eg) = inst.entry_gate_ball_id {
                    balls.push(eg);
                }
                for r in &inst.rooms {
                    for o in &r.objects {
                        balls.push(o.ball_id);
                    }
                    for g in &r.npc_groups {
                        balls.extend_from_slice(&g.npc_ball_ids);
                    }
                }
            }
            balls
        });
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        for bid in balls_to_remove {
            sys_handle.try_remove_ball(bid);
        }
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnDungeonExited {
                dungeon_id: i64::from(dungeon_id),
                instance_id: inst_id,
            },
        );
    }

    if let Some((
        dest_station,
        is_struct,
        old_sys_id,
        new_sys_id,
        ship_id,
        uid,
        corp_id,
        alliance_id,
        const_id,
        reg_id,
    )) = restore_station
    {
        ctx.systems.get_or_spawn(old_sys_id).try_remove_ball(ship_id);
        if let Some(sess) = session.as_deref_mut() {
            let _ = ctx.execute_session_transition_sync(
                Some(sess),
                crate::router::SessionTransitionSpec::for_kind(
                    char_id,
                    crate::router::TransitionKind::Teleport {
                        solar_system_id: new_sys_id,
                        dock_id: Some((dest_station, is_struct)),
                        ship_id: None,
                    },
                    "npe_return",
                )
                .with_apply_db(false)
                .with_sync_local_chat(false)
                .with_emit_on_session_changed(false),
            );
        }
        let _ = (const_id, reg_id);
        let (station_rep, structure_rep, change_key) = if is_struct {
            (PyRep::None, PyRep::Integer(dest_station), "structureid")
        } else {
            (PyRep::Integer(dest_station), PyRep::None, "stationid")
        };
        let sess_rep = PyRep::keyval([
            ("userid", PyRep::Integer(uid as i64)),
            ("charid", PyRep::Integer(char_id)),
            ("corpid", PyRep::Integer(corp_id)),
            (
                "allianceid",
                alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
            ),
            ("shipid", PyRep::Integer(ship_id)),
            ("stationid", station_rep),
            ("structureid", structure_rep),
            ("solarsystemid2", PyRep::Integer(i64::from(new_sys_id))),
            ("solarsystemid", PyRep::Integer(i64::from(new_sys_id))),
            ("locationid", PyRep::Integer(dest_station)),
            ("constellationid", PyRep::Integer(i64::from(const_id))),
            ("regionid", PyRep::Integer(i64::from(reg_id))),
        ]);
        let mut changes = vec![(
            PyRep::String(change_key.into()),
            PyRep::Tuple(vec![PyRep::None, PyRep::Integer(dest_station)]),
        )];
        if old_sys_id != new_sys_id {
            changes.push((
                PyRep::String("solarsystemid2".into()),
                PyRep::Tuple(vec![
                    PyRep::Integer(i64::from(old_sys_id)),
                    PyRep::Integer(i64::from(new_sys_id)),
                ]),
            ));
        }
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnSessionChanged {
                is_remote: false,
                session: sess_rep,
                change: PyRep::Dict(changes),
            },
        );
    }

    Ok((stopped_instances, stop_master_by_id))
}

#[cfg(test)]
pub(crate) fn cleanup_npe_pocket_and_restore_ui(
    ctx: &ServerContext,
    session: Option<&mut SessionState>,
    char_id: i64,
) -> (Vec<i64>, Option<i64>) {
    try_cleanup_npe_pocket_and_restore_ui(ctx, session, char_id, false).unwrap_or_default()
}

#[cfg(test)]
#[path = "air_npe_nodegraph_pocket_tests.rs"]
mod air_npe_nodegraph_pocket_tests;

#[cfg(test)]
#[path = "air_npe_nodegraph_pocket_edges_tests.rs"]
mod air_npe_nodegraph_pocket_edges_tests;

