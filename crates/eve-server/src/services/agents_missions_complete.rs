//! `agents_missions_complete` — `agentMgr.DoAction` mission completion arms (`6 | 7`).

#![forbid(unsafe_code)]

use super::{
    apply_mission_completion_standings, compute_character_mission_rewards,
    epic_arc_required_missions, finalize_mission_state_cleanup, is_agent_colocated, is_fw_agent,
    is_rnd_agent, is_storyline_template, load_epic_arc_states, load_rnd_research_entries,
    notify_standing_changes, resolve_agent_and_corp_for_system, resolve_courier_mission_details,
    resolve_epic_arc_id_for_agent, resolve_mining_mission_details,
    resolve_security_mission_encounter, resolve_target_mission_id, resolve_trade_mission_details,
    rnd_tritanium_required_qty, save_epic_arc_states, save_rnd_research_entries,
    CharacterMissionRewards, EpicArcStateEntry,
};
use crate::router::notify_registry::{
    OnAgentMissionChange, OnAgentMissionObjectivesCompleted, OnAgentMissionObjectivesFailed,
};
use crate::router::{NotificationTarget, ServerContext};
use crate::services::notify_emitters_character_services::emit_agent_mission_dialogue;
use eve_db::{
    decide_mission_kind, CourierObjective, DeliveryObjective, EncounterObjective, IskCents,
    MissionInstance, MissionKind, MissionObjective, MissionState, StandingTransactionRecord,
    WalletJournalEntry,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub(super) async fn handle_mission_complete_action(
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
    let mission_id = match resolve_target_mission_id(ctx, char_id, lookup_agent_id, Some(MissionState::Accepted), args, _kwargs) { Ok(v) => v, Err(e) => return Some(Err(e)) };
    let Some(tmpl) = ctx.sde.missions.get(&mission_id).cloned() else {
        return Some(Err(BridgeRpcError { code: "UnknownMission".into(), message: format!("Mission {mission_id} does not exist in SDE"), client_error: None }));
    };
    let is_storyline = is_storyline_template(&tmpl);
    let (active_agent_id, pre_dest_station, pre_is_dc, pre_is_trade, pre_is_courier) =
        ctx.db.with_snapshot(|snap| {
            let Some(m) = snap.missions.get_by_character_template(char_id, mission_id) else {
                return (agent_id, None, false, false, false);
            };
            (
                m.agent_id,
                m.destination_station_id(),
                m.kind == MissionKind::DataCenterTurnIn,
                m.kind == MissionKind::Trade,
                m.kind == MissionKind::Courier,
            )
        });
    if action_id == 6 {
        if let Some(ref agent_info) = ctx.sde.agents.get(&active_agent_id).cloned() {
            let at_courier_dest = pre_is_courier
                && pre_dest_station.is_some()
                && session.station_id == pre_dest_station;
            if !at_courier_dest && !is_agent_colocated(ctx, session, sys_id, agent_info).await {
                return Some(Err(BridgeRpcError {
                    code: "AgentNotCoLocated".into(),
                    message: format!(
                        "Agent {active_agent_id} is in system {:?} / station {:?}, not character's current location (system {sys_id}, station {:?})",
                        agent_info.solar_system_id, agent_info.station_id, session.station_id
                    ),
                    client_error: None,
                }));
            }
        } else if pre_is_dc || pre_is_trade {
            return Some(Err(BridgeRpcError {
                code: "UnknownAgent".into(),
                message: format!("Agent {active_agent_id} does not exist in SDE"),
                client_error: None,
            }));
        }
    }
    let agent_level = ctx.sde.agents.get(&active_agent_id).map_or(1.0, |a| a.level.max(1) as f64);
    let storyline_mult = if is_storyline { 3.0 } else { 1.0 };
    let standing_pct = if tmpl.has_standing_rewards { 1.5 * agent_level * storyline_mult } else { 0.0 };
    let (storyline_agent_id, _) = resolve_agent_and_corp_for_system(ctx, char_id, sys_id, session.station_id, true);
    let CharacterMissionRewards {
        isk_reward,
        lp_reward,
        bonus_reward,
    } = compute_character_mission_rewards(ctx, char_id, active_agent_id, mission_id, sys_id);
    let corp_id = ctx.sde.agents.get(&active_agent_id).map(|a| a.corporation_id)
        .unwrap_or_else(|| {
            resolve_agent_and_corp_for_system(ctx, char_id, sys_id, session.station_id, is_storyline).1
        });
    let faction_id = ctx.sde.npc_corporations.get(&corp_id).and_then(|c| c.faction_id).map(|fid| fid as i64);
    let now = ctx.now_unix();

    let is_expired = ctx.db.with_snapshot(|snap| {
        snap.missions
            .get_by_character_template(char_id, mission_id)
            .and_then(|m| m.accept_expiry_unix)
            .is_some_and(|exp| exp > 0 && now > exp)
    });
    if is_expired {
        ctx.db.with_mut_snapshot(|snap| {
            finalize_mission_state_cleanup(snap, char_id, mission_id, MissionState::Failed, true);
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                let cur_ag = ch.standings.get(&active_agent_id).copied().unwrap_or(0.0);
                ch.standings.insert(active_agent_id, (cur_ag - 0.10).max(-10.0));
                let cur_corp = ch.standings.get(&corp_id).copied().unwrap_or(0.0);
                ch.standings.insert(corp_id, (cur_corp - 0.10).max(-10.0));
                if let Some(fac_id) = faction_id {
                    let cur_fac = ch.standings.get(&fac_id).copied().unwrap_or(0.0);
                    ch.standings.insert(fac_id, (cur_fac - 0.05).max(-10.0));
                }
            }
            snap.next_entity_id += 1;
            let ev_id = snap.next_entity_id;
            snap.standing_transactions.entry(char_id).or_default().push(
                StandingTransactionRecord {
                    event_id: ev_id,
                    from_id: corp_id,
                    to_id: char_id,
                    modification: -0.10,
                    event_type_id: 84,
                },
            );
        });
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            OnAgentMissionObjectivesFailed {
                agent_id: active_agent_id,
            },
        );
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            OnAgentMissionChange {
                action: "failed".into(),
                agent_id: active_agent_id,
            },
        );
        return Some(Err(BridgeRpcError {
            code: "MissionExpired".into(),
            message: format!("Mission {mission_id} has expired and failed"),
            client_error: None,
        }));
    }

    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(m) = snap.missions.get_by_character_template(char_id, mission_id) else {
            return Err(BridgeRpcError { code: "MissionNotActive".into(), message: format!("Mission {mission_id} is not active for character {char_id}"), client_error: None });
        };
        let m_kind = m.kind;
        let m_state = m.state;
        let m_npcs_remaining = m.npcs_remaining();
        let m_agent_id = m.agent_id;
        let m_origin_station_id = m.origin_station_id;
        let m_dest_station_id = m.destination_station_id();
        let m_granted_cid = m.granted_courier().and_then(|(cid, _)| cid);
        let m_bonus_expiry = m.bonus_expiry_unix;
        let m_req_delivery = m.required_delivery().map(|(tid, qty)| (tid, qty.max(1)));
        let agent_opt = ctx.sde.agents.get(&m_agent_id);
        let is_courier = m_kind == MissionKind::Courier;
        let is_mining_mission = m_kind == MissionKind::Mining;
        let is_kill = m_kind == MissionKind::Encounter;

        if m_state != MissionState::Accepted || (m_npcs_remaining > 0 && is_kill) {
            return Err(BridgeRpcError { code: "MissionIncomplete".into(), message: format!("Mission {mission_id} cannot be completed (state='{}', npcs_remaining={m_npcs_remaining})", m_state.as_str()), client_error: None });
        }

        let req_delivery = m_req_delivery.or_else(|| match m_kind {
            MissionKind::Mining => {
                let md = resolve_mining_mission_details(ctx, &tmpl);
                Some((md.ore_type_id, md.required_units))
            }
            MissionKind::Trade => {
                let td = resolve_trade_mission_details(ctx, &tmpl);
                Some((td.trade_type_id, td.trade_quantity))
            }
            MissionKind::ResearchTurnIn => {
                Some((34, rnd_tritanium_required_qty(agent_opt.map_or(1, |a| a.level))))
            }
            MissionKind::DataCenterTurnIn => Some((17202, 1)),
            MissionKind::Courier | MissionKind::Heraldry | MissionKind::Encounter => None,
        });
        if action_id == 7 && req_delivery.is_some() {
            return Err(BridgeRpcError {
                code: "CannotCompleteRemotely".into(),
                message: format!(
                    "Mission {mission_id} requires physical item delivery and cannot be completed remotely"
                ),
                client_error: None,
            });
        }
        if let Some((req_tid, req_units)) = req_delivery {
            let current_station = snap.characters.get(&char_id).and_then(|c| c.station_id).or(session.station_id);
            let agent_station_id = ctx
                .sde
                .agents
                .get(&m_agent_id)
                .and_then(|a| a.station_id)
                .or(m_origin_station_id);
            let active_ship_id = snap
                .characters
                .get(&char_id)
                .map(|c| c.active_ship_id)
                .filter(|&id| id > 0)
                .or(session.ship_id);
            super::agents_mission_delivery::verify_and_consume_mission_delivery_items(
                snap,
                char_id,
                mission_id,
                req_tid,
                req_units,
                is_courier,
                is_mining_mission,
                current_station,
                agent_station_id,
                active_ship_id,
                m_dest_station_id,
                m_granted_cid,
            )?;
        }

        finalize_mission_state_cleanup(snap, char_id, mission_id, MissionState::Completed, false);

        let bonus_granted = m_bonus_expiry.is_none_or(|bexp| now <= bexp);
        let bonus_paid = if bonus_granted { bonus_reward } else { 0 };
        let char_sys = snap.characters.get(&char_id).map(|c| c.solar_system_id).unwrap_or(0);
        let lp_mult = crate::services::pve_special_sites::get_incursion_effects_for_system_in_snap(&ctx.sde, snap, char_sys)
            .map(|e| (1.0 - e.lp_penalty).clamp(0.0, 1.0))
            .unwrap_or(1.0);
        // NPC corporations charge 0% LP tax; independent 11% NPC-corp (or player-corp) ISK
        // taxation for base reward and time-bonus wallet journal entries (>= 100_000 ISK
        // threshold for NPC corps) is applied right below by `lp_store::record_lp_reward_log`.
        let credited_lp = ((lp_reward.max(0) as f64) * lp_mult).round() as i64;

        if let Some(ch) = snap.characters.get_mut(&char_id) {
            if isk_reward > 0 {
                let reward_cents = IskCents::from_isk(isk_reward);
                ch.balance = ch.balance.checked_add(reward_cents).unwrap_or(ch.balance);
                let bal_after_reward = ch.balance;

                snap.next_entity_id += 1;
                let jid1 = snap.next_entity_id;
                snap.wallet_journal.push(WalletJournalEntry {
                    entry_id: jid1,
                    character_id: char_id,
                    entry_type: "agent_mission_reward".to_string(),
                    amount: reward_cents,
                    balance_after: bal_after_reward,
                    reason: format!("Mission {mission_id} reward"),
                });
                snap.wallet_journal_timestamps.insert(jid1, now);
            }

            if bonus_paid > 0 {
                let bonus_cents = IskCents::from_isk(bonus_paid);
                ch.balance = ch.balance.checked_add(bonus_cents).unwrap_or(ch.balance);
                snap.next_entity_id += 1;
                let jid2 = snap.next_entity_id;
                snap.wallet_journal.push(WalletJournalEntry {
                    entry_id: jid2,
                    character_id: char_id,
                    entry_type: "agent_mission_time_bonus_reward".to_string(),
                    amount: bonus_cents,
                    balance_after: ch.balance,
                    reason: format!("Mission {mission_id} time bonus reward"),
                });
                snap.wallet_journal_timestamps.insert(jid2, now);
            }

            if credited_lp > 0 {
                let lp_entry = ch.loyalty_points.entry(corp_id).or_insert(0);
                *lp_entry = lp_entry.saturating_add(credited_lp);
                if corp_id == crate::services::lp_store_catalog::PARAGON_CORP_ID && ch.corporation_id >= 98_000_000 {
                    let c_lp = snap.corp_lp_balances.entry((ch.corporation_id, corp_id)).or_insert(0);
                    *c_lp = c_lp.saturating_add(credited_lp);
                }
            }
        }
        crate::services::lp_store::record_lp_reward_log(
            snap,
            char_id,
            corp_id,
            credited_lp,
            format!("Mission {mission_id} (agent {m_agent_id})"),
            now,
        );

        let is_rnd = ctx.sde.agents.get(&m_agent_id).is_some_and(is_rnd_agent);
        if is_rnd {
            crate::services::pve_ess_reserve_banks::record_agent_mission_completed(snap, char_id, m_agent_id);
            let ag_lvl = ctx.sde.agents.get(&m_agent_id).map_or(1, |a| a.level);
            let mut research = load_rnd_research_entries(snap, char_id);
            if let Some(re) = research.iter_mut().find(|r| r.agent_id == m_agent_id) {
                let elapsed_days = (now.saturating_sub(re.started_unix) as f64) / 86_400.0;
                re.initial_points += re.points_per_day * elapsed_days + re.points_per_day;
                let sk_lvl = snap.skills.get(&char_id).and_then(|m| m.get(&re.research_type_id)).map_or(1, |s| s.level);
                re.points_per_day = re.points_per_day.max(crate::services::pve_ess_reserve_banks::compute_rnd_rp_per_day(ctx, snap, char_id, m_agent_id, ag_lvl, sk_lvl));
                re.started_unix = now;
            }
            save_rnd_research_entries(snap, char_id, &research);
        }

        let is_special = is_storyline
            || ctx.sde.agents.get(&m_agent_id)
                .is_some_and(|a| matches!(a.agent_type_id, 5 | 8 | 10));
        let (new_st, st_changes) = apply_mission_completion_standings(ctx, snap, char_id, m_agent_id, mission_id, corp_id, standing_pct, is_special);

        let mut arc_completed_now = false;
        if ctx.sde.agents.get(&m_agent_id)
            .is_some_and(|a| a.agent_type_id == 10)
        {
            let (arc_id, fac_id) = resolve_epic_arc_id_for_agent(ctx, m_agent_id);
            let required_missions = epic_arc_required_missions(ctx, fac_id);
            let mut arcs = load_epic_arc_states(snap, char_id);
            if let Some(arc) = arcs.iter_mut().find(|a| a.epic_arc_id == arc_id) {
                if arc.completed_at_unix.is_some() {
                    arc.completed_missions.clear();
                    arc.completed_at_unix = None;
                    arc.cooldown_until_unix = None;
                    arc.chapter = 1;
                }
                if !arc.completed_missions.contains(&mission_id) {
                    arc.completed_missions.push(mission_id);
                }
                let done = arc.completed_missions.len();
                let computed_chapter = ((done * 3).saturating_add(required_missions - 1) / required_missions).clamp(1, 3) as u32;
                arc.chapter = arc.chapter.max(computed_chapter);
                if done >= required_missions {
                    arc.completed_at_unix = Some(now);
                    arc.cooldown_until_unix = Some(now + 7_776_000);
                    arc_completed_now = true;
                }
            } else {
                arcs.push(EpicArcStateEntry {
                    epic_arc_id: arc_id,
                    agent_id: m_agent_id,
                    faction_id: fac_id,
                    chapter: 1,
                    completed_missions: vec![mission_id],
                    chosen_branch: None,
                    completed_at_unix: None,
                    cooldown_until_unix: None,
                });
            }
            save_epic_arc_states(snap, char_id, &arcs);
        }

        let is_epic_arc = ctx.sde.agents.get(&m_agent_id).is_some_and(|a| matches!(a.agent_type_id, 10 | 11));
        let mut resolved_storyline_agent_id = storyline_agent_id;
        let mut resolved_storyline_level = ctx.sde.agents.get(&m_agent_id).map_or(1, |a| a.level.clamp(1, 5));
        let (t, storyline) = if (!is_storyline && tmpl.has_standing_rewards) || is_rnd || is_epic_arc {
            let sl_opt = ctx.sde.agents.get(&m_agent_id).and_then(|ca| crate::services::pve_ess_reserve_banks::record_mission_completion_and_resolve_storyline(ctx, snap, char_id, ca));
            let qualifies = ctx.sde.agents.get(&m_agent_id).is_some_and(|ca| crate::services::pve_ess_reserve_banks::is_storyline_qualifying_agent_type(ca.agent_type_id)) && !crate::services::pve_ess_reserve_banks::is_interbus_or_militia_corp(&ctx.sde, corp_id);
            let t = if !qualifies {
                snap.mission_tally.get(&char_id).copied().unwrap_or(0)
            } else {
                let count = snap.mission_tally.entry(char_id).or_insert(0); *count += 1; *count
            };
            if let Some((sl_id, sl_lvl)) = sl_opt { resolved_storyline_agent_id = sl_id; resolved_storyline_level = sl_lvl; }
            (t, sl_opt.is_some())
        } else {
            (snap.mission_tally.get(&char_id).copied().unwrap_or(0), false)
        };
        if storyline {
            let storyline_agent_id = resolved_storyline_agent_id;
            if let Some(storyline_tmpl) = crate::services::pve_ess_reserve_banks::select_storyline_mission_for_level(
                &ctx.sde,
                &snap.missions,
                char_id,
                resolved_storyline_level,
            )
            {
                let storyline_mission_id = storyline_tmpl.mission_id;
                if !snap
                    .missions
                    .contains_character_template(char_id, storyline_mission_id)
                {
                    let sl_agent = ctx.sde.agents.get(&storyline_agent_id);
                    let sl_sys_id = sl_agent.and_then(|a| a.solar_system_id).unwrap_or(sys_id);
                    let sl_kind = decide_mission_kind(Some(storyline_tmpl), sl_agent);
                    let (sl_objective, origin_sid) = match sl_kind {
                        MissionKind::Courier => {
                            if let Some(sa) = sl_agent.and_then(|a| {
                                resolve_courier_mission_details(ctx, storyline_tmpl, a).ok()
                            }) {
                                (
                                    MissionObjective::Courier(CourierObjective {
                                        cargo_type_id: Some(sa.cargo_type_id),
                                        quantity: sa.cargo_quantity,
                                        destination_station_id: Some(sa.destination_station_id),
                                        granted_item_id: None,
                                        granted_qty: 0,
                                    }),
                                    Some(sa.origin_station_id),
                                )
                            } else {
                                (
                                    MissionObjective::Courier(CourierObjective {
                                        cargo_type_id: None,
                                        quantity: 0,
                                        destination_station_id: None,
                                        granted_item_id: None,
                                        granted_qty: 0,
                                    }),
                                    sl_agent.and_then(|a| a.station_id),
                                )
                            }
                        }
                        MissionKind::Trade => {
                            let td = resolve_trade_mission_details(ctx, storyline_tmpl);
                            (
                                MissionObjective::Delivery(DeliveryObjective {
                                    item_type_id: Some(td.trade_type_id),
                                    quantity: td.trade_quantity,
                                }),
                                sl_agent.and_then(|a| a.station_id),
                            )
                        }
                        MissionKind::Mining => {
                            let md = resolve_mining_mission_details(ctx, storyline_tmpl);
                            (
                                MissionObjective::Delivery(DeliveryObjective {
                                    item_type_id: Some(md.ore_type_id),
                                    quantity: md.required_units,
                                }),
                                sl_agent.and_then(|a| a.station_id),
                            )
                        }
                        MissionKind::ResearchTurnIn => (
                            MissionObjective::Delivery(DeliveryObjective {
                                item_type_id: Some(34),
                                quantity: rnd_tritanium_required_qty(
                                    sl_agent.map_or(1, |a| a.level),
                                ),
                            }),
                            sl_agent.and_then(|a| a.station_id),
                        ),
                        MissionKind::DataCenterTurnIn => (
                            MissionObjective::Delivery(DeliveryObjective {
                                item_type_id: Some(17202),
                                quantity: 1,
                            }),
                            sl_agent.and_then(|a| a.station_id),
                        ),
                        MissionKind::Heraldry => {
                            (MissionObjective::None, sl_agent.and_then(|a| a.station_id))
                        }
                        MissionKind::Encounter => {
                            if let Some(sa) = sl_agent {
                                let mut scaled_sa = sa.clone();
                                scaled_sa.level = resolved_storyline_level;
                                let enc = resolve_security_mission_encounter(
                                    ctx,
                                    storyline_tmpl,
                                    &scaled_sa,
                                    sl_sys_id,
                                );
                                (
                                    MissionObjective::Encounter(EncounterObjective {
                                        npcs_remaining: enc.wave_npc_count,
                                        spawned_npc_ball_ids: Vec::new(),
                                        initial_wave_count: None,
                                        initial_wave_types: Vec::new(),
                                        awaiting_downtime_respawn: false,
                                    }),
                                    sa.station_id,
                                )
                            } else {
                                (
                                    MissionObjective::Encounter(EncounterObjective {
                                        npcs_remaining: 1,
                                        spawned_npc_ball_ids: Vec::new(),
                                        initial_wave_count: None,
                                        initial_wave_types: Vec::new(),
                                        awaiting_downtime_respawn: false,
                                    }),
                                    None,
                                )
                            }
                        }
                    };
                    let sl_instance_id = snap.alloc_entity_id();
                    snap.missions
                        .try_insert(MissionInstance {
                            instance_id: sl_instance_id,
                            character_id: char_id,
                            agent_id: storyline_agent_id,
                            template_id: storyline_mission_id,
                            state: MissionState::Offered,
                            kind: sl_kind,
                            solar_system_id: sl_sys_id,
                            current_room_id: 1,
                            bookmark_id: None,
                            origin_station_id: origin_sid,
                            offered_level: Some(resolved_storyline_level),
                            is_anomic_burner: false,
                            offered_at_unix: now,
                            offer_expiry_unix: now + 604_800,
                            accepted_at_unix: None,
                            accept_expiry_unix: None,
                            bonus_expiry_unix: None,
                            objective: sl_objective,
                        })
                        .map_err(super::mission_store_rpc_err)?;
                }
            }
        }
        let delivery_station = ctx.sde.agents.get(&m_agent_id).and_then(|a| a.station_id).or_else(|| snap.characters.get(&char_id).and_then(|c| c.station_id)).unwrap_or(0);
        let item_reward = crate::services::agents_mission_rewards::mint_mission_item_reward_in_snap(ctx, snap, char_id, delivery_station, &tmpl, ctx.sde.agents.get(&m_agent_id));
        Ok((new_st, t, storyline, bonus_paid, st_changes, item_reward, credited_lp, m_agent_id, is_courier, is_mining_mission, is_kill, arc_completed_now))
    });

    match res {
        Ok((new_standing, tally, storyline_offered, bonus_paid, st_changes, item_reward, credited_lp, completed_agent_id, is_courier, is_mining_mission, is_kill, arc_completed_now)) => {
            notify_standing_changes(ctx, char_id, &st_changes);
            let mission_title_career = ctx.sde.missions.get(&mission_id).map(|m| m.title.as_str()).unwrap_or("");
            let is_fw_mission_agent = ctx.sde.agents.get(&completed_agent_id).is_some_and(is_fw_agent);
            crate::quasar_gateway::isolate_side_effect("agents_missions.career_mission_complete", || {
                crate::services::quasar_career_goals::record_mission_completion_career_goals(
                    ctx,
                    char_id,
                    completed_agent_id,
                    mission_title_career,
                    is_courier,
                    is_mining_mission,
                    is_kill,
                    is_fw_mission_agent,
                    arc_completed_now,
                    credited_lp,
                );
            });
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                OnAgentMissionObjectivesCompleted {
                    agent_id: active_agent_id,
                },
            );
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                OnAgentMissionChange {
                    action: "completed".into(),
                    agent_id: active_agent_id,
                },
            );
            let agent_type_id = ctx.sde.agents.get(&active_agent_id).map(|a| a.agent_type_id).unwrap_or(2);
            let mission_title = ctx.sde.missions.get(&mission_id).map(|m| m.title.clone()).unwrap_or_else(|| format!("Mission {mission_id}"));
            let complete_msg = format!(
                "Well done! Your mission '{mission_title}' is complete and your rewards have been credited."
            );
            emit_agent_mission_dialogue(
                ctx,
                char_id,
                active_agent_id,
                agent_type_id,
                &mission_title,
                &complete_msg,
                true,
            );
            Some(Ok(PyRep::keyval([
                ("missionID", PyRep::Integer(mission_id as i64)),
                ("contentID", PyRep::Integer(mission_id as i64)),
                ("agentID", PyRep::Integer(active_agent_id)),
                ("title", PyRep::String(mission_title)),
                ("state", PyRep::String(MissionState::Completed.as_str().into())),
                ("missionState", PyRep::Integer(4)),
                ("agentSays", PyRep::String(complete_msg.clone())),
                ("briefing", PyRep::String(complete_msg)),
                (
                    "dialogue",
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(2),
                        PyRep::String("Request Mission".into()),
                    ])]),
                ),
                ("iskReward", PyRep::Integer(isk_reward)),
                ("bonusReward", PyRep::Integer(bonus_paid)),
                ("lpReward", PyRep::Integer(lp_reward)),
                ("newStanding", PyRep::Real(new_standing)),
                ("missionTally", PyRep::Integer(tally as i64)),
                ("storylineTriggered", PyRep::Bool(storyline_offered)),
                ("rewardItemID", item_reward.map_or(PyRep::None, |r| PyRep::Integer(r.item_id))),
                ("rewardItemTypeID", item_reward.map_or(PyRep::None, |r| PyRep::Integer(r.type_id as i64))),
                ("rewardItemQuantity", item_reward.map_or(PyRep::None, |r| PyRep::Integer(r.quantity as i64))),
            ])))
        }
        Err(e) => Some(Err(e)),
    }
}
