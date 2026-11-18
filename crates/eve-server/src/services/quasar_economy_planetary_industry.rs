//! `quasar_economy_planetary_industry` — Quasar V24.01 Accounting, LP, Contracts, Industry, Planets & PI (`Q-6`).
//!
//! Implements backend handlers for:
//! - `eve.accounting.*` (`GetCharacterEntriesRequest`, `GetCorporationEntriesRequest`, `GetAllRequest`);
//! - `eve.loyaltypoints.*` (`WithdrawRequest`, `DepositRequest`);
//! - `eve.contract.item_exchange.*` (`GetRequest`);
//! - `eve.industry.*` (`GetRequest` for facilities, `GetCostIndicesRequest`);
//! - `eve.planet.*` (`GetRequest`, `GetPlanetsInSolarSystemRequest`);
//! - `eve.planet.skyhook.*` (`ChangeOwnerRequest`, `GetReinforcementExitHourRequest`, `GetStateRequest`);
//! - `eve.planetinteraction.*` (`collector.GetRequest`, `factory.GetRequest`, `schematic.GetRequest`, `storage.GetRequest`);
//! - `eve.moon.*` (`GetRequest`, `GetMoonsOfPlanetRequest`);
//! - `eve.asteroidbelt.*` (`GetRequest`, `GetBeltsOfPlanetRequest`);
//! - `eve.bloodline.*` (`GetAllRequest`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
    STATUS_INTERNAL_ERROR, STATUS_NOT_FOUND, STATUS_UNAUTHORIZED,
};
use crate::services::industry_pi::{ATTR_BLUEPRINT_ME, ATTR_BLUEPRINT_TE};
use eve_db::WorldRepository;
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, QuasarDescriptorRegistry,
};
use eve_sde::{CelestialInfo, SdeRegistry};
use std::collections::BTreeMap;
use std::sync::Arc;

fn caller_solar_system(ctx: &AuthoritativeContext, db: &WorldRepository) -> u64 {
    ctx.effective_character_id()
        .and_then(|cid| {
            db.with_snapshot(|s| {
                s.characters.get(&(cid as i64)).map(|c| c.solar_system_id as u64)
            })
        })
        .unwrap_or(0)
}

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

fn extract_u64_from_field(dyn_msg: &DynamicQuasarMessage, field_num: u32) -> u64 {
    match dyn_msg.get(field_num) {
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::Message(inner)) => match inner.get(1) {
                Some(DynamicValue::Uint64(n)) => *n,
                Some(DynamicValue::Uint32(n)) => *n as u64,
                Some(DynamicValue::Int64(n)) => *n as u64,
                Some(DynamicValue::Int32(n)) => *n as u64,
                _ => 0,
            },
            Some(DynamicValue::Uint64(n)) => *n,
            Some(DynamicValue::Uint32(n)) => *n as u64,
            Some(DynamicValue::Int64(n)) => *n as u64,
            Some(DynamicValue::Int32(n)) => *n as u64,
            _ => 0,
        },
        Some(DynamicValue::Uint64(n)) => *n,
        Some(DynamicValue::Uint32(n)) => *n as u64,
        Some(DynamicValue::Int64(n)) => *n as u64,
        Some(DynamicValue::Int32(n)) => *n as u64,
        _ => 0,
    }
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
    let ctx_roles = ctx
        .identity
        .as_ref()
        .and_then(|id| id.roles.as_ref().map(|r| r.bitmask))
        .unwrap_or(0);
    let session_roles = if caller_corp == target_corp { ctx_roles } else { 0 };
    let (is_ceo, eff_roles) =
        snap.effective_corp_roles_at(target_corp, effective_cid, None, session_roles, 0);
    let roles = eff_roles | session_roles;
    if is_ceo {
        return Ok(target_corp);
    }
    if (roles & allowed_roles) == 0 {
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

            let entries = db_clone.with_snapshot(|snap| {
                snap.wallet_journal
                    .iter()
                    .filter(|e| e.character_id == target_cid)
                    .cloned()
                    .collect::<Vec<_>>()
            });

            let mut resp = pb::message(
                "eve.accounting.character.journal.GetCharacterEntriesResponse",
            );
            for e in entries.iter().take(200) {
                resp = resp.add_with("entries", |entry| {
                    entry
                        .set("id.transaction", e.entry_id as u64)
                        .set_with("attributes", |attrs| {
                            attrs
                                .set("date.seconds", 1_760_000_000i64)
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
            let entries = db_clone.with_snapshot(|snap| {
                let target_corp = authorize_corp_financial_access(ctx, snap, req_corp_id)?;
                Ok::<_, (u32, String)>(
                    snap.wallet_journal
                        .iter()
                        .filter(|e| {
                            e.character_id == target_corp
                                || snap.wallet_journal_meta.get(&e.entry_id).is_some_and(|m| {
                                    m.first_party_id == target_corp
                                        || m.second_party_id == target_corp
                                })
                        })
                        .cloned()
                        .collect::<Vec<_>>(),
                )
            })?;

            let mut resp = pb::message(
                "eve.accounting.corporation.journal.GetCorporationEntriesResponse",
            );
            for e in entries.iter().take(200) {
                resp = resp.add_with("entries", |entry| {
                    entry
                        .set("id.transaction", e.entry_id as u64)
                        .set_with("attributes", |attrs| {
                            attrs
                                .set("date.seconds", 1_760_000_000i64)
                                .set_with("amount", |c| set_isk_currency(c, e.amount.0))
                                .set_with("account_balance", |c| {
                                    set_isk_currency(c, e.balance_after.0)
                                })
                        })
                });
            }

            resp.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 3. eve.accounting.entrytype.GetAllRequest
    router.register_handler(
        "eve.accounting.entrytype.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let sample_types = [
                (1u32, "Player Trading"),
                (2u32, "Market Transaction"),
                (10u32, "Bounty Prize"),
                (42u32, "Industry Job Fee"),
            ];
            let mut resp = pb::message("eve.accounting.entrytype.GetAllResponse");
            for (id, name) in sample_types {
                resp = resp.add_with("entry_types", |et| {
                    et.set("id.sequential", id).set("attributes.name", name)
                });
            }
            resp.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 4. eve.loyaltypoints.assetholding.operation.api.WithdrawRequest
    router.register_handler(
        "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            if ctx.effective_character_id().is_none()
                && crate::quasar_gateway::require_admin_role(ctx).is_err()
            {
                return Err((
                    crate::quasar_gateway::STATUS_FORBIDDEN,
                    "Authentication required for loyaltypoints assetholding WithdrawRequest".into(),
                ));
            }
            pb::message("eve.loyaltypoints.assetholding.operation.api.WithdrawResponse")
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 5. eve.loyaltypoints.assetholding.operation.api.DepositRequest
    router.register_handler(
        "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            if ctx.effective_character_id().is_none()
                && crate::quasar_gateway::require_admin_role(ctx).is_err()
            {
                return Err((
                    crate::quasar_gateway::STATUS_FORBIDDEN,
                    "Authentication required for loyaltypoints assetholding DepositRequest".into(),
                ));
            }
            pb::message("eve.loyaltypoints.assetholding.operation.api.DepositResponse")
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 6. eve.contract.item_exchange.GetRequest
    let db_clone = Arc::clone(&db);
    router.register_handler(
        "eve.contract.item_exchange.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let effective_cid = ctx.effective_character_id().unwrap_or(0) as i64;
            let contract_id = extract_u64_from_field(dyn_msg, 1) as i64;
            let contract = db_clone.with_snapshot(|snap| -> Result<Option<eve_db::ContractRecord>, (u32, String)> {
                let Some(c) = snap.contracts.get(&contract_id).cloned() else {
                    return Ok(None);
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
                Ok(Some(c))
            })?;

            pb::message("eve.contract.item_exchange.GetResponse")
                .set_with("contract", |attrs| {
                    if let Some(c) = contract {
                        attrs
                            .set("issuer.character.sequential", c.issuer_id as u32)
                            .set("available_to_public", c.availability == "Public")
                            .set("issued_time.seconds", 1_760_000_000i64)
                            .set("expiry_time.seconds", c.expiration_unix as i64)
                            .set_with("price", |p| set_isk_currency(p, c.price.0))
                            .set_with("reward", |r| set_isk_currency(r, c.reward.0))
                    } else {
                        attrs
                            .set("available_to_public", true)
                            .set("issued_time.seconds", 1_760_000_000i64)
                            .set_with("price", |p| set_isk_currency(p, 1_000_000))
                    }
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
            let (station_id, caller_sys) = caller_station_and_solar_system(ctx, &db_fac);
            let (corp_id, region_id, sys_id, facility_type_id) = if station_id > 0 {
                if let Some(st) = sde_fac.station(station_id as i64) {
                    (
                        st.owner_corporation_id as u32,
                        st.region_id as u64,
                        st.solar_system_id as u64,
                        Some(st.station_type_id),
                    )
                } else {
                    let st_info = db_fac.with_snapshot(|snap| {
                        snap.structures
                            .get(&(station_id as i64))
                            .map(|s| (s.owner_corp_id, s.solar_system_id, s.type_id))
                    });
                    if let Some((owner_corp, s_sys, stype)) = st_info {
                        let reg = sde_fac
                            .solar_system(s_sys)
                            .map(|ss| ss.region_id as u64)
                            .unwrap_or(10_000_002);
                        (owner_corp as u32, reg, s_sys as u64, Some(stype))
                    } else {
                        (1_000_002u32, 10_000_002u64, caller_sys, None)
                    }
                }
            } else {
                (1_000_002u32, 10_000_002u64, caller_sys, None)
            };
            pb::message("eve.industry.facility.GetResponse")
                .add_with("facilities", |fac| {
                    fac.set("id.sequential", station_id)
                        .set_with("attributes", |attrs| {
                            attrs
                                .set("corporation.sequential", corp_id)
                                .set("region.sequential", region_id)
                                .set("solar_system.sequential", sys_id)
                                .set_opt("type.sequential", facility_type_id)
                                .set("online", true)
                        })
                })
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 8b. eve.industry.system.GetCostIndicesRequest
    let db_cost = Arc::clone(&db);
    let sde_cost = Arc::clone(&sde);
    router.register_handler(
        "eve.industry.system.GetCostIndicesRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let caller_sys = caller_solar_system(ctx, &db_cost) as u32;
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
                    let map = out.entry(sid).or_default();
                    for (act, sw) in acts {
                        let uw = univ_work.get(&act).copied().unwrap_or(0);
                        let sci = if uw > 0 && sw > 0 {
                            ((sw as f64) / (uw as f64)).sqrt().clamp(0.001, 0.50) as f32
                        } else {
                            0.01f32
                        };
                        map.insert(act, sci);
                    }
                }
                out.entry(caller_sys)
                    .or_insert_with(|| BTreeMap::from([(1i32, 0.01f32)]));
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
    let db_planet = Arc::clone(&db);
    let sde_planet = Arc::clone(&sde);
    router.register_handler(
        "eve.planet.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let planet_id = extract_u64_from_field(dyn_msg, 1);
            let sde_match =
                lookup_sde_celestial_with_group(&sde_planet, planet_id as i64, 7, "Planet")?;
            let fallback_sys = caller_solar_system(ctx, &db_planet);
            pb::message("eve.planet.GetResponse")
                .set_with("planet", |attrs| {
                    if let Some(c) = sde_match {
                        encode_celestial_attributes(
                            attrs,
                            "solarsystem.sequential",
                            &c.name,
                            c.solar_system_id as u64,
                            Some([c.position.x, c.position.y, c.position.z]),
                        )
                        .set("type.sequential", c.type_id as u64)
                    } else {
                        encode_celestial_attributes(
                            attrs,
                            "solarsystem.sequential",
                            &format!("Planet #{planet_id}"),
                            fallback_sys,
                            None,
                        )
                        .set("type.sequential", 11u64)
                    }
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
            let mut resp = pb::message("eve.planet.GetPlanetsInSolarSystemResponse");
            if sys_id > 0 && sde_sys_planets.solar_systems.contains_key(&(sys_id as u32)) {
                let mut planets: Vec<&CelestialInfo> = sde_sys_planets
                    .celestials_in_system_slice(sys_id as u32)
                    .iter()
                    .filter(|c| c.group_id == 7)
                    .collect();
                planets.sort_by_key(|c| (c.celestial_index.unwrap_or(i32::MAX), c.celestial_id));
                for p in planets {
                    resp = resp.add_with("planets", |pl| pl.set("sequential", p.celestial_id as u64));
                }
            } else {
                for i in 1..=8 {
                    let planet_id = sys_id * 100 + i;
                    resp = resp.add_with("planets", |p| p.set("sequential", planet_id));
                }
            }
            resp.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 11. eve.planet.skyhook.admin.api.ChangeOwnerRequest
    router.register_handler(
        "eve.planet.skyhook.admin.api.ChangeOwnerRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            pb::message("eve.planet.skyhook.admin.api.ChangeOwnerResponse")
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 12. eve.planet.skyhook.api.GetReinforcementExitHourRequest
    router.register_handler(
        "eve.planet.skyhook.api.GetReinforcementExitHourRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            pb::message("eve.planet.skyhook.api.GetReinforcementExitHourResponse")
                .set("reinforcement_exit_hour", 18u32) // 18:00 UTC
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 13. eve.planet.skyhook.api.GetStateRequest
    router.register_handler(
        "eve.planet.skyhook.api.GetStateRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            pb::message("eve.planet.skyhook.api.GetStateResponse")
                .set("state", pb::enum_name("STATE_SHIELD_VULNERABLE"))
                .set("no_timer", true)
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 14. eve.planetinteraction.collector.GetRequest
    router.register_handler(
        "eve.planetinteraction.collector.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let col_id = extract_u64_from_field(dyn_msg, 1);
            let cid = ctx.effective_character_id().unwrap_or(0) as u32;
            pb::message("eve.planetinteraction.collector.GetResponse")
                .set_with("collector", |attrs| {
                    attrs
                        .set("character.sequential", cid)
                        .set("planet.sequential", col_id.max(1))
                })
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 15. eve.planetinteraction.factory.GetRequest
    router.register_handler(
        "eve.planetinteraction.factory.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let fact_id = extract_u64_from_field(dyn_msg, 1);
            let cid = ctx.effective_character_id().unwrap_or(0) as u32;
            pb::message("eve.planetinteraction.factory.GetResponse")
                .set_with("factory", |attrs| {
                    attrs
                        .set("character.sequential", cid)
                        .set("planet.sequential", fact_id.max(1))
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
            let (cycle_time_sec, schematic_name) = sde_schematic
                .planet_schematic(sch_id as u32)
                .map(|s| (s.cycle_time.min(u32::MAX as u64) as u32, s.name.clone()))
                .unwrap_or_else(|| (3600u32, format!("PI Schematic #{sch_id}")));
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
    router.register_handler(
        "eve.planetinteraction.storage.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let store_id = extract_u64_from_field(dyn_msg, 1);
            let cid = ctx.effective_character_id().unwrap_or(0) as u32;
            pb::message("eve.planetinteraction.storage.GetResponse")
                .set_with("storage", |attrs| {
                    attrs
                        .set("character.sequential", cid)
                        .set("planet.sequential", store_id.max(1))
                })
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 18. eve.moon.GetRequest
    let db_moon = Arc::clone(&db);
    let sde_moon = Arc::clone(&sde);
    router.register_handler(
        "eve.moon.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let moon_id = extract_u64_from_field(dyn_msg, 1);
            let sde_match =
                lookup_sde_celestial_with_group(&sde_moon, moon_id as i64, 8, "Moon")?;
            let fallback_sys = caller_solar_system(ctx, &db_moon);
            pb::message("eve.moon.GetResponse")
                .set_with("attributes", |attrs| {
                    if let Some(c) = sde_match {
                        encode_celestial_attributes(
                            attrs,
                            "solar_system.sequential",
                            &c.name,
                            c.solar_system_id as u64,
                            Some([c.position.x, c.position.y, c.position.z]),
                        )
                    } else {
                        encode_celestial_attributes(
                            attrs,
                            "solar_system.sequential",
                            &format!("Moon #{moon_id}"),
                            fallback_sys,
                            None,
                        )
                    }
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
            let mut resp = pb::message("eve.moon.GetMoonsOfPlanetResponse");
            if let Some(moons) = children_of_planet_in_sde(&sde_planet_moons, planet_id as i64, 8)? {
                for moon_id in moons {
                    resp = resp.add_with("moons", |m| m.set("sequential", moon_id as u64));
                }
            } else {
                for i in 1..=4 {
                    resp = resp.add_with("moons", |m| m.set("sequential", planet_id * 10 + i));
                }
            }
            resp.to_any().map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 20. eve.asteroidbelt.GetRequest
    let db_belt = Arc::clone(&db);
    let sde_belt = Arc::clone(&sde);
    router.register_handler(
        "eve.asteroidbelt.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let belt_id = extract_u64_from_field(dyn_msg, 1);
            let sde_match = lookup_sde_celestial_with_group(
                &sde_belt,
                belt_id as i64,
                9,
                "Asteroid belt",
            )?;
            let fallback_sys = caller_solar_system(ctx, &db_belt);
            pb::message("eve.asteroidbelt.GetResponse")
                .set_with("attributes", |attrs| {
                    if let Some(c) = sde_match {
                        encode_celestial_attributes(
                            attrs,
                            "solar_system.sequential",
                            &c.name,
                            c.solar_system_id as u64,
                            Some([c.position.x, c.position.y, c.position.z]),
                        )
                    } else {
                        encode_celestial_attributes(
                            attrs,
                            "solar_system.sequential",
                            &format!("Asteroid Belt #{belt_id}"),
                            fallback_sys,
                            None,
                        )
                    }
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
            let mut resp = pb::message("eve.asteroidbelt.GetBeltsOfPlanetResponse");
            if let Some(belts) =
                children_of_planet_in_sde(&sde_planet_belts, planet_id as i64, 9)?
            {
                for belt_id in belts {
                    resp = resp.add_with("belts", |b| b.set("sequential", belt_id as u64));
                }
            } else {
                for i in 1..=2 {
                    resp = resp.add_with("belts", |b| b.set("sequential", planet_id * 100 + i));
                }
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

    // 23. eve.isk.wallet.api.TransferByOwnerRequest
    let db_isk_xfer = Arc::clone(&db);
    let isk_notices = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve.isk.wallet.api.TransferByOwnerRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(caller_u32) = ctx.effective_character_id().filter(|&v| v > 0) else {
                return Err((STATUS_UNAUTHORIZED, "Active character session required".into()));
            };
            let caller = caller_u32 as i64;
            let amount = match dyn_msg.get(5) {
                Some(DynamicValue::Message(m)) => match m.get(1) {
                    Some(DynamicValue::Int64(i)) => *i,
                    Some(DynamicValue::Uint64(u)) => *u as i64,
                    _ => 10_000,
                },
                _ => 10_000,
            };
            let amount_cents = amount.max(0) * 100;
            db_isk_xfer.try_with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&caller) {
                    if ch.balance.0 < amount_cents {
                        return Err((STATUS_BAD_REQUEST, "Insufficient ISK".into()));
                    }
                    ch.balance.0 -= amount_cents;
                }
                Ok(())
            })?;
            match pb::message("eve.isk.wallet.api.GrantCommanded")
                .set("from_wallet_owner", true)
                .set_with("amount", |a| a.set("units", amount.max(0)).set("nanos", 0i32))
                .to_any()
            {
                Ok(notice) => isk_notices
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((NoticeScope::Character(caller as u32), notice)),
                Err(e) => tracing::warn!(
                    character_id = caller,
                    error = %e,
                    "failed to encode GrantCommanded notice"
                ),
            }
            pb::message("eve.isk.wallet.api.TransferByOwnerResponse")
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 24. eve.localization.message.api.GetLocalizedTextRequest
    router.register_handler(
        "eve.localization.message.api.GetLocalizedTextRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let seq = match dyn_msg.get(1) {
                Some(DynamicValue::Message(m)) => match m.get(1) {
                    Some(DynamicValue::Uint32(n)) => *n as u64,
                    Some(DynamicValue::Uint64(n)) => *n,
                    _ => 1,
                },
                _ => 1,
            };
            pb::message("eve.localization.message.api.GetLocalizedTextResponse")
                .set("text", format!("LocalizedText#{seq}"))
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    // 25. eve.localization.cerberus.api.GetLocalizedTextRequest
    router.register_handler(
        "eve.localization.cerberus.api.GetLocalizedTextRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            pb::message("eve.localization.cerberus.api.GetLocalizedTextResponse")
                .set("text", "CerberusLocalizedText")
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
    router.register_handler(
        "eve.id.api.GetRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as i32;
            pb::message("eve.id.api.GetResponse")
                .set_with("id", |attrs| attrs.add_with("users", |u| u.set("sequential", uid)))
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );

    router.register_handler(
        "eve.id.api.GetByUserRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uid = match dyn_msg.get(1) {
                Some(DynamicValue::Message(m)) => match m.get(1) {
                    Some(DynamicValue::Uint32(n)) => *n as u64,
                    Some(DynamicValue::Uint64(n)) => *n,
                    _ => ctx.effective_user_id().unwrap_or(1) as u64,
                },
                _ => ctx.effective_user_id().unwrap_or(1) as u64,
            };
            let uuid_str = format!("usr-{:016x}", uid);
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
            let uid = match dyn_msg.get(1) {
                Some(DynamicValue::Message(m)) => match m.get(1) {
                    Some(DynamicValue::Uint32(n)) => *n as u64,
                    Some(DynamicValue::Uint64(n)) => *n,
                    _ => ctx.effective_user_id().unwrap_or(1) as u64,
                },
                _ => ctx.effective_user_id().unwrap_or(1) as u64,
            };
            let username = db_id_legacy.with_snapshot(|snap| {
                snap.accounts.values().find(|a| a.user_id == uid).map(|a| a.username.clone())
            }).unwrap_or_else(|| format!("pilot_{uid}"));
            pb::message("eve.id.legacy.api.GetUserResponse")
                .set("username", &username)
                .set_with("email", |email| {
                    email
                        .set("address", format!("{username}@ccpgames.test"))
                        .set("verified", true)
                })
                .set("eula_agreed", true)
                .to_any()
                .map_err(|e| (STATUS_INTERNAL_ERROR, e.to_string()))
        }),
    );
}

#[cfg(test)]
#[path = "quasar_economy_planetary_industry_tests.rs"]
mod quasar_economy_planetary_industry_tests;

