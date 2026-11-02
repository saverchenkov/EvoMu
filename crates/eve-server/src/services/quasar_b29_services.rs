//! `quasar_b29_services` — Batch 29 Quasar Public Gateway Admin & Orchestrator Handlers.
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
use crate::services::quasar_payment_store_types::{record_plex_transaction, PaymentReceiptRecord};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_fixed64_field, encode_string_field, encode_varint_field,
    DynamicQuasarMessage, DynamicValue, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

const BLOB_COSMETICS_MARKET: &str = "quasar_cosmetics_market";
const BLOB_ORCHESTRATOR: &str = "content_orchestrator_state";
const BLOB_FAILED_FULFILLMENTS: &str = "store_failed_fulfillments";
const BLOB_TARGETED_OFFERS: &str = "store_targeted_offers";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemVerSpec {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Default for SemVerSpec {
    fn default() -> Self {
        Self {
            major: 1,
            minor: 0,
            patch: 0,
        }
    }
}

impl SemVerSpec {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.major > 0 {
            encode_varint_field(1, self.major as u64, &mut out);
        }
        if self.minor > 0 {
            encode_varint_field(2, self.minor as u64, &mut out);
        }
        if self.patch > 0 {
            encode_varint_field(3, self.patch as u64, &mut out);
        }
        out
    }

    pub fn from_dyn(msg: &DynamicQuasarMessage) -> Self {
        Self {
            major: get_u64(msg, 1) as u32,
            minor: get_u64(msg, 2) as u32,
            patch: get_u64(msg, 3) as u32,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentRecord {
    pub id: u64,
    pub name: String,
    pub short_description: String,
    pub content_group_id: u64,
    pub enabled: bool,
    pub current_content_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentGroupRecord {
    pub id: u64,
    pub name: String,
    pub content_type_id: u64,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentTypeRecord {
    pub id: u64,
    pub name: String,
    pub short_description: String,
    pub dungeon_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentInstanceRecord {
    pub uuid: Vec<u8>,
    pub deployment_id: u64,
    pub content_type_id: u64,
    pub solar_system_id: u32,
    pub constellation_id: u32,
    pub created_unix: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrchestratorState {
    pub default_tenant_version: SemVerSpec,
    pub tenant_version: Option<SemVerSpec>,
    pub deployments: BTreeMap<u64, DeploymentRecord>,
    pub content_groups: BTreeMap<u64, ContentGroupRecord>,
    pub content_types: BTreeMap<u64, ContentTypeRecord>,
    pub instances: Vec<DeploymentInstanceRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedFulfillmentRecord {
    pub id: u32,
    pub user_id: u64,
    pub plex_units: i64,
    pub retries: u32,
    pub fulfilled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetedOfferAdminRecord {
    pub user_id: u64,
    pub catalogue_entry_name: String,
    pub promoted: bool,
    pub retracted: bool,
    pub earliest_reissue_unix: i64,
    pub partner_code: String,
}

fn load_orch_state(db: &WorldRepository) -> OrchestratorState {
    db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get(BLOB_ORCHESTRATOR)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    })
}

fn save_orch_state(db: &WorldRepository, state: &OrchestratorState) {
    if let Ok(json) = serde_json::to_string(state) {
        db.with_mut_snapshot(|snap| {
            snap.service_state_blobs
                .insert(BLOB_ORCHESTRATOR.to_string(), json);
        });
    }
}

fn any_resp(type_name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value,
    }
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

fn encode_seq_id(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_varint_field(1, id, &mut sub);
    encode_bytes_field(field_num, &sub, out);
}

fn encode_uuid_id(field_num: u32, uuid: &[u8], out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_bytes_field(1, uuid, &mut sub);
    encode_bytes_field(field_num, &sub, out);
}

fn encode_space_location_solar_system(
    field_num: u32,
    sys_id: u32,
    const_id: u32,
    out: &mut Vec<u8>,
) {
    let mut const_msg = Vec::new();
    encode_seq_id(1, const_id as u64, &mut const_msg);
    encode_seq_id(2, sys_id as u64, &mut const_msg);
    let mut loc_msg = Vec::new();
    encode_bytes_field(2, &const_msg, &mut loc_msg);
    encode_bytes_field(field_num, &loc_msg, out);
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
                Ok(any_resp(
                    "eve.content.orchestrator.admin.api.SetDefaultTenantVersionResponse",
                    Vec::new(),
                ))
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
                Ok(any_resp(
                    "eve.content.orchestrator.admin.api.SetTenantVersionResponse",
                    Vec::new(),
                ))
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
                let mut out = Vec::new();
                encode_bytes_field(1, &st.default_tenant_version.encode(), &mut out);
                Ok(any_resp(
                    "eve.content.orchestrator.api.GetDefaultTenantVersionResponse",
                    out,
                ))
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
                let mut out = Vec::new();
                encode_bytes_field(1, &ver.encode(), &mut out);
                Ok(any_resp(
                    "eve.content.orchestrator.api.GetTenantVersionResponse",
                    out,
                ))
            }),
        );

        let sde_dry = Arc::clone(&sde);
        router.register_handler(
            "eve.content.orchestrator.api.DryRunRulesetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let raw_tag = match dyn_msg.get(1) {
                    Some(DynamicValue::Message(rs)) => get_string(rs, 1),
                    _ => String::new(),
                };
                let tag_name = if raw_tag.trim().is_empty() {
                    "highsec".to_string()
                } else {
                    raw_tag.trim().to_string()
                };
                let mut sys_ids: Vec<u32> = sde_dry.solar_systems.keys().copied().collect();
                sys_ids.sort_unstable();
                let matched_sys = sys_ids.into_iter().find_map(|sid| {
                    let info = sde_dry.solar_systems.get(&sid)?;
                    let ok = match tag_name.as_str() {
                        "highsec" => info.security_status >= 0.45,
                        "lowsec" => (0.05..0.45).contains(&info.security_status),
                        "nullsec" => info.security_status < 0.05 && sid < 31_000_000,
                        "wormhole" => sid >= 31_000_000,
                        other => other.parse::<u32>().is_ok_and(|id| id == sid),
                    };
                    ok.then_some((sid, info.constellation_id))
                });
                let Some((sys_id, const_id)) = matched_sys else {
                    return Err((
                        STATUS_NOT_FOUND,
                        format!("No SDE solar system matches ruleset tag '{tag_name}'"),
                    ));
                };
                let mut eval_buf = Vec::new();
                encode_space_location_solar_system(1, sys_id, const_id, &mut eval_buf);
                let mut tid = Vec::new();
                encode_string_field(1, &tag_name, &mut tid);
                encode_bytes_field(2, &tid, &mut eval_buf);

                let mut out = Vec::new();
                encode_bytes_field(1, &eval_buf, &mut out);
                Ok(any_resp(
                    "eve.content.orchestrator.api.DryRunRulesetResponse",
                    out,
                ))
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
                let mut out = Vec::new();
                for dep in st.deployments.values() {
                    let mut dbuf = Vec::new();
                    encode_seq_id(1, dep.id, &mut dbuf);
                    encode_string_field(2, &dep.name, &mut dbuf);
                    encode_string_field(3, &dep.short_description, &mut dbuf);
                    encode_varint_field(4, u64::from(dep.enabled), &mut dbuf);
                    encode_bytes_field(1, &dbuf, &mut out);
                }
                Ok(any_resp(
                    "eve.content.orchestrator.deployment.api.ListResponse",
                    out,
                ))
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
                let mut attr_buf = Vec::new();
                encode_string_field(1, &dep.name, &mut attr_buf);
                encode_seq_id(2, dep.content_group_id, &mut attr_buf);
                encode_varint_field(6, dep.current_content_count as u64, &mut attr_buf);
                let mut out = Vec::new();
                encode_bytes_field(1, &attr_buf, &mut out);
                Ok(any_resp(
                    "eve.content.orchestrator.deployment.api.GetResponse",
                    out,
                ))
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
                Ok(any_resp(
                    "eve.content.orchestrator.deployment.api.EnableResponse",
                    Vec::new(),
                ))
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
                Ok(any_resp(
                    "eve.content.orchestrator.deployment.api.DisableResponse",
                    Vec::new(),
                ))
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
                let mut wt_buf = Vec::new();
                encode_seq_id(1, cg.content_type_id, &mut wt_buf);
                encode_fixed64_field(2, cg.weight.to_bits(), &mut wt_buf);
                let mut attr_buf = Vec::new();
                encode_string_field(2, &cg.name, &mut attr_buf);
                encode_bytes_field(3, &wt_buf, &mut attr_buf);
                let mut out = Vec::new();
                encode_bytes_field(1, &attr_buf, &mut out);
                Ok(any_resp(
                    "eve.content.orchestrator.deployment.contentgroup.api.GetResponse",
                    out,
                ))
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
                let mut attr_buf = Vec::new();
                encode_string_field(1, &ct.name, &mut attr_buf);
                encode_string_field(2, &ct.short_description, &mut attr_buf);
                encode_seq_id(3, ct.dungeon_id, &mut attr_buf);
                let mut out = Vec::new();
                encode_bytes_field(1, &attr_buf, &mut out);
                Ok(any_resp(
                    "eve.content.orchestrator.deployment.contenttype.api.GetResponse",
                    out,
                ))
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
                Ok(any_resp(
                    "eve.content.orchestrator.deployment.instance.admin.api.RemoveResponse",
                    Vec::new(),
                ))
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
                let mut out = Vec::new();
                for inst in st
                    .instances
                    .iter()
                    .filter(|i| dep_id == 0 || i.deployment_id == dep_id)
                {
                    encode_uuid_id(1, &inst.uuid, &mut out);
                }
                Ok(any_resp(
                    "eve.content.orchestrator.deployment.instance.api.ListResponse",
                    out,
                ))
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
                let mut attr_buf = Vec::new();
                encode_seq_id(1, inst.deployment_id, &mut attr_buf);
                encode_seq_id(3, inst.content_type_id, &mut attr_buf);
                encode_space_location_solar_system(
                    4,
                    inst.solar_system_id,
                    inst.constellation_id,
                    &mut attr_buf,
                );
                let mut out = Vec::new();
                encode_bytes_field(1, &attr_buf, &mut out);
                Ok(any_resp(
                    "eve.content.orchestrator.deployment.instance.api.GetResponse",
                    out,
                ))
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
                let mut out = Vec::new();
                for l in data.component_listings.values() {
                    let mut attr_buf = Vec::new();
                    let mut plex_buf = Vec::new();
                    encode_varint_field(1, l.price_plex_cents.unwrap_or(0), &mut plex_buf);
                    encode_bytes_field(5, &plex_buf, &mut attr_buf);
                    encode_seq_id(6, l.component_item_type_id as u64, &mut attr_buf);
                    encode_varint_field(7, l.bundle_size, &mut attr_buf);
                    encode_varint_field(8, l.discount_percent as u64, &mut attr_buf);

                    let mut entry_buf = Vec::new();
                    encode_uuid_id(1, &l.uuid, &mut entry_buf);
                    encode_bytes_field(2, &attr_buf, &mut entry_buf);
                    encode_bytes_field(1, &entry_buf, &mut out);
                }
                Ok(any_resp(
                    "eve.cosmetic.market.component.listing.api.GetAllResponse",
                    out,
                ))
            }),
        );
    }

    // 9. eve.cosmetic.market.skin.listing.api.admin.* -> canonical CosmeticsMarketData
    {
        let db1 = Arc::clone(&db);
        router.register_handler(
            "eve.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let uuid = get_bytes(dyn_msg, 1);
                if uuid.is_empty() {
                    return Err((STATUS_BAD_REQUEST, "listing uuid required".into()));
                }
                let expires_unix = match dyn_msg.get(2) {
                    Some(DynamicValue::Message(ts)) => get_u64(ts, 1),
                    _ => 0,
                };
                let updated = db1.with_mut_snapshot(|snap| {
                    let mut data: CosmeticsMarketData = snap
                        .quasar_state_blobs
                        .get(BLOB_COSMETICS_MARKET)
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default();
                    let found = data.skin_listings.values_mut().find(|l| l.uuid.as_slice() == uuid.as_slice());
                    if let Some(listing) = found {
                        listing.expires_unix = expires_unix;
                        if let Ok(json) = serde_json::to_string(&data) {
                            snap.quasar_state_blobs
                                .insert(BLOB_COSMETICS_MARKET.to_string(), json);
                        }
                        true
                    } else {
                        false
                    }
                });
                if !updated {
                    return Err((STATUS_NOT_FOUND, "Skin listing not found".into()));
                }
                Ok(any_resp(
                    "eve.cosmetic.market.skin.listing.api.admin.SetExpirationResponse",
                    Vec::new(),
                ))
            }),
        );

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
                let mut out = Vec::new();
                for l in data
                    .skin_listings
                    .values()
                    .filter(|l| l.seller_id as u64 == creator_id)
                {
                    let mut attr_buf = Vec::new();
                    encode_seq_id(1, l.seller_id as u64, &mut attr_buf);
                    let mut ts_buf = Vec::new();
                    encode_varint_field(1, l.expires_unix, &mut ts_buf);
                    encode_bytes_field(3, &ts_buf, &mut attr_buf);
                    encode_varint_field(8, l.quantity as u64, &mut attr_buf);

                    let mut entry_buf = Vec::new();
                    encode_uuid_id(1, &l.uuid, &mut entry_buf);
                    encode_bytes_field(2, &attr_buf, &mut entry_buf);
                    encode_bytes_field(1, &entry_buf, &mut out);
                }
                Ok(any_resp(
                    "eve.cosmetic.market.skin.listing.api.admin.GetAllListedByCharacterResponse",
                    out,
                ))
            }),
        );
    }

    // 10. eve.payment.admin.api.* -> canonical payment_receipts + accounts/characters
    {
        let db1 = Arc::clone(&db);
        router.register_handler(
            "eve.payment.admin.api.ProcessPaymentFilesRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let merchant_id = get_u64(dyn_msg, 1);
                if merchant_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "merchant identifier required".into()));
                }
                let uid = ctx.effective_user_id().unwrap_or(0) as u64;
                let now_unix = ctx.now_unix();
                db1.with_mut_snapshot(|snap| {
                    let mut receipts: Vec<PaymentReceiptRecord> = snap
                        .service_state_blobs
                        .get("payment_receipts")
                        .and_then(|j| serde_json::from_str(j).ok())
                        .unwrap_or_default();
                    if !receipts.iter().any(|r| r.receipt_id == merchant_id) {
                        receipts.push(PaymentReceiptRecord {
                            receipt_id: merchant_id,
                            user_id: uid,
                            offer_id: format!("merchant-{merchant_id}"),
                            amount_in_cents: 0,
                            currency: "USD".to_string(),
                            timestamp_unix: now_unix,
                        });
                        if let Ok(s) = serde_json::to_string(&receipts) {
                            snap.service_state_blobs
                                .insert("payment_receipts".to_string(), s);
                        }
                    }
                });
                Ok(any_resp(
                    "eve.payment.admin.api.ProcessPaymentFilesResponse",
                    Vec::new(),
                ))
            }),
        );

        let db2 = Arc::clone(&db);
        router.register_handler(
            "eve.payment.admin.api.GetPersonalDataRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let user_id = get_u64(dyn_msg, 1);
                if user_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "user identifier required".into()));
                }
                let resolved = db2.with_snapshot(|snap| {
                    let acc = snap.accounts.values().find(|a| a.user_id == user_id)?;
                    let ch = snap.characters.values().find(|c| c.user_id == user_id);
                    let display_name = ch
                        .map(|c| c.name.clone())
                        .unwrap_or_else(|| acc.username.clone());
                    let region_code = ch
                        .map(|c| format!("REG-{}", c.region_id))
                        .unwrap_or_else(|| "UNASSIGNED".to_string());
                    Some((display_name, region_code))
                });
                let Some((display_name, region_code)) = resolved else {
                    return Err((STATUS_NOT_FOUND, format!("User {user_id} not found")));
                };
                let mut tax_buf = Vec::new();
                encode_string_field(1, &display_name, &mut tax_buf);
                encode_string_field(5, &region_code, &mut tax_buf);

                let mut bill_buf = Vec::new();
                encode_string_field(1, &display_name, &mut bill_buf);
                encode_string_field(6, &region_code, &mut bill_buf);

                let mut out = Vec::new();
                encode_bytes_field(3, &tax_buf, &mut out);
                encode_bytes_field(4, &bill_buf, &mut out);
                Ok(any_resp(
                    "eve.payment.admin.api.GetPersonalDataResponse",
                    out,
                ))
            }),
        );
    }

    // 11. eve.store.sale.admin.* -> canonical store_failed_fulfillments + plex_balances
    {
        let db1 = Arc::clone(&db);
        router.register_handler(
            "eve.store.sale.admin.RetryFailedFulfillmentRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let fid = get_u64(dyn_msg, 1) as u32;
                let now_unix = ctx.now_unix();
                let found = db1.with_mut_snapshot(|snap| {
                    let mut map: BTreeMap<u32, FailedFulfillmentRecord> = snap
                        .service_state_blobs
                        .get(BLOB_FAILED_FULFILLMENTS)
                        .and_then(|j| serde_json::from_str(j).ok())
                        .unwrap_or_default();
                    let Some(rec) = map.get_mut(&fid) else {
                        return false;
                    };
                    rec.retries += 1;
                    if !rec.fulfilled && rec.plex_units > 0 {
                        rec.fulfilled = true;
                        let bal = snap.account_plex_vault.entry(rec.user_id).or_insert(0);
                        *bal += rec.plex_units;
                        let new_bal = *bal;
                        snap.next_entity_id += 1;
                        let tx_id = snap.next_entity_id as u64;
                        let uid = rec.user_id;
                        let amt = rec.plex_units;
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
                    if let Ok(s) = serde_json::to_string(&map) {
                        snap.service_state_blobs
                            .insert(BLOB_FAILED_FULFILLMENTS.to_string(), s);
                    }
                    true
                });
                if !found {
                    return Err((STATUS_NOT_FOUND, "Failed fulfillment not found".into()));
                }
                Ok(any_resp(
                    "eve.store.sale.admin.RetryFailedFulfillmentResponse",
                    Vec::new(),
                ))
            }),
        );

        let db2 = Arc::clone(&db);
        router.register_handler(
            "eve.store.sale.admin.RetryAllFailedFulfillmentsRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let now_unix = ctx.now_unix();
                db2.with_mut_snapshot(|snap| {
                    let mut map: BTreeMap<u32, FailedFulfillmentRecord> = snap
                        .service_state_blobs
                        .get(BLOB_FAILED_FULFILLMENTS)
                        .and_then(|j| serde_json::from_str(j).ok())
                        .unwrap_or_default();
                    for rec in map.values_mut() {
                        rec.retries += 1;
                        if !rec.fulfilled && rec.plex_units > 0 {
                            rec.fulfilled = true;
                            let bal = snap.account_plex_vault.entry(rec.user_id).or_insert(0);
                            *bal += rec.plex_units;
                            let new_bal = *bal;
                            snap.next_entity_id += 1;
                            let tx_id = snap.next_entity_id as u64;
                            record_plex_transaction(
                                snap,
                                tx_id,
                                rec.user_id,
                                rec.plex_units,
                                new_bal,
                                &format!("Admin retry-all fulfillment #{}", rec.id),
                                now_unix,
                            );
                        }
                    }
                    if let Ok(s) = serde_json::to_string(&map) {
                        snap.service_state_blobs
                            .insert(BLOB_FAILED_FULFILLMENTS.to_string(), s);
                    }
                });
                Ok(any_resp(
                    "eve.store.sale.admin.RetryAllFailedFulfillmentsResponse",
                    Vec::new(),
                ))
            }),
        );

        let db3 = Arc::clone(&db);
        router.register_handler(
            "eve.store.sale.admin.DropFailedFulfillmentRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let fid = get_u64(dyn_msg, 1) as u32;
                let removed = db3.with_mut_snapshot(|snap| {
                    let mut map: BTreeMap<u32, FailedFulfillmentRecord> = snap
                        .service_state_blobs
                        .get(BLOB_FAILED_FULFILLMENTS)
                        .and_then(|j| serde_json::from_str(j).ok())
                        .unwrap_or_default();
                    let was_present = map.remove(&fid).is_some();
                    if was_present {
                        if let Ok(s) = serde_json::to_string(&map) {
                            snap.service_state_blobs
                                .insert(BLOB_FAILED_FULFILLMENTS.to_string(), s);
                        }
                    }
                    was_present
                });
                if !removed {
                    return Err((STATUS_NOT_FOUND, "Failed fulfillment not found".into()));
                }
                Ok(any_resp(
                    "eve.store.sale.admin.DropFailedFulfillmentResponse",
                    Vec::new(),
                ))
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
                    let mut map: BTreeMap<String, TargetedOfferAdminRecord> = snap
                        .service_state_blobs
                        .get(BLOB_TARGETED_OFFERS)
                        .and_then(|j| serde_json::from_str(j).ok())
                        .unwrap_or_default();
                    let rec = map.entry(key).or_insert(TargetedOfferAdminRecord {
                        user_id: uid,
                        catalogue_entry_name: cat_name,
                        promoted: false,
                        retracted: false,
                        earliest_reissue_unix: 0,
                        partner_code: String::new(),
                    });
                    rec.promoted = true;
                    rec.retracted = false;
                    if let Ok(s) = serde_json::to_string(&map) {
                        snap.service_state_blobs
                            .insert(BLOB_TARGETED_OFFERS.to_string(), s);
                    }
                });
                Ok(any_resp(
                    "eve.store.targetaudience.offer.api.admin.PromoteResponse",
                    Vec::new(),
                ))
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
                    let mut map: BTreeMap<String, TargetedOfferAdminRecord> = snap
                        .service_state_blobs
                        .get(BLOB_TARGETED_OFFERS)
                        .and_then(|j| serde_json::from_str(j).ok())
                        .unwrap_or_default();
                    let Some(rec) = map.get_mut(&key) else {
                        return false;
                    };
                    rec.retracted = true;
                    if let Ok(s) = serde_json::to_string(&map) {
                        snap.service_state_blobs
                            .insert(BLOB_TARGETED_OFFERS.to_string(), s);
                    }
                    true
                });
                if !found {
                    return Err((STATUS_NOT_FOUND, "Targeted offer not found".into()));
                }
                Ok(any_resp(
                    "eve.store.targetaudience.offer.api.admin.RetractResponse",
                    Vec::new(),
                ))
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
                    let mut map: BTreeMap<String, TargetedOfferAdminRecord> = snap
                        .service_state_blobs
                        .get(BLOB_TARGETED_OFFERS)
                        .and_then(|j| serde_json::from_str(j).ok())
                        .unwrap_or_default();
                    let rec = map.entry(key).or_insert(TargetedOfferAdminRecord {
                        user_id: uid,
                        catalogue_entry_name: cat_name,
                        promoted: true,
                        retracted: false,
                        earliest_reissue_unix: 0,
                        partner_code: String::new(),
                    });
                    rec.partner_code = partner_code;
                    if let Ok(s) = serde_json::to_string(&map) {
                        snap.service_state_blobs
                            .insert(BLOB_TARGETED_OFFERS.to_string(), s);
                    }
                });
                Ok(any_resp(
                    "eve.store.targetaudience.offer.api.admin.BackfillPartnerCodeResponse",
                    Vec::new(),
                ))
            }),
        );
    }
}
