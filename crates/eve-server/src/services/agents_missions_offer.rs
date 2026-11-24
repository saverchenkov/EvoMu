//! `agents_missions_offer` — `agentMgr.DoAction` / `GetOffer` mission offer arms (`1 | 2 | 8`).

#![forbid(unsafe_code)]

use super::{
    build_mission_briefing_text, classify_mission_archetype, compute_character_mission_rewards,
    compute_effective_agent_standing, is_fw_agent, is_rnd_agent, is_storyline_agent_type,
    is_unsupported_direct_agent_type, load_epic_arc_states, load_mission_meta,
    load_rnd_research_entries, mission_bonus_window_secs, mission_matches_agent_pool_with_sde,
    resolve_courier_mission_details, resolve_epic_arc_id_for_agent, resolve_fw_warzone_system,
    resolve_mining_mission_details, resolve_security_mission_encounter,
    resolve_trade_mission_details, rnd_tritanium_required_qty, save_mission_meta,
    CharacterMissionRewards, MissionTimingAndCargoMeta,
};
use crate::router::notify_registry::{OnAgentMissionChange, OnIncomingTransmission};
use crate::router::{NotificationTarget, ServerContext};
use eve_db::ActiveMissionRecord;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub(super) async fn handle_mission_offer_action(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    sys_id: u32,
    mut agent_id: i64,
    mut explicit_agent: bool,
    bound_agent_id: Option<i64>,
    is_implicit_greeting: bool,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let supplied_id = if args
        .first().and_then(|a| a.as_i64())
        .is_some_and(|v| (1..=19).contains(&v))
    {
        args.get(1).and_then(|a| a.as_i64())
    } else {
        args.first()
            .and_then(|a| a.as_i64())
            .or_else(|| args.get(1).and_then(|a| a.as_i64()))
            .or_else(|| _kwargs.get("agentID").and_then(|a| a.as_i64()))
    };
    if let Some(sid) = supplied_id.filter(|_| bound_agent_id.is_none()) {
        agent_id = sid;
        explicit_agent = true;
    }
    let Some(agent_info) = ctx.sde.agents.get(&agent_id) else {
        return Some(Err(BridgeRpcError { code: "UnknownAgent".into(), message: format!("Agent {agent_id} does not exist in SDE"), client_error: None }));
    };
    if is_implicit_greeting {
        let existing_accepted = ctx.db.with_snapshot(|snap| {
            snap.active_missions
                .values()
                .find(|m| {
                    m.character_id == char_id
                        && m.agent_id == agent_id
                        && m.state == "Accepted"
                })
                .cloned()
        });
        if let Some(acc) = existing_accepted {
            let mid = acc.mission_id;
            let title = ctx
                .sde
                .missions
                .get(&mid)
                .map(|t| t.title.clone())
                .unwrap_or_else(|| format!("Mission {mid}"));
            let msg = format!(
                "Your assignment '{title}' is currently in progress. Return here once your objectives are complete."
            );
            let mut dlg = vec![
                PyRep::Tuple(vec![
                    PyRep::Integer(6),
                    PyRep::String("Complete Mission".into()),
                ]),
                PyRep::Tuple(vec![
                    PyRep::Integer(11),
                    PyRep::String("Quit Mission".into()),
                ]),
            ];
            if agent_info.is_locator {
                dlg.push(PyRep::Tuple(vec![
                    PyRep::Integer(15),
                    PyRep::String("Locate Character".into()),
                ]));
            }
            return Some(Ok(PyRep::keyval([
                ("missionID", PyRep::Integer(mid as i64)),
                ("contentID", PyRep::Integer(mid as i64)),
                ("agentID", PyRep::Integer(agent_id)),
                ("title", PyRep::String(title)),
                ("state", PyRep::String("Accepted".into())),
                ("missionState", PyRep::Integer(2)),
                ("briefing", PyRep::String(msg.clone())),
                ("agentSays", PyRep::String(msg)),
                ("dialogue", PyRep::List(dlg)),
            ])));
        }
    }

    let station_has_local_agents = session.station_id.is_some_and(|sid| {
        ctx.sde.agents.values().any(|a| {
            a.solar_system_id == Some(sys_id)
                && a.station_id == Some(sid)
                && !is_storyline_agent_type(a.agent_type_id)
                && !is_unsupported_direct_agent_type(a.agent_type_id)
        })
    });
    if agent_info.solar_system_id != Some(sys_id)
        || (session.station_id.is_some()
            && (explicit_agent || station_has_local_agents)
            && agent_info.station_id.is_some()
            && agent_info.station_id != session.station_id)
    {
        let sys_label = agent_info
            .solar_system_id
            .map(|sid| {
                ctx.sde
                    .solar_systems
                    .get(&sid)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| sid.to_string())
            })
            .unwrap_or_else(|| "unknown system".to_string());
        let station_label = agent_info
            .station_id
            .map(|stid| {
                ctx.sde
                    .stations
                    .get(&stid)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| stid.to_string())
            })
            .unwrap_or_else(|| "space".to_string());
        if is_implicit_greeting {
            let msg = format!(
                "Please dock at my station in {sys_label} ({station_label}) if you wish to request a new assignment."
            );
            return Some(Ok(PyRep::keyval([
                ("agentID", PyRep::Integer(agent_id)),
                ("state", PyRep::String("NotCoLocated".into())),
                ("missionState", PyRep::Integer(0)),
                ("briefing", PyRep::String(msg.clone())),
                ("agentSays", PyRep::String(msg)),
                ("dialogue", PyRep::List(Vec::new())),
            ])));
        }
        return Some(Err(BridgeRpcError { code: "AgentNotCoLocated".into(), message: format!("Agent {agent_id} is in system {sys_label} / station {station_label}, not character's current location (system {sys_id}, station {:?})", session.station_id), client_error: None }));
    }
    if is_storyline_agent_type(agent_info.agent_type_id) {
        return Some(Err(BridgeRpcError { code: "StorylineAgentRequiresReferral".into(), message: format!("Storyline agent {agent_id} (agentType {}) cannot be requested directly without a 16-mission referral", agent_info.agent_type_id), client_error: None }));
    }
    // V24 parity (`agents.pyj:OpenDialogueWindow`): `agentTypeAura = 11` rejects tutorial agent
    // dialogue with `NewNPEClickOnTutorialLinkDisabled`; locked as `UnimplementedRPC` by
    // `auth_persistence` and `agents_missions_extra_unit_tests` (#1204).
    if agent_info.agent_type_id == 11 {
        return Some(Err(BridgeRpcError { code: "UnimplementedRPC".into(), message: format!("Agent {agent_id} (agentType {}) uses Aura node-graph missions which are not yet implemented", agent_info.agent_type_id), client_error: None }));
    }
    let now = ctx.now_unix();
    if agent_info.agent_type_id == 10 {
        let (arc_id, _) = resolve_epic_arc_id_for_agent(ctx, agent_id);
        let on_cd = ctx.db.with_snapshot(|snap| {
            load_epic_arc_states(snap, char_id).iter().any(|a| {
                a.epic_arc_id == arc_id
                    && a.cooldown_until_unix.is_some_and(|until| now < until)
            })
        });
        if on_cd {
            return Some(Err(BridgeRpcError { code: "EpicArcOnCooldown".into(), message: format!("Epic Arc {arc_id} (agent {agent_id}) is on 90-day repeat cooldown"), client_error: None }));
        }
    }
    if agent_info.agent_type_id == 9 {
        let req_fac = ctx.sde.factions.values().find(|f| f.militia_corporation_id == Some(agent_info.corporation_id)).map(|f| f.faction_id as i64);
        let ch_fac = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).and_then(|c| c.war_faction_id));
        if req_fac.is_some() && ch_fac != req_fac {
            return Some(Err(BridgeRpcError { code: "NotEnlistedInMilitia".into(), message: format!("FW agent {agent_id} requires enlistment in militia faction {req_fac:?} (have {ch_fac:?})"), client_error: None }));
        }
    }
    if matches!(agent_info.agent_type_id, 5 | 8 | 12) {
        let already_done = ctx.db.with_snapshot(|snap| {
            snap.active_missions.values().any(|m| {
                m.character_id == char_id
                    && m.agent_id == agent_id
                    && m.state == "Completed"
            })
        });
        if already_done {
            return Some(Err(BridgeRpcError { code: "OneTimeAgentMissionCompleted".into(), message: format!("One-time COSMOS / Data Center / Career agent {agent_id} mission has already been completed"), client_error: None }));
        }
    }

    if matches!(agent_info.agent_type_id, 5 | 8 | 12) {
        let cosmos_expired = ctx.db.with_snapshot(|snap| {
            snap.active_missions.values().any(|m| {
                m.character_id == char_id && m.agent_id == agent_id && m.state == "Expired"
            })
        });
        if cosmos_expired {
            return Some(Err(BridgeRpcError {
                code: "CosmosMissionExpired".into(),
                message: format!("One-time COSMOS agent {agent_id} mission offer has expired and cannot be re-offered"),
                client_error: None,
            }));
        }
    }

    let is_alpha = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).is_some_and(|c| c.clone_grade <= 1)
    });
    if is_alpha && agent_info.level >= 4 {
        return Some(Err(BridgeRpcError {
            code: "CantDoMissionAlphaClone".into(),
            message: format!("Agent level {} requires Omega clone state", agent_info.level),
            client_error: None,
        }));
    }

    if agent_info.level == 5 {
        let agent_sys = agent_info.solar_system_id.unwrap_or(sys_id);
        let sec = ctx.sde.solar_systems.get(&agent_sys).map(|s| s.security_status).unwrap_or(1.0);
        if sec >= 0.45 {
            return Some(Err(BridgeRpcError {
                code: "CantDoMission".into(),
                message: format!("Level 5 agent {agent_id} cannot operate in high-security space ({sec:.2})"),
                client_error: None,
            }));
        }
    }

    let faction_id = ctx.sde.npc_corporations.get(&agent_info.corporation_id).and_then(|c| c.faction_id).map(|fid| fid as i64);
    let effective_standing = ctx.db.with_snapshot(|snap| {
        compute_effective_agent_standing(ctx, snap, char_id, agent_id, agent_info.corporation_id, faction_id)
    });
    if agent_info.level > 1 {
        let required_standing = ((agent_info.level as f64) - 1.0) * 2.0 - 1.0;
        if effective_standing < required_standing {
            if is_implicit_greeting {
                let refusal = format!(
                    "I am sorry, capsuleer, but I only offer assignments to pilots with an effective standing of at least {required_standing:.1} (yours is {effective_standing:.2})."
                );
                return Some(Ok(PyRep::keyval([
                    ("agentID", PyRep::Integer(agent_id)),
                    ("state", PyRep::String("Refused".into())),
                    ("missionState", PyRep::Integer(0)),
                    ("briefing", PyRep::String(refusal.clone())),
                    ("agentSays", PyRep::String(refusal)),
                    ("dialogue", PyRep::List(Vec::new())),
                ])));
            }
            return Some(Err(BridgeRpcError { code: "InsufficientAgentStanding".into(), message: format!("Agent {agent_id} (level {}) requires agent/corporation/faction standing >= {required_standing:.1}, character has {effective_standing:.2}", agent_info.level), client_error: None }));
        }
    } else if !matches!(agent_info.agent_type_id, 7 | 12) && effective_standing <= -2.0 {
        if is_implicit_greeting {
            let refusal = format!(
                "I do not work with pilots whose standing has fallen so low ({effective_standing:.2}). Improve your standing above -2.0 first."
            );
            return Some(Ok(PyRep::keyval([
                ("agentID", PyRep::Integer(agent_id)),
                ("state", PyRep::String("Refused".into())),
                ("missionState", PyRep::Integer(0)),
                ("briefing", PyRep::String(refusal.clone())),
                ("agentSays", PyRep::String(refusal)),
                ("dialogue", PyRep::List(Vec::new())),
            ])));
        }
        return Some(Err(BridgeRpcError { code: "InsufficientAgentStanding".into(), message: format!("Agent {agent_id} (level 1) requires standing > -2.0, character has {effective_standing:.2}"), client_error: None }));
    }

    let is_rnd = is_rnd_agent(agent_info);
    if is_rnd {
        let has_research = ctx.db.with_snapshot(|snap| {
            load_rnd_research_entries(snap, char_id).iter().any(|r| r.agent_id == agent_id)
        });
        if !has_research {
            if is_implicit_greeting {
                let greeting = "Greetings, capsuleer. We do not have an active research project yet, but we can begin one whenever you are ready.".to_string();
                return Some(Ok(PyRep::keyval([
                    ("agentID", PyRep::Integer(agent_id)),
                    ("state", PyRep::String("Greeting".into())),
                    ("missionState", PyRep::Integer(0)),
                    ("briefing", PyRep::String(greeting.clone())),
                    ("agentSays", PyRep::String(greeting)),
                    (
                        "dialogue",
                        PyRep::List(vec![PyRep::Tuple(vec![
                            PyRep::Integer(12),
                            PyRep::String("Start Research".into()),
                        ])]),
                    ),
                ])));
            }
            return Some(Err(BridgeRpcError {
                code: "ResearchNotActive".into(),
                message: format!("Agent {agent_id} requires active research to offer daily missions"),
                client_error: None,
            }));
        }
    }

    let offer_dur = 604_800_u64;

    let res = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(existing_active) = snap.active_missions.values().find(|m| {
            m.character_id == char_id
                && m.agent_id == agent_id
                && m.state == "Offered"
        }) {
            let mid = existing_active.mission_id;
            let title = ctx.sde.missions.get(&mid).map(|t| t.title.clone()).unwrap_or_default();
            let meta = load_mission_meta(snap, char_id, mid);
            let dest_sid = meta.as_ref().and_then(|m| m.destination_station_id);
            let briefing = build_mission_briefing_text(
                ctx,
                agent_id,
                mid,
                &title,
                existing_active.solar_system_id,
                meta.as_ref(),
            );
            let objectives = format!("Execute mission objectives for {} and report back.", title);
            return Ok((mid, title, briefing, objectives, dest_sid));
        }
        let is_heraldry_agent = agent_info.agent_type_id == 13 || agent_info.division_id == 37;
        let next_tmpl = if is_heraldry_agent && explicit_agent {
            ctx.sde.missions
                .values().filter(|m| {
                     m.content_template.as_deref().unwrap_or_default().contains("Heraldry")
                        && !snap.active_missions.contains_key(&(char_id, m.mission_id))
                }).min_by_key(|m| m.mission_id)
        } else {
            ctx.sde.missions
                .values().filter(|m| {
                    mission_matches_agent_pool_with_sde(&ctx.sde, m, agent_info)
                        && m.has_standing_rewards
                        && !snap.active_missions.contains_key(&(char_id, m.mission_id))
                }).min_by_key(|m| m.mission_id).or_else(|| {
                    ctx.sde.missions
                        .values().filter(|m| {
                            mission_matches_agent_pool_with_sde(&ctx.sde, m, agent_info)
                                && !snap.active_missions.contains_key(&(char_id, m.mission_id))
                        }).min_by_key(|m| m.mission_id)
                })
        };
        let Some(tmpl) = next_tmpl else {
            return Err(BridgeRpcError { code: "NoMissionsAvailable".into(), message: format!("No unoffered missions remaining in SDE pool for agent {agent_id} and character {char_id}"), client_error: None });
        };
        let mission_id = tmpl.mission_id;
        let title = tmpl.title.clone();
        let ct = tmpl.content_template.as_deref().unwrap_or_default();
        let arch = classify_mission_archetype(Some(tmpl), None, Some(agent_info));
        let is_heraldry = arch.is_heraldry;
        let is_trade = arch.is_trade;
        let is_courier = arch.is_courier;
        let is_data_center = arch.is_data_center;
        let is_kill = arch.is_kill;
        let is_mining = !is_kill && !is_trade && !is_courier && !is_heraldry && (ct.contains("BasicMining")
            || tmpl.title.to_lowercase().contains("mining")
            || agent_info.division_id == 23);

        let target_sys = if is_fw_agent(agent_info) {
            resolve_fw_warzone_system(agent_info.corporation_id)
        } else {
            sys_id
        };
        let (req_tid, req_qty, origin_sid, dest_sid, npcs_remaining, objectives) = if is_rnd {
            let qty = rnd_tritanium_required_qty(agent_info.level);
            (Some(34), qty, agent_info.station_id, None, 0, format!("Deliver {qty} units of Tritanium to agent station hangar."))
        } else if is_data_center {
            (Some(17202), 1, agent_info.station_id, None, 0, "Deliver 1 Pirate Tag to agent station hangar.".to_string())
        } else if is_courier {
            let cd = resolve_courier_mission_details(ctx, tmpl, agent_info)?;
            let cargo_name = ctx.sde.types.get(&cd.cargo_type_id).map(|t| t.name.as_str()).unwrap_or("cargo");
            let dest_name = ctx.sde.stations.get(&cd.destination_station_id).map(|s| s.name.as_str()).unwrap_or("destination station");
            (Some(cd.cargo_type_id), cd.cargo_quantity, Some(cd.origin_station_id), Some(cd.destination_station_id), 0, format!("Transport {} units of {} to {}.", cd.cargo_quantity, cargo_name, dest_name))
        } else if is_trade {
            let td = resolve_trade_mission_details(ctx, tmpl);
            let trade_name = ctx.sde.types.get(&td.trade_type_id).map(|t| t.name.as_str()).unwrap_or("goods");
            (Some(td.trade_type_id), td.trade_quantity, agent_info.station_id, None, 0, format!("Acquire and deliver {} units of {} to the agent station hangar.", td.trade_quantity, trade_name))
        } else if is_mining {
            let md = resolve_mining_mission_details(ctx, tmpl);
            (Some(md.ore_type_id), md.required_units, agent_info.station_id, None, 0, md.objective_text)
        } else if is_heraldry {
            (None, 0, agent_info.station_id, None, 0, "Complete heraldry assignment and report back.".to_string())
        } else {
            let enc = resolve_security_mission_encounter(ctx, tmpl, agent_info, target_sys);
            (None, 0, agent_info.station_id, None, enc.wave_npc_count, enc.objective_text)
        };

        let is_burner = title.contains("Anomic")
            || (agent_info.level >= 4 && agent_info.division_id == 24);
        let offer_meta = MissionTimingAndCargoMeta {
            mission_id,
            agent_id,
            offered_at_unix: now,
            offer_expiry_unix: now + offer_dur,
            accepted_at_unix: None,
            accept_expiry_unix: None,
            bonus_expiry_unix: None,
            completed_at_unix: None,
            required_delivery_type_id: req_tid,
            required_delivery_qty: req_qty,
            granted_courier_item_id: None,
            granted_courier_qty: 0,
            is_anomic_burner: is_burner,
            chosen_branch: None,
            destination_station_id: dest_sid,
            origin_station_id: origin_sid,
            offered_level: Some(agent_info.level),
        };
        let briefing = build_mission_briefing_text(
            ctx,
            agent_id,
            mission_id,
            &title,
            target_sys,
            Some(&offer_meta),
        );
        save_mission_meta(snap, char_id, &offer_meta);
        snap.active_missions.insert((char_id, mission_id), ActiveMissionRecord { mission_id, character_id: char_id, agent_id, solar_system_id: target_sys, state: "Offered".into(), current_room_id: 1, npcs_remaining, spawned_npc_ball_ids: Vec::new(), bookmark_id: None });
        Ok((mission_id, title, briefing, objectives, dest_sid))
    });

    match res {
        Ok((mission_id, title, briefing, objectives, dest_sid)) => {
            let expiry_unix = now + offer_dur;
            let dialogue = PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3), PyRep::String("Accept".into())]), PyRep::Tuple(vec![PyRep::Integer(9), PyRep::String("Decline".into())])]);
            let CharacterMissionRewards {
                isk_reward,
                lp_reward,
                bonus_reward,
            } = compute_character_mission_rewards(ctx, char_id, agent_id, mission_id, sys_id);
            let bonus_window_secs =
                mission_bonus_window_secs(ctx, char_id, agent_id, mission_id);
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                OnAgentMissionChange {
                    action: "offered".into(),
                    agent_id,
                },
            );
            let clean_header = title.trim_start_matches("[d]").trim();
            let header_str = if clean_header.is_empty() { "Agent Transmission" } else { clean_header };
            let trans_kv = PyRep::keyval([("header", PyRep::String(header_str.to_string())), ("text", PyRep::String(briefing.clone())), ("icon", PyRep::String("res:/UI/Texture/WindowIcons/agent.png".into())), ("agentID", PyRep::Integer(agent_id))]);
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                OnIncomingTransmission {
                    transmission: trans_kv,
                    is_agent_mission: true,
                },
            );
            let mut offer_kvs = vec![
                ("missionID", PyRep::Integer(mission_id as i64)),
                ("contentID", PyRep::Integer(mission_id as i64)),
                ("agentID", PyRep::Integer(agent_id)),
                ("title", PyRep::String(title)),
                ("state", PyRep::String("Offered".into())),
                ("missionState", PyRep::Integer(1)),
                ("briefing", PyRep::String(briefing.clone())),
                ("agentSays", PyRep::String(briefing)),
                ("objectives", PyRep::String(objectives)),
                ("dialogue", dialogue),
                ("iskReward", PyRep::Integer(isk_reward)),
                ("lpReward", PyRep::Integer(lp_reward)),
                ("bonusReward", PyRep::Integer(bonus_reward)),
                ("bonusWindowSecs", PyRep::Integer(bonus_window_secs as i64)),
                ("expiryUnix", PyRep::Integer(expiry_unix as i64)),
            ];
            if let Some(dsid) = dest_sid {
                offer_kvs.push(("destinationStationID", PyRep::Integer(dsid)));
            }
            Some(Ok(PyRep::keyval(offer_kvs)))
        }
        Err(e) => Some(Err(e)),
    }
}
