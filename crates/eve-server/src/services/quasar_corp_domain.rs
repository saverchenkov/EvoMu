//! `quasar_corp_domain` — State-backed Quasar alliance & corporation governance, accounting,
//! divisions, home station, ALSC logs, industry, members, titles, votes, starbases, medals,
//! market entries, NPC standings, and planetary customs offices.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    require_admin_role, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use eve_db::{CorporationRecord, WorldRepository};
use eve_proto::pb::{self, ProtoBuilder};
use eve_proto::quasar::{DynamicQuasarMessage, DynamicValue, GoogleTimestamp};
use std::collections::BTreeMap;
use std::sync::Arc;

fn build_isk_currency(cents: i64) -> ProtoBuilder {
    let units = cents / 100;
    let nanos = ((cents % 100) * 10_000_000) as i32;
    pb::message("eve.isk.Currency")
        .set("units", units)
        .set("nanos", nanos)
}

fn build_standing_value(standing: f64) -> ProtoBuilder {
    let units = standing.trunc() as i64;
    let nanos = ((standing - standing.trunc()) * 1_000_000_000.0).round() as i32;
    pb::message("eve.standing.Value")
        .set("units", units)
        .set("nanos", nanos)
}

fn build_security_status_value(sec: f64) -> ProtoBuilder {
    let units = sec.trunc() as i64;
    let nanos = ((sec - sec.trunc()) * 1_000_000_000.0).round() as i32;
    pb::message("eve.securitystatus.Value")
        .set("units", units)
        .set("nanos", nanos)
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

#[path = "quasar_corp_domain_auth.rs"]
mod quasar_corp_domain_auth;
pub(crate) use quasar_corp_domain_auth::{
    check_corp_admin_role, live_caller_corp, require_corp_roles, resolve_authorized_corp,
};

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

fn build_role_collection(mask: u64) -> ProtoBuilder {
    pb::message("eve.corporation.role.Collection").set("mask", mask)
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

fn wallet_key_id_variant(division_id: u8) -> &'static str {
    match division_id {
        1 => "Division0",
        2 => "Division1",
        3 => "Division2",
        4 => "Division3",
        5 => "Division4",
        6 => "Division5",
        7 => "Division6",
        _ => "Division0",
    }
}

fn division_identifier_variant(division_id: u8) -> &'static str {
    match division_id {
        1 => "DIVISION_1",
        2 => "DIVISION_2",
        3 => "DIVISION_3",
        4 => "DIVISION_4",
        5 => "DIVISION_5",
        6 => "DIVISION_6",
        7 => "DIVISION_7",
        _ => "DIVISION_1",
    }
}

fn structure_state_flag_variant(state: &str) -> &'static str {
    match state {
        "Unanchored" => "STATE_UNANCHORED",
        "Anchoring" => "STATE_ANCHORING",
        "Onlining" | "OnliningVulnerable" => "STATE_ONLINING_VULNERABLE",
        "ShieldVulnerable" => "STATE_SHIELD_VULNERABLE",
        "ArmorReinforced" => "STATE_ARMOR_REINFORCE",
        "ArmorVulnerable" => "STATE_ARMOR_VULNERABLE",
        "HullReinforced" => "STATE_HULL_REINFORCE",
        "HullVulnerable" => "STATE_HULL_VULNERABLE",
        "NoCore" | "FittingInvulnerable" => "STATE_FITTING_INVULNERABLE",
        "AnchorVulnerable" => "STATE_ANCHOR_VULNERABLE",
        "DeployVulnerable" => "STATE_DEPLOY_VULNERABLE",
        "Online" | "OnlineDeprecated" => "STATE_ONLINE_DEPRECATED",
        _ => "STATE_UNKNOWN",
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
                Ok(pb::message("eve.alliance.GetResponse")
                    .set("alliance.name", name)
                    .set("alliance.short_name", short_name)
                    .set("alliance.date_founded", GoogleTimestamp::from_unix(founded as i64, 0))
                    .set("alliance.creator_character_id.sequential", creator_char)
                    .set("alliance.creator_corporation_id.sequential", creator_corp)
                    .set("alliance.executor_corporation_id.sequential", exec_corp)
                    .must_to_any())
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
                let mut resp = pb::message("eve.alliance.GetAllResponse");
                for id in ids {
                    resp = resp.add_with("alliance_ids", |b| b.set("sequential", id));
                }
                Ok(resp.must_to_any())
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
                    Some(DynamicValue::Message(sub)) => match sub.get(1) {
                        Some(DynamicValue::String(s)) => s.to_ascii_lowercase(),
                        Some(DynamicValue::Bytes(b)) => String::from_utf8_lossy(b).to_ascii_lowercase(),
                        _ => String::new(),
                    },
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
                let mut resp = pb::message("eve.alliance.SearchResponse");
                for id in ids {
                    resp = resp.add_with("alliances", |b| b.set("sequential", id));
                }
                Ok(resp.must_to_any())
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
                let mut resp = pb::message("eve.alliance.corporation.GetMembersResponse");
                for cid in corp_ids {
                    resp = resp.add_with("corporation_ids", |b| b.set("sequential", cid as u64));
                }
                Ok(resp.must_to_any())
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
                require_corp_roles(
                    ctx,
                    &d,
                    corpid,
                    eve_db::corp_roles::ROLE_ACCOUNTANT
                        | eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT
                        | (0x7Fu64 << 27),
                )?;
                let divs = d.with_snapshot(|s| {
                    let c = s.corporations.get(&corpid).ok_or_else(|| {
                        (STATUS_NOT_FOUND, format!("Corporation {corpid} not found"))
                    })?;
                    let w = if c.wallet_divisions.is_empty() {
                        CorporationRecord::default_wallet_divisions(c.master_wallet_balance())
                    } else {
                        c.wallet_divisions
                            .iter()
                            .map(|div| eve_db::types::CorpWalletDivisionRecord {
                                division_id: div.division_id,
                                name: div.name.clone(),
                                balance: c.division_balance(div.division_id),
                            })
                            .collect()
                    };
                    Ok::<_, (u32, String)>(w)
                })?;
                let mut resp = pb::message("eve.corporation.accounting.wallet.GetAllDivisionBalanceResponse");
                for div in divs {
                    resp = resp.add_with("divisions", |b| {
                        b.set("division", pb::enum_name(wallet_key_id_variant(div.division_id)))
                            .set("balance", build_isk_currency(div.balance.0))
                    });
                }
                Ok(resp.must_to_any())
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
                        CorporationRecord::default_wallet_divisions(c.master_wallet_balance())
                    } else {
                        c.wallet_divisions
                            .iter()
                            .map(|div| eve_db::types::CorpWalletDivisionRecord {
                                division_id: div.division_id,
                                name: div.name.clone(),
                                balance: c.division_balance(div.division_id),
                            })
                            .collect()
                    };
                    Ok::<_, (u32, String)>((h, w))
                })?;
                let mut resp = pb::message("eve.corporation.divisions.GetResponse");
                for idx in 1..=7u8 {
                    let custom = hangar_map.get(&idx).filter(|s| !s.is_empty()).cloned();
                    resp = resp.add_with("hangar", |b| {
                        let b = b.set("id", pb::enum_name(division_identifier_variant(idx)));
                        match custom {
                            Some(name) => b.set("custom_name", name),
                            None => b.set("default_name", true),
                        }
                    });
                }
                for w in wallet_divs {
                    resp = resp.add_with("wallet", |b| {
                        let b = b.set("id", pb::enum_name(division_identifier_variant(w.division_id)));
                        if !w.name.is_empty() {
                            b.set("custom_name", w.name)
                        } else {
                            b.set("default_name", true)
                        }
                    });
                }
                Ok(resp.must_to_any())
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
                Ok(pb::message("eve.corporation.homestation.GetResponse")
                    .set("station.sequential", st_id as u64)
                    .must_to_any())
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
                let resp = pb::message("eve.corporation.alliance.api.GetCurrentMembershipResponse");
                let resp = if let Some(a) = aid.filter(|&x| x > 0) {
                    resp.set("alliance.sequential", a)
                } else {
                    resp.set("no_alliance", true)
                };
                Ok(resp.must_to_any())
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
                let mut resp = pb::message("eve.corporation.alliancemembershiprecord.GetAllResponse");
                for rec in history {
                    resp = resp.add_with("records", |b| {
                        let b = b
                            .set("id.sequential", rec.record_id as u32)
                            .set(
                                "attributes.timestamp",
                                GoogleTimestamp::from_unix(rec.start_date_unix as i64, 0),
                            );
                        if let Some(aid) = rec.alliance_id.filter(|&a| a > 0) {
                            b.set("attributes.alliance.id.sequential", aid)
                                .set("attributes.alliance.is_deleted", rec.is_deleted.unwrap_or(false))
                        } else {
                            b.set("attributes.unaffiliated", true)
                        }
                    });
                }
                Ok(resp.must_to_any())
            }),
        );
    }

    {
        let d = Arc::clone(&db);
        router.register_handler(
            "eve.corporation.alsclogs.GetRequest",
            Arc::new(move |ctx, _any, msg| {
                let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
                require_corp_roles(
                    ctx,
                    &d,
                    corpid,
                    eve_db::corp_roles::ROLE_CONFIG_EQUIPMENT | eve_db::corp_roles::ROLE_AUDITOR,
                )?;
                let logs = d.with_snapshot(|s| {
                    s.corp_container_logs
                        .get(&corpid)
                        .cloned()
                        .unwrap_or_default()
                });
                let mut resp = pb::message("eve.corporation.alsclogs.GetResponse");
                for entry in logs {
                    resp = resp.add_with("entries", |b| {
                        let b = b
                            .set("timestamp", GoogleTimestamp::from_unix(entry.logged_at_unix as i64, 0))
                            .set("container.sequential", entry.container_id as u64)
                            .set("container_type.sequential", entry.container_type_id as u64)
                            .set("container_location.sequential", entry.location_id as u64)
                            .set("container_location_flag", parse_location_flag(&entry.location_flag) as u32)
                            .set("character.sequential", entry.character_id as u32);
                        let act_lower = entry.action.to_ascii_lowercase();
                        let set_type_qty = |sub: ProtoBuilder| -> ProtoBuilder {
                            sub.set_opt("type.sequential", entry.type_id.map(|tid| tid as u64))
                                .set_opt("quantity", entry.quantity.map(|q| q.max(0) as u64))
                        };
                        match act_lower.as_str() {
                            "assemble" => b.set_with("action.assemble", |sub| {
                                sub.set_opt("type.sequential", entry.type_id.map(|tid| tid as u64))
                            }),
                            "repackage" => b.set_with("action.repackage", |sub| sub),
                            "set_name" | "setname" => b.set_with("action.set_name", |sub| sub),
                            "move" => b.set_with("action.move", |sub| sub),
                            "set_password" | "setpassword" => {
                                b.set("action.set_password.password_type", pb::enum_name("PASSWORD_TYPE_GENERAL"))
                            }
                            "add" => b.set_with("action.add", set_type_qty),
                            "lock" => b.set_with("action.lock", set_type_qty),
                            "unlock" => b.set_with("action.unlock", set_type_qty),
                            "enter_password" | "enterpassword" => {
                                b.set("action.enter_password.password_type", pb::enum_name("PASSWORD_TYPE_GENERAL"))
                            }
                            "configure" => b
                                .set(
                                    "action.configure.old_config_bitmask",
                                    entry.old_config_bitmask.unwrap_or(0) as u64,
                                )
                                .set(
                                    "action.configure.new_config_bitmask",
                                    entry.new_config_bitmask.unwrap_or(0) as u64,
                                ),
                            _ => {
                                let field = if entry.type_id.is_some() {
                                    "action.add"
                                } else {
                                    "action.lock"
                                };
                                b.set_with(field, set_type_qty)
                            }
                        }
                    });
                }
                Ok(resp.must_to_any())
            }),
        );
    }

    {
        let d = Arc::clone(&db);
        router.register_handler(
            "eve.corporation.industry.job.GetActiveRequest",
            Arc::new(move |ctx, _any, msg| {
                let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
                require_corp_roles(
                    ctx,
                    &d,
                    corpid,
                    eve_db::corp_roles::ROLE_FACTORY_MANAGER
                        | eve_db::corp_roles::ROLE_RENT_FACTORY_FACILITY
                        | eve_db::corp_roles::ROLE_RENT_RESEARCH_FACILITY,
                )?;
                let jobs = d.with_snapshot(|s| {
                    let mut v: Vec<_> = s
                        .industry_jobs
                        .values()
                        .filter(|j| {
                            matches!(j.status.as_str(), "Active" | "Ready")
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
                let mut resp = pb::message("eve.corporation.industry.job.GetActiveResponse");
                for j in jobs {
                    let details_field = match j.activity_id {
                        3 => "research_duration",
                        4 => "research_materials",
                        5 => "blueprint_copy",
                        8 => "blueprint_invention",
                        9 | 11 => "reaction",
                        _ => "manufacturing",
                    };
                    resp = resp.add_with("jobs", |b| {
                        b.set("id.sequential", j.job_id as u32)
                            .set_with(details_field, |sub| {
                                let sub = sub
                                    .set("corporation.sequential", corpid as u32)
                                    .set("installer.sequential", j.installer_id as u32)
                                    .set("facility.sequential", j.facility_id as u64)
                                    .set("runs", j.runs as u32)
                                    .set("duration", j.duration_seconds as u32);
                                let bp_item = j.blueprint_item_id.max(0) as u64;
                                match j.activity_id {
                                    3 | 4 => sub
                                        .set("blueprint_original.sequential", bp_item)
                                        .set("blueprint_type.sequential", j.blueprint_type_id as u64),
                                    5 => sub
                                        .set("blueprint_original.sequential", bp_item)
                                        .set("product.sequential", j.product_type_id as u64)
                                        .set("blueprint_type.sequential", j.blueprint_type_id as u64),
                                    8 => sub
                                        .set("blueprint_copy.sequential", bp_item)
                                        .set("product.sequential", j.product_type_id as u64)
                                        .set("blueprint_type.sequential", j.blueprint_type_id as u64),
                                    _ => sub
                                        .set("blueprint_original.sequential", bp_item)
                                        .set("product.sequential", j.product_type_id as u64)
                                        .set("blueprint_type.sequential", j.blueprint_type_id as u64),
                                }
                            })
                    });
                }
                Ok(resp.must_to_any())
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
                require_corp_roles(
                    ctx,
                    &d,
                    corpid,
                    eve_db::corp_roles::ROLE_FACTORY_MANAGER
                        | eve_db::corp_roles::ROLE_RENT_FACTORY_FACILITY
                        | eve_db::corp_roles::ROLE_RENT_RESEARCH_FACILITY,
                )?;
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
                let mut resp = pb::message("eve.corporation.industry.blueprints.GetResponse");
                for item in items {
                    let me = item.dynamic_attributes.get(&1112).copied().unwrap_or(0.0).max(0.0) as u32;
                    let te = item.dynamic_attributes.get(&1113).copied().unwrap_or(0.0).max(0.0) as u32;
                    let loc = pb::message("eve.industry.blueprintlocation.Identifier")
                        .set("station_id.sequential", item.location_id as u64)
                        .set("location_flag", item.flag as u32);
                    if !item.singleton && item.quantity > 1 && !item.is_blueprint_copy {
                        resp = resp.add_with("blueprints_stacks", |b| {
                            b.set("item_id.sequential", item.item_id as u64)
                                .set("blueprint_type.sequential", item.type_id as u64)
                                .set("location", loc)
                                .set("quantity", item.quantity as u32)
                        });
                    } else if item.is_blueprint_copy {
                        resp = resp.add_with("blueprint_copies", |b| {
                            b.set("blueprint_id.sequential", item.item_id as u64)
                                .set("attributes.type.sequential", item.type_id as u64)
                                .set("attributes.material_efficiency", me)
                                .set("attributes.time_efficiency", te)
                                .set("attributes.runs_remaining", item.blueprint_runs_remaining.unwrap_or(1))
                                .set("location", loc)
                        });
                    } else {
                        resp = resp.add_with("blueprints", |b| {
                            b.set("blueprint_id.sequential", item.item_id as u64)
                                .set("attributes.type.sequential", item.type_id as u64)
                                .set("attributes.material_efficiency", me)
                                .set("attributes.time_efficiency", te)
                                .set("location", loc)
                        });
                    }
                }
                Ok(resp.must_to_any())
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
                let mut resp = pb::message("eve.corporation.industry.facilties.GetResponse");
                for st in structs {
                    resp = resp.add_with("facilities", |b| {
                        b.set("id.sequential", st.structure_id as u64)
                            .set("attributes.corporation.sequential", corpid as u32)
                            .set("attributes.solar_system.sequential", st.solar_system_id as u64)
                            .set("attributes.type.sequential", st.type_id)
                            .set("attributes.online", true)
                    });
                }
                Ok(resp.must_to_any())
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
                let now = ctx.now_unix() as i64;
                let mut resp = pb::message("eve.corporation.industry.observer.GetAllDetailsResponse");
                for oid in obs_ids {
                    resp = resp.add_with("observers", |b| {
                        b.set("id.sequential", oid as u64)
                            .set("attrs.last_updated", GoogleTimestamp::from_unix(now, 0))
                    });
                }
                Ok(resp.must_to_any())
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
            let mut resp = pb::message("eve.corporation.members.GetResponse");
            for cid in members {
                resp = resp.add_with("members", |b| b.set("sequential", cid as u32));
            }
            Ok(resp.must_to_any())
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.members.GetLimitRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let limit = d.with_snapshot(|s| {
                crate::domain::corp_governance::resolve_corp_member_limit(None, s, corpid).unwrap_or(10)
            });
            Ok(pb::message("eve.corporation.members.GetLimitResponse")
                .set("member_limit", limit.max(0) as u32)
                .must_to_any())
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
                let has_corp = s.corporations.contains_key(&corpid);
                members.into_iter().map(|cid| {
                    let mask = if has_corp {
                        s.effective_corp_roles_at(corpid, cid, None, 0, 0).1
                    } else {
                        0
                    };
                    (cid, mask)
                }).collect::<Vec<_>>()
            });
            let mut resp = pb::message("eve.corporation.members.roles.GetResponse");
            for (cid, mask) in member_roles {
                resp = resp.add_with("members_roles", |b| {
                    b.set("character.sequential", cid as u32)
                        .set("roles_at_all", build_role_collection(mask))
                });
            }
            Ok(resp.must_to_any())
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
            let mut resp = pb::message("eve.corporation.members.titles.GetResponse");
            for (cid, tids) in rows {
                resp = resp.add_with("members", |mut b| {
                    b = b.set("character.sequential", cid as u32);
                    for tid in tids {
                        b = b.add_with("titles", |t| t.set("sequential", tid as u32));
                    }
                    b
                });
            }
            Ok(resp.must_to_any())
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
                    let ch = s.characters.get(&cid);
                    let station_id = ch.and_then(|c| c.station_id);
                    let logoff = s.character_login_stats.get(&cid).map(|st| st.last_logout_unix).unwrap_or(0);
                    let logon = s.character_login_stats.get(&cid).map(|st| st.last_login_unix).unwrap_or(0);
                    (cid, station_id, logon, logoff)
                }).collect::<Vec<_>>()
            });
            let mut resp = pb::message("eve.corporation.members.tracking.GetResponse");
            for (cid, station_id, logon, logoff) in rows {
                resp = resp.add_with("members_tracking_data", |b| {
                    let b = b
                        .set("character.sequential", cid as u32)
                        .set("start_date", GoogleTimestamp::from_unix(1_700_000_000, 0))
                        .set("no_base", true);
                    let b = if logon > 0 {
                        b.set("last_logon_datetime", GoogleTimestamp::from_unix(logon as i64, 0))
                    } else {
                        b.set("no_last_logon_datetime", true)
                    };
                    let b = if logoff > 0 {
                        b.set("last_logoff_datetime", GoogleTimestamp::from_unix(logoff as i64, 0))
                    } else {
                        b.set("no_last_logoff_datetime", true)
                    };
                    if let Some(st) = station_id.filter(|&v| v > 0) {
                        b.set("station.sequential", st as u64)
                    } else {
                        b
                    }
                });
            }
            Ok(resp.must_to_any())
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.members.roles.auditlog.GetRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            require_corp_roles(
                ctx,
                &d,
                corpid,
                eve_db::corp_roles::ROLE_PERSONNEL_MANAGER | eve_db::corp_roles::ROLE_AUDITOR,
            )?;
            let history = d.with_snapshot(|s| s.corp_role_history.get(&corpid).cloned().unwrap_or_default());
            let mut resp = pb::message("eve.corporation.members.roles.auditlog.GetResponse");
            for entry in history {
                let rtype = match entry.role_type.as_str() {
                    "grantable_roles" => "TYPE_GRANTABLE_AT_ALL",
                    "roles_at_hq" => "TYPE_AT_HQ",
                    "grantable_roles_at_hq" => "TYPE_GRANTABLE_AT_HQ",
                    "roles_at_base" => "TYPE_AT_BASE",
                    "grantable_roles_at_base" => "TYPE_GRANTABLE_AT_BASE",
                    "roles_at_other" => "TYPE_AT_OTHER",
                    "grantable_roles_at_other" => "TYPE_GRANTABLE_AT_OTHER",
                    _ => "TYPE_AT_ALL",
                };
                resp = resp.add_with("roles_changes", |b| {
                    b.set("timestamp", GoogleTimestamp::from_unix(entry.changed_at_unix as i64, 0))
                        .set("character.sequential", entry.character_id as u32)
                        .set("issuer.sequential", entry.issuer_id as u32)
                        .set("role_type", pb::enum_name(rtype))
                        .set("old_roles", build_role_collection(role_names_to_mask(&entry.old_roles)))
                        .set("new_roles", build_role_collection(role_names_to_mask(&entry.new_roles)))
                        .set("known_issuer.sequential", entry.issuer_id as u32)
                });
            }
            Ok(resp.must_to_any())
        }));
    }

    {
        let sde_npc = Arc::clone(&sde);
        router.register_handler("eve.corporation.npc.GetAllRequest", Arc::new(move |_ctx, _any, _msg| {
            let mut npc_corps: Vec<u32> = sde_npc.stations.values().map(|st| st.owner_corporation_id).filter(|&cid| cid > 0 && cid < 2_000_000).map(|cid| cid as u32).collect();
            npc_corps.sort_unstable();
            npc_corps.dedup();
            let mut resp = pb::message("eve.corporation.npc.GetAllResponse");
            for cid in npc_corps {
                resp = resp.add_with("corporations", |b| b.set("sequential", cid));
            }
            Ok(resp.must_to_any())
        }));
    }

    router.register_handler("eve.corporation.npc.IsNPCCorporationRequest", Arc::new(move |_ctx, _any, msg| {
        let corpid = extract_id(msg, 1).unwrap_or(0);
        Ok(pb::message("eve.corporation.npc.IsNPCCorporationResponse")
            .set("is_npc", corpid > 0 && corpid < 2_000_000)
            .must_to_any())
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
            Ok(pb::message("eve.corporation.logo.api.GetResponse")
                .set_with("logo", |mut l| {
                    for (shape, color) in [(s1, c1), (s2, c2), (s3, c3)] {
                        if shape > 0 || color > 0 {
                            l = l.add_with("layers", |layer| {
                                layer
                                    .set("shape", shape.max(0) as u32)
                                    .set("color", color.max(0) as u32)
                            });
                        }
                    }
                    l
                })
                .must_to_any())
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
            let mut resp = pb::message("eve.corporation.office.api.GetSolarSystemsResponse");
            for sys_id in systems {
                resp = resp.add_with("solar_systems", |b| b.set("sequential", sys_id as u64));
            }
            Ok(resp.must_to_any())
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.structure.GetAllWithDetailsRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let structures = d.with_snapshot(|s| {
                let mut v: Vec<(i64, String, u32, u32, String)> = s
                    .structures
                    .values()
                    .filter(|st| st.owner_corp_id == corpid)
                    .map(|st| (st.structure_id, st.name.clone(), st.solar_system_id, st.type_id, st.state.clone()))
                    .collect();
                v.sort_by_key(|(id, _, _, _, _)| *id);
                v
            });
            let mut resp = pb::message("eve.corporation.structure.GetAllWithDetailsResponse");
            for (sid, name, sys_id, type_id, state) in structures {
                resp = resp.add_with("structures", |b| {
                    b.set("id.sequential", sid as u64)
                        .set("attributes.type.sequential", type_id as u64)
                        .set("attributes.solarsystem_id.sequential", sys_id as u64)
                        .set("attributes.corporation_id.sequential", corpid as u32)
                        .set("attributes.name", name)
                        .set("state.flag", pb::enum_name(structure_state_flag_variant(&state)))
                });
            }
            Ok(resp.must_to_any())
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
            let mut resp = pb::message("eve.corporation.title.GetAllResponse");
            for t in titles {
                resp = resp.add_with("titles", |mut b| {
                    b = b
                        .set("id.sequential", t.title_id as u32)
                        .set("attributes.name", t.name);
                    for (field, r) in [
                        ("roles_at_all", &t.roles),
                        ("grantable_roles_at_all", &t.grantable_roles),
                        ("roles_at_base", &t.roles_at_base),
                        ("grantable_roles_at_base", &t.grantable_roles_at_base),
                        ("roles_at_hq", &t.roles_at_hq),
                        ("grantable_roles_at_hq", &t.grantable_roles_at_hq),
                        ("roles_at_other", &t.roles_at_other),
                        ("grantable_roles_at_other", &t.grantable_roles_at_other),
                    ] {
                        b = b.set(field, build_role_collection(role_names_to_mask(r)));
                    }
                    b
                });
            }
            Ok(resp.must_to_any())
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.vote.GetRequest", Arc::new(move |ctx, _any, msg| {
            let vote_id = extract_id(msg, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing vote identifier".into()))? as i64;
            let vc = d.with_snapshot(|s| s.corp_vote_cases.get(&vote_id).cloned()).ok_or_else(|| (STATUS_NOT_FOUND, format!("Vote {vote_id} not found")))?;
            let is_admin = require_admin_role(ctx).is_ok();
            let caller_corp = live_caller_corp(ctx, &d);
            if !is_admin && caller_corp != Some(vc.corporation_id) {
                return Err((STATUS_FORBIDDEN, "Cross-corporation vote access denied".into()));
            }
            Ok(pb::message("eve.corporation.vote.GetResponse")
                .set("vote.corporation_id.sequential", vc.corporation_id as u32)
                .must_to_any())
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
            let mut resp = pb::message("eve.corporation.starbase.GetAllResponse");
            for (sb_id, type_id, sys_id, state_flag) in starbases {
                resp = resp.add_with("starbases", |b| {
                    b.set("id.sequential", sb_id as u64)
                        .set("attributes.type.sequential", type_id as u64)
                        .set("attributes.corporation.sequential", corpid as u32)
                        .set("attributes.solarsystem.sequential", sys_id as u64)
                        .set("state.flag", state_flag as i32)
                        .set("state.no_data", true)
                });
            }
            Ok(resp.must_to_any())
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.starbase.GetSettingsAndFuelsRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            require_corp_roles(
                ctx,
                &d,
                corpid,
                eve_db::corp_roles::ROLE_CONFIG_EQUIPMENT
                    | eve_db::corp_roles::ROLE_STATION_MANAGER,
            )?;
            let sb_id = extract_id(msg, 3).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing starbase identifier".into()))? as i64;
            let (cfg, fuel_blocks) = d.with_snapshot(|s| {
                let st_opt = s.structures.get(&sb_id);
                let owner_corp = st_opt
                    .map(|st| st.owner_corp_id)
                    .or_else(|| s.items.get(&sb_id).map(|it| it.owner_id))
                    .ok_or_else(|| (STATUS_NOT_FOUND, format!("Starbase {sb_id} not found")))?;
                if owner_corp != corpid && require_admin_role(ctx).is_err() {
                    return Err((STATUS_FORBIDDEN, "Cross-corporation starbase access denied".into()));
                }
                Ok::<_, (u32, String)>((s.starbase_settings.get(&sb_id).cloned().unwrap_or_default(), st_opt.map(|st| st.fuel_blocks).unwrap_or(0)))
            })?;
            let mut resp = pb::message("eve.corporation.starbase.GetSettingsAndFuelsResponse")
                .set_with("settings", |mut s| {
                    s = s
                        .set("roles.fuel_bay_view", starbase_role_enum(&cfg.fuel_bay_view) as i32)
                        .set("roles.fuel_bay_take", starbase_role_enum(&cfg.fuel_bay_take) as i32)
                        .set("roles.anchor", starbase_role_enum(&cfg.anchor) as i32)
                        .set("roles.unanchor", starbase_role_enum(&cfg.unanchor) as i32)
                        .set("roles.online", starbase_role_enum(&cfg.online) as i32)
                        .set("roles.offline", starbase_role_enum(&cfg.offline) as i32)
                        .set("use_alliance_standings", cfg.use_alliance_standings)
                        .set("allow_corporation_members", cfg.allow_corporation_members)
                        .set("allow_alliance_members", cfg.allow_alliance_members)
                        .set("attack_on_status_dropping", cfg.attack_if_other_security_status_dropping)
                        .set("attack_if_at_war", cfg.attack_if_at_war);
                    s = if let Some(th) = cfg.attack_standing_threshold {
                        s.set("attack_standing_threshold", build_standing_value(th))
                    } else {
                        s.set("no_attack_standing_threshold", true)
                    };
                    if let Some(sec) = cfg.attack_security_status_threshold {
                        s.set("attack_security_status_threshold", build_security_status_value(sec))
                    } else {
                        s.set("no_attack_security_status_threshold", true)
                    }
                });
            if fuel_blocks > 0 {
                resp = resp.set_with("fuels", |f| {
                    f.add_with("blocks", |blk| {
                        blk.set("type.sequential", 4247u64)
                            .set("quantity", fuel_blocks as u32)
                    })
                });
            }
            Ok(resp.must_to_any())
        }));
    }

    {
        let d = Arc::clone(&db);
        let medals_fn: crate::quasar_gateway::QuasarHandlerFn = Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let medals = d.with_snapshot(|s| s.corp_medals.get(&corpid).cloned().unwrap_or_default());
            let mut resp = pb::message("eve.corporation.medals.GetResponse");
            for entry in medals {
                resp = resp.add_with("medals", |b| {
                    b.set("id.sequential", entry.medal_id as u32)
                        .set("attributes.title", entry.title)
                        .set("attributes.description", entry.description)
                        .set("attributes.corporation.sequential", corpid as u32)
                });
            }
            Ok(resp.must_to_any())
        });
        router.register_handler("eve.corporation.medals.GetRequest", Arc::clone(&medals_fn));
        router.register_handler("eve.corporation.medals.medals.GetRequest", medals_fn);
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.medals.issued.GetRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            let issued = d.with_snapshot(|s| s.corp_issued_medals.get(&corpid).cloned().unwrap_or_default());
            let mut resp = pb::message("eve.corporation.medals.issued.GetResponse");
            for entry in issued {
                resp = resp.add_with("issued_medals", |b| {
                    b.set("medal.sequential", entry.medal_id as u32)
                        .set("character.sequential", entry.character_id as u32)
                        .set("attributes.reason", entry.reason)
                        .set("attributes.status", pb::enum_name("STATUS_PUBLIC"))
                });
            }
            Ok(resp.must_to_any())
        }));
    }

    {
        let d = Arc::clone(&db);
        router.register_handler("eve.corporation.market.entry.GetEntriesRequest", Arc::new(move |ctx, _any, msg| {
            let corpid = resolve_authorized_corp(ctx, msg, 1, &d)?;
            require_corp_roles(
                ctx,
                &d,
                corpid,
                eve_db::corp_roles::ROLE_ACCOUNTANT
                    | eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT
                    | eve_db::corp_roles::ROLE_TRADER
                    | (0x7Fu64 << 27),
            )?;
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
            let mut resp = pb::message("eve.corporation.market.entry.GetEntriesResponse");
            for tx in txs {
                resp = resp.add_with("entries", |b| {
                    b.set("id.sequential", tx.transaction_id as u64)
                        .set("attributes.date", GoogleTimestamp::from_unix(tx.date_unix as i64, 0))
                        .set("attributes.entry_type.sequential", 2u32)
                        .set(
                            "attributes.price",
                            build_isk_currency((tx.unit_price * 100.0).round() as i64),
                        )
                        .set("attributes.quantity", tx.quantity.max(0) as u32)
                        .set("attributes.station.sequential", tx.location_id as u64)
                        .set("attributes.customer_character.sequential", tx.client_id as u32)
                        .set(
                            "attributes.accounting_reference.transaction",
                            tx.journal_ref_id.max(1) as u64,
                        )
                        .set("attributes.item_type.sequential", tx.type_id as u64)
                        .set("type", pb::enum_name(if tx.is_buy { "BUY" } else { "SELL" }))
                });
            }
            Ok(resp.must_to_any())
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
            let mut resp = pb::message("eve.corporation.standing.npc.GetAllResponse");
            for (npc_id, val) in standings {
                resp = resp.add_with("npc_standings", |b| {
                    let b = if (500_000..600_000).contains(&npc_id) {
                        b.set("faction.sequential", npc_id as u64)
                    } else if (1_000_000..2_000_000).contains(&npc_id) {
                        b.set("corporation.sequential", npc_id as u32)
                    } else if (3_000_000..4_000_000).contains(&npc_id) {
                        b.set("agent.sequential", npc_id as u32)
                    } else {
                        b.set("unknown", npc_id as u64)
                    };
                    b.set("standing", build_standing_value(val.clamp(-10.0, 10.0)))
                });
            }
            Ok(resp.must_to_any())
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
            let mut resp = pb::message("eve.corporation.planetinteraction.office.GetResponse");
            for (off, office_type_id) in offices {
                resp = resp.add_with("offices", |b| {
                    let b = b
                        .set("id.sequential", off.office_id as u64)
                        .set("attributes.solarsystem.sequential", off.system_id as u64);
                    let b = if let Some(ct) = off.corporation_tax_rate {
                        b.set("attributes.corporation_tax_rate", ct as f32)
                    } else {
                        b.set("attributes.no_allowed_corporation_access", true)
                    };
                    let b = if off.allow_alliance_access {
                        b.set("attributes.alliance_tax_rate", off.alliance_tax_rate.unwrap_or(0.0) as f32)
                    } else {
                        b.set("attributes.no_allowed_alliance_access", true)
                    };
                    let b = if off.allow_access_with_standings {
                        b.set("attributes.tax_rates.excellent", off.excellent_standing_tax_rate.unwrap_or(0.0) as f32)
                            .set("attributes.tax_rates.good", off.good_standing_tax_rate.unwrap_or(0.0) as f32)
                            .set("attributes.tax_rates.neutral", off.neutral_standing_tax_rate.unwrap_or(0.0) as f32)
                            .set("attributes.tax_rates.bad", off.bad_standing_tax_rate.unwrap_or(0.0) as f32)
                            .set("attributes.tax_rates.terrible", off.terrible_standing_tax_rate.unwrap_or(0.0) as f32)
                    } else {
                        b.set("attributes.no_allowed_access_with_standings", true)
                    };
                    b.set_opt("attributes.office_type.sequential", office_type_id)
                });
            }
            Ok(resp.must_to_any())
        }));
    }
}

#[cfg(test)] #[path = "quasar_corp_domain_tests.rs"]
mod quasar_corp_domain_tests;
