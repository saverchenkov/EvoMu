//! `quasar_corp_structure_services` — Batch 29 Quasar Public Gateway Admin & Orchestrator Handlers.
//!
//! All handlers require `ROLE_ADMIN | ROLE_GMH` (enforced by `PublicGatewayRouter::requires_admin_role`)
//! and read/write canonical `WorldRepository` state (`quasar_cosmetics_market`, `payment_receipts`,
//! `plex_balances`, `plex_tx_logs`, `accounts`, `characters`, and `service_state_blobs`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND,
};
use crate::services::quasar_cosmetics_market::seed_component_listings_if_empty;
use crate::services::quasar_cosmetics_market_types::CosmeticsMarketData;
use crate::services::quasar_payment_store_types::record_plex_transaction;
use eve_db::WorldRepository;
use eve_proto::pb;
#[cfg(test)]
use eve_proto::quasar::GoogleAny;
use eve_proto::quasar::{DynamicQuasarMessage, DynamicValue, GoogleTimestamp};
use eve_sde::SdeRegistry;
use std::collections::BTreeMap;
use std::sync::Arc;

const BLOB_COSMETICS_MARKET: &str = "quasar_cosmetics_market";

pub use eve_db::{
    ContentGroupRecord, ContentTypeRecord, DeploymentInstanceRecord, DeploymentRecord,
    FailedFulfillmentRecord, OrchestratorState, SemVerSpec, TargetedOfferAdminRecord,
};

fn semver_builder(ver: &SemVerSpec) -> pb::ProtoBuilder {
    pb::message("eve.semanticversion.Specification")
        .when(ver.major > 0, |b| b.set("major", ver.major))
        .when(ver.minor > 0, |b| b.set("minor", ver.minor))
        .when(ver.patch > 0, |b| b.set("patch", ver.patch))
}

pub(crate) trait SemVerSpecExt {
    #[cfg(test)]
    fn encode(&self) -> Vec<u8>;
    fn from_dyn(msg: &DynamicQuasarMessage) -> Self;
}

impl SemVerSpecExt for SemVerSpec {
    #[cfg(test)]
    fn encode(&self) -> Vec<u8> {
        semver_builder(self).must_encode()
    }

    fn from_dyn(msg: &DynamicQuasarMessage) -> Self {
        Self {
            major: get_u64(msg, 1) as u32,
            minor: get_u64(msg, 2) as u32,
            patch: get_u64(msg, 3) as u32,
        }
    }
}

fn load_orch_state(db: &WorldRepository) -> OrchestratorState {
    db.with_snapshot(|snap| snap.quasar_commerce.orchestrator.clone())
}

fn save_orch_state(db: &WorldRepository, state: &OrchestratorState) {
    db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.orchestrator = state.clone();
    });
}

fn get_u64(msg: &DynamicQuasarMessage, field_num: u32) -> u64 {
    match msg.get(field_num) {
        Some(DynamicValue::Uint64(v)) => *v,
        Some(DynamicValue::Uint32(v)) => *v as u64,
        Some(DynamicValue::Int64(v)) => *v as u64,
        Some(DynamicValue::Int32(v)) => *v as u64,
        Some(DynamicValue::Sint64(v)) => *v as u64,
        Some(DynamicValue::Sint32(v)) => *v as u64,
        Some(DynamicValue::Enum(v)) => *v as u64,
        Some(DynamicValue::Message(sub)) => get_u64(sub, 1),
        _ => 0,
    }
}

fn get_bool(msg: &DynamicQuasarMessage, field_num: u32) -> bool {
    match msg.get(field_num) {
        Some(DynamicValue::Bool(b)) => *b,
        Some(DynamicValue::Uint64(v)) => *v != 0,
        Some(DynamicValue::Int64(v)) => *v != 0,
        _ => false,
    }
}

fn get_string(msg: &DynamicQuasarMessage, field_num: u32) -> String {
    match msg.get(field_num) {
        Some(DynamicValue::String(s)) => s.clone(),
        Some(DynamicValue::Message(sub)) => get_string(sub, 1),
        _ => String::new(),
    }
}

fn get_bytes(msg: &DynamicQuasarMessage, field_num: u32) -> Vec<u8> {
    match msg.get(field_num) {
        Some(DynamicValue::Bytes(b)) => b.clone(),
        Some(DynamicValue::Message(sub)) => get_bytes(sub, 1),
        _ => Vec::new(),
    }
}

fn extract_tag_list(msg: &DynamicQuasarMessage, field_num: u32) -> Vec<String> {
    match msg.get(field_num) {
        Some(DynamicValue::Repeated(items)) => items
            .iter()
            .filter_map(|v| match v {
                DynamicValue::Message(m) => {
                    let s = get_string(m, 1).trim().to_string();
                    (!s.is_empty()).then_some(s)
                }
                _ => None,
            })
            .collect(),
        Some(DynamicValue::Message(m)) => {
            let s = get_string(m, 1).trim().to_string();
            if s.is_empty() { Vec::new() } else { vec![s] }
        }
        _ => Vec::new(),
    }
}

fn system_matches_tag(sid: u32, info: &eve_sde::SolarSystem, tag: &str) -> bool {
    match tag {
        "highsec" => info.security_status >= 0.45,
        "lowsec" => (0.05..0.45).contains(&info.security_status),
        "nullsec" => info.security_status < 0.05 && sid < 31_000_000,
        "wormhole" => sid >= 31_000_000,
        other => {
            if let Some(rest) = other.strip_prefix("region:") {
                rest.parse::<u32>().is_ok_and(|id| id == info.region_id)
            } else if let Some(rest) = other.strip_prefix("constellation:") {
                rest.parse::<u32>().is_ok_and(|id| id == info.constellation_id)
            } else if let Some(rest) = other.strip_prefix("sid:") {
                rest.parse::<u32>().is_ok_and(|id| id == sid)
            } else {
                other.parse::<u32>().is_ok_and(|id| id == sid)
            }
        }
    }
}

fn set_solar_system_location(
    m: pb::ProtoBuilder,
    sys_id: u32,
    const_id: u32,
) -> pb::ProtoBuilder {
    m.set("constellation.constellation.sequential", const_id as u64)
        .set("constellation.solar_system.sequential", sys_id as u64)
}

pub fn register_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    register_orchestrator_handlers(router, Arc::clone(&db), Arc::clone(&sde));
    register_commerce_admin_handlers(router, db, sde);
}

fn register_orchestrator_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    // 1. eve.content.orchestrator.admin.api.*
    {
        let db1 = Arc::clone(&db);
        router.register_handler(
            "eve.content.orchestrator.admin.api.SetDefaultTenantVersionRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let ver = match dyn_msg.get(1) {
                    Some(DynamicValue::Message(m)) => SemVerSpec::from_dyn(m),
                    _ => return Err((STATUS_BAD_REQUEST, "version required".into())),
                };
                let mut st = load_orch_state(&db1);
                st.default_tenant_version = ver;
                save_orch_state(&db1, &st);
                Ok(pb::message(
                    "eve.content.orchestrator.admin.api.SetDefaultTenantVersionResponse",
                )
                .must_to_any())
            }),
        );

        let db2 = Arc::clone(&db);
        router.register_handler(
            "eve.content.orchestrator.admin.api.SetTenantVersionRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let use_default = get_bool(dyn_msg, 1);
                let mut st = load_orch_state(&db2);
                if use_default {
                    st.tenant_version = None;
                } else if let Some(DynamicValue::Message(m)) = dyn_msg.get(2) {
                    st.tenant_version = Some(SemVerSpec::from_dyn(m));
                } else {
                    return Err((
                        STATUS_BAD_REQUEST,
                        "Must specify default_tenant_version or version".into(),
                    ));
                }
                save_orch_state(&db2, &st);
                Ok(pb::message(
                    "eve.content.orchestrator.admin.api.SetTenantVersionResponse",
                )
                .must_to_any())
            }),
        );
    }

    // 2. eve.content.orchestrator.api.*
    {
        let db1 = Arc::clone(&db);
        router.register_handler(
            "eve.content.orchestrator.api.GetDefaultTenantVersionRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let st = load_orch_state(&db1);
                Ok(pb::message(
                    "eve.content.orchestrator.api.GetDefaultTenantVersionResponse",
                )
                .set("version", semver_builder(&st.default_tenant_version))
                .must_to_any())
            }),
        );

        let db2 = Arc::clone(&db);
        router.register_handler(
            "eve.content.orchestrator.api.GetTenantVersionRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let st = load_orch_state(&db2);
                let ver = st
                    .tenant_version
                    .as_ref()
                    .unwrap_or(&st.default_tenant_version);
                Ok(pb::message(
                    "eve.content.orchestrator.api.GetTenantVersionResponse",
                )
                .set("version", semver_builder(ver))
                .must_to_any())
            }),
        );

        let sde_dry = Arc::clone(&sde);
        router.register_handler(
            "eve.content.orchestrator.api.DryRunRulesetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let Some(DynamicValue::Message(rs)) = dyn_msg.get(1) else {
                    return Err((STATUS_BAD_REQUEST, "Ruleset is required".into()));
                };
                let inc_all = extract_tag_list(rs, 1);
                let inc_any = extract_tag_list(rs, 2);
                let exc_all = extract_tag_list(rs, 3);
                let exc_any = extract_tag_list(rs, 4);
                if inc_all.is_empty() && inc_any.is_empty() && exc_all.is_empty() && exc_any.is_empty() {
                    return Err((
                        STATUS_BAD_REQUEST,
                        "Ruleset must specify at least one tag".into(),
                    ));
                }

                let mut all_tags: Vec<String> = Vec::new();
                for t in inc_all
                    .iter()
                    .chain(inc_any.iter())
                    .chain(exc_all.iter())
                    .chain(exc_any.iter())
                {
                    if !all_tags.contains(t) {
                        all_tags.push(t.clone());
                    }
                }

                let mut sys_ids: Vec<u32> = sde_dry.solar_systems.keys().copied().collect();
                sys_ids.sort_unstable();

                let mut matched_locations: Vec<(u32, u32, Vec<String>)> = Vec::new();
                let mut tag_locations: BTreeMap<String, Vec<(u32, u32)>> = BTreeMap::new();

                for sid in sys_ids {
                    let Some(info) = sde_dry.solar_systems.get(&sid) else {
                        continue;
                    };
                    for t in &all_tags {
                        if system_matches_tag(sid, info, t) {
                            let locs = tag_locations.entry(t.clone()).or_default();
                            if locs.len() < 10 {
                                locs.push((sid, info.constellation_id));
                            }
                        }
                    }
                    let ok_inc_all = inc_all.iter().all(|t| system_matches_tag(sid, info, t));
                    let ok_inc_any = inc_any.is_empty()
                        || inc_any.iter().any(|t| system_matches_tag(sid, info, t));
                    let ok_exc_all = exc_all.is_empty()
                        || !exc_all.iter().all(|t| system_matches_tag(sid, info, t));
                    let ok_exc_any = !exc_any.iter().any(|t| system_matches_tag(sid, info, t));
                    if ok_inc_all && ok_inc_any && ok_exc_all && ok_exc_any && matched_locations.len() < 10 {
                        let matched_tags: Vec<String> = all_tags
                            .iter()
                            .filter(|t| system_matches_tag(sid, info, t))
                            .cloned()
                            .collect();
                        matched_locations.push((sid, info.constellation_id, matched_tags));
                    }
                }

                if matched_locations.is_empty() {
                    return Err((
                        STATUS_NOT_FOUND,
                        "No SDE solar system matches ruleset".into(),
                    ));
                }

                let mut resp = pb::message("eve.content.orchestrator.api.DryRunRulesetResponse");
                for (sys_id, const_id, tags) in &matched_locations {
                    resp = resp.add_with("evaluation_result", |mut m| {
                        m = m.set_with("location", |loc| {
                            set_solar_system_location(loc, *sys_id, *const_id)
                        });
                        for t in tags {
                            m = m.add_with("tags", |tag| tag.set("name", t.as_str()));
                        }
                        m
                    });
                }
                for t in &all_tags {
                    resp = resp.add_with("ruleset_tags_evaluated", |mut m| {
                        m = m.set("tag.name", t.as_str());
                        if let Some(locs) = tag_locations.get(t) {
                            for (sys_id, const_id) in locs {
                                m = m.add_with("locations", |loc| {
                                    set_solar_system_location(loc, *sys_id, *const_id)
                                });
                            }
                        }
                        m
                    });
                }
                Ok(resp.must_to_any())
            }),
        );
    }

    // 3. eve.content.orchestrator.deployment.api.*
    {
        let db1 = Arc::clone(&db);
        router.register_handler(
            "eve.content.orchestrator.deployment.api.ListRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let st = load_orch_state(&db1);
                let mut resp =
                    pb::message("eve.content.orchestrator.deployment.api.ListResponse");
                for dep in st.deployments.values() {
                    resp = resp.add_with("deployments", |m| {
                        m.set("id.sequential", dep.id)
                            .set("name", dep.name.as_str())
                            .set("short_description", dep.short_description.as_str())
                    });
                }
                Ok(resp.must_to_any())
            }),
        );

        let db2 = Arc::clone(&db);
        router.register_handler(
            "eve.content.orchestrator.deployment.api.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let dep_id = get_u64(dyn_msg, 1);
                let st = load_orch_state(&db2);
                let dep = st
                    .deployments
                    .get(&dep_id)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Deployment not found".into()))?;
                Ok(pb::message("eve.content.orchestrator.deployment.api.GetResponse")
                    .set("attributes.name", dep.name.as_str())
                    .set("attributes.content_group.sequential", dep.content_group_id)
                    .set(
                        "attributes.current_content_count",
                        dep.current_content_count,
                    )
                    .set("attributes.no_region_limit", dep.no_region_limit)
                    .must_to_any())
            }),
        );

        let db3 = Arc::clone(&db);
        router.register_handler(
            "eve.content.orchestrator.deployment.api.EnableRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let dep_id = get_u64(dyn_msg, 1);
                let mut st = load_orch_state(&db3);
                let dep = st
                    .deployments
                    .get_mut(&dep_id)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Deployment not found".into()))?;
                dep.enabled = true;
                save_orch_state(&db3, &st);
                Ok(
                    pb::message("eve.content.orchestrator.deployment.api.EnableResponse")
                        .must_to_any(),
                )
            }),
        );

        let db4 = Arc::clone(&db);
        router.register_handler(
            "eve.content.orchestrator.deployment.api.DisableRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let dep_id = get_u64(dyn_msg, 1);
                let mut st = load_orch_state(&db4);
                let dep = st
                    .deployments
                    .get_mut(&dep_id)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Deployment not found".into()))?;
                dep.enabled = false;
                save_orch_state(&db4, &st);
                Ok(
                    pb::message("eve.content.orchestrator.deployment.api.DisableResponse")
                        .must_to_any(),
                )
            }),
        );
    }

    // 4. eve.content.orchestrator.deployment.contentgroup.api.GetRequest
    {
        let db1 = Arc::clone(&db);
        router.register_handler(
            "eve.content.orchestrator.deployment.contentgroup.api.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let cg_id = get_u64(dyn_msg, 1);
                let st = load_orch_state(&db1);
                let cg = st
                    .content_groups
                    .get(&cg_id)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Content group not found".into()))?;
                Ok(pb::message(
                    "eve.content.orchestrator.deployment.contentgroup.api.GetResponse",
                )
                .set("attributes.name", cg.name.as_str())
                .add_with("attributes.weighted_content_types", |m| {
                    m.set("content_type.sequential", cg.content_type_id)
                        .set("weight", cg.weight)
                })
                .must_to_any())
            }),
        );
    }

    // 5. eve.content.orchestrator.deployment.contenttype.api.GetRequest
    {
        let db1 = Arc::clone(&db);
        router.register_handler(
            "eve.content.orchestrator.deployment.contenttype.api.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let ct_id = get_u64(dyn_msg, 1);
                let st = load_orch_state(&db1);
                let ct = st
                    .content_types
                    .get(&ct_id)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Content type not found".into()))?;
                Ok(pb::message(
                    "eve.content.orchestrator.deployment.contenttype.api.GetResponse",
                )
                .set("attributes.name", ct.name.as_str())
                .set(
                    "attributes.short_description",
                    ct.short_description.as_str(),
                )
                .set("attributes.dungeon.sequential", ct.dungeon_id)
                .must_to_any())
            }),
        );
    }

    // 6. eve.content.orchestrator.deployment.instance.admin.api.RemoveRequest
    {
        let db1 = Arc::clone(&db);
        router.register_handler(
            "eve.content.orchestrator.deployment.instance.admin.api.RemoveRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let uuid = get_bytes(dyn_msg, 1);
                if uuid.is_empty() {
                    return Err((STATUS_BAD_REQUEST, "instance uuid required".into()));
                }
                let mut st = load_orch_state(&db1);
                let before = st.instances.len();
                st.instances.retain(|inst| inst.uuid != uuid);
                if st.instances.len() == before {
                    return Err((STATUS_NOT_FOUND, "Deployment instance not found".into()));
                }
                save_orch_state(&db1, &st);
                Ok(pb::message(
                    "eve.content.orchestrator.deployment.instance.admin.api.RemoveResponse",
                )
                .must_to_any())
            }),
        );
    }

    // 7. eve.content.orchestrator.deployment.instance.api.*
    {
        let db1 = Arc::clone(&db);
        router.register_handler(
            "eve.content.orchestrator.deployment.instance.api.ListRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let dep_id = get_u64(dyn_msg, 1);
                let st = load_orch_state(&db1);
                let mut resp = pb::message(
                    "eve.content.orchestrator.deployment.instance.api.ListResponse",
                );
                for inst in st
                    .instances
                    .iter()
                    .filter(|i| dep_id == 0 || i.deployment_id == dep_id)
                {
                    resp = resp.add_with("instances", |m| m.set("uuid", inst.uuid.clone()));
                }
                Ok(resp.must_to_any())
            }),
        );

        let db2 = Arc::clone(&db);
        router.register_handler(
            "eve.content.orchestrator.deployment.instance.api.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let uuid = get_bytes(dyn_msg, 1);
                if uuid.is_empty() {
                    return Err((STATUS_BAD_REQUEST, "instance uuid required".into()));
                }
                let st = load_orch_state(&db2);
                let inst = st
                    .instances
                    .iter()
                    .find(|i| i.uuid == uuid)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Instance not found".into()))?;
                Ok(pb::message(
                    "eve.content.orchestrator.deployment.instance.api.GetResponse",
                )
                .set(
                    "attributes.deployment_template.sequential",
                    inst.deployment_id,
                )
                .set("attributes.content.sequential", inst.content_type_id)
                .set_with("attributes.location", |loc| {
                    set_solar_system_location(loc, inst.solar_system_id, inst.constellation_id)
                })
                .must_to_any())
            }),
        );
    }
}

fn register_commerce_admin_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    // 8. eve.cosmetic.market.component.listing.api.GetAllRequest -> canonical CosmeticsMarketData
    {
        let db1 = Arc::clone(&db);
        let sde1 = Arc::clone(&sde);
        router.register_handler(
            "eve.cosmetic.market.component.listing.api.GetAllRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let mut data: CosmeticsMarketData = db1.with_snapshot(|snap| {
                    snap.quasar_state_blobs
                        .get(BLOB_COSMETICS_MARKET)
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default()
                });
                seed_component_listings_if_empty(&mut data, &sde1);
                let mut resp = pb::message(
                    "eve.cosmetic.market.component.listing.api.GetAllResponse",
                );
                for l in data.component_listings.values() {
                    resp = resp.add_with("listings", |m| {
                        m.set("id.uuid", l.uuid.to_vec())
                            .set(
                                "attributes.plex.total_in_cents",
                                l.price_plex_cents.unwrap_or(0) as i64,
                            )
                            .set(
                                "attributes.component_license_item.sequential",
                                l.component_item_type_id as u64,
                            )
                            .set("attributes.bundle_size", l.bundle_size)
                            .set("attributes.discount_percent", l.discount_percent)
                    });
                }
                Ok(resp.must_to_any())
            }),
        );
    }

    // 9. eve.cosmetic.market.skin.listing.api.admin.* -> canonical CosmeticsMarketData
    {
        let db2 = Arc::clone(&db);
        router.register_handler(
            "eve.cosmetic.market.skin.listing.api.admin.GetAllListedByCharacterRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let creator_id = get_u64(dyn_msg, 1);
                if creator_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "creator character id required".into()));
                }
                let data: CosmeticsMarketData = db2.with_snapshot(|snap| {
                    snap.quasar_state_blobs
                        .get(BLOB_COSMETICS_MARKET)
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default()
                });
                let mut resp = pb::message(
                    "eve.cosmetic.market.skin.listing.api.admin.GetAllListedByCharacterResponse",
                );
                for l in data
                    .skin_listings
                    .values()
                    .filter(|l| l.seller_id as u64 == creator_id)
                {
                    resp = resp.add_with("listings", |m| {
                        m.set("id.uuid", l.uuid.to_vec())
                            .set("attributes.seller.sequential", l.seller_id)
                            .set(
                                "attributes.expires",
                                GoogleTimestamp::from_unix(l.expires_unix as i64, 0),
                            )
                            .set("attributes.quantity", l.quantity)
                    });
                }
                Ok(resp.must_to_any())
            }),
        );
    }

    // 10. eve.store.sale.admin.* -> canonical store_failed_fulfillments + plex_balances
    {
        let db1 = Arc::clone(&db);
        router.register_handler(
            "eve.store.sale.admin.RetryFailedFulfillmentRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let fid = get_u64(dyn_msg, 1) as u32;
                let now_unix = ctx.now_unix();
                let found = db1.with_mut_snapshot(|snap| {
                    let credit_info = snap
                        .quasar_commerce
                        .failed_fulfillments
                        .iter_mut()
                        .enumerate()
                        .find(|(_, r)| r.id == fid)
                        .map(|(idx, rec)| {
                            rec.retries += 1;
                            if !rec.fulfilled && rec.plex_units > 0 {
                                Some((idx, rec.user_id, rec.plex_units))
                            } else {
                                None
                            }
                        });
                    let Some(maybe_credit) = credit_info else {
                        return false;
                    };
                    if let Some((idx, uid, amt)) = maybe_credit {
                        if let Some(new_bal) = snap.adjust_unified_plex_for_user(uid, amt) {
                            snap.quasar_commerce.failed_fulfillments[idx].fulfilled = true;
                            snap.next_entity_id += 1;
                            let tx_id = snap.next_entity_id as u64;
                            record_plex_transaction(
                                snap,
                                tx_id,
                                uid,
                                amt,
                                new_bal,
                                &format!("Admin retry fulfillment #{fid}"),
                                now_unix,
                            );
                        }
                    }
                    true
                });
                if !found {
                    return Err((STATUS_NOT_FOUND, "Failed fulfillment not found".into()));
                }
                Ok(
                    pb::message("eve.store.sale.admin.RetryFailedFulfillmentResponse")
                        .must_to_any(),
                )
            }),
        );

        let db2 = Arc::clone(&db);
        router.register_handler(
            "eve.store.sale.admin.RetryAllFailedFulfillmentsRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let now_unix = ctx.now_unix();
                db2.with_mut_snapshot(|snap| {
                    let mut candidates: Vec<(usize, u64, i64, u32)> = Vec::new();
                    for (idx, rec) in snap.quasar_commerce.failed_fulfillments.iter_mut().enumerate() {
                        rec.retries += 1;
                        if !rec.fulfilled && rec.plex_units > 0 {
                            candidates.push((idx, rec.user_id, rec.plex_units, rec.id));
                        }
                    }
                    for (idx, uid, amt, rec_id) in candidates {
                        if let Some(new_bal) = snap.adjust_unified_plex_for_user(uid, amt) {
                            snap.quasar_commerce.failed_fulfillments[idx].fulfilled = true;
                            snap.next_entity_id += 1;
                            let tx_id = snap.next_entity_id as u64;
                            record_plex_transaction(
                                snap,
                                tx_id,
                                uid,
                                amt,
                                new_bal,
                                &format!("Admin retry-all fulfillment #{rec_id}"),
                                now_unix,
                            );
                        }
                    }
                });
                Ok(
                    pb::message("eve.store.sale.admin.RetryAllFailedFulfillmentsResponse")
                        .must_to_any(),
                )
            }),
        );

        let db3 = Arc::clone(&db);
        router.register_handler(
            "eve.store.sale.admin.DropFailedFulfillmentRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let fid = get_u64(dyn_msg, 1) as u32;
                let removed = db3.with_mut_snapshot(|snap| {
                    let before = snap.quasar_commerce.failed_fulfillments.len();
                    snap.quasar_commerce
                        .failed_fulfillments
                        .retain(|r| r.id != fid);
                    snap.quasar_commerce.failed_fulfillments.len() != before
                });
                if !removed {
                    return Err((STATUS_NOT_FOUND, "Failed fulfillment not found".into()));
                }
                Ok(
                    pb::message("eve.store.sale.admin.DropFailedFulfillmentResponse")
                        .must_to_any(),
                )
            }),
        );
    }

    // 12. eve.store.targetaudience.offer.api.admin.* -> canonical store_targeted_offers
    {
        let db1 = Arc::clone(&db);
        router.register_handler(
            "eve.store.targetaudience.offer.api.admin.PromoteRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let (uid, cat_name) = match dyn_msg.get(1) {
                    Some(DynamicValue::Message(id)) => (get_u64(id, 1), get_string(id, 2)),
                    _ => return Err((STATUS_BAD_REQUEST, "offer identifier required".into())),
                };
                if uid == 0 || cat_name.is_empty() {
                    return Err((
                        STATUS_BAD_REQUEST,
                        "user_id and catalogue_entry required".into(),
                    ));
                }
                let key = format!("{uid}:{cat_name}");
                db1.with_mut_snapshot(|snap| {
                    let rec = snap
                        .quasar_commerce
                        .targeted_offers
                        .entry(key)
                        .or_insert(TargetedOfferAdminRecord {
                            user_id: uid,
                            catalogue_entry_name: cat_name,
                            promoted: false,
                            retracted: false,
                            earliest_reissue_unix: 0,
                            partner_code: String::new(),
                        });
                    rec.promoted = true;
                    rec.retracted = false;
                });
                Ok(
                    pb::message("eve.store.targetaudience.offer.api.admin.PromoteResponse")
                        .must_to_any(),
                )
            }),
        );

        let db2 = Arc::clone(&db);
        router.register_handler(
            "eve.store.targetaudience.offer.api.admin.RetractRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let (uid, cat_name) = match dyn_msg.get(1) {
                    Some(DynamicValue::Message(id)) => (get_u64(id, 1), get_string(id, 2)),
                    _ => return Err((STATUS_BAD_REQUEST, "offer identifier required".into())),
                };
                let key = format!("{uid}:{cat_name}");
                let found = db2.with_mut_snapshot(|snap| {
                    let Some(rec) = snap.quasar_commerce.targeted_offers.get_mut(&key) else {
                        return false;
                    };
                    rec.retracted = true;
                    true
                });
                if !found {
                    return Err((STATUS_NOT_FOUND, "Targeted offer not found".into()));
                }
                Ok(
                    pb::message("eve.store.targetaudience.offer.api.admin.RetractResponse")
                        .must_to_any(),
                )
            }),
        );

        let db3 = Arc::clone(&db);
        router.register_handler(
            "eve.store.targetaudience.offer.api.admin.BackfillPartnerCodeRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let uid = get_u64(dyn_msg, 1);
                let cat_name = match dyn_msg.get(3) {
                    Some(DynamicValue::Message(id)) => get_string(id, 2),
                    _ => String::new(),
                };
                let partner_code = get_string(dyn_msg, 4);
                if uid == 0 || partner_code.is_empty() {
                    return Err((
                        STATUS_BAD_REQUEST,
                        "user and partner_code required".into(),
                    ));
                }
                let key = format!("{uid}:{cat_name}");
                db3.with_mut_snapshot(|snap| {
                    let rec = snap
                        .quasar_commerce
                        .targeted_offers
                        .entry(key)
                        .or_insert(TargetedOfferAdminRecord {
                            user_id: uid,
                            catalogue_entry_name: cat_name,
                            promoted: true,
                            retracted: false,
                            earliest_reissue_unix: 0,
                            partner_code: String::new(),
                        });
                    rec.partner_code = partner_code;
                });
                Ok(pb::message(
                    "eve.store.targetaudience.offer.api.admin.BackfillPartnerCodeResponse",
                )
                .must_to_any())
            }),
        );
    }
}

#[cfg(test)] #[path = "quasar_corp_structure_services_tests.rs"]
mod quasar_corp_structure_services_tests;

