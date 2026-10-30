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
    pending_notifications: &std::sync::RwLock<
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
            recipient_char_id: char_id,
            event: "OnAccountChange".into(),
            id_type: "charid".into(),
            args,
            packet,
            marshaled_packet,
            created_filetime: now_ft,
        };
        if let Ok(mut map) = pending_notifications.write() {
            map.entry(char_id).or_default().push(notif.clone());
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
                        let mult = if fleet_count == site.optimal_fleet_size {
                            1.0
                        } else if fleet_count < site.optimal_fleet_size {
                            (fleet_count as f64) / (site.optimal_fleet_size as f64)
                        } else {
                            0.75
                        };
                        let reward_paid = ((site.base_pilot_isk as f64) * mult).round() as i64;
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
                let j_id = (snap.wallet_journal.len() as i64) + 965_000;
                let mut ship_id = 0;
                if let Some(ch) = snap.characters.get_mut(&cid) {
                    let g = ch.standings.entry(g_fac).or_insert(0.0);
                    *g = (*g + 0.15).clamp(-10.0, 10.0);
                    let l = ch.standings.entry(l_fac).or_insert(0.0);
                    *l = (*l - 0.15).clamp(-10.0, 10.0);
                    ch.balance = ch
                        .balance
                        .checked_add(IskCents::from_isk(base_isk))
                        .unwrap_or(ch.balance);
                    let new_bal = ch.balance;
                    acct_changes.push((cid, new_bal.to_wire_f64()));
                    ship_id = ch.active_ship_id;
                    snap.wallet_journal.push(eve_db::WalletJournalEntry {
                        entry_id: j_id,
                        character_id: cid,
                        entry_type: "CorporateRewardPayout".into(),
                        amount: IskCents::from_isk(base_isk),
                        balance_after: new_bal,
                        reason: "Triglavian Site Payout".into(),
                    });
                }
                deposit_loot_into_ship_or_container_in_snapshot(
                    snap,
                    _sde,
                    cid,
                    solar_system_id,
                    ship_id,
                    &[(loot_id, 48_121, db_drop as i32)],
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
                reg.next_seq += 1;
                let elem_id = 1_899_000_000 + reg.next_seq;
                reg.next_seq += 1;
                let blue_id = 1_899_100_000 + reg.next_seq;
                (elem_tid, elem_id, blue_id)
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

    if let Some((elem_tid, elem_id, blue_id)) = hive_loot {
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
                    &[(elem_id, elem_tid, 1), (blue_id, 30_745, 5)],
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
    acct_changes
}
