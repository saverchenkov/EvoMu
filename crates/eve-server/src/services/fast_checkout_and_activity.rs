//! `fast_checkout_and_activity` — FastCheckoutService moon mining & tutorial RPCs and activityMgr badge state.
//!
//! Real V24.01 client call sites:
//! - `moonmining/autoMoonMining/client/auto_moon_miner_service.pyj`:
//!   - `AutoMoonMinerService.get_mining_cycle_output` -> `FastCheckoutService.RequestMiningCycleOutput`
//!   - `AutoMoonMinerService.get_mining_details` -> `FastCheckoutService.RequestMiningDetails`
//! - `projectdiscovery/client/projects/covid/service.pyj`:
//!   - `CovidClientService.is_tutorial_complete` -> `FastCheckoutService.get_tutorial_completion_status`
//!   - `CovidClientService.OnCharacterSessionChanged` -> `FastCheckoutService.initialize_tutorial_status`
//!   - `CovidClientService.restart_all` -> `FastCheckoutService.reset_tutorial`
//!   - `CovidClientService.complete_tutorial` -> `FastCheckoutService.skip_tutorial`
//! - `projectdiscovery/client/baseservice.pyj`:
//!   - `CommonProjectDiscoveryClientService.is_enabled` -> `FastCheckoutService.is_enabled`
//! - `eve/client/script/ui/services/activitiesSvc.pyj`:
//!   - `ActivitiesService.OnActivitySeen` -> `activityMgr.ActivitySeen`
//!   - `ActivitiesService.GetStatus` -> `activityMgr.GetActivitiesPopState`
//!   - `ActivitiesService.LoadActivityStatus` -> `activityMgr.GetActivityBadgeStatuses`
//!   - `ActivitiesService.OnStatusChanged` -> `activityMgr.SetActivitiesPopState`

#![forbid(unsafe_code)]

use crate::router::context::ServerContext;
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

const MAX_SEEN_ACTIVITIES: usize = 128;

static SDE_NEW_ACTIVITY_IDS: std::sync::OnceLock<std::collections::BTreeSet<i64>> =
    std::sync::OnceLock::new();

fn sde_new_activity_ids(sde: &eve_sde::SdeRegistry) -> &'static std::collections::BTreeSet<i64> {
    SDE_NEW_ACTIVITY_IDS.get_or_init(|| sde.load_new_activity_ids().unwrap_or_default())
}

pub use eve_db::{
    CharacterActivityState, FastCheckoutOfferRecord, FastCheckoutProductRecord,
    UserFastCheckoutState,
};

fn load_catalog_plex_offers(snap: &eve_db::WorldStateSnapshot) -> Vec<FastCheckoutOfferRecord> {
    let catalog = crate::services::quasar_payment_store_types::read_store_catalog(snap);
    let tags_map = crate::services::quasar_payment_store_types::read_store_offer_tags(snap);
    catalog
        .offers
        .iter()
        .filter_map(|offer| {
            let cents = offer.price_cents.unwrap_or(0);
            let mut plex_amount: i64 = 0;
            let mut products = Vec::new();
            for pid in &offer.product_ids {
                if let Some(p) = catalog.products.iter().find(|prod| prod.id == *pid) {
                    if p.type_id == crate::services::market_contracts::PLEX_TYPE_ID && p.quantity > 0 {
                        plex_amount += i64::from(p.quantity);
                    } else if p.quantity > 0 {
                        products.push(FastCheckoutProductRecord {
                            type_id: p.type_id,
                            quantity: p.quantity,
                        });
                    }
                }
            }
            if plex_amount <= 0 && products.is_empty() {
                return None;
            }
            let tags = tags_map.get(&offer.id).cloned().unwrap_or_default();
            Some(FastCheckoutOfferRecord {
                offer_id: i64::from(offer.id),
                plex_amount,
                price_usd: (cents as f64) / 100.0,
                price_plex: offer.price_plex,
                name: offer.name.clone(),
                description: offer.description.clone(),
                tags,
                products,
                category_id: offer.category_id,
            })
        })
        .collect()
}

fn default_free_plex_pack_offers() -> Vec<FastCheckoutOfferRecord> {
    vec![
        FastCheckoutOfferRecord {
            offer_id: 1,
            plex_amount: 100,
            price_usd: 4.99,
            price_plex: 0,
            name: "100 PLEX".into(),
            description: "100 PLEX Vault Pack".into(),
            tags: vec!["plex".into(), "popular".into()],
            products: Vec::new(),
            category_id: 0,
        },
        FastCheckoutOfferRecord {
            offer_id: 2,
            plex_amount: 250,
            price_usd: 12.49,
            price_plex: 0,
            name: "250 PLEX".into(),
            description: "250 PLEX Vault Pack".into(),
            tags: vec!["plex".into()],
            products: Vec::new(),
            category_id: 0,
        },
        FastCheckoutOfferRecord {
            offer_id: 3,
            plex_amount: 500,
            price_usd: 24.99,
            price_plex: 0,
            name: "500 PLEX".into(),
            description: "500 PLEX Vault Pack".into(),
            tags: vec!["plex".into(), "bestseller".into()],
            products: Vec::new(),
            category_id: 0,
        },
        FastCheckoutOfferRecord {
            offer_id: 4,
            plex_amount: 1100,
            price_usd: 49.99,
            price_plex: 0,
            name: "1100 PLEX".into(),
            description: "1100 PLEX Vault Pack".into(),
            tags: vec!["plex".into()],
            products: Vec::new(),
            category_id: 0,
        },
    ]
}

fn load_fast_checkout_offers_for_ctx(
    _ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
) -> Vec<FastCheckoutOfferRecord> {
    let catalog_offers = load_catalog_plex_offers(snap);
    let need_default_packs = !catalog_offers.iter().any(|o| o.plex_amount > 0);
    let mut offers = Vec::new();
    if need_default_packs {
        offers.extend(default_free_plex_pack_offers());
    }
    for o in catalog_offers {
        if !offers.iter().any(|existing| existing.offer_id == o.offer_id) {
            offers.push(o);
        }
    }
    offers
}

pub fn get_user_fast_checkout_state(ctx: &ServerContext, user_id: u64) -> UserFastCheckoutState {
    ctx.db.with_snapshot(|snap| {
        snap.seasons_and_campaigns
            .fast_checkout_states
            .get(&user_id)
            .cloned()
            .unwrap_or_default()
    })
}

pub fn save_user_fast_checkout_state(
    ctx: &ServerContext,
    user_id: u64,
    state: &UserFastCheckoutState,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.seasons_and_campaigns
            .fast_checkout_states
            .insert(user_id, state.clone());
    });
}

pub fn set_user_fast_checkout_enabled(ctx: &ServerContext, user_id: u64, enabled: bool) {
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap
            .seasons_and_campaigns
            .fast_checkout_states
            .entry(user_id)
            .or_default();
        st.enabled = enabled;
    });
}

pub fn handle(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("FastCheckoutService", "RequestMiningCycleOutput") => {
            Some(handle_request_mining_cycle_output(ctx, session, args, kwargs))
        }
        ("FastCheckoutService", "RequestMiningDetails") => {
            Some(handle_request_mining_details(ctx, session, args, kwargs))
        }
        ("FastCheckoutService", "get_tutorial_completion_status") => {
            Some(handle_get_tutorial_completion_status(ctx, session))
        }
        ("FastCheckoutService", "initialize_tutorial_status") => {
            Some(handle_initialize_tutorial_status(ctx, session))
        }
        ("FastCheckoutService", "is_enabled") => {
            Some(handle_is_enabled(ctx, session))
        }
        ("FastCheckoutService", "reset_tutorial") => {
            Some(handle_reset_tutorial(ctx, session))
        }
        ("FastCheckoutService", "skip_tutorial") => {
            Some(handle_skip_tutorial(ctx, session))
        }
        ("FastCheckoutService", "IsFastCheckoutEnabledForUser") => {
            Some(handle_is_fast_checkout_enabled_for_user(ctx, session))
        }
        ("FastCheckoutService", "GetOffersForUser") => {
            Some(handle_get_offers_for_user(ctx, session))
        }
        ("FastCheckoutService", "ClearFastCheckoutCacheForUser") => {
            Some(handle_clear_fast_checkout_cache_for_user(ctx, session))
        }
        ("FastCheckoutService", "BuyOffer") => {
            Some(handle_buy_offer(ctx, session, args, kwargs))
        }
        ("FastCheckoutService", "GetTestingConfiguration") => {
            Some(handle_get_testing_configuration(ctx))
        }
        ("activityMgr", "ActivitySeen") => {
            Some(handle_activity_seen(ctx, session, args, kwargs))
        }
        ("activityMgr", "GetActivitiesPopState") => {
            Some(handle_get_activities_pop_state(ctx, session))
        }
        ("activityMgr", "GetActivityBadgeStatuses") => {
            Some(handle_get_activity_badge_statuses(ctx, session))
        }
        ("activityMgr", "SetActivitiesPopState") => {
            Some(handle_set_activities_pop_state(ctx, session, args, kwargs))
        }
        _ => None,
    }
}

pub fn compute_mining_materials_per_cycle(
    sde: &eve_sde::SdeRegistry,
    solar_system_id: u32,
    moon_id: i64,
    custom_ores: &[(u32, f64)],
) -> Vec<PyRep> {
    let composition: Vec<(u32, f64)> = if !custom_ores.is_empty() {
        custom_ores.to_vec()
    } else if moon_id != 0 {
        crate::services::mining_advanced::derive_moon_ores_from_sde(sde, solar_system_id, moon_id)
    } else {
        Vec::new()
    };

    let mut totals: std::collections::BTreeMap<u32, f64> = std::collections::BTreeMap::new();
    for (ore_tid, frac) in &composition {
        let portion_vol = sde
            .types
            .get(ore_tid)
            .map(|t| (t.volume.max(0.1) as f64) * (t.portion_size.max(1) as f64))
            .unwrap_or(1_000.0);
        let portions = (30_000.0 * frac) / portion_vol;
        if let Some(mats) = sde.type_materials.get(ore_tid) {
            for mat in mats {
                let is_moon_material = sde
                    .types
                    .get(&mat.material_type_id)
                    .is_some_and(|t| t.group_id == 427);
                if is_moon_material {
                    *totals.entry(mat.material_type_id).or_insert(0.0) +=
                        portions * (mat.quantity as f64) * 0.40;
                }
            }
        }
    }

    if totals.is_empty() && !composition.is_empty() {
        // Fallback for custom mock ores without SDE group 427 materials:
        // iterate across all ores and accumulate output tuples without early exit.
        let mut out = Vec::new();
        for (ore_tid, frac) in composition {
            let unit_vol = sde.types.get(&ore_tid).map(|t| t.volume as f64).unwrap_or(0.1);
            let qty = (30_000.0 * frac / unit_vol.max(0.1)).round().max(1.0) as u64;
            let total_vol = (qty as f64) * unit_vol;
            out.push(PyRep::Tuple(vec![
                PyRep::Integer(ore_tid as i64),
                PyRep::Integer(qty as i64),
                PyRep::Real(total_vol),
            ]));
        }
        return out;
    }

    totals
        .into_iter()
        .filter_map(|(tid, qty_f)| {
            let qty = qty_f.round() as u64;
            if qty > 0 {
                let unit_vol = sde.types.get(&tid).map(|t| t.volume as f64).unwrap_or(0.1);
                let total_vol = (qty as f64) * unit_vol;
                Some(PyRep::Tuple(vec![
                    PyRep::Integer(tid as i64),
                    PyRep::Integer(qty as i64),
                    PyRep::Real(total_vol),
                ]))
            } else {
                None
            }
        })
        .collect()
}

fn handle_request_mining_cycle_output(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;

    let Some(structure_id) = args
        .first()
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("structureID").and_then(|v| v.as_i64()))
    else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "RequestMiningCycleOutput requires structureID".into(),
            client_error: None,
        });
    };

    ctx.db.with_snapshot(|snap| {
        let efficiency = snap
            .auto_moon_miners
            .get(&structure_id)
            .map(|r| r.efficiency)
            .unwrap_or(0.40);
        let info = crate::services::mining_advanced::mining_compression::inspect_metenox_drill(
            &ctx.sde,
            snap,
            structure_id,
            char_id,
            efficiency,
        )?;
        if let Some(ext) = snap.moon_extractions.get(&structure_id) {
            if !ext.moon_ores.is_empty() {
                let sys_id = snap
                    .structures
                    .get(&structure_id)
                    .map(|s| s.solar_system_id)
                    .unwrap_or(0);
                return Ok(PyRep::List(compute_mining_materials_per_cycle(
                    &ctx.sde,
                    sys_id,
                    ext.moon_id,
                    &ext.moon_ores,
                )));
            }
        }
        let tuples: Vec<PyRep> = info
            .hourly_materials
            .into_iter()
            .map(|(tid, qty)| {
                let uvol = ctx.sde.types.get(&tid).map(|t| t.volume).unwrap_or(0.1);
                PyRep::Tuple(vec![
                    PyRep::Integer(tid as i64),
                    PyRep::Integer(qty as i64),
                    PyRep::Real((qty as f64) * uvol),
                ])
            })
            .collect();
        Ok(PyRep::List(tuples))
    })
}

fn handle_request_mining_details(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;

    let Some(structure_id) = args
        .first()
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("structureID").and_then(|v| v.as_i64()))
    else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "RequestMiningDetails requires structureID".into(),
            client_error: None,
        });
    };

    let now = ctx.now_unix();
    let (available_fuel, moon_id, next_harvest_time, remaining_capacity, materials) =
        ctx.db.with_snapshot(|snap| {
            let Some(st) = snap.structures.get(&structure_id) else {
                return Err(BridgeRpcError {
                    code: "StructureNotFound".into(),
                    message: format!("Structure {structure_id} not found"),
                    client_error: None,
                });
            };

            let caller_corp = snap
                .characters
                .get(&char_id)
                .map(|c| c.corporation_id)
                .unwrap_or(0);
            let is_corp_member = st.owner_corp_id != 0 && caller_corp == st.owner_corp_id;
            let is_allowed = is_corp_member
                || crate::services::station_structure::structure_upwell_ops::evaluate_character_structure_access_in_snap(
                    snap,
                    structure_id,
                    char_id,
                );
            if !is_allowed {
                return Err(BridgeRpcError {
                    code: "AccessDenied".into(),
                    message: "Character lacks corporation permissions for structure mining details".into(),
                    client_error: None,
                });
            }

            let ext = snap.moon_extractions.get(&structure_id);
            let auto_miner = snap.auto_moon_miners.get(&structure_id);

            let item_fuel: u32 = snap
                .items
                .values()
                .filter(|it| {
                    it.location_id == structure_id
                        && it.owner_id == st.owner_corp_id
                        && matches!(
                            it.flag,
                            InventoryFlag::SpecializedFuelBay | InventoryFlag::StructureFuel
                        )
                        && it.quantity > 0
                        && ctx
                            .sde
                            .types
                            .get(&it.type_id)
                            .is_some_and(|t| t.group_id == 1136)
                })
                .fold(0u32, |acc, it| {
                    acc.saturating_add(it.quantity.max(0) as u32)
                });
            let mut fuel = if item_fuel > 0 {
                item_fuel as i64
            } else {
                st.fuel_blocks as i64
            };
            if fuel == 0 {
                if let Some(am) = auto_miner {
                    fuel = am.fuel_units_remaining as i64;
                }
            }

            let sys_id = st.solar_system_id;
            let m_id = if let Some(e) = ext {
                e.moon_id
            } else {
                crate::services::mining_advanced::resolve_structure_moon_id(
                    &ctx.sde,
                    st.solar_system_id,
                    st.position,
                )
                .unwrap_or(0)
            };

            let next_harvest_unix = if let Some(e) = ext {
                e.chunk_arrival_unix
            } else if let Some(am) = auto_miner {
                am.last_cycle_unix + am.cycle_duration_seconds
            } else {
                now + 3600
            };
            let next_harvest_filetime =
                (next_harvest_unix as i64 + 11_644_473_600) * 10_000_000;

            let total_capacity: f64 = ctx
                .sde
                .types
                .get(&st.type_id)
                .and_then(|t| {
                    t.default_attributes
                        .get(&5693)
                        .copied()
                        .filter(|&c| c > 0.0)
                        .or_else(|| (t.capacity > 0.0).then_some(t.capacity))
                })
                .unwrap_or(50_000.0);
            let used_vol: f64 = snap
                .items
                .values()
                .filter(|it| {
                    it.location_id == structure_id
                        && (it.flag == InventoryFlag::SpecializedMineralHold
                            || it.flag == InventoryFlag::SpecializedMaterialBay
                            || it.flag == InventoryFlag::CorpHangar
                            || (it.flag as u16 >= 116 && it.flag as u16 <= 122))
                })
                .map(|it| {
                    let unit_vol = ctx
                        .sde
                        .types
                        .get(&it.type_id)
                        .map(|t| t.volume as f64)
                        .unwrap_or(0.1);
                    (it.quantity.max(0) as f64) * unit_vol
                })
                .sum();
            let remaining_cap = (total_capacity - used_vol).max(0.0);

            let ores = ext.map(|e| e.moon_ores.clone()).unwrap_or_default();
            let mats = compute_mining_materials_per_cycle(&ctx.sde, sys_id, m_id, &ores);

            Ok((fuel, m_id, next_harvest_filetime, remaining_cap, mats))
        })?;

    let dict = PyRep::Dict(vec![
        (
            PyRep::String("availableFuel".into()),
            PyRep::Integer(available_fuel),
        ),
        (PyRep::String("moonID".into()), PyRep::Integer(moon_id)),
        (
            PyRep::String("nextHarvestTime".into()),
            PyRep::Integer(next_harvest_time),
        ),
        (
            PyRep::String("outputMaterials".into()),
            PyRep::List(materials),
        ),
        (
            PyRep::String("remainingOutputBayCapacity".into()),
            PyRep::Real(remaining_capacity),
        ),
    ]);

    Ok(dict)
}

fn handle_is_enabled(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, _char_id) = ServerContext::require_character(session)?;
    let enabled = crate::services::project_discovery::is_project_discovery_enabled(ctx);
    Ok(PyRep::Bool(enabled))
}

fn handle_get_tutorial_completion_status(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let completed = ctx.db.with_snapshot(|snap| {
        snap.seasons_and_campaigns
            .project_discovery_players
            .get(&char_id)
            .map(|st| st.tutorial_complete || st.tutorial_skipped)
            .unwrap_or(false)
    });
    Ok(PyRep::Bool(completed))
}

fn handle_initialize_tutorial_status(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let completed = ctx.db.with_mut_snapshot(|snap| {
        let state = snap
            .seasons_and_campaigns
            .project_discovery_players
            .entry(char_id)
            .or_default();
        state.tutorial_complete || state.tutorial_skipped
    });
    Ok(PyRep::Bool(completed))
}

fn handle_reset_tutorial(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    ctx.db.with_mut_snapshot(|snap| {
        let state = snap
            .seasons_and_campaigns
            .project_discovery_players
            .entry(char_id)
            .or_default();
        state.tutorial_level = 1;
        state.tutorial_skipped = false;
        state.tutorial_complete = false;
    });
    Ok(PyRep::Bool(true))
}

fn handle_skip_tutorial(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    ctx.db.with_mut_snapshot(|snap| {
        let state = snap
            .seasons_and_campaigns
            .project_discovery_players
            .entry(char_id)
            .or_default();
        state.tutorial_level = 5;
        state.tutorial_skipped = true;
        state.tutorial_complete = false;
    });
    Ok(PyRep::Bool(true))
}

fn handle_activity_seen(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(activity_id) = args
        .first()
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("activityID").and_then(|v| v.as_i64()))
        .filter(|&id| id > 0)
    else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "ActivitySeen requires a positive activityID".into(),
            client_error: None,
        });
    };

    let valid_ids = sde_new_activity_ids(&ctx.sde);
    if !valid_ids.contains(&activity_id) {
        return Err(BridgeRpcError {
            code: "InvalidActivityID".into(),
            message: format!(
                "Unknown activityID {activity_id} not in res:/staticdata/newactivities.fsdbinary"
            ),
            client_error: None,
        });
    }

    ctx.db.with_mut_snapshot(|snap| {
        let state = snap
            .seasons_and_campaigns
            .character_activities
            .entry(char_id)
            .or_default();
        if !state.seen_activities.contains(&activity_id) {
            if state.seen_activities.len() >= MAX_SEEN_ACTIVITIES {
                let overflow = state.seen_activities.len() + 1 - MAX_SEEN_ACTIVITIES;
                state.seen_activities.drain(0..overflow);
            }
            state.seen_activities.push(activity_id);
        }
    });
    Ok(PyRep::Bool(true))
}

fn handle_get_activities_pop_state(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let pop_state = ctx.db.with_snapshot(|snap| {
        snap.seasons_and_campaigns
            .character_activities
            .get(&char_id)
            .map(|st| st.pop_state.clone())
            .unwrap_or_default()
    });
    Ok(PyRep::Dict(pop_state))
}

fn handle_get_activity_badge_statuses(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let seen_activities = ctx.db.with_snapshot(|snap| {
        snap.seasons_and_campaigns
            .character_activities
            .get(&char_id)
            .map(|st| st.seen_activities.clone())
            .unwrap_or_default()
    });
    let list = seen_activities.into_iter().map(PyRep::Integer).collect();
    Ok(PyRep::PySet(list))
}

fn handle_set_activities_pop_state(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let raw_val = args.first().or_else(|| kwargs.get("state"));
    let mut pop_dict = match raw_val {
        Some(PyRep::Dict(entries)) => entries.clone(),
        Some(PyRep::KeyVal(kv)) => kv
            .iter()
            .map(|(k, v)| (PyRep::String(k.clone()), v.clone()))
            .collect(),
        _ => {
            return Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "Expected dict of pop states".into(),
                client_error: None,
            });
        }
    };

    if pop_dict.len() > 100 {
        pop_dict.truncate(100);
    }

    ctx.db.with_mut_snapshot(|snap| {
        let state = snap
            .seasons_and_campaigns
            .character_activities
            .entry(char_id)
            .or_default();
        state.pop_state = pop_dict;
    });
    Ok(PyRep::Bool(true))
}

pub fn is_fast_checkout_enabled_for_user(ctx: &ServerContext, user_id: u64) -> bool {
    let st = get_user_fast_checkout_state(ctx, user_id);
    if st.enabled {
        return true;
    }
    if let Ok(guard) = ctx.global_config_values.read() {
        if let Some(val) = guard.get("fastCheckoutEnabled") {
            return match val {
                PyRep::Bool(b) => *b,
                PyRep::Integer(i) => *i != 0,
                PyRep::String(s) => s == "1" || s.eq_ignore_ascii_case("true"),
                _ => false,
            };
        }
    }
    ctx.feature_flags.free_fast_checkout_plex
}

fn handle_is_fast_checkout_enabled_for_user(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let user_id = ServerContext::require_auth(session)?;
    Ok(PyRep::Bool(is_fast_checkout_enabled_for_user(ctx, user_id)))
}

fn handle_get_offers_for_user(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let user_id = ServerContext::require_auth(session)?;
    if !is_fast_checkout_enabled_for_user(ctx, user_id) {
        return Ok(PyRep::List(Vec::new()));
    }
    let free_plex_mode = ctx.feature_flags.free_fast_checkout_plex;
    let mut st = get_user_fast_checkout_state(ctx, user_id);
    let offers = match st.cached_offers.clone() {
        Some(cached)
            if !cached.is_empty()
                && (!free_plex_mode || cached.iter().any(|o| o.plex_amount > 0)) =>
        {
            cached
        }
        _ => ctx
            .db
            .with_snapshot(|snap| load_fast_checkout_offers_for_ctx(ctx, snap)),
    };
    let session_token = format!("fc_token_{}_{}", user_id, ctx.now_unix());
    st.cached_session_token = Some(session_token);
    if !offers.is_empty() {
        st.cached_offers = Some(offers.clone());
    }
    save_user_fast_checkout_state(ctx, user_id, &st);

    let plex_offers: Vec<PyRep> = offers
        .into_iter()
        .map(|o| {
            let base_qty = if o.plex_amount > 0 {
                o.plex_amount
            } else {
                o.products.first().map(|p| p.quantity as i64).unwrap_or(1)
            };
            let display_price = if free_plex_mode {
                0.0
            } else if o.price_usd > 0.0 {
                o.price_usd
            } else {
                o.price_plex as f64
            };
            let mut prod_reps: Vec<PyRep> = o
                .products
                .iter()
                .map(|p| {
                    PyRep::Dict(vec![
                        (PyRep::String("typeId".into()), PyRep::Integer(p.type_id as i64)),
                        (PyRep::String("quantity".into()), PyRep::Integer(p.quantity as i64)),
                    ])
                })
                .collect();
            if prod_reps.is_empty() && o.plex_amount > 0 {
                prod_reps.push(PyRep::Dict(vec![
                    (
                        PyRep::String("typeId".into()),
                        PyRep::Integer(i64::from(crate::services::market_contracts::PLEX_TYPE_ID)),
                    ),
                    (PyRep::String("quantity".into()), PyRep::Integer(base_qty)),
                ]));
            }
            PyRep::Dict(vec![
                (PyRep::String("offerId".into()), PyRep::Integer(o.offer_id)),
                (PyRep::String("offerID".into()), PyRep::Integer(o.offer_id)),
                (PyRep::String("baseQuantity".into()), PyRep::Integer(base_qty)),
                (PyRep::String("plexAmount".into()), PyRep::Integer(o.plex_amount)),
                (PyRep::String("price".into()), PyRep::Real(display_price)),
                (PyRep::String("pricePlex".into()), PyRep::Integer(o.price_plex)),
                (
                    PyRep::String("tags".into()),
                    PyRep::List(o.tags.into_iter().map(PyRep::String).collect()),
                ),
                (PyRep::String("name".into()), PyRep::String(o.name)),
                (PyRep::String("description".into()), PyRep::String(o.description)),
                (PyRep::String("products".into()), PyRep::List(prod_reps)),
            ])
        })
        .collect();

    Ok(PyRep::Dict(vec![
        (PyRep::String("plex".into()), PyRep::List(plex_offers.clone())),
        (PyRep::String("offers".into()), PyRep::List(plex_offers)),
    ]))
}

fn handle_clear_fast_checkout_cache_for_user(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let user_id = ServerContext::require_auth(session)?;
    let mut st = get_user_fast_checkout_state(ctx, user_id);
    st.cached_session_token = None;
    st.cached_offers = None;
    save_user_fast_checkout_state(ctx, user_id, &st);
    Ok(PyRep::None)
}

fn extract_offer_id(rep: &PyRep) -> Option<i64> {
    if let Some(i) = rep.as_i64() {
        return Some(i);
    }
    for k in ["offerId", "offerID", "id", "offer_id"] {
        if let Some(v) = rep.dict_get(k).and_then(|v| v.as_i64()) {
            return Some(v);
        }
    }
    None
}

fn handle_buy_offer(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let user_id = ServerContext::require_auth(session)?;
    if !is_fast_checkout_enabled_for_user(ctx, user_id) {
        return Err(BridgeRpcError {
            code: "FastCheckoutDisabled".into(),
            message: "Fast checkout is disabled for this user".into(),
            client_error: None,
        });
    }

    let raw_offer_id = args
        .first()
        .and_then(extract_offer_id)
        .or_else(|| kwargs.get("offer").and_then(extract_offer_id))
        .or_else(|| kwargs.get("offerID").and_then(extract_offer_id))
        .or_else(|| kwargs.get("offer_id").and_then(extract_offer_id))
        .or_else(|| kwargs.get("id").and_then(extract_offer_id));

    let offer_id = match raw_offer_id {
        Some(id) if id > 0 => id,
        Some(id) if id <= 0 => {
            return Err(BridgeRpcError {
                code: "OfferNotFound".into(),
                message: "UserError: OfferNotFound: invalid offerID".into(),
                client_error: None,
            });
        }
        _ => {
            return Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "BuyOffer requires offerID".into(),
                client_error: None,
            });
        }
    };

    let (quantity, password_opt) = if args.len() >= 3 {
        let q = args[1].as_i64().unwrap_or(1);
        let p = args[2].as_str();
        (q, p)
    } else if args.len() == 2 {
        if let Some(p) = args[1].as_str() {
            (1, Some(p))
        } else {
            let q = args[1].as_i64().unwrap_or(1);
            (q, None)
        }
    } else {
        let q = kwargs.get("quantity").and_then(|a| a.as_i64()).unwrap_or(1);
        (q, None)
    };

    if quantity <= 0 {
        return Err(BridgeRpcError {
            code: "InvalidQuantity".into(),
            message: "Quantity must be positive".into(),
            client_error: None,
        });
    }

    let free_plex_mode = ctx.feature_flags.free_fast_checkout_plex;
    let password = password_opt
        .or_else(|| kwargs.get("password").and_then(|a| a.as_str()))
        .unwrap_or("");
    if !free_plex_mode && password.is_empty() {
        return Err(BridgeRpcError {
            code: "InvalidPassword".into(),
            message: "Password required for FastCheckout purchase".into(),
            client_error: None,
        });
    }

    let (account, active_char_id, active_station_id) = ctx.db.with_snapshot(|snap| {
        let acc = snap.accounts.values().find(|a| a.user_id == user_id).cloned();
        let cid = session.character_id.or_else(|| {
            snap.characters
                .values()
                .find(|c| c.user_id == user_id)
                .map(|c| c.character_id)
        });
        let st_id = cid.and_then(|id| snap.characters.get(&id)).map(|c| {
            session.station_id.or(session.structure_id).unwrap_or(c.home_station_id)
        });
        (acc, cid, st_id)
    });

    let Some(acc) = account else {
        return Err(BridgeRpcError {
            code: "UserNotFound".into(),
            message: format!("User account {user_id} not found"),
            client_error: None,
        });
    };

    if let Err(e) = acc.check_active_restriction(ctx.now_unix()) {
        return Err(BridgeRpcError {
            code: "AccountBanned".into(),
            message: e.to_string(),
            client_error: None,
        });
    }

    let st = get_user_fast_checkout_state(ctx, user_id);
    let offers = match st.cached_offers {
        Some(cached)
            if !cached.is_empty()
                && (!free_plex_mode || cached.iter().any(|o| o.plex_amount > 0)) =>
        {
            cached
        }
        _ => ctx
            .db
            .with_snapshot(|snap| load_fast_checkout_offers_for_ctx(ctx, snap)),
    };
    let Some(offer) = offers.iter().find(|o| o.offer_id == offer_id).cloned() else {
        return Err(BridgeRpcError {
            code: "OfferNotFound".into(),
            message: format!("UserError: Offer {offer_id} not found in catalog"),
            client_error: None,
        });
    };

    if (!free_plex_mode || !password.is_empty())
        && ctx
            .db
            .authenticate_at(&acc.username, password, ctx.now_unix())
            .is_err()
    {
        return Err(BridgeRpcError {
            code: "InvalidPassword".into(),
            message: "Incorrect password for fast checkout authorization".into(),
            client_error: None,
        });
    }

    if offer.price_plex < 0 {
        return Err(BridgeRpcError {
            code: "InvalidOffer".into(),
            message: format!("Offer {offer_id} has invalid negative PLEX price"),
            client_error: None,
        });
    }

    let total_plex = offer
        .plex_amount
        .checked_mul(quantity)
        .filter(|&v| v >= 0)
        .ok_or_else(|| BridgeRpcError {
            code: "ArithmeticOverflow".into(),
            message: "Total PLEX quantity overflows i64".into(),
            client_error: None,
        })?;
    let total_cost_plex = offer
        .price_plex
        .checked_mul(quantity)
        .filter(|&v| v >= 0)
        .ok_or_else(|| BridgeRpcError {
            code: "ArithmeticOverflow".into(),
            message: "Total PLEX price overflows i64".into(),
            client_error: None,
        })?;

    let now_unix = ctx.now_unix();
    let unit_cents = if free_plex_mode || offer.price_plex > 0 {
        0_u32
    } else {
        (offer.price_usd * 100.0).round().max(0.0) as u32
    };
    let amount_in_cents = u32::try_from(quantity)
        .ok()
        .and_then(|q| unit_cents.checked_mul(q))
        .ok_or_else(|| BridgeRpcError {
            code: "ArithmeticOverflow".into(),
            message: "Payment amount overflows u32".into(),
            client_error: None,
        })?;
    if !free_plex_mode && offer.price_plex <= 0 && amount_in_cents == 0 {
        return Err(BridgeRpcError {
            code: "InvalidOffer".into(),
            message: format!("Offer {offer_id} has no valid price"),
            client_error: None,
        });
    }

    let (tx_id, rem_plex, delivered_items_count, omega_notice) = ctx.db.try_with_mut_snapshot(|snap| {
        if amount_in_cents > 0 {
            let Some(token) = snap
                .quasar_commerce
                .payment_tokens
                .iter_mut()
                .find(|t| t.user_id == user_id && t.is_quick_pay && !t.consumed)
            else {
                return Err(BridgeRpcError {
                    code: "PaymentTokenRequired".into(),
                    message: "No active quick-pay payment token for user".into(),
                    client_error: None,
                });
            };
            token.consumed = true;
        }

        let vault_uid = active_char_id
            .and_then(|cid| snap.account_of_character(cid))
            .unwrap_or(user_id);
        let mut cur_rem = snap.unified_plex_balance_for_user(vault_uid);
        if total_cost_plex > 0 {
            cur_rem = snap
                .adjust_unified_plex_for_user(vault_uid, -total_cost_plex)
                .ok_or_else(|| BridgeRpcError {
                    code: "UserError".into(),
                    message: format!(
                        "UserError: Insufficient balance: need {total_cost_plex} PLEX, have {cur_rem}"
                    ),
                    client_error: None,
                })?;
        }

        if total_plex > 0 {
            cur_rem = snap
                .adjust_unified_plex_for_user(vault_uid, total_plex)
                .ok_or_else(|| BridgeRpcError {
                    code: "ArithmeticOverflow".into(),
                    message: "Account PLEX vault overflows i64".into(),
                    client_error: None,
                })?;
        }

        snap.next_entity_id += 1;
        let tid = snap.next_entity_id as u64;

        if total_cost_plex > 0 || total_plex > 0 {
            let delta = total_plex
                .checked_sub(total_cost_plex)
                .ok_or_else(|| BridgeRpcError {
                    code: "ArithmeticOverflow".into(),
                    message: "Net PLEX delta overflows i64".into(),
                    client_error: None,
                })?;
            crate::services::quasar_payment_store::record_plex_transaction(
                snap,
                tid,
                user_id,
                delta,
                cur_rem,
                "FastCheckout Purchase",
                now_unix,
            );
        }

        let mut delivered_count = 0_i64;
        let char_id = active_char_id.unwrap_or(user_id as i64);
        let station_id = active_station_id
            .or_else(|| {
                snap.characters
                    .get(&char_id)
                    .and_then(|c| c.station_id.or(c.structure_id).or(Some(c.home_station_id)).filter(|&id| id > 0))
            })
            .unwrap_or(0);
        for prod in &offer.products {
            if prod.type_id > 0 && prod.quantity > 0 {
                let total_item_qty = i32::try_from(
                    (prod.quantity as i64)
                        .checked_mul(quantity)
                        .ok_or_else(|| BridgeRpcError {
                            code: "ArithmeticOverflow".into(),
                            message: "Product quantity overflows i32".into(),
                            client_error: None,
                        })?,
                )
                .map_err(|_| BridgeRpcError {
                    code: "ArithmeticOverflow".into(),
                    message: "Product quantity overflows i32".into(),
                    client_error: None,
                })?;
                snap.next_entity_id += 1;
                let item_id = snap.next_entity_id;
                snap.items.insert(
                    item_id,
                    eve_db::InventoryItemRecord {
                        item_id,
                        type_id: prod.type_id,
                        owner_id: char_id,
                        location_id: station_id,
                        flag: eve_db::InventoryFlag::Hangar,
                        quantity: total_item_qty,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    },
                );
                delivered_count += total_item_qty as i64;
            }
        }

        let omega_notice = if offer.category_id == 1 {
            let duration_secs = u64::try_from(quantity)
                .unwrap_or(1)
                .saturating_mul(30 * 86_400);
            let (_, notice) =
                crate::services::quasar_auth_user::activate_user_omega_license_in_snapshot(
                    snap,
                    user_id,
                    duration_secs,
                    now_unix,
                );
            Some(notice)
        } else {
            None
        };

        if amount_in_cents > 0 {
            snap.quasar_commerce.payment_receipts.push(
                crate::services::quasar_payment_store_types::PaymentReceiptRecord {
                    receipt_id: tid,
                    user_id,
                    offer_id: offer_id.to_string(),
                    amount_in_cents,
                    currency: "USD".to_string(),
                    timestamp_unix: now_unix,
                },
            );
        }

        Ok::<_, BridgeRpcError>((tid, cur_rem, delivered_count, omega_notice))
    })?;

    if let Some(notice) = omega_notice {
        ctx.quasar_gateway
            .pending_notices
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push((
                crate::quasar_gateway::NoticeScope::User(user_id as i32),
                notice,
            ));
    }

    if total_plex > 0 {
        crate::services::quasar_payment_store::emit_balance_changed_notice(
            &ctx.quasar_gateway.pending_notices,
            user_id,
            tx_id,
            total_plex,
            rem_plex,
            "FastCheckout Purchase",
            ctx.now_unix(),
        );
    }
    if let Some(char_id) = active_char_id {
        let _ = ctx.notify_account_change(char_id, "plex", char_id, rem_plex as f64);
    }

    let status_str = if free_plex_mode {
        "COMPLETED"
    } else {
        "CONFIRMED"
    };
    Ok(PyRep::Dict(vec![
        (PyRep::String("status".into()), PyRep::String(status_str.into())),
        (PyRep::String("transactionID".into()), PyRep::Integer(tx_id as i64)),
        (PyRep::String("offerID".into()), PyRep::Integer(offer_id)),
        (PyRep::String("plexAmount".into()), PyRep::Integer(total_plex)),
        (PyRep::String("itemsDelivered".into()), PyRep::Integer(delivered_items_count)),
        (PyRep::String("newPlexBalance".into()), PyRep::Integer(rem_plex)),
        (PyRep::String("Message".into()), PyRep::String("OK".into())),
    ]))
}

fn handle_get_testing_configuration(
    ctx: &ServerContext,
) -> Result<PyRep, BridgeRpcError> {
    let is_test = ctx.feature_flags.seed_demo;
    let fake_url = if is_test {
        format!("http://127.0.0.1:{}/fastcheckout/test_buy_plex", ctx.esi_port)
    } else {
        String::new()
    };
    Ok(PyRep::Tuple(vec![
        PyRep::Bool(is_test),
        PyRep::String(fake_url),
        PyRep::Bool(is_test),
    ]))
}

#[cfg(test)] #[path = "fast_checkout_and_activity_tests.rs"] mod fast_checkout_and_activity_tests;
#[cfg(test)]
#[path = "fast_checkout_and_activity_edges_tests.rs"]
mod fast_checkout_and_activity_edges_tests;

