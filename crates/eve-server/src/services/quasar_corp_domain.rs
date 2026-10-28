//! `quasar_corp_domain` — State-backed Quasar alliance & corporation governance, accounting,
//! divisions, home station, ALSC logs, industry, members, titles, votes, starbases, medals,
//! market entries, NPC standings, and planetary customs offices.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    require_admin_role, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use eve_db::{CorporationRecord, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::BTreeMap;
use std::sync::Arc;

fn any(type_name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value,
    }
}

fn seq_id(field: u32, id: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    encode_bytes_field(field, &b, out);
}

fn encode_f32_field(field: u32, v: f32, out: &mut Vec<u8>) {
    out.push(((field as u8) << 3) | 5);
    out.extend_from_slice(&v.to_le_bytes());
}

fn zigzag64(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn encode_isk_currency(cents: i64) -> Vec<u8> {
    let units = cents / 100;
    let nanos = (cents % 100) * 10_000_000;
    let mut b = Vec::new();
    encode_varint_field(1, zigzag64(units), &mut b);
    encode_varint_field(2, zigzag64(nanos), &mut b);
    b
}

fn encode_standing_value(standing: f64) -> Vec<u8> {
    let units = standing.trunc() as i64;
    let nanos = ((standing - standing.trunc()) * 1_000_000_000.0).round() as i64;
    let mut b = Vec::new();
    encode_varint_field(1, zigzag64(units), &mut b);
    encode_varint_field(2, zigzag64(nanos), &mut b);
    b
}

fn extract_id(msg: &DynamicQuasarMessage, field: u32) -> Option<u32> {
    match msg.get(field)? {
        DynamicValue::Message(m) => match m.get(1)? {
            DynamicValue::Uint32(v) => Some(*v),
            DynamicValue::Uint64(v) => u32::try_from(*v).ok(),
            DynamicValue::Int32(v) => u32::try_from(*v).ok(),
            DynamicValue::Int64(v) => u32::try_from(*v).ok(),
            DynamicValue::Message(inner) => match inner.get(1)? {
                DynamicValue::Uint32(v) => Some(*v),
                DynamicValue::Uint64(v) => u32::try_from(*v).ok(),
                DynamicValue::Int32(v) => u32::try_from(*v).ok(),
                DynamicValue::Int64(v) => u32::try_from(*v).ok(),
                _ => None,
            },
            _ => None,
        },
        DynamicValue::Uint32(v) => Some(*v),
        DynamicValue::Uint64(v) => u32::try_from(*v).ok(),
        DynamicValue::Int32(v) => u32::try_from(*v).ok(),
        DynamicValue::Int64(v) => u32::try_from(*v).ok(),
        _ => None,
    }
    .filter(|&v| v > 0)
}

fn resolve_authorized_corp(
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
    corp_field: u32,
    db: &WorldRepository,
) -> Result<i64, (u32, String)> {
    let is_admin = require_admin_role(ctx).is_ok();
    let caller_cid = ctx.effective_character_id();
    if caller_cid.is_none() && !is_admin {
        return Err((STATUS_FORBIDDEN, "Character authentication required".into()));
    }
    let caller_corp = ctx.effective_corporation_id().or_else(|| {
        let cid = caller_cid? as i64;
        db.with_snapshot(|s| s.characters.get(&cid).map(|c| c.corporation_id as u32))
    });
    let req_corp = extract_id(msg, corp_field)
        .or_else(|| if corp_field != 1 { extract_id(msg, 1) } else { None });
    let target = match (req_corp, caller_corp) {
        (Some(req), Some(own)) => {
            if req != own && !is_admin {
                return Err((STATUS_FORBIDDEN, "Cross-corporation access denied".into()));
            }
            req
        }
        (Some(req), None) if is_admin => req,
        (None, Some(own)) => own,
        _ => return Err((STATUS_FORBIDDEN, "Corporation context required".into())),
    };
    Ok(target as i64)
}

fn check_corp_admin_role(
    ctx: &AuthoritativeContext,
    db: &WorldRepository,
) -> Result<(u32, i64), (u32, String)> {
    let Some(cid) = ctx.effective_character_id() else {
        return Err((STATUS_FORBIDDEN, "Authentication required".into()));
    };
    let corpid = ctx
        .effective_corporation_id()
        .or_else(|| db.with_snapshot(|s| s.characters.get(&(cid as i64)).map(|c| c.corporation_id as u32)))
        .ok_or_else(|| (STATUS_FORBIDDEN, "Missing corporation context".into()))? as i64;
    let corp_role = if ctx.corp_role_mask != 0 {
        ctx.corp_role_mask
    } else {
        ctx.identity
            .as_ref()
            .and_then(|id| id.roles.as_ref().map(|r| r.bitmask))
            .unwrap_or(0)
    };
    let is_admin = (corp_role & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_PERSONNEL_MANAGER)) != 0
        || require_admin_role(ctx).is_ok()
        || db.with_snapshot(|s| s.corporations.get(&corpid).is_some_and(|c| c.ceo_id == cid as i64));
    if !is_admin {
        return Err((STATUS_FORBIDDEN, "Insufficient corporation role".into()));
    }
    Ok((cid, corpid))
}

fn parse_location_flag(s: &str) -> u64 {
    match s {
        "Hangar" | "4" => 4,
        "Cargo" | "5" => 5,
        "CorpSAG1" | "116" => 116,
        "CorpSAG2" | "117" => 117,
        "CorpSAG3" | "118" => 118,
        "CorpSAG4" | "119" => 119,
        "CorpSAG5" | "120" => 120,
        "CorpSAG6" | "121" => 121,
        "CorpSAG7" | "122" => 122,
        other => other.parse::<u64>().unwrap_or(4),
    }
}

fn role_names_to_mask(names: &[String]) -> u64 {
    let mut mask = 0u64;
    for r in names {
        match r.as_str() {
            "Director" => mask |= eve_db::corp_roles::ROLE_DIRECTOR,
            "Personnel_Manager" | "PersonnelManager" => mask |= eve_db::corp_roles::ROLE_PERSONNEL_MANAGER,
            "Accountant" => mask |= eve_db::corp_roles::ROLE_ACCOUNTANT,
            "Security_Officer" | "SecurityOfficer" => mask |= eve_db::corp_roles::ROLE_SECURITY_OFFICER,
            "Factory_Manager" | "FactoryManager" => mask |= eve_db::corp_roles::ROLE_FACTORY_MANAGER,
            "Station_Manager" | "StationManager" => mask |= eve_db::corp_roles::ROLE_STATION_MANAGER,
            "Auditor" => mask |= eve_db::corp_roles::ROLE_AUDITOR,
            "Diplomat" => mask |= eve_db::corp_roles::ROLE_DIPLOMAT,
            "Trader" => mask |= eve_db::corp_roles::ROLE_TRADER,
            other => {
                if let Ok(bit) = other.parse::<u64>() {
                    mask |= bit;
                }
            }
        }
    }
    mask
}

fn encode_role_collection(field: u32, mask: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, mask, &mut b);
    encode_bytes_field(field, &b, out);
}

fn starbase_role_enum(role_str: &str) -> u64 {
    match role_str {
        "config_starbase_equipment" => 1,
        "corporation_member" | "corp" => 2,
        "alliance_member" | "alliance" => 3,
        "starbase_fuel_technician" => 4,
        _ => 2,
    }
}

pub fn register(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    let sde = Arc::clone(&router.sde);

    // ========================================================================
    // ALLIANCE DOMAIN
    // ========================================================================
    {
        let d = Arc::clone(&db);
        router.register_handler(
            "eve.alliance.GetRequest",
            Arc::new(move |ctx, _any, msg| {
                let all_id = extract_id(msg, 1).map(i64::from).or_else(|| {
                    ctx.effective_character_id().and_then(|cid| {
                        d.with_snapshot(|s| s.characters.get(&(cid as i64)).and_then(|c| c.alliance_id))
                    })
                }).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing alliance identifier".into()))?;
                let (name, short_name, founded, creator_char, creator_corp, exec_corp) =
                    d.with_snapshot(|s| {
                        let a = s.alliances.get(&all_id)?;
                        let meta = s.alliance_lifecycle_meta.get(&all_id);
                        let founded = meta.map(|m| m.date_founded_unix).unwrap_or(1_700_000_000);
                        let c_char = meta
                            .map(|m| m.creator_id as u64)
                            .or_else(|| s.corporations.get(&a.executor_corp_id).map(|c| c.ceo_id as u64))
                            .unwrap_or(0);
                        let c_corp = meta
                            .map(|m| m.creator_corporation_id as u64)
                            .unwrap_or(a.executor_corp_id as u64);
                        Some((
                            a.name.clone(),
                            a.short_name.clone(),
                            founded,
                            c_char,
                            c_corp,
                            a.executor_corp_id as u64,
                        ))
                    })
                    .ok_or_else(|| (STATUS_NOT_FOUND, format!("Alliance {all_id} not found")))?;
                let (mut attr, mut ts, mut out) = (Vec::new(), Vec::new(), Vec::new());
                encode_string_field(1, &name, &mut attr);
                encode_string_field(2, &short_name, &mut attr);
                encode_varint_field(1, founded, &mut ts);
                encode_bytes_field(3, &ts, &mut attr);
                seq_id(4, creator_char, &mut attr);
                seq_id(5, creator_corp, &mut attr);
                seq_id(6, exec_corp, &mut attr);
                encode_bytes_field(1, &attr, &mut out);
                Ok(any("eve.alliance.GetResponse", out))
            }),
        );
    }

    {
        let d = Arc::clone(&db);
        router.register_handler(
            "eve.alliance.GetAllRequest",
            Arc::new(move |_ctx, _any, _msg| {
                let ids = d.with_snapshot(|s| {
                    let mut v: Vec<i64> = s.alliances.keys().copied().collect();
                    v.sort_unstable();
                    v
                });
                let mut out = Vec::new();
                for id in ids {
                    seq_id(1, id as u64, &mut out);
                }
                Ok(any("eve.alliance.GetAllResponse", out))
            }),
        );
    }

    {
        let d = Arc::clone(&db);
        router.register_handler(
            "eve.alliance.SearchRequest",
            Arc::new(move |_ctx, _any, msg| {
                let query = match msg.get(1) {
                    Some(DynamicValue::String(s)) => s.to_ascii_lowercase(),
                    Some(DynamicValue::Bytes(b)) => String::from_utf8_lossy(b).to_ascii_lowercase(),
                    _ => String::new(),
                };
                let ids = d.with_snapshot(|s| {
                    let mut v: Vec<i64> = s
                        .alliances
                        .values()
                        .filter(|a| {
                            query.is_empty()
                                || a.name.to_ascii_lowercase().contains(&query)
                                || a.short_name.to_ascii_lowercase().contains(&query)
                        })
                        .map(|a| a.alliance_id)
                        .collect();
                    v.sort_unstable();
                    v
                });
                let mut out = Vec::new();
                for id in ids {
                    seq_id(1, id as u64, &mut out);
                }
                Ok(any("eve.alliance.SearchResponse", out))
            }),
        );
    }

    {
        let d = Arc::clone(&db);
        router.register_handler(
            "eve.alliance.corporation.GetMembersRequest",
            Arc::new(move |ctx, _any, msg| {
                let all_id = extract_id(msg, 1).map(i64::from).or_else(|| {
                    ctx.effective_character_id().and_then(|cid| {
                        d.with_snapshot(|s| s.characters.get(&(cid as i64)).and_then(|c| c.alliance_id))
                    })
                }).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing alliance identifier".into()))?;
                let corp_ids = d
                    .with_snapshot(|s| {
                        let a = s.alliances.get(&all_id)?;
                        let mut v: Vec<i64> = a.member_corp_ids.iter().copied().collect();
                        for corp in s.corporations.values() {
                            if corp.alliance_id == Some(all_id) && !v.contains(&corp.corporation_id) {
                                v.push(corp.corporation_id);
                            }
                        }
                        v.sort_unstable();
                        Some(v)
                    })
                    .ok_or_else(|| (STATUS_NOT_FOUND, format!("Alliance {all_id} not found")))?;
                let mut out = Vec::new();
                for cid in corp_ids {
                    seq_id(1, cid as u64, &mut out);
                }
                Ok(any("eve.alliance.corporation.GetMembersResponse", out))
            }),
        );
    }

    // ========================================================================
    // CORPORATION ACCOUNTING, DIVISIONS, HOMESTATION, ALLIANCE, ALSC, INDUSTRY
    // ========================================================================
    {
        let d = Arc::clone(&db);
        router.register_handler(
            "eve.corporation.accounting.wallet.GetAllDivisionBalanceRequest",
            Arc::new(move |ctx, _any, msg| {
                let corpid = resolve_authorized_corp(ctx, msg, 2, &d)?;
                let divs = d.with_snapshot(|s| {
                    let c = s.corporations.get(&corpid).ok_or_else(|| {
                        (STATUS_NOT_FOUND, format!("Corporation {corpid} not found"))
                    })?;
                    let w = if c.wallet_divisions.is_empty() {
                        CorporationRecord::default_wallet_divisions(c.master_wallet)
                    } else {
                        c.wallet_divisions.clone()
                    };
                    Ok::<_, (u32, String)>(w)
                })?;
                let mut out = Vec::new();
                for div in divs {
                    let key_id = if (1..=7).contains(&div.division_id) {
                        999 + div.division_id as u64
                    } else {
                        1000
                    };
                    let mut db_msg = Vec::new();
                    encode_varint_field(1, key_id, &mut db_msg);
                    encode_bytes_field(2, &encode_isk_currency(div.balance.0), &mut db_msg);
                    encode_bytes_field(1, &db_msg, &mut out);
                }
                Ok(any(
                    "eve.corporation.accounting.wallet.GetAllDivisionBalanceResponse",
                    out,
                ))
            }),
        );
    }

    {
        let d = Arc::clone(&db);
        router.register_handler(
            "eve.corporation.divisions.GetRequest",
            Arc::new(move |ctx, _any, msg| {
                let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
                let (hangar_map, wallet_divs) = d.with_snapshot(|s| {
                    let c = s.corporations.get(&corpid).ok_or_else(|| {
                        (STATUS_NOT_FOUND, format!("Corporation {corpid} not found"))
                    })?;
                    let h = s.corp_hangar_divisions.get(&corpid).cloned().unwrap_or_default();
                    let w = if c.wallet_divisions.is_empty() {
                        CorporationRecord::default_wallet_divisions(c.master_wallet)
                    } else {
                        c.wallet_divisions.clone()
                    };
                    Ok::<_, (u32, String)>((h, w))
                })?;
                let mut out = Vec::new();
                for idx in 1..=7u8 {
                    let mut h_msg = Vec::new();
                    encode_varint_field(1, idx as u64, &mut h_msg);
                    if let Some(custom) = hangar_map.get(&idx).filter(|s| !s.is_empty()) {
                        encode_string_field(3, custom, &mut h_msg);
                    } else {
                        encode_varint_field(2, 1, &mut h_msg);
                    }
                    encode_bytes_field(1, &h_msg, &mut out);
                }
                for w in wallet_divs {
                    let mut w_msg = Vec::new();
                    encode_varint_field(1, w.division_id as u64, &mut w_msg);
                    if !w.name.is_empty() {
                        encode_string_field(3, &w.name, &mut w_msg);
                    } else {
                        encode_varint_field(2, 1, &mut w_msg);
                    }
                    encode_bytes_field(2, &w_msg, &mut out);
                }
                Ok(any("eve.corporation.divisions.GetResponse", out))
            }),
        );
    }

    {
        let d = Arc::clone(&db);
        router.register_handler(
            "eve.corporation.homestation.GetRequest",
            Arc::new(move |ctx, _any, msg| {
                let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
                let st_id = d.with_snapshot(|s| {
                    let corp = s.corporations.get(&corpid).ok_or_else(|| {
                        (STATUS_NOT_FOUND, format!("Corporation {corpid} not found"))
                    })?;
                    corp.hq_station_id.ok_or_else(|| {
                        (
                            STATUS_NOT_FOUND,
                            format!("Corporation {corpid} has no configured home station"),
                        )
                    })
                })?;
                let mut out = Vec::new();
                seq_id(1, st_id as u64, &mut out);
                Ok(any("eve.corporation.homestation.GetResponse", out))
            }),
        );
    }

    {
        let d = Arc::clone(&db);
        router.register_handler(
            "eve.corporation.alliance.api.GetCurrentMembershipRequest",
            Arc::new(move |ctx, _any, msg| {
                let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
                let aid = d.with_snapshot(|s| {
                    s.corporations
                        .get(&corpid)
                        .ok_or_else(|| (STATUS_NOT_FOUND, format!("Corporation {corpid} not found")))
                        .map(|c| c.alliance_id)
                })?;
                let mut out = Vec::new();
                if let Some(a) = aid.filter(|&x| x > 0) {
                    seq_id(1, a as u64, &mut out);
                } else {
                    encode_varint_field(2, 1, &mut out);
                }
                Ok(any(
                    "eve.corporation.alliance.api.GetCurrentMembershipResponse",
                    out,
                ))
            }),
        );
    }

    {
        let d = Arc::clone(&db);
        router.register_handler(
            "eve.corporation.alliancemembershiprecord.GetAllRequest",
            Arc::new(move |ctx, _any, msg| {
                let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
                let history = d.with_snapshot(|s| {
                    s.corp_alliance_history
                        .get(&corpid)
                        .cloned()
                        .unwrap_or_default()
                });
                let mut out = Vec::new();
                for rec in history {
                    let (mut r_msg, mut at_msg, mut ts_msg) = (Vec::new(), Vec::new(), Vec::new());
                    seq_id(1, rec.record_id as u64, &mut r_msg);
                    if let Some(aid) = rec.alliance_id.filter(|&a| a > 0) {
                        let mut al_msg = Vec::new();
                        seq_id(1, aid as u64, &mut al_msg);
                        encode_varint_field(2, u64::from(rec.is_deleted.unwrap_or(false)), &mut al_msg);
                        encode_bytes_field(2, &al_msg, &mut at_msg);
                    } else {
                        encode_varint_field(1, 1, &mut at_msg);
                    }
                    encode_varint_field(1, rec.start_date_unix, &mut ts_msg);
                    encode_bytes_field(3, &ts_msg, &mut at_msg);
                    encode_bytes_field(2, &at_msg, &mut r_msg);
                    encode_bytes_field(1, &r_msg, &mut out);
                }
                Ok(any(
                    "eve.corporation.alliancemembershiprecord.GetAllResponse",
                    out,
                ))
            }),
        );
    }

    {
        let d = Arc::clone(&db);
        router.register_handler(
            "eve.corporation.alsclogs.GetRequest",
            Arc::new(move |ctx, _any, msg| {
                let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
                let logs = d.with_snapshot(|s| {
                    s.corp_container_logs
                        .get(&corpid)
                        .cloned()
                        .unwrap_or_default()
                });
                let mut out = Vec::new();
                for entry in logs {
                    let (mut e_msg, mut ts_msg, mut act_msg) = (Vec::new(), Vec::new(), Vec::new());
                    encode_varint_field(1, entry.logged_at_unix, &mut ts_msg);
                    encode_bytes_field(1, &ts_msg, &mut e_msg);
                    seq_id(2, entry.container_id as u64, &mut e_msg);
                    seq_id(3, entry.container_type_id as u64, &mut e_msg);
                    seq_id(4, entry.location_id as u64, &mut e_msg);
                    encode_varint_field(5, parse_location_flag(&entry.location_flag), &mut e_msg);
                    let act_lower = entry.action.to_ascii_lowercase();
                    let encode_type_qty = |target: &mut Vec<u8>| {
                        if let Some(tid) = entry.type_id {
                            seq_id(1, tid as u64, target);
                        }
                        if let Some(q) = entry.quantity {
                            encode_varint_field(2, q.max(0) as u64, target);
                        }
                    };
                    match act_lower.as_str() {
                        "assemble" => {
                            let mut sub = Vec::new();
                            if let Some(tid) = entry.type_id {
                                seq_id(2, tid as u64, &mut sub);
                            }
                            encode_bytes_field(1, &sub, &mut act_msg);
                        }
                        "repackage" => encode_bytes_field(2, &[], &mut act_msg),
                        "set_name" | "setname" => encode_bytes_field(3, &[], &mut act_msg),
                        "move" => encode_bytes_field(4, &[], &mut act_msg),
                        "set_password" | "setpassword" => {
                            let mut sub = Vec::new();
                            encode_varint_field(1, 1, &mut sub);
                            encode_bytes_field(5, &sub, &mut act_msg);
                        }
                        "add" => {
                            let mut sub = Vec::new();
                            encode_type_qty(&mut sub);
                            encode_bytes_field(6, &sub, &mut act_msg);
                        }
                        "lock" => {
                            let mut sub = Vec::new();
                            encode_type_qty(&mut sub);
                            encode_bytes_field(7, &sub, &mut act_msg);
                        }
                        "unlock" => {
                            let mut sub = Vec::new();
                            encode_type_qty(&mut sub);
                            encode_bytes_field(8, &sub, &mut act_msg);
                        }
                        "enter_password" | "enterpassword" => {
                            let mut sub = Vec::new();
                            encode_varint_field(1, 1, &mut sub);
                            encode_bytes_field(9, &sub, &mut act_msg);
                        }
                        "configure" => {
                            let mut sub = Vec::new();
                            encode_varint_field(1, entry.old_config_bitmask.unwrap_or(0) as u64, &mut sub);
                            encode_varint_field(2, entry.new_config_bitmask.unwrap_or(0) as u64, &mut sub);
                            encode_bytes_field(10, &sub, &mut act_msg);
                        }
                        _ => {
                            let mut sub = Vec::new();
                            encode_type_qty(&mut sub);
                            encode_bytes_field(if entry.type_id.is_some() { 6 } else { 7 }, &mut sub, &mut act_msg);
                        }
                    }
                    encode_bytes_field(6, &act_msg, &mut e_msg);
                    seq_id(7, entry.character_id as u64, &mut e_msg);
                    encode_bytes_field(1, &e_msg, &mut out);
                }
                Ok(any("eve.corporation.alsclogs.GetResponse", out))
            }),
        );
    }

    {
        let d = Arc::clone(&db);
        router.register_handler(
            "eve.corporation.industry.job.GetActiveRequest",
            Arc::new(move |ctx, _any, msg| {
                let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
                let jobs = d.with_snapshot(|s| {
                    let mut v: Vec<_> = s
                        .industry_jobs
                        .values()
                        .filter(|j| {
                            j.status == "Active"
                                && (s.characters
                                    .get(&j.installer_id)
                                    .is_some_and(|c| c.corporation_id == corpid)
                                    || s.corporations
                                        .get(&corpid)
                                        .is_some_and(|c| c.members.contains(&j.installer_id)))
                        })
                        .cloned()
                        .collect();
                    v.sort_by_key(|j| j.job_id);
                    v
                });
                let mut out = Vec::new();
                for j in jobs {
                    let (mut entry, mut sub) = (Vec::new(), Vec::new());
                    seq_id(1, j.job_id as u64, &mut entry);
                    seq_id(2, corpid as u64, &mut sub);
                    seq_id(3, j.installer_id as u64, &mut sub);
                    seq_id(4, j.facility_id as u64, &mut sub);
                    encode_varint_field(8, j.runs as u64, &mut sub);
                    encode_varint_field(14, j.duration_seconds, &mut sub);
                    seq_id(21, j.blueprint_item_id.max(0) as u64, &mut sub);
                    let entry_field = match j.activity_id {
                        3 => {
                            seq_id(22, j.blueprint_type_id as u64, &mut sub);
                            6
                        }
                        4 => {
                            seq_id(22, j.blueprint_type_id as u64, &mut sub);
                            7
                        }
                        5 => {
                            seq_id(22, j.product_type_id as u64, &mut sub);
                            seq_id(24, j.blueprint_type_id as u64, &mut sub);
                            2
                        }
                        8 => {
                            seq_id(22, j.product_type_id as u64, &mut sub);
                            seq_id(25, j.blueprint_type_id as u64, &mut sub);
                            3
                        }
                        9 | 11 => {
                            seq_id(23, j.product_type_id as u64, &mut sub);
                            seq_id(25, j.blueprint_type_id as u64, &mut sub);
                            5
                        }
                        _ => {
                            seq_id(23, j.product_type_id as u64, &mut sub);
                            seq_id(25, j.blueprint_type_id as u64, &mut sub);
                            4
                        }
                    };
                    encode_bytes_field(entry_field, &sub, &mut entry);
                    encode_bytes_field(1, &entry, &mut out);
                }
                Ok(any("eve.corporation.industry.job.GetActiveResponse", out))
            }),
        );
    }

    {
        let d = Arc::clone(&db);
        let sde_bp = Arc::clone(&sde);
        router.register_handler(
            "eve.corporation.industry.blueprints.GetRequest",
            Arc::new(move |ctx, _any, msg| {
                let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
                let items = d.with_snapshot(|s| {
                    let mut v: Vec<_> = s
                        .items
                        .values()
                        .filter(|i| {
                            i.owner_id == corpid
                                && (sde_bp.blueprints.contains_key(&i.type_id)
                                    || i.is_blueprint_copy
                                    || i.blueprint_runs_remaining.is_some())
                        })
                        .cloned()
                        .collect();
                    v.sort_by_key(|i| i.item_id);
                    v
                });
                let mut out = Vec::new();
                for item in items {
                    let me = item.dynamic_attributes.get(&1112).copied().unwrap_or(0.0).max(0.0) as u64;
                    let te = item.dynamic_attributes.get(&1113).copied().unwrap_or(0.0).max(0.0) as u64;
                    let mut loc = Vec::new();
                    seq_id(1, item.location_id as u64, &mut loc);
                    encode_varint_field(4, item.flag as u32 as u64, &mut loc);
                    if !item.singleton && item.quantity > 1 && !item.is_blueprint_copy {
                        let mut stack = Vec::new();
                        seq_id(1, item.item_id as u64, &mut stack);
                        seq_id(2, item.type_id as u64, &mut stack);
                        encode_bytes_field(3, &loc, &mut stack);
                        encode_varint_field(4, item.quantity as u64, &mut stack);
                        encode_bytes_field(3, &stack, &mut out);
                    } else if item.is_blueprint_copy {
                        let (mut bpc, mut attrs) = (Vec::new(), Vec::new());
                        seq_id(1, item.item_id as u64, &mut bpc);
                        seq_id(1, item.type_id as u64, &mut attrs);
                        encode_varint_field(2, me, &mut attrs);
                        encode_varint_field(3, te, &mut attrs);
                        encode_varint_field(4, zigzag64(item.blueprint_runs_remaining.unwrap_or(1) as i64), &mut attrs);
                        encode_bytes_field(2, &attrs, &mut bpc);
                        encode_bytes_field(3, &loc, &mut bpc);
                        encode_bytes_field(2, &bpc, &mut out);
                    } else {
                        let (mut bp, mut attrs) = (Vec::new(), Vec::new());
                        seq_id(1, item.item_id as u64, &mut bp);
                        seq_id(1, item.type_id as u64, &mut attrs);
                        encode_varint_field(2, me, &mut attrs);
                        encode_varint_field(3, te, &mut attrs);
                        encode_bytes_field(2, &attrs, &mut bp);
                        encode_bytes_field(3, &loc, &mut bp);
                        encode_bytes_field(1, &bp, &mut out);
                    }
                }
                Ok(any("eve.corporation.industry.blueprints.GetResponse", out))
            }),
        );
    }

    {
        let d = Arc::clone(&db);
        router.register_handler(
            "eve.corporation.industry.facilties.GetRequest",
            Arc::new(move |ctx, _any, msg| {
                let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
                let structs = d.with_snapshot(|s| {
                    let mut v: Vec<_> = s
                        .structures
                        .values()
                        .filter(|st| st.owner_corp_id == corpid)
                        .cloned()
                        .collect();
                    v.sort_by_key(|st| st.structure_id);
                    v
                });
                let mut out = Vec::new();
                for st in structs {
                    let (mut f_msg, mut at_msg) = (Vec::new(), Vec::new());
                    seq_id(1, st.structure_id as u64, &mut f_msg);
                    seq_id(1, corpid as u64, &mut at_msg);
                    seq_id(3, st.solar_system_id as u64, &mut at_msg);
                    seq_id(4, st.type_id as u64, &mut at_msg);
                    encode_varint_field(5, 1, &mut at_msg);
                    encode_bytes_field(2, &at_msg, &mut f_msg);
                    encode_bytes_field(1, &f_msg, &mut out);
                }
                Ok(any("eve.corporation.industry.facilties.GetResponse", out))
            }),
        );
    }

    {
        let d = Arc::clone(&db);
        router.register_handler(
            "eve.corporation.industry.observer.GetAllDetailsRequest",
            Arc::new(move |ctx, _any, msg| {
                let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
                let obs_ids = d.with_snapshot(|s| {
                    let mut ids: Vec<i64> = s
                        .structures
                        .values()
                        .filter(|st| {
                            st.owner_corp_id == corpid
                                && s.observer_mining_ledger.contains_key(&st.structure_id)
                        })
                        .map(|st| st.structure_id)
                        .collect();
                    ids.sort_unstable();
                    ids
                });
                let mut out = Vec::new();
                for oid in obs_ids {
                    let (mut o_msg, mut at_msg, mut ts_msg) = (Vec::new(), Vec::new(), Vec::new());
                    seq_id(1, oid as u64, &mut o_msg);
                    encode_varint_field(1, ctx.now_unix(), &mut ts_msg);
                    encode_bytes_field(1, &ts_msg, &mut at_msg);
                    encode_bytes_field(2, &at_msg, &mut o_msg);
                    encode_bytes_field(1, &o_msg, &mut out);
                }
                Ok(any(
                    "eve.corporation.industry.observer.GetAllDetailsResponse",
                    out,
                ))
            }),
        );
    }

    // ========================================================================
    // CORPORATION MEMBERS, GOVERNANCE, TITLES, VOTES, STARBASES, STANDINGS, PI
    // ========================================================================
    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.members.GetRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let members = d.with_snapshot(|s| {
                let mut v: Vec<i64> = s.corporations.get(&corpid).map(|c| c.members.iter().copied().collect()).unwrap_or_default();
                for ch in s.characters.values() {
                    if ch.corporation_id == corpid && !v.contains(&ch.character_id) { v.push(ch.character_id); }
                }
                v.sort_unstable();
                v
            });
            let mut out = Vec::new();
            for cid in members { seq_id(1, cid as u64, &mut out); }
            Ok(any("eve.corporation.members.GetResponse", out))
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.members.GetLimitRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let limit = d.with_snapshot(|s| {
                s.corp_member_limits.get(&corpid).copied()
                    .or_else(|| s.corp_governance.get(&corpid).and_then(|g| g.member_limit.map(|l| l as i64)))
                    .unwrap_or(500)
            });
            let mut out = Vec::new();
            encode_varint_field(1, limit.max(0) as u64, &mut out);
            Ok(any("eve.corporation.members.GetLimitResponse", out))
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.members.roles.GetRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let member_roles = d.with_snapshot(|s| {
                let mut members: Vec<i64> = s.corporations.get(&corpid).map(|c| c.members.iter().copied().collect()).unwrap_or_default();
                for ch in s.characters.values() {
                    if ch.corporation_id == corpid && !members.contains(&ch.character_id) { members.push(ch.character_id); }
                }
                members.sort_unstable();
                let corp_opt = s.corporations.get(&corpid);
                members.into_iter().map(|cid| {
                    let mask = corp_opt.and_then(|c| c.member_roles.get(&cid).copied().or_else(|| (c.ceo_id == cid).then_some(u64::MAX))).unwrap_or(0);
                    (cid, mask)
                }).collect::<Vec<_>>()
            });
            let mut out = Vec::new();
            for (cid, mask) in member_roles {
                let mut cr = Vec::new();
                seq_id(1, cid as u64, &mut cr);
                encode_varint_field(2, mask, &mut cr);
                encode_bytes_field(1, &cr, &mut out);
            }
            Ok(any("eve.corporation.members.roles.GetResponse", out))
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.members.titles.GetRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let rows = d.with_snapshot(|s| {
                let mut map: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
                for (&(c, cid), t_ids) in &s.corp_member_titles {
                    if c == corpid {
                        let e = map.entry(cid).or_default();
                        for &tid in t_ids { if !e.contains(&tid) { e.push(tid); } }
                    }
                }
                if let Some(gov) = s.corp_governance.get(&corpid) {
                    for (&cid, &mask) in &gov.member_titles {
                        let e = map.entry(cid).or_default();
                        for bit in 0..16u32 {
                            let tid = 1i64 << bit;
                            if (mask & (tid as u64)) != 0 && !e.contains(&tid) { e.push(tid); }
                        }
                    }
                }
                for v in map.values_mut() { v.sort_unstable(); }
                map
            });
            let mut out = Vec::new();
            for (cid, tids) in rows {
                let mut ct = Vec::new();
                seq_id(1, cid as u64, &mut ct);
                for tid in tids { seq_id(2, tid as u64, &mut ct); }
                encode_bytes_field(1, &ct, &mut out);
            }
            Ok(any("eve.corporation.members.titles.GetResponse", out))
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.members.tracking.GetRequest", Arc::new(move |ctx, _any, msg| {
            let (_caller_cid, caller_corpid) = check_corp_admin_role(ctx, &d)?;
            let corpid = extract_id(msg, 1).map(i64::from).unwrap_or(caller_corpid);
            if corpid != caller_corpid && require_admin_role(ctx).is_err() {
                return Err((STATUS_FORBIDDEN, "Cross-corporation tracking access denied".into()));
            }
            let rows = d.with_snapshot(|s| {
                let mut members: Vec<i64> = s.corporations.get(&corpid).map(|c| c.members.iter().copied().collect()).unwrap_or_default();
                for ch in s.characters.values() {
                    if ch.corporation_id == corpid && !members.contains(&ch.character_id) { members.push(ch.character_id); }
                }
                members.sort_unstable();
                members.into_iter().map(|cid| {
                    let sys_id = s.characters.get(&cid).map(|ch| ch.solar_system_id as u64).unwrap_or(0);
                    let logoff = s.character_login_stats.get(&cid).map(|st| st.last_logout_unix).unwrap_or(0);
                    (cid, sys_id, logoff)
                }).collect::<Vec<_>>()
            });
            let mut out = Vec::new();
            for (cid, sys_id, logoff) in rows {
                let mut t = Vec::new();
                seq_id(1, cid as u64, &mut t);
                encode_varint_field(2, sys_id, &mut t);
                encode_varint_field(3, logoff, &mut t);
                encode_bytes_field(1, &t, &mut out);
            }
            Ok(any("eve.corporation.members.tracking.GetResponse", out))
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.members.roles.auditlog.GetRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let history = d.with_snapshot(|s| s.corp_role_history.get(&corpid).cloned().unwrap_or_default());
            let mut out = Vec::new();
            for entry in history {
                let (mut rc, mut ts) = (Vec::new(), Vec::new());
                encode_varint_field(1, entry.changed_at_unix, &mut ts);
                encode_bytes_field(1, &ts, &mut rc);
                seq_id(2, entry.character_id as u64, &mut rc);
                seq_id(3, entry.issuer_id as u64, &mut rc);
                let rtype = match entry.role_type.as_str() {
                    "grantable_roles" => 2, "roles_at_hq" => 3, "grantable_roles_at_hq" => 4,
                    "roles_at_base" => 5, "grantable_roles_at_base" => 6, "roles_at_other" => 7,
                    "grantable_roles_at_other" => 8, _ => 1,
                };
                encode_varint_field(4, rtype, &mut rc);
                encode_role_collection(5, role_names_to_mask(&entry.old_roles), &mut rc);
                encode_role_collection(6, role_names_to_mask(&entry.new_roles), &mut rc);
                seq_id(7, entry.issuer_id as u64, &mut rc);
                encode_bytes_field(1, &rc, &mut out);
            }
            Ok(any("eve.corporation.members.roles.auditlog.GetResponse", out))
        }));
    }

    {
        let sde_npc = Arc::clone(&sde);
        router.register_handler("eve.corporation.npc.GetAllRequest", Arc::new(move |_ctx, _any, _msg| {
            let mut npc_corps: Vec<u32> = sde_npc.stations.values().map(|st| st.owner_corporation_id).filter(|&cid| cid > 0 && cid < 2_000_000).map(|cid| cid as u32).collect();
            npc_corps.sort_unstable();
            npc_corps.dedup();
            let mut out = Vec::new();
            for cid in npc_corps { seq_id(1, cid as u64, &mut out); }
            Ok(any("eve.corporation.npc.GetAllResponse", out))
        }));
    }

    router.register_handler("eve.corporation.npc.IsNPCCorporationRequest", Arc::new(move |_ctx, _any, msg| {
        let corpid = extract_id(msg, 1).unwrap_or(0);
        let mut out = Vec::new();
        encode_varint_field(1, u64::from(corpid > 0 && corpid < 2_000_000), &mut out);
        Ok(any("eve.corporation.npc.IsNPCCorporationResponse", out))
    }));

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.logo.api.GetRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = extract_id(msg, 1).or_else(|| ctx.effective_corporation_id()).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing corporation identifier".into()))? as i64;
            let ((s1, s2, s3, c1), (c2, c3)) = d.with_snapshot(|s| {
                if !s.corporations.contains_key(&corpid) && corpid >= 2_000_000 {
                    return Err((STATUS_NOT_FOUND, format!("Corporation {corpid} not found")));
                }
                Ok(s.corp_governance.get(&corpid).map(|g| (g.logo, g.logo_extra_colors)).unwrap_or(((415, 0, 0, 671), (0, 0))))
            })?;
            let mut logo_msg = Vec::new();
            for (shape, color) in [(s1, c1), (s2, c2), (s3, c3)] {
                if shape > 0 || color > 0 {
                    let mut layer = Vec::new();
                    encode_varint_field(1, shape.max(0) as u64, &mut layer);
                    encode_varint_field(2, color.max(0) as u64, &mut layer);
                    encode_bytes_field(1, &layer, &mut logo_msg);
                }
            }
            let mut out = Vec::new();
            encode_bytes_field(1, &logo_msg, &mut out);
            Ok(any("eve.corporation.logo.api.GetResponse", out))
        }));
    }

    {
        let d = Arc::clone(&db);
        let sde_off = Arc::clone(&sde);
        router.register_handler("eve.corporation.office.api.GetSolarSystemsRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let systems = d.with_snapshot(|s| {
                let mut v: Vec<u32> = Vec::new();
                for item in s.items.values() {
                    if item.owner_id == corpid && item.type_id == 27 {
                        if let Some(st) = sde_off.stations.get(&item.location_id) { v.push(st.solar_system_id); }
                        else if let Some(st) = s.structures.get(&item.location_id) { v.push(st.solar_system_id); }
                    }
                }
                if let Some(hq) = s.corporations.get(&corpid).and_then(|c| c.hq_station_id) {
                    if let Some(st) = sde_off.stations.get(&hq) { v.push(st.solar_system_id); }
                }
                v.sort_unstable();
                v.dedup();
                v
            });
            let mut out = Vec::new();
            for sys_id in systems { seq_id(1, sys_id as u64, &mut out); }
            Ok(any("eve.corporation.office.api.GetSolarSystemsResponse", out))
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.structure.GetAllWithDetailsRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let structures = d.with_snapshot(|s| {
                let mut v: Vec<(i64, String, u32)> = s.structures.values().filter(|st| st.owner_corp_id == corpid).map(|st| (st.structure_id, st.name.clone(), st.solar_system_id)).collect();
                v.sort_by_key(|(id, _, _)| *id);
                v
            });
            let mut out = Vec::new();
            for (sid, name, sys_id) in structures {
                let mut st = Vec::new();
                seq_id(1, sid as u64, &mut st);
                encode_string_field(2, &name, &mut st);
                seq_id(3, sys_id as u64, &mut st);
                encode_bytes_field(1, &st, &mut out);
            }
            Ok(any("eve.corporation.structure.GetAllWithDetailsResponse", out))
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.title.GetAllRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let titles = d.with_snapshot(|s| {
                let mut list = s.corp_titles.get(&corpid).cloned().unwrap_or_default();
                if list.is_empty() {
                    if let Some(gov) = s.corp_governance.get(&corpid) {
                        for (&tid, (name, _)) in &gov.titles {
                            list.push(eve_db::snapshot_ext::CorpTitleEntry { title_id: tid, name: name.clone(), ..Default::default() });
                        }
                    }
                }
                list.sort_by_key(|t| t.title_id);
                list
            });
            let mut out = Vec::new();
            for t in titles {
                let (mut t_msg, mut at_msg) = (Vec::new(), Vec::new());
                seq_id(1, t.title_id as u64, &mut t_msg);
                encode_string_field(1, &t.name, &mut at_msg);
                encode_bytes_field(2, &at_msg, &mut t_msg);
                for (f, r) in [(3, &t.roles), (4, &t.grantable_roles), (5, &t.roles_at_base), (6, &t.grantable_roles_at_base), (7, &t.roles_at_hq), (8, &t.grantable_roles_at_hq), (9, &t.roles_at_other), (10, &t.grantable_roles_at_other)] {
                    encode_role_collection(f, role_names_to_mask(r), &mut t_msg);
                }
                encode_bytes_field(1, &t_msg, &mut out);
            }
            Ok(any("eve.corporation.title.GetAllResponse", out))
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.vote.GetRequest", Arc::new(move |ctx, _any, msg| {
            let vote_id = extract_id(msg, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing vote identifier".into()))? as i64;
            let vc = d.with_snapshot(|s| s.corp_vote_cases.get(&vote_id).cloned()).ok_or_else(|| (STATUS_NOT_FOUND, format!("Vote {vote_id} not found")))?;
            let is_admin = require_admin_role(ctx).is_ok();
            let caller_corp = ctx.effective_corporation_id().map(i64::from).or_else(|| {
                let cid = ctx.effective_character_id()? as i64;
                d.with_snapshot(|s| s.characters.get(&cid).map(|c| c.corporation_id))
            });
            if !is_admin && caller_corp != Some(vc.corporation_id) {
                return Err((STATUS_FORBIDDEN, "Cross-corporation vote access denied".into()));
            }
            let (mut attr, mut out) = (Vec::new(), Vec::new());
            seq_id(1, vc.corporation_id as u64, &mut attr);
            encode_bytes_field(1, &attr, &mut out);
            Ok(any("eve.corporation.vote.GetResponse", out))
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.starbase.GetAllRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let starbases = d.with_snapshot(|s| {
                let mut list: Vec<(i64, u32, u32, u64)> = s.structures.values().filter(|st| st.owner_corp_id == corpid).map(|st| {
                    let flag = match st.state.as_str() { "Unanchoring" => 1, "Offline" => 2, "Anchoring" | "OnliningVulnerable" => 3, "ArmorReinforced" | "HullReinforced" => 4, _ => 5 };
                    (st.structure_id, st.type_id, st.solar_system_id, flag)
                }).collect();
                list.sort_by_key(|(id, _, _, _)| *id);
                list
            });
            let mut out = Vec::new();
            for (sb_id, type_id, sys_id, state_flag) in starbases {
                let (mut sb, mut attr, mut st) = (Vec::new(), Vec::new(), Vec::new());
                seq_id(1, sb_id as u64, &mut sb);
                seq_id(1, type_id as u64, &mut attr);
                seq_id(2, corpid as u64, &mut attr);
                seq_id(3, sys_id as u64, &mut attr);
                encode_bytes_field(2, &attr, &mut sb);
                encode_varint_field(1, state_flag, &mut st);
                encode_varint_field(2, 1, &mut st);
                encode_bytes_field(3, &st, &mut sb);
                encode_bytes_field(1, &sb, &mut out);
            }
            Ok(any("eve.corporation.starbase.GetAllResponse", out))
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.starbase.GetSettingsAndFuelsRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let sb_id = extract_id(msg, 3).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing starbase identifier".into()))? as i64;
            let (cfg, fuel_blocks) = d.with_snapshot(|s| {
                let st_opt = s.structures.get(&sb_id);
                if let Some(st) = st_opt {
                    if st.owner_corp_id != corpid && require_admin_role(ctx).is_err() {
                        return Err((STATUS_FORBIDDEN, "Cross-corporation starbase access denied".into()));
                    }
                } else if !s.starbase_settings.contains_key(&sb_id) {
                    return Err((STATUS_NOT_FOUND, format!("Starbase {sb_id} not found")));
                }
                Ok::<_, (u32, String)>((s.starbase_settings.get(&sb_id).cloned().unwrap_or_default(), st_opt.map(|st| st.fuel_blocks).unwrap_or(0)))
            })?;
            let (mut roles, mut set_msg, mut fuels_msg, mut out) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for (f, r) in [(1, &cfg.fuel_bay_view), (2, &cfg.fuel_bay_take), (3, &cfg.anchor), (4, &cfg.unanchor), (5, &cfg.online), (6, &cfg.offline)] {
                encode_varint_field(f, starbase_role_enum(r), &mut roles);
            }
            encode_bytes_field(1, &roles, &mut set_msg);
            encode_varint_field(2, u64::from(cfg.use_alliance_standings), &mut set_msg);
            encode_varint_field(3, u64::from(cfg.allow_corporation_members), &mut set_msg);
            encode_varint_field(4, u64::from(cfg.allow_alliance_members), &mut set_msg);
            if let Some(th) = cfg.attack_standing_threshold { encode_bytes_field(5, &encode_standing_value(th), &mut set_msg); } else { encode_varint_field(6, 1, &mut set_msg); }
            if let Some(sec) = cfg.attack_security_status_threshold { encode_bytes_field(7, &encode_standing_value(sec), &mut set_msg); } else { encode_varint_field(8, 1, &mut set_msg); }
            encode_varint_field(9, u64::from(cfg.attack_if_other_security_status_dropping), &mut set_msg);
            encode_varint_field(10, u64::from(cfg.attack_if_at_war), &mut set_msg);
            if fuel_blocks > 0 {
                let mut f_item = Vec::new();
                seq_id(1, 4247, &mut f_item);
                encode_varint_field(2, fuel_blocks as u64, &mut f_item);
                encode_bytes_field(1, &f_item, &mut fuels_msg);
            }
            encode_bytes_field(1, &set_msg, &mut out);
            encode_bytes_field(2, &fuels_msg, &mut out);
            Ok(any("eve.corporation.starbase.GetSettingsAndFuelsResponse", out))
        }));
    }

    {
        let d = Arc::clone(&db);
        let medals_fn: crate::quasar_gateway::QuasarHandlerFn = Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let medals = d.with_snapshot(|s| s.corp_medals.get(&corpid).cloned().unwrap_or_default());
            let mut out = Vec::new();
            for entry in medals {
                let mut m = Vec::new();
                seq_id(1, entry.medal_id as u64, &mut m);
                encode_string_field(2, &entry.title, &mut m);
                encode_string_field(3, &entry.description, &mut m);
                encode_bytes_field(1, &m, &mut out);
            }
            Ok(any("eve.corporation.medals.GetResponse", out))
        });
        router.register_handler("eve.corporation.medals.GetRequest", Arc::clone(&medals_fn));
        router.register_handler("eve.corporation.medals.medals.GetRequest", medals_fn);
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.medals.issued.GetRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let issued = d.with_snapshot(|s| s.corp_issued_medals.get(&corpid).cloned().unwrap_or_default());
            let mut out = Vec::new();
            for entry in issued {
                let mut m = Vec::new();
                seq_id(1, entry.medal_id as u64, &mut m);
                seq_id(2, entry.character_id as u64, &mut m);
                encode_string_field(3, &entry.reason, &mut m);
                encode_bytes_field(1, &m, &mut out);
            }
            Ok(any("eve.corporation.medals.issued.GetResponse", out))
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.market.entry.GetEntriesRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let div_key = match msg.get(1) {
                Some(DynamicValue::Message(m)) => match m.get(2) {
                    Some(DynamicValue::Enum(e)) if *e >= 1000 => (*e - 999) as i64,
                    Some(DynamicValue::Uint32(u)) if *u >= 1000 => (*u - 999) as i64,
                    Some(DynamicValue::Uint64(u)) if *u >= 1000 => (*u - 999) as i64,
                    Some(DynamicValue::Uint32(u)) if *u >= 1 => *u as i64,
                    _ => 1,
                },
                _ => 1,
            };
            let txs = d.with_snapshot(|s| {
                let mut v = s.corp_wallet_transactions.get(&(corpid, div_key)).cloned().unwrap_or_default();
                if v.is_empty() && div_key == 1 {
                    v = s.corp_wallet_transactions.get(&(corpid, 1000)).cloned().unwrap_or_default();
                }
                v
            });
            let mut out = Vec::new();
            for tx in txs {
                let (mut e_msg, mut id_msg, mut at_msg, mut ts_msg) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
                encode_varint_field(2, tx.transaction_id as u64, &mut id_msg);
                encode_bytes_field(1, &id_msg, &mut e_msg);
                encode_varint_field(1, tx.date_unix, &mut ts_msg);
                encode_bytes_field(1, &ts_msg, &mut at_msg);
                seq_id(2, 2, &mut at_msg);
                encode_bytes_field(3, &encode_isk_currency((tx.unit_price * 100.0).round() as i64), &mut at_msg);
                encode_varint_field(4, tx.quantity.max(0) as u64, &mut at_msg);
                seq_id(5, tx.location_id as u64, &mut at_msg);
                seq_id(6, tx.client_id as u64, &mut at_msg);
                seq_id(10, tx.journal_ref_id.max(1) as u64, &mut at_msg);
                seq_id(14, tx.type_id as u64, &mut at_msg);
                encode_bytes_field(2, &at_msg, &mut e_msg);
                encode_varint_field(3, if tx.is_buy { 1 } else { 2 }, &mut e_msg);
                encode_bytes_field(1, &e_msg, &mut out);
            }
            Ok(any("eve.corporation.market.entry.GetEntriesResponse", out))
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.standing.npc.GetAllRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let standings = d.with_snapshot(|s| {
                let mut map: BTreeMap<i64, f64> = BTreeMap::new();
                if let Some(txs) = s.standing_transactions.get(&corpid) {
                    for tx in txs {
                        let npc_id = if tx.to_id == corpid { tx.from_id } else { tx.to_id };
                        if npc_id > 0 && npc_id < 4_000_000 { *map.entry(npc_id).or_insert(0.0) += tx.modification; }
                    }
                }
                if let Some(contacts) = s.corp_contacts.get(&corpid) {
                    for c in contacts {
                        if c.contact_id > 0 && c.contact_id < 4_000_000 { map.insert(c.contact_id, c.standing); }
                    }
                }
                map
            });
            let mut out = Vec::new();
            for (npc_id, val) in standings {
                let mut row = Vec::new();
                if (500_000..600_000).contains(&npc_id) { seq_id(1, npc_id as u64, &mut row); }
                else if (1_000_000..2_000_000).contains(&npc_id) { seq_id(2, npc_id as u64, &mut row); }
                else if (3_000_000..4_000_000).contains(&npc_id) { seq_id(3, npc_id as u64, &mut row); }
                else { encode_varint_field(4, npc_id as u64, &mut row); }
                encode_bytes_field(5, &encode_standing_value(val.clamp(-10.0, 10.0)), &mut row);
                encode_bytes_field(1, &row, &mut out);
            }
            Ok(any("eve.corporation.standing.npc.GetAllResponse", out))
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.planetinteraction.office.GetRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let offices = d.with_snapshot(|s| {
                s.corp_customs_offices
                    .get(&corpid)
                    .map(|list| {
                        list.iter()
                            .map(|off| {
                                let office_type_id = s
                                    .items
                                    .get(&off.office_id)
                                    .map(|i| i.type_id as u64)
                                    .or_else(|| s.structures.get(&off.office_id).map(|st| st.type_id as u64));
                                (off.clone(), office_type_id)
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            });
            let mut out = Vec::new();
            for (off, office_type_id) in offices {
                let (mut o_msg, mut at_msg, mut rates) = (Vec::new(), Vec::new(), Vec::new());
                seq_id(1, off.office_id as u64, &mut o_msg);
                seq_id(1, off.system_id as u64, &mut at_msg);
                if let Some(ct) = off.corporation_tax_rate { encode_f32_field(3, ct as f32, &mut at_msg); } else { encode_varint_field(2, 1, &mut at_msg); }
                if off.allow_alliance_access { encode_f32_field(5, off.alliance_tax_rate.unwrap_or(0.0) as f32, &mut at_msg); } else { encode_varint_field(4, 1, &mut at_msg); }
                if off.allow_access_with_standings {
                    for (f, r) in [(2, off.excellent_standing_tax_rate), (3, off.good_standing_tax_rate), (4, off.neutral_standing_tax_rate), (5, off.bad_standing_tax_rate), (6, off.terrible_standing_tax_rate)] {
                        encode_f32_field(f, r.unwrap_or(0.0) as f32, &mut rates);
                    }
                    encode_bytes_field(7, &rates, &mut at_msg);
                } else {
                    encode_varint_field(6, 1, &mut at_msg);
                }
                if let Some(tid) = office_type_id {
                    seq_id(9, tid, &mut at_msg);
                }
                encode_bytes_field(2, &at_msg, &mut o_msg);
                encode_bytes_field(1, &o_msg, &mut out);
            }
            Ok(any("eve.corporation.planetinteraction.office.GetResponse", out))
        }));
    }
}
