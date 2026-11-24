//! `agents_missions_accept` — `agentMgr.DoAction` mission accept arms (`3..=5`).

#![forbid(unsafe_code)]

use super::{
    build_mission_briefing_text, classify_mission_archetype, deterministic_mission_site_origin,
    finalize_mission_state_cleanup, is_agent_colocated, is_rnd_agent, is_storyline_agent_type,
    is_storyline_template, load_mission_meta, mission_bonus_window_secs,
    resolve_mission_npc_type, resolve_security_mission_encounter, resolve_target_mission_id,
    save_mission_meta,
};
use crate::router::notify_registry::OnAgentMissionChange;
use crate::router::{NotificationTarget, ServerContext};
use crate::services::notify_emitters_character_services::emit_agent_mission_dialogue;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

pub(super) async fn handle_mission_accept_action(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    sys_id: u32,
    action_id: i64,
    agent_id: i64,
    explicit_agent: bool,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let lookup_agent_id = if explicit_agent { agent_id } else { 0 };
    let mission_id = match resolve_target_mission_id(ctx, char_id, lookup_agent_id, Some("Offered"), args, _kwargs) { Ok(v) => v, Err(e) => return Some(Err(e)) };
    let (is_alpha, agent_lvl, actual_agent_id) = ctx.db.with_snapshot(|snap| {
        let alpha = snap.characters.get(&char_id).is_some_and(|c| c.clone_grade <= 1);
        let mid_agent = snap.active_missions.get(&(char_id, mission_id)).map(|m| m.agent_id).unwrap_or(agent_id);
        let lvl = ctx.sde.agents.get(&mid_agent).map_or(1, |a| a.level);
        (alpha, lvl, mid_agent)
    });
    if is_alpha && agent_lvl >= 4 {
        return Some(Err(BridgeRpcError {
            code: "CantDoMissionAlphaClone".into(),
            message: format!("Agent level {agent_lvl} requires Omega clone state"),
            client_error: None,
        }));
    }
    let tmpl_opt = ctx.sde.missions.get(&mission_id).cloned();
    let agent_info_opt = ctx.sde.agents.get(&actual_agent_id).cloned();
    if let Some(ref agent_info) = agent_info_opt {
        let colocated = is_agent_colocated(ctx, session, sys_id, agent_info).await;
        if matches!(action_id, 3 | 4) && !colocated {
            return Some(Err(BridgeRpcError {
                code: "AgentNotCoLocated".into(),
                message: format!(
                    "Agent {actual_agent_id} is in system {:?} / station {:?}, not character's current location (system {sys_id}, station {:?})",
                    agent_info.solar_system_id, agent_info.station_id, session.station_id
                ),
                client_error: None,
            }));
        }
        if action_id == 5 && !colocated {
            let is_burner = ctx.db.with_snapshot(|snap| {
                load_mission_meta(snap, char_id, mission_id)
                    .is_some_and(|mt| mt.is_anomic_burner)
            });
            let remote_eligible = is_storyline_agent_type(agent_info.agent_type_id)
                || tmpl_opt.as_ref().is_some_and(is_storyline_template)
                || matches!(agent_info.agent_type_id, 5 | 8)
                || is_burner;
            if !remote_eligible {
                return Some(Err(BridgeRpcError {
                    code: "CannotAcceptRemotely".into(),
                    message: format!(
                        "Mission {mission_id} from agent {actual_agent_id} requires visiting the agent to accept"
                    ),
                    client_error: None,
                }));
            }
        }
    }
    let mission_sys_id = ctx
        .db
        .with_snapshot(|snap| {
            snap.active_missions
                .get(&(char_id, mission_id))
                .map(|m| m.solar_system_id)
                .filter(|&s| s > 0)
        })
        .or_else(|| {
            agent_info_opt
                .as_ref()
                .and_then(|a| a.solar_system_id)
                .filter(|&s| s > 0)
        })
        .unwrap_or(sys_id);
    let enc_opt = tmpl_opt.as_ref().and_then(|t| {
        agent_info_opt
            .as_ref()
            .map(|a| resolve_security_mission_encounter(ctx, t, a, mission_sys_id))
    });
    let handle = ctx.systems.get_or_spawn(mission_sys_id);
    let existing_accepted = ctx.db.with_snapshot(|snap| {
        snap.active_missions
            .get(&(char_id, mission_id))
            .filter(|m| m.state == "Accepted" && m.npcs_remaining > 0)
            .map(|m| (m.solar_system_id, m.spawned_npc_ball_ids.clone()))
    });
    let can_respawn_missing = if let Some((m_sys_id, prev_balls)) = existing_accepted {
        if m_sys_id == mission_sys_id && !prev_balls.is_empty() {
            let m_handle = ctx.systems.get_or_spawn(m_sys_id);
            let mut all_missing = true;
            for bid in prev_balls {
                if m_handle.get_ball_snapshot(bid).await.ok().flatten().is_some() {
                    all_missing = false;
                    break;
                }
            }
            all_missing
        } else {
            false
        }
    } else {
        false
    };

    let now = ctx.now_unix();
    let accept_dur = 604_800_u64;
    let bonus_dur =
        mission_bonus_window_secs(ctx, char_id, actual_agent_id, mission_id);

    let m_info = ctx.db.with_snapshot(|snap| {
        let m = snap.active_missions.get(&(char_id, mission_id))?;
        let meta = load_mission_meta(snap, char_id, mission_id)?;
        let is_expired = meta.offer_expiry_unix > 0 && now > meta.offer_expiry_unix;
        Some((m.state.clone(), m.solar_system_id, is_expired))
    });
    if let Some((state, sys, is_expired)) = m_info {
        if state == "Offered" {
            if is_expired {
                ctx.db.with_mut_snapshot(|snap| {
                    finalize_mission_state_cleanup(snap, char_id, mission_id, "Expired", false);
                });
                return Some(Err(BridgeRpcError { code: "MissionOfferExpired".into(), message: format!("Mission {mission_id} offer has expired"), client_error: None }));
            }
        } else if !(state == "Accepted" && sys == mission_sys_id && can_respawn_missing) {
            return Some(Err(BridgeRpcError { code: "InvalidMissionState".into(), message: format!("Mission {mission_id} is in state '{state}' (system {sys}), not Offered"), client_error: None }));
        }
    }

    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(m_check) = snap.active_missions.get(&(char_id, mission_id)) else {
            return Err(BridgeRpcError { code: "MissionNotOffered".into(), message: format!("Mission {mission_id} has not been offered to character {char_id}"), client_error: None });
        };
        if m_check.state != "Offered" && !(m_check.state == "Accepted" && m_check.solar_system_id == mission_sys_id && can_respawn_missing) {
            return Err(BridgeRpcError { code: "InvalidMissionState".into(), message: format!("Mission {mission_id} is in state '{}' (system {}), not Offered", m_check.state, m_check.solar_system_id), client_error: None });
        }
        let mut spawned_npcs: Vec<(i64, eve_db::TypeId)> = Vec::new();
        let mut need_npc = false;
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
            m.state = "Accepted".into();
            if m.solar_system_id == 0 {
                m.solar_system_id = mission_sys_id;
            }
            need_npc = m.npcs_remaining > 0;
        }
        if need_npc {
            let count = enc_opt.as_ref().map(|e| e.wave_npc_count).unwrap_or(1) as usize;
            let pool = enc_opt.as_ref().map(|e| e.npc_type_ids.as_slice()).unwrap_or(&[]);
            let mut ball_ids = Vec::with_capacity(count);
            for i in 0..count {
                let npc_id = snap.alloc_entity_id();
                let tid = if !pool.is_empty() { pool[i % pool.len()] } else { resolve_mission_npc_type(ctx) };
                ball_ids.push(npc_id);
                spawned_npcs.push((npc_id, tid));
            }
            if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                m.npcs_remaining = count as u32;
                m.spawned_npc_ball_ids = ball_ids;
            }
            let wave_tids: Vec<u32> = spawned_npcs.iter().map(|&(_, tid)| tid).collect();
            snap.sp6_lifecycle.mission_initial_npc_counts.insert(format!("{char_id}:{mission_id}"), count as u32);
            snap.sp6_lifecycle.mission_initial_npc_types.insert(format!("{char_id}:{mission_id}"), wave_tids);
        }
        let mut final_accept_expiry = now + accept_dur;
        let mut final_bonus_expiry = now + bonus_dur;
        if let Some(mut meta) = load_mission_meta(snap, char_id, mission_id) {
            meta.accepted_at_unix.get_or_insert(now);
            final_accept_expiry = *meta.accept_expiry_unix.get_or_insert(now + accept_dur);
            final_bonus_expiry = *meta.bonus_expiry_unix.get_or_insert(now + bonus_dur);
            let agent_opt = ctx.sde.agents.get(&meta.agent_id);
            let is_courier = classify_mission_archetype(
                tmpl_opt.as_ref(),
                Some(&meta),
                agent_opt,
            )
            .is_courier;
            if is_courier && meta.granted_courier_item_id.is_none() {
                let cargo_tid = meta.required_delivery_type_id.ok_or_else(|| BridgeRpcError { code: "InvalidMissionLocation".into(), message: format!("Courier mission {mission_id} has no delivery item type specified"), client_error: None })?;
                let cargo_qty = meta.required_delivery_qty.max(1);
                let agent_station = meta.origin_station_id
                    .or_else(|| ctx.sde.agents.get(&meta.agent_id).and_then(|a| a.station_id))
                    .or_else(|| ctx.sde.agents.get(&meta.agent_id).and_then(|a| a.solar_system_id).and_then(|sys| ctx.sde.stations.values().filter(|st| st.solar_system_id == sys).map(|st| st.station_id).min()))
                    .ok_or_else(|| BridgeRpcError { code: "InvalidMissionLocation".into(), message: "Agent has no valid station location".into(), client_error: None })?;
                snap.next_entity_id += 1;
                let cid = snap.next_entity_id;
                let item = InventoryItemRecord { item_id: cid, type_id: cargo_tid, owner_id: char_id, location_id: agent_station, flag: InventoryFlag::Hangar, quantity: cargo_qty, singleton: false, version: 1, ..Default::default() };
                snap.items.insert(cid, item);
                snap.item_custom_names.insert(cid, format!("courier_cargo:{char_id}:{mission_id}"));
                meta.granted_courier_item_id = Some(cid);
                meta.granted_courier_qty = cargo_qty;
            }
            save_mission_meta(snap, char_id, &meta);
        }
        Ok((spawned_npcs, final_accept_expiry, final_bonus_expiry))
    });
    let (spawned_npcs, expiry_unix, bonus_expiry_unix) = match res { Ok(v) => v, Err(e) => return Some(Err(e)) };
    let first_npc_id = spawned_npcs.first().map(|(id, _)| *id);

    let site_origin = deterministic_mission_site_origin(ctx, mission_sys_id, mission_id);
    if !spawned_npcs.is_empty() {
        for (idx, (npc_id, tid)) in spawned_npcs.into_iter().enumerate() {
            let pocket_offset = ((npc_id % 40) as f64) * 35.0 + (idx as f64) * 350.0;
            let pos = site_origin + DVec3::new(800.0 + pocket_offset, (idx as f64) * 120.0, 0.0);
            let (ball, cs, mut entity) = crate::services::wormhole_pve_seeding::build_specialized_npc_components(ctx, mission_sys_id, npc_id, tid, "Pirate", 1, pos);
            entity.bounty_isk = entity.bounty_isk.max(10_000.0);
            let _ = handle.spawn_ball_with_combat(ball, cs).await;
            crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                rt.npc_entities.insert(npc_id, entity);
            });
        }
    }

    let is_mining_mission = ctx.db.with_snapshot(|snap| {
        let meta_opt = load_mission_meta(snap, char_id, mission_id);
        let agent_opt = meta_opt.as_ref().and_then(|m| ctx.sde.agents.get(&m.agent_id));
        !agent_opt.is_some_and(is_rnd_agent)
            && ctx.sde.missions.get(&mission_id).is_some_and(|m| {
                let arch = classify_mission_archetype(Some(m), meta_opt.as_ref(), agent_opt);
                if arch.is_kill || arch.is_trade || arch.is_courier || arch.is_heraldry || arch.is_data_center {
                    return false;
                }
                let agent_div = agent_opt.map(|a| a.division_id).unwrap_or(0);
                let t = m.title.to_lowercase();
                let c_lower = m.content_template.as_deref().unwrap_or_default().to_lowercase();
                agent_div == 23 || c_lower.contains("mining") || t.contains("mining") || t.contains("mineral") || t.contains("ore")
            })
    });
    if is_mining_mission {
        let ore_tid = ctx.db.with_snapshot(|snap| load_mission_meta(snap, char_id, mission_id).and_then(|m| m.required_delivery_type_id).unwrap_or(1230));
        let ore_vol = ctx.sde.types.get(&ore_tid).map(|t| t.volume.max(0.01)).unwrap_or(0.1);
        let ast_id = 910_000_000 + (mission_id as i64) * 1000 + 1;
        let ast_pos = site_origin + DVec3::new(8_000.0, 1_000.0, -5_000.0);
        ctx.db.with_mut_snapshot(|snap| {
            snap.asteroids.insert(ast_id, eve_db::AsteroidRecord { asteroid_id: ast_id, solar_system_id: mission_sys_id, belt_id: 0, type_id: ore_tid, quantity: 5_000, initial_quantity: 5_000, position: (ast_pos.x, ast_pos.y, ast_pos.z), radius: 50.0 });
        });
        let ast_ball = Ball { id: ast_id, type_id: ore_tid, position: ast_pos, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 50_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
        let ast_state = crate::system_actor::AsteroidState { asteroid_id: ast_id, belt_id: 0, type_id: ore_tid, initial_units: 5_000, remaining_units: 5_000, unit_volume: ore_vol, depleted: false, respawn_at_tick: None };
        let _ = handle.spawn_asteroid(ast_state, ast_ball).await;
    }

    let mission_title = ctx.sde.missions.get(&mission_id).map(|m| m.title.clone()).unwrap_or_else(|| format!("Mission {mission_id}"));
    let bookmark_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let bmid = snap.next_entity_id;
        let b_sys = snap.active_missions.get(&(char_id, mission_id)).map(|m| m.solar_system_id).filter(|&s| s > 0).unwrap_or(mission_sys_id);
        snap.bookmarks.insert(bmid, eve_db::BookmarkRecord { bookmark_id: bmid, owner_id: char_id, solar_system_id: b_sys, label: format!("Mission: {mission_title}"), x: site_origin.x, y: site_origin.y, z: site_origin.z, item_id: None, memo: "Mission Pocket Destination".into(), folder_id: None, is_shared: false });
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
            m.bookmark_id = Some(bmid);
        }
        bmid
    });
    let dialogue = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::String("Complete Mission".into())]),
        PyRep::Tuple(vec![PyRep::Integer(11), PyRep::String("Quit Mission".into())]),
    ]);
    let (active_agent_id, active_sys_id, active_meta) = ctx.db.with_snapshot(|snap| {
        let am = snap.active_missions.get(&(char_id, mission_id));
        (
            am.map_or(agent_id, |m| m.agent_id),
            am.map(|m| m.solar_system_id)
                .filter(|&s| s > 0)
                .unwrap_or(mission_sys_id),
            load_mission_meta(snap, char_id, mission_id),
        )
    });
    let agent_type_id = ctx.sde.agents.get(&active_agent_id).map(|a| a.agent_type_id).unwrap_or(2);
    let mission_body = build_mission_briefing_text(
        ctx,
        active_agent_id,
        mission_id,
        &mission_title,
        active_sys_id,
        active_meta.as_ref(),
    );
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnAgentMissionChange {
            action: "offer_accepted".into(),
            agent_id: active_agent_id,
        },
    );
    emit_agent_mission_dialogue(
        ctx,
        char_id,
        active_agent_id,
        agent_type_id,
        &mission_title,
        &mission_body,
        false,
    );
    let accept_msg = format!(
        "Excellent. Your mission '{mission_title}' is now active in your journal. Return here once your objectives are complete."
    );
    Some(Ok(PyRep::keyval([
        ("missionID", PyRep::Integer(mission_id as i64)),
        ("contentID", PyRep::Integer(mission_id as i64)),
        ("agentID", PyRep::Integer(active_agent_id)),
        ("title", PyRep::String(mission_title)),
        ("state", PyRep::String("Accepted".into())),
        ("missionState", PyRep::Integer(2)),
        ("agentSays", PyRep::String(accept_msg.clone())),
        ("briefing", PyRep::String(accept_msg)),
        ("dialogue", dialogue),
        ("dungeonID", PyRep::None),
        ("currentRoomID", PyRep::Integer(1)),
        ("bookmarkID", PyRep::Integer(bookmark_id)),
        ("expiryUnix", PyRep::Integer(expiry_unix as i64)),
        ("bonusExpiryUnix", PyRep::Integer(bonus_expiry_unix as i64)),
        ("bonusWindowSecs", PyRep::Integer(bonus_dur as i64)),
        ("spawnedNpcBallID", first_npc_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
    ])))
}
