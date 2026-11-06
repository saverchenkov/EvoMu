//! `pvp_filament_arena` — Proving Grounds multi-team / FFA match resolution, spatial arena isolation,
//! multi-tick player-ship destruction hooks, and `AbyssalPVPEndGateActivation` extraction.

#![forbid(unsafe_code)]

use crate::router::cache_notifications::NotificationTarget;
use crate::router::ServerContext;
use crate::services::pve_abyssal_hazards::resolve_character_home_station_and_system;
use crate::services::pve_abyssal_pylons::destroy_pod_and_respawn_faction_corvette_in_snap;
use crate::services::pvp_filament_mgr::{
    build_character_statistics_dict_for_event, build_leaderboard_rows_dict_for_event,
    get_pvp_filament_match_spec, load_pvp_store, lookup_sde_event_ids, save_pvp_store,
    PvpArenaMatchInstance, PvpFilamentStore,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

pub const PROVING_ARENA_BOUNDARY_RADIUS_M: f64 = 75_000.0;
pub const PROVING_ARENA_DURATION_SECS: u64 = 900;
/// DERIVED: Server-only Proving Grounds leaderboard scoring weights (`+100` per win, `+25` per loss); exact Tranquility weights are server-side and absent from V24 client SDE.
pub const PROVING_WIN_POINTS: i64 = 100;
pub const PROVING_LOSS_POINTS: i64 = 25;
/// DERIVED: Server-only Proving Grounds boundary/collapse escalating environmental DPS (`250 * (1 + 0.5 * tick)`); exact Tranquility ramp table is server-side and absent from V24 client SDE.
pub const PROVING_HAZARD_BASE_DPS: f64 = 250.0;
pub const PROVING_HAZARD_RAMP_PER_TICK: f64 = 0.5;

pub fn credit_proving_grounds_match_rewards_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    sde_opt: Option<&eve_sde::SdeRegistry>,
    filament_type_id: u32,
    winning_pilots: &[i64],
) {
    if winning_pilots.is_empty() {
        return;
    }
    let mut rewards = sde_opt
        .filter(|_| filament_type_id > 0)
        .map(|sde| get_pvp_filament_match_spec(sde, filament_type_id).rewards)
        .unwrap_or_default();
    if rewards.is_empty() {
        // DERIVED: Server-only 1,000,000 ISK fallback reward (type 52996) when a filament's V24 SDE entry carries no explicit reward rows.
        rewards.push((52_996, 1_000_000));
    }
    for &cid in winning_pilots {
        let home_station_id = if let Some(sde) = sde_opt {
            resolve_character_home_station_and_system(snap, sde, cid).0
        } else {
            snap.characters
                .get(&cid)
                .map(|c| c.home_station_id)
                .filter(|&h| h > 0)
                .unwrap_or(60_000_004)
        };
        for &(reward_type_id, amount) in &rewards {
            if amount == 0 {
                continue;
            }
            if reward_type_id == 52_996 {
                let isk = eve_db::IskCents::from_isk(i64::from(amount));
                if let Some(ch) = snap.characters.get_mut(&cid) {
                    ch.balance = eve_db::IskCents(ch.balance.0.saturating_add(isk.0));
                    let bal = ch.balance;
                    snap.append_journal(
                        cid,
                        "ProvingGroundsReward",
                        isk,
                        bal,
                        "Abyssal Proving Grounds match victory reward",
                    );
                }
            } else {
                snap.next_entity_id += 1;
                let iid = snap.next_entity_id;
                snap.items.insert(
                    iid,
                    eve_db::InventoryItemRecord {
                        item_id: iid,
                        type_id: reward_type_id,
                        owner_id: cid,
                        location_id: home_station_id,
                        flag: eve_db::InventoryFlag::Hangar,
                        quantity: amount as i32,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    },
                );
            }
        }
    }
}

pub fn effective_match_teams(store: &PvpFilamentStore, m: &PvpArenaMatchInstance) -> Vec<Vec<i64>> {
    if let Some(teams) = store.match_teams.get(&m.match_id) {
        if !teams.is_empty() {
            return teams.clone();
        }
    }
    vec![m.team_a.clone(), m.team_b.clone()]
}

pub fn match_contains_char(store: &PvpFilamentStore, m: &PvpArenaMatchInstance, cid: i64) -> bool {
    effective_match_teams(store, m)
        .iter()
        .any(|t| t.contains(&cid))
}

pub fn are_opposing_in_match(
    store: &PvpFilamentStore,
    m: &PvpArenaMatchInstance,
    char_a: i64,
    char_b: i64,
) -> bool {
    let teams = effective_match_teams(store, m);
    let ta = teams.iter().position(|t| t.contains(&char_a));
    let tb = teams.iter().position(|t| t.contains(&char_b));
    matches!((ta, tb), (Some(ia), Some(ib)) if ia != ib)
}

/// Applies a Proving Grounds kill inside `snap`, advancing team elimination and concluding the match
/// only when at most one team has surviving (non-eliminated) pilots.
pub fn record_proving_grounds_combat_result_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    sde_opt: Option<&eve_sde::SdeRegistry>,
    winner_char_id: i64,
    loser_char_id: i64,
    damage_done: f64,
    _now_unix: u64,
) -> Result<(i64, Vec<i64>), BridgeRpcError> {
    if winner_char_id == loser_char_id {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "Winner and loser must be distinct characters".into(),
        });
    }
    let mut store = load_pvp_store(snap);
    let active_mid = store
        .active_matches
        .values()
        .find(|m| m.active && are_opposing_in_match(&store, m, winner_char_id, loser_char_id))
        .map(|m| m.match_id);
    let both_queued =
        store.queue.contains_key(&winner_char_id) && store.queue.contains_key(&loser_char_id);

    if active_mid.is_none() && !both_queued {
        return Err(BridgeRpcError {
            code: "NotInPvpQueue".into(),
            message: "Both combatants must be queued in Proving Grounds or in an active match".into(),
        });
    }

    let mut affected_pilots = vec![winner_char_id, loser_char_id];
    let mut reward_winners: Option<(u32, Vec<i64>)> = None;
    let matched_event_id = if let Some(mid) = active_mid {
        let teams = store
            .active_matches
            .get(&mid)
            .map(|m| effective_match_teams(&store, m))
            .unwrap_or_default();
        let ev_id = store
            .active_matches
            .get(&mid)
            .map(|m| m.event_id)
            .unwrap_or(171);

        let elim = store.match_eliminated.entry(mid).or_default();
        elim.insert(loser_char_id);

        // Credit individual kill + damage to the killer immediately
        let wk = store.stats.entry(winner_char_id).or_insert((0, 0, 0, 0, 0.0));
        wk.3 += 1;
        wk.4 += damage_done;
        let ewk = store
            .event_stats
            .entry(ev_id)
            .or_default()
            .entry(winner_char_id)
            .or_insert((0, 0, 0, 0, 0.0));
        ewk.3 += 1;
        ewk.4 += damage_done;

        let surviving_team_indices: Vec<usize> = teams
            .iter()
            .enumerate()
            .filter_map(|(idx, t)| {
                t.iter()
                    .any(|&cid| !elim.contains(&cid))
                    .then_some(idx)
            })
            .collect();

        if surviving_team_indices.len() <= 1 {
            let win_team_idx = surviving_team_indices
                .first()
                .copied()
                .or_else(|| teams.iter().position(|t| t.contains(&winner_char_id)))
                .unwrap_or(0);
            if let Some(m) = store.active_matches.get_mut(&mid) {
                m.active = false;
                m.winner_char_id = Some(winner_char_id);
            }
            let fil_tid = store.match_filament_types.get(&mid).copied().unwrap_or(0);
            if let Some(w_team) = teams.get(win_team_idx) {
                reward_winners = Some((fil_tid, w_team.clone()));
            }
            for (t_idx, team) in teams.iter().enumerate() {
                for &cid in team {
                    store.queue.remove(&cid);
                    if !affected_pilots.contains(&cid) {
                        affected_pilots.push(cid);
                    }
                    if t_idx == win_team_idx {
                        let st = store.stats.entry(cid).or_insert((0, 0, 0, 0, 0.0));
                        st.0 += 1;
                        st.2 += PROVING_WIN_POINTS;
                        let est = store
                            .event_stats
                            .entry(ev_id)
                            .or_default()
                            .entry(cid)
                            .or_insert((0, 0, 0, 0, 0.0));
                        est.0 += 1;
                        est.2 += PROVING_WIN_POINTS;
                    } else {
                        let st = store.stats.entry(cid).or_insert((0, 0, 0, 0, 0.0));
                        st.1 += 1;
                        st.2 += PROVING_LOSS_POINTS;
                        let est = store
                            .event_stats
                            .entry(ev_id)
                            .or_default()
                            .entry(cid)
                            .or_insert((0, 0, 0, 0, 0.0));
                        est.1 += 1;
                        est.2 += PROVING_LOSS_POINTS;
                    }
                }
            }
        }
        ev_id
    } else {
        store.queue.remove(&winner_char_id);
        store.queue.remove(&loser_char_id);
        let ev_id = store.events.last().map(|e| e.event_id).unwrap_or(171);

        let w = store.stats.entry(winner_char_id).or_insert((0, 0, 0, 0, 0.0));
        w.0 += 1;
        w.2 += PROVING_WIN_POINTS;
        w.3 += 1;
        w.4 += damage_done;
        let l = store.stats.entry(loser_char_id).or_insert((0, 0, 0, 0, 0.0));
        l.1 += 1;
        l.2 += PROVING_LOSS_POINTS;

        let ev_stats = store.event_stats.entry(ev_id).or_default();
        let ew = ev_stats.entry(winner_char_id).or_insert((0, 0, 0, 0, 0.0));
        ew.0 += 1;
        ew.2 += PROVING_WIN_POINTS;
        ew.3 += 1;
        ew.4 += damage_done;
        let el = ev_stats.entry(loser_char_id).or_insert((0, 0, 0, 0, 0.0));
        el.1 += 1;
        el.2 += PROVING_LOSS_POINTS;
        ev_id
    };

    save_pvp_store(snap, &store);
    if let Some((fil_tid, winners)) = reward_winners {
        credit_proving_grounds_match_rewards_in_snap(snap, sde_opt, fil_tid, &winners);
    }
    Ok((matched_event_id, affected_pilots))
}

fn enqueue_pvp_char_notification(
    db: &eve_db::WorldRepository,
    pending_opt: Option<&std::sync::RwLock<std::collections::HashMap<i64, Vec<crate::router::PendingMachoNotification>>>>,
    notify_tx_opt: Option<&tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>>,
    cid: i64,
    event: &str,
    id_type: &str,
    args: Vec<PyRep>,
) {
    let Some(pending) = pending_opt else {
        return;
    };
    let Some(uid) = db.with_snapshot(|s| s.characters.get(&cid).map(|c| c.user_id)) else {
        return;
    };
    if !db.get_online_session(uid).is_some_and(|os| os.character_id == Some(cid)) {
        return;
    }
    let (packet, marshaled_packet) =
        ServerContext::build_macho_notification_packet(cid, Some(uid), event, id_type, args.clone());
    let created_filetime = (db.clock().now_unix_secs() + 11_644_473_600) * 10_000_000;
    let notif = crate::router::PendingMachoNotification {
        recipient_char_id: cid,
        event: event.to_string(),
        id_type: id_type.to_string(),
        args,
        packet,
        marshaled_packet,
        created_filetime: created_filetime as i64,
    };
    {
        let mut qmap = pending.write().unwrap_or_else(|e| e.into_inner());
        let q = qmap.entry(cid).or_default();
        if q.len() >= crate::router::MAX_PENDING_NOTIFICATIONS_PER_CHAR {
            q.remove(0);
        }
        q.push(notif.clone());
    }
    if let Some(tx) = notify_tx_opt {
        let _ = tx.send((cid, notif));
    }
}

pub fn notify_pvp_stats_for_pilots_in_db(
    db: &eve_db::WorldRepository,
    sde_opt: Option<&eve_sde::SdeRegistry>,
    pending_opt: Option<&std::sync::RwLock<std::collections::HashMap<i64, Vec<crate::router::PendingMachoNotification>>>>,
    notify_tx_opt: Option<&tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>>,
    event_id: i64,
    pilots: &[i64],
) {
    if pilots.is_empty() || pending_opt.is_none() {
        return;
    }
    let (match_type_id, schedule_id, pilot_stats, leaderboard) = db.with_snapshot(|snap| {
        let store = load_pvp_store(snap);
        let (mt_id, sched_id) = store
            .events
            .iter()
            .find(|e| e.event_id == event_id)
            .map(|e| (e.match_type_id, e.schedule_id))
            .or_else(|| sde_opt.and_then(|sde| lookup_sde_event_ids(sde, event_id)))
            .unwrap_or((event_id, event_id));
        let pstats: Vec<(i64, PyRep)> = pilots
            .iter()
            .map(|&cid| (cid, build_character_statistics_dict_for_event(&store, event_id, cid)))
            .collect();
        let lb = build_leaderboard_rows_dict_for_event(&store, event_id);
        (mt_id, sched_id, pstats, lb)
    });
    let lb_notif = PyRep::Dict(vec![
        (PyRep::String("eventID".into()), PyRep::Integer(event_id)),
        (PyRep::String("event_id".into()), PyRep::Integer(event_id)),
        (PyRep::String("matchTypeID".into()), PyRep::Integer(match_type_id)),
        (PyRep::String("scheduleID".into()), PyRep::Integer(schedule_id)),
        (PyRep::String("entries".into()), PyRep::List(leaderboard)),
    ]);
    for (cid, stats_rep) in pilot_stats {
        let stat_notif = PyRep::Dict(vec![
            (PyRep::String("eventID".into()), PyRep::Integer(event_id)),
            (PyRep::String("event_id".into()), PyRep::Integer(event_id)),
            (PyRep::String("matchTypeID".into()), PyRep::Integer(match_type_id)),
            (PyRep::String("scheduleID".into()), PyRep::Integer(schedule_id)),
            (PyRep::String("statistics".into()), stats_rep),
        ]);
        enqueue_pvp_char_notification(
            db,
            pending_opt,
            notify_tx_opt,
            cid,
            "OnPVPFilamentsCharacterStatistics",
            "charid",
            vec![stat_notif],
        );
        enqueue_pvp_char_notification(
            db,
            pending_opt,
            notify_tx_opt,
            cid,
            "OnPVPFilamentsLeaderboard",
            "charid",
            vec![lb_notif.clone()],
        );
    }
}

/// Real-time 1 Hz Proving Grounds arena boundary (`75 km` sphere around each match's `arena_offset`)
/// and arena collapse timer (`900 s`) escalating environmental damage tick.
pub fn step_proving_grounds_arena_hazards_in_ballpark(
    ballpark: &mut eve_destiny::Ballpark,
    sde_opt: Option<&eve_sde::SdeRegistry>,
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    solar_system_id: eve_sde::SolarSystemId,
    pending_notifications_opt: Option<&std::sync::RwLock<std::collections::HashMap<i64, Vec<crate::router::PendingMachoNotification>>>>,
    notify_tx_opt: Option<&tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>>,
) -> Vec<i64> {
    let mut destroyed_balls = Vec::new();
    if solar_system_id != 32_000_001 {
        return destroyed_balls;
    }
    let Some(db) = db_opt else {
        return destroyed_balls;
    };
    let now_unix = db.clock().now_unix_secs();
    let mut respawned_pilots: Vec<(i64, i64, i64, u32, i64)> = Vec::new();
    let mut ejected_pods: Vec<(i64, i64, i64)> = Vec::new();
    let mut concluded_matches: Vec<(i64, Vec<i64>)> = Vec::new();

    crate::sp6_lifecycle::with_mut_sp6_state(db, |sp6, snap| {
        let mut store = load_pvp_store(snap);
        if store.active_matches.is_empty() {
            return;
        }
        let match_ids: Vec<i64> = store
            .active_matches
            .values()
            .filter(|m| m.arena_solar_system_id == 32_000_001)
            .map(|m| m.match_id)
            .collect();
        let mut store_dirty = false;
        let mut pending_rewards: Vec<(u32, Vec<i64>)> = Vec::new();

        for mid in match_ids {
            let Some(m_snap) = store.active_matches.get(&mid).cloned() else {
                continue;
            };
            let teams = effective_match_teams(&store, &m_snap);
            let center = store
                .match_arena_offsets
                .get(&mid)
                .copied()
                .map(DVec3::from_array)
                .or_else(|| ballpark.balls.get(&m_snap.end_gate_id).map(|b| b.position))
                .unwrap_or(DVec3::ZERO);
            let exceeded_timer =
                now_unix > m_snap.started_unix.saturating_add(PROVING_ARENA_DURATION_SECS);

            let mut newly_eliminated: Vec<(i64, i64, u32, DVec3)> = Vec::new();
            for &cid in teams.iter().flatten() {
                if m_snap.exited_pilots.contains(&cid) {
                    continue;
                }
                let already_eliminated = store
                    .match_eliminated
                    .get(&mid)
                    .is_some_and(|e| e.contains(&cid));
                let Some(ship_id) = snap
                    .characters
                    .get(&cid)
                    .filter(|c| c.solar_system_id == 32_000_001)
                    .map(|c| c.active_ship_id)
                    .filter(|&sid| sid > 0)
                else {
                    continue;
                };
                let Some((ship_pos, ship_type_id)) = ballpark
                    .balls
                    .get(&ship_id)
                    .map(|b| (b.position, b.type_id))
                else {
                    continue;
                };
                if already_eliminated && ship_type_id != 670 {
                    continue;
                }
                let outside_sphere = ship_pos.distance(center) > PROVING_ARENA_BOUNDARY_RADIUS_M;
                if !outside_sphere && !exceeded_timer {
                    if store.hazard_ticks.remove(&ship_id).is_some() {
                        store_dirty = true;
                    }
                    continue;
                }
                let next_tick = store.hazard_ticks.get(&ship_id).copied().unwrap_or(0) + 1;
                store.hazard_ticks.insert(ship_id, next_tick);
                store_dirty = true;

                let mut rem_dmg =
                    PROVING_HAZARD_BASE_DPS * (1.0 + PROVING_HAZARD_RAMP_PER_TICK * f64::from(next_tick - 1));
                let mut ship_dead = false;
                if let Some(cs) = ballpark.combat_states.get_mut(&ship_id) {
                    let sh = rem_dmg.min(cs.shield_hp.max(0.0));
                    cs.shield_hp = (cs.shield_hp - sh).max(0.0);
                    rem_dmg -= sh;
                    if rem_dmg > 0.0 {
                        let ar = rem_dmg.min(cs.armor_hp.max(0.0));
                        cs.armor_hp = (cs.armor_hp - ar).max(0.0);
                        rem_dmg -= ar;
                    }
                    if rem_dmg > 0.0 {
                        cs.structure_hp = (cs.structure_hp - rem_dmg).max(0.0);
                    }
                    if cs.structure_hp <= 0.0 {
                        ship_dead = true;
                    }
                }
                if ship_dead {
                    store.hazard_ticks.remove(&ship_id);
                    ballpark.remove_ball(ship_id);
                    destroyed_balls.push(ship_id);
                    newly_eliminated.push((cid, ship_id, ship_type_id, ship_pos));
                }
            }

            if newly_eliminated.is_empty() {
                continue;
            }
            for (cid, ship_id, ship_type_id, pos) in newly_eliminated {
                let base_price = sde_opt
                    .and_then(|sde| sde.types.get(&ship_type_id))
                    .map(|t| t.base_price)
                    .unwrap_or(250_000.0);
                snap.items.remove(&ship_id);
                snap.item_positions.remove(&ship_id);
                snap.module_online_states.remove(&ship_id);
                sp6.ship_combat_hp.remove(&ship_id);
                snap.items.retain(|_, it| it.location_id != ship_id);
                if ship_type_id != 670 {
                    snap.next_entity_id += 1;
                    let kid = snap.next_entity_id;
                    snap.record_killmail(
                        eve_db::KillmailRecord {
                            kill_id: kid,
                            victim_character_id: cid,
                            attacker_character_id: 0,
                            attacker_corp_id: 1_000_298,
                            solar_system_id: 32_000_001,
                            ship_type_id,
                            total_value_isk: base_price,
                            ..Default::default()
                        },
                        now_unix,
                    );
                    snap.next_entity_id += 1;
                    let cap_id = snap.next_entity_id;
                    snap.items.insert(
                        cap_id,
                        eve_db::InventoryItemRecord {
                            item_id: cap_id,
                            type_id: 670,
                            owner_id: cid,
                            location_id: 32_000_001,
                            flag: eve_db::InventoryFlag::None,
                            quantity: 1,
                            singleton: true,
                            ..Default::default()
                        },
                    );
                    snap.item_positions.insert(cap_id, [pos.x, pos.y, pos.z]);
                    if let Some(ch) = snap.characters.get_mut(&cid) {
                        ch.active_ship_id = cap_id;
                    }
                    let (mass, radius, inertia, max_vel, shield, armor, hull, sig) = sde_opt
                        .and_then(|sde| sde.types.get(&670))
                        .map(|t| {
                            let a = &t.default_attributes;
                            (
                                t.mass.max(12_000.0),
                                t.radius.max(18.0),
                                a.get(&eve_sde::attr_ids::AGILITY).copied().unwrap_or(0.5),
                                a.get(&eve_sde::attr_ids::MAX_VELOCITY).copied().unwrap_or(250.0),
                                a.get(&eve_sde::attr_ids::SHIELD_CAPACITY).copied().unwrap_or(250.0),
                                a.get(&eve_sde::attr_ids::ARMOR_HP).copied().unwrap_or(250.0),
                                a.get(&eve_sde::attr_ids::HP_STRUCT).copied().unwrap_or(250.0),
                                a.get(&eve_sde::attr_ids::SIGNATURE_RADIUS).copied().unwrap_or(25.0),
                            )
                        })
                        .unwrap_or((12_000.0, 18.0, 0.5, 250.0, 250.0, 250.0, 250.0, 25.0));
                    ballpark.add_ball(Ball {
                        id: cap_id,
                        type_id: 670,
                        position: pos,
                        velocity: DVec3::ZERO,
                        max_velocity: max_vel,
                        inertia,
                        mass,
                        radius,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    });
                    ballpark.combat_states.insert(
                        cap_id,
                        eve_destiny::CombatState {
                            shield_hp: shield,
                            max_shield_hp: shield,
                            armor_hp: armor,
                            max_armor_hp: armor,
                            structure_hp: hull,
                            max_structure_hp: hull,
                            signature_radius: sig,
                            base_signature_radius: Some(sig),
                            ..eve_destiny::CombatState::default()
                        },
                    );
                    ejected_pods.push((cid, ship_id, cap_id));
                } else {
                    let (home_station_id, home_sys_id, corvette_id, _, _) =
                        destroy_pod_and_respawn_faction_corvette_in_snap(
                            snap,
                            sde_opt,
                            cid,
                            32_000_001,
                            [pos.x, pos.y, pos.z],
                            now_unix,
                        );
                    respawned_pilots.push((cid, ship_id, home_station_id, home_sys_id, corvette_id));
                }
                store.match_eliminated.entry(mid).or_default().insert(cid);
                store.queue.remove(&cid);
            }

            if m_snap.active {
                let elim = store.match_eliminated.get(&mid).cloned().unwrap_or_default();
                let surviving_team_indices: Vec<usize> = teams
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, t)| t.iter().any(|&c| !elim.contains(&c)).then_some(idx))
                    .collect();
                if surviving_team_indices.len() <= 1 {
                    let win_team_idx = surviving_team_indices.first().copied();
                    let winner_cid = win_team_idx.and_then(|ti| {
                        teams[ti]
                            .iter()
                            .copied()
                            .find(|c| !elim.contains(c))
                            .or_else(|| teams[ti].first().copied())
                    });
                    if let Some(m) = store.active_matches.get_mut(&mid) {
                        m.active = false;
                        m.winner_char_id = winner_cid;
                    }
                    let ev_id = m_snap.event_id;
                    let mut all_participants = Vec::new();
                    for (t_idx, team) in teams.iter().enumerate() {
                        let is_win = win_team_idx == Some(t_idx);
                        for &cid in team {
                            if !all_participants.contains(&cid) {
                                all_participants.push(cid);
                            }
                            store.queue.remove(&cid);
                            let st = store.stats.entry(cid).or_insert((0, 0, 0, 0, 0.0));
                            let est = store
                                .event_stats
                                .entry(ev_id)
                                .or_default()
                                .entry(cid)
                                .or_insert((0, 0, 0, 0, 0.0));
                            if is_win {
                                st.0 += 1;
                                st.2 += PROVING_WIN_POINTS;
                                est.0 += 1;
                                est.2 += PROVING_WIN_POINTS;
                            } else {
                                st.1 += 1;
                                st.2 += PROVING_LOSS_POINTS;
                                est.1 += 1;
                                est.2 += PROVING_LOSS_POINTS;
                            }
                        }
                    }
                    concluded_matches.push((ev_id, all_participants));
                    if let Some(ti) = win_team_idx {
                        let fil_tid = store.match_filament_types.get(&mid).copied().unwrap_or(0);
                        pending_rewards.push((fil_tid, teams[ti].clone()));
                    }
                }
            }
        }

        if store_dirty {
            save_pvp_store(snap, &store);
        }
        for (fil_tid, winners) in pending_rewards {
            credit_proving_grounds_match_rewards_in_snap(snap, sde_opt, fil_tid, &winners);
        }
    });

    for (cid, old_ship_id, home_station_id, home_sys_id, corvette_id) in respawned_pilots {
        let char_meta = db.with_snapshot(|s| {
            s.characters
                .get(&cid)
                .map(|c| (c.user_id as i64, c.corporation_id, c.alliance_id))
        });
        let (uid_i64, corp_id, alliance_id) = char_meta.unwrap_or((0, 1_000_009, None));
        let sess_rep = PyRep::keyval([
            ("charid", PyRep::Integer(cid)),
            ("userid", PyRep::Integer(uid_i64)),
            ("corpid", PyRep::Integer(corp_id)),
            ("allianceid", alliance_id.map_or(PyRep::None, PyRep::Integer)),
            ("stationid", PyRep::Integer(home_station_id)),
            ("stationid2", PyRep::Integer(home_station_id)),
            ("structureid", PyRep::None),
            ("solarsystemid2", PyRep::Integer(i64::from(home_sys_id))),
            ("solarsystemid", PyRep::None),
            ("shipid", PyRep::Integer(corvette_id)),
        ]);
        let change_dict = PyRep::Dict(vec![
            (
                PyRep::String("solarsystemid".into()),
                PyRep::Tuple(vec![PyRep::Integer(32_000_001), PyRep::None]),
            ),
            (
                PyRep::String("solarsystemid2".into()),
                PyRep::Tuple(vec![PyRep::Integer(32_000_001), PyRep::Integer(i64::from(home_sys_id))]),
            ),
            (
                PyRep::String("stationid".into()),
                PyRep::Tuple(vec![PyRep::None, PyRep::Integer(home_station_id)]),
            ),
            (
                PyRep::String("stationid2".into()),
                PyRep::Tuple(vec![PyRep::None, PyRep::Integer(home_station_id)]),
            ),
            (
                PyRep::String("shipid".into()),
                PyRep::Tuple(vec![PyRep::Integer(old_ship_id), PyRep::Integer(corvette_id)]),
            ),
        ]);
        enqueue_pvp_char_notification(
            db,
            pending_notifications_opt,
            notify_tx_opt,
            cid,
            "OnSessionChanged",
            "charid",
            vec![PyRep::Bool(false), sess_rep, change_dict],
        );
    }
    for (cid, old_ship_id, cap_id) in ejected_pods {
        let char_meta = db.with_snapshot(|s| {
            s.characters
                .get(&cid)
                .map(|c| (c.user_id as i64, c.corporation_id, c.alliance_id))
        });
        let (uid_i64, corp_id, alliance_id) = char_meta.unwrap_or((0, 1_000_009, None));
        let sess_rep = PyRep::keyval([
            ("charid", PyRep::Integer(cid)),
            ("userid", PyRep::Integer(uid_i64)),
            ("corpid", PyRep::Integer(corp_id)),
            ("allianceid", alliance_id.map_or(PyRep::None, PyRep::Integer)),
            ("solarsystemid2", PyRep::Integer(32_000_001)),
            ("solarsystemid", PyRep::Integer(32_000_001)),
            ("shipid", PyRep::Integer(cap_id)),
        ]);
        let change_dict = PyRep::Dict(vec![(
            PyRep::String("shipid".into()),
            PyRep::Tuple(vec![PyRep::Integer(old_ship_id), PyRep::Integer(cap_id)]),
        )]);
        enqueue_pvp_char_notification(
            db,
            pending_notifications_opt,
            notify_tx_opt,
            cid,
            "OnSessionChanged",
            "charid",
            vec![PyRep::Bool(false), sess_rep, change_dict],
        );
    }
    for (ev_id, participants) in concluded_matches {
        notify_pvp_stats_for_pilots_in_db(
            db,
            sde_opt,
            pending_notifications_opt,
            notify_tx_opt,
            ev_id,
            &participants,
        );
    }

    destroyed_balls
}

pub fn notify_pvp_stats_for_pilots(ctx: &ServerContext, event_id: i64, pilots: &[i64]) {
    if pilots.is_empty() {
        return;
    }
    let (match_type_id, schedule_id, pilot_stats, leaderboard) = ctx.db.with_snapshot(|snap| {
        let store = load_pvp_store(snap);
        let (mt_id, sched_id) = store
            .events
            .iter()
            .find(|e| e.event_id == event_id)
            .map(|e| (e.match_type_id, e.schedule_id))
            .unwrap_or_else(|| lookup_sde_event_ids(&ctx.sde, event_id).unwrap_or((event_id, event_id)));
        let pstats: Vec<(i64, PyRep)> = pilots
            .iter()
            .map(|&cid| (cid, build_character_statistics_dict_for_event(&store, event_id, cid)))
            .collect();
        let lb = build_leaderboard_rows_dict_for_event(&store, event_id);
        (mt_id, sched_id, pstats, lb)
    });
    let lb_notif = PyRep::Dict(vec![
        (PyRep::String("eventID".into()), PyRep::Integer(event_id)),
        (PyRep::String("event_id".into()), PyRep::Integer(event_id)),
        (PyRep::String("matchTypeID".into()), PyRep::Integer(match_type_id)),
        (PyRep::String("scheduleID".into()), PyRep::Integer(schedule_id)),
        (PyRep::String("entries".into()), PyRep::List(leaderboard)),
    ]);
    for (cid, stats_rep) in pilot_stats {
        let stat_notif = PyRep::Dict(vec![
            (PyRep::String("eventID".into()), PyRep::Integer(event_id)),
            (PyRep::String("event_id".into()), PyRep::Integer(event_id)),
            (PyRep::String("matchTypeID".into()), PyRep::Integer(match_type_id)),
            (PyRep::String("scheduleID".into()), PyRep::Integer(schedule_id)),
            (PyRep::String("statistics".into()), stats_rep),
        ]);
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnPVPFilamentsCharacterStatistics",
            "charid",
            vec![stat_notif],
        );
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnPVPFilamentsLeaderboard",
            "charid",
            vec![lb_notif.clone()],
        );
    }
}



#[allow(clippy::type_complexity)]
pub async fn spawn_matched_arena_instance(
    ctx: &ServerContext,
    session: &mut SessionState,
    mid: i64,
    end_gate_id: i64,
    arena_offset: [f64; 3],
    teams: Vec<Vec<i64>>,
    participants: Vec<(i64, i64, u32)>,
) {
    session.solar_system_id = Some(32_000_001);
    let arena = ctx.systems.get_or_spawn(32_000_001);
    let center = DVec3::from_array(arena_offset);
    if arena.get_ball_snapshot(end_gate_id).await.ok().flatten().is_none() {
        let gate_ball = Ball {
            id: end_gate_id,
            type_id: 47459,
            position: center,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let _ = arena.spawn_ball_with_combat(gate_ball, Default::default()).await;
    }

    let get_ball_physics = |ship_type_id: u32| -> (f64, f64, f64, f64) {
        ctx.sde
            .types
            .get(&ship_type_id)
            .map(|st| {
                let mv = st
                    .default_attributes
                    .get(&eve_sde::attr_ids::MAX_VELOCITY)
                    .copied()
                    .unwrap_or(250.0);
                let ag = st
                    .default_attributes
                    .get(&eve_sde::attr_ids::AGILITY)
                    .copied()
                    .unwrap_or(0.5);
                (st.mass, st.radius, ag, mv)
            })
            .unwrap_or((1_000_000.0, 50.0, 0.5, 250.0))
    };

    let num_teams = teams.len().max(2);
    for (pid, p_ship_id, p_origin_sys) in participants {
        crate::services::map_service::record_solar_system_visit(ctx, pid, 32_000_001);
        let sess_rep = PyRep::keyval([
            ("charid", PyRep::Integer(pid)),
            ("solarsystemid2", PyRep::Integer(32_000_001)),
            ("solarsystemid", PyRep::Integer(32_000_001)),
            ("matchID", PyRep::Integer(mid)),
        ]);
        ctx.notify(
            NotificationTarget::Character(pid),
            "OnSessionChanged",
            "charid",
            vec![
                PyRep::Bool(false),
                sess_rep,
                PyRep::Dict(vec![(
                    PyRep::String("solarsystemid".into()),
                    PyRep::Tuple(vec![
                        PyRep::Integer(p_origin_sys as i64),
                        PyRep::Integer(32_000_001),
                    ]),
                )]),
            ],
        );
        crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(
            ctx,
            pid,
            p_ship_id,
            p_origin_sys,
        );
        ctx.systems.get_or_spawn(p_origin_sys).try_remove_ball(p_ship_id);

        let p_ship_type = ctx
            .db
            .with_snapshot(|s| s.items.get(&p_ship_id).map(|i| i.type_id).unwrap_or(621));
        let (mass, radius, inertia, max_vel) = get_ball_physics(p_ship_type);
        let cs = ctx.build_ship_combat_state(pid, p_ship_id, p_ship_type);

        let (t_idx, m_idx) = teams
            .iter()
            .enumerate()
            .find_map(|(ti, t)| t.iter().position(|&c| c == pid).map(|mi| (ti, mi)))
            .unwrap_or((0, 0));
        let rel_pos = if num_teams == 2 {
            let x = if t_idx == 0 { -10_000.0 } else { 10_000.0 };
            DVec3::new(x, (m_idx as f64) * 2_000.0, 0.0)
        } else {
            let theta = std::f64::consts::TAU * (t_idx as f64) / (num_teams as f64);
            DVec3::new(
                10_000.0 * theta.cos(),
                10_000.0 * theta.sin() + (m_idx as f64) * 2_000.0,
                0.0,
            )
        };
        let ball = Ball {
            id: p_ship_id,
            type_id: p_ship_type,
            position: center + rel_pos,
            velocity: DVec3::ZERO,
            max_velocity: max_vel,
            inertia,
            mass,
            radius,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let _ = arena.spawn_ball_with_combat(ball, cs).await;
    }
}

pub async fn handle_pvp_end_gate_activation(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    args: &[PyRep],
    now_unix: u64,
) -> Result<PyRep, BridgeRpcError> {
    let gate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);

    let (is_in_arena, active_ship_id) = ctx.db.with_snapshot(|snap| {
        let c = snap.characters.get(&char_id);
        let sys = session
            .solar_system_id
            .or_else(|| c.map(|c| c.solar_system_id))
            .unwrap_or(0);
        let ship = session
            .ship_id
            .or_else(|| c.map(|c| c.active_ship_id))
            .filter(|&s| s > 0);
        (sys == 32_000_001, ship)
    });

    if !is_in_arena {
        return Err(BridgeRpcError::user_error("NotInArena"));
    }

    let match_precheck = ctx.db.with_snapshot(|snap| {
        let store = load_pvp_store(snap);
        store
            .active_matches
            .values()
            .filter(|m| {
                let is_eliminated = store
                    .match_eliminated
                    .get(&m.match_id)
                    .is_some_and(|e| e.contains(&char_id));
                m.arena_solar_system_id == 32_000_001
                    && match_contains_char(&store, m, char_id)
                    && !m.exited_pilots.contains(&char_id)
                    && !is_eliminated
                    && (m.active || m.winner_char_id.is_some())
            })
            .max_by_key(|m| m.match_id)
            .map(|m| {
                let on_winning_side = m.winner_char_id.is_some_and(|wcid| {
                    let teams = effective_match_teams(&store, m);
                    teams
                        .iter()
                        .find(|t| t.contains(&wcid))
                        .map_or(wcid == char_id, |t| t.contains(&char_id))
                });
                (
                    m.match_id,
                    m.end_gate_id,
                    m.winner_char_id,
                    on_winning_side,
                    m.started_unix,
                )
            })
    });

    let Some((mid, end_gate_id, winner_char_id, on_winning_side, m_started_unix)) = match_precheck else {
        return Err(BridgeRpcError::user_error("NoActiveMatch"));
    };

    let match_timed_out = now_unix >= m_started_unix.saturating_add(900);
    if winner_char_id.is_none() {
        let err_code = if match_timed_out {
            "ArenaCollapsed"
        } else {
            "MatchNotConcluded"
        };
        return Err(BridgeRpcError::user_error(err_code));
    }
    if !on_winning_side {
        return Err(BridgeRpcError::user_error("AccessDenied"));
    }

    let target_gate_id = if end_gate_id > 0 { end_gate_id } else { 70_001 };
    if gate_id != target_gate_id {
        return Err(BridgeRpcError::user_error("TooFarFromGate"));
    }

    let arena = ctx.systems.get_or_spawn(32_000_001);
    let Some((_, gb)) = arena.get_ball_snapshot(target_gate_id).await.ok().flatten() else {
        return Err(BridgeRpcError::user_error("TooFarFromGate"));
    };
    let Some(sid) = active_ship_id else {
        return Err(BridgeRpcError::user_error("TooFarFromGate"));
    };
    let Some((_, sb)) = arena.get_ball_snapshot(sid).await.ok().flatten() else {
        return Err(BridgeRpcError::user_error("TooFarFromGate"));
    };
    if sb.position.distance(gb.position) > 50_000.0 {
        return Err(BridgeRpcError::user_error("TooFarFromGate"));
    }

    let match_info = ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        store.queue.remove(&char_id);
        let fallback_sys = resolve_character_home_station_and_system(snap, &ctx.sde, char_id).1;
        let opos = store
            .origin_positions
            .get(&char_id)
            .copied()
            .unwrap_or([0.0, 0.0, 0.0]);
        let teams = store
            .active_matches
            .get(&mid)
            .map(|m| effective_match_teams(&store, m))
            .unwrap_or_default();
        let elim = store.match_eliminated.get(&mid).cloned().unwrap_or_default();

        let mut matched_entry = None;
        let mut cleanup_gate_id = None;
        if let Some(m) = store.active_matches.get_mut(&mid) {
            if !m.exited_pilots.contains(&char_id) {
                let osys = m.origin_systems.get(&char_id).copied().unwrap_or(fallback_sys);
                m.exited_pilots.insert(char_id);
                let all_done = teams
                    .iter()
                    .flatten()
                    .all(|cid| m.exited_pilots.contains(cid) || elim.contains(cid));
                if all_done {
                    m.active = false;
                    cleanup_gate_id = Some(target_gate_id);
                }
                matched_entry = Some((m.match_id, osys, opos));
            }
        }
        if cleanup_gate_id.is_some() {
            store.match_arena_offsets.remove(&mid);
        }
        let Some((mid, osys, opos)) = matched_entry else {
            save_pvp_store(snap, &store);
            return None;
        };
        let active_ship_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.active_ship_id)
            .filter(|&s| s > 0);
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = osys;
        }
        if let Some(sid) = active_ship_id {
            if let Some(sh) = snap.items.get_mut(&sid) {
                sh.location_id = osys as i64;
            }
            snap.item_positions.insert(sid, opos);
        }
        save_pvp_store(snap, &store);
        Some((mid, osys, opos, active_ship_id, cleanup_gate_id))
    });

    let Some((_mid, osys, opos, active_ship_id, cleanup_gate_id)) = match_info else {
        return Err(BridgeRpcError::user_error("NoActiveMatch"));
    };

    session.solar_system_id = Some(osys);
    crate::services::map_service::record_solar_system_visit(ctx, char_id, osys);
    let cur_ship = active_ship_id.or(session.ship_id).unwrap_or(char_id);
    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, cur_ship, 32_000_001);
    arena.try_remove_ball(cur_ship);
    if let Some(gid) = cleanup_gate_id.filter(|&g| g > 70_001) {
        arena.try_remove_ball(gid);
    }
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        if let Some(st) = cw.states.get_mut(&char_id) {
            st.pvp_timer_until_unix = 0;
            st.weapon_timer_until_unix = 0;
        }
    }
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _| {
        st.pvp_timers_until.remove(&char_id);
        st.weapon_timers_until.remove(&char_id);
    });

    let origin_sys_handle = ctx.systems.get_or_spawn(osys);
    let ship_type = ctx
        .db
        .with_snapshot(|s| s.items.get(&cur_ship).map(|i| i.type_id).unwrap_or(621));
    let (mass, radius, inertia, max_vel) = ctx
        .sde
        .types
        .get(&ship_type)
        .map(|st| {
            let mv = st
                .default_attributes
                .get(&eve_sde::attr_ids::MAX_VELOCITY)
                .copied()
                .unwrap_or(250.0);
            let ag = st
                .default_attributes
                .get(&eve_sde::attr_ids::AGILITY)
                .copied()
                .unwrap_or(0.5);
            (st.mass, st.radius, ag, mv)
        })
        .unwrap_or((1_000_000.0, 50.0, 0.5, 250.0));
    let cs = ctx.build_ship_combat_state(char_id, cur_ship, ship_type);
    let ball = Ball {
        id: cur_ship,
        type_id: ship_type,
        position: DVec3::from_array(opos),
        velocity: DVec3::ZERO,
        max_velocity: max_vel,
        inertia,
        mass,
        radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = origin_sys_handle.spawn_ball_with_combat(ball, cs).await;

    let sess_rep = PyRep::keyval([
        ("charid", PyRep::Integer(char_id)),
        ("solarsystemid2", PyRep::Integer(osys as i64)),
        ("solarsystemid", PyRep::Integer(osys as i64)),
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
                    PyRep::Integer(32_000_001),
                    PyRep::Integer(osys as i64),
                ]),
            )]),
        ],
    );

    Ok(PyRep::keyval([
        ("gateID", PyRep::Integer(if gate_id > 0 { gate_id } else { target_gate_id })),
        ("activated", PyRep::Bool(true)),
        ("originSystemID", PyRep::Integer(osys as i64)),
    ]))
}

#[cfg(test)] #[path = "pvp_filament_arena_tests.rs"] mod pvp_filament_arena_tests;

