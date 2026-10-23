//! Incremental snapshot synchronization to SQLite relational tables.

use crate::sqlite_store::fingerprints::*;

use crate::persistence::WorldStateWalDelta;
use crate::sqlite_store::fingerprints::TableFingerprints;
use crate::types::*;
use eve_sde::{SolarSystemId, TypeId};
use rusqlite::{params, Connection, Transaction};

pub fn sync_dirty_to_sqlite(
    tx: &Transaction<'_>,
    snap: &WorldStateSnapshot,
    fp: &mut TableFingerprints,
    need_wal_delta: bool,
) -> Result<WorldStateWalDelta, String> {
    let mut delta = WorldStateWalDelta {
        seq: snap.wal_seq,
        next_entity_id: snap.next_entity_id,
        ..Default::default()
    };
    let mut cold_changed = false;

    if fp.next_entity_id != snap.next_entity_id
        || fp.wal_seq != snap.wal_seq
        || fp.markets_seeded != snap.markets_seeded
    {
        if fp.markets_seeded != snap.markets_seeded {
            cold_changed = true;
        }
        tx.execute(
            "INSERT OR REPLACE INTO world_meta (meta_key, meta_value) VALUES ('next_entity_id', ?1), ('wal_seq', ?2), ('markets_seeded', ?3)",
            params![
                snap.next_entity_id,
                snap.wal_seq as i64,
                i64::from(snap.markets_seeded)
            ],
        )
        .map_err(|e| e.to_string())?;
        fp.next_entity_id = snap.next_entity_id;
        fp.wal_seq = snap.wal_seq;
        fp.markets_seeded = snap.markets_seeded;
    }

    // 1. accounts
    let removed_accounts: Vec<String> = fp
        .accounts
        .keys()
        .filter(|k| !snap.accounts.contains_key(*k))
        .cloned()
        .collect();
    for k in removed_accounts {
        cold_changed = true;
        tx.execute("DELETE FROM accounts WHERE username = ?1", params![k])
            .map_err(|e| e.to_string())?;
        fp.accounts.remove(&k);
    }
    for (k, v) in &snap.accounts {
        let h = fp_account(v);
        if fp.accounts.get(k) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO accounts (username, user_id, password_token, role, banned_until_unix, ban_reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    v.username,
                    v.user_id as i64,
                    v.password_token,
                    v.role as i64,
                    v.banned_until_unix.map(|u| u as i64),
                    v.ban_reason,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.accounts.insert(k.clone(), h);
        }
    }

    // 2. characters
    let removed_chars: Vec<i64> = fp
        .characters
        .keys()
        .filter(|k| !snap.characters.contains_key(*k))
        .copied()
        .collect();
    for k in removed_chars {
        cold_changed = true;
        tx.execute("DELETE FROM characters WHERE character_id = ?1", params![k])
            .map_err(|e| e.to_string())?;
        fp.characters.remove(&k);
    }
    for (&k, v) in &snap.characters {
        let h = fp_character(v);
        if fp.characters.get(&k) != Some(&h) {
            let rec_json = serde_json::to_string(v).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT INTO characters (character_id, user_id, name, corporation_id, alliance_id, station_id, structure_id, solar_system_id, active_ship_id, balance_cents, version, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(character_id) DO UPDATE SET
                     user_id = excluded.user_id,
                     name = excluded.name,
                     corporation_id = excluded.corporation_id,
                     alliance_id = excluded.alliance_id,
                     station_id = excluded.station_id,
                     structure_id = excluded.structure_id,
                     solar_system_id = excluded.solar_system_id,
                     active_ship_id = excluded.active_ship_id,
                     balance_cents = excluded.balance_cents,
                     version = excluded.version,
                     record_json = excluded.record_json",
                params![
                    v.character_id,
                    v.user_id as i64,
                    v.name,
                    v.corporation_id,
                    v.alliance_id,
                    v.station_id,
                    v.structure_id,
                    v.solar_system_id as i64,
                    v.active_ship_id,
                    v.balance.0,
                    v.version as i64,
                    rec_json,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.characters.insert(k, h);
            delta.upserted_characters.push(v.clone());
        }
    }

    // 3. skills
    let removed_skill_chars: Vec<i64> = fp
        .skills
        .keys()
        .filter(|k| !snap.skills.contains_key(*k))
        .copied()
        .collect();
    for cid in removed_skill_chars {
        cold_changed = true;
        tx.execute("DELETE FROM skills WHERE character_id = ?1", params![cid])
            .map_err(|e| e.to_string())?;
        fp.skills.remove(&cid);
    }
    for (&cid, sk_map) in &snap.skills {
        let fp_map = fp.skills.entry(cid).or_default();
        let rem_tids: Vec<TypeId> = fp_map
            .keys()
            .filter(|tid| !sk_map.contains_key(*tid))
            .copied()
            .collect();
        for tid in rem_tids {
            cold_changed = true;
            tx.execute(
                "DELETE FROM skills WHERE character_id = ?1 AND type_id = ?2",
                params![cid, tid as i64],
            )
            .map_err(|e| e.to_string())?;
            fp_map.remove(&tid);
        }
        for (&tid, sk) in sk_map {
            let mut h = FnvHasher::new();
            h.write_u32(sk.type_id);
            h.write_u8(sk.level);
            h.write_f64(sk.skill_points);
            let digest = h.finish();
            if fp_map.get(&tid) != Some(&digest) {
                cold_changed = true;
                tx.execute(
                    "INSERT OR REPLACE INTO skills (character_id, type_id, level, skill_points) VALUES (?1, ?2, ?3, ?4)",
                    params![cid, tid as i64, sk.level as i64, sk.skill_points],
                )
                .map_err(|e| e.to_string())?;
                fp_map.insert(tid, digest);
            }
        }
    }

    // 4. skill_queues
    let rem_q_chars: Vec<i64> = fp
        .skill_queues
        .keys()
        .filter(|k| !snap.skill_queues.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_q_chars {
        cold_changed = true;
        tx.execute(
            "DELETE FROM skill_queues WHERE character_id = ?1",
            params![cid],
        )
        .map_err(|e| e.to_string())?;
        fp.skill_queues.remove(&cid);
    }
    for (&cid, q) in &snap.skill_queues {
        let h = fp_skill_queue(q);
        if fp.skill_queues.get(&cid) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "DELETE FROM skill_queues WHERE character_id = ?1",
                params![cid],
            )
            .map_err(|e| e.to_string())?;
            for (idx, entry) in q.iter().enumerate() {
                tx.execute(
                    "INSERT INTO skill_queues (character_id, queue_index, type_id, target_level, start_sp, end_sp, duration_seconds, started_at_unix)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        cid,
                        idx as i64,
                        entry.type_id as i64,
                        entry.target_level as i64,
                        entry.start_sp,
                        entry.end_sp,
                        entry.duration_seconds,
                        entry.started_at_unix.map(|v| v as i64),
                    ],
                )
                .map_err(|e| e.to_string())?;
            }
            fp.skill_queues.insert(cid, h);
        }
    }

    // 5. jump_clones
    let rem_jc: Vec<i64> = fp
        .jump_clones
        .keys()
        .filter(|k| !snap.jump_clones.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_jc {
        cold_changed = true;
        tx.execute(
            "DELETE FROM jump_clones WHERE character_id = ?1",
            params![cid],
        )
        .map_err(|e| e.to_string())?;
        fp.jump_clones.remove(&cid);
    }
    for (&cid, clones) in &snap.jump_clones {
        let h = fp_json(clones);
        if fp.jump_clones.get(&cid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(clones).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO jump_clones (character_id, clones_json) VALUES (?1, ?2)",
                params![cid, js],
            )
            .map_err(|e| e.to_string())?;
            fp.jump_clones.insert(cid, h);
        }
    }

    // 6. active_implants
    let rem_imp: Vec<i64> = fp
        .active_implants
        .keys()
        .filter(|k| !snap.active_implants.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_imp {
        cold_changed = true;
        tx.execute(
            "DELETE FROM active_implants WHERE character_id = ?1",
            params![cid],
        )
        .map_err(|e| e.to_string())?;
        fp.active_implants.remove(&cid);
    }
    for (&cid, imps) in &snap.active_implants {
        let h = fp_json(imps);
        if fp.active_implants.get(&cid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(imps).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO active_implants (character_id, implants_json) VALUES (?1, ?2)",
                params![cid, js],
            )
            .map_err(|e| e.to_string())?;
            fp.active_implants.insert(cid, h);
        }
    }

    // 7. items
    let removed_items: Vec<i64> = fp
        .items
        .keys()
        .filter(|k| !snap.items.contains_key(*k))
        .copied()
        .collect();
    for id in removed_items {
        tx.execute("DELETE FROM items WHERE item_id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        fp.items.remove(&id);
        delta.removed_items.push(id);
    }
    for (&id, item) in &snap.items {
        let h = fp_item(item);
        if fp.items.get(&id) != Some(&h) {
            let rec_json = serde_json::to_string(item).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO items (item_id, type_id, owner_id, location_id, flag, quantity, singleton, version, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    item.item_id,
                    item.type_id as i64,
                    item.owner_id,
                    item.location_id,
                    item.flag as i64,
                    item.quantity as i64,
                    i64::from(item.singleton),
                    item.version as i64,
                    rec_json,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.items.insert(id, h);
            delta.upserted_items.push(item.clone());
        }
    }

    // 8. saved_fittings
    let rem_fit: Vec<i64> = fp
        .saved_fittings
        .keys()
        .filter(|k| !snap.saved_fittings.contains_key(*k))
        .copied()
        .collect();
    for fid in rem_fit {
        cold_changed = true;
        tx.execute(
            "DELETE FROM saved_fittings WHERE fitting_id = ?1",
            params![fid],
        )
        .map_err(|e| e.to_string())?;
        fp.saved_fittings.remove(&fid);
    }
    for (&fid, fit) in &snap.saved_fittings {
        let h = fp_json(fit);
        if fp.saved_fittings.get(&fid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(fit).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO saved_fittings (fitting_id, owner_id, name, ship_type_id, description, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![fit.fitting_id, fit.owner_id, fit.name, fit.ship_type_id as i64, fit.description, js],
            )
            .map_err(|e| e.to_string())?;
            fp.saved_fittings.insert(fid, h);
        }
    }

    // 9. market_orders
    let removed_orders: Vec<i64> = fp
        .market_orders
        .keys()
        .filter(|k| !snap.market_orders.contains_key(*k))
        .copied()
        .collect();
    for oid in removed_orders {
        tx.execute(
            "DELETE FROM market_orders WHERE order_id = ?1",
            params![oid],
        )
        .map_err(|e| e.to_string())?;
        fp.market_orders.remove(&oid);
        delta.removed_market_orders.push(oid);
    }
    for (&oid, ord) in &snap.market_orders {
        let h = fp_market_order(ord);
        if fp.market_orders.get(&oid) != Some(&h) {
            tx.execute(
                "INSERT OR REPLACE INTO market_orders (order_id, owner_id, station_id, region_id, type_id, is_bid, price_cents, vol_entered, vol_remaining, min_volume, is_npc, escrow_cents)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    ord.order_id,
                    ord.owner_id,
                    ord.station_id,
                    ord.region_id as i64,
                    ord.type_id as i64,
                    i64::from(ord.is_bid),
                    ord.price.0,
                    ord.vol_entered as i64,
                    ord.vol_remaining as i64,
                    ord.min_volume as i64,
                    i64::from(ord.is_npc),
                    ord.escrow.0,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.market_orders.insert(oid, h);
            delta.upserted_market_orders.push(ord.clone());
        }
    }

    // 10. wallet_journal
    if snap.wallet_journal.len() < fp.wallet_journal_len
        || (fp.wallet_journal_len > 0
            && fp_wallet_journal_slice(&snap.wallet_journal[..fp.wallet_journal_len])
                != fp.wallet_journal_hash)
    {
        cold_changed = true;
        tx.execute("DELETE FROM wallet_journal", [])
            .map_err(|e| e.to_string())?;
        fp.wallet_journal_len = 0;
        fp.wallet_journal_hash = fp_wallet_journal_slice(&[]);
    }
    if snap.wallet_journal.len() > fp.wallet_journal_len {
        for (idx, entry) in snap.wallet_journal[fp.wallet_journal_len..]
            .iter()
            .enumerate()
        {
            let seq_idx = (fp.wallet_journal_len + idx) as i64;
            tx.execute(
                "INSERT OR REPLACE INTO wallet_journal (seq_idx, entry_id, character_id, entry_type, amount_cents, balance_after_cents, reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    seq_idx,
                    entry.entry_id,
                    entry.character_id,
                    entry.entry_type,
                    entry.amount.0,
                    entry.balance_after.0,
                    entry.reason,
                ],
            )
            .map_err(|e| e.to_string())?;
            delta.appended_wallet_journal.push(entry.clone());
        }
        fp.wallet_journal_len = snap.wallet_journal.len();
        fp.wallet_journal_hash = fp_wallet_journal_slice(&snap.wallet_journal);
    }

    // 11. contracts
    let rem_contracts: Vec<i64> = fp
        .contracts
        .keys()
        .filter(|k| !snap.contracts.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_contracts {
        cold_changed = true;
        tx.execute("DELETE FROM contracts WHERE contract_id = ?1", params![cid])
            .map_err(|e| e.to_string())?;
        fp.contracts.remove(&cid);
    }
    for (&cid, c) in &snap.contracts {
        let h = fp_contract(c);
        if fp.contracts.get(&cid) != Some(&h) {
            let js = serde_json::to_string(c).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO contracts (contract_id, issuer_id, acceptor_id, contract_type, status, start_station_id, end_station_id, price_cents, reward_cents, collateral_cents, expiration_unix, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    c.contract_id,
                    c.issuer_id,
                    c.acceptor_id,
                    c.contract_type,
                    c.status,
                    c.start_station_id,
                    c.end_station_id,
                    c.price.0,
                    c.reward.0,
                    c.collateral.0,
                    c.expiration_unix as i64,
                    js,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.contracts.insert(cid, h);
            delta.upserted_contracts.push(c.clone());
        }
    }

    // 12. industry_jobs
    let rem_jobs: Vec<i64> = fp
        .industry_jobs
        .keys()
        .filter(|k| !snap.industry_jobs.contains_key(*k))
        .copied()
        .collect();
    for jid in rem_jobs {
        cold_changed = true;
        tx.execute("DELETE FROM industry_jobs WHERE job_id = ?1", params![jid])
            .map_err(|e| e.to_string())?;
        fp.industry_jobs.remove(&jid);
    }
    for (&jid, j) in &snap.industry_jobs {
        let h = fp_json(j);
        if fp.industry_jobs.get(&jid) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO industry_jobs (job_id, installer_id, facility_id, blueprint_item_id, blueprint_type_id, activity_id, runs, product_type_id, status, duration_seconds, start_unix)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    j.job_id,
                    j.installer_id,
                    j.facility_id,
                    j.blueprint_item_id,
                    j.blueprint_type_id as i64,
                    j.activity_id as i64,
                    j.runs as i64,
                    j.product_type_id as i64,
                    j.status,
                    j.duration_seconds as i64,
                    j.start_unix as i64,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.industry_jobs.insert(jid, h);
        }
    }

    // 13. planetary_colonies
    let rem_col: Vec<(i64, i64)> = fp
        .planetary_colonies
        .keys()
        .filter(|k| !snap.planetary_colonies.contains_key(*k))
        .copied()
        .collect();
    for (oid, pid) in rem_col {
        cold_changed = true;
        tx.execute(
            "DELETE FROM planetary_colonies WHERE owner_id = ?1 AND planet_id = ?2",
            params![oid, pid],
        )
        .map_err(|e| e.to_string())?;
        fp.planetary_colonies.remove(&(oid, pid));
    }
    for (&(oid, pid), col) in &snap.planetary_colonies {
        let h = fp_json(col);
        if fp.planetary_colonies.get(&(oid, pid)) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(col).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO planetary_colonies (owner_id, planet_id, solar_system_id, command_center_level, pins_count, last_extracted_unix, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    oid,
                    pid,
                    col.solar_system_id as i64,
                    col.command_center_level as i64,
                    col.pins_count as i64,
                    col.last_extracted_unix as i64,
                    js,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.planetary_colonies.insert((oid, pid), h);
        }
    }

    // 14. active_missions
    let rem_mis: Vec<(i64, u32)> = fp
        .active_missions
        .keys()
        .filter(|k| !snap.active_missions.contains_key(*k))
        .copied()
        .collect();
    for (cid, mid) in rem_mis {
        cold_changed = true;
        tx.execute(
            "DELETE FROM missions WHERE character_id = ?1 AND mission_id = ?2",
            params![cid, mid as i64],
        )
        .map_err(|e| e.to_string())?;
        fp.active_missions.remove(&(cid, mid));
    }
    for (&(cid, mid), m) in &snap.active_missions {
        let h = fp_json(m);
        if fp.active_missions.get(&(cid, mid)) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(m).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO missions (character_id, mission_id, agent_id, solar_system_id, state, current_room_id, npcs_remaining, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    cid,
                    mid as i64,
                    m.agent_id,
                    m.solar_system_id as i64,
                    m.state,
                    m.current_room_id as i64,
                    m.npcs_remaining as i64,
                    js,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.active_missions.insert((cid, mid), h);
        }
    }

    // 15. mission_tally
    let rem_tally: Vec<i64> = fp
        .mission_tally
        .keys()
        .filter(|k| !snap.mission_tally.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_tally {
        cold_changed = true;
        tx.execute(
            "DELETE FROM mission_tally WHERE character_id = ?1",
            params![cid],
        )
        .map_err(|e| e.to_string())?;
        fp.mission_tally.remove(&cid);
    }
    for (&cid, &t) in &snap.mission_tally {
        if fp.mission_tally.get(&cid) != Some(&t) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO mission_tally (character_id, tally) VALUES (?1, ?2)",
                params![cid, t as i64],
            )
            .map_err(|e| e.to_string())?;
            fp.mission_tally.insert(cid, t);
        }
    }

    // 16. standing_transactions
    let rem_st_tx: Vec<i64> = fp
        .standing_transactions
        .keys()
        .filter(|k| !snap.standing_transactions.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_st_tx {
        cold_changed = true;
        tx.execute(
            "DELETE FROM standings WHERE character_id = ?1",
            params![cid],
        )
        .map_err(|e| e.to_string())?;
        fp.standing_transactions.remove(&cid);
    }
    for (&cid, list) in &snap.standing_transactions {
        let h = fp_json(list);
        if fp.standing_transactions.get(&cid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(list).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO standings (character_id, tx_json) VALUES (?1, ?2)",
                params![cid, js],
            )
            .map_err(|e| e.to_string())?;
            fp.standing_transactions.insert(cid, h);
        }
    }

    // 17. npe_progress
    let rem_npe: Vec<i64> = fp
        .npe_progress
        .keys()
        .filter(|k| !snap.npe_progress.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_npe {
        cold_changed = true;
        tx.execute(
            "DELETE FROM npe_progress WHERE character_id = ?1",
            params![cid],
        )
        .map_err(|e| e.to_string())?;
        fp.npe_progress.remove(&cid);
    }
    for (&cid, npe) in &snap.npe_progress {
        let h = fp_json(npe);
        if fp.npe_progress.get(&cid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(npe).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO npe_progress (character_id, node_graph_id, current_node_id, completed, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    cid,
                    npe.node_graph_id as i64,
                    npe.current_node_id as i64,
                    i64::from(npe.completed),
                    js,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.npe_progress.insert(cid, h);
        }
    }

    // 18. bookmarks
    let rem_bm: Vec<i64> = fp
        .bookmarks
        .keys()
        .filter(|k| !snap.bookmarks.contains_key(*k))
        .copied()
        .collect();
    for bid in rem_bm {
        cold_changed = true;
        tx.execute("DELETE FROM bookmarks WHERE bookmark_id = ?1", params![bid])
            .map_err(|e| e.to_string())?;
        fp.bookmarks.remove(&bid);
    }
    for (&bid, bm) in &snap.bookmarks {
        let h = fp_json(bm);
        if fp.bookmarks.get(&bid) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO bookmarks (bookmark_id, owner_id, label, solar_system_id, x, y, z)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![bm.bookmark_id, bm.owner_id, bm.label, bm.solar_system_id as i64, bm.x, bm.y, bm.z],
            )
            .map_err(|e| e.to_string())?;
            fp.bookmarks.insert(bid, h);
        }
    }

    // 19. mails
    if snap.mails.len() < fp.mails_len
        || (fp.mails_len > 0 && fp_mails_slice(&snap.mails[..fp.mails_len]) != fp.mails_hash)
    {
        cold_changed = true;
        tx.execute("DELETE FROM mail", [])
            .map_err(|e| e.to_string())?;
        fp.mails_len = 0;
        fp.mails_hash = fp_mails_slice(&[]);
    }
    if snap.mails.len() > fp.mails_len {
        cold_changed = true;
        for (idx, m) in snap.mails[fp.mails_len..].iter().enumerate() {
            let seq_idx = (fp.mails_len + idx) as i64;
            tx.execute(
                "INSERT OR REPLACE INTO mail (seq_idx, message_id, sender_id, recipient_id, subject, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![seq_idx, m.message_id, m.sender_id, m.recipient_id, m.subject, m.body],
            )
            .map_err(|e| e.to_string())?;
        }
        fp.mails_len = snap.mails.len();
        fp.mails_hash = fp_mails_slice(&snap.mails);
    }

    // 20. fleets
    let rem_fl: Vec<i64> = fp
        .fleets
        .keys()
        .filter(|k| !snap.fleets.contains_key(*k))
        .copied()
        .collect();
    for fid in rem_fl {
        cold_changed = true;
        tx.execute("DELETE FROM fleets WHERE fleet_id = ?1", params![fid])
            .map_err(|e| e.to_string())?;
        fp.fleets.remove(&fid);
    }
    for (&fid, fl) in &snap.fleets {
        let h = fp_json(fl);
        if fp.fleets.get(&fid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(fl).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO fleets (fleet_id, boss_id, free_move, is_registered, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![fl.fleet_id, fl.boss_id, i64::from(fl.free_move), i64::from(fl.is_registered), js],
            )
            .map_err(|e| e.to_string())?;
            fp.fleets.insert(fid, h);
        }
    }

    // 21. corporations
    let rem_corps: Vec<i64> = fp
        .corporations
        .keys()
        .filter(|k| !snap.corporations.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_corps {
        cold_changed = true;
        tx.execute("DELETE FROM corps WHERE corporation_id = ?1", params![cid])
            .map_err(|e| e.to_string())?;
        fp.corporations.remove(&cid);
    }
    for (&cid, corp) in &snap.corporations {
        let h = fp_json(corp);
        if fp.corporations.get(&cid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(corp).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO corps (corporation_id, name, ticker, ceo_id, alliance_id, tax_rate, master_wallet_cents, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    corp.corporation_id,
                    corp.name,
                    corp.ticker,
                    corp.ceo_id,
                    corp.alliance_id,
                    corp.tax_rate,
                    corp.master_wallet.0,
                    js,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.corporations.insert(cid, h);
        }
    }

    // 22. alliances
    let rem_all: Vec<i64> = fp
        .alliances
        .keys()
        .filter(|k| !snap.alliances.contains_key(*k))
        .copied()
        .collect();
    for aid in rem_all {
        cold_changed = true;
        tx.execute("DELETE FROM alliances WHERE alliance_id = ?1", params![aid])
            .map_err(|e| e.to_string())?;
        fp.alliances.remove(&aid);
    }
    for (&aid, al) in &snap.alliances {
        let h = fp_json(al);
        if fp.alliances.get(&aid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(al).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO alliances (alliance_id, name, short_name, executor_corp_id, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![al.alliance_id, al.name, al.short_name, al.executor_corp_id, js],
            )
            .map_err(|e| e.to_string())?;
            fp.alliances.insert(aid, h);
        }
    }

    // 23. wars
    let rem_wars: Vec<i64> = fp
        .wars
        .keys()
        .filter(|k| !snap.wars.contains_key(*k))
        .copied()
        .collect();
    for wid in rem_wars {
        cold_changed = true;
        tx.execute("DELETE FROM wars WHERE war_id = ?1", params![wid])
            .map_err(|e| e.to_string())?;
        fp.wars.remove(&wid);
    }
    for (&wid, w) in &snap.wars {
        let h = fp_json(w);
        if fp.wars.get(&wid) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO wars (war_id, aggressor_corp_id, defender_corp_id, mutual, started_unix, active)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    w.war_id,
                    w.aggressor_corp_id,
                    w.defender_corp_id,
                    i64::from(w.mutual),
                    w.started_unix as i64,
                    i64::from(w.active),
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.wars.insert(wid, h);
        }
    }

    // 24. corp_projects
    let rem_proj: Vec<i64> = fp
        .corp_projects
        .keys()
        .filter(|k| !snap.corp_projects.contains_key(*k))
        .copied()
        .collect();
    for pid in rem_proj {
        cold_changed = true;
        tx.execute(
            "DELETE FROM corp_projects WHERE project_id = ?1",
            params![pid],
        )
        .map_err(|e| e.to_string())?;
        fp.corp_projects.remove(&pid);
    }
    for (&pid, p) in &snap.corp_projects {
        let h = fp_json(p);
        if fp.corp_projects.get(&pid) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO corp_projects (project_id, corporation_id, name, target_quantity, current_quantity, isk_per_unit_cents, escrow_remaining_cents)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    p.project_id,
                    p.corporation_id,
                    p.name,
                    p.target_quantity as i64,
                    p.current_quantity as i64,
                    p.isk_per_unit.0,
                    p.escrow_remaining.0,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.corp_projects.insert(pid, h);
        }
    }

    // 25. killmails
    let rem_km: Vec<i64> = fp
        .killmails
        .keys()
        .filter(|k| !snap.killmails.contains_key(*k))
        .copied()
        .collect();
    for kid in rem_km {
        cold_changed = true;
        tx.execute("DELETE FROM killmails WHERE kill_id = ?1", params![kid])
            .map_err(|e| e.to_string())?;
        fp.killmails.remove(&kid);
    }
    for (&kid, km) in &snap.killmails {
        let h = fp_json(km);
        if fp.killmails.get(&kid) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO killmails (kill_id, victim_character_id, attacker_character_id, solar_system_id, ship_type_id, total_value_isk, killed_unix, victim_corp_id, victim_alliance_id, attacker_corp_id, attacker_alliance_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    km.kill_id,
                    km.victim_character_id,
                    km.attacker_character_id,
                    km.solar_system_id as i64,
                    km.ship_type_id as i64,
                    km.total_value_isk,
                    km.killed_unix as i64,
                    km.victim_corp_id,
                    km.victim_alliance_id,
                    km.attacker_corp_id,
                    km.attacker_alliance_id,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.killmails.insert(kid, h);
        }
    }

    // 26. kill_rights
    let rem_kr: Vec<i64> = fp
        .kill_rights
        .keys()
        .filter(|k| !snap.kill_rights.contains_key(*k))
        .copied()
        .collect();
    for krid in rem_kr {
        cold_changed = true;
        tx.execute(
            "DELETE FROM kill_rights WHERE kill_right_id = ?1",
            params![krid],
        )
        .map_err(|e| e.to_string())?;
        fp.kill_rights.remove(&krid);
    }
    for (&krid, kr) in &snap.kill_rights {
        let h = fp_json(kr);
        if fp.kill_rights.get(&krid) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO kill_rights (kill_right_id, owner_id, target_id, activation_fee_cents, is_active_suspect)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    kr.kill_right_id,
                    kr.owner_id,
                    kr.target_id,
                    kr.activation_fee.0,
                    i64::from(kr.is_active_suspect),
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.kill_rights.insert(krid, h);
        }
    }

    // 27. structures
    let rem_st: Vec<i64> = fp
        .structures
        .keys()
        .filter(|k| !snap.structures.contains_key(*k))
        .copied()
        .collect();
    for stid in rem_st {
        cold_changed = true;
        tx.execute(
            "DELETE FROM structures WHERE structure_id = ?1",
            params![stid],
        )
        .map_err(|e| e.to_string())?;
        fp.structures.remove(&stid);
    }
    for (&stid, st) in &snap.structures {
        let h = fp_structure(st);
        if fp.structures.get(&stid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(st).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO structures (structure_id, type_id, owner_corp_id, solar_system_id, name, state, quantum_core_installed, fuel_blocks, shield_hp, armor_hp, hull_hp, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    st.structure_id,
                    st.type_id as i64,
                    st.owner_corp_id,
                    st.solar_system_id as i64,
                    st.name,
                    st.state,
                    i64::from(st.quantum_core_installed),
                    st.fuel_blocks as i64,
                    st.shield_hp,
                    st.armor_hp,
                    st.hull_hp,
                    js,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.structures.insert(stid, h);
        }
    }

    // 28. system_states
    let rem_sys: Vec<SolarSystemId> = fp
        .system_states
        .keys()
        .filter(|k| !snap.system_states.contains_key(*k))
        .copied()
        .collect();
    for sys_id in rem_sys {
        cold_changed = true;
        tx.execute(
            "DELETE FROM system_states WHERE solar_system_id = ?1",
            params![sys_id as i64],
        )
        .map_err(|e| e.to_string())?;
        fp.system_states.remove(&sys_id);
    }
    for (&sys_id, st) in &snap.system_states {
        let h = fp_system_state(st);
        if fp.system_states.get(&sys_id) != Some(&h) {
            let js = serde_json::to_string(st).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO system_states (solar_system_id, dbs_multiplier, ess_main_bank_cents, ess_reserve_bank_cents, sov_alliance_id, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    sys_id as i64,
                    st.dbs_multiplier,
                    st.ess_main_bank.0,
                    st.ess_reserve_bank.0,
                    st.sov_alliance_id,
                    js,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.system_states.insert(sys_id, h);
            delta.upserted_system_states.push(st.clone());
        }
    }

    // 29. idempotency_keys
    let rem_idem: Vec<(i64, String)> = fp
        .committed_idempotency_keys
        .iter()
        .filter(|k| !snap.committed_idempotency_keys.contains(*k))
        .cloned()
        .collect();
    for (cid, key) in rem_idem {
        cold_changed = true;
        tx.execute(
            "DELETE FROM idempotency_keys WHERE character_id = ?1 AND idem_key = ?2",
            params![cid, key],
        )
        .map_err(|e| e.to_string())?;
        fp.committed_idempotency_keys.remove(&(cid, key));
    }
    for k in &snap.committed_idempotency_keys {
        if !fp.committed_idempotency_keys.contains(k) {
            tx.execute(
                "INSERT OR REPLACE INTO idempotency_keys (character_id, idem_key) VALUES (?1, ?2)",
                params![k.0, k.1],
            )
            .map_err(|e| e.to_string())?;
            fp.committed_idempotency_keys.insert(k.clone());
            delta.added_idempotency_keys.push(k.clone());
        }
    }

    // 30. licensed_skins
    let rem_lskins: Vec<i64> = fp
        .licensed_skins
        .keys()
        .filter(|k| !snap.licensed_skins.contains_key(*k))
        .copied()
        .collect();
    for cid in rem_lskins {
        cold_changed = true;
        tx.execute(
            "DELETE FROM licensed_skins WHERE character_id = ?1",
            params![cid],
        )
        .map_err(|e| e.to_string())?;
        fp.licensed_skins.remove(&cid);
    }
    for (&cid, skins) in &snap.licensed_skins {
        let h = fp_json(skins);
        if fp.licensed_skins.get(&cid) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(skins).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO licensed_skins (character_id, skins_json) VALUES (?1, ?2)",
                params![cid, js],
            )
            .map_err(|e| e.to_string())?;
            fp.licensed_skins.insert(cid, h);
        }
    }

    // 31. applied_ship_skins
    let rem_askins: Vec<i64> = fp
        .applied_ship_skins
        .keys()
        .filter(|k| !snap.applied_ship_skins.contains_key(*k))
        .copied()
        .collect();
    for sid in rem_askins {
        cold_changed = true;
        tx.execute(
            "DELETE FROM applied_ship_skins WHERE ship_id = ?1",
            params![sid],
        )
        .map_err(|e| e.to_string())?;
        fp.applied_ship_skins.remove(&sid);
    }
    for (&sid, &skin_id) in &snap.applied_ship_skins {
        if fp.applied_ship_skins.get(&sid) != Some(&skin_id) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO applied_ship_skins (ship_id, skin_id) VALUES (?1, ?2)",
                params![sid, skin_id as i64],
            )
            .map_err(|e| e.to_string())?;
            fp.applied_ship_skins.insert(sid, skin_id);
        }
    }

    // 32. char_mining_ledger (synced via d6_batch4_store::sync_d6_batch4)

    // 33. asteroids
    let rem_asteroids: Vec<i64> = fp
        .asteroids
        .keys()
        .filter(|k| !snap.asteroids.contains_key(*k))
        .copied()
        .collect();
    for id in rem_asteroids {
        cold_changed = true;
        tx.execute("DELETE FROM asteroids WHERE asteroid_id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        fp.asteroids.remove(&id);
    }
    for (&id, ast) in &snap.asteroids {
        let h = fp_json(ast);
        if fp.asteroids.get(&id) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO asteroids (asteroid_id, solar_system_id, belt_id, type_id, quantity, initial_quantity, x, y, z, radius) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    ast.asteroid_id,
                    ast.solar_system_id,
                    ast.belt_id,
                    ast.type_id,
                    ast.quantity,
                    ast.initial_quantity,
                    ast.position.0,
                    ast.position.1,
                    ast.position.2,
                    ast.radius,
                ],
            )
            .map_err(|e| e.to_string())?;
            fp.asteroids.insert(id, h);
        }
    }

    cold_changed |= super::sync_ext::sync_extension_tables(tx, snap, fp)?;
    cold_changed |= super::ledger_store::sync_ledger(tx, snap, fp)?;
    cold_changed |= super::d6_store::sync_d6(tx, snap, fp)?;
    cold_changed |= super::d6_social_store::sync_d6_social(tx, snap, fp)?;
    cold_changed |= super::d6_corp_crimewatch_store::sync_d6_corp_crimewatch(tx, snap, fp)?;
    cold_changed |= super::d6_batch4_store::sync_d6_batch4(tx, snap, fp)?;

    if need_wal_delta && cold_changed {
        delta.full_snapshot = Some(snap.clone());
    }
    Ok(delta)
}

pub fn replace_sqlite_from_snapshot(
    conn: &mut Connection,
    snap: &WorldStateSnapshot,
) -> Result<TableFingerprints, String> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    tx.execute_batch(
        "DELETE FROM world_meta;
         DELETE FROM accounts;
         DELETE FROM characters;
         DELETE FROM skills;
         DELETE FROM skill_queues;
         DELETE FROM jump_clones;
         DELETE FROM active_implants;
         DELETE FROM items;
         DELETE FROM saved_fittings;
         DELETE FROM market_orders;
         DELETE FROM wallet_journal;
         DELETE FROM contracts;
         DELETE FROM industry_jobs;
         DELETE FROM planetary_colonies;
         DELETE FROM missions;
         DELETE FROM mission_tally;
         DELETE FROM standings;
         DELETE FROM npe_progress;
         DELETE FROM bookmarks;
         DELETE FROM mail;
         DELETE FROM fleets;
         DELETE FROM corps;
         DELETE FROM alliances;
         DELETE FROM wars;
         DELETE FROM corp_projects;
         DELETE FROM killmails;
         DELETE FROM kill_rights;
         DELETE FROM structures;
         DELETE FROM system_states;
         DELETE FROM idempotency_keys;
         DELETE FROM licensed_skins;
         DELETE FROM applied_ship_skins;
         DELETE FROM char_mining_ledger;
         DELETE FROM asteroids;
         DELETE FROM character_last_jump_unix;
         DELETE FROM character_cspa_charges;
         DELETE FROM mail_timestamps;
         DELETE FROM character_login_stats;
         DELETE FROM item_custom_names;
         DELETE FROM item_positions;
         DELETE FROM structure_vulnerability_windows;
         DELETE FROM skill_history;
         DELETE FROM character_doll_info;
         DELETE FROM wallet_journal_meta;
         DELETE FROM market_transactions;
         DELETE FROM account_plex_vault;
         DELETE FROM owner_notes;
         DELETE FROM corp_lp_balances;
         DELETE FROM char_lp_logs;
         DELETE FROM read_mails;
         DELETE FROM deleted_mails;
         DELETE FROM trashed_mails;
         DELETE FROM mail_labels;
         DELETE FROM mail_assigned_labels;
         DELETE FROM mailing_lists;
         DELETE FROM calendar_events;
         DELETE FROM bookmark_meta;
         DELETE FROM bookmark_folders;
         DELETE FROM chat_channels;
         DELETE FROM char_contacts;
         DELETE FROM char_contact_labels;
         DELETE FROM corp_contacts;
         DELETE FROM corp_contact_labels;
         DELETE FROM alliance_contacts;
         DELETE FROM alliance_contact_labels;
         DELETE FROM char_notes;
         DELETE FROM d6_corp_crimewatch_kv;
         DELETE FROM world_kv_blobs;",
    )
    .map_err(|e| e.to_string())?;
    let mut fp = TableFingerprints::default();
    let _ = sync_dirty_to_sqlite(&tx, snap, &mut fp, false)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(fp)
}
