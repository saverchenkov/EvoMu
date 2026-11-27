//! `agents_missions_accept` — `agentMgr.DoAction` mission accept arms (`3..=5`).

#![forbid(unsafe_code)]

use super::{
    build_mission_briefing_text, finalize_mission_state_cleanup, is_agent_colocated,
    is_storyline_agent_type, is_storyline_template, mission_bonus_window_secs,
    mission_site_origin_for_instance, resolve_mission_npc_type,
    resolve_security_mission_encounter, resolve_target_mission_id,
};
use crate::router::notify_registry::OnAgentMissionChange;
use crate::router::{NotificationTarget, ServerContext};
use crate::services::notify_emitters_character_services::emit_agent_mission_dialogue;
use eve_db::{InventoryFlag, InventoryItemRecord, MissionKind, MissionState};
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
    let mission_id = match resolve_target_mission_id(ctx, char_id, lookup_agent_id, Some(MissionState::Offered), args, _kwargs) { Ok(v) => v, Err(e) => return Some(Err(e)) };
    let (is_alpha, agent_lvl, actual_agent_id, needs_encounter_wiki_check) =
        ctx.db.with_snapshot(|snap| {
            let alpha = snap
                .characters
                .get(&char_id)
                .is_some_and(|c| c.clone_grade <= 1);
            let m_opt = snap.missions.get_by_character_template(char_id, mission_id);
            let mid_agent = m_opt.map(|m| m.agent_id).unwrap_or(agent_id);
            let lvl = ctx.sde.agents.get(&mid_agent).map_or(1, |a| a.level);
            let is_enc = m_opt.is_some_and(|m| {
                m.kind == MissionKind::Encounter && m.npcs_remaining() > 0
            });
            (alpha, lvl, mid_agent, is_enc)
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
                snap.missions
                    .get_by_character_template(char_id, mission_id)
                    .is_some_and(|m| m.is_anomic_burner)
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
            snap.missions
                .get_by_character_template(char_id, mission_id)
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

    let now = ctx.now_unix();
    let accept_dur = 604_800_u64;
    let bonus_dur =
        mission_bonus_window_secs(ctx, char_id, actual_agent_id, mission_id);
    let mission_title = tmpl_opt
        .as_ref()
        .map(|m| m.title.clone())
        .unwrap_or_else(|| format!("Mission {mission_id}"));
    let wiki_dungeon_initial_npcs = if needs_encounter_wiki_check {
        tmpl_opt
            .as_ref()
            .and_then(|t| t.dungeon_id)
            .filter(|&d| d > 0)
            .and_then(|sde_did| {
                super::agents_missions_dungeon::count_wiki_dungeon_initial_npcs(
                    ctx,
                    mission_sys_id,
                    sde_did,
                    &mission_title,
                )
            })
    } else {
        None
    };

    let expired_sys_cleanup = ctx.db.with_snapshot(|snap| {
        let m = snap.missions.get_by_character_template(char_id, mission_id)?;
        (m.state == MissionState::Offered
            && m.offer_expiry_unix > 0
            && now > m.offer_expiry_unix)
        .then_some(m.solar_system_id)
    });
    if expired_sys_cleanup.is_some() {
        let (clean_sys, ast_id, ball_ids) = ctx.db.with_mut_snapshot(|snap| {
            finalize_mission_state_cleanup(snap, char_id, mission_id, MissionState::Expired, false, now)
        });
        super::agents_missions_dungeon::despawn_mission_cleanup_balls(
            ctx, clean_sys, ast_id, &ball_ids,
        )
        .await;
        return Some(Err(BridgeRpcError {
            code: "MissionOfferExpired".into(),
            message: format!("Mission {mission_id} offer has expired"),
            client_error: None,
        }));
    }

    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(m_check) = snap.missions.get_by_character_template(char_id, mission_id) else {
            if let Some(hist) = snap
                .missions
                .latest_history_for_character_template(char_id, mission_id)
            {
                let msg = format!(
                    "Mission {mission_id} is in terminal state '{}', not Offered",
                    hist.final_state.as_str()
                );
                return Err(BridgeRpcError::with_client_error(
                    "InvalidMissionState",
                    msg.clone(),
                    eve_proto::ClientError::CustomInfo(msg),
                ));
            }
            return Err(BridgeRpcError {
                code: "MissionNotOffered".into(),
                message: format!("Mission {mission_id} has not been offered to character {char_id}"),
                client_error: None,
            });
        };
        if m_check.state != MissionState::Offered {
            return Err(BridgeRpcError {
                code: "InvalidMissionState".into(),
                message: format!(
                    "Mission {mission_id} is in state '{}' (system {}), not Offered",
                    m_check.state.as_str(),
                    m_check.solar_system_id
                ),
                client_error: None,
            });
        }
        if m_check.offer_expiry_unix > 0 && now > m_check.offer_expiry_unix {
            return Err(BridgeRpcError {
                code: "MissionOfferExpired".into(),
                message: format!("Mission {mission_id} offer has expired"),
                client_error: None,
            });
        }

        let m_sys = if m_check.solar_system_id > 0 {
            m_check.solar_system_id
        } else {
            mission_sys_id
        };
        let raw_origin = mission_site_origin_for_instance(ctx, m_sys, m_check.instance_id);
        let origin_m = [
            raw_origin.x.round() as i64,
            raw_origin.y.round() as i64,
            raw_origin.z.round() as i64,
        ];
        let site_origin = DVec3::new(origin_m[0] as f64, origin_m[1] as f64, origin_m[2] as f64);

        let need_npc = m_check.npcs_remaining() > 0;
        let is_courier = m_check.kind == MissionKind::Courier;
        let is_mining = m_check.kind == MissionKind::Mining;
        let mining_ore_tid = is_mining
            .then(|| m_check.required_delivery().map_or(1230, |(tid, _)| tid));
        let courier_obj = m_check.courier().cloned();
        let m_agent_id = m_check.agent_id;
        let m_origin_station_id = m_check.origin_station_id;

        let mut wave_tids = Vec::new();
        let mut wave_count = 0_u32;
        if need_npc {
            if let Some(wiki_cnt) = wiki_dungeon_initial_npcs {
                wave_count = wiki_cnt;
            } else {
                let count = enc_opt.as_ref().map(|e| e.wave_npc_count).unwrap_or(1) as usize;
                let pool = enc_opt.as_ref().map(|e| e.npc_type_ids.as_slice()).unwrap_or(&[]);
                wave_tids.reserve(count);
                for i in 0..count {
                    let tid = if !pool.is_empty() {
                        pool[i % pool.len()]
                    } else {
                        resolve_mission_npc_type(ctx)
                    };
                    wave_tids.push(tid);
                }
                wave_count = count as u32;
            }
        }

        let mut new_courier_grant: Option<(i64, i32)> = None;
        if is_courier {
            if let Some(ref co) = courier_obj {
                if co.granted_item_id.is_none() {
                    let cargo_tid = co.cargo_type_id.ok_or_else(|| BridgeRpcError {
                        code: "InvalidMissionLocation".into(),
                        message: format!(
                            "Courier mission {mission_id} has no delivery item type specified"
                        ),
                        client_error: None,
                    })?;
                    let cargo_qty = co.quantity.max(1);
                    let agent_station = m_origin_station_id
                        .or_else(|| ctx.sde.agents.get(&m_agent_id).and_then(|a| a.station_id))
                        .or_else(|| {
                            ctx.sde
                                .agents
                                .get(&m_agent_id)
                                .and_then(|a| a.solar_system_id)
                                .and_then(|sys| {
                                    ctx.sde
                                        .stations
                                        .values()
                                        .filter(|st| st.solar_system_id == sys)
                                        .map(|st| st.station_id)
                                        .min()
                                })
                        })
                        .ok_or_else(|| BridgeRpcError {
                            code: "InvalidMissionLocation".into(),
                            message: "Agent has no valid station location".into(),
                            client_error: None,
                        })?;
                    let cid = snap.alloc_entity_id();
                    let item = InventoryItemRecord {
                        item_id: cid,
                        type_id: cargo_tid,
                        owner_id: char_id,
                        location_id: agent_station,
                        flag: InventoryFlag::Hangar,
                        quantity: cargo_qty,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    };
                    snap.items.insert(cid, item);
                    snap.item_custom_names
                        .insert(cid, format!("courier_cargo:{char_id}:{mission_id}"));
                    new_courier_grant = Some((cid, cargo_qty));
                }
            }
        }

        let allocated_ast_id = if let Some(ore_tid) = mining_ore_tid {
            let ast_id = snap.alloc_entity_id();
            let ast_pos = site_origin + DVec3::new(8_000.0, 1_000.0, -5_000.0);
            snap.asteroids.insert(
                ast_id,
                eve_db::AsteroidRecord {
                    asteroid_id: ast_id,
                    solar_system_id: m_sys,
                    belt_id: 0,
                    type_id: ore_tid,
                    quantity: 5_000,
                    initial_quantity: 5_000,
                    position: (ast_pos.x, ast_pos.y, ast_pos.z),
                    radius: 50.0,
                },
            );
            Some(ast_id)
        } else {
            None
        };

        let bookmark_id = snap.alloc_entity_id();
        snap.bookmarks.insert(
            bookmark_id,
            eve_db::BookmarkRecord {
                bookmark_id,
                owner_id: char_id,
                solar_system_id: m_sys,
                label: format!("Mission: {mission_title}"),
                x: site_origin.x,
                y: site_origin.y,
                z: site_origin.z,
                item_id: None,
                memo: "Mission Pocket Destination".into(),
                folder_id: None,
                is_shared: false,
            },
        );

        let mut final_accept_expiry = now + accept_dur;
        let mut final_bonus_expiry = now + bonus_dur;
        snap.missions
            .update_by_character_template(char_id, mission_id, |m| {
                m.state = MissionState::Accepted;
                if m.solar_system_id == 0 {
                    m.solar_system_id = m_sys;
                }
                m.site_origin_m = Some(origin_m);
                m.bookmark_id = Some(bookmark_id);
                if let Some(ast_id) = allocated_ast_id {
                    m.asteroid_entity_id = Some(ast_id);
                }
                m.accepted_at_unix.get_or_insert(now);
                final_accept_expiry = *m.accept_expiry_unix.get_or_insert(now + accept_dur);
                final_bonus_expiry = *m.bonus_expiry_unix.get_or_insert(now + bonus_dur);
                if need_npc {
                    if let Some(enc) = m.encounter_mut() {
                        enc.npcs_remaining = wave_count;
                        enc.spawned_npc_ball_ids.clear();
                        enc.initial_wave_count = Some(wave_count);
                        enc.initial_wave_types = wave_tids;
                    }
                }
                if let Some((cid, cargo_qty)) = new_courier_grant {
                    if let Some(co) = m.courier_mut() {
                        co.granted_item_id = Some(cid);
                        co.granted_qty = cargo_qty;
                    }
                }
            })
            .map_err(super::mission_store_rpc_err)?;

        let am = snap.missions.get_by_character_template(char_id, mission_id);
        let active_agent_id = am.map_or(agent_id, |m| m.agent_id);
        let active_sys_id = am
            .map(|m| m.solar_system_id)
            .filter(|&s| s > 0)
            .unwrap_or(m_sys);
        let mission_body = build_mission_briefing_text(
            ctx,
            active_agent_id,
            mission_id,
            &mission_title,
            active_sys_id,
            am,
        );
        Ok((
            final_accept_expiry,
            final_bonus_expiry,
            bookmark_id,
            allocated_ast_id,
            active_agent_id,
            mission_body,
        ))
    });
    let (
        expiry_unix,
        bonus_expiry_unix,
        bookmark_id,
        allocated_ast_id,
        active_agent_id,
        mission_body,
    ) = match res {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };

    let dialogue = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::String("Complete Mission".into())]),
        PyRep::Tuple(vec![PyRep::Integer(11), PyRep::String("Quit Mission".into())]),
    ]);
    let agent_type_id = ctx.sde.agents.get(&active_agent_id).map(|a| a.agent_type_id).unwrap_or(2);
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
        ("state", PyRep::String(MissionState::Accepted.as_str().into())),
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
        ("spawnedNpcBallID", PyRep::None),
        (
            "asteroidEntityID",
            allocated_ast_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        ),
    ])))
}
