//! `pve_abyssal_lifecycle` — Abyssal Deadspace 20-minute pocket collapse, 11:00 UTC downtime
//! evacuation, Void Space session expiry evacuation, and login reconciliation.

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::abyssal_void_mgr::VOID_SPACE_SYSTEM_ID;
use crate::services::pve_abyssal_hazards::{
    resolve_character_home_station_and_system, restore_abyssal_weather_dogma_modifiers,
};
use crate::services::pve_abyssal_pylons::{
    build_fitted_ship_space_ball, destroy_pod_and_respawn_faction_corvette_in_snap,
};
use crate::services::pve_special_sites::{
    spec_now_sec, with_pve_special_reg, VoidSpaceSessionRecord,
};
use eve_proto::PyRep;
use eve_sde::{AttributeId, SolarSystemId};
use glam::DVec3;
use std::collections::{HashMap, HashSet};

type CharacterId = i64;
type ItemId = i64;

pub(crate) fn random_offset_around_trace_origin(
    ctx: &ServerContext,
    origin_pos: [f64; 3],
) -> [f64; 3] {
    let u1 = ctx.mechanic_rng.try_unit_f64().unwrap_or(0.37);
    let u2 = ctx.mechanic_rng.try_unit_f64().unwrap_or(0.61);
    let u3 = ctx.mechanic_rng.try_unit_f64().unwrap_or(0.83);
    let sign = |u: f64| if u >= 0.5 { 1.0 } else { -1.0 };
    let mag = |u: f64| 500.0 + ((u * 2.0 - 1.0).abs()) * 4_500.0;
    [
        origin_pos[0] + sign(u1) * mag(u1),
        origin_pos[1] + sign(u2) * mag(u2),
        origin_pos[2] + sign(u3) * mag(u3),
    ]
}

pub fn collapse_abyssal_pocket_and_pod_pilot(
    ctx: &ServerContext,
    char_id: CharacterId,
    ship_id: ItemId,
    abyssal_sys: SolarSystemId,
) -> SolarSystemId {
    collapse_abyssal_pocket_and_pod_pilot_at(ctx, char_id, ship_id, abyssal_sys, ctx.now_unix())
}

pub fn collapse_abyssal_pocket_and_pod_pilot_at(
    ctx: &ServerContext,
    char_id: CharacterId,
    ship_id: ItemId,
    abyssal_sys: SolarSystemId,
    now_unix: u64,
) -> SolarSystemId {
    let pocket_cleanup = with_pve_special_reg(ctx, |reg| {
        let (pid, pocket_sys, origin_sys, bound_tid, npcs) = {
            let p = reg.abyssal_pockets.get_mut(&char_id)?;
            p.collapsed_destroyed = true;
            p.completed = true;
            let npcs: Vec<i64> = p
                .rooms
                .iter()
                .flat_map(|r| r.npc_ball_ids.iter().copied())
                .collect();
            (
                p.pocket_id,
                p.abyssal_solar_system_id,
                p.origin_solar_system_id,
                p.trace_ball_id,
                npcs,
            )
        };
        let any_other_alive = reg
            .abyssal_pockets
            .values()
            .any(|other| other.pocket_id == pid && !other.completed && !other.collapsed_destroyed);
        let mut trace_ids = Vec::new();
        if !any_other_alive {
            if bound_tid > 0 {
                trace_ids.push(bound_tid);
            }
            reg.abyssal_traces.retain(|&tid, t| {
                if t.pocket_id == Some(pid) || (bound_tid > 0 && tid == bound_tid) {
                    if !trace_ids.contains(&tid) {
                        trace_ids.push(tid);
                    }
                    false
                } else {
                    true
                }
            });
        }
        let markers = [
            1_895_000_000 + pid * 10 + 1,
            1_895_000_000 + pid * 10 + 2,
            1_895_000_000 + pid * 10 + 3,
        ];
        let caches = [
            1_896_000_000 + pid * 10 + 1,
            1_896_000_000 + pid * 10 + 2,
            1_896_000_000 + pid * 10 + 3,
            1_896_500_000 + pid * 10 + 1,
            1_896_500_000 + pid * 10 + 2,
            1_896_500_000 + pid * 10 + 3,
        ];
        let mut env_balls = Vec::new();
        for r in 1..=3_i64 {
            for c in 1..=3_i64 {
                env_balls.push(1_898_000_000 + pid * 100 + r * 10 + c);
            }
            for py in 1..=5_i64 {
                env_balls.push(1_899_000_000 + pid * 100 + r * 10 + py);
            }
        }
        Some((
            pocket_sys,
            origin_sys,
            any_other_alive,
            trace_ids,
            markers,
            caches,
            env_balls,
            npcs,
        ))
    });

    let eff_pocket_sys = pocket_cleanup.as_ref().map(|c| c.0).unwrap_or(abyssal_sys);
    let handle = ctx.systems.get_or_spawn(eff_pocket_sys);
    handle.try_remove_ball(ship_id);
    if let Some((_, origin_sys, any_other_alive, ref trace_ids, ref markers, ref caches, ref env_balls, ref npcs)) =
        pocket_cleanup
    {
        let origin_handle = ctx.systems.get_or_spawn(origin_sys);
        for &tid in trace_ids {
            origin_handle.try_remove_ball(tid);
        }
        if !any_other_alive {
            for &mid in markers {
                handle.try_remove_ball(mid);
            }
            for &cid in caches {
                handle.try_remove_ball(cid);
            }
            for &eid in env_balls {
                handle.try_remove_ball(eid);
            }
            for &nid in npcs {
                handle.try_remove_ball(nid);
            }
            crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                for nid in npcs {
                    rt.npc_entities.remove(nid);
                }
            });
        }
    }
    ctx.invalidate_ship_fitting(ship_id);
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.insurance_contracts.remove(&ship_id);
    });
    crate::services::inventory_fitting::fitting_containers_ships::write_container_ship_state(ctx, |st| {
        st.insurance_contracts.remove(&ship_id);
    });

    let (home_sys_id, _home_station_id, _corvette_id, cleared_zarzakh_gate) =
        crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, snap| {
            st.abyssal_sessions.remove(&char_id);
            if let Some((_, _, false, _, _, ref caches, _, _)) = pocket_cleanup {
                for &cid in caches {
                    snap.items.remove(&cid);
                    snap.item_positions.remove(&cid);
                }
                snap.items.retain(|_, item| !caches.contains(&item.location_id));
            }

            let ship_type_id = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(606);
            let death_pos = snap.item_positions.remove(&ship_id).unwrap_or([0.0, 0.0, 0.0]);
            let base_price = ctx
                .sde
                .types
                .get(&ship_type_id)
                .map(|t| t.base_price)
                .unwrap_or(250_000.0);

            snap.items.remove(&ship_id);
            snap.module_online_states.remove(&ship_id);
            st.ship_combat_hp.remove(&ship_id);
            snap.items.retain(|_, item| item.location_id != ship_id);
            let cleared_lock = snap.zarzakh_gate_locks.remove(&char_id).map(|(gid, _)| gid);

            if ship_type_id != 670 {
                snap.next_entity_id += 1;
                let ship_kid = snap.next_entity_id;
                snap.record_killmail(
                    eve_db::KillmailRecord {
                        kill_id: ship_kid,
                        victim_character_id: char_id,
                        attacker_character_id: 0,
                        attacker_corp_id: 1_000_298,
                        solar_system_id: eff_pocket_sys,
                        ship_type_id,
                        total_value_isk: base_price,
                        ..Default::default()
                    },
                    now_unix,
                );
            }
            let (home_station_id, home_sys_id, corvette_id, _corpse_id, _pod_kid) =
                destroy_pod_and_respawn_faction_corvette_in_snap(
                    snap,
                    Some(&ctx.sde),
                    char_id,
                    eff_pocket_sys,
                    death_pos,
                    now_unix,
                );
            (home_sys_id, home_station_id, corvette_id, cleared_lock)
        });

    if let Some(locked_gate_id) = cleared_zarzakh_gate {
        crate::services::quasar_sov_services::publish_stargate_lock_deactivated_notice(
            &ctx.quasar_gateway,
            char_id,
            30_100_000,
            locked_gate_id,
            now_unix,
        );
    }
    crate::services::space_destiny::record_session_change_timer(
        ctx,
        char_id,
        now_unix,
        home_sys_id,
        0,
        "podding",
    );
    crate::services::map_service::record_solar_system_visit(ctx, char_id, home_sys_id);
    home_sys_id
}

#[allow(clippy::type_complexity)]
pub fn evacuate_abyssal_pockets_for_downtime(ctx: &ServerContext) -> i64 {
    let (evacuated, pocket_cleanups, despawned_traces): (
        Vec<(
            CharacterId,
            ItemId,
            SolarSystemId,
            SolarSystemId,
            [f64; 3],
            HashMap<AttributeId, f64>,
        )>,
        Vec<(i64, SolarSystemId, Vec<i64>)>,
        Vec<(SolarSystemId, i64)>,
    ) = with_pve_special_reg(ctx, |reg| {
        let mut out = Vec::new();
        let mut seen_pids = HashSet::new();
        let mut cleanups = Vec::new();
        let mut traces = Vec::new();
        let mut bound_by_pid: HashMap<i64, (SolarSystemId, i64)> = HashMap::new();
        for p in reg.abyssal_pockets.values_mut() {
            if !p.completed && !p.collapsed_destroyed {
                p.completed = true;
                out.push((
                    p.character_id,
                    p.ship_id,
                    p.abyssal_solar_system_id,
                    p.origin_solar_system_id,
                    p.origin_position,
                    p.saved_ship_attrs.clone(),
                ));
                if p.trace_ball_id > 0 {
                    bound_by_pid.insert(p.pocket_id, (p.origin_solar_system_id, p.trace_ball_id));
                    traces.push((p.origin_solar_system_id, p.trace_ball_id));
                }
                if seen_pids.insert(p.pocket_id) {
                    let npcs: Vec<i64> = p
                        .rooms
                        .iter()
                        .flat_map(|r| r.npc_ball_ids.iter().copied())
                        .collect();
                    cleanups.push((p.pocket_id, p.abyssal_solar_system_id, npcs));
                }
            }
        }
        if !seen_pids.is_empty() {
            reg.abyssal_traces.retain(|&tid, t| {
                let matches_evac = t.pocket_id.is_some_and(|pid| seen_pids.contains(&pid))
                    || bound_by_pid.values().any(|&(_, btid)| btid == tid);
                if matches_evac {
                    let entry = (t.origin_solar_system_id, tid);
                    if !traces.contains(&entry) {
                        traces.push(entry);
                    }
                    false
                } else {
                    true
                }
            });
        }
        (out, cleanups, traces)
    });

    for (origin_sys, tid) in despawned_traces {
        ctx.systems.get_or_spawn(origin_sys).try_remove_ball(tid);
    }

    for (pid, abyss_sys, npcs) in pocket_cleanups {
        let abyss_handle = ctx.systems.get_or_spawn(abyss_sys);
        for &nid in &npcs {
            abyss_handle.try_remove_ball(nid);
        }
        crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
            for &nid in &npcs {
                rt.npc_entities.remove(&nid);
            }
        });
        for r in 1..=3_i64 {
            let marker_id = 1_895_000_000 + pid * 10 + r;
            let cache_id = 1_896_000_000 + pid * 10 + r;
            let subnode_id = cache_id + 500_000;
            abyss_handle.try_remove_ball(marker_id);
            abyss_handle.try_remove_ball(cache_id);
            abyss_handle.try_remove_ball(subnode_id);
            for c in 1..=3_i64 {
                abyss_handle.try_remove_ball(1_898_000_000 + pid * 100 + r * 10 + c);
            }
            for py in 1..=5_i64 {
                abyss_handle.try_remove_ball(1_899_000_000 + pid * 100 + r * 10 + py);
            }
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.remove(&cache_id);
                snap.item_positions.remove(&cache_id);
                snap.items.remove(&subnode_id);
                snap.item_positions.remove(&subnode_id);
                snap.items
                    .retain(|_, item| item.location_id != cache_id && item.location_id != subnode_id);
            });
        }
    }

    for (cid, sid, abyss_sys, origin_sys, origin_pos, saved_attrs) in &evacuated {
        restore_abyssal_weather_dogma_modifiers(ctx, *sid, saved_attrs);
        crate::sp6_lifecycle::clear_abyssal_session(ctx, *cid);
        let evacuated_pos = random_offset_around_trace_origin(ctx, *origin_pos);
        let abyss_handle = ctx.systems.get_or_spawn(*abyss_sys);
        abyss_handle.try_remove_ball(*sid);
        let mut destroyed_drones: Vec<ItemId> = Vec::new();
        crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
            rt.active_drones_in_space.retain(|&did, d| {
                if d.owner_char_id == *cid || d.parent_ship_id == *sid {
                    destroyed_drones.push(did);
                    false
                } else {
                    true
                }
            });
        });
        for did in &destroyed_drones {
            abyss_handle.try_remove_ball(*did);
        }
        let ship_type = ctx.db.with_mut_snapshot(|snap| {
            for did in &destroyed_drones {
                snap.items.remove(did);
                snap.item_positions.remove(did);
            }
            if let Some(ch) = snap.characters.get_mut(cid) {
                ch.solar_system_id = *origin_sys;
                ch.version += 1;
                ch.system_epoch += 1;
            }
            if let Some(sh) = snap.items.get_mut(sid) {
                sh.location_id = *origin_sys as i64;
            }
            snap.item_positions.insert(*sid, evacuated_pos);
            snap.items.get(sid).map(|i| i.type_id).unwrap_or(621)
        });
        let cs = ctx.build_ship_combat_state(*cid, *sid, ship_type);
        let ball = build_fitted_ship_space_ball(
            ctx,
            *cid,
            *sid,
            ship_type,
            DVec3::from_array(evacuated_pos),
        );
        let meta = ctx.build_ship_slim_meta(*cid, ship_type);
        ctx.systems
            .get_or_spawn(*origin_sys)
            .try_spawn_ball_with_meta(ball, cs, meta);
        ctx.sync_character_local_chat(*cid, Some(*abyss_sys), *origin_sys);
        crate::services::map_service::record_solar_system_visit(ctx, *cid, *origin_sys);
    }
    evacuated.len() as i64
}

/// Evacuates an expired Void Space (`32_000_002`) session back to its origin solar system so no
/// pilot is ever stranded in `32_000_002` when the pocket expires.
pub fn evacuate_void_space_session_sync(
    ctx: &ServerContext,
    char_id: CharacterId,
    vs: &VoidSpaceSessionRecord,
    _now_unix: u64,
) -> SolarSystemId {
    with_pve_special_reg(ctx, |reg| {
        reg.void_space_sessions.remove(&char_id);
    });
    crate::sp6_lifecycle::clear_abyssal_session(ctx, char_id);

    let origin_sys = vs.origin_solar_system_id;
    let exit_pos = [
        vs.origin_position[0] + 1_000.0,
        vs.origin_position[1] + 1_000.0,
        vs.origin_position[2] + 1_000.0,
    ];
    let (was_in_void, eff_ship_id, ship_type, finished_content_id) =
        ctx.db.with_mut_snapshot(|snap| {
            let cid = snap
                .service_state_blobs
                .remove(&format!("void_space_content:{char_id}"))
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(vs.target_id);
            let Some(ch) = snap.characters.get_mut(&char_id) else {
                return (false, vs.ship_id, 621_u32, cid);
            };
            if ch.solar_system_id != VOID_SPACE_SYSTEM_ID {
                return (false, ch.active_ship_id, 621_u32, cid);
            }
            let sid = if ch.active_ship_id > 0 {
                ch.active_ship_id
            } else {
                vs.ship_id
            };
            ch.solar_system_id = origin_sys;
            ch.version += 1;
            ch.system_epoch += 1;
            if let Some(sh) = snap.items.get_mut(&sid) {
                sh.location_id = origin_sys as i64;
            }
            snap.item_positions.insert(sid, exit_pos);
            let stype = snap.items.get(&sid).map(|i| i.type_id).unwrap_or(621);
            (true, sid, stype, cid)
        });

    if !was_in_void {
        return ctx
            .db
            .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))
            .unwrap_or(origin_sys);
    }

    ctx.systems
        .get_or_spawn(VOID_SPACE_SYSTEM_ID)
        .try_remove_ball(eff_ship_id);
    let cs = ctx.build_ship_combat_state(char_id, eff_ship_id, ship_type);
    let ball = build_fitted_ship_space_ball(
        ctx,
        char_id,
        eff_ship_id,
        ship_type,
        DVec3::from_array(exit_pos),
    );
    let meta = ctx.build_ship_slim_meta(char_id, ship_type);
    ctx.systems
        .get_or_spawn(origin_sys)
        .try_spawn_ball_with_meta(ball, cs, meta);
    ctx.sync_character_local_chat(char_id, Some(VOID_SPACE_SYSTEM_ID), origin_sys);
    crate::services::map_service::record_solar_system_visit(ctx, char_id, origin_sys);

    let sess_rep = PyRep::keyval([
        ("charid", PyRep::Integer(char_id)),
        ("solarsystemid2", PyRep::Integer(origin_sys as i64)),
        ("solarsystemid", PyRep::Integer(origin_sys as i64)),
    ]);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSessionChanged",
        "charid",
        vec![
            PyRep::Bool(false),
            sess_rep,
            PyRep::Dict(vec![(
                PyRep::String("solarsystemid".into()),
                PyRep::Tuple(vec![
                    PyRep::Integer(VOID_SPACE_SYSTEM_ID as i64),
                    PyRep::Integer(origin_sys as i64),
                ]),
            )]),
        ],
    );
    // DATA GAP: V24 content-finished timer_type enum mappings are Tranquility server-only
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnVoidSpaceContentFinished",
        "charid",
        vec![PyRep::Integer(finished_content_id), PyRep::Integer(1)],
    );
    origin_sys
}

/// Sweeps any expired `void_space_sessions` during cluster time progression.
pub fn sweep_expired_void_space_sessions_sync(ctx: &ServerContext, now_unix: u64) {
    let expired: Vec<VoidSpaceSessionRecord> = with_pve_special_reg(ctx, |reg| {
        let due_cids: Vec<CharacterId> = reg
            .void_space_sessions
            .iter()
            .filter(|(_, vs)| vs.expires_at_sec > 0 && now_unix >= vs.expires_at_sec)
            .map(|(&cid, _)| cid)
            .collect();
        due_cids
            .into_iter()
            .filter_map(|cid| reg.void_space_sessions.remove(&cid))
            .collect()
    });
    for vs in expired {
        evacuate_void_space_session_sync(ctx, vs.char_id, &vs, now_unix);
    }
}

pub enum AbyssalLoginAction {
    Active(SolarSystemId),
    CollapsedDestroy {
        ship_id: ItemId,
        abyssal_sys: SolarSystemId,
    },
    EvacuatedVoid(VoidSpaceSessionRecord),
    Evacuated {
        origin_sys: SolarSystemId,
        origin_pos: [f64; 3],
        saved_attrs: HashMap<AttributeId, f64>,
    },
}

pub fn reconcile_abyssal_login(
    ctx: &ServerContext,
    char_id: CharacterId,
    cur_sys: SolarSystemId,
    ship_id: ItemId,
) -> SolarSystemId {
    if !(32_000_000..33_000_000).contains(&cur_sys) {
        return cur_sys;
    }
    let has_sp6_only = with_pve_special_reg(ctx, |reg| {
        !reg.abyssal_pockets.contains_key(&char_id)
            && !reg.void_space_sessions.contains_key(&char_id)
    }) && crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
        st.abyssal_sessions.contains_key(&char_id)
    });
    if has_sp6_only {
        return cur_sys;
    }
    let fallback_sys = ctx.db.with_snapshot(|snap| {
        resolve_character_home_station_and_system(snap, &ctx.sde, char_id).1
    });
    let action = with_pve_special_reg(ctx, |reg| {
        let now = spec_now_sec(reg);
        if cur_sys == VOID_SPACE_SYSTEM_ID {
            if let Some(vs) = reg.void_space_sessions.get(&char_id).cloned() {
                if vs.expires_at_sec == 0 || now < vs.expires_at_sec {
                    return AbyssalLoginAction::Active(VOID_SPACE_SYSTEM_ID);
                }
                return AbyssalLoginAction::EvacuatedVoid(vs);
            }
        }
        if let Some(p) = reg.abyssal_pockets.get_mut(&char_id) {
            if !p.completed && !p.collapsed_destroyed && now < p.collapse_at_sec {
                return AbyssalLoginAction::Active(p.abyssal_solar_system_id);
            }
            if !p.completed && (p.collapsed_destroyed || now >= p.collapse_at_sec) {
                p.collapsed_destroyed = true;
                p.completed = true;
                return AbyssalLoginAction::CollapsedDestroy {
                    ship_id: p.ship_id,
                    abyssal_sys: p.abyssal_solar_system_id,
                };
            }
            return AbyssalLoginAction::Evacuated {
                origin_sys: p.origin_solar_system_id,
                origin_pos: p.origin_position,
                saved_attrs: p.saved_ship_attrs.clone(),
            };
        }
        AbyssalLoginAction::Evacuated {
            origin_sys: fallback_sys,
            origin_pos: [145_000.0, -75_000.0, 42_000.0],
            saved_attrs: HashMap::new(),
        }
    });
    match action {
        AbyssalLoginAction::Active(abyssal_sys) => abyssal_sys,
        AbyssalLoginAction::CollapsedDestroy {
            ship_id: sid,
            abyssal_sys,
        } => collapse_abyssal_pocket_and_pod_pilot(ctx, char_id, sid, abyssal_sys),
        AbyssalLoginAction::EvacuatedVoid(vs) => {
            evacuate_void_space_session_sync(ctx, char_id, &vs, ctx.now_unix())
        }
        AbyssalLoginAction::Evacuated {
            origin_sys,
            origin_pos,
            saved_attrs,
        } => {
            if !saved_attrs.is_empty() {
                restore_abyssal_weather_dogma_modifiers(ctx, ship_id, &saved_attrs);
            }
            let rand_pos = random_offset_around_trace_origin(ctx, origin_pos);
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.solar_system_id = origin_sys;
                }
                if let Some(sh) = snap.items.get_mut(&ship_id) {
                    if sh.location_id == cur_sys as i64 {
                        sh.location_id = origin_sys as i64;
                    }
                }
                snap.item_positions.entry(ship_id).or_insert(rand_pos);
            });
            origin_sys
        }
    }
}
