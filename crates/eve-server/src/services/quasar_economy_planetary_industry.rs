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
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, QuasarDescriptorRegistry,
};
use eve_sde::SdeRegistry;
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

            Ok(resp.must_to_any())
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

            Ok(resp.must_to_any())
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
            Ok(resp.must_to_any())
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
            Ok(pb::message("eve.loyaltypoints.assetholding.operation.api.WithdrawResponse")
                .must_to_any())
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
            Ok(pb::message("eve.loyaltypoints.assetholding.operation.api.DepositResponse")
                .must_to_any())
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

            Ok(pb::message("eve.contract.item_exchange.GetResponse")
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
                .must_to_any())
        }),
    );

    // 7. eve.character.industry.job.GetRequest
    for (req_name, resp_name) in [
        (
            "eve.character.industry.job.GetRequest",
            "eve.character.industry.job.GetResponse",
        ),
        (
            "eve.character.industry.job.GetActiveRequest",
            "eve.character.industry.job.GetActiveResponse",
        ),
        (
            "eve.character.industry.job.GetCompletedRequest",
            "eve.character.industry.job.GetCompletedResponse",
        ),
    ] {
        router.register_handler(
            req_name,
            Arc::new(move |_ctx, _any, _dyn_msg| {
                Ok(pb::message(resp_name)
                    .add_with("jobs", |j| j.set("id.sequential", 80001u32))
                    .must_to_any())
            }),
        );
    }
    router.register_handler(
        "eve.character.industry.blueprints.GetRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(pb::message("eve.character.industry.blueprints.GetResponse").must_to_any())
        }),
    );

    // 8. eve.industry.facility.GetRequest
    let db_fac = Arc::clone(&db);
    router.register_handler(
        "eve.industry.facility.GetRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let (station_id, sys_id) = caller_station_and_solar_system(ctx, &db_fac);
            Ok(pb::message("eve.industry.facility.GetResponse")
                .add_with("facilities", |fac| {
                    fac.set("id.sequential", station_id)
                        .set_with("attributes", |attrs| {
                            attrs
                                .set("corporation.sequential", 1000002u32)
                                .set("region.sequential", 10000002u64)
                                .set("solar_system.sequential", sys_id)
                                .set("online", true)
                        })
                })
                .must_to_any())
        }),
    );

    // 8. eve.industry.system.GetCostIndicesRequest
    let db_cost = Arc::clone(&db);
    router.register_handler(
        "eve.industry.system.GetCostIndicesRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let sys_id = caller_solar_system(ctx, &db_cost);
            Ok(pb::message("eve.industry.system.GetCostIndicesResponse")
                .add_with("cost_indices", |idx| idx.set("solar_system.sequential", sys_id))
                .must_to_any())
        }),
    );

    // 9. eve.planet.GetRequest
    let db_planet = Arc::clone(&db);
    router.register_handler(
        "eve.planet.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let sys_id = caller_solar_system(ctx, &db_planet);
            let planet_id = extract_u64_from_field(dyn_msg, 1);
            Ok(pb::message("eve.planet.GetResponse")
                .set_with("planet", |attrs| {
                    attrs
                        .set("solarsystem.sequential", sys_id)
                        .set("name", format!("Planet #{planet_id}"))
                        .set("type.sequential", 11u64)
                })
                .must_to_any())
        }),
    );

    // 10. eve.planet.GetPlanetsInSolarSystemRequest
    router.register_handler(
        "eve.planet.GetPlanetsInSolarSystemRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let sys_id = extract_u64_from_field(dyn_msg, 1);
            let mut resp = pb::message("eve.planet.GetPlanetsInSolarSystemResponse");
            for i in 1..=8 {
                let planet_id = sys_id * 100 + i;
                resp = resp.add_with("planets", |p| p.set("sequential", planet_id));
            }
            Ok(resp.must_to_any())
        }),
    );

    // 11. eve.planet.skyhook.admin.api.ChangeOwnerRequest
    router.register_handler(
        "eve.planet.skyhook.admin.api.ChangeOwnerRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            Ok(pb::message("eve.planet.skyhook.admin.api.ChangeOwnerResponse").must_to_any())
        }),
    );

    // 12. eve.planet.skyhook.api.GetReinforcementExitHourRequest
    router.register_handler(
        "eve.planet.skyhook.api.GetReinforcementExitHourRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(pb::message("eve.planet.skyhook.api.GetReinforcementExitHourResponse")
                .set("reinforcement_exit_hour", 18u32) // 18:00 UTC
                .must_to_any())
        }),
    );

    // 13. eve.planet.skyhook.api.GetStateRequest
    router.register_handler(
        "eve.planet.skyhook.api.GetStateRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(pb::message("eve.planet.skyhook.api.GetStateResponse")
                .set("state", pb::enum_name("STATE_SHIELD_VULNERABLE"))
                .set("no_timer", true)
                .must_to_any())
        }),
    );

    // 14. eve.planetinteraction.collector.GetRequest
    router.register_handler(
        "eve.planetinteraction.collector.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let col_id = extract_u64_from_field(dyn_msg, 1);
            let cid = ctx.effective_character_id().unwrap_or(0) as u32;
            Ok(pb::message("eve.planetinteraction.collector.GetResponse")
                .set_with("collector", |attrs| {
                    attrs
                        .set("character.sequential", cid)
                        .set("planet.sequential", col_id.max(1))
                })
                .must_to_any())
        }),
    );

    // 15. eve.planetinteraction.factory.GetRequest
    router.register_handler(
        "eve.planetinteraction.factory.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let fact_id = extract_u64_from_field(dyn_msg, 1);
            let cid = ctx.effective_character_id().unwrap_or(0) as u32;
            Ok(pb::message("eve.planetinteraction.factory.GetResponse")
                .set_with("factory", |attrs| {
                    attrs
                        .set("character.sequential", cid)
                        .set("planet.sequential", fact_id.max(1))
                })
                .must_to_any())
        }),
    );

    // 16. eve.planetinteraction.schematic.GetRequest
    router.register_handler(
        "eve.planetinteraction.schematic.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let sch_id = extract_u64_from_field(dyn_msg, 1);
            Ok(pb::message("eve.planetinteraction.schematic.GetResponse")
                .set_with("schematic", |attrs| {
                    attrs
                        .set("cycle_time_sec", 3600u32) // 1h cycle
                        .set("schematic_name", format!("PI Schematic #{sch_id}"))
                })
                .must_to_any())
        }),
    );

    // 17. eve.planetinteraction.storage.GetRequest
    router.register_handler(
        "eve.planetinteraction.storage.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let store_id = extract_u64_from_field(dyn_msg, 1);
            let cid = ctx.effective_character_id().unwrap_or(0) as u32;
            Ok(pb::message("eve.planetinteraction.storage.GetResponse")
                .set_with("storage", |attrs| {
                    attrs
                        .set("character.sequential", cid)
                        .set("planet.sequential", store_id.max(1))
                })
                .must_to_any())
        }),
    );

    // 18. eve.moon.GetRequest
    let db_moon = Arc::clone(&db);
    router.register_handler(
        "eve.moon.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let sys_id = caller_solar_system(ctx, &db_moon);
            let moon_id = extract_u64_from_field(dyn_msg, 1);
            Ok(pb::message("eve.moon.GetResponse")
                .set_with("attributes", |attrs| {
                    attrs
                        .set("name", format!("Moon #{moon_id}"))
                        .set("solar_system.sequential", sys_id)
                })
                .must_to_any())
        }),
    );

    // 19. eve.moon.GetMoonsOfPlanetRequest
    router.register_handler(
        "eve.moon.GetMoonsOfPlanetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let planet_id = extract_u64_from_field(dyn_msg, 1);
            let mut resp = pb::message("eve.moon.GetMoonsOfPlanetResponse");
            for i in 1..=4 {
                resp = resp.add_with("moons", |m| m.set("sequential", planet_id * 10 + i));
            }
            Ok(resp.must_to_any())
        }),
    );

    // 20. eve.asteroidbelt.GetRequest
    let db_belt = Arc::clone(&db);
    router.register_handler(
        "eve.asteroidbelt.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let sys_id = caller_solar_system(ctx, &db_belt);
            let belt_id = extract_u64_from_field(dyn_msg, 1);
            Ok(pb::message("eve.asteroidbelt.GetResponse")
                .set_with("attributes", |attrs| {
                    attrs
                        .set("name", format!("Asteroid Belt #{belt_id}"))
                        .set("solar_system.sequential", sys_id)
                })
                .must_to_any())
        }),
    );

    // 21. eve.asteroidbelt.GetBeltsOfPlanetRequest
    router.register_handler(
        "eve.asteroidbelt.GetBeltsOfPlanetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let planet_id = extract_u64_from_field(dyn_msg, 1);
            let mut resp = pb::message("eve.asteroidbelt.GetBeltsOfPlanetResponse");
            for i in 1..=2 {
                resp = resp.add_with("belts", |b| b.set("sequential", planet_id * 100 + i));
            }
            Ok(resp.must_to_any())
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
            Ok(resp.must_to_any())
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
            let notice = pb::message("eve.isk.wallet.api.GrantCommanded")
                .set("from_wallet_owner", true)
                .set_with("amount", |a| a.set("units", amount.max(0)).set("nanos", 0i32))
                .must_to_any();
            isk_notices
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push((NoticeScope::Character(caller as u32), notice));
            Ok(pb::message("eve.isk.wallet.api.TransferByOwnerResponse").must_to_any())
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
            Ok(pb::message("eve.localization.message.api.GetLocalizedTextResponse")
                .set("text", format!("LocalizedText#{seq}"))
                .must_to_any())
        }),
    );

    // 25. eve.localization.cerberus.api.GetLocalizedTextRequest
    router.register_handler(
        "eve.localization.cerberus.api.GetLocalizedTextRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(pb::message("eve.localization.cerberus.api.GetLocalizedTextResponse")
                .set("text", "CerberusLocalizedText")
                .must_to_any())
        }),
    );

    // 26. eve.killmail.Attributes
    router.register_handler(
        "eve.killmail.Attributes",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(pb::message("eve.killmail.Attributes").must_to_any())
        }),
    );

    // 27. eve.id.api.GetRequest & GetByUserRequest
    router.register_handler(
        "eve.id.api.GetRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as i32;
            Ok(pb::message("eve.id.api.GetResponse")
                .set_with("id", |attrs| attrs.add_with("users", |u| u.set("sequential", uid)))
                .must_to_any())
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
            Ok(pb::message("eve.id.api.GetByUserResponse")
                .set("id.uuid", uuid_str.into_bytes())
                .must_to_any())
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
            Ok(pb::message("eve.id.legacy.api.GetUserResponse")
                .set("username", &username)
                .set_with("email", |email| {
                    email
                        .set("address", format!("{username}@ccpgames.test"))
                        .set("verified", true)
                })
                .set("eula_agreed", true)
                .must_to_any())
        }),
    );
}

#[cfg(test)]
#[path = "quasar_economy_planetary_industry_tests.rs"]
mod quasar_economy_planetary_industry_tests;

