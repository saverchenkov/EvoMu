//! `air_npe_nodegraph_pocket` — Tutorial Pocket Spawning, Starter Ship Hangar-to-Pocket
//! Transition, and Pocket/UI Restoration Helpers (`plan:E-7`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::air_npe_nodegraph_runner::{
    load_runner_state, save_runner_state, NPE_DUNGEON_SCENE_1_ID,
};
use crate::services::operations_manager::{load_store, save_store};
use crate::services::pve_dungeon_runtime::{
    with_dungeon_runtime_store, DungeonNpcGroup, DungeonRuntimeInstance, DungeonRuntimeObject,
    DungeonRuntimeRoom, DungeonTrigger, DungeonTriggerAction, DungeonTriggerKind,
};
use crate::services::pve_exploration_seeding::build_type_combat_state_from_sde;
use crate::services::pve_special_sites::make_site_npc_ball;
use eve_db::InventoryFlag;
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

pub(crate) fn cleanup_npe_pocket_and_restore_ui(
    ctx: &ServerContext,
    mut session: Option<&mut SessionState>,
    char_id: i64,
) -> (Vec<i64>, Option<i64>) {
    let (stopped_instances, stop_master_by_id, pocket_cleanup, restore_station) =
        ctx.db.with_mut_snapshot(|snap| {
            let mut stopped = Vec::new();
            let mut master_by_id = None;
            let mut pocket_info = None;
            let mut station_restore = None;

            if let Some(mut runner) = load_runner_state(snap, char_id) {
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
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        if ch.station_id.is_none() && ch.structure_id.is_none() {
                            let target_station = runner
                                .origin_station_id
                                .unwrap_or(ch.home_station_id);
                            let ship_id = ch.active_ship_id;
                            let sys_id = ch.solar_system_id;
                            if target_station > 0 {
                                ch.station_id = Some(target_station);
                                ch.system_epoch += 1;
                                ch.version += 1;
                                if let Some(ship) = snap.items.get_mut(&ship_id) {
                                    ship.location_id = target_station;
                                    ship.flag = InventoryFlag::Hangar;
                                    ship.version += 1;
                                }
                                station_restore = Some((
                                    target_station,
                                    sys_id,
                                    ship_id,
                                    ch.user_id,
                                    ch.corporation_id,
                                    ch.alliance_id,
                                    ch.constellation_id,
                                    ch.region_id,
                                ));
                            }
                        }
                    }
                }
                save_runner_state(snap, &runner);
            }
            (stopped, master_by_id, pocket_info, station_restore)
        });

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
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnDungeonExited",
            "charid",
            vec![
                PyRep::Integer(i64::from(dungeon_id)),
                PyRep::Integer(inst_id),
            ],
        );
    }

    if let Some((
        target_station,
        sys_id,
        ship_id,
        uid,
        corp_id,
        alliance_id,
        const_id,
        reg_id,
    )) = restore_station
    {
        ctx.systems.get_or_spawn(sys_id).try_remove_ball(ship_id);
        if let Some(sess) = session.as_deref_mut() {
            sess.station_id = Some(target_station);
            sess.structure_id = None;
            sess.solar_system_id = Some(sys_id);
        }
        let sess_rep = PyRep::keyval([
            ("userid", PyRep::Integer(uid as i64)),
            ("charid", PyRep::Integer(char_id)),
            ("corpid", PyRep::Integer(corp_id)),
            (
                "allianceid",
                alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
            ),
            ("shipid", PyRep::Integer(ship_id)),
            ("stationid", PyRep::Integer(target_station)),
            ("structureid", PyRep::None),
            ("solarsystemid2", PyRep::Integer(i64::from(sys_id))),
            ("solarsystemid", PyRep::Integer(i64::from(sys_id))),
            ("locationid", PyRep::Integer(target_station)),
            ("constellationid", PyRep::Integer(i64::from(const_id))),
            ("regionid", PyRep::Integer(i64::from(reg_id))),
        ]);
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnSessionChanged",
            "charid",
            vec![
                PyRep::Bool(false),
                sess_rep,
                PyRep::Dict(vec![(
                    PyRep::String("stationid".into()),
                    PyRep::Tuple(vec![PyRep::None, PyRep::Integer(target_station)]),
                )]),
            ],
        );
    }

    (stopped_instances, stop_master_by_id)
}
