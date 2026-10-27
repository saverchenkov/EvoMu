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
//! - `eve.asteroidbelt.*` (`GetRequest`, `GetBeltsOfPlanetRequest`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{PublicGatewayRouter, STATUS_FORBIDDEN, STATUS_UNAUTHORIZED};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry,
    QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::Arc;

fn encode_identifier(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut id_buf = Vec::new();
    encode_varint_field(1, id, &mut id_buf);
    encode_bytes_field(field_num, &id_buf, out);
}

fn encode_currency(field_num: u32, isk_cents: i64, out: &mut Vec<u8>) {
    let mut cur_buf = Vec::new();
    encode_varint_field(1, (isk_cents.max(0) / 100) as u64, &mut cur_buf);
    encode_bytes_field(field_num, &cur_buf, out);
}

fn encode_timestamp(field_num: u32, secs: i64, out: &mut Vec<u8>) {
    let ts = GoogleTimestamp {
        seconds: secs,
        nanos: 0,
    };
    encode_bytes_field(field_num, &ts.encode_proto(), out);
}

fn extract_u64_from_field(dyn_msg: &DynamicQuasarMessage, field_num: u32) -> u64 {
    match dyn_msg.get(field_num) {
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
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
    let mut roles = if caller_corp == target_corp { ctx_roles } else { 0 };
    if let Some(corp) = snap.corporations.get(&target_corp) {
        if corp.ceo_id == effective_cid {
            return Ok(target_corp);
        }
        roles |= corp.member_roles.get(&effective_cid).copied().unwrap_or(0);
    }
    if let Some(ra) = snap.corp_role_assignments.get(&(target_corp, effective_cid)) {
        roles |= ra.roles;
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
    _sde: Arc<SdeRegistry>,
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

            let mut out = Vec::new();
            for e in entries.iter().take(200) {
                let mut entry = Vec::new();
                encode_identifier(1, e.entry_id as u64, &mut entry);

                let mut attrs = Vec::new();
                encode_timestamp(2, 1_760_000_000, &mut attrs);
                encode_currency(4, e.amount.0, &mut attrs);
                encode_currency(5, e.balance_after.0, &mut attrs);
                encode_string_field(6, &e.reason, &mut attrs);
                encode_bytes_field(2, &attrs, &mut entry);

                encode_bytes_field(1, &entry, &mut out);
            }

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.accounting.character.journal.GetCharacterEntriesResponse"
                ),
                value: out,
            })
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

            let mut out = Vec::new();
            for e in entries.iter().take(200) {
                let mut entry = Vec::new();
                encode_identifier(1, e.entry_id as u64, &mut entry);
                let mut attrs = Vec::new();
                encode_timestamp(2, 1_760_000_000, &mut attrs);
                encode_currency(4, e.amount.0, &mut attrs);
                encode_currency(5, e.balance_after.0, &mut attrs);
                encode_bytes_field(2, &attrs, &mut entry);
                encode_bytes_field(1, &entry, &mut out);
            }

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.accounting.corporation.journal.GetCorporationEntriesResponse"
                ),
                value: out,
            })
        }),
    );

    // 3. eve.accounting.entrytype.GetAllRequest
    router.register_handler(
        "eve.accounting.entrytype.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            let sample_types = [
                (1, "Player Trading", "Transfer between players"),
                (2, "Market Transaction", "Market purchase or sale"),
                (10, "Bounty Prize", "NPC bounty reward"),
                (42, "Industry Job Fee", "Installation cost"),
            ];
            for (id, name, desc) in sample_types {
                let mut et = Vec::new();
                encode_identifier(1, id, &mut et);
                let mut attrs = Vec::new();
                encode_string_field(1, name, &mut attrs);
                encode_string_field(2, desc, &mut attrs);
                encode_bytes_field(2, &attrs, &mut et);
                encode_bytes_field(1, &et, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.accounting.entrytype.GetAllResponse"
                ),
                value: out,
            })
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
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.loyaltypoints.assetholding.operation.api.WithdrawResponse"
                ),
                value: Vec::new(),
            })
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
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.loyaltypoints.assetholding.operation.api.DepositResponse"
                ),
                value: Vec::new(),
            })
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
                            && (c.issuer_corp_id == caller_corp || c.assignee_id == caller_corp))
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

            let mut attrs = Vec::new();
            if let Some(c) = contract {
                let mut issuer = Vec::new();
                encode_identifier(1, c.issuer_id as u64, &mut issuer);
                encode_bytes_field(1, &issuer, &mut attrs);
                encode_varint_field(2, if c.availability == "Public" { 1 } else { 0 }, &mut attrs);
                encode_timestamp(3, 1_760_000_000, &mut attrs);
                encode_timestamp(4, c.expiration_unix as i64, &mut attrs);
                encode_currency(8, c.price.0, &mut attrs);
                encode_currency(10, c.reward.0, &mut attrs);
            } else {
                encode_varint_field(2, 1, &mut attrs); // public
                encode_timestamp(3, 1_760_000_000, &mut attrs);
                encode_currency(8, 1_000_000, &mut attrs);
            }

            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.contract.item_exchange.GetResponse"
                ),
                value: out,
            })
        }),
    );

    // 7. eve.character.industry.job.GetRequest
    router.register_handler(
        "eve.character.industry.job.GetRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            let mut job_entry = Vec::new();
            encode_identifier(1, 80001, &mut job_entry);
            encode_bytes_field(1, &job_entry, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.character.industry.job.GetResponse"),
                value: out,
            })
        }),
    );
    router.register_handler(
        "eve.character.industry.job.GetActiveRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            let mut job_entry = Vec::new();
            encode_identifier(1, 80001, &mut job_entry);
            encode_bytes_field(1, &job_entry, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.character.industry.job.GetActiveResponse"
                ),
                value: out,
            })
        }),
    );
    router.register_handler(
        "eve.character.industry.job.GetCompletedRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            let mut job_entry = Vec::new();
            encode_identifier(1, 80001, &mut job_entry);
            encode_bytes_field(1, &job_entry, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.character.industry.job.GetCompletedResponse"
                ),
                value: out,
            })
        }),
    );
    router.register_handler(
        "eve.character.industry.blueprints.GetRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.character.industry.blueprints.GetResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 8. eve.industry.facility.GetRequest
    router.register_handler(
        "eve.industry.facility.GetRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            let mut fac = Vec::new();
            encode_identifier(1, 60_003_760, &mut fac); // Jita 4-4 station
            let mut attrs = Vec::new();
            encode_identifier(1, 1000002, &mut attrs); // corp
            encode_identifier(2, 10000002, &mut attrs); // region
            encode_identifier(3, 30000142, &mut attrs); // solar system
            encode_varint_field(5, 1, &mut attrs); // online
            encode_bytes_field(2, &attrs, &mut fac);
            encode_bytes_field(1, &fac, &mut out);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.industry.facility.GetResponse"),
                value: out,
            })
        }),
    );

    // 8. eve.industry.system.GetCostIndicesRequest
    router.register_handler(
        "eve.industry.system.GetCostIndicesRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            let mut idx = Vec::new();
            encode_identifier(1, 30000142, &mut idx); // Jita
            encode_bytes_field(1, &idx, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.industry.system.GetCostIndicesResponse"
                ),
                value: out,
            })
        }),
    );

    // 9. eve.planet.GetRequest
    router.register_handler(
        "eve.planet.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let planet_id = extract_u64_from_field(dyn_msg, 1);
            let mut attrs = Vec::new();
            encode_identifier(1, 30000142, &mut attrs); // solar system
            encode_string_field(3, &format!("Planet #{planet_id}"), &mut attrs);
            encode_identifier(5, 11, &mut attrs); // planet type Temperate

            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.planet.GetResponse"),
                value: out,
            })
        }),
    );

    // 10. eve.planet.GetPlanetsInSolarSystemRequest
    router.register_handler(
        "eve.planet.GetPlanetsInSolarSystemRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let sys_id = extract_u64_from_field(dyn_msg, 1);
            let mut out = Vec::new();
            for i in 1..=8 {
                let planet_id = sys_id * 100 + i;
                encode_identifier(1, planet_id, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.planet.GetPlanetsInSolarSystemResponse"
                ),
                value: out,
            })
        }),
    );

    // 11. eve.planet.skyhook.admin.api.ChangeOwnerRequest
    router.register_handler(
        "eve.planet.skyhook.admin.api.ChangeOwnerRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.planet.skyhook.admin.api.ChangeOwnerResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 12. eve.planet.skyhook.api.GetReinforcementExitHourRequest
    router.register_handler(
        "eve.planet.skyhook.api.GetReinforcementExitHourRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_varint_field(1, 18, &mut out); // 18:00 UTC
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.planet.skyhook.api.GetReinforcementExitHourResponse"
                ),
                value: out,
            })
        }),
    );

    // 13. eve.planet.skyhook.api.GetStateRequest
    router.register_handler(
        "eve.planet.skyhook.api.GetStateRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_varint_field(1, 1, &mut out); // Online
            encode_varint_field(2, 1, &mut out); // no_timer = true
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.planet.skyhook.api.GetStateResponse"
                ),
                value: out,
            })
        }),
    );

    // 14. eve.planetinteraction.collector.GetRequest
    router.register_handler(
        "eve.planetinteraction.collector.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let col_id = extract_u64_from_field(dyn_msg, 1);
            let cid = ctx.effective_character_id().unwrap_or(0) as u64;
            let mut attrs = Vec::new();
            encode_identifier(1, cid, &mut attrs);
            encode_identifier(2, col_id.max(1), &mut attrs);
            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.planetinteraction.collector.GetResponse"
                ),
                value: out,
            })
        }),
    );

    // 15. eve.planetinteraction.factory.GetRequest
    router.register_handler(
        "eve.planetinteraction.factory.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let fact_id = extract_u64_from_field(dyn_msg, 1);
            let cid = ctx.effective_character_id().unwrap_or(0) as u64;
            let mut attrs = Vec::new();
            encode_identifier(1, cid, &mut attrs);
            encode_identifier(2, fact_id.max(1), &mut attrs);
            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.planetinteraction.factory.GetResponse"
                ),
                value: out,
            })
        }),
    );

    // 16. eve.planetinteraction.schematic.GetRequest
    router.register_handler(
        "eve.planetinteraction.schematic.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let sch_id = extract_u64_from_field(dyn_msg, 1);
            let mut attrs = Vec::new();
            encode_varint_field(1, 3600, &mut attrs); // 1h cycle
            encode_string_field(2, &format!("PI Schematic #{sch_id}"), &mut attrs);
            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.planetinteraction.schematic.GetResponse"
                ),
                value: out,
            })
        }),
    );

    // 17. eve.planetinteraction.storage.GetRequest
    router.register_handler(
        "eve.planetinteraction.storage.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let store_id = extract_u64_from_field(dyn_msg, 1);
            let cid = ctx.effective_character_id().unwrap_or(0) as u64;
            let mut attrs = Vec::new();
            encode_identifier(1, cid, &mut attrs);
            encode_identifier(2, store_id.max(1), &mut attrs);
            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.planetinteraction.storage.GetResponse"
                ),
                value: out,
            })
        }),
    );

    // 18. eve.moon.GetRequest
    router.register_handler(
        "eve.moon.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let moon_id = extract_u64_from_field(dyn_msg, 1);
            let mut attrs = Vec::new();
            encode_string_field(1, &format!("Moon #{moon_id}"), &mut attrs);
            encode_identifier(3, 30000142, &mut attrs); // solar system
            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.moon.GetResponse"),
                value: out,
            })
        }),
    );

    // 19. eve.moon.GetMoonsOfPlanetRequest
    router.register_handler(
        "eve.moon.GetMoonsOfPlanetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let planet_id = extract_u64_from_field(dyn_msg, 1);
            let mut out = Vec::new();
            for i in 1..=4 {
                encode_identifier(1, planet_id * 10 + i, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.moon.GetMoonsOfPlanetResponse"),
                value: out,
            })
        }),
    );

    // 20. eve.asteroidbelt.GetRequest
    router.register_handler(
        "eve.asteroidbelt.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let belt_id = extract_u64_from_field(dyn_msg, 1);
            let mut attrs = Vec::new();
            encode_string_field(1, &format!("Asteroid Belt #{belt_id}"), &mut attrs);
            encode_identifier(3, 30000142, &mut attrs); // solar system
            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.asteroidbelt.GetResponse"),
                value: out,
            })
        }),
    );

    // 21. eve.asteroidbelt.GetBeltsOfPlanetRequest
    router.register_handler(
        "eve.asteroidbelt.GetBeltsOfPlanetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let planet_id = extract_u64_from_field(dyn_msg, 1);
            let mut out = Vec::new();
            for i in 1..=2 {
                encode_identifier(1, planet_id * 100 + i, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.asteroidbelt.GetBeltsOfPlanetResponse"
                ),
                value: out,
            })
        }),
    );
}
