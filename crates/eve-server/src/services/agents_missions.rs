//! `agents_missions` — Domain 9: Agent Dialogue, Missions, 16-Mission Storyline Tally, Epic Arcs, & AIR NPE (`§3.8`).

#![forbid(unsafe_code)]

use crate::formulas::{apply_standing_increase, compute_mission_reward_with_skills};
use crate::router::ServerContext;
use eve_db::{ActiveMissionRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub fn handle(
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
            let mission_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1001) as u32;
            let Some(tmpl) = ctx.sde.missions.get(&mission_id).cloned() else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownMission".into(),
                    message: format!("Mission {mission_id} does not exist in SDE"),
                }));
            };
            let agent_id = tmpl.agent_id;
            let title = tmpl.title.clone();

            let res = ctx.db.with_mut_snapshot(|snap| {
                if let Some(existing) = snap.active_missions.get(&(char_id, mission_id)) {
                    if existing.state == "Offered" || existing.state == "Accepted" {
                        return Err(BridgeRpcError {
                            code: "MissionAlreadyActive".into(),
                            message: format!(
                                "Mission {mission_id} is already in state '{}' for character {char_id}",
                                existing.state
                            ),
                        });
                    }
                }
                snap.active_missions.insert(
                    (char_id, mission_id),
                    ActiveMissionRecord {
                        mission_id,
                        character_id: char_id,
                        agent_id,
                        state: "Offered".into(),
                        current_room_id: 1,
                        npcs_remaining: 1,
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
            let mission_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1001) as u32;
            let res = ctx.db.with_mut_snapshot(|snap| {
                let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) else {
                    return Err(BridgeRpcError {
                        code: "MissionNotOffered".into(),
                        message: format!(
                            "Mission {mission_id} has not been offered to character {char_id}"
                        ),
                    });
                };
                if m.state != "Offered" {
                    return Err(BridgeRpcError {
                        code: "InvalidMissionState".into(),
                        message: format!(
                            "Mission {mission_id} is in state '{}', not Offered",
                            m.state
                        ),
                    });
                }
                m.state = "Accepted".into();
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("missionID", PyRep::Integer(mission_id as i64)),
                    ("state", PyRep::String("Accepted".into())),
                    ("dungeonID", PyRep::Integer(5001)),
                    ("currentRoomID", PyRep::Integer(1)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("keeper", "ActivateAccelerationGate")
        | ("keeper", "GetCurrentDungeonForCharacter") => {
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
            let mission_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1001) as u32;
            let res = ctx.db.with_mut_snapshot(|snap| {
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
            let mission_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1001) as u32;
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

            let (isk_reward, lp_reward) =
                compute_mission_reward_with_skills(base_isk, base_lp, 4, 4, 1);

            let res = ctx.db.with_mut_snapshot(|snap| {
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
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    let reward_cents = IskCents::from_isk(isk_reward);
                    ch.balance = ch.balance.checked_add(reward_cents).unwrap_or(ch.balance);
                    *ch.loyalty_points.entry(corp_id).or_insert(0) += lp_reward;
                    let cur_st = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
                    new_st = apply_standing_increase(cur_st, standing_pct);
                    ch.standings.insert(corp_id, new_st);
                }
                let count = snap.mission_tally.entry(char_id).or_insert(0);
                *count += 1;
                let t = *count;
                let storyline = t % 16 == 0;
                if storyline {
                    snap.active_missions.insert(
                        (char_id, 1002),
                        ActiveMissionRecord {
                            mission_id: 1002,
                            character_id: char_id,
                            agent_id: 3_018_925,
                            state: "Offered".into(),
                            current_room_id: 1,
                            npcs_remaining: 0,
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

        // AIR NPE Node Graph `473` & AIR Career Program (`air_npe` / `seasonManager` / `milestoneMgr`, `E2E-16`)
        ("air_npe", "AdvanceNode")
        | ("air_npe", "CompleteCurrentNode")
        | ("air_npe", "get_air_npe_state")
        | ("air_npe", "is_air_npe_enabled")
        | ("air_npe", "skip_air_npe")
        | ("operationsManager", "complete_category_for_character") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (curr_node, completed) = ctx.db.with_mut_snapshot(|snap| {
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
                if npe.current_node_id < 4 {
                    npe.current_node_id += 1;
                }
                if npe.current_node_id >= 4 {
                    npe.completed = true;
                }
                *npe.career_goal_progress.entry(1).or_insert(0) += 4;
                *npe.career_goal_progress.entry(2).or_insert(0) += 4;
                (npe.current_node_id, npe.completed)
            });
            Some(Ok(PyRep::keyval([
                ("nodeGraphID", PyRep::Integer(473)),
                ("currentNodeID", PyRep::Integer(curr_node as i64)),
                ("completed", PyRep::Bool(completed)),
            ])))
        }

        ("seasonManager", "ClaimGoalReward")
        | ("seasonManager", "claim_goal_reward")
        | ("seasonManager", "claim_challenge_rewards")
        | ("seasonManager", "RecordGoalProgress")
        | ("milestoneMgr", "ClaimRewards") => {
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
            if method == "RecordGoalProgress" {
                let delta = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1).max(1) as u64;
                let prog = ctx.db.with_mut_snapshot(|snap| {
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
                    let p = npe.career_goal_progress.entry(goal_id).or_insert(0);
                    *p = p.saturating_add(delta);
                    *p
                });
                return Some(Ok(PyRep::keyval([
                    ("goalID", PyRep::Integer(goal_id as i64)),
                    ("currentProgress", PyRep::Integer(prog as i64)),
                ])));
            }

            let sp_reward = goal_def.sp_reward as f64;
            let target_prog = goal_def.target_progress as u64;

            let res = ctx.db.with_mut_snapshot(|snap| {
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

        ("agentMgr", "GetAgents")
        | ("agentMgr", "GetMyJournalDetails")
        | ("agentMgr", "GetMyEpicArcStatus")
        | ("epicArcStatus", "AgentHasEpicMissionsForCharacter")
        | ("missionTrackerMgr", "GetAllMissionObjectives")
        | ("tutorialSvc", "GetTutorials") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.active_missions
                    .values()
                    .filter(|m| m.character_id == char_id)
                    .map(|m| {
                        vec![
                            PyRep::Integer(m.mission_id as i64),
                            PyRep::Integer(m.agent_id),
                            PyRep::String(m.state.clone()),
                            PyRep::Integer(m.current_room_id as i64),
                            PyRep::Integer(m.npcs_remaining as i64),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec![
                    "missionID",
                    "agentID",
                    "state",
                    "currentRoomID",
                    "npcsRemaining",
                ],
                rows,
            )))
        }

        _ => None,
    }
}
