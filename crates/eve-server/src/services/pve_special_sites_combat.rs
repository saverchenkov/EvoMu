//! `pve_special_sites_combat` — Special-Site NPC Destruction Handlers (Homefront Operations,
//! Triglavian Sites, Drifter Hives, Abyssal Bioadaptive Caches, and CRAB Rogue Drone Waves).
//!
// parity: feat:pve.homefront-ops, feat:pve.drifter-hives, feat:pve.triglavian-sites

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_incursion_sites::make_site_npc_ball;
use crate::services::pve_special_sites::{
    build_special_site_npc_combat_state, deposit_loot_into_ship_or_container_in_snapshot,
    with_pve_special_reg, with_pve_special_reg_db, DrifterHiveRecord,
};
use eve_db::lock_rank::AnyRwLock;
use eve_db::{IskCents, WorldRepository};
use eve_proto::PyRep;
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::sync::Arc;

type CharacterId = i64;

pub fn get_drifter_hive_for_system(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> Option<DrifterHiveRecord> {
    with_pve_special_reg(ctx, |reg| reg.drifter_hives.get(&sys_id).cloned())
}

pub fn get_all_drifter_hives(ctx: &ServerContext) -> Vec<DrifterHiveRecord> {
    with_pve_special_reg(ctx, |reg| reg.drifter_hives.values().cloned().collect())
}

pub fn notify_special_site_account_changes(
    db: &WorldRepository,
    pending_notifications: &dyn AnyRwLock<
        std::collections::HashMap<i64, Vec<crate::router::cache_notifications::PendingMachoNotification>>,
    >,
    notify_tx: &tokio::sync::broadcast::Sender<(
        i64,
        crate::router::cache_notifications::PendingMachoNotification,
    )>,
    changes: &[(i64, f64)],
) {
    if changes.is_empty() {
        return;
    }
    let now_ft = db.clock().now_blueos_filetime_ticks();
    for &(char_id, new_balance_isk) in changes {
        let args = vec![
            PyRep::String("cash".into()),
            PyRep::Integer(char_id),
            PyRep::Real(new_balance_isk),
        ];
        let (packet, marshaled_packet) =
            crate::router::ServerContext::build_macho_notification_packet(
                char_id,
                None,
                "OnAccountChange",
                "charid",
                args.clone(),
            );
        let notif = crate::router::cache_notifications::PendingMachoNotification {
            seq_id: crate::router::cache_notifications::next_notification_seq_id(),
            recipient_char_id: char_id,
            event: "OnAccountChange".into(),
            id_type: "charid".into(),
            args,
            packet,
            marshaled_packet,
            created_filetime: now_ft,
        };
        if let Ok(mut map) = pending_notifications.write() {
            crate::router::cache_notifications::push_pending_notification_capped(
                map.entry(char_id).or_default(),
                notif.clone(),
            );
        }
        let _ = notify_tx.send((char_id, notif));
    }
}

pub async fn on_special_site_npc_destroyed_core(
    _systems: &Arc<crate::system_actor::SystemRegistry>,
    db: &Arc<WorldRepository>,
    _sde: &Arc<eve_sde::SdeRegistry>,
    solar_system_id: SolarSystemId,
    npc_ball_id: i64,
    killer_char_id: Option<CharacterId>,
) -> Vec<(CharacterId, f64)> {
    let now_sec = db.clock().now_unix_secs();
    let mut acct_changes = Vec::new();

    // 0. Check Homefront sites when destroyed with attacker attribution in space
    if let Some(cid) = killer_char_id {
        let fleet_pilots: Vec<CharacterId> = db.with_snapshot(|snap| {
            let fleet_id = snap.characters.get(&cid).and_then(|c| c.fleet_id);
            if let Some(fid) = fleet_id {
                let mut members: Vec<CharacterId> = snap
                    .characters
                    .values()
                    .filter(|c| {
                        c.fleet_id == Some(fid)
                            && c.solar_system_id == solar_system_id
                            && c.station_id.is_none()
                            && c.structure_id.is_none()
                    })
                    .map(|c| c.character_id)
                    .collect();
                if !members.contains(&cid) {
                    members.push(cid);
                }
                members.sort_unstable();
                members
            } else {
                vec![cid]
            }
        });
        let fleet_count = fleet_pilots.len().max(1);
        let hf_res = with_pve_special_reg_db(db, now_sec, |reg| {
            if let Some(list) = reg.homefront_sites.get_mut(&solar_system_id) {
                for site in list.iter_mut() {
                    if site.boss_npc_ball_id == npc_ball_id && !site.completed {
                        let reward_paid =
                            crate::services::pve_special_sites_ops::compute_homefront_combat_pilot_isk(
                                site.base_pilot_isk,
                                site.optimal_fleet_size,
                                fleet_count,
                            );
                        site.completed = true;
                        site.reward_paid_isk = reward_paid;
                        return Some((site.dungeon_name.clone(), reward_paid));
                    }
                }
            }
            None
        });
        if let Some((dungeon_name, reward_paid)) = hf_res {
            db.with_mut_snapshot(|snap| {
                for &pid in &fleet_pilots {
                    if let Some(ch) = snap.characters.get_mut(&pid) {
                        ch.balance = ch
                            .balance
                            .checked_add(IskCents::from_isk(reward_paid))
                            .unwrap_or(ch.balance);
                        let new_bal = ch.balance;
                        acct_changes.push((pid, new_bal.to_wire_f64()));
                        snap.append_journal(
                            pid,
                            "CorporateRewardPayout",
                            IskCents::from_isk(reward_paid),
                            new_bal,
                            format!("Homefront Operation Payout ({dungeon_name})"),
                        );
                    }
                }
            });
            return acct_changes;
        }
    }

    // 1. Check Triglavian sites
    let mut trig_escorts_to_remove = Vec::new();
    let trig_res = with_pve_special_reg_db(db, now_sec, |reg| {
        let mut found = None;
        if let Some(list) = reg.triglavian_sites.get_mut(&solar_system_id) {
            for site in list.iter_mut() {
                if site.boss_npc_ball_id == npc_ball_id && !site.completed {
                    site.completed = true;
                    trig_escorts_to_remove = site.escort_ball_ids.clone();
                    let (g_fac, l_fac) = if site.faction_owner_id == 500_027 {
                        (500_026_i64, 500_027_i64)
                    } else {
                        (500_027_i64, 500_026_i64)
                    };
                    found = Some((site.instance_id, site.base_isk, site.survey_db_drop, g_fac, l_fac));
                    break;
                }
            }
        }
        found.map(|(inst_id, base_isk, survey_db_drop, g_fac, l_fac)| {
            reg.next_seq += 1;
            let loot_id = 1_899_500_000 + reg.next_seq;
            (inst_id, base_isk, survey_db_drop, g_fac, l_fac, loot_id)
        })
    });

    if let Some((_inst_id, base_isk, db_drop, g_fac, l_fac, loot_id)) = trig_res {
        if !trig_escorts_to_remove.is_empty() {
            let sys_handle = _systems.get_or_spawn(solar_system_id);
            for ebid in trig_escorts_to_remove {
                sys_handle.try_remove_ball(ebid);
            }
        }
        if let Some(cid) = killer_char_id {
            let spawned_cid = db.with_mut_snapshot(|snap| {
                let (new_bal_opt, _) =
                    crate::services::pve_special_sites_ops::apply_triglavian_standings_and_isk(
                        snap,
                        cid,
                        g_fac,
                        l_fac,
                        base_isk,
                        "Triglavian Site Payout",
                    );
                if let Some(nb) = new_bal_opt {
                    acct_changes.push((cid, nb));
                }
                let ship_id = snap
                    .characters
                    .get(&cid)
                    .map(|c| c.active_ship_id)
                    .unwrap_or(0);
                deposit_loot_into_ship_or_container_in_snapshot(
                    snap,
                    _sde,
                    cid,
                    solar_system_id,
                    ship_id,
                    &[
                        (loot_id, 48_121, db_drop as i32),
                        (loot_id + 1_000_000, 30_747, 2),
                    ],
                )
            });
            if let Some(container_id) = spawned_cid {
                let sys_handle = _systems.get_or_spawn(solar_system_id);
                let _ = sys_handle
                    .spawn_ball(make_site_npc_ball(container_id, 23, DVec3::new(10_500.0, 0.0, 0.0)))
                    .await;
            }
        }
        return acct_changes;
    }

    // 2. Check Drifter Hives
    let drifter_res = with_pve_special_reg_db(db, now_sec, |reg| {
        let mut spawn_next = None;
        let mut hive_loot = None;
        if let Some(hive) = reg.drifter_hives.get_mut(&solar_system_id) {
            if hive.warden_npc_ball_id == npc_ball_id && !hive.cleared {
                hive.cleared = true;
                hive.current_room = hive.total_rooms;
                if let Some(r) = hive.rooms.iter_mut().find(|r| r.room_index == hive.total_rooms) {
                    r.cleared = true;
                    if !r.destroyed_npc_ball_ids.contains(&npc_ball_id) {
                        r.destroyed_npc_ball_ids.push(npc_ball_id);
                    }
                }
                hive_loot = Some(hive.element_type_id);
            } else if !hive.cleared {
                let cur_room_idx = hive.current_room;
                if let Some(r) = hive
                    .rooms
                    .iter_mut()
                    .find(|r| r.room_index == cur_room_idx && !r.cleared)
                {
                    if r.npc_ball_ids.contains(&npc_ball_id) {
                        if !r.destroyed_npc_ball_ids.contains(&npc_ball_id) {
                            r.destroyed_npc_ball_ids.push(npc_ball_id);
                        }
                        let wave_cleared = r
                            .npc_ball_ids
                            .iter()
                            .all(|bid| r.destroyed_npc_ball_ids.contains(bid));
                        if wave_cleared {
                            r.cleared = true;
                            hive.current_room += 1;
                            let next_room_idx = hive.current_room;
                            if let Some(next_r) = hive
                                .rooms
                                .iter()
                                .find(|nr| nr.room_index == next_room_idx)
                                .cloned()
                            {
                                spawn_next = Some(next_r);
                            }
                        }
                    }
                }
            }
        }
        (
            spawn_next,
            hive_loot.map(|elem_tid| {
                crate::services::pve_special_sites_ops::allocate_drifter_hive_bonus_loot(
                    reg, elem_tid,
                )
            }),
        )
    });

    // Abyssal Bioadaptive Cache destroyed: wreck + loot transfer
    crate::services::pve_special_sites_abyssal::on_abyssal_cache_destroyed_core(
        _systems,
        db,
        solar_system_id,
        npc_ball_id,
        killer_char_id,
    )
    .await;
    // CRAB Rogue Drone wave NPC destroyed: advance next wave if cleared early
    crate::services::pve_crab_beacons::on_crab_wave_npc_destroyed(
        _systems,
        db,
        _sde,
        solar_system_id,
        npc_ball_id,
    )
    .await;

    let (spawn_next, hive_loot) = drifter_res;
    if let Some(next_room) = spawn_next {
        let sys_handle = _systems.get_or_spawn(solar_system_id);
        for (&bid, &tid) in next_room.npc_ball_ids.iter().zip(&next_room.npc_type_ids) {
            _systems.claim_special_site_ball(solar_system_id, bid);
            let cs = build_special_site_npc_combat_state(_sde, tid);
            let _ = sys_handle
                .spawn_ball_with_combat(
                    make_site_npc_ball(bid, tid, DVec3::new(10_000.0, 0.0, 0.0)),
                    cs,
                )
                .await;
        }
        if next_room.beacon_ball_id != 0 {
            _systems.claim_special_site_ball(solar_system_id, next_room.beacon_ball_id);
            let _ = sys_handle
                .spawn_ball(make_site_npc_ball(
                    next_room.beacon_ball_id,
                    42_538,
                    DVec3::new(12_000.0, 0.0, 0.0),
                ))
                .await;
        }
    }

    if let Some(hive_items) = hive_loot {
        if let Some(cid) = killer_char_id {
            let spawned_cid = db.with_mut_snapshot(|snap| {
                let ship_id = snap
                    .characters
                    .get(&cid)
                    .map(|c| c.active_ship_id)
                    .unwrap_or(0);
                deposit_loot_into_ship_or_container_in_snapshot(
                    snap,
                    _sde,
                    cid,
                    solar_system_id,
                    ship_id,
                    &hive_items,
                )
            });
            if let Some(container_id) = spawned_cid {
                let sys_handle = _systems.get_or_spawn(solar_system_id);
                let _ = sys_handle
                    .spawn_ball(make_site_npc_ball(container_id, 23, DVec3::new(10_500.0, 0.0, 0.0)))
                    .await;
            }
        }
    }

    // 3. Check Seasonal Event sites
    let mut seasonal_escorts = Vec::new();
    let seasonal_res = with_pve_special_reg_db(db, now_sec, |reg| {
        let mut found = None;
        if let Some(list) = reg.seasonal_event_sites.get_mut(&solar_system_id) {
            for site in list.iter_mut() {
                if site.boss_npc_ball_id == npc_ball_id && !site.completed {
                    site.completed = true;
                    seasonal_escorts = site.escort_ball_ids.clone();
                    found = Some((
                        site.dungeon_id,
                        site.instance_id,
                        site.dungeon_name.clone(),
                        site.reward_isk,
                        site.reward_points,
                        site.loot_type_id,
                    ));
                    break;
                }
            }
        }
        found.map(|(did, iid, dname, isk, pts, ltid)| {
            reg.next_seq += 1;
            let loot_item_id = 1_895_800_000 + reg.next_seq;
            (did, iid, dname, isk, pts, ltid, loot_item_id)
        })
    });

    if let Some((did, iid, dname, reward_isk, reward_points, loot_tid, loot_item_id)) = seasonal_res {
        if !seasonal_escorts.is_empty() {
            let sys_handle = _systems.get_or_spawn(solar_system_id);
            for ebid in seasonal_escorts {
                sys_handle.try_remove_ball(ebid);
            }
        }
        let anom_key = format!("dng-{did}-{iid}");
        let spawned_cid = db.with_mut_snapshot(|snap| {
            if let Some(list) = snap.cosmic_anomalies.get_mut(&solar_system_id) {
                for a in list.iter_mut() {
                    if a.anomaly_id == anom_key {
                        a.cleared = true;
                    }
                }
            }
            if let Some(cid) = killer_char_id {
                let mut ship_id = 0;
                if let Some(ch) = snap.characters.get_mut(&cid) {
                    ch.balance = ch
                        .balance
                        .checked_add(IskCents::from_isk(reward_isk))
                        .unwrap_or(ch.balance);
                    let new_bal = ch.balance;
                    acct_changes.push((cid, new_bal.to_wire_f64()));
                    ship_id = ch.active_ship_id;
                    snap.append_journal(
                        cid,
                        "BountyPrizes",
                        IskCents::from_isk(reward_isk),
                        new_bal,
                        format!("Seasonal Event Site Completion ({dname})"),
                    );
                }
                let mut st = crate::services::season_manager::load_season_state_from_blobs(snap, cid);
                st.season_points = st.season_points.saturating_add(reward_points);
                crate::services::season_manager::save_season_state_to_blobs(snap, cid, &st);
                let mut loot_specs = vec![(loot_item_id, loot_tid, 1)];
                let booster_tid = if _sde.types.contains_key(&9950) { 9950 } else { 15457 };
                if booster_tid != loot_tid && _sde.types.contains_key(&booster_tid) {
                    loot_specs.push((loot_item_id + 1, booster_tid, 1));
                }
                deposit_loot_into_ship_or_container_in_snapshot(
                    snap,
                    _sde,
                    cid,
                    solar_system_id,
                    ship_id,
                    &loot_specs,
                )
            } else {
                None
            }
        });
        if let Some(container_id) = spawned_cid {
            let cont_type = if _sde.types.contains_key(&17996) { 17996 } else { 23 };
            let sys_handle = _systems.get_or_spawn(solar_system_id);
            let _ = sys_handle
                .spawn_ball(make_site_npc_ball(container_id, cont_type, DVec3::new(1.5e11, 2.0e10, -1.0e11)))
                .await;
        }
    }

    acct_changes
}

/// Spawns seasonal event sites in `sys_id` during an active seasonal event window and despawns expired ones.
pub async fn ensure_seasonal_event_sites_for_system(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> Vec<crate::services::pve_special_sites::SeasonalEventSiteRecord> {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let now_sec = ctx.now_unix();
    let season = ctx.db.with_snapshot(|snap| {
        if snap.seasons_and_campaigns.season_configuration.is_some() {
            crate::services::season_fsd::load_season_config(snap)
        } else {
            crate::services::season_fsd::load_seasons_catalog(snap, &ctx.sde, now_ft)
                .into_values()
                .find(|s| now_ft >= s.start_time_ft && now_ft <= s.end_time_ft)
                .unwrap_or_else(|| crate::services::season_fsd::load_season_config(snap))
        }
    });
    let in_window = now_ft >= season.start_time_ft && now_ft <= season.end_time_ft;

    let (expired_balls, expired_anom_ids, mut active_sites) = with_pve_special_reg(ctx, |reg| {
        let mut exp_balls = Vec::new();
        let mut exp_anoms = Vec::new();
        if let Some(list) = reg.seasonal_event_sites.get_mut(&sys_id) {
            list.retain(|s| {
                let valid = !s.completed && now_ft >= s.window_start_ft && now_ft <= s.window_end_ft;
                if !valid {
                    exp_balls.push(s.boss_npc_ball_id);
                    exp_balls.extend_from_slice(&s.escort_ball_ids);
                    exp_anoms.push(format!("dng-{}-{}", s.dungeon_id, s.instance_id));
                }
                valid
            });
        }
        let active = reg.seasonal_event_sites.get(&sys_id).cloned().unwrap_or_default();
        (exp_balls, exp_anoms, active)
    });

    if !expired_balls.is_empty() || !expired_anom_ids.is_empty() {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        for bid in expired_balls {
            sys_handle.try_remove_ball(bid);
        }
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(list) = snap.cosmic_anomalies.get_mut(&sys_id) {
                list.retain(|a| !expired_anom_ids.contains(&a.anomaly_id));
            }
        });
    }

    if in_window && active_sites.is_empty() {
        let (dungeon_id, dungeon_name) = ctx
            .sde
            .dungeons
            .get(&47)
            .map(|d| (d.dungeon_id, d.name.clone()))
            .or_else(|| {
                ctx.sde
                    .dungeons
                    .values()
                    .find(|d| d.archetype_id == Some(45))
                    .map(|d| (d.dungeon_id, d.name.clone()))
            })
            .unwrap_or((47, "Crimson Hand Supply Depot".to_string()));
        let boss_type_id = ctx
            .sde
            .types
            .values()
            .find(|t| t.published && (t.name.contains("Corpus Crimson") || t.group_id == 550))
            .map(|t| t.type_id)
            .unwrap_or(17615);
        let escort_type_id = ctx
            .sde
            .types
            .values()
            .find(|t| t.published && (t.name.contains("Crimson Sentinel") || t.name.contains("Crimson Lord")))
            .map(|t| t.type_id)
            .unwrap_or(17993);
        let loot_type_id = if ctx.sde.types.contains_key(&2838) {
            2838
        } else if ctx.sde.types.contains_key(&2048) {
            2048
        } else {
            34
        };

        let new_site = with_pve_special_reg(ctx, |reg| {
            reg.next_seq += 1;
            let inst_id = 1_895_000_000 + reg.next_seq;
            reg.next_seq += 1;
            let boss_bid = 1_895_500_000 + reg.next_seq;
            reg.next_seq += 1;
            let escort_bid = 1_895_500_000 + reg.next_seq;
            let rec = crate::services::pve_special_sites::SeasonalEventSiteRecord {
                instance_id: inst_id,
                season_id: season.season_id,
                dungeon_id,
                dungeon_name: dungeon_name.clone(),
                solar_system_id: sys_id,
                window_start_ft: season.start_time_ft,
                window_end_ft: season.end_time_ft,
                boss_npc_ball_id: boss_bid,
                boss_type_id,
                escort_ball_ids: vec![escort_bid],
                reward_isk: 750_000,
                reward_points: 25,
                loot_type_id,
                completed: false,
            };
            reg.seasonal_event_sites.entry(sys_id).or_default().push(rec.clone());
            rec
        });

        let pos = DVec3::new(1.5e11, 2.0e10, -1.0e11);
        ctx.db.with_mut_snapshot(|snap| {
            snap.cosmic_anomalies.entry(sys_id).or_default().push(eve_db::CosmicAnomalyRecord {
                anomaly_id: format!("dng-{}-{}", new_site.dungeon_id, new_site.instance_id),
                solar_system_id: sys_id,
                anomaly_type: "SeasonalEventSite".to_string(),
                name: new_site.dungeon_name.clone(),
                position: [pos.x, pos.y, pos.z],
                signal_strength: 1.0,
                ore_types: Vec::new(),
                remaining_volume: 0.0,
                created_unix: now_sec,
                lifetime_seconds: 86_400,
                cleared: false,
            });
        });

        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let boss_ball = make_site_npc_ball(
            new_site.boss_npc_ball_id,
            new_site.boss_type_id,
            pos + DVec3::new(2_500.0, 0.0, 0.0),
        );
        let escort_ball = make_site_npc_ball(
            new_site.escort_ball_ids[0],
            escort_type_id,
            pos + DVec3::new(-2_500.0, 0.0, 0.0),
        );
        let boss_cs = build_special_site_npc_combat_state(&ctx.sde, new_site.boss_type_id);
        let escort_cs = build_special_site_npc_combat_state(&ctx.sde, escort_type_id);
        let _ = sys_handle.spawn_ball_with_combat(boss_ball, boss_cs).await;
        let _ = sys_handle.spawn_ball_with_combat(escort_ball, escort_cs).await;
        active_sites.push(new_site);
    }

    active_sites
}

#[cfg(test)]
#[path = "pve_special_sites_combat_tests.rs"]
mod pve_special_sites_combat_tests;
#[cfg(test)]
#[path = "pve_special_sites_combat_edges_tests.rs"]
mod pve_special_sites_combat_edges_tests;

