//! `quasar_economy_planetary_industry` — Quasar V24.01 Accounting, Contracts, Industry, Planets, PI, Skyhooks, Localization & Identity (`Q-6`).
//!
//! Implements backend handlers for:
//! - `eve.accounting.*` (`GetCharacterEntriesRequest`, `GetCorporationEntriesRequest`, `GetAllRequest`);
//! - `eve.contract.item_exchange.*` (`GetRequest`);
//! - `eve.industry.*` (`GetRequest` for facilities, `GetCostIndicesRequest`);
//! - `eve.planet.*` (`GetRequest`, `GetPlanetsInSolarSystemRequest`);
//! - `eve.planet.skyhook.*` (`ChangeOwnerRequest`, `GetReinforcementExitHourRequest`, `GetStateRequest`);
//! - `eve.planetinteraction.*` (`collector.GetRequest`, `factory.GetRequest`, `schematic.GetRequest`, `storage.GetRequest`);
//! - `eve.moon.*` (`GetRequest`, `GetMoonsOfPlanetRequest`);
//! - `eve.asteroidbelt.*` (`GetRequest`, `GetBeltsOfPlanetRequest`);
//! - `eve.bloodline.*` (`GetAllRequest`);
//! - `eve.isk.wallet.api.TransferByOwnerRequest`, `eve.localization.*`, `eve.id.*`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
    STATUS_INTERNAL_ERROR, STATUS_NOT_FOUND, STATUS_UNAUTHORIZED,
};
use crate::services::industry_pi::{ATTR_BLUEPRINT_ME, ATTR_BLUEPRINT_TE};
use eve_db::ledger::{ref_types, JournalEntryInput, JournalMeta};
use eve_db::{IskCents, WorldRepository};
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, QuasarDescriptorRegistry,
};
use eve_sde::{CelestialInfo, SdeRegistry};
use std::collections::BTreeMap;
use std::sync::Arc;

fn caller_station_and_solar_system(
    ctx: &AuthoritativeContext,
    db: &WorldRepository,
) -> (u64, u64) {
    ctx.effective_character_id()
        .and_then(|cid| {
            db.with_snapshot(|s| {
                s.characters.get(&(cid as i64)).map(|c| {
                    (
                        c.station_id.or(c.structure_id).unwrap_or(0) as u64,
                        c.solar_system_id as u64,
                    )
                })
            })
        })
        .unwrap_or((0, 0))
}

fn set_isk_currency(b: pb::ProtoBuilder, isk_cents: i64) -> pb::ProtoBuilder {
    let clamped = isk_cents.max(0);
    b.set("units", clamped / 100)
        .set("nanos", ((clamped % 100) * 10_000_000) as i32)
}

fn resolve_industry_target_char(
    ctx: &AuthoritativeContext,
    requested_cid: i64,
) -> Result<i64, (u32, String)> {
    let caller_cid = ctx.effective_character_id().unwrap_or(0) as i64;
    let is_admin = require_admin_role(ctx).is_ok();
    let target = if requested_cid > 0 { requested_cid } else { caller_cid };
    if target <= 0 {
        return Err((STATUS_UNAUTHORIZED, "Active character identity required".to_string()));
    }
    if caller_cid != target && !is_admin {
        return Err((
            STATUS_FORBIDDEN,
            format!("Character {caller_cid} is not authorized to read character {target} industry data"),
        ));
    }
    Ok(target)
}

fn industry_job_status_code(status: &str) -> i32 {
    if status.eq_ignore_ascii_case("Active") {
        1
    } else if status.eq_ignore_ascii_case("Paused") {
        2
    } else if status.eq_ignore_ascii_case("Ready") {
        3
    } else if status.eq_ignore_ascii_case("Delivered") || status.eq_ignore_ascii_case("Completed") {
        101
    } else if status.eq_ignore_ascii_case("Cancelled") {
        102
    } else if status.eq_ignore_ascii_case("Reverted") {
        103
    } else {
        1
    }
}

fn encode_industry_job_entry(entry: pb::ProtoBuilder, job: &eve_db::IndustryJobRecord) -> pb::ProtoBuilder {
    let oneof_field = match job.activity_id {
        3 => "research_duration",
        4 => "research_materials",
        5 => "blueprint_copy",
        8 => "blueprint_invention",
        9 | 11 => "reaction",
        _ => "manufacturing",
    };
    let status_code = industry_job_status_code(&job.status);
    let end_unix = job.start_unix.saturating_add(job.duration_seconds);
    let duration_u32 = job.duration_seconds.min(u32::MAX as u64) as u32;
    let is_invention = job.activity_id == 8;
    let has_product = matches!(job.activity_id, 1 | 8 | 9 | 11);
    entry
        .set("id.sequential", job.job_id as u32)
        .set_with(oneof_field, |mut attrs| {
            attrs = attrs
                .set("character.sequential", job.installer_id as u32)
                .set("installer.sequential", job.installer_id as u32)
                .set("facility.sequential", job.facility_id as u64)
                .set("station.sequential", job.facility_id as u64)
                .set("status", status_code)
                .set("runs", job.runs)
                .set("start_date.seconds", job.start_unix as i64)
                .set("end_date.seconds", end_unix as i64)
                .set("duration", duration_u32)
                .set("blueprint_type.sequential", job.blueprint_type_id);
            if let Some(cost_isk) = job.cost {
                let cents = (cost_isk * 100.0).round() as i64;
                attrs = attrs.set_with("cost", |c| set_isk_currency(c, cents));
            }
            if is_invention {
                attrs = attrs.set("blueprint_copy.sequential", job.blueprint_item_id as u64);
            } else {
                attrs = attrs.set("blueprint_original.sequential", job.blueprint_item_id as u64);
            }
            if has_product && job.product_type_id > 0 {
                attrs = attrs.set("product.sequential", job.product_type_id);
            }
            attrs
        })
}

fn encode_blueprint_location(
    loc: pb::ProtoBuilder,
    location_id: i64,
    sde: &SdeRegistry,
    snap: &eve_db::WorldStateSnapshot,
) -> pb::ProtoBuilder {
    if sde.stations.contains_key(&location_id) || !snap.structures.contains_key(&location_id) {
        loc.set("station_id.sequential", location_id as u64)
    } else {
        loc.set("item_id.sequential", location_id as u64)
    }
}

fn encode_celestial_attributes(
    attrs: pb::ProtoBuilder,
    sys_field: &str,
    name: &str,
    sys_id: u64,
    pos: Option<[f64; 3]>,
) -> pb::ProtoBuilder {
    let mut b = attrs.set("name", name).set(sys_field, sys_id);
    if let Some([x, y, z]) = pos {
        b = b.set_with("position", |p| p.set("x", x).set("y", y).set("z", z));
    }
    b
}

fn lookup_sde_celestial_with_group<'a>(
    sde: &'a SdeRegistry,
    celestial_id: i64,
    expected_group_id: u32,
    kind_label: &str,
) -> Result<Option<&'a CelestialInfo>, (u32, String)> {
    match sde.celestial(celestial_id) {
        Some(c) if c.group_id == expected_group_id => Ok(Some(c)),
        Some(_) => Err((
            STATUS_NOT_FOUND,
            format!("{kind_label} {celestial_id} not found"),
        )),
        None => Ok(None),
    }
}

fn children_of_planet_in_sde(
    sde: &SdeRegistry,
    planet_id: i64,
    child_group_id: u32,
) -> Result<Option<Vec<i64>>, (u32, String)> {
    let Some(planet) = lookup_sde_celestial_with_group(sde, planet_id, 7, "Planet")? else {
        return Ok(None);
    };
    let mut children: Vec<&CelestialInfo> = sde
        .celestials_in_system_slice(planet.solar_system_id)
        .iter()
        .filter(|c| c.group_id == child_group_id && c.orbit_id == Some(planet_id))
        .collect();
    children.sort_by_key(|c| (c.orbit_index.unwrap_or(i32::MAX), c.celestial_id));
    Ok(Some(children.into_iter().map(|c| c.celestial_id).collect()))
}

fn parse_dynamic_u64(v: &DynamicValue) -> Option<u64> {
    match v {
        DynamicValue::Uint64(n) | DynamicValue::Fixed64(n) => Some(*n),
        DynamicValue::Uint32(n) | DynamicValue::Fixed32(n) => Some(u64::from(*n)),
        DynamicValue::Int64(n) | DynamicValue::Sint64(n) | DynamicValue::Sfixed64(n) => {
            u64::try_from(*n).ok()
        }
        DynamicValue::Int32(n) | DynamicValue::Sint32(n) | DynamicValue::Sfixed32(n) => {
            u64::try_from(*n).ok()
        }
        _ => None,
    }
}

fn parse_dynamic_i64(v: &DynamicValue) -> Option<i64> {
    match v {
        DynamicValue::Int64(n) | DynamicValue::Sint64(n) | DynamicValue::Sfixed64(n) => Some(*n),
        DynamicValue::Int32(n) | DynamicValue::Sint32(n) | DynamicValue::Sfixed32(n) => {
            Some(i64::from(*n))
        }
        DynamicValue::Uint32(n) | DynamicValue::Fixed32(n) => Some(i64::from(*n)),
        DynamicValue::Uint64(n) | DynamicValue::Fixed64(n) => i64::try_from(*n).ok(),
        _ => None,
    }
}

fn extract_u64_from_field(dyn_msg: &DynamicQuasarMessage, field_num: u32) -> u64 {
    match dyn_msg.get(field_num) {
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::Message(inner)) => {
                inner.get(1).and_then(parse_dynamic_u64).unwrap_or(0)
            }
            Some(val) => parse_dynamic_u64(val).unwrap_or(0),
            None => 0,
        },
        Some(val) => parse_dynamic_u64(val).unwrap_or(0),
        None => 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WalletOwnerId {
    Character(i64),
    Corporation(i64),
}

impl WalletOwnerId {
    fn raw_id(self) -> i64 {
        match self {
            Self::Character(id) | Self::Corporation(id) => id,
        }
    }
}

fn parse_wallet_owner_field(
    dyn_msg: &DynamicQuasarMessage,
    field_num: u32,
) -> Result<Option<WalletOwnerId>, (u32, String)> {
    let Some(val) = dyn_msg.get(field_num) else {
        return Ok(None);
    };
    let DynamicValue::Message(owner_msg) = val else {
        return Err((STATUS_BAD_REQUEST, "Invalid wallet owner field".into()));
    };
    if let Some(ch_val) = owner_msg.get(1) {
        let cid = match ch_val {
            DynamicValue::Message(m) => m.get(1).and_then(parse_dynamic_u64).unwrap_or(0),
            other => parse_dynamic_u64(other).unwrap_or(0),
        };
        if cid == 0 {
            return Err((STATUS_BAD_REQUEST, "Invalid character wallet owner ID".into()));
        }
        return Ok(Some(WalletOwnerId::Character(cid as i64)));
    }
    if let Some(corp_val) = owner_msg.get(2) {
        let corp_id = match corp_val {
            DynamicValue::Message(m) => m.get(1).and_then(parse_dynamic_u64).unwrap_or(0),
            other => parse_dynamic_u64(other).unwrap_or(0),
        };
        if corp_id == 0 {
            return Err((STATUS_BAD_REQUEST, "Invalid corporation wallet owner ID".into()));
        }
        return Ok(Some(WalletOwnerId::Corporation(corp_id as i64)));
    }
    Err((STATUS_BAD_REQUEST, "Missing wallet owner character or corporation ID".into()))
}

fn parse_positive_isk_currency(
    dyn_msg: &DynamicQuasarMessage,
    field_num: u32,
) -> Result<(i64, i32, IskCents), (u32, String)> {
    let Some(DynamicValue::Message(curr_msg)) = dyn_msg.get(field_num) else {
        return Err((STATUS_BAD_REQUEST, "Missing ISK amount".into()));
    };
    let units = match curr_msg.get(1) {
        Some(v) => parse_dynamic_i64(v)
            .ok_or_else(|| (STATUS_BAD_REQUEST, "Invalid ISK currency units".into()))?,
        None => 0,
    };
    let nanos_i64 = match curr_msg.get(2) {
        Some(v) => parse_dynamic_i64(v)
            .ok_or_else(|| (STATUS_BAD_REQUEST, "Invalid ISK currency nanos".into()))?,
        None => 0,
    };
    if units < 0 || !(0..1_000_000_000).contains(&nanos_i64) {
        return Err((STATUS_BAD_REQUEST, "ISK amount must be non-negative".into()));
    }
    let nanos = nanos_i64 as i32;
    let unit_cents = units
        .checked_mul(100)
        .ok_or_else(|| (STATUS_BAD_REQUEST, "ISK amount overflows cents".into()))?;
    let nano_cents = i64::from(nanos / 10_000_000);
    let total_cents = unit_cents
        .checked_add(nano_cents)
        .ok_or_else(|| (STATUS_BAD_REQUEST, "ISK amount overflows cents".into()))?;
    if total_cents <= 0 {
        return Err((STATUS_BAD_REQUEST, "Transfer ISK amount must be positive".into()));
    }
    Ok((units, nanos, IskCents(total_cents)))
}

fn resolve_skyhook_exit_hour(
    snap: &eve_db::WorldStateSnapshot,
    skyhook_id: i64,
    owner_corp_id: i64,
) -> Option<u32> {
    if let Some(&(start_or_hour, day_or_end)) = snap.structure_vulnerability_windows.get(&skyhook_id)
    {
        if start_or_hour < 24 && day_or_end < 7 {
            return Some(start_or_hour as u32);
        }
        return Some(((start_or_hour / 3_600) % 24) as u32);
    }
    if let Some(gov) = snap.corp_governance.get(&owner_corp_id) {
        return Some(u32::from(gov.reinforce_hour % 24));
    }
    snap.sov_runtime
        .skyhook_theft_windows
        .get(&skyhook_id)
        .map(|&(start_unix, _)| ((start_unix / 3_600) % 24) as u32)
}

fn map_skyhook_state_enum(state: &str) -> &'static str {
    match state {
        "ShieldVulnerable" | "Anchoring" | "OnliningVulnerable" => "STATE_SHIELD_VULNERABLE",
        "ArmorReinforced" => "STATE_ARMOR_REINFORCE",
        "ArmorVulnerable" => "STATE_ARMOR_VULNERABLE",
        "HullReinforced" => "STATE_HULL_REINFORCE",
        "HullVulnerable" => "STATE_HULL_VULNERABLE",
        _ => "STATE_UNSPECIFIED",
    }
}

fn parse_cerberus_parameter_value(param: &DynamicQuasarMessage) -> Option<(String, String)> {
    let key = match param.get(1) {
        Some(DynamicValue::String(k)) if !k.is_empty() => k.clone(),
        _ => return None,
    };
    let val = if let Some(DynamicValue::String(s)) = param.get(2) {
        s.clone()
    } else if let Some(u) = param.get(3).and_then(parse_dynamic_u64) {
        u.to_string()
    } else if let Some(i) = param.get(4).and_then(parse_dynamic_i64) {
        i.to_string()
    } else if let Some(DynamicValue::Double(d)) = param.get(5) {
        d.to_string()
    } else if let Some(DynamicValue::Float(f)) = param.get(5) {
        f.to_string()
    } else if let Some(DynamicValue::Message(ts)) = param.get(6) {
        ts.get(1).and_then(parse_dynamic_i64)?.to_string()
    } else if let Some(DynamicValue::Message(dur)) = param.get(7) {
        dur.get(1).and_then(parse_dynamic_i64)?.to_string()
    } else {
        return None;
    };
    Some((key, val))
}

fn resolve_caller_corp_id(
    ctx: &AuthoritativeContext,
    snap: &eve_db::WorldStateSnapshot,
    effective_cid: i64,
) -> i64 {
    let ctx_corp = ctx
        .effective_corporation_id()
        .map(|id| id as i64)
        .filter(|&id| id > 0);
    let char_corp = snap
        .characters
        .get(&effective_cid)
        .map(|c| c.corporation_id)
        .filter(|&id| id > 0);
    match (ctx_corp, char_corp) {
        (Some(ctx_c), Some(1_000_009)) => ctx_c,
        (_, Some(ch_c)) => ch_c,
        (Some(ctx_c), _) => ctx_c,
        _ => 0,
    }
}

fn authorize_corp_financial_access(
    ctx: &AuthoritativeContext,
    snap: &eve_db::WorldStateSnapshot,
    requested_corp_id: i64,
) -> Result<i64, (u32, String)> {
    let effective_cid = ctx.effective_character_id().unwrap_or(0) as i64;
    if effective_cid <= 0 {
        return Err((
            STATUS_UNAUTHORIZED,
            "Active character identity required".to_string(),
        ));
    }
    let caller_corp = resolve_caller_corp_id(ctx, snap, effective_cid);
    let target_corp = if requested_corp_id != 0 {
        requested_corp_id
    } else {
        caller_corp
    };
    if target_corp <= 0 {
        return Err((
            STATUS_FORBIDDEN,
            "No target corporation available for financial query".to_string(),
        ));
    }
    let is_member = caller_corp == target_corp
        || snap
            .corporations
            .get(&target_corp)
            .is_some_and(|corp| corp.ceo_id == effective_cid || corp.members.contains(&effective_cid));
    if !is_member {
        return Err((
            STATUS_FORBIDDEN,
            format!("Character {effective_cid} is not a member of corporation {target_corp}"),
        ));
    }
    let allowed_roles = eve_db::corp_roles::ROLE_DIRECTOR
        | eve_db::corp_roles::ROLE_ACCOUNTANT
        | eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT
        | eve_db::corp_roles::ROLE_TRADER;
    let (is_ceo, eff_roles) =
        snap.effective_corp_roles_at_db(target_corp, effective_cid, None, 0);
    if is_ceo {
        return Ok(target_corp);
    }
    if (eff_roles & allowed_roles) == 0 {
        return Err((
            STATUS_FORBIDDEN,
            format!(
                "Character {effective_cid} lacks corporate financial/trader role for corporation {target_corp}"
            ),
        ));
    }
    Ok(target_corp)
}

pub fn register_economy_planetary_industry_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
    _descriptors: Arc<QuasarDescriptorRegistry>,
) {
    // 1. eve.accounting.character.journal.GetCharacterEntriesRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.accounting.character.journal.GetCharacterEntriesRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let req_cid = extract_u64_from_field(dyn_msg, 1) as i64;
            let effective_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            if effective_cid <= 0 {
                return Err((
                    STATUS_UNAUTHORIZED,
                    "Active character identity required".to_string(),
                ));
            }
            if req_cid != 0 && req_cid != effective_cid {
                return Err((
                    STATUS_FORBIDDEN,
                    format!(
                        "Character {effective_cid} is not authorized to read journal entries for character {req_cid}"
                    ),
                ));
            }
            let target_cid = effective_cid;

            let (entries, timestamps) = db_clone.with_snapshot(|snap| {
                let list: Vec<_> = snap
                    .wallet_journal
                    .iter()
                    .rev()
                    .filter(|e| e.character_id == target_cid)
                    .take(200)
                    .cloned()
                    .collect();
                let ts: BTreeMap<i64, u64> = list
                    .iter()
                    .map(|e| {
                        let date = snap
                            .wallet_journal_meta
                            .get(&e.entry_id)
                            .map(|m| m.date_unix)
                            .filter(|&d| d > 0)
                            .or_else(|| snap.wallet_journal_timestamps.get(&e.entry_id).copied())
                            .unwrap_or(0);
                        (e.entry_id, date)
                    })
                    .collect();
                (list, ts)
            });

            let mut resp = pb::message(
                "eve.accounting.character.journal.GetCharacterEntriesResponse",
            );
            for e in &entries {
                let date_unix = timestamps.get(&e.entry_id).copied().unwrap_or(0) as i64;
                resp = resp.add_with("entries", |entry| {
                    entry
                        .set("id.transaction", e.entry_id as u64)
                        .set_with("attributes", |attrs| {
                            attrs
                                .set("date.seconds", date_unix)
                                .set_with("amount", |c| set_isk_currency(c, e.amount.0))
                                .set_with("account_balance", |c| {
                                    set_isk_currency(c, e.balance_after.0)
                                })
                                .set("description", &e.reason)
                        })
                });
            }

            resp.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 2. eve.accounting.corporation.journal.GetCorporationEntriesRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.accounting.corporation.journal.GetCorporationEntriesRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let req_corp_id = extract_u64_from_field(dyn_msg, 1) as i64;
            let (entries, timestamps) = db_clone.with_snapshot(|snap| {
                let target_corp = authorize_corp_financial_access(ctx, snap, req_corp_id)?;
                // Filter strictly by owning corporation (`e.character_id == target_corp`, matching
                // `wallet_ledger.rs`): never include counterparty pilot rows via `wallet_journal_meta`,
                // which would duplicate transfers and leak personal `balance_after` values.
                let list: Vec<_> = snap
                    .wallet_journal
                    .iter()
                    .rev()
                    .filter(|e| e.character_id == target_corp)
                    .take(200)
                    .cloned()
                    .collect();
                let ts: BTreeMap<i64, u64> = list
                    .iter()
                    .map(|e| {
                        let date = snap
                            .wallet_journal_meta
                            .get(&e.entry_id)
                            .map(|m| m.date_unix)
                            .filter(|&d| d > 0)
                            .or_else(|| snap.wallet_journal_timestamps.get(&e.entry_id).copied())
                            .unwrap_or(0);
                        (e.entry_id, date)
                    })
                    .collect();
                Ok::<_, (u32, String)>((list, ts))
            })?;

            let mut resp = pb::message(
                "eve.accounting.corporation.journal.GetCorporationEntriesResponse",
            );
            for e in &entries {
                let date_unix = timestamps.get(&e.entry_id).copied().unwrap_or(0) as i64;
                resp = resp.add_with("entries", |entry| {
                    entry
                        .set("id.transaction", e.entry_id as u64)
                        .set_with("attributes", |attrs| {
                            attrs
                                .set("date.seconds", date_unix)
                                .set_with("amount", |c| set_isk_currency(c, e.amount.0))
                                .set_with("account_balance", |c| {
                                    set_isk_currency(c, e.balance_after.0)
                                })
                                .set("description", &e.reason)
                        })
                });
            }

            resp.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 3. eve.accounting.entrytype.GetAllRequest (shared canonical table with Macho account.GetEntryTypes)
    router.register_handler(
        "eve.accounting.entrytype.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut resp = pb::message("eve.accounting.entrytype.GetAllResponse");
            for &(id, name, _desc) in crate::services::wallet_ledger::JOURNAL_ENTRY_TYPES {
                let Ok(seq_id) = u32::try_from(id) else {
                    continue;
                };
                resp = resp.add_with("entry_types", |et| {
                    et.set("id.sequential", seq_id).set("attributes.name", name)
                });
            }
            resp.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 6. eve.contract.item_exchange.GetRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.contract.item_exchange.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let effective_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            let contract_id = extract_u64_from_field(dyn_msg, 1) as i64;
            let contract = db_clone.with_snapshot(|snap| -> Result<eve_db::ContractRecord, (u32, String)> {
                let Some(c) = snap.contracts.get(&contract_id).cloned() else {
                    return Err((
                        STATUS_NOT_FOUND,
                        format!("Contract {contract_id} not found"),
                    ));
                };
                let is_public = (c.availability.is_empty() || c.availability.eq_ignore_ascii_case("Public"))
                    && c.assignee_id == 0;
                if !is_public {
                    if effective_cid <= 0 {
                        return Err((
                            STATUS_UNAUTHORIZED,
                            "Active character identity required to read private contract".to_string(),
                        ));
                    }
                    let caller_corp = resolve_caller_corp_id(ctx, snap, effective_cid);
                    let caller_alliance = snap
                        .corporations
                        .get(&caller_corp)
                        .and_then(|corp| corp.alliance_id)
                        .unwrap_or(0);
                    let authorized = c.issuer_id == effective_cid
                        || c.acceptor_id == Some(effective_cid)
                        || c.assignee_id == effective_cid
                        || (caller_corp != 0
                            && ((c.for_corp && c.issuer_corp_id == caller_corp)
                                || (c.availability.eq_ignore_ascii_case("Corporation")
                                    && c.issuer_corp_id == caller_corp)
                                || c.assignee_id == caller_corp
                                || c.acceptor_id == Some(caller_corp)))
                        || (caller_alliance != 0 && c.assignee_id == caller_alliance);
                    if !authorized {
                        return Err((
                            STATUS_FORBIDDEN,
                            format!(
                                "Character {effective_cid} is not authorized to read private contract {contract_id}"
                            ),
                        ));
                    }
                }
                Ok(c)
            })?;

            pb::message("eve.contract.item_exchange.GetResponse")
                .set_with("contract", |attrs| {
                    attrs
                        .set("issuer.character.sequential", contract.issuer_id as u32)
                        .set(
                            "available_to_public",
                            (contract.availability.is_empty()
                                || contract.availability.eq_ignore_ascii_case("Public"))
                                && contract.assignee_id == 0,
                        )
                        .set("issued_time.seconds", contract.date_issued_unix as i64)
                        .set("expiry_time.seconds", contract.expiration_unix as i64)
                        .set_with("price", |p| set_isk_currency(p, contract.price.0))
                        .set_with("reward", |r| set_isk_currency(r, contract.reward.0))
                })
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 7. eve.character.industry.job.* & blueprints
    for (req_name, resp_name, filter_mode) in [
        (
            "eve.character.industry.job.GetRequest",
            "eve.character.industry.job.GetResponse",
            0u8,
        ),
        (
            "eve.character.industry.job.GetActiveRequest",
            "eve.character.industry.job.GetActiveResponse",
            1u8,
        ),
        (
            "eve.character.industry.job.GetCompletedRequest",
            "eve.character.industry.job.GetCompletedResponse",
            2u8,
        ),
    ] {
        let db_jobs = Arc::clone(&db);
        router.register_handler(
            req_name,
            Arc::new(move |ctx, _any, dyn_msg| {
                let target_cid =
                    resolve_industry_target_char(ctx, extract_u64_from_field(dyn_msg, 1) as i64)?;
                let mut jobs: Vec<eve_db::IndustryJobRecord> = db_jobs.with_snapshot(|snap| {
                    snap.industry_jobs
                        .values()
                        .filter(|j| j.installer_id == target_cid)
                        .filter(|j| match filter_mode {
                            1 => {
                                j.status.eq_ignore_ascii_case("Active")
                                    || j.status.eq_ignore_ascii_case("Paused")
                                    || j.status.eq_ignore_ascii_case("Ready")
                            }
                            2 => {
                                j.status.eq_ignore_ascii_case("Delivered")
                                    || j.status.eq_ignore_ascii_case("Completed")
                            }
                            _ => true,
                        })
                        .cloned()
                        .collect()
                });
                jobs.sort_by_key(|j| j.job_id);
                let mut resp = pb::message(resp_name);
                for job in &jobs {
                    resp = resp.add_with("jobs", |entry| encode_industry_job_entry(entry, job));
                }
                resp.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
            }),
        );
    }
    let db_bps = Arc::clone(&db);
    let sde_bps = Arc::clone(&sde);
    router.register_handler(
        "eve.character.industry.blueprints.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let target_cid =
                resolve_industry_target_char(ctx, extract_u64_from_field(dyn_msg, 1) as i64)?;
            let resp = db_bps.with_snapshot(|snap| {
                let mut resp = pb::message("eve.character.industry.blueprints.GetResponse");
                let mut bps: Vec<&eve_db::InventoryItemRecord> = snap
                    .items
                    .values()
                    .filter(|it| {
                        it.owner_id == target_cid
                            && it.quantity > 0
                            && sde_bps
                                .types
                                .get(&it.type_id)
                                .is_some_and(|t| t.category_id == 9)
                    })
                    .collect();
                bps.sort_by_key(|it| it.item_id);
                for it in bps {
                    let me = it
                        .dynamic_attributes
                        .get(&ATTR_BLUEPRINT_ME)
                        .copied()
                        .unwrap_or(0.0)
                        .max(0.0) as u32;
                    let te = it
                        .dynamic_attributes
                        .get(&ATTR_BLUEPRINT_TE)
                        .copied()
                        .unwrap_or(0.0)
                        .max(0.0) as u32;
                    if it.is_blueprint_copy {
                        let runs = it.blueprint_runs_remaining.unwrap_or(1).max(0) as u32;
                        resp = resp.add_with("blueprint_copies", |b| {
                            b.set("blueprint_id.sequential", it.item_id as u64)
                                .set_with("attributes", |attrs| {
                                    attrs
                                        .set("type.sequential", it.type_id)
                                        .set("material_efficiency", me)
                                        .set("time_efficiency", te)
                                        .set("runs_remaining", runs)
                                })
                                .set_with("location", |loc| {
                                    encode_blueprint_location(loc, it.location_id, &sde_bps, snap)
                                })
                        });
                    } else if it.singleton {
                        resp = resp.add_with("blueprints", |b| {
                            b.set("blueprint_id.sequential", it.item_id as u64)
                                .set_with("attributes", |attrs| {
                                    attrs
                                        .set("type.sequential", it.type_id)
                                        .set("material_efficiency", me)
                                        .set("time_efficiency", te)
                                })
                                .set_with("location", |loc| {
                                    encode_blueprint_location(loc, it.location_id, &sde_bps, snap)
                                })
                        });
                    } else {
                        resp = resp.add_with("blueprints_stacks", |b| {
                            b.set("item_id.sequential", it.item_id as u64)
                                .set("blueprint_type.sequential", it.type_id)
                                .set("quantity", it.quantity.max(0) as u32)
                                .set_with("location", |loc| {
                                    encode_blueprint_location(loc, it.location_id, &sde_bps, snap)
                                })
                        });
                    }
                }
                resp
            });
            resp.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 8. eve.industry.facility.GetRequest
    let db_fac = Arc::clone(&db);
    let sde_fac = Arc::clone(&sde);
    router.register_handler(
        "eve.industry.facility.GetRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let (station_id, _) = caller_station_and_solar_system(ctx, &db_fac);
            let facility_opt = if station_id > 0 {
                if let Some(st) = sde_fac.station(station_id as i64) {
                    Some((
                        st.owner_corporation_id as u32,
                        st.region_id as u64,
                        st.solar_system_id as u64,
                        Some(st.station_type_id),
                        true,
                    ))
                } else {
                    let st_info = db_fac.with_snapshot(|snap| {
                        snap.structures.get(&(station_id as i64)).map(|s| {
                            (
                                s.owner_corp_id,
                                s.solar_system_id,
                                s.type_id,
                                !s.state.eq_ignore_ascii_case("Anchoring")
                                    && !s.state.eq_ignore_ascii_case("Destroyed"),
                            )
                        })
                    });
                    let Some((owner_corp, s_sys, stype, online)) = st_info else {
                        return Err((
                            STATUS_NOT_FOUND,
                            format!("Station or facility {station_id} not found"),
                        ));
                    };
                    let Some(ss) = sde_fac.solar_system(s_sys) else {
                        return Err((
                            STATUS_NOT_FOUND,
                            format!("Solar system {s_sys} for facility {station_id} not found"),
                        ));
                    };
                    Some((
                        owner_corp as u32,
                        ss.region_id as u64,
                        s_sys as u64,
                        Some(stype),
                        online,
                    ))
                }
            } else {
                None
            };
            let mut resp = pb::message("eve.industry.facility.GetResponse");
            if let Some((corp_id, region_id, sys_id, facility_type_id, online)) = facility_opt {
                resp = resp.add_with("facilities", |fac| {
                    fac.set("id.sequential", station_id)
                        .set_with("attributes", |attrs| {
                            attrs
                                .set("corporation.sequential", corp_id)
                                .set("region.sequential", region_id)
                                .set("solar_system.sequential", sys_id)
                                .set_opt("type.sequential", facility_type_id)
                                .set("online", online)
                        })
                });
            }
            resp.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 8b. eve.industry.system.GetCostIndicesRequest
    let db_cost = Arc::clone(&db);
    let sde_cost = Arc::clone(&sde);
    router.register_handler(
        "eve.industry.system.GetCostIndicesRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let entries: BTreeMap<u32, BTreeMap<i32, f32>> = db_cost.with_snapshot(|snap| {
                let max_start = snap
                    .industry_jobs
                    .values()
                    .map(|j| j.start_unix)
                    .max()
                    .unwrap_or(0);
                let cutoff = max_start.saturating_sub(28 * 86_400);
                let mut sys_work: BTreeMap<u32, BTreeMap<i32, u64>> = BTreeMap::new();
                let mut univ_work: BTreeMap<i32, u64> = BTreeMap::new();
                for j in snap.industry_jobs.values() {
                    if j.status.eq_ignore_ascii_case("Cancelled")
                        || j.status.eq_ignore_ascii_case("Reverted")
                        || (j.start_unix > 0 && j.start_unix < cutoff)
                        || j.duration_seconds == 0
                    {
                        continue;
                    }
                    let act = match j.activity_id {
                        1 => 1i32,
                        3 => 3i32,
                        4 => 4i32,
                        5 => 5i32,
                        8 => 8i32,
                        9 | 11 => 9i32,
                        _ => continue,
                    };
                    let Some(jsys) = sde_cost
                        .station(j.facility_id)
                        .map(|st| st.solar_system_id)
                        .or_else(|| snap.structures.get(&j.facility_id).map(|s| s.solar_system_id))
                    else {
                        continue;
                    };
                    *sys_work.entry(jsys).or_default().entry(act).or_insert(0) += j.duration_seconds;
                    *univ_work.entry(act).or_insert(0) += j.duration_seconds;
                }
                let mut out: BTreeMap<u32, BTreeMap<i32, f32>> = BTreeMap::new();
                for (sid, acts) in sys_work {
                    let mut map = BTreeMap::new();
                    for (act, sw) in acts {
                        let uw = univ_work.get(&act).copied().unwrap_or(0);
                        if uw == 0 || sw == 0 {
                            continue;
                        }
                        let sci = ((sw as f64) / (uw as f64)).sqrt().clamp(0.001, 0.50) as f32;
                        map.insert(act, sci);
                    }
                    if !map.is_empty() {
                        out.insert(sid, map);
                    }
                }
                out
            });

            let mut resp = pb::message("eve.industry.system.GetCostIndicesResponse");
            for (sid, acts) in entries {
                resp = resp.add_with("cost_indices", |mut idx| {
                    idx = idx.set("solar_system.sequential", sid as u64);
                    for (act, sci) in acts {
                        idx = idx.add_with("indices", |a| {
                            a.set("activity", act).set("cost_index", sci)
                        });
                    }
                    idx
                });
            }
            resp.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 9. eve.planet.GetRequest
    let sde_planet = Arc::clone(&sde);
    router.register_handler(
        "eve.planet.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let planet_id = extract_u64_from_field(dyn_msg, 1);
            let Some(c) =
                lookup_sde_celestial_with_group(&sde_planet, planet_id as i64, 7, "Planet")?
            else {
                return Err((STATUS_NOT_FOUND, format!("Planet {planet_id} not found")));
            };
            pb::message("eve.planet.GetResponse")
                .set_with("planet", |attrs| {
                    encode_celestial_attributes(
                        attrs,
                        "solarsystem.sequential",
                        &c.name,
                        c.solar_system_id as u64,
                        Some([c.position.x, c.position.y, c.position.z]),
                    )
                    .set("type.sequential", c.type_id as u64)
                })
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 10. eve.planet.GetPlanetsInSolarSystemRequest
    let sde_sys_planets = Arc::clone(&sde);
    router.register_handler(
        "eve.planet.GetPlanetsInSolarSystemRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let sys_id = extract_u64_from_field(dyn_msg, 1);
            if sys_id == 0 || !sde_sys_planets.solar_systems.contains_key(&(sys_id as u32)) {
                return Err((STATUS_NOT_FOUND, format!("Solar system {sys_id} not found")));
            }
            let mut resp = pb::message("eve.planet.GetPlanetsInSolarSystemResponse");
            let mut planets: Vec<&CelestialInfo> = sde_sys_planets
                .celestials_in_system_slice(sys_id as u32)
                .iter()
                .filter(|c| c.group_id == 7)
                .collect();
            planets.sort_by_key(|c| (c.celestial_index.unwrap_or(i32::MAX), c.celestial_id));
            for p in planets {
                resp = resp.add_with("planets", |pl| pl.set("sequential", p.celestial_id as u64));
            }
            resp.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 11. eve.planet.skyhook.admin.api.ChangeOwnerRequest
    let db_sky_owner = Arc::clone(&db);
    let sky_owner_notices = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve.planet.skyhook.admin.api.ChangeOwnerRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let skyhook_id = extract_u64_from_field(dyn_msg, 2) as i64;
            let req_sys = extract_u64_from_field(dyn_msg, 3) as u32;
            let new_corp_id = extract_u64_from_field(dyn_msg, 4) as i64;
            if skyhook_id <= 0 {
                return Err((STATUS_NOT_FOUND, "Skyhook not found".into()));
            }
            if new_corp_id <= 0 {
                return Err((STATUS_BAD_REQUEST, "Target corporation ID required".into()));
            }
            let (sys_id, planet_id, old_corp_id) = db_sky_owner.try_with_mut_snapshot(|snap| {
                if !snap.corporations.contains_key(&new_corp_id) {
                    return Err((
                        STATUS_NOT_FOUND,
                        format!("Corporation {new_corp_id} not found"),
                    ));
                }
                let planet = snap
                    .sov_runtime
                    .skyhook_planets
                    .get(&skyhook_id)
                    .copied()
                    .unwrap_or(0);
                let st = snap
                    .structures
                    .get_mut(&skyhook_id)
                    .filter(|s| {
                        s.type_id == super::sov_services::TYPE_ORBITAL_SKYHOOK
                            && (req_sys == 0 || s.solar_system_id == req_sys)
                    })
                    .ok_or_else(|| (STATUS_NOT_FOUND, format!("Skyhook {skyhook_id} not found")))?;
                let old_corp = st.owner_corp_id;
                let sys = st.solar_system_id;
                st.owner_corp_id = new_corp_id;
                Ok((sys, planet, old_corp))
            })?;
            let mut ev = pb::message("eve.planet.skyhook.api.OwnerChanged")
                .set("skyhook.sequential", skyhook_id as u64)
                .set("corporation.sequential", new_corp_id as u32)
                .set("old_corporation.sequential", old_corp_id as u32);
            if planet_id > 0 {
                ev = ev.set("planet.sequential", planet_id as u64);
            }
            if let Ok(notice) = ev.to_any() {
                sky_owner_notices
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((NoticeScope::SolarSystem(sys_id), notice));
            }
            pb::message("eve.planet.skyhook.admin.api.ChangeOwnerResponse")
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 12. eve.planet.skyhook.api.GetReinforcementExitHourRequest
    let db_sky_exit = Arc::clone(&db);
    router.register_handler(
        "eve.planet.skyhook.api.GetReinforcementExitHourRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let skyhook_id = extract_u64_from_field(dyn_msg, 1) as i64;
            let req_sys = extract_u64_from_field(dyn_msg, 2) as u32;
            if skyhook_id <= 0 {
                return Err((STATUS_NOT_FOUND, "Skyhook not found".into()));
            }
            let exit_hour = db_sky_exit.with_snapshot(|snap| {
                let st = snap
                    .structures
                    .get(&skyhook_id)
                    .filter(|s| {
                        s.type_id == super::sov_services::TYPE_ORBITAL_SKYHOOK
                            && (req_sys == 0 || s.solar_system_id == req_sys)
                    })
                    .ok_or_else(|| (STATUS_NOT_FOUND, format!("Skyhook {skyhook_id} not found")))?;
                resolve_skyhook_exit_hour(snap, skyhook_id, st.owner_corp_id).ok_or_else(|| {
                    (
                        STATUS_NOT_FOUND,
                        format!("Skyhook {skyhook_id} reinforcement exit hour not configured"),
                    )
                })
            })?;
            pb::message("eve.planet.skyhook.api.GetReinforcementExitHourResponse")
                .set("reinforcement_exit_hour", exit_hour)
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 13. eve.planet.skyhook.api.GetStateRequest
    let db_sky_state = Arc::clone(&db);
    router.register_handler(
        "eve.planet.skyhook.api.GetStateRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let skyhook_id = extract_u64_from_field(dyn_msg, 1) as i64;
            if skyhook_id <= 0 {
                return Err((STATUS_NOT_FOUND, "Skyhook not found".into()));
            }
            let (state_name, reinforced_until) = db_sky_state.with_snapshot(|snap| {
                let st = snap
                    .structures
                    .get(&skyhook_id)
                    .filter(|s| s.type_id == super::sov_services::TYPE_ORBITAL_SKYHOOK)
                    .ok_or_else(|| (STATUS_NOT_FOUND, format!("Skyhook {skyhook_id} not found")))?;
                Ok::<_, (u32, String)>((st.state.clone(), st.reinforced_until_unix))
            })?;
            let mut resp = pb::message("eve.planet.skyhook.api.GetStateResponse")
                .set("state", pb::enum_name(map_skyhook_state_enum(&state_name)));
            if reinforced_until > ctx.now_unix() {
                resp = resp.set("timer_end.seconds", reinforced_until as i64);
            } else {
                resp = resp.set("no_timer", true);
            }
            resp.to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    fn lookup_pi_pin(
        ctx: &AuthoritativeContext,
        db: &WorldRepository,
        sde: &SdeRegistry,
        pin_id: u64,
        allowed_groups: &[u32],
        kind: &str,
    ) -> Result<(u32, u64), (u32, String)> {
        let caller = ctx.effective_character_id().unwrap_or(0) as i64;
        let is_admin = require_admin_role(ctx).is_ok();
        if caller <= 0 && !is_admin {
            return Err((
                STATUS_UNAUTHORIZED,
                "Active character identity required".to_string(),
            ));
        }
        if pin_id == 0 {
            return Err((STATUS_NOT_FOUND, format!("PI {kind} {pin_id} not found")));
        }
        db.with_snapshot(|snap| {
            let mut found_forbidden = false;
            for (&(owner_cid, planet_id), layout) in &snap.planetary_colony_layouts {
                let Some(pin) = layout.pins.iter().find(|p| p.pin_id == pin_id as i64) else {
                    continue;
                };
                let group_ok = sde
                    .types
                    .get(&pin.type_id)
                    .is_some_and(|t| allowed_groups.contains(&t.group_id));
                if !group_ok {
                    continue;
                }
                if owner_cid == caller || is_admin {
                    return Ok((owner_cid as u32, planet_id as u64));
                }
                found_forbidden = true;
            }
            if found_forbidden {
                Err((
                    STATUS_FORBIDDEN,
                    format!(
                        "Character {caller} is not authorized to read PI {kind} {pin_id}"
                    ),
                ))
            } else {
                Err((STATUS_NOT_FOUND, format!("PI {kind} {pin_id} not found")))
            }
        })
    }

    // 14. eve.planetinteraction.collector.GetRequest
    let db_col = Arc::clone(&db);
    let sde_col = Arc::clone(&sde);
    router.register_handler(
        "eve.planetinteraction.collector.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let col_id = extract_u64_from_field(dyn_msg, 1);
            let (cid, planet_id) =
                lookup_pi_pin(ctx, &db_col, &sde_col, col_id, &[1026, 1063], "collector")?;
            pb::message("eve.planetinteraction.collector.GetResponse")
                .set_with("collector", |attrs| {
                    attrs
                        .set("character.sequential", cid)
                        .set("planet.sequential", planet_id)
                })
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 15. eve.planetinteraction.factory.GetRequest
    let db_fact = Arc::clone(&db);
    let sde_fact = Arc::clone(&sde);
    router.register_handler(
        "eve.planetinteraction.factory.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let fact_id = extract_u64_from_field(dyn_msg, 1);
            let (cid, planet_id) =
                lookup_pi_pin(ctx, &db_fact, &sde_fact, fact_id, &[1028], "factory")?;
            pb::message("eve.planetinteraction.factory.GetResponse")
                .set_with("factory", |attrs| {
                    attrs
                        .set("character.sequential", cid)
                        .set("planet.sequential", planet_id)
                })
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 16. eve.planetinteraction.schematic.GetRequest
    let sde_schematic = Arc::clone(&sde);
    router.register_handler(
        "eve.planetinteraction.schematic.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let sch_id = extract_u64_from_field(dyn_msg, 1);
            let Some(schem) = sde_schematic.planet_schematic(sch_id as u32) else {
                return Err((
                    STATUS_NOT_FOUND,
                    format!("PI schematic {sch_id} not found"),
                ));
            };
            let cycle_time_sec = schem.cycle_time.min(u32::MAX as u64) as u32;
            let schematic_name = schem.name.clone();
            pb::message("eve.planetinteraction.schematic.GetResponse")
                .set_with("schematic", |attrs| {
                    attrs
                        .set("cycle_time_sec", cycle_time_sec)
                        .set("schematic_name", schematic_name)
                })
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 17. eve.planetinteraction.storage.GetRequest
    let db_stor = Arc::clone(&db);
    let sde_stor = Arc::clone(&sde);
    router.register_handler(
        "eve.planetinteraction.storage.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let store_id = extract_u64_from_field(dyn_msg, 1);
            let (cid, planet_id) =
                lookup_pi_pin(ctx, &db_stor, &sde_stor, store_id, &[1029, 1030], "storage")?;
            pb::message("eve.planetinteraction.storage.GetResponse")
                .set_with("storage", |attrs| {
                    attrs
                        .set("character.sequential", cid)
                        .set("planet.sequential", planet_id)
                })
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 18. eve.moon.GetRequest
    let sde_moon = Arc::clone(&sde);
    router.register_handler(
        "eve.moon.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let moon_id = extract_u64_from_field(dyn_msg, 1);
            let Some(c) =
                lookup_sde_celestial_with_group(&sde_moon, moon_id as i64, 8, "Moon")?
            else {
                return Err((STATUS_NOT_FOUND, format!("Moon {moon_id} not found")));
            };
            pb::message("eve.moon.GetResponse")
                .set_with("attributes", |attrs| {
                    encode_celestial_attributes(
                        attrs,
                        "solar_system.sequential",
                        &c.name,
                        c.solar_system_id as u64,
                        Some([c.position.x, c.position.y, c.position.z]),
                    )
                })
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 19. eve.moon.GetMoonsOfPlanetRequest
    let sde_planet_moons = Arc::clone(&sde);
    router.register_handler(
        "eve.moon.GetMoonsOfPlanetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let planet_id = extract_u64_from_field(dyn_msg, 1);
            let Some(moons) = children_of_planet_in_sde(&sde_planet_moons, planet_id as i64, 8)?
            else {
                return Err((STATUS_NOT_FOUND, format!("Planet {planet_id} not found")));
            };
            let mut resp = pb::message("eve.moon.GetMoonsOfPlanetResponse");
            for moon_id in moons {
                resp = resp.add_with("moons", |m| m.set("sequential", moon_id as u64));
            }
            resp.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 20. eve.asteroidbelt.GetRequest
    let sde_belt = Arc::clone(&sde);
    router.register_handler(
        "eve.asteroidbelt.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let belt_id = extract_u64_from_field(dyn_msg, 1);
            let Some(c) = lookup_sde_celestial_with_group(
                &sde_belt,
                belt_id as i64,
                9,
                "Asteroid belt",
            )?
            else {
                return Err((
                    STATUS_NOT_FOUND,
                    format!("Asteroid belt {belt_id} not found"),
                ));
            };
            pb::message("eve.asteroidbelt.GetResponse")
                .set_with("attributes", |attrs| {
                    encode_celestial_attributes(
                        attrs,
                        "solar_system.sequential",
                        &c.name,
                        c.solar_system_id as u64,
                        Some([c.position.x, c.position.y, c.position.z]),
                    )
                })
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 21. eve.asteroidbelt.GetBeltsOfPlanetRequest
    let sde_planet_belts = Arc::clone(&sde);
    router.register_handler(
        "eve.asteroidbelt.GetBeltsOfPlanetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let planet_id = extract_u64_from_field(dyn_msg, 1);
            let Some(belts) =
                children_of_planet_in_sde(&sde_planet_belts, planet_id as i64, 9)?
            else {
                return Err((STATUS_NOT_FOUND, format!("Planet {planet_id} not found")));
            };
            let mut resp = pb::message("eve.asteroidbelt.GetBeltsOfPlanetResponse");
            for belt_id in belts {
                resp = resp.add_with("belts", |b| b.set("sequential", belt_id as u64));
            }
            resp.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 22. eve.bloodline.GetAllRequest
    let sde_bloodlines = Arc::clone(&sde);
    router.register_handler(
        "eve.bloodline.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut resp = pb::message("eve.bloodline.GetAllResponse");
            let mut bl_ids: Vec<_> = sde_bloodlines.bloodlines.keys().copied().collect();
            bl_ids.sort_unstable();
            for id in bl_ids {
                if let Some(bl) = sde_bloodlines.bloodlines.get(&id) {
                    resp = resp.add_with("bloodlines", |entry| {
                        entry
                            .set("identifier.sequential", id as u32)
                            .set_with("attributes", |attrs| {
                                attrs
                                    .set("name", &bl.name)
                                    .set("description", &bl.description)
                                    .set("race.sequential", bl.race_id as u32)
                                    .set_opt("ship_type.sequential", bl.ship_type_id.map(|t| t as u64))
                                    .set("charisma", bl.charisma.max(0) as u32)
                                    .set("intelligence", bl.intelligence.max(0) as u32)
                                    .set("memory", bl.memory.max(0) as u32)
                                    .set("perception", bl.perception.max(0) as u32)
                                    .set("willpower", bl.willpower.max(0) as u32)
                            })
                    });
                }
            }
            resp.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 23. eve.isk.wallet.api.TransferByOwnerRequest (admin-only authoritative transfer)
    let db_isk_xfer = Arc::clone(&db);
    let isk_notices = Arc::clone(&router.pending_notices);
    let isk_macho_sink = Arc::clone(&router.macho_notify_sink);
    router.register_handler(
        "eve.isk.wallet.api.TransferByOwnerRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let (units, nanos, amt) = parse_positive_isk_currency(dyn_msg, 5)?;
            let src_owner = match parse_wallet_owner_field(dyn_msg, 3)? {
                Some(owner) => owner,
                None => {
                    let acting = extract_u64_from_field(dyn_msg, 2) as i64;
                    let cid = if acting > 0 {
                        acting
                    } else {
                        ctx.effective_character_id()
                            .filter(|&v| v > 0)
                            .map(i64::from)
                            .unwrap_or(0)
                    };
                    if cid <= 0 {
                        return Err((
                            STATUS_UNAUTHORIZED,
                            "Active character session or transfer_from required".into(),
                        ));
                    }
                    WalletOwnerId::Character(cid)
                }
            };
            let dst_owner = parse_wallet_owner_field(dyn_msg, 4)?
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Recipient transfer_to required".into()))?;
            if src_owner == dst_owner {
                return Err((
                    STATUS_BAD_REQUEST,
                    "Source and recipient wallet owners must differ".into(),
                ));
            }
            let ref_type_id = match dyn_msg.get(6) {
                Some(DynamicValue::Message(m)) => m
                    .get(1)
                    .and_then(parse_dynamic_i64)
                    .and_then(|v| i32::try_from(v).ok())
                    .filter(|&v| v > 0)
                    .unwrap_or(ref_types::PLAYER_DONATION),
                _ => ref_types::PLAYER_DONATION,
            };
            let now_unix = ctx.now_unix();

            let (_src_bal, _dst_bal) = db_isk_xfer.try_with_mut_snapshot(|snap| {
                // Preflight recipient capacity before mutating source balance.
                let dst_next = match dst_owner {
                    WalletOwnerId::Character(id) => {
                        let ch = snap.characters.get(&id).ok_or_else(|| {
                            (STATUS_NOT_FOUND, format!("Recipient character {id} not found"))
                        })?;
                        ch.balance
                            .checked_add(amt)
                            .ok_or_else(|| (STATUS_BAD_REQUEST, "Recipient ISK balance overflow".into()))?
                    }
                    WalletOwnerId::Corporation(id) => {
                        let corp = snap.corporations.get(&id).ok_or_else(|| {
                            (STATUS_NOT_FOUND, format!("Recipient corporation {id} not found"))
                        })?;
                        corp.division_balance(1)
                            .checked_add(amt)
                            .ok_or_else(|| (STATUS_BAD_REQUEST, "Recipient ISK balance overflow".into()))?
                    }
                };
                let src_next = match src_owner {
                    WalletOwnerId::Character(id) => {
                        let ch = snap.characters.get_mut(&id).ok_or_else(|| {
                            (STATUS_NOT_FOUND, format!("Source character {id} not found"))
                        })?;
                        let next = ch
                            .balance
                            .checked_sub(amt)
                            .ok_or_else(|| (STATUS_BAD_REQUEST, "Insufficient ISK".into()))?;
                        ch.balance = next;
                        next
                    }
                    WalletOwnerId::Corporation(id) => {
                        let corp = snap.corporations.get_mut(&id).ok_or_else(|| {
                            (STATUS_NOT_FOUND, format!("Source corporation {id} not found"))
                        })?;
                        corp.debit_division(1, amt)
                            .ok_or_else(|| (STATUS_BAD_REQUEST, "Insufficient ISK".into()))?
                    }
                };
                match dst_owner {
                    WalletOwnerId::Character(id) => {
                        snap.characters.get_mut(&id).expect("verified").balance = dst_next;
                    }
                    WalletOwnerId::Corporation(id) => {
                        snap.corporations
                            .get_mut(&id)
                            .expect("verified")
                            .set_division_balance(1, dst_next);
                    }
                }
                let mut meta =
                    JournalMeta::new(ref_type_id, src_owner.raw_id(), dst_owner.raw_id(), now_unix);
                meta.account_key = 1000;
                snap.append_journal_entry(JournalEntryInput {
                    owner_id: src_owner.raw_id(),
                    entry_type: "TransferByOwner",
                    amount: IskCents(-amt.0),
                    balance_after: src_next,
                    reason: format!("Quasar TransferByOwner to {}", dst_owner.raw_id()),
                    meta: meta.clone(),
                });
                snap.append_journal_entry(JournalEntryInput {
                    owner_id: dst_owner.raw_id(),
                    entry_type: "TransferByOwner",
                    amount: amt,
                    balance_after: dst_next,
                    reason: format!("Quasar TransferByOwner from {}", src_owner.raw_id()),
                    meta,
                });
                Ok((src_next, dst_next))
            })?;

            let set_owner = |b: pb::ProtoBuilder, owner: WalletOwnerId| match owner {
                WalletOwnerId::Character(id) => b.set("character.sequential", id as u32),
                WalletOwnerId::Corporation(id) => b.set("corporation.sequential", id as u32),
            };
            match pb::message("eve.isk.wallet.api.GrantCommanded")
                .set("from_wallet_owner", true)
                .set_with("transfer_from", |o| set_owner(o, src_owner))
                .set_with("transfer_to", |o| set_owner(o, dst_owner))
                .set_with("amount", |a| a.set("units", units).set("nanos", nanos))
                .set("entry_type.sequential", ref_type_id as u32)
                .to_any()
            {
                Ok(notice) => {
                    let mut guard = isk_notices.write().unwrap_or_else(|e| e.into_inner());
                    let scope_of = |owner: WalletOwnerId| match owner {
                        WalletOwnerId::Character(id) => NoticeScope::Character(id as u32),
                        WalletOwnerId::Corporation(id) => NoticeScope::Corporation(id as u32),
                    };
                    guard.push((scope_of(src_owner), notice.clone()));
                    guard.push((scope_of(dst_owner), notice));
                }
                Err(e) => tracing::warn!(
                    src = src_owner.raw_id(),
                    dst = dst_owner.raw_id(),
                    error = %e,
                    "failed to encode GrantCommanded notice"
                ),
            }
            let _ = &isk_macho_sink;
            pb::message("eve.isk.wallet.api.TransferByOwnerResponse")
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 24. eve.localization.message.api.GetLocalizedTextRequest
    let sde_loc_msg = Arc::clone(&sde);
    router.register_handler(
        "eve.localization.message.api.GetLocalizedTextRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let seq = extract_u64_from_field(dyn_msg, 1);
            let Some(msg_id) = u32::try_from(seq).ok().filter(|&v| v > 0) else {
                return Err((
                    STATUS_BAD_REQUEST,
                    "Missing or invalid localization message ID".into(),
                ));
            };
            let text = sde_loc_msg
                .localized_message_en_us(msg_id)
                .ok_or_else(|| (STATUS_NOT_FOUND, format!("Localization message {msg_id} not found")))?;
            pb::message("eve.localization.message.api.GetLocalizedTextResponse")
                .set("text", &text)
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 25. eve.localization.cerberus.api.GetLocalizedTextRequest
    let sde_loc_cerb = Arc::clone(&sde);
    router.register_handler(
        "eve.localization.cerberus.api.GetLocalizedTextRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let Some(DynamicValue::Message(fmt_msg)) = dyn_msg.get(1) else {
                return Err((
                    STATUS_BAD_REQUEST,
                    "Missing Cerberus FormattedMessage payload".into(),
                ));
            };
            let msg_id = fmt_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Message(id_msg) => id_msg.get(1).and_then(parse_dynamic_u64),
                    other => parse_dynamic_u64(other),
                })
                .and_then(|v| u32::try_from(v).ok())
                .filter(|&v| v > 0)
                .ok_or_else(|| {
                    (
                        STATUS_BAD_REQUEST,
                        "Missing or invalid Cerberus message identifier".into(),
                    )
                })?;
            let mut text = sde_loc_cerb
                .localized_message_en_us(msg_id)
                .ok_or_else(|| (STATUS_NOT_FOUND, format!("Localization message {msg_id} not found")))?;
            match fmt_msg.get(2) {
                Some(DynamicValue::Repeated(items)) => {
                    for item in items {
                        if let DynamicValue::Message(param) = item {
                            if let Some((key, val)) = parse_cerberus_parameter_value(param) {
                                text = text.replace(&format!("{{{key}}}"), &val);
                            }
                        }
                    }
                }
                Some(DynamicValue::Message(param)) => {
                    if let Some((key, val)) = parse_cerberus_parameter_value(param) {
                        text = text.replace(&format!("{{{key}}}"), &val);
                    }
                }
                _ => {}
            }
            pb::message("eve.localization.cerberus.api.GetLocalizedTextResponse")
                .set("text", &text)
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 26. eve.killmail.Attributes
    router.register_handler(
        "eve.killmail.Attributes",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            pb::message("eve.killmail.Attributes")
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 27. eve.id.api.GetRequest & GetByUserRequest
    let db_id_api = Arc::clone(&db);
    router.register_handler(
        "eve.id.api.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_uid = ctx
                .effective_user_id()
                .filter(|&v| v > 0)
                .map(|v| v as u64)
                .ok_or_else(|| (STATUS_UNAUTHORIZED, "Authenticated user session required".into()))?;
            let target_uid = match dyn_msg.get(1) {
                Some(DynamicValue::Message(id_msg)) => match id_msg.get(1) {
                    Some(DynamicValue::Bytes(raw)) if !raw.is_empty() => std::str::from_utf8(raw)
                        .ok()
                        .and_then(|s| s.strip_prefix("usr-"))
                        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
                        .filter(|&u| u > 0)
                        .ok_or_else(|| (STATUS_BAD_REQUEST, "Invalid EVE ID UUID".into()))?,
                    _ => caller_uid,
                },
                _ => caller_uid,
            };
            if target_uid != caller_uid && require_admin_role(ctx).is_err() {
                return Err((STATUS_FORBIDDEN, "Not authorized to read another account's EVE ID".into()));
            }
            let exists = db_id_api.with_snapshot(|snap| {
                snap.accounts.values().any(|a| a.user_id == target_uid)
            });
            if !exists {
                return Err((STATUS_NOT_FOUND, format!("Account for user {target_uid} not found")));
            }
            pb::message("eve.id.api.GetResponse")
                .set_with("id", |attrs| {
                    attrs.add_with("users", |u| u.set("sequential", target_uid as i32))
                })
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    let db_id_by_user = Arc::clone(&db);
    router.register_handler(
        "eve.id.api.GetByUserRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_uid = ctx.effective_user_id().filter(|&v| v > 0).map(|v| v as u64);
            let is_admin = require_admin_role(ctx).is_ok();
            let Some(default_uid) = caller_uid else {
                return Err((STATUS_UNAUTHORIZED, "Authenticated user session required".into()));
            };
            let uid = if dyn_msg.get(1).is_some() {
                let parsed = extract_u64_from_field(dyn_msg, 1);
                if parsed == 0 {
                    return Err((STATUS_BAD_REQUEST, "Invalid user ID".into()));
                }
                parsed
            } else {
                default_uid
            };
            if uid != default_uid && !is_admin {
                return Err((STATUS_FORBIDDEN, "Not authorized to query another user's EVE ID".into()));
            }
            let exists = db_id_by_user.with_snapshot(|snap| {
                snap.accounts.values().any(|a| a.user_id == uid)
            });
            if !exists {
                return Err((STATUS_NOT_FOUND, format!("User {uid} not found")));
            }
            let uuid_str = format!("usr-{uid:016x}");
            pb::message("eve.id.api.GetByUserResponse")
                .set("id.uuid", uuid_str.into_bytes())
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 28. eve.id.legacy.api.GetUserRequest
    let db_id_legacy = Arc::clone(&db);
    router.register_handler(
        "eve.id.legacy.api.GetUserRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_uid = ctx.effective_user_id().filter(|&v| v > 0).map(|v| v as u64);
            let is_admin = require_admin_role(ctx).is_ok();
            if caller_uid.is_none() && !is_admin {
                return Err((STATUS_UNAUTHORIZED, "Authenticated user session required".into()));
            }
            let uid = if dyn_msg.get(1).is_some() {
                let parsed = extract_u64_from_field(dyn_msg, 1);
                if parsed == 0 {
                    return Err((STATUS_BAD_REQUEST, "Invalid user ID".into()));
                }
                parsed
            } else {
                caller_uid.ok_or_else(|| (STATUS_BAD_REQUEST, "Missing user ID".into()))?
            };
            if caller_uid != Some(uid) && !is_admin {
                return Err((
                    STATUS_FORBIDDEN,
                    "Not authorized to inspect another user's legacy account".into(),
                ));
            }
            let username = db_id_legacy
                .with_snapshot(|snap| {
                    snap.accounts
                        .values()
                        .find(|a| a.user_id == uid)
                        .map(|a| a.username.clone())
                })
                .ok_or_else(|| (STATUS_NOT_FOUND, format!("User {uid} not found")))?;
            pb::message("eve.id.legacy.api.GetUserResponse")
                .set("username", &username)
                .set("no_email", true)
                .set("eula_agreed", true)
                .set("no_full_name", true)
                .set("no_birthdate", true)
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );
}

#[cfg(test)]
#[path = "quasar_economy_planetary_industry_tests.rs"]
mod quasar_economy_planetary_industry_tests;

