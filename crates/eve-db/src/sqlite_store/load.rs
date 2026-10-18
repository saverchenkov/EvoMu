//! Relational SQLite world state loader.

use crate::sqlite_store::fingerprints::{compute_fingerprints, TableFingerprints};
use crate::types::*;
use eve_sde::{SolarSystemId, TypeId};
use rusqlite::Connection;

/// Loads the complete `WorldStateSnapshot` and its `TableFingerprints` from the SQLite database.
pub fn load_snapshot_from_sqlite(
    conn: &Connection,
) -> Result<(WorldStateSnapshot, TableFingerprints), String> {
    let mut snap = WorldStateSnapshot {
        next_entity_id: 200_000_000,
        ..Default::default()
    };

    {
        let mut stmt = conn
            .prepare("SELECT meta_key, meta_value FROM world_meta")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (k, v) = r.map_err(|e| e.to_string())?;
            match k.as_str() {
                "next_entity_id" => snap.next_entity_id = v,
                "wal_seq" => snap.wal_seq = v as u64,
                "markets_seeded" => snap.markets_seeded = v != 0,
                _ => {}
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT username, user_id, password_token, role, banned_until_unix, ban_reason FROM accounts")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(AccountRecord {
                    username: r.get(0)?,
                    user_id: r.get::<_, i64>(1)? as u64,
                    password_token: r.get(2)?,
                    role: r.get::<_, i64>(3)? as u64,
                    banned_until_unix: r.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                    ban_reason: r.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let a = r.map_err(|e| e.to_string())?;
            snap.accounts.insert(a.username.clone(), a);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM characters")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let ch: CharacterRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.characters.insert(ch.character_id, ch);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT character_id, type_id, level, skill_points FROM skills")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    TrainedSkillRecord {
                        type_id: r.get::<_, i64>(1)? as TypeId,
                        level: r.get::<_, i64>(2)? as u8,
                        skill_points: r.get(3)?,
                    },
                ))
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (cid, sk) = r.map_err(|e| e.to_string())?;
            snap.skills.entry(cid).or_default().insert(sk.type_id, sk);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT character_id, type_id, target_level, start_sp, end_sp, duration_seconds FROM skill_queues ORDER BY character_id ASC, queue_index ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    SkillQueueEntry {
                        type_id: r.get::<_, i64>(1)? as TypeId,
                        target_level: r.get::<_, i64>(2)? as u8,
                        start_sp: r.get(3)?,
                        end_sp: r.get(4)?,
                        duration_seconds: r.get(5)?,
                    },
                ))
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (cid, q) = r.map_err(|e| e.to_string())?;
            snap.skill_queues.entry(cid).or_default().push(q);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT character_id, clones_json FROM jump_clones")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (cid, js) = r.map_err(|e| e.to_string())?;
            let clones: Vec<JumpCloneRecord> =
                serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.jump_clones.insert(cid, clones);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT character_id, implants_json FROM active_implants")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (cid, js) = r.map_err(|e| e.to_string())?;
            let imps: Vec<TypeId> = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.active_implants.insert(cid, imps);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM items")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let item: InventoryItemRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.items.insert(item.item_id, item);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM saved_fittings")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let fit: SavedFittingRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.saved_fittings.insert(fit.fitting_id, fit);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT order_id, owner_id, station_id, region_id, type_id, is_bid, price_cents, vol_entered, vol_remaining, min_volume, is_npc, escrow_cents FROM market_orders")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
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
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let ord = r.map_err(|e| e.to_string())?;
            snap.market_orders.insert(ord.order_id, ord);
        }
    }

    {
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

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM contracts")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let c: ContractRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.contracts.insert(c.contract_id, c);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT job_id, installer_id, facility_id, blueprint_item_id, blueprint_type_id, activity_id, runs, product_type_id, status, duration_seconds, start_unix FROM industry_jobs")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
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
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let j = r.map_err(|e| e.to_string())?;
            snap.industry_jobs.insert(j.job_id, j);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM planetary_colonies")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let col: PlanetaryColonyRecord =
                serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.planetary_colonies
                .insert((col.owner_id, col.planet_id), col);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM missions")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let m: ActiveMissionRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.active_missions
                .insert((m.character_id, m.mission_id), m);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT character_id, tally FROM mission_tally")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as u32))
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (cid, t) = r.map_err(|e| e.to_string())?;
            snap.mission_tally.insert(cid, t);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT character_id, tx_json FROM standings")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (cid, js) = r.map_err(|e| e.to_string())?;
            let list: Vec<StandingTransactionRecord> =
                serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.standing_transactions.insert(cid, list);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM npe_progress")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let npe: NpeProgressRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.npe_progress.insert(npe.character_id, npe);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT bookmark_id, owner_id, label, solar_system_id, x, y, z FROM bookmarks")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(BookmarkRecord {
                    bookmark_id: r.get(0)?,
                    owner_id: r.get(1)?,
                    label: r.get(2)?,
                    solar_system_id: r.get::<_, i64>(3)? as SolarSystemId,
                    x: r.get(4)?,
                    y: r.get(5)?,
                    z: r.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let bm = r.map_err(|e| e.to_string())?;
            snap.bookmarks.insert(bm.bookmark_id, bm);
        }
    }

    {
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

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM fleets")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let fl: FleetRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.fleets.insert(fl.fleet_id, fl);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM corps")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let corp: CorporationRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.corporations.insert(corp.corporation_id, corp);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM alliances")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let al: AllianceRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.alliances.insert(al.alliance_id, al);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT war_id, aggressor_corp_id, defender_corp_id, mutual, started_unix, active FROM wars")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(WarRecord {
                    war_id: r.get(0)?,
                    aggressor_corp_id: r.get(1)?,
                    defender_corp_id: r.get(2)?,
                    mutual: r.get::<_, i64>(3)? != 0,
                    started_unix: r.get::<_, i64>(4)? as u64,
                    active: r.get::<_, i64>(5)? != 0,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let w = r.map_err(|e| e.to_string())?;
            snap.wars.insert(w.war_id, w);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT project_id, corporation_id, name, target_quantity, current_quantity, isk_per_unit_cents, escrow_remaining_cents FROM corp_projects")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(CorpProjectRecord {
                    project_id: r.get(0)?,
                    corporation_id: r.get(1)?,
                    name: r.get(2)?,
                    target_quantity: r.get::<_, i64>(3)? as u64,
                    current_quantity: r.get::<_, i64>(4)? as u64,
                    isk_per_unit: IskCents(r.get(5)?),
                    escrow_remaining: IskCents(r.get(6)?),
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let p = r.map_err(|e| e.to_string())?;
            snap.corp_projects.insert(p.project_id, p);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT kill_id, victim_character_id, attacker_character_id, solar_system_id, ship_type_id, total_value_isk FROM killmails")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(KillmailRecord {
                    kill_id: r.get(0)?,
                    victim_character_id: r.get(1)?,
                    attacker_character_id: r.get(2)?,
                    solar_system_id: r.get::<_, i64>(3)? as SolarSystemId,
                    ship_type_id: r.get::<_, i64>(4)? as TypeId,
                    total_value_isk: r.get(5)?,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let km = r.map_err(|e| e.to_string())?;
            snap.killmails.insert(km.kill_id, km);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT kill_right_id, owner_id, target_id, activation_fee_cents, is_active_suspect FROM kill_rights")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(KillRightRecord {
                    kill_right_id: r.get(0)?,
                    owner_id: r.get(1)?,
                    target_id: r.get(2)?,
                    activation_fee: IskCents(r.get(3)?),
                    is_active_suspect: r.get::<_, i64>(4)? != 0,
                })
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let kr = r.map_err(|e| e.to_string())?;
            snap.kill_rights.insert(kr.kill_right_id, kr);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM structures")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let st: UpwellStructureRecord = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.structures.insert(st.structure_id, st);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT record_json FROM system_states")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let js = r.map_err(|e| e.to_string())?;
            let st: SystemDynamicState = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.system_states.insert(st.solar_system_id, st);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT character_id, idem_key FROM idempotency_keys")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for r in rows {
            snap.committed_idempotency_keys
                .insert(r.map_err(|e| e.to_string())?);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT character_id, skins_json FROM licensed_skins")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (cid, js) = r.map_err(|e| e.to_string())?;
            let skins: Vec<u32> = serde_json::from_str(&js).map_err(|e| e.to_string())?;
            snap.licensed_skins.insert(cid, skins);
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT ship_id, skin_id FROM applied_ship_skins")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as u32))
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (sid, skin_id) = r.map_err(|e| e.to_string())?;
            snap.applied_ship_skins.insert(sid, skin_id);
        }
    }

    let fp = compute_fingerprints(&snap);
    Ok((snap, fp))
}
