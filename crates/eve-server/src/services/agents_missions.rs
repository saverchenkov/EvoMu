//! `agents_missions` — Domain 9: Agent Dialogue, Missions, 16-Mission Storyline Tally, Epic Arcs, & AIR NPE (`§3.8`).

#![forbid(unsafe_code)]

use crate::formulas::{apply_standing_increase, compute_mission_reward_with_skills};
use crate::router::ServerContext;
use eve_db::{ActiveMissionRecord, IskCents, StandingTransactionRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

fn parse_mission_id(args: &[PyRep]) -> Result<u32, BridgeRpcError> {
    let raw = args.first().and_then(|a| a.as_i64()).unwrap_or(1001);
    u32::try_from(raw).map_err(|_| BridgeRpcError {
        code: "InvalidMissionId".into(),
        message: format!("mission_id {raw} is out of valid u32 range"),
    })
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("agentMgr", "RequestMission") | ("missionMgr", "RequestMission") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mission_id = match parse_mission_id(args) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(tmpl) = ctx.sde.missions.get(&mission_id).cloned() else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownMission".into(),
                    message: format!("Mission {mission_id} does not exist in SDE"),
                }));
            };
            let agent_id = tmpl.agent_id;
            let title = tmpl.title.clone();
            let is_storyline = tmpl.mission_type.starts_with("Storyline") || mission_id == 2001;
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if is_storyline {
                    let tally = snap.mission_tally.get(&char_id).copied().unwrap_or(0);
                    if tally < 16 {
                        return Err(BridgeRpcError {
                            code: "StorylineMissionLocked".into(),
                            message: format!(
                                "Storyline mission {mission_id} requires completing 16 regular missions first (current tally: {tally})"
                            ),
                        });
                    }
                }
                if let Some(existing) = snap.active_missions.get(&(char_id, mission_id)) {
                    return Err(BridgeRpcError {
                        code: "MissionAlreadyActiveOrCompleted".into(),
                        message: format!(
                            "Mission {mission_id} is already in state '{}' for character {char_id}",
                            existing.state
                        ),
                    });
                }
                snap.active_missions.insert(
                    (char_id, mission_id),
                    ActiveMissionRecord {
                        mission_id,
                        character_id: char_id,
                        agent_id,
                        solar_system_id: sys_id,
                        state: "Offered".into(),
                        current_room_id: 1,
                        npcs_remaining: if is_storyline { 0 } else { 1 },
                        spawned_npc_ball_ids: Vec::new(),
                    },
                );
                Ok(())
            });

            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("missionID", PyRep::Integer(mission_id as i64)),
                    ("agentID", PyRep::Integer(agent_id)),
                    ("title", PyRep::String(title)),
                    ("state", PyRep::String("Offered".into())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("agentMgr", "AcceptMission") | ("missionMgr", "AcceptMission") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mission_id = match parse_mission_id(args) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            let handle = ctx.systems.get_or_spawn(sys_id);
            let existing_accepted = ctx.db.with_snapshot(|snap| {
                snap.active_missions
                    .get(&(char_id, mission_id))
                    .filter(|m| m.state == "Accepted" && m.npcs_remaining > 0)
                    .map(|m| (m.solar_system_id, m.spawned_npc_ball_ids.clone()))
            });
            let can_respawn_missing = if let Some((m_sys_id, prev_balls)) = existing_accepted {
                if m_sys_id == sys_id && !prev_balls.is_empty() {
                    let m_handle = ctx.systems.get_or_spawn(m_sys_id);
                    let mut all_missing = true;
                    for bid in prev_balls {
                        if m_handle
                            .get_ball_snapshot(bid)
                            .await
                            .ok()
                            .flatten()
                            .is_some()
                        {
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
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(m_check) = snap.active_missions.get(&(char_id, mission_id)) else {
                    return Err(BridgeRpcError {
                        code: "MissionNotOffered".into(),
                        message: format!(
                            "Mission {mission_id} has not been offered to character {char_id}"
                        ),
                    });
                };
                if m_check.state != "Offered"
                    && !(m_check.state == "Accepted"
                        && m_check.solar_system_id == sys_id
                        && can_respawn_missing)
                {
                    return Err(BridgeRpcError {
                        code: "InvalidMissionState".into(),
                        message: format!(
                            "Mission {mission_id} is in state '{}' (system {}), not Offered",
                            m_check.state, m_check.solar_system_id
                        ),
                    });
                };
                let mut npc_id_opt = None;
                let mut need_npc = false;
                if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                    m.state = "Accepted".into();
                    m.solar_system_id = sys_id;
                    need_npc = m.npcs_remaining > 0;
                }
                if need_npc {
                    snap.next_entity_id += 1;
                    let npc_id = 700_000_000_i64.saturating_add(snap.next_entity_id);
                    if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
                        m.spawned_npc_ball_ids = vec![npc_id];
                    }
                    npc_id_opt = Some(npc_id);
                }
                Ok(npc_id_opt)
            });
            let npc_id_opt = match res {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
            };

            if let Some(npc_id) = npc_id_opt {
                // Materialize mission pocket pirate NPC ball (`npc_id`) in current solar system's Ballpark
                let origin = if let Some(ship_id) = session.ship_id {
                    handle
                        .get_ball_snapshot(ship_id)
                        .await
                        .ok()
                        .flatten()
                        .map(|(_, b)| b.position)
                        .unwrap_or_else(|| {
                            ctx.sde
                                .solar_systems
                                .get(&sys_id)
                                .and_then(|s| {
                                    s.stations
                                        .first()
                                        .map(|st| st.position + DVec3::new(1_500.0, 0.0, 0.0))
                                })
                                .unwrap_or(DVec3::ZERO)
                        })
                } else {
                    DVec3::ZERO
                };
                let pocket_offset = ((npc_id % 40) as f64) * 35.0;
                let _ = handle
                    .spawn_ball_with_combat(
                        Ball {
                            id: npc_id,
                            type_id: 11001, // Guristas Pithi Invader
                            position: origin + DVec3::new(800.0 + pocket_offset, 0.0, 0.0),
                            velocity: DVec3::ZERO,
                            max_velocity: 310.0,
                            inertia: 2.5,
                            mass: 1_000_000.0,
                            radius: 35.0,
                            speed_fraction: 0.0,
                            mode: MovementMode::Stop,
                        },
                        ctx.build_type_combat_state(11001),
                    )
                    .await;
            }

            Some(Ok(PyRep::keyval([
                ("missionID", PyRep::Integer(mission_id as i64)),
                ("state", PyRep::String("Accepted".into())),
                ("dungeonID", PyRep::Integer(5001)),
                ("currentRoomID", PyRep::Integer(1)),
                (
                    "spawnedNpcBallID",
                    npc_id_opt.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
            ])))
        }

        ("agentMgr", "RemoveOfferFromJournal") | ("agentMgr", "QuitMission") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mission_id = match parse_mission_id(args) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) else {
                    return Err(BridgeRpcError {
                        code: "MissionNotActive".into(),
                        message: format!(
                            "Mission {mission_id} is not active or offered for character {char_id}"
                        ),
                    });
                };
                if !matches!(m.state.as_str(), "Offered" | "Accepted") {
                    return Err(BridgeRpcError {
                        code: "MissionNotActive".into(),
                        message: format!(
                            "Mission {mission_id} is in terminal state '{}' and cannot be abandoned",
                            m.state
                        ),
                    });
                }
                let was_accepted = m.state == "Accepted";
                let sys_id = m.solar_system_id;
                m.state = if was_accepted {
                    "Quit".into()
                } else {
                    "Declined".into()
                };
                m.npcs_remaining = 0;
                let ball_ids = std::mem::take(&mut m.spawned_npc_ball_ids);
                if was_accepted {
                    let corp_id = ctx
                        .sde
                        .missions
                        .get(&mission_id)
                        .map(|def| def.corporation_id)
                        .unwrap_or(1_000_035);
                    let mut actual_delta = None;
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        let cur = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
                        let next = (cur - 0.1).max(-10.0);
                        ch.standings.insert(corp_id, next);
                        actual_delta = Some(if cur - 0.1 >= -10.0 { -0.1 } else { -10.0 - cur });
                    }
                    if let Some(delta) = actual_delta {
                        snap.next_entity_id += 1;
                        let ev_id = snap.next_entity_id;
                        snap.standing_transactions
                            .entry(char_id)
                            .or_default()
                            .push(StandingTransactionRecord {
                                event_id: ev_id,
                                from_id: corp_id,
                                to_id: char_id,
                                modification: delta,
                                event_type_id: 83,
                            });
                    }
                }
                Ok((sys_id, ball_ids, m.state.clone()))
            });
            match res {
                Ok((sys_id, ball_ids, new_state)) => {
                    if sys_id > 0 && !ball_ids.is_empty() {
                        let handle = ctx.systems.get_or_spawn(sys_id);
                        for bid in ball_ids {
                            let _ = handle.remove_ball(bid).await;
                        }
                    }
                    Some(Ok(PyRep::keyval([
                        ("missionID", PyRep::Integer(mission_id as i64)),
                        ("state", PyRep::String(new_state)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("keeper", "ActivateAccelerationGate") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot activate dungeon acceleration gate while docked".into(),
                }));
            }
            let mission_id = match parse_mission_id(args) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) else {
                    return Err(BridgeRpcError {
                        code: "MissionNotActive".into(),
                        message: format!(
                            "Mission {mission_id} is not active for character {char_id}"
                        ),
                    });
                };
                if m.state != "Accepted" {
                    return Err(BridgeRpcError {
                        code: "InvalidMissionState".into(),
                        message: format!(
                            "Mission {mission_id} is in state '{}', not Accepted",
                            m.state
                        ),
                    });
                }
                if m.npcs_remaining > 0 {
                    return Err(BridgeRpcError {
                        code: "RoomNpcsRemaining".into(),
                        message: format!(
                            "Cannot activate acceleration gate while {} room NPC(s) remain",
                            m.npcs_remaining
                        ),
                    });
                }
                m.current_room_id += 1;
                Ok(m.current_room_id)
            });
            match res {
                Ok(next_room) => Some(Ok(PyRep::keyval([
                    ("missionID", PyRep::Integer(mission_id as i64)),
                    ("currentRoomID", PyRep::Integer(next_room as i64)),
                    ("gateUnlocked", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("agentMgr", "CompleteMission") | ("missionMgr", "CompleteMission") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mission_id = match parse_mission_id(args) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(tmpl) = ctx.sde.missions.get(&mission_id).cloned() else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownMission".into(),
                    message: format!("Mission {mission_id} does not exist in SDE"),
                }));
            };
            let base_isk = tmpl.isk_reward + tmpl.time_bonus_isk;
            let base_lp = tmpl.lp_reward;
            let standing_pct = tmpl.standing_gain_pct;
            let corp_id = tmpl.corporation_id;
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);

            let (neg_lvl, sec_conn_lvl) = ctx.db.with_snapshot(|snap| {
                let sk = snap.skills.get(&char_id);
                let neg = sk
                    .and_then(|m| m.get(&3356))
                    .map(|s| s.level)
                    .unwrap_or(0);
                let sec = sk
                    .and_then(|m| m.get(&3893))
                    .map(|s| s.level)
                    .unwrap_or(0);
                (neg, sec)
            });
            let (isk_reward, lp_reward) =
                compute_mission_reward_with_skills(base_isk, base_lp, neg_lvl, sec_conn_lvl, 1);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) else {
                    return Err(BridgeRpcError {
                        code: "MissionNotActive".into(),
                        message: format!(
                            "Mission {mission_id} is not active for character {char_id}"
                        ),
                    });
                };
                if m.state != "Accepted" || m.npcs_remaining > 0 {
                    return Err(BridgeRpcError {
                        code: "MissionIncomplete".into(),
                        message: format!(
                            "Mission {mission_id} cannot be completed (state='{}', npcs_remaining={})",
                            m.state, m.npcs_remaining
                        ),
                    });
                }
                m.state = "Completed".into();

                let mut new_st = 2.0;
                let mut actual_delta = None;
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    let reward_cents = IskCents::from_isk(isk_reward);
                    ch.balance = ch.balance.checked_add(reward_cents).unwrap_or(ch.balance);
                    *ch.loyalty_points.entry(corp_id).or_insert(0) += lp_reward;
                    let cur_st = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
                    new_st = apply_standing_increase(cur_st, standing_pct);
                    ch.standings.insert(corp_id, new_st);
                    actual_delta = Some(new_st - cur_st);
                }
                if let Some(delta) = actual_delta {
                    snap.next_entity_id += 1;
                    let ev_id = snap.next_entity_id;
                    snap.standing_transactions
                        .entry(char_id)
                        .or_default()
                        .push(StandingTransactionRecord {
                            event_id: ev_id,
                            from_id: corp_id,
                            to_id: char_id,
                            modification: delta,
                            event_type_id: 85,
                        });
                }
                let count = snap.mission_tally.entry(char_id).or_insert(0);
                *count += 1;
                let t = *count;
                let storyline = t % 16 == 0;
                if storyline {
                    snap.active_missions.insert(
                        (char_id, 2001),
                        ActiveMissionRecord {
                            mission_id: 2001,
                            character_id: char_id,
                            agent_id: 3_018_925,
                            solar_system_id: sys_id,
                            state: "Offered".into(),
                            current_room_id: 1,
                            npcs_remaining: 0,
                            spawned_npc_ball_ids: Vec::new(),
                        },
                    );
                }
                Ok((new_st, t, storyline))
            });

            match res {
                Ok((new_standing, tally, storyline_offered)) => Some(Ok(PyRep::keyval([
                    ("missionID", PyRep::Integer(mission_id as i64)),
                    ("iskReward", PyRep::Integer(isk_reward)),
                    ("lpReward", PyRep::Integer(lp_reward)),
                    ("newStanding", PyRep::Real(new_standing)),
                    ("missionTally", PyRep::Integer(tally as i64)),
                    ("storylineTriggered", PyRep::Bool(storyline_offered)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        // AIR NPE Node Graph `473` & AIR Career Program (`air_npe` / `seasonManager`, `E2E-16`)
        ("air_npe", "AdvanceNode") | ("air_npe", "CompleteCurrentNode") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let npe = snap.npe_progress.entry(char_id).or_insert_with(|| {
                    eve_db::NpeProgressRecord {
                        character_id: char_id,
                        node_graph_id: 473,
                        current_node_id: 1,
                        completed: false,
                        career_goal_progress: Default::default(),
                        claimed_goals: Default::default(),
                    }
                });
                if npe.completed {
                    return Err(BridgeRpcError {
                        code: "NodeGraphAlreadyCompleted".into(),
                        message: "AIR NPE node graph 473 is already completed".into(),
                    });
                }
                // Node 3 ("DestroySeekers") requires at least 1 NPC kill recorded in career_goal_progress[1]
                if npe.current_node_id == 3
                    && npe.career_goal_progress.get(&1).copied().unwrap_or(0) < 1
                {
                    return Err(BridgeRpcError {
                        code: "NodeObjectiveNotMet".into(),
                        message:
                            "Node 3 (DestroySeekers) requires destroying at least 1 NPC before advancing"
                                .into(),
                    });
                }
                if npe.current_node_id < 4 {
                    npe.current_node_id += 1;
                }
                if npe.current_node_id >= 4 {
                    npe.completed = true;
                }
                Ok((npe.current_node_id, npe.completed))
            });
            match res {
                Ok((curr_node, completed)) => Some(Ok(PyRep::keyval([
                    ("nodeGraphID", PyRep::Integer(473)),
                    ("currentNodeID", PyRep::Integer(curr_node as i64)),
                    ("completed", PyRep::Bool(completed)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("air_npe", "get_air_npe_state") | ("air_npe", "is_air_npe_enabled") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (curr_node, completed) = ctx.db.with_snapshot(|snap| {
                snap.npe_progress
                    .get(&char_id)
                    .map(|n| (n.current_node_id, n.completed))
                    .unwrap_or((1, false))
            });
            Some(Ok(PyRep::keyval([
                ("nodeGraphID", PyRep::Integer(473)),
                ("currentNodeID", PyRep::Integer(curr_node as i64)),
                ("completed", PyRep::Bool(completed)),
                ("enabled", PyRep::Bool(true)),
            ])))
        }

        ("seasonManager", "get_character_progress_by_character_id") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let goal_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1) as u32;
            if !ctx.sde.career_goals.contains_key(&goal_id) {
                return Some(Err(BridgeRpcError {
                    code: "UnknownCareerGoal".into(),
                    message: format!("Career goal {goal_id} does not exist in SDE"),
                }));
            }
            let prog = ctx.db.with_snapshot(|snap| {
                snap.npe_progress
                    .get(&char_id)
                    .and_then(|n| n.career_goal_progress.get(&goal_id).copied())
                    .unwrap_or(0)
            });
            Some(Ok(PyRep::keyval([
                ("goalID", PyRep::Integer(goal_id as i64)),
                ("currentProgress", PyRep::Integer(prog as i64)),
            ])))
        }

        ("seasonManager", "claim_goal_reward") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let goal_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1) as u32;
            let Some(goal_def) = ctx.sde.career_goals.get(&goal_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownCareerGoal".into(),
                    message: format!("Career goal {goal_id} does not exist in SDE"),
                }));
            };
            let sp_reward = goal_def.sp_reward as f64;
            let target_prog = goal_def.target_progress as u64;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let npe = snap.npe_progress.entry(char_id).or_insert_with(|| {
                    eve_db::NpeProgressRecord {
                        character_id: char_id,
                        node_graph_id: 473,
                        current_node_id: 1,
                        completed: false,
                        career_goal_progress: Default::default(),
                        claimed_goals: Default::default(),
                    }
                });
                let cur_prog = npe.career_goal_progress.get(&goal_id).copied().unwrap_or(0);
                if cur_prog < target_prog {
                    return Err(BridgeRpcError {
                        code: "GoalProgressIncomplete".into(),
                        message: format!(
                            "Career goal {goal_id} requires progress {target_prog}, current is {cur_prog}"
                        ),
                    });
                }
                if !npe.claimed_goals.insert(goal_id) {
                    return Err(BridgeRpcError {
                        code: "GoalAlreadyClaimed".into(),
                        message: format!("Career goal {goal_id} has already been claimed"),
                    });
                }
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.free_skill_points += sp_reward;
                    Ok(ch.free_skill_points)
                } else {
                    Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    })
                }
            });

            match res {
                Ok(new_free_sp) => Some(Ok(PyRep::keyval([
                    ("goalID", PyRep::Integer(goal_id as i64)),
                    ("spReward", PyRep::Real(sp_reward)),
                    ("freeSkillPoints", PyRep::Real(new_free_sp)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}
