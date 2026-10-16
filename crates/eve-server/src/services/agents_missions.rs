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
            let tmpl = ctx.sde.missions.get(&mission_id).cloned();
            let agent_id = tmpl.as_ref().map(|m| m.agent_id).unwrap_or(3_018_924);
            let title = tmpl
                .as_ref()
                .map(|m| m.title.clone())
                .unwrap_or_else(|| "Stop the Thief".into());

            ctx.db.with_mut_snapshot(|snap| {
                snap.active_missions.insert(
                    (char_id, mission_id),
                    ActiveMissionRecord {
                        mission_id,
                        character_id: char_id,
                        agent_id,
                        state: "Offered".into(),
                        current_room_id: 1,
                        npcs_remaining: 3,
                    },
                );
            });

            Some(Ok(PyRep::keyval([
                ("missionID", PyRep::Integer(mission_id as i64)),
                ("agentID", PyRep::Integer(agent_id)),
                ("title", PyRep::String(title)),
                ("state", PyRep::String("Offered".into())),
            ])))
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
                        message: format!("Mission {mission_id} has not been offered to character {char_id}"),
                    });
                };
                if m.state != "Offered" {
                    return Err(BridgeRpcError {
                        code: "InvalidMissionState".into(),
                        message: format!("Mission {mission_id} is in state '{}', not Offered", m.state),
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

        ("dungeon", "ClearRoomAndActivateGate")
        | ("dungeonExplorationMgr", "ClearRoomAndActivateGate") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mission_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1001) as u32;
            let res = ctx.db.with_mut_snapshot(|snap| {
                let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) else {
                    return Err(BridgeRpcError {
                        code: "MissionNotActive".into(),
                        message: format!("Mission {mission_id} is not active for character {char_id}"),
                    });
                };
                if m.state != "Accepted" {
                    return Err(BridgeRpcError {
                        code: "InvalidMissionState".into(),
                        message: format!("Mission {mission_id} is in state '{}', not Accepted", m.state),
                    });
                }
                m.current_room_id += 1;
                m.npcs_remaining = 0;
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
            let tmpl = ctx.sde.missions.get(&mission_id).cloned();
            let base_isk = tmpl
                .as_ref()
                .map(|m| m.isk_reward + m.time_bonus_isk)
                .unwrap_or(270_000);
            let base_lp = tmpl.as_ref().map(|m| m.lp_reward).unwrap_or(180);
            let standing_pct = tmpl.as_ref().map(|m| m.standing_gain_pct).unwrap_or(1.25);
            let corp_id = tmpl.as_ref().map(|m| m.corporation_id).unwrap_or(1_000_035);

            let (isk_reward, lp_reward) =
                compute_mission_reward_with_skills(base_isk, base_lp, 4, 4, 1);

            let res = ctx.db.with_mut_snapshot(|snap| {
                let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) else {
                    return Err(BridgeRpcError {
                        code: "MissionNotActive".into(),
                        message: format!("Mission {mission_id} is not active for character {char_id}"),
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
                    ch.balance = IskCents(ch.balance.0 + IskCents::from_isk(isk_reward).0);
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

        // AIR NPE Node Graph `473` & AIR Career Program (`air_npe` / `careerPortal` / `goalMgr`, `E2E-16`)
        ("air_npe", "AdvanceNode") | ("air_npe", "CompleteCurrentNode") => {
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
                (npe.current_node_id, npe.completed)
            });
            Some(Ok(PyRep::keyval([
                ("nodeGraphID", PyRep::Integer(473)),
                ("currentNodeID", PyRep::Integer(curr_node as i64)),
                ("completed", PyRep::Bool(completed)),
            ])))
        }

        ("careerPortal", "RecordGoalProgress") | ("goalMgr", "ClaimGoalReward") => {
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
        | ("taleMgr", "GetTemplates")
        | ("incursionSvc", "GetActiveIncursions")
        | ("tutorialSvc", "GetTutorials")
        | ("dailyGoals", "GetDailyGoals") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::List(vec![])))
        }

        _ => None,
    }
}
