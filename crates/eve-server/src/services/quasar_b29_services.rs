//! `quasar_b29_services` — Batch 29 Quasar Public Gateway Protobuf Handlers.
//!
//! Implements state-backed Quasar protobuf handlers for:
//! 1. `eve.content.orchestrator.admin.api` (`SetDefaultTenantVersionRequest`, `SetTenantVersionRequest`);
//! 2. `eve.content.orchestrator.api` (`GetDefaultTenantVersionRequest`, `GetTenantVersionRequest`, `DryRunRulesetRequest`);
//! 3. `eve.content.orchestrator.deployment.api` (`ListRequest`, `GetRequest`, `EnableRequest`, `DisableRequest`);
//! 4. `eve.content.orchestrator.deployment.contentgroup.api` (`GetRequest`);
//! 5. `eve.content.orchestrator.deployment.contenttype.api` (`GetRequest`);
//! 6. `eve.content.orchestrator.deployment.instance.admin.api` (`RemoveRequest`);
//! 7. `eve.content.orchestrator.deployment.instance.api` (`ListRequest`, `GetRequest`);
//! 8. `eve.cosmetic.market.component.listing.api` (`GetAllRequest`);
//! 9. `eve.cosmetic.market.skin.listing.api.admin` (`SetExpirationRequest`, `GetAllListedByCharacterRequest`);
//! 10. `eve.payment.admin.api` (`ProcessPaymentFilesRequest`, `GetPersonalDataRequest`);
//! 11. `eve.store.sale.admin` (`RetryFailedFulfillmentRequest`, `RetryAllFailedFulfillmentsRequest`, `DropFailedFulfillmentRequest`);
//! 12. `eve.store.targetaudience.offer.api.admin` (`PromoteRequest`, `RetractRequest`, `BackfillPartnerCodeRequest`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_fixed64_field, encode_string_field, encode_varint_field,
    DynamicQuasarMessage, DynamicValue, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

const BLOB_B29_ORCHESTRATOR: &str = "quasar_b29_orchestrator_state";
const BLOB_B29_COMMERCE_ADMIN: &str = "quasar_b29_commerce_admin_state";

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorState {
    pub default_tenant_version: SemVerSpec,
    pub tenant_version: Option<SemVerSpec>,
    pub deployments: BTreeMap<u64, DeploymentRecord>,
    pub content_groups: BTreeMap<u64, ContentGroupRecord>,
    pub content_types: BTreeMap<u64, ContentTypeRecord>,
    pub instances: Vec<DeploymentInstanceRecord>,
}

impl Default for OrchestratorState {
    fn default() -> Self {
        let mut deployments = BTreeMap::new();
        deployments.insert(
            101,
            DeploymentRecord {
                id: 101,
                name: "pirate_insurgency_deployment".into(),
                short_description: "Pirate insurgency forward operating sites".into(),
                content_group_id: 201,
                enabled: true,
                current_content_count: 1,
            },
        );
        let mut content_groups = BTreeMap::new();
        content_groups.insert(
            201,
            ContentGroupRecord {
                id: 201,
                name: "insurgency_sites".into(),
                content_type_id: 301,
                weight: 1.0,
            },
        );
        let mut content_types = BTreeMap::new();
        content_types.insert(
            301,
            ContentTypeRecord {
                id: 301,
                name: "guristas_outpost".into(),
                short_description: "Guristas forward outpost".into(),
                dungeon_id: 8001,
            },
        );
        let instances = vec![DeploymentInstanceRecord {
            uuid: b"b29-orch-inst-01".to_vec(),
            deployment_id: 101,
            content_type_id: 301,
            solar_system_id: 30_000_142,
            constellation_id: 20_000_020,
            created_unix: 1_700_000_000,
        }];
        Self {
            default_tenant_version: SemVerSpec::default(),
            tenant_version: None,
            deployments,
            content_groups,
            content_types,
            instances,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentListingRecord {
    pub uuid: Vec<u8>,
    pub component_type_id: u32,
    pub bundle_size: u64,
    pub discount_percent: u32,
    pub plex_units: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkinListingAdminRecord {
    pub uuid: Vec<u8>,
    pub seller_char_id: u64,
    pub expires_unix: i64,
    pub quantity: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedFulfillmentRecord {
    pub id: u32,
    pub user_id: u64,
    pub retries: u32,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommerceAdminState {
    pub component_listings: Vec<ComponentListingRecord>,
    pub skin_listings: Vec<SkinListingAdminRecord>,
    pub processed_merchants: Vec<u64>,
    pub failed_fulfillments: BTreeMap<u32, FailedFulfillmentRecord>,
    pub targeted_offers: HashMap<String, TargetedOfferAdminRecord>,
}

impl Default for CommerceAdminState {
    fn default() -> Self {
        let mut failed_fulfillments = BTreeMap::new();
        failed_fulfillments.insert(
            5001,
            FailedFulfillmentRecord {
                id: 5001,
                user_id: 1001,
                retries: 0,
            },
        );
        failed_fulfillments.insert(
            5002,
            FailedFulfillmentRecord {
                id: 5002,
                user_id: 1002,
                retries: 0,
            },
        );
        Self {
            component_listings: vec![ComponentListingRecord {
                uuid: b"b29-comp-list-01".to_vec(),
                component_type_id: 81001,
                bundle_size: 5,
                discount_percent: 10,
                plex_units: 25,
            }],
            skin_listings: vec![SkinListingAdminRecord {
                uuid: b"b29-skin-list-01".to_vec(),
                seller_char_id: 2112000001,
                expires_unix: 1_800_000_000,
                quantity: 1,
            }],
            processed_merchants: Vec::new(),
            failed_fulfillments,
            targeted_offers: HashMap::new(),
        }
    }
}

fn load_orch_state(db: &WorldRepository) -> OrchestratorState {
    db.with_snapshot(|snap| {
        snap.quasar_state_blobs
            .get(BLOB_B29_ORCHESTRATOR)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    })
}

fn save_orch_state(db: &WorldRepository, state: &OrchestratorState) {
    if let Ok(json) = serde_json::to_string(state) {
        let _ = db.try_with_mut_snapshot(|snap| {
            snap.quasar_state_blobs
                .insert(BLOB_B29_ORCHESTRATOR.to_string(), json);
            Ok::<(), String>(())
        });
    }
}

fn load_commerce_state(db: &WorldRepository) -> CommerceAdminState {
    db.with_snapshot(|snap| {
        snap.quasar_state_blobs
            .get(BLOB_B29_COMMERCE_ADMIN)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    })
}

fn save_commerce_state(db: &WorldRepository, state: &CommerceAdminState) {
    if let Ok(json) = serde_json::to_string(state) {
        let _ = db.try_with_mut_snapshot(|snap| {
            snap.quasar_state_blobs
                .insert(BLOB_B29_COMMERCE_ADMIN.to_string(), json);
            Ok::<(), String>(())
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

fn encode_space_location_solar_system(field_num: u32, sys_id: u32, const_id: u32, out: &mut Vec<u8>) {
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
    _sde: Arc<SdeRegistry>,
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

        router.register_handler(
            "eve.content.orchestrator.api.DryRunRulesetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let tag_name = match dyn_msg.get(1) {
                    Some(DynamicValue::Message(rs)) => get_string(rs, 1),
                    _ => String::new(),
                };
                let tag_name = if tag_name.is_empty() {
                    "highsec".to_string()
                } else {
                    tag_name
                };
                let mut eval_buf = Vec::new();
                encode_space_location_solar_system(1, 30_000_142, 20_000_020, &mut eval_buf);
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

    // 8. eve.cosmetic.market.component.listing.api.GetAllRequest
    {
        let db1 = Arc::clone(&db);
        router.register_handler(
            "eve.cosmetic.market.component.listing.api.GetAllRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let st = load_commerce_state(&db1);
                let mut out = Vec::new();
                for l in &st.component_listings {
                    let mut attr_buf = Vec::new();
                    let mut plex_buf = Vec::new();
                    encode_varint_field(1, l.plex_units, &mut plex_buf);
                    encode_bytes_field(5, &plex_buf, &mut attr_buf);
                    encode_seq_id(6, l.component_type_id as u64, &mut attr_buf);
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

    // 9. eve.cosmetic.market.skin.listing.api.admin.*
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
                    Some(DynamicValue::Message(ts)) => get_u64(ts, 1) as i64,
                    _ => 0,
                };
                let mut st = load_commerce_state(&db1);
                let listing = st
                    .skin_listings
                    .iter_mut()
                    .find(|l| l.uuid == uuid)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Skin listing not found".into()))?;
                listing.expires_unix = expires_unix;
                save_commerce_state(&db1, &st);
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
                let st = load_commerce_state(&db2);
                let mut out = Vec::new();
                for l in st
                    .skin_listings
                    .iter()
                    .filter(|l| l.seller_char_id == creator_id)
                {
                    let mut attr_buf = Vec::new();
                    encode_seq_id(1, l.seller_char_id, &mut attr_buf);
                    let mut ts_buf = Vec::new();
                    encode_varint_field(1, l.expires_unix as u64, &mut ts_buf);
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

    // 10. eve.payment.admin.api.*
    {
        let db1 = Arc::clone(&db);
        router.register_handler(
            "eve.payment.admin.api.ProcessPaymentFilesRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let merchant_id = get_u64(dyn_msg, 1);
                if merchant_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "merchant identifier required".into()));
                }
                let mut st = load_commerce_state(&db1);
                if !st.processed_merchants.contains(&merchant_id) {
                    st.processed_merchants.push(merchant_id);
                }
                save_commerce_state(&db1, &st);
                Ok(any_resp(
                    "eve.payment.admin.api.ProcessPaymentFilesResponse",
                    Vec::new(),
                ))
            }),
        );

        router.register_handler(
            "eve.payment.admin.api.GetPersonalDataRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let user_id = get_u64(dyn_msg, 1);
                if user_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "user identifier required".into()));
                }
                let mut tax_buf = Vec::new();
                encode_string_field(1, &format!("Pilot {user_id}"), &mut tax_buf);
                encode_string_field(5, "US", &mut tax_buf);

                let mut bill_buf = Vec::new();
                encode_string_field(1, &format!("Pilot {user_id}"), &mut bill_buf);
                encode_string_field(6, "US", &mut bill_buf);

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

    // 11. eve.store.sale.admin.*
    {
        let db1 = Arc::clone(&db);
        router.register_handler(
            "eve.store.sale.admin.RetryFailedFulfillmentRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let fid = get_u64(dyn_msg, 1) as u32;
                let mut st = load_commerce_state(&db1);
                let rec = st
                    .failed_fulfillments
                    .get_mut(&fid)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Failed fulfillment not found".into()))?;
                rec.retries += 1;
                save_commerce_state(&db1, &st);
                Ok(any_resp(
                    "eve.store.sale.admin.RetryFailedFulfillmentResponse",
                    Vec::new(),
                ))
            }),
        );

        let db2 = Arc::clone(&db);
        router.register_handler(
            "eve.store.sale.admin.RetryAllFailedFulfillmentsRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let mut st = load_commerce_state(&db2);
                for rec in st.failed_fulfillments.values_mut() {
                    rec.retries += 1;
                }
                save_commerce_state(&db2, &st);
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
                let mut st = load_commerce_state(&db3);
                if st.failed_fulfillments.remove(&fid).is_none() {
                    return Err((STATUS_NOT_FOUND, "Failed fulfillment not found".into()));
                }
                save_commerce_state(&db3, &st);
                Ok(any_resp(
                    "eve.store.sale.admin.DropFailedFulfillmentResponse",
                    Vec::new(),
                ))
            }),
        );
    }

    // 12. eve.store.targetaudience.offer.api.admin.*
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
                let mut st = load_commerce_state(&db1);
                let rec = st.targeted_offers.entry(key).or_insert(TargetedOfferAdminRecord {
                    user_id: uid,
                    catalogue_entry_name: cat_name,
                    promoted: false,
                    retracted: false,
                    earliest_reissue_unix: 0,
                    partner_code: String::new(),
                });
                rec.promoted = true;
                rec.retracted = false;
                save_commerce_state(&db1, &st);
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
                let mut st = load_commerce_state(&db2);
                let rec = st
                    .targeted_offers
                    .get_mut(&key)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Targeted offer not found".into()))?;
                rec.retracted = true;
                save_commerce_state(&db2, &st);
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
                let mut st = load_commerce_state(&db3);
                let rec = st.targeted_offers.entry(key).or_insert(TargetedOfferAdminRecord {
                    user_id: uid,
                    catalogue_entry_name: cat_name,
                    promoted: true,
                    retracted: false,
                    earliest_reissue_unix: 0,
                    partner_code: String::new(),
                });
                rec.partner_code = partner_code;
                save_commerce_state(&db3, &st);
                Ok(any_resp(
                    "eve.store.targetaudience.offer.api.admin.BackfillPartnerCodeResponse",
                    Vec::new(),
                ))
            }),
        );
    }
}
