//! Incremental snapshot synchronization to SQLite relational tables.

use crate::cow::CowCell;
#[cfg(any(test, feature = "legacy-json-snapshot"))]
use crate::persistence::WorldStateWalDelta;
use crate::sqlite_store::checked_json::to_checked_json_string;
use crate::types::*;
use eve_sde::SolarSystemId;
use rusqlite::{params, Connection, Transaction};
use std::collections::HashMap;
use std::hash::Hash;

fn sync_cell_map<K, V>(
    tx: &Transaction<'_>,
    before: Option<&CowCell<HashMap<K, V>>>,
    after: &CowCell<HashMap<K, V>>,
    mut delete_fn: impl FnMut(&Transaction<'_>, &K) -> Result<(), String>,
    mut upsert_fn: impl FnMut(&Transaction<'_>, &K, &V) -> Result<(), String>,
) -> Result<(), String>
where
    K: Eq + Hash,
    V: PartialEq,
{
    if let Some(b) = before {
        if b.ptr_eq(after) {
            return Ok(());
        }
        for k in b.keys() {
            if !after.contains_key(k) {
                delete_fn(tx, k)?;
            }
        }
        for (k, v) in after.iter() {
            if b.get(k) != Some(v) {
                upsert_fn(tx, k, v)?;
            }
        }
        return Ok(());
    }
    for (k, v) in after.iter() {
        upsert_fn(tx, k, v)?;
    }
    Ok(())
}

/// Syncs only the cold domains flagged in `cd` without scanning unchanged cold tables or hot `CowMap`s.
pub(crate) fn sync_selected_cold_domains_to_sqlite(
    tx: &Transaction<'_>,
    before: &WorldStateSnapshot,
    snap: &WorldStateSnapshot,
    cd: &crate::tx_engine::ColdDomainDiff,
    #[cfg(any(test, feature = "legacy-json-snapshot"))] mut delta: Option<&mut WorldStateWalDelta>,
) -> Result<(), String> {
    if cd.part1_cold {
        sync_part1_cold_tables(tx, Some(before), snap)?;
    }
    if cd.part2_activity {
        sync_part2_activity_tables(
            tx,
            Some(before),
            snap,
            #[cfg(any(test, feature = "legacy-json-snapshot"))]
            delta.as_deref_mut(),
        )?;
    }
    if cd.part3_social_corp {
        sync_part3_social_corp_tables(tx, Some(before), snap)?;
    }
    if cd.part4_world_tail {
        sync_part4_world_tail_tables(
            tx,
            Some(before),
            snap,
            #[cfg(any(test, feature = "legacy-json-snapshot"))]
            delta,
        )?;
    }
    if cd.extension_tables {
        super::sync_ext::sync_extension_tables_cold_only(tx, Some(before), snap)?;
    }
    if cd.d6_notes_lp_seq {
        super::notes_lp_sequence_store::sync_d6(tx, Some(before), snap)?;
    }
    if cd.d6_batch4 {
        super::industry_market_pve_store::sync_mining_ledger(tx, Some(before), snap)?;
    }
    if cd.service_domains {
        super::service_domains_store::sync_service_domains(tx, Some(before), snap)?;
    }
    Ok(())
}

#[inline(never)]
fn sync_part1_cold_tables(
    tx: &Transaction<'_>,
    before: Option<&WorldStateSnapshot>,
    snap: &WorldStateSnapshot,
) -> Result<(), String> {
    // 1. accounts
    sync_cell_map(
        tx,
        before.map(|b| &b.accounts),
        &snap.accounts,
        |tx, k| {
            tx.execute("DELETE FROM accounts WHERE username = ?1", params![k])
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        |tx, _k, v| {
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
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 5. jump_clones
    sync_cell_map(
        tx,
        before.map(|b| &b.jump_clones),
        &snap.jump_clones,
        |tx, &cid| {
            tx.execute(
                "DELETE FROM jump_clones WHERE character_id = ?1",
                params![cid],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
        |tx, &cid, clones| {
            let js = to_checked_json_string(clones)?;
            tx.execute(
                "INSERT OR REPLACE INTO jump_clones (character_id, clones_json) VALUES (?1, ?2)",
                params![cid, js],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 6. active_implants
    sync_cell_map(
        tx,
        before.map(|b| &b.active_implants),
        &snap.active_implants,
        |tx, &cid| {
            tx.execute(
                "DELETE FROM active_implants WHERE character_id = ?1",
                params![cid],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
        |tx, &cid, imps| {
            let js = to_checked_json_string(imps)?;
            tx.execute(
                "INSERT OR REPLACE INTO active_implants (character_id, implants_json) VALUES (?1, ?2)",
                params![cid, js],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 8. saved_fittings
    sync_cell_map(
        tx,
        before.map(|b| &b.saved_fittings),
        &snap.saved_fittings,
        |tx, &fid| {
            tx.execute(
                "DELETE FROM saved_fittings WHERE fitting_id = ?1",
                params![fid],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
        |tx, &_fid, fit| {
            let js = to_checked_json_string(fit)?;
            tx.execute(
                "INSERT OR REPLACE INTO saved_fittings (fitting_id, owner_id, name, ship_type_id, description, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    fit.fitting_id,
                    fit.owner_id,
                    fit.name,
                    fit.ship_type_id as i64,
                    fit.description,
                    js
                ],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    Ok(())
}

#[inline(never)]
fn sync_part2_activity_tables(
    tx: &Transaction<'_>,
    before: Option<&WorldStateSnapshot>,
    snap: &WorldStateSnapshot,
    #[cfg(any(test, feature = "legacy-json-snapshot"))] mut delta: Option<&mut WorldStateWalDelta>,
) -> Result<(), String> {
    // 11. contracts
    sync_cell_map(
        tx,
        before.map(|b| &b.contracts),
        &snap.contracts,
        |tx, &cid| {
            tx.execute("DELETE FROM contracts WHERE contract_id = ?1", params![cid])
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        |tx, &_cid, c| {
            let js = to_checked_json_string(c)?;
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
            #[cfg(any(test, feature = "legacy-json-snapshot"))]
            if let Some(d) = delta.as_deref_mut() {
                d.upserted_contracts.push(c.clone());
            }
            Ok(())
        },
    )?;

    // 12. industry_jobs
    sync_cell_map(
        tx,
        before.map(|b| &b.industry_jobs),
        &snap.industry_jobs,
        |tx, &jid| {
            tx.execute("DELETE FROM industry_jobs WHERE job_id = ?1", params![jid])
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        |tx, &jid, j| {
            if j.cost.is_some_and(|c| !c.is_finite())
                || j.probability.is_some_and(|p| !p.is_finite())
            {
                return Err(format!("non-finite float in industry_jobs[{jid}]"));
            }
            tx.execute(
                "INSERT OR REPLACE INTO industry_jobs (job_id, installer_id, facility_id, blueprint_item_id, blueprint_type_id, activity_id, runs, product_type_id, status, duration_seconds, start_unix, blueprint_location_id, output_location_id, cost, licensed_runs, probability, completed_unix, completed_character_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
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
                    j.blueprint_location_id,
                    j.output_location_id,
                    j.cost,
                    j.licensed_runs.map(|v| v as i64),
                    j.probability.map(|v| v as f64),
                    j.completed_unix.map(|v| v as i64),
                    j.completed_character_id,
                ],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 13. planetary_colonies
    sync_cell_map(
        tx,
        before.map(|b| &b.planetary_colonies),
        &snap.planetary_colonies,
        |tx, &(oid, pid)| {
            tx.execute(
                "DELETE FROM planetary_colonies WHERE owner_id = ?1 AND planet_id = ?2",
                params![oid, pid],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
        |tx, &(oid, pid), col| {
            let js = to_checked_json_string(col)?;
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
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 14. active_missions
    if before.is_none_or(|b| b.active_missions != snap.active_missions) {
        if let Some(b) = before {
            for &(cid, mid) in b.active_missions.keys() {
                if !snap.active_missions.contains_key(&(cid, mid)) {
                    tx.execute(
                        "DELETE FROM missions WHERE character_id = ?1 AND mission_id = ?2",
                        params![cid, mid as i64],
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
        }
        for (&(cid, mid), m) in &snap.active_missions {
            if before.and_then(|b| b.active_missions.get(&(cid, mid))) != Some(m) {
                let js = to_checked_json_string(m)?;
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
            }
        }
    }

    // 15. mission_tally
    sync_cell_map(
        tx,
        before.map(|b| &b.mission_tally),
        &snap.mission_tally,
        |tx, &cid| {
            tx.execute(
                "DELETE FROM mission_tally WHERE character_id = ?1",
                params![cid],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
        |tx, &cid, &t| {
            tx.execute(
                "INSERT OR REPLACE INTO mission_tally (character_id, tally) VALUES (?1, ?2)",
                params![cid, t as i64],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 16. standing_transactions
    sync_cell_map(
        tx,
        before.map(|b| &b.standing_transactions),
        &snap.standing_transactions,
        |tx, &cid| {
            tx.execute("DELETE FROM standings WHERE character_id = ?1", params![cid])
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        |tx, &cid, list| {
            let js = to_checked_json_string(list)?;
            tx.execute(
                "INSERT OR REPLACE INTO standings (character_id, tx_json) VALUES (?1, ?2)",
                params![cid, js],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 17. npe_progress
    sync_cell_map(
        tx,
        before.map(|b| &b.npe_progress),
        &snap.npe_progress,
        |tx, &cid| {
            tx.execute(
                "DELETE FROM npe_progress WHERE character_id = ?1",
                params![cid],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
        |tx, &cid, npe| {
            let js = to_checked_json_string(npe)?;
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
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 18. bookmarks
    sync_cell_map(
        tx,
        before.map(|b| &b.bookmarks),
        &snap.bookmarks,
        |tx, &bid| {
            tx.execute("DELETE FROM bookmarks WHERE bookmark_id = ?1", params![bid])
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        |tx, &bid, bm| {
            if !bm.x.is_finite() || !bm.y.is_finite() || !bm.z.is_finite() {
                return Err(format!("non-finite float in bookmarks[{bid}]"));
            }
            tx.execute(
                "INSERT OR REPLACE INTO bookmarks (bookmark_id, owner_id, label, solar_system_id, x, y, z, item_id, memo, folder_id, is_shared)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    bm.bookmark_id,
                    bm.owner_id,
                    bm.label,
                    bm.solar_system_id as i64,
                    bm.x,
                    bm.y,
                    bm.z,
                    bm.item_id,
                    bm.memo,
                    bm.folder_id,
                    i64::from(bm.is_shared),
                ],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    Ok(())
}

#[inline(never)]
fn sync_part3_social_corp_tables(
    tx: &Transaction<'_>,
    before: Option<&WorldStateSnapshot>,
    snap: &WorldStateSnapshot,
) -> Result<(), String> {
    // 20. fleets
    sync_cell_map(
        tx,
        before.map(|b| &b.fleets),
        &snap.fleets,
        |tx, &fid| {
            tx.execute("DELETE FROM fleets WHERE fleet_id = ?1", params![fid])
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        |tx, &_fid, fl| {
            let js = to_checked_json_string(fl)?;
            tx.execute(
                "INSERT OR REPLACE INTO fleets (fleet_id, boss_id, free_move, is_registered, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    fl.fleet_id,
                    fl.boss_id,
                    i64::from(fl.free_move),
                    i64::from(fl.is_registered),
                    js
                ],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 21. corporations
    sync_cell_map(
        tx,
        before.map(|b| &b.corporations),
        &snap.corporations,
        |tx, &cid| {
            tx.execute("DELETE FROM corps WHERE corporation_id = ?1", params![cid])
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        |tx, &_cid, corp| {
            let js = to_checked_json_string(corp)?;
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
                    corp.division_balance(1).0,
                    js,
                ],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 22. alliances
    sync_cell_map(
        tx,
        before.map(|b| &b.alliances),
        &snap.alliances,
        |tx, &aid| {
            tx.execute("DELETE FROM alliances WHERE alliance_id = ?1", params![aid])
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        |tx, &_aid, al| {
            let js = to_checked_json_string(al)?;
            tx.execute(
                "INSERT OR REPLACE INTO alliances (alliance_id, name, short_name, executor_corp_id, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![al.alliance_id, al.name, al.short_name, al.executor_corp_id, js],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 23. wars
    sync_cell_map(
        tx,
        before.map(|b| &b.wars),
        &snap.wars,
        |tx, &wid| {
            tx.execute("DELETE FROM wars WHERE war_id = ?1", params![wid])
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        |tx, &_wid, w| {
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
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 24. corp_projects
    sync_cell_map(
        tx,
        before.map(|b| &b.corp_projects),
        &snap.corp_projects,
        |tx, &pid| {
            tx.execute(
                "DELETE FROM corp_projects WHERE project_id = ?1",
                params![pid],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
        |tx, &_pid, p| {
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
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 25. killmails
    sync_cell_map(
        tx,
        before.map(|b| &b.killmails),
        &snap.killmails,
        |tx, &kid| {
            tx.execute("DELETE FROM killmails WHERE kill_id = ?1", params![kid])
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        |tx, &_kid, km| {
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
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 26. kill_rights
    sync_cell_map(
        tx,
        before.map(|b| &b.kill_rights),
        &snap.kill_rights,
        |tx, &krid| {
            tx.execute(
                "DELETE FROM kill_rights WHERE kill_right_id = ?1",
                params![krid],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
        |tx, &_krid, kr| {
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
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    Ok(())
}

#[inline(never)]
fn sync_part4_world_tail_tables(
    tx: &Transaction<'_>,
    before: Option<&WorldStateSnapshot>,
    snap: &WorldStateSnapshot,
    #[cfg(any(test, feature = "legacy-json-snapshot"))] mut delta: Option<&mut WorldStateWalDelta>,
) -> Result<(), String> {
    // 28. system_states
    sync_cell_map::<SolarSystemId, _>(
        tx,
        before.map(|b| &b.system_states),
        &snap.system_states,
        |tx, &sys_id| {
            tx.execute(
                "DELETE FROM system_states WHERE solar_system_id = ?1",
                params![sys_id as i64],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
        |tx, &sys_id, st| {
            let js = to_checked_json_string(st)?;
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
            #[cfg(any(test, feature = "legacy-json-snapshot"))]
            if let Some(d) = delta.as_deref_mut() {
                d.upserted_system_states.push(st.clone());
            }
            Ok(())
        },
    )?;

    // 29. idempotency_keys
    if let Some(b) = before {
        if !b
            .committed_idempotency_keys
            .ptr_eq(&snap.committed_idempotency_keys)
        {
            for (cid, key) in b
                .committed_idempotency_keys
                .difference(&*snap.committed_idempotency_keys)
            {
                tx.execute(
                    "DELETE FROM idempotency_keys WHERE character_id = ?1 AND idem_key = ?2",
                    params![cid, key],
                )
                .map_err(|e| e.to_string())?;
            }
            for k in snap
                .committed_idempotency_keys
                .difference(&*b.committed_idempotency_keys)
            {
                tx.execute(
                    "INSERT OR REPLACE INTO idempotency_keys (character_id, idem_key) VALUES (?1, ?2)",
                    params![k.0, k.1],
                )
                .map_err(|e| e.to_string())?;
                #[cfg(any(test, feature = "legacy-json-snapshot"))]
                if let Some(d) = delta.as_deref_mut() {
                    d.added_idempotency_keys.push(k.clone());
                }
            }
        }
    } else {
        for k in &snap.committed_idempotency_keys {
            tx.execute(
                "INSERT OR REPLACE INTO idempotency_keys (character_id, idem_key) VALUES (?1, ?2)",
                params![k.0, k.1],
            )
            .map_err(|e| e.to_string())?;
            #[cfg(any(test, feature = "legacy-json-snapshot"))]
            if let Some(d) = delta.as_deref_mut() {
                d.added_idempotency_keys.push(k.clone());
            }
        }
    }

    // 30. licensed_skins
    sync_cell_map(
        tx,
        before.map(|b| &b.licensed_skins),
        &snap.licensed_skins,
        |tx, &cid| {
            tx.execute(
                "DELETE FROM licensed_skins WHERE character_id = ?1",
                params![cid],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
        |tx, &cid, skins| {
            let js = to_checked_json_string(skins)?;
            tx.execute(
                "INSERT OR REPLACE INTO licensed_skins (character_id, skins_json) VALUES (?1, ?2)",
                params![cid, js],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 31. applied_ship_skins
    sync_cell_map(
        tx,
        before.map(|b| &b.applied_ship_skins),
        &snap.applied_ship_skins,
        |tx, &sid| {
            tx.execute(
                "DELETE FROM applied_ship_skins WHERE ship_id = ?1",
                params![sid],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
        |tx, &sid, &skin_id| {
            tx.execute(
                "INSERT OR REPLACE INTO applied_ship_skins (ship_id, skin_id) VALUES (?1, ?2)",
                params![sid, skin_id as i64],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 32. char_mining_ledger (synced via industry_market_pve_store::sync_mining_ledger)

    // 33. asteroids
    sync_cell_map(
        tx,
        before.map(|b| &b.asteroids),
        &snap.asteroids,
        |tx, &id| {
            tx.execute("DELETE FROM asteroids WHERE asteroid_id = ?1", params![id])
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
        |tx, &id, ast| {
            if !ast.position.0.is_finite()
                || !ast.position.1.is_finite()
                || !ast.position.2.is_finite()
                || !ast.radius.is_finite()
            {
                return Err(format!("non-finite float in asteroids[{id}]"));
            }
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
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    Ok(())
}

pub fn replace_sqlite_from_snapshot(
    conn: &mut Connection,
    snap: &WorldStateSnapshot,
) -> Result<(), String> {
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
         DELETE FROM domain_rows;
         DELETE FROM entity_kv;",
    )
    .map_err(|e| e.to_string())?;

    tx.execute(
        "INSERT OR REPLACE INTO world_meta (meta_key, meta_value) VALUES ('next_entity_id', ?1), ('wal_seq', ?2), ('markets_seeded', ?3)",
        params![
            snap.next_entity_id,
            snap.wal_seq as i64,
            i64::from(snap.markets_seeded)
        ],
    )
    .map_err(|e| e.to_string())?;
    super::notes_lp_sequence_store::sync_d6(&tx, None, snap)?;

    let empty = WorldStateSnapshot::default();
    let mut changes = crate::tx_engine::ChangeSet::diff(snap.wal_seq, &empty, snap);
    changes.meta_changed = false;
    changes.cold_domains.d6_notes_lp_seq = false;
    let mut exec =
        crate::tx_engine::TxSqlExecutor::new(&tx, crate::tx_engine::TxFaultPlan::None);
    let _ = crate::tx_engine::apply_changeset_to_sqlite(
        &mut exec,
        &empty,
        snap,
        &changes,
        #[cfg(any(test, feature = "legacy-json-snapshot"))]
        false,
    )
    .map_err(|e| e.to_string())?;

    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn sync_snapshot_diff_for_test(
    tx: &Transaction<'_>,
    before: &mut WorldStateSnapshot,
    after: &WorldStateSnapshot,
    need_wal_delta: bool,
) -> Result<WorldStateWalDelta, String> {
    let changes = crate::tx_engine::ChangeSet::diff(after.wal_seq, before, after);
    let mut exec =
        crate::tx_engine::TxSqlExecutor::new(tx, crate::tx_engine::TxFaultPlan::None);
    let delta = crate::tx_engine::apply_changeset_to_sqlite(
        &mut exec,
        before,
        after,
        &changes,
        need_wal_delta,
    )
    .map_err(|e| e.to_string())?;
    *before = after.clone();
    Ok(delta)
}

#[cfg(test)]
#[path = "sync_unit_tests.rs"]
mod sync_unit_tests;
#[cfg(test)]
#[path = "sync_edges_tests.rs"]
mod sync_edges_tests;
