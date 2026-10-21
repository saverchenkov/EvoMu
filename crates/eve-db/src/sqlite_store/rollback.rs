//! Dirty snapshot rollback from SQLite for transactional integrity.

use crate::sqlite_store::fingerprints::*;
use crate::types::*;
use eve_sde::{SolarSystemId, TypeId};
use rusqlite::{params, Connection};
use std::collections::HashMap;

pub fn rollback_dirty_from_sqlite(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    fp: &TableFingerprints,
) -> Result<(), String> {
    snap.next_entity_id = fp.next_entity_id;
    snap.wal_seq = fp.wal_seq;
    snap.markets_seeded = fp.markets_seeded;

    // 1. accounts
    snap.accounts.retain(|k, _| fp.accounts.contains_key(k));
    for (k, &expected_h) in &fp.accounts {
        if snap.accounts.get(k).map(fp_account) != Some(expected_h) {
            let row = conn
                .query_row(
                    "SELECT username, user_id, password_token, role, banned_until_unix, ban_reason FROM accounts WHERE username = ?1",
                    params![k],
                    |r| {
                        Ok(AccountRecord {
                            username: r.get(0)?,
                            user_id: r.get::<_, i64>(1)? as u64,
                            password_token: r.get(2)?,
                            role: r.get::<_, i64>(3)? as u64,
                            banned_until_unix: r.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                            ban_reason: r.get(5)?,
                        })
                    },
                )
                .map_err(|e| e.to_string())?;
            snap.accounts.insert(k.clone(), row);
        }
    }

    // 2. characters
    snap.characters.retain(|k, _| fp.characters.contains_key(k));
    for (&k, &expected_h) in &fp.characters {
        if snap.characters.get(&k).map(fp_character) != Some(expected_h) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM characters WHERE character_id = ?1",
                    params![k],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            let ch: CharacterRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.characters.insert(k, ch);
        }
    }

    // 3. items
    snap.items.retain(|k, _| fp.items.contains_key(k));
    for (&k, &expected_h) in &fp.items {
        if snap.items.get(&k).map(fp_item) != Some(expected_h) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM items WHERE item_id = ?1",
                    params![k],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            let item: InventoryItemRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.items.insert(k, item);
        }
    }

    // 4. market_orders
    snap.market_orders
        .retain(|k, _| fp.market_orders.contains_key(k));
    for (&k, &expected_h) in &fp.market_orders {
        if snap.market_orders.get(&k).map(fp_market_order) != Some(expected_h) {
            let ord = conn
                .query_row(
                    "SELECT order_id, owner_id, station_id, region_id, type_id, is_bid, price_cents, vol_entered, vol_remaining, min_volume, is_npc, escrow_cents FROM market_orders WHERE order_id = ?1",
                    params![k],
                    |r| {
                        Ok(MarketOrderRecord {
                            order_id: r.get(0)?,
                            owner_id: r.get(1)?,
                            station_id: r.get(2)?,
                            region_id: r.get::<_, i64>(3)? as u32,
                            type_id: r.get::<_, i64>(4)? as TypeId,
                            is_bid: r.get::<_, i64>(5)? != 0,
                            price: IskCents(r.get(6)?),
                            vol_entered: r.get::<_, i64>(7)? as i32,
                            vol_remaining: r.get::<_, i64>(8)? as i32,
                            min_volume: r.get::<_, i64>(9)? as i32,
                            is_npc: r.get::<_, i64>(10)? != 0,
                            escrow: IskCents(r.get(11)?),
                        })
                    },
                )
                .map_err(|e| e.to_string())?;
            snap.market_orders.insert(k, ord);
        }
    }

    // 5. In-memory direct restores for exact-value tracked sets/maps and append-only logs
    if snap.wallet_journal.len() > fp.wallet_journal_len {
        snap.wallet_journal.truncate(fp.wallet_journal_len);
    } else if snap.wallet_journal.len() < fp.wallet_journal_len {
        snap.wallet_journal.clear();
        let mut stmt = conn
            .prepare("SELECT entry_id, character_id, entry_type, amount_cents, balance_after_cents, reason FROM wallet_journal ORDER BY seq_idx ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(WalletJournalEntry {
                    entry_id: r.get(0)?,
                    character_id: r.get(1)?,
                    entry_type: r.get(2)?,
                    amount: IskCents(r.get(3)?),
                    balance_after: IskCents(r.get(4)?),
                    reason: r.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            snap.wallet_journal.push(r.map_err(|e| e.to_string())?);
        }
    }

    if snap.mails.len() > fp.mails_len {
        snap.mails.truncate(fp.mails_len);
    } else if snap.mails.len() < fp.mails_len {
        snap.mails.clear();
        let mut stmt = conn
            .prepare("SELECT message_id, sender_id, recipient_id, subject, body FROM mail ORDER BY seq_idx ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(MailRecord {
                    message_id: r.get(0)?,
                    sender_id: r.get(1)?,
                    recipient_id: r.get(2)?,
                    subject: r.get(3)?,
                    body: r.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            snap.mails.push(r.map_err(|e| e.to_string())?);
        }
    }

    if snap.committed_idempotency_keys != fp.committed_idempotency_keys {
        snap.committed_idempotency_keys = fp.committed_idempotency_keys.clone();
    }
    if snap.mission_tally != fp.mission_tally {
        snap.mission_tally = fp.mission_tally.clone();
    }
    if snap.applied_ship_skins != fp.applied_ship_skins {
        snap.applied_ship_skins = fp.applied_ship_skins.clone();
    }

    // 6. skills
    snap.skills.retain(|cid, _| fp.skills.contains_key(cid));
    for (&cid, expected_map) in &fp.skills {
        let sk_dirty = match snap.skills.get(&cid) {
            None => true,
            Some(cur_map) => {
                cur_map.len() != expected_map.len()
                    || expected_map.iter().any(|(&tid, &eh)| {
                        cur_map.get(&tid).map(|sk| {
                            let mut h = FnvHasher::new();
                            h.write_u32(sk.type_id);
                            h.write_u8(sk.level);
                            h.write_f64(sk.skill_points);
                            h.finish()
                        }) != Some(eh)
                    })
            }
        };
        if sk_dirty {
            let mut restored = HashMap::new();
            let mut stmt = conn
                .prepare("SELECT type_id, level, skill_points FROM skills WHERE character_id = ?1")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![cid], |r| {
                    Ok(TrainedSkillRecord {
                        type_id: r.get::<_, i64>(0)? as TypeId,
                        level: r.get::<_, i64>(1)? as u8,
                        skill_points: r.get(2)?,
                    })
                })
                .map_err(|e| e.to_string())?;
            for r in rows {
                let sk = r.map_err(|e| e.to_string())?;
                restored.insert(sk.type_id, sk);
            }
            snap.skills.insert(cid, restored);
        }
    }

    // 7. skill_queues
    snap.skill_queues
        .retain(|cid, _| fp.skill_queues.contains_key(cid));
    for (&cid, &eh) in &fp.skill_queues {
        if snap.skill_queues.get(&cid).map(|q| fp_skill_queue(q)) != Some(eh) {
            let mut q = Vec::new();
            let mut stmt = conn
                .prepare("SELECT type_id, target_level, start_sp, end_sp, duration_seconds, started_at_unix FROM skill_queues WHERE character_id = ?1 ORDER BY queue_index ASC")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![cid], |r| {
                    Ok(SkillQueueEntry {
                        type_id: r.get::<_, i64>(0)? as TypeId,
                        target_level: r.get::<_, i64>(1)? as u8,
                        start_sp: r.get(2)?,
                        end_sp: r.get(3)?,
                        duration_seconds: r.get(4)?,
                        started_at_unix: r.get::<_, Option<i64>>(5)?.map(|v| v as u64),
                    })
                })
                .map_err(|e| e.to_string())?;
            for r in rows {
                q.push(r.map_err(|e| e.to_string())?);
            }
            snap.skill_queues.insert(cid, q);
        }
    }

    // 8. jump_clones
    snap.jump_clones
        .retain(|cid, _| fp.jump_clones.contains_key(cid));
    for (&cid, &eh) in &fp.jump_clones {
        if snap.jump_clones.get(&cid).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT clones_json FROM jump_clones WHERE character_id = ?1",
                    params![cid],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.jump_clones
                .insert(cid, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 9. active_implants
    snap.active_implants
        .retain(|cid, _| fp.active_implants.contains_key(cid));
    for (&cid, &eh) in &fp.active_implants {
        if snap.active_implants.get(&cid).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT implants_json FROM active_implants WHERE character_id = ?1",
                    params![cid],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.active_implants
                .insert(cid, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 10. saved_fittings
    snap.saved_fittings
        .retain(|id, _| fp.saved_fittings.contains_key(id));
    for (&id, &eh) in &fp.saved_fittings {
        if snap.saved_fittings.get(&id).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM saved_fittings WHERE fitting_id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.saved_fittings
                .insert(id, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 11. contracts
    snap.contracts.retain(|id, _| fp.contracts.contains_key(id));
    for (&id, &eh) in &fp.contracts {
        if snap.contracts.get(&id).map(fp_contract) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM contracts WHERE contract_id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.contracts
                .insert(id, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 12. industry_jobs
    snap.industry_jobs
        .retain(|id, _| fp.industry_jobs.contains_key(id));
    for (&id, &eh) in &fp.industry_jobs {
        if snap.industry_jobs.get(&id).map(fp_json) != Some(eh) {
            let j = conn
                .query_row(
                    "SELECT job_id, installer_id, facility_id, blueprint_item_id, blueprint_type_id, activity_id, runs, product_type_id, status, duration_seconds, start_unix FROM industry_jobs WHERE job_id = ?1",
                    params![id],
                    |r| {
                        Ok(IndustryJobRecord {
                            job_id: r.get(0)?,
                            installer_id: r.get(1)?,
                            facility_id: r.get(2)?,
                            blueprint_item_id: r.get(3)?,
                            blueprint_type_id: r.get::<_, i64>(4)? as TypeId,
                            activity_id: r.get::<_, i64>(5)? as u8,
                            runs: r.get::<_, i64>(6)? as u32,
                            product_type_id: r.get::<_, i64>(7)? as TypeId,
                            status: r.get(8)?,
                            duration_seconds: r.get::<_, i64>(9)? as u64,
                            start_unix: r.get::<_, i64>(10)? as u64,
                        })
                    },
                )
                .map_err(|e| e.to_string())?;
            snap.industry_jobs.insert(id, j);
        }
    }

    // 13. planetary_colonies
    snap.planetary_colonies
        .retain(|k, _| fp.planetary_colonies.contains_key(k));
    for (&k, &eh) in &fp.planetary_colonies {
        if snap.planetary_colonies.get(&k).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM planetary_colonies WHERE owner_id = ?1 AND planet_id = ?2",
                    params![k.0, k.1],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.planetary_colonies
                .insert(k, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 14. active_missions
    snap.active_missions
        .retain(|k, _| fp.active_missions.contains_key(k));
    for (&k, &eh) in &fp.active_missions {
        if snap.active_missions.get(&k).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM missions WHERE character_id = ?1 AND mission_id = ?2",
                    params![k.0, k.1 as i64],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.active_missions
                .insert(k, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 15. standing_transactions
    snap.standing_transactions
        .retain(|cid, _| fp.standing_transactions.contains_key(cid));
    for (&cid, &eh) in &fp.standing_transactions {
        if snap.standing_transactions.get(&cid).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT tx_json FROM standings WHERE character_id = ?1",
                    params![cid],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.standing_transactions
                .insert(cid, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 16. npe_progress
    snap.npe_progress
        .retain(|cid, _| fp.npe_progress.contains_key(cid));
    for (&cid, &eh) in &fp.npe_progress {
        if snap.npe_progress.get(&cid).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM npe_progress WHERE character_id = ?1",
                    params![cid],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.npe_progress
                .insert(cid, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 17. bookmarks
    snap.bookmarks.retain(|id, _| fp.bookmarks.contains_key(id));
    for (&id, &eh) in &fp.bookmarks {
        if snap.bookmarks.get(&id).map(fp_json) != Some(eh) {
            let bm = conn
                .query_row(
                    "SELECT bookmark_id, owner_id, label, solar_system_id, x, y, z FROM bookmarks WHERE bookmark_id = ?1",
                    params![id],
                    |r| {
                        Ok(BookmarkRecord {
                            bookmark_id: r.get(0)?,
                            owner_id: r.get(1)?,
                            label: r.get(2)?,
                            solar_system_id: r.get::<_, i64>(3)? as SolarSystemId,
                            x: r.get(4)?,
                            y: r.get(5)?,
                            z: r.get(6)?,
                            item_id: None,
                            memo: String::new(),
                            folder_id: None,
                            is_shared: false,
                        })
                    },
                )
                .map_err(|e| e.to_string())?;
            snap.bookmarks.insert(id, bm);
        }
    }

    // 18. fleets
    snap.fleets.retain(|id, _| fp.fleets.contains_key(id));
    for (&id, &eh) in &fp.fleets {
        if snap.fleets.get(&id).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM fleets WHERE fleet_id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.fleets
                .insert(id, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 19. corporations
    snap.corporations
        .retain(|id, _| fp.corporations.contains_key(id));
    for (&id, &eh) in &fp.corporations {
        if snap.corporations.get(&id).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM corps WHERE corporation_id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.corporations
                .insert(id, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 20. alliances
    snap.alliances.retain(|id, _| fp.alliances.contains_key(id));
    for (&id, &eh) in &fp.alliances {
        if snap.alliances.get(&id).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM alliances WHERE alliance_id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.alliances
                .insert(id, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 21. wars
    snap.wars.retain(|id, _| fp.wars.contains_key(id));
    for (&id, &eh) in &fp.wars {
        if snap.wars.get(&id).map(fp_json) != Some(eh) {
            let w = conn
                .query_row(
                    "SELECT war_id, aggressor_corp_id, defender_corp_id, mutual, started_unix, active FROM wars WHERE war_id = ?1",
                    params![id],
                    |r| {
                        Ok(WarRecord {
                            war_id: r.get(0)?,
                            aggressor_corp_id: r.get(1)?,
                            defender_corp_id: r.get(2)?,
                            mutual: r.get::<_, i64>(3)? != 0,
                            started_unix: r.get::<_, i64>(4)? as u64,
                            active: r.get::<_, i64>(5)? != 0,
                        })
                    },
                )
                .map_err(|e| e.to_string())?;
            snap.wars.insert(id, w);
        }
    }

    // 22. corp_projects
    snap.corp_projects
        .retain(|id, _| fp.corp_projects.contains_key(id));
    for (&id, &eh) in &fp.corp_projects {
        if snap.corp_projects.get(&id).map(fp_json) != Some(eh) {
            let p = conn
                .query_row(
                    "SELECT project_id, corporation_id, name, target_quantity, current_quantity, isk_per_unit_cents, escrow_remaining_cents FROM corp_projects WHERE project_id = ?1",
                    params![id],
                    |r| {
                        Ok(CorpProjectRecord {
                            project_id: r.get(0)?,
                            corporation_id: r.get(1)?,
                            name: r.get(2)?,
                            target_quantity: r.get::<_, i64>(3)? as u64,
                            current_quantity: r.get::<_, i64>(4)? as u64,
                            isk_per_unit: IskCents(r.get(5)?),
                            escrow_remaining: IskCents(r.get(6)?),
                        })
                    },
                )
                .map_err(|e| e.to_string())?;
            snap.corp_projects.insert(id, p);
        }
    }

    // 23. killmails
    snap.killmails.retain(|id, _| fp.killmails.contains_key(id));
    for (&id, &eh) in &fp.killmails {
        if snap.killmails.get(&id).map(fp_json) != Some(eh) {
            let km = conn
                .query_row(
                    "SELECT kill_id, victim_character_id, attacker_character_id, solar_system_id, ship_type_id, total_value_isk FROM killmails WHERE kill_id = ?1",
                    params![id],
                    |r| {
                        Ok(KillmailRecord {
                            kill_id: r.get(0)?,
                            victim_character_id: r.get(1)?,
                            attacker_character_id: r.get(2)?,
                            solar_system_id: r.get::<_, i64>(3)? as SolarSystemId,
                            ship_type_id: r.get::<_, i64>(4)? as TypeId,
                            total_value_isk: r.get(5)?,
                        })
                    },
                )
                .map_err(|e| e.to_string())?;
            snap.killmails.insert(id, km);
        }
    }

    // 24. kill_rights
    snap.kill_rights
        .retain(|id, _| fp.kill_rights.contains_key(id));
    for (&id, &eh) in &fp.kill_rights {
        if snap.kill_rights.get(&id).map(fp_json) != Some(eh) {
            let kr = conn
                .query_row(
                    "SELECT kill_right_id, owner_id, target_id, activation_fee_cents, is_active_suspect FROM kill_rights WHERE kill_right_id = ?1",
                    params![id],
                    |r| {
                        Ok(KillRightRecord {
                            kill_right_id: r.get(0)?,
                            owner_id: r.get(1)?,
                            target_id: r.get(2)?,
                            activation_fee: IskCents(r.get(3)?),
                            is_active_suspect: r.get::<_, i64>(4)? != 0,
                        })
                    },
                )
                .map_err(|e| e.to_string())?;
            snap.kill_rights.insert(id, kr);
        }
    }

    // 25. structures
    snap.structures
        .retain(|id, _| fp.structures.contains_key(id));
    for (&id, &eh) in &fp.structures {
        if snap.structures.get(&id).map(fp_structure) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM structures WHERE structure_id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.structures
                .insert(id, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 26. system_states
    snap.system_states
        .retain(|id, _| fp.system_states.contains_key(id));
    for (&id, &eh) in &fp.system_states {
        if snap.system_states.get(&id).map(fp_system_state) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT record_json FROM system_states WHERE solar_system_id = ?1",
                    params![id as i64],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.system_states
                .insert(id, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 27. licensed_skins
    snap.licensed_skins
        .retain(|cid, _| fp.licensed_skins.contains_key(cid));
    for (&cid, &eh) in &fp.licensed_skins {
        if snap.licensed_skins.get(&cid).map(fp_json) != Some(eh) {
            let js: String = conn
                .query_row(
                    "SELECT skins_json FROM licensed_skins WHERE character_id = ?1",
                    params![cid],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            snap.licensed_skins
                .insert(cid, serde_json::from_str(&js).map_err(|e| e.to_string())?);
        }
    }

    // 28. asteroids
    snap.asteroids
        .retain(|id, _| fp.asteroids.contains_key(id));
    for (&id, &eh) in &fp.asteroids {
        if snap.asteroids.get(&id).map(fp_json) != Some(eh) {
            let ast_res = conn.query_row(
                "SELECT asteroid_id, solar_system_id, belt_id, type_id, quantity, initial_quantity, x, y, z, radius FROM asteroids WHERE asteroid_id = ?1",
                params![id],
                |r| {
                    Ok(AsteroidRecord {
                        asteroid_id: r.get(0)?,
                        solar_system_id: r.get(1)?,
                        belt_id: r.get(2)?,
                        type_id: r.get(3)?,
                        quantity: r.get(4)?,
                        initial_quantity: r.get(5)?,
                        position: (r.get(6)?, r.get(7)?, r.get(8)?),
                        radius: r.get(9)?,
                    })
                },
            );
            if let Ok(ast) = ast_res {
                snap.asteroids.insert(id, ast);
            }
        }
    }

    // 29. character_last_jump_unix
    snap.character_last_jump_unix
        .retain(|k, _| fp.character_last_jump_unix.contains_key(k));
    for (&k, &expected_v) in &fp.character_last_jump_unix {
        if snap.character_last_jump_unix.get(&k) != Some(&expected_v) {
            let res: Result<i64, _> = conn.query_row(
                "SELECT last_jump_unix FROM character_last_jump_unix WHERE character_id = ?1",
                params![k],
                |r| r.get(0),
            );
            if let Ok(v) = res {
                snap.character_last_jump_unix.insert(k, v as u64);
            }
        }
    }

    // 30. character_cspa_charges
    snap.character_cspa_charges
        .retain(|k, _| fp.character_cspa_charges.contains_key(k));
    for (&k, &expected_cents) in &fp.character_cspa_charges {
        if snap.character_cspa_charges.get(&k).map(|c| c.0) != Some(expected_cents) {
            let res: Result<i64, _> = conn.query_row(
                "SELECT cspa_charge_cents FROM character_cspa_charges WHERE character_id = ?1",
                params![k],
                |r| r.get(0),
            );
            if let Ok(cents) = res {
                snap.character_cspa_charges.insert(k, IskCents(cents));
            }
        }
    }

    // 31. mail_timestamps
    snap.mail_timestamps
        .retain(|k, _| fp.mail_timestamps.contains_key(k));
    for (&k, &expected_v) in &fp.mail_timestamps {
        if snap.mail_timestamps.get(&k) != Some(&expected_v) {
            let res: Result<i64, _> = conn.query_row(
                "SELECT timestamp_unix FROM mail_timestamps WHERE mail_id = ?1",
                params![k],
                |r| r.get(0),
            );
            if let Ok(v) = res {
                snap.mail_timestamps.insert(k, v as u64);
            }
        }
    }

    // 32. character_login_stats
    snap.character_login_stats
        .retain(|k, _| fp.character_login_stats.contains_key(k));
    for (&k, &expected_h) in &fp.character_login_stats {
        if snap.character_login_stats.get(&k).map(fp_json) != Some(expected_h) {
            let res = conn.query_row(
                "SELECT last_login_unix, last_logout_unix, logins_count FROM character_login_stats WHERE character_id = ?1",
                params![k],
                |r| {
                    Ok(CharacterLoginStats {
                        last_login_unix: r.get::<_, i64>(0)? as u64,
                        last_logout_unix: r.get::<_, i64>(1)? as u64,
                        logins_count: r.get::<_, i64>(2)? as u64,
                    })
                },
            );
            if let Ok(st) = res {
                snap.character_login_stats.insert(k, st);
            }
        }
    }

    // 33. item_custom_names
    snap.item_custom_names
        .retain(|k, _| fp.item_custom_names.contains_key(k));
    for (&k, &expected_h) in &fp.item_custom_names {
        if snap.item_custom_names.get(&k).map(fp_json) != Some(expected_h) {
            let res: Result<String, _> = conn.query_row(
                "SELECT custom_name FROM item_custom_names WHERE item_id = ?1",
                params![k],
                |r| r.get(0),
            );
            if let Ok(name) = res {
                snap.item_custom_names.insert(k, name);
            }
        }
    }

    // 34. item_positions
    snap.item_positions
        .retain(|k, _| fp.item_positions.contains_key(k));
    for (&k, &expected_h) in &fp.item_positions {
        if snap.item_positions.get(&k).map(fp_json) != Some(expected_h) {
            let res: Result<[f64; 3], _> = conn.query_row(
                "SELECT x, y, z FROM item_positions WHERE item_id = ?1",
                params![k],
                |r| Ok([r.get(0)?, r.get(1)?, r.get(2)?]),
            );
            if let Ok(pos) = res {
                snap.item_positions.insert(k, pos);
            }
        }
    }

    // 35. structure_vulnerability_windows
    snap.structure_vulnerability_windows
        .retain(|k, _| fp.structure_vulnerability_windows.contains_key(k));
    for (&k, &expected_v) in &fp.structure_vulnerability_windows {
        if snap.structure_vulnerability_windows.get(&k) != Some(&expected_v) {
            let res: Result<(i64, i64), _> = conn.query_row(
                "SELECT window_start_unix, window_end_unix FROM structure_vulnerability_windows WHERE structure_id = ?1",
                params![k],
                |r| Ok((r.get(0)?, r.get(1)?)),
            );
            if let Ok(v) = res {
                snap.structure_vulnerability_windows.insert(k, (v.0 as u64, v.1 as u64));
            }
        }
    }

    // 36. skill_history
    snap.skill_history
        .retain(|k, _| fp.skill_history.contains_key(k));
    for (&k, &expected_h) in &fp.skill_history {
        if snap.skill_history.get(&k).map(fp_json) != Some(expected_h) {
            let res: Result<String, _> = conn.query_row(
                "SELECT history_json FROM skill_history WHERE character_id = ?1",
                params![k],
                |r| r.get(0),
            );
            if let Ok(js) = res {
                if let Ok(hist) = serde_json::from_str::<Vec<SkillHistoryEntry>>(&js) {
                    snap.skill_history.insert(k, hist);
                }
            }
        }
    }

    // 37. character_doll_info
    snap.character_doll_info
        .retain(|k, _| fp.character_doll_info.contains_key(k));
    for (&k, &expected_h) in &fp.character_doll_info {
        if snap.character_doll_info.get(&k).map(fp_json) != Some(expected_h) {
            let res: Result<(String, String), _> = conn.query_row(
                "SELECT doll_dna, portrait_meta FROM character_doll_info WHERE character_id = ?1",
                params![k],
                |r| Ok((r.get(0)?, r.get(1)?)),
            );
            if let Ok(info) = res {
                snap.character_doll_info.insert(k, info);
            }
        }
    }

    super::ledger_store::rollback_ledger(conn, snap, fp);
    Ok(())
}
