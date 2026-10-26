//! `quasar_social_freelance` — Quasar Freelance & Corporation Projects Services (`Q-5`).
//!
//! Implements live request/response and notice fan-out semantics for all 31 freelance protos:
//! - `eve_public/freelance/project/api/requests.proto`
//! - `eve_public/freelance/project/api/notices.proto`
//! - `eve_public/freelance/contributionmethod/itemdelivery/api/requests.proto`
//! - `eve_public/freelance/contributionmethod/itemdelivery/api/notices.proto`
//! - `eve_public/freelance/contributionmethod/definition/api/requests.proto`
//! - `eve/freelance/project/api/requests.proto`
//! - `eve/freelance/project/api/admin/requests.proto`

use super::quasar_social_chat::{ctx_now_unix, ctx_queue_notice};
use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicValue, GoogleAny, GoogleEmpty, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

const STATUS_UNAUTHENTICATED: u32 = STATUS_UNAUTHORIZED;
const STATUS_PERMISSION_DENIED: u32 = STATUS_FORBIDDEN;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FreelancePersistedState {
    pub projects: Vec<FreelanceProjectRecord>,
    #[serde(default = "default_seq_one")]
    pub next_seq: u64,
    #[serde(default)]
    pub seeded: bool,
}

fn default_seq_one() -> u64 {
    1
}

fn legacy_serde_type_id() -> u32 {
    34
}

fn legacy_serde_batch_size() -> i64 {
    5
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FreelanceProjectRecord {
    pub uuid: Vec<u8>,
    pub owner_corp_id: u32,
    pub creator_char_id: u32,
    pub name: String,
    pub description: String,
    pub career: i32,
    pub deadline: u64,
    pub created: u64,
    pub target_progress: i64,
    pub current_progress: i64,
    pub state: i32, // 1 = Active, 2 = Completed, 3 = Closed
    pub committed_participants: Vec<u32>,
    pub contributors: HashMap<u32, (i64, i64)>, // char_id -> (progress, unredeemed_units)
    pub reward_isk: i64,
    #[serde(default = "legacy_serde_type_id")]
    pub required_type_id: u32,
    #[serde(default = "legacy_serde_batch_size")]
    pub batch_size: i64,
}

#[derive(Default)]
pub struct QuasarFreelanceState {
    pub db: Option<Arc<WorldRepository>>,
    pub projects: RwLock<HashMap<Vec<u8>, FreelanceProjectRecord>>,
    pub next_seq: AtomicU64,
}

impl QuasarFreelanceState {
    pub fn from_db(db: Arc<WorldRepository>) -> Arc<Self> {
        let (projects_map, saved_next, _already_seeded) = db.with_snapshot(|snap| {
            let mut projs = HashMap::new();
            let mut n_seq = 1u64;
            let mut seeded = false;
            if let Some(s) = snap.quasar_state_blobs.get("quasar_freelance") {
                seeded = true;
                if let Ok(st) = serde_json::from_str::<FreelancePersistedState>(s) {
                    n_seq = st.next_seq.max(1);
                    seeded = st.seeded || seeded;
                    for p in st.projects {
                        projs.insert(p.uuid.clone(), p);
                    }
                } else if let Ok(saved) = serde_json::from_str::<Vec<FreelanceProjectRecord>>(s) {
                    for p in saved {
                        projs.insert(p.uuid.clone(), p);
                    }
                } else if let Ok(saved_map) = serde_json::from_str::<HashMap<Vec<u8>, FreelanceProjectRecord>>(s) {
                    projs = saved_map;
                }
            }
            (projs, n_seq, seeded)
        });

        Arc::new(Self {
            db: Some(Arc::clone(&db)),
            projects: RwLock::new(projects_map),
            next_seq: AtomicU64::new(saved_next),
        })
    }

    pub fn sync_to_db(&self) {
        if let Some(db) = &self.db {
            let projs: Vec<FreelanceProjectRecord> = self.projects.read().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
            let payload = FreelancePersistedState {
                projects: projs,
                next_seq: self.next_seq.load(Ordering::SeqCst),
                seeded: true,
            };
            if let Ok(json) = serde_json::to_string(&payload) {
                let _ = db.try_with_mut_snapshot(|snap| {
                    snap.quasar_state_blobs.insert("quasar_freelance".into(), json);
                    Ok::<(), String>(())
                });
            }
        }
    }
}

fn encode_uuid_field(field_num: u32, uuid: &[u8], out: &mut Vec<u8>) {
    let mut id_bytes = Vec::with_capacity(20);
    encode_bytes_field(1, uuid, &mut id_bytes);
    encode_bytes_field(field_num, &id_bytes, out);
}

fn encode_char_id_field(field_num: u32, cid: u32, out: &mut Vec<u8>) {
    let mut id_bytes = Vec::with_capacity(8);
    encode_varint_field(1, cid as u64, &mut id_bytes);
    encode_bytes_field(field_num, &id_bytes, out);
}

fn encode_corp_id_field(field_num: u32, corpid: u32, out: &mut Vec<u8>) {
    let mut id_bytes = Vec::with_capacity(8);
    encode_varint_field(1, corpid as u64, &mut id_bytes);
    encode_bytes_field(field_num, &id_bytes, out);
}

fn check_corp_admin_role(
    ctx: &AuthoritativeContext,
    db: Option<&Arc<WorldRepository>>,
    cid: u32,
    corpid: u32,
) -> Result<(), (u32, String)> {
    let corp_role = ctx
        .identity
        .as_ref()
        .and_then(|id| id.roles.as_ref().map(|r| r.bitmask))
        .unwrap_or(0);
    let has_role = if let Some(db) = db {
        db.with_snapshot(|snap| {
            if let Some(corp) = snap.corporations.get(&(corpid as i64)) {
                if corp.ceo_id == cid as i64 {
                    return true;
                }
            }
            (corp_role & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_PROJECT_MANAGER)) != 0
        })
    } else {
        (corp_role & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_PROJECT_MANAGER)) != 0
    };
    if !has_role {
        return Err((STATUS_FORBIDDEN, "Insufficient corporation role".into()));
    }
    Ok(())
}

pub fn register(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    let state = QuasarFreelanceState::from_db(db);
    let sde_create = Arc::clone(&router.sde);

    // 1. CreateRequest -> CreateResponse
    let state_create = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.CreateRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_UNAUTHENTICATED, "Unauthenticated".into()));
            };
            let Some(corpid) = ctx.effective_corporation_id() else {
                return Err((STATUS_PERMISSION_DENIED, "Permission denied".into()));
            };
            check_corp_admin_role(ctx, state_create.db.as_ref(), cid, corpid)?;

            let Some(DynamicValue::String(raw_name)) = dyn_msg.get(1) else {
                return Err((STATUS_BAD_REQUEST, "Freelance project name is required".into()));
            };
            let name = raw_name.trim().to_string();
            if name.is_empty() {
                return Err((STATUS_BAD_REQUEST, "Freelance project name is required".into()));
            }
            let desc = match dyn_msg.get(2) {
                Some(DynamicValue::String(s)) => s.clone(),
                _ => String::new(),
            };
            let target_prog = match dyn_msg.get(3) {
                Some(DynamicValue::Int64(i)) if *i > 0 => *i,
                Some(DynamicValue::Uint64(u)) if *u > 0 => *u as i64,
                _ => return Err((STATUS_BAD_REQUEST, "Positive target_progress is required".into())),
            };

            let mut required_type_id: u32 = 0;
            let mut batch_size: i64 = 0;
            let parse_matcher_u32 = |m: &eve_proto::quasar::DynamicQuasarMessage| -> Option<u32> {
                if let Some(DynamicValue::Repeated(vals)) = m.get(1) {
                    for ve in vals {
                        if let DynamicValue::Message(vem) = ve {
                            if let Some(DynamicValue::Repeated(strs)) = vem.get(2) {
                                for s in strs {
                                    if let DynamicValue::String(st) = s {
                                        if let Ok(v) = st.trim().parse::<u32>() {
                                            return Some(v);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                None
            };
            if let Some(DynamicValue::Message(cfg)) = dyn_msg.get(6) {
                if let Some(DynamicValue::Repeated(params)) = cfg.get(3) {
                    for p in params {
                        if let DynamicValue::Message(pm) = p {
                            let val_u32 = pm.get(3).and_then(|o| match o {
                                DynamicValue::Message(om) => match om.get(1) {
                                    Some(DynamicValue::Repeated(sel)) => sel.iter().find_map(|x| match x {
                                        DynamicValue::String(s) => s.trim().parse::<u32>().ok(),
                                        _ => None,
                                    }),
                                    _ => None,
                                },
                                _ => None,
                            }).or_else(|| pm.get(2).and_then(|m| match m {
                                DynamicValue::Message(mm) => parse_matcher_u32(mm),
                                _ => None,
                            })).or_else(|| pm.get(5).and_then(|cid_cfg| match cid_cfg {
                                DynamicValue::Message(cm) => cm.get(1).and_then(|it| match it {
                                    DynamicValue::Message(mm) => parse_matcher_u32(mm),
                                    _ => None,
                                }),
                                _ => None,
                            }));
                            if let Some(v) = val_u32 {
                                if required_type_id == 0 {
                                    required_type_id = v;
                                } else if batch_size == 0 {
                                    batch_size = v as i64;
                                }
                            }
                        }
                    }
                }
            }
            if let Some(DynamicValue::Message(mods)) = dyn_msg.get(7) {
                if let Some(b) = mods.get(4).or_else(|| mods.get(2)).and_then(|v| match v {
                    DynamicValue::Uint64(u) => i64::try_from(*u).ok(),
                    DynamicValue::Int64(i) => Some(*i),
                    _ => None,
                }) {
                    batch_size = b;
                }
            }
            if required_type_id == 0 || batch_size <= 0 || !sde_create.types.contains_key(&required_type_id) {
                return Err((STATUS_BAD_REQUEST, "Missing or invalid item delivery required_type_id or batch_size".into()));
            }

            let budget_isk: i64 = match dyn_msg.get(10) {
                Some(DynamicValue::Message(rew)) => rew.get(1).and_then(|c| match c {
                    DynamicValue::Message(cm) => cm.get(1).and_then(|u| match u {
                        DynamicValue::Uint64(v) => i64::try_from(*v).ok().map(|v| v.saturating_mul(target_prog)),
                        DynamicValue::Int64(v) => Some(v.saturating_mul(target_prog)),
                        _ => None,
                    }),
                    _ => None,
                }).unwrap_or(50_000_000),
                _ => 50_000_000,
            };
            if budget_isk < 0 {
                return Err((STATUS_BAD_REQUEST, "Invalid project budget".into()));
            }
            let cost = eve_db::IskCents::from_isk(budget_isk);

            // Release any lock on projects BEFORE calling db.with_mut_snapshot!
            if cost.0 > 0 {
                if let Some(db) = &state_create.db {
                    let mut funded = false;
                    db.with_mut_snapshot(|snap| {
                        if let Some(corp) = snap.corporations.get_mut(&(corpid as i64)) {
                            if let Some(debit) = corp.master_wallet.checked_sub(cost) {
                                corp.master_wallet = debit;
                                funded = true;
                            }
                        }
                    });
                    if !funded {
                        return Err((STATUS_BAD_REQUEST, "Insufficient corporation funds for project escrow".into()));
                    }
                }
            }

            let seq = state_create.next_seq.fetch_add(1, Ordering::SeqCst);

            let mut uuid = [0u8; 16];
            uuid[0..8].copy_from_slice(&ctx_now_unix(ctx).to_be_bytes());
            uuid[8..16].copy_from_slice(&seq.to_be_bytes());
            let uuid_vec = uuid.to_vec();

            let proj = FreelanceProjectRecord {
                uuid: uuid_vec.clone(),
                owner_corp_id: corpid,
                creator_char_id: cid,
                name: name.clone(),
                description: desc.clone(),
                career: 1,
                deadline: ctx_now_unix(ctx) + 604800,
                created: ctx_now_unix(ctx),
                target_progress: target_prog,
                current_progress: 0,
                state: 1, // Active
                committed_participants: Vec::new(),
                contributors: HashMap::new(),
                reward_isk: (cost.0 / 100).max(0),
                required_type_id,
                batch_size,
            };

            state_create
                .projects
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(uuid_vec.clone(), proj);
            state_create.sync_to_db();

            // Notice: CreatedNotice to corporation
            let mut notice_bytes = Vec::new();
            encode_uuid_field(1, &uuid_vec, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.CreatedNotice"),
                    value: notice_bytes,
                },
                ctx_now_unix(ctx),
            );

            // Response: CreateResponse with created.project.uuid
            let mut created_msg = Vec::new();
            encode_uuid_field(1, &uuid_vec, &mut created_msg);
            let mut out = Vec::new();
            encode_bytes_field(1, &created_msg, &mut out);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.CreateResponse"),
                value: out,
            })
        }),
    );

    // 2. GetRequest -> GetResponse
    let state_get = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uuid = extract_uuid(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".to_string()))?;
            let projects = state_get.projects.read().unwrap_or_else(|e| e.into_inner());
            let Some(proj) = projects.get(&uuid).cloned() else {
                return Err((STATUS_NOT_FOUND, "Freelance project not found".to_string()));
            };

            let cid = ctx.effective_character_id().unwrap_or(0);
            let (char_prog, unredeemed) = proj.contributors.get(&cid).copied().unwrap_or((0, 0));

            // Attributes
            let mut attr = Vec::new();
            encode_corp_id_field(1, proj.owner_corp_id, &mut attr); // owner
            encode_char_id_field(2, proj.creator_char_id, &mut attr); // creator
            encode_varint_field(3, proj.state as u64, &mut attr); // state
            encode_string_field(4, &proj.name, &mut attr); // name
            encode_string_field(5, &proj.description, &mut attr); // description
            encode_varint_field(11, proj.target_progress as u64, &mut attr); // target_progress
            encode_varint_field(12, proj.current_progress as u64, &mut attr); // current_progress
            encode_varint_field(13, proj.committed_participants.len() as u64, &mut attr);

            let mut out = Vec::new();
            encode_bytes_field(1, &attr, &mut out); // project
            encode_varint_field(2, 1, &mut out); // state = Committed
            encode_varint_field(3, char_prog as u64, &mut out); // progress
            encode_varint_field(4, unredeemed as u64, &mut out); // units_unredeemed

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.GetResponse"),
                value: out,
            })
        }),
    );

    // 3. GetForManagerRequest -> GetForManagerResponse
    let state_mgr = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.GetForManagerRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let uuid = extract_uuid(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".to_string()))?;
            let projects = state_mgr.projects.read().unwrap_or_else(|e| e.into_inner());
            let Some(proj) = projects.get(&uuid).cloned() else {
                return Err((STATUS_NOT_FOUND, "Freelance project not found".to_string()));
            };

            let mut attr = Vec::new();
            encode_corp_id_field(1, proj.owner_corp_id, &mut attr);
            encode_string_field(4, &proj.name, &mut attr);
            encode_varint_field(11, proj.target_progress as u64, &mut attr);
            encode_varint_field(12, proj.current_progress as u64, &mut attr);

            let mut out = Vec::new();
            encode_bytes_field(1, &attr, &mut out);
            encode_varint_field(3, 1, &mut out);
            encode_varint_field(4, proj.current_progress as u64, &mut out);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.GetForManagerResponse"),
                value: out,
            })
        }),
    );

    // 4. GetAllActiveForCorporationRequest -> GetAllActiveForCorporationResponse
    let state_all_corp = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.GetAllActiveForCorporationRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let corpid = extract_id(dyn_msg.get(1))
                .map(|v| v as u32)
                .or_else(|| ctx.effective_corporation_id())
                .ok_or_else(|| (STATUS_FORBIDDEN, "Corporation required".to_string()))?;
            let projects = state_all_corp.projects.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            for p in projects.values().filter(|p| p.owner_corp_id == corpid && p.state == 1) {
                encode_uuid_field(1, &p.uuid, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.GetAllActiveForCorporationResponse"),
                value: out,
            })
        }),
    );

    // 5. GetAllActiveRequest -> GetAllActiveResponse
    let state_all_act = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.GetAllActiveRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let projects = state_all_act.projects.read().unwrap_or_else(|e| e.into_inner());
            let mut out = Vec::new();
            for p in projects.values().filter(|p| p.state == 1) {
                encode_uuid_field(1, &p.uuid, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.GetAllActiveResponse"),
                value: out,
            })
        }),
    );

    // 6. GetAllInactiveRequest -> GetAllInactiveResponse
    let state_all_inact = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.GetAllInactiveRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let projects = state_all_inact.projects.read().unwrap_or_else(|e| e.into_inner());
            let mut out = Vec::new();
            for p in projects.values().filter(|p| p.state != 1) {
                encode_uuid_field(1, &p.uuid, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.GetAllInactiveResponse"),
                value: out,
            })
        }),
    );

    // 7. GetAllBroadcastedRequest -> GetAllBroadcastedResponse
    let state_bcast = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.GetAllBroadcastedRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let projects = state_bcast.projects.read().unwrap_or_else(|e| e.into_inner());
            let mut bcast = Vec::new();
            for p in projects.values() {
                encode_uuid_field(1, &p.uuid, &mut bcast);
            }
            let mut out = Vec::new();
            encode_bytes_field(1, &bcast, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.GetAllBroadcastedResponse"),
                value: out,
            })
        }),
    );

    // 8. GetAllCommittedRequest -> GetAllCommittedResponse
    let state_comm = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.GetAllCommittedRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let cid = ctx
                .effective_character_id()
                .ok_or_else(|| (STATUS_FORBIDDEN, "Character required".to_string()))?;
            let projects = state_comm.projects.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            for p in projects.values().filter(|p| p.committed_participants.contains(&cid)) {
                encode_uuid_field(1, &p.uuid, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.GetAllCommittedResponse"),
                value: out,
            })
        }),
    );

    // 9. GetParticipationDetailsRequest -> GetParticipationDetailsResponse
    let state_part = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.GetParticipationDetailsRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let uuid = extract_uuid(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".to_string()))?;
            let projects = state_part.projects.read().unwrap_or_else(|e| e.into_inner());
            let p = projects
                .get(&uuid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Freelance project not found".to_string()))?;

            let mut out = Vec::new();
            for cid in &p.committed_participants {
                let mut part = Vec::new();
                encode_char_id_field(1, *cid, &mut part);
                encode_varint_field(2, 1, &mut part); // Committed
                encode_bytes_field(1, &part, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.GetParticipationDetailsResponse"),
                value: out,
            })
        }),
    );

    // 10. GetStatsRequest -> GetStatsResponse
    let state_stats = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.GetStatsRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let projects = state_stats.projects.read().unwrap_or_else(|e| e.into_inner());
            let mut requested_uuids = Vec::new();
            if let Some(DynamicValue::Repeated(list)) = dyn_msg.get(1) {
                for item in list {
                    if let Some(u) = extract_uuid(Some(item)) {
                        requested_uuids.push(u);
                    }
                }
            }

            let mut out = Vec::new();
            let entries: Vec<_> = if requested_uuids.is_empty() {
                projects.values().collect()
            } else {
                requested_uuids.iter().filter_map(|u| projects.get(u)).collect()
            };

            for p in entries {
                let mut pstat = Vec::new();
                encode_uuid_field(1, &p.uuid, &mut pstat);
                encode_varint_field(2, p.current_progress.max(0) as u64, &mut pstat);
                encode_varint_field(3, p.committed_participants.len() as u64, &mut pstat);
                encode_varint_field(4, p.committed_participants.len().max(p.contributors.len()) as u64, &mut pstat);
                encode_varint_field(5, p.contributors.len() as u64, &mut pstat);
                encode_bytes_field(1, &pstat, &mut out);
            }

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.GetStatsResponse"),
                value: out,
            })
        }),
    );

    // 11. GetCreationLimitsRequest -> GetCreationLimitsResponse
    router.register_handler(
        "eve_public.freelance.project.api.GetCreationLimitsRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_varint_field(1, 100, &mut out); // max_active_projects_per_corporation
            encode_varint_field(2, 50, &mut out); // max_active_projects_broadcast_per_system
            encode_varint_field(3, 30, &mut out); // max_character_age_days
            encode_varint_field(4, 10, &mut out); // max_broadcasting_locations_per_project
            encode_varint_field(5, 500, &mut out); // max_committed_participants_per_project
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.GetCreationLimitsResponse"),
                value: out,
            })
        }),
    );

    // 12. GetCreationPricesRequest -> GetCreationPricesResponse
    router.register_handler(
        "eve_public.freelance.project.api.GetCreationPricesRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut prices = Vec::new();
            encode_varint_field(1, 10_000_000, &mut prices); // base_price
            let mut out = Vec::new();
            encode_bytes_field(1, &prices, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.GetCreationPricesResponse"),
                value: out,
            })
        }),
    );

    // 13. GetCommitLimitsRequest -> GetCommitLimitsResponse
    router.register_handler(
        "eve_public.freelance.project.api.GetCommitLimitsRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_varint_field(1, 10, &mut out); // max_active_projects_per_participant
            encode_varint_field(2, 500, &mut out); // max_committed_participants_per_project
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.GetCommitLimitsResponse"),
                value: out,
            })
        }),
    );

    // 14. CommitRequest -> CommitResponse
    let state_commit = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.CommitRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uuid = extract_uuid(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".into()))?;
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Character required".into()));
            };
            let corpid = {
                let mut projects = state_commit.projects.write().unwrap_or_else(|e| e.into_inner());
                let p = projects
                    .get_mut(&uuid)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Freelance project not found".into()))?;
                if !p.committed_participants.contains(&cid) {
                    p.committed_participants.push(cid);
                }
                p.owner_corp_id
            };
            state_commit.sync_to_db();

            // Notice: ParticipantAddedNotice
            let mut notice_bytes = Vec::new();
            encode_uuid_field(1, &uuid, &mut notice_bytes);
            encode_char_id_field(2, cid, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.ParticipantAddedNotice"),
                    value: notice_bytes,
                },
                ctx_now_unix(ctx),
            );

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.CommitResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 15. AbandonRequest -> AbandonResponse
    let state_abandon = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.AbandonRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uuid = extract_uuid(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".into()))?;
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Character required".into()));
            };
            let corpid = {
                let mut projects = state_abandon.projects.write().unwrap_or_else(|e| e.into_inner());
                let p = projects
                    .get_mut(&uuid)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Freelance project not found".into()))?;
                p.committed_participants.retain(|&c| c != cid);
                p.owner_corp_id
            };
            state_abandon.sync_to_db();

            // Notice: ParticipantRemovedNotice
            let mut notice_bytes = Vec::new();
            encode_uuid_field(1, &uuid, &mut notice_bytes);
            encode_char_id_field(2, cid, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.ParticipantRemovedNotice"),
                    value: notice_bytes,
                },
                ctx_now_unix(ctx),
            );

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.AbandonResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 16. KickRequest -> KickResponse
    let state_kick = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.KickRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_UNAUTHENTICATED, "Unauthenticated".into()));
            };
            let caller_corp = ctx.effective_corporation_id();
            let uuid = extract_uuid(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".into()))?;
            let target_cid = extract_id(dyn_msg.get(2))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing target character ID".into()))? as u32;

            {
                let mut projects = state_kick.projects.write().unwrap_or_else(|e| e.into_inner());
                let Some(p) = projects.get_mut(&uuid) else {
                    return Err((STATUS_NOT_FOUND, "Freelance project not found".into()));
                };
                if caller_corp != Some(p.owner_corp_id) {
                    return Err((STATUS_PERMISSION_DENIED, "Permission denied".into()));
                }
                check_corp_admin_role(ctx, state_kick.db.as_ref(), cid, p.owner_corp_id)?;
                p.committed_participants.retain(|&c| c != target_cid);
            }
            state_kick.sync_to_db();

            // Notice: ParticipantKickedNotice to target character
            let mut notice_bytes = Vec::new();
            encode_uuid_field(1, &uuid, &mut notice_bytes);
            encode_char_id_field(2, target_cid, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Character(target_cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.ParticipantKickedNotice"),
                    value: notice_bytes,
                },
                ctx_now_unix(ctx),
            );

            let mut out = Vec::new();
            encode_varint_field(1, 1, &mut out); // kicked
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.KickResponse"),
                value: out,
            })
        }),
    );

    // BroadcastRequest -> BroadcastResponse
    let state_bcast_cmd = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.BroadcastRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_UNAUTHENTICATED, "Unauthenticated".into()));
            };
            let caller_corp = ctx.effective_corporation_id();
            let uuid = extract_uuid(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".into()))?;
            {
                let mut projects = state_bcast_cmd.projects.write().unwrap_or_else(|e| e.into_inner());
                let Some(p) = projects.get_mut(&uuid) else {
                    return Err((STATUS_NOT_FOUND, "Freelance project not found".into()));
                };
                if caller_corp != Some(p.owner_corp_id) {
                    return Err((STATUS_PERMISSION_DENIED, "Permission denied".into()));
                }
                check_corp_admin_role(ctx, state_bcast_cmd.db.as_ref(), cid, p.owner_corp_id)?;
                if p.state == 0 {
                    p.state = 1;
                }
            }
            state_bcast_cmd.sync_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    // CloseRequest -> CloseResponse
    let state_close_cmd = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.CloseRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_UNAUTHENTICATED, "Unauthenticated".into()));
            };
            let caller_corp = ctx.effective_corporation_id();
            let uuid = extract_uuid(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".into()))?;
            let corpid;
            let refund_isk;
            {
                let mut projects = state_close_cmd.projects.write().unwrap_or_else(|e| e.into_inner());
                let Some(p) = projects.get_mut(&uuid) else {
                    return Err((STATUS_NOT_FOUND, "Freelance project not found".into()));
                };
                if caller_corp != Some(p.owner_corp_id) {
                    return Err((STATUS_PERMISSION_DENIED, "Permission denied".into()));
                }
                check_corp_admin_role(ctx, state_close_cmd.db.as_ref(), cid, p.owner_corp_id)?;
                p.state = 3; // Closed
                corpid = p.owner_corp_id;
                refund_isk = p.reward_isk;
                p.reward_isk = 0;
            }
            state_close_cmd.sync_to_db();

            if refund_isk > 0 {
                if let Some(db) = &state_close_cmd.db {
                    db.with_mut_snapshot(|snap| {
                        if let Some(corp) = snap.corporations.get_mut(&(corpid as i64)) {
                            corp.master_wallet = corp.master_wallet.checked_add(eve_db::IskCents::from_isk(refund_isk)).unwrap_or(corp.master_wallet);
                        }
                    });
                }
            }

            let mut notice_bytes = Vec::new();
            encode_uuid_field(1, &uuid, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.ClosedNotice"),
                    value: notice_bytes,
                },
                ctx_now_unix(ctx),
            );
            Ok(GoogleEmpty.to_any())
        }),
    );

    // DeleteRequest -> DeleteResponse
    let state_del_cmd = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.DeleteRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_UNAUTHENTICATED, "Unauthenticated".into()));
            };
            let caller_corp = ctx.effective_corporation_id();
            let uuid = extract_uuid(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".into()))?;
            let corpid;
            let refund_isk;
            {
                let mut projects = state_del_cmd.projects.write().unwrap_or_else(|e| e.into_inner());
                let Some(p) = projects.get_mut(&uuid) else {
                    return Err((STATUS_NOT_FOUND, "Freelance project not found".into()));
                };
                if caller_corp != Some(p.owner_corp_id) {
                    return Err((STATUS_PERMISSION_DENIED, "Permission denied".into()));
                }
                check_corp_admin_role(ctx, state_del_cmd.db.as_ref(), cid, p.owner_corp_id)?;
                corpid = p.owner_corp_id;
                refund_isk = p.reward_isk;
                projects.remove(&uuid);
            }
            state_del_cmd.sync_to_db();

            if refund_isk > 0 {
                if let Some(db) = &state_del_cmd.db {
                    db.with_mut_snapshot(|snap| {
                        if let Some(corp) = snap.corporations.get_mut(&(corpid as i64)) {
                            corp.master_wallet = corp.master_wallet.checked_add(eve_db::IskCents::from_isk(refund_isk)).unwrap_or(corp.master_wallet);
                        }
                    });
                }
            }

            let mut notice_bytes = Vec::new();
            encode_uuid_field(1, &uuid, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.AclDeletedNotice"),
                    value: notice_bytes,
                },
                ctx_now_unix(ctx),
            );
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 17. RedeemRewardRequest -> RedeemRewardResponse
    let state_redeem = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.RedeemRewardRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uuid = extract_uuid(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".into()))?;
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Character required".into()));
            };

            let (units_paid, payout_isk) = {
                let mut projects = state_redeem.projects.write().unwrap_or_else(|e| e.into_inner());
                let p = projects
                    .get_mut(&uuid)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Freelance project not found".into()))?;
                let entry = p
                    .contributors
                    .get_mut(&cid)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "No contributor progress for freelance project".into()))?;
                let unredeemed = entry.1.max(0);
                let available_units = p.reward_isk.max(0) / 100_000;
                let units = unredeemed.min(available_units);
                if units <= 0 {
                    return Err((STATUS_BAD_REQUEST, "No redeemable freelance rewards available".into()));
                }
                let payout = units.saturating_mul(100_000);
                entry.1 = entry.1.saturating_sub(units);
                p.reward_isk = p.reward_isk.saturating_sub(payout);
                (units, payout)
            };

            let Some(db) = &state_redeem.db else {
                return Err((STATUS_BAD_REQUEST, "Database not available".into()));
            };
            let reward_cents = eve_db::IskCents::from_isk(payout_isk);
            let mut credited = false;
            db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&(cid as i64)) {
                    if let Some(new_bal) = ch.balance.checked_add(reward_cents) {
                        ch.balance = new_bal;
                        credited = true;
                        snap.append_journal(
                            cid as i64,
                            "FreelanceReward",
                            reward_cents,
                            new_bal,
                            "Freelance project reward",
                        );
                    }
                }
            });
            if !credited {
                let mut projects = state_redeem.projects.write().unwrap_or_else(|e| e.into_inner());
                if let Some(p) = projects.get_mut(&uuid) {
                    if let Some(entry) = p.contributors.get_mut(&cid) {
                        entry.1 = entry.1.saturating_add(units_paid);
                    }
                    p.reward_isk = p.reward_isk.saturating_add(payout_isk);
                }
                return Err((STATUS_BAD_REQUEST, "Unable to credit freelance reward".into()));
            }
            state_redeem.sync_to_db();

            // Notice: ParticipantRewardRedeemedNotice
            let mut notice_bytes = Vec::new();
            encode_uuid_field(1, &uuid, &mut notice_bytes);
            encode_char_id_field(2, cid, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Character(cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.ParticipantRewardRedeemedNotice"),
                    value: notice_bytes,
                },
                ctx_now_unix(ctx),
            );

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.RedeemRewardResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 18. DeliverRequest -> DeliverResponse (Item Delivery Contribution Method)
    let state_deliver = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_UNAUTHENTICATED, "Unauthenticated".into()));
            };
            let uuid = extract_uuid(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing project UUID".into()))?;

            // 1. Verify project state == 1 (Active) and caller is committed participant before touching DB
            let (target_type_id, batch_size, corpid) = {
                let projects = state_deliver.projects.read().unwrap_or_else(|e| e.into_inner());
                let Some(p) = projects.get(&uuid) else {
                    return Err((STATUS_NOT_FOUND, "Freelance project not found".into()));
                };
                if p.state != 1 || !p.committed_participants.contains(&cid) {
                    return Err((STATUS_BAD_REQUEST, "Project not active or caller not committed".into()));
                };
                (p.required_type_id, p.batch_size.max(1), p.owner_corp_id)
            };
            let req_qty = dyn_msg.get(4).and_then(|v| match v {
                DynamicValue::Uint64(u) => Some(*u as i64),
                DynamicValue::Int64(i) => Some(*i),
                DynamicValue::Uint32(u) => Some(*u as i64),
                DynamicValue::Int32(i) => Some(*i as i64),
                _ => None,
            }).unwrap_or(25).max(batch_size);

            // 2. Consume stack of required_type_id in station/structure hangar without holding projects lock
            let Some(db) = &state_deliver.db else {
                return Err((STATUS_BAD_REQUEST, "Database not available".into()));
            };
            let mut delivered_units = 0i64;
            let mut consumed_loc = 0i64;
            let mut found = false;
            db.with_mut_snapshot(|snap| {
                let Some(char_loc) = snap.characters.get(&(cid as i64)).and_then(|c| c.station_id.or(c.structure_id)) else {
                    return;
                };
                let mut to_remove = None;
                for item in snap.items.values_mut() {
                    if item.owner_id == cid as i64
                        && item.type_id == target_type_id
                        && item.quantity >= (batch_size as i32)
                        && item.flag == eve_db::InventoryFlag::Hangar
                        && item.location_id == char_loc
                    {
                        let take = ((item.quantity.min(req_qty as i32) as i64) / batch_size) * batch_size;
                        if take > 0 {
                            item.quantity -= take as i32;
                            delivered_units = take;
                            consumed_loc = char_loc;
                            found = true;
                            if item.quantity == 0 {
                                to_remove = Some(item.item_id);
                            }
                            break;
                        }
                    }
                }
                if let Some(iid) = to_remove {
                    snap.items.remove(&iid);
                }
            });
            if !found {
                return Err((STATUS_BAD_REQUEST, format!("Failed precondition: no items of type {target_type_id} available to deliver")));
            }

            // 3. Re-verify project is still active under write lock; if cancelled concurrently, refund consumed items
            let mut completed = false;
            let mut total_prog = 0i64;
            let applied = {
                let mut projects = state_deliver.projects.write().unwrap_or_else(|e| e.into_inner());
                if let Some(p) = projects.get_mut(&uuid).filter(|p| p.state == 1 && p.committed_participants.contains(&cid)) {
                    p.current_progress += delivered_units;
                    total_prog = p.current_progress;
                    let entry = p.contributors.entry(cid).or_insert((0, 0));
                    let old_total = entry.0;
                    entry.0 += delivered_units;
                    let newly_earned = (entry.0 / batch_size) - (old_total / batch_size);
                    entry.1 += newly_earned;
                    if p.current_progress >= p.target_progress {
                        p.state = 2; // Completed
                        completed = true;
                    }
                    true
                } else {
                    false
                }
            };
            if !applied {
                let _ = db.add_or_stack_item(cid as i64, consumed_loc, eve_db::InventoryFlag::Hangar, target_type_id, delivered_units);
                return Err((STATUS_BAD_REQUEST, "Project no longer active".into()));
            }
            state_deliver.sync_to_db();

            // 1. ContributionSucceededNotice
            let mut succ_bytes = Vec::new();
            encode_uuid_field(1, &uuid, &mut succ_bytes);
            encode_char_id_field(2, cid, &mut succ_bytes);
            encode_varint_field(3, delivered_units as u64, &mut succ_bytes); // units contributed
            ctx_queue_notice(
                NoticeScope::Character(cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.contributionmethod.itemdelivery.api.ContributionSucceededNotice"),
                    value: succ_bytes,
                },
                ctx_now_unix(ctx),
            );

            // 2. ProgressedNotice to corporation
            let mut prog_bytes = Vec::new();
            encode_uuid_field(1, &uuid, &mut prog_bytes);
            encode_varint_field(2, total_prog as u64, &mut prog_bytes);
            encode_varint_field(3, 1, &mut prog_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.ProgressedNotice"),
                    value: prog_bytes,
                },
                ctx_now_unix(ctx),
            );

            // 3. CompletedNotice if reached target
            if completed {
                let mut comp_bytes = Vec::new();
                encode_uuid_field(1, &uuid, &mut comp_bytes);
                ctx_queue_notice(
                    NoticeScope::Corporation(corpid),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.CompletedNotice"),
                        value: comp_bytes,
                    },
                    ctx_now_unix(ctx),
                );
            }

            // Response: DeliverResponse (deliver_result = Escrowed / Succeeded)
            let mut out = Vec::new();
            encode_varint_field(1, 1, &mut out); // success / escrowed
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.contributionmethod.itemdelivery.api.DeliverResponse"),
                value: out,
            })
        }),
    );

    // 21. eve_public.freelance.contributionmethod.definition.api.GetAllLatestRequest
    {
        let sde = Arc::clone(&router.sde);
        router.register_handler(
            "eve_public.freelance.contributionmethod.definition.api.GetAllLatestRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                Ok(encode_freelance_contribution_methods(
                    &sde,
                    &format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.contributionmethod.definition.api.GetAllLatestResponse"),
                    true,
                    1,
                ))
            }),
        );
    }

    // 22. eve_public.freelance.contributionmethod.definition.api.GetAllLatestWithinMajorRequest
    {
        let sde = Arc::clone(&router.sde);
        router.register_handler(
            "eve_public.freelance.contributionmethod.definition.api.GetAllLatestWithinMajorRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let major = dyn_msg.get(1).and_then(|v| match v {
                    DynamicValue::Uint32(n) => Some(*n),
                    DynamicValue::Int32(n) => Some(*n as u32),
                    DynamicValue::Uint64(n) => Some(*n as u32),
                    DynamicValue::Int64(n) => Some(*n as u32),
                    _ => None,
                }).unwrap_or(1);
                let include_defs = major == 0 || major == 1;
                Ok(encode_freelance_contribution_methods(
                    &sde,
                    &format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.contributionmethod.definition.api.GetAllLatestWithinMajorResponse"),
                    include_defs,
                    if major == 0 { 1 } else { major },
                ))
            }),
        );
    }
}

fn encode_freelance_contribution_methods(
    sde: &eve_sde::SdeRegistry,
    rsp_type_url: &str,
    include_defs: bool,
    major: u32,
) -> GoogleAny {
    let mut out = Vec::new();
    // `contributionmethods.fsdbinary` carries no semantic version, so the static SDE set is
    // published as `<major>.0.0` (minor / patch have no source data to vary from).
    let mut ver_bytes = Vec::new();
    encode_varint_field(1, major as u64, &mut ver_bytes);
    encode_varint_field(2, 0, &mut ver_bytes);
    encode_varint_field(3, 0, &mut ver_bytes);
    encode_bytes_field(1, &ver_bytes, &mut out);

    // Definitions come exclusively from SDE `res:/staticdata/contributionmethods.fsdbinary`; when
    // the table cannot be loaded the response carries no definitions (no embedded fallback table).
    if include_defs {
        if let Ok(methods) = sde.contribution_methods() {
            for (name, val) in &methods {
                encode_single_freelance_def(name, val, &mut out);
            }
        }
    }

    GoogleAny {
        type_url: rsp_type_url.to_string(),
        value: out,
    }
}

fn encode_loc_id(field: u32, val: &serde_json::Value, key: &str, out: &mut Vec<u8>) {
    if let Some(id) = val.get(key).and_then(|v| v.as_u64()).filter(|v| *v > 0) {
        let mut b = Vec::new();
        encode_varint_field(1, id, &mut b);
        encode_bytes_field(field, &b, out);
    }
}

fn encode_name_id(field: u32, name: &str, out: &mut Vec<u8>) {
    if !name.is_empty() {
        let mut b = Vec::new();
        encode_string_field(1, name, &mut b);
        encode_bytes_field(field, &b, out);
    }
}

/// Encodes one SDE contribution method record as
/// `eve_public.freelance.contributionmethod.definition.api.*Response.Definition`.
fn encode_single_freelance_def(name: &str, val: &serde_json::Value, out: &mut Vec<u8>) {
    let mut id_bytes = Vec::new();
    encode_string_field(1, name, &mut id_bytes);

    let mut attr = Vec::new();
    encode_loc_id(1, val, "titleID", &mut attr);
    encode_loc_id(2, val, "descriptionID", &mut attr);
    encode_loc_id(3, val, "progressDescriptionID", &mut attr);
    encode_loc_id(4, val, "targetValueDescriptionID", &mut attr);
    encode_loc_id(5, val, "rewardingDescriptionID", &mut attr);
    encode_name_id(6, val.get("icon").and_then(|v| v.as_str()).unwrap_or(""), &mut attr);
    for tag in val.get("contentTags").and_then(|v| v.as_array()).into_iter().flatten() {
        encode_name_id(7, tag.as_str().unwrap_or(""), &mut attr);
    }
    // SDE parameters -> `definition.Parameter { matcher = MatcherParameter }` (key, type, icon,
    // optional, max_entries). Their `title` / `info` are label paths, not message ids, so they
    // are not encoded as `localization.message.Identifier`.
    for p in val.get("parameters").and_then(|v| v.as_array()).into_iter().flatten() {
        let mut m = Vec::new();
        encode_string_field(1, p.get("parameterKey").and_then(|v| v.as_str()).unwrap_or(""), &mut m);
        encode_string_field(2, p.get("parameterType").and_then(|v| v.as_str()).unwrap_or(""), &mut m);
        encode_name_id(5, p.get("icon").and_then(|v| v.as_str()).unwrap_or(""), &mut m);
        let required = p.get("isRequired").and_then(|v| v.as_u64()).unwrap_or(0) != 0;
        encode_varint_field(8, u64::from(!required), &mut m);
        if let Some(max) = p.get("maxEntries").and_then(|v| v.as_u64()) {
            encode_varint_field(9, max, &mut m);
        }
        let mut param = Vec::new();
        encode_bytes_field(1, &m, &mut param);
        encode_bytes_field(11, &param, &mut attr);
    }

    let mut def_bytes = Vec::new();
    encode_bytes_field(1, &id_bytes, &mut def_bytes);
    encode_bytes_field(2, &attr, &mut def_bytes);
    encode_bytes_field(2, &def_bytes, out);
}

fn extract_uuid(val: Option<&DynamicValue>) -> Option<Vec<u8>> {
    match val {
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::Bytes(b)) => Some(b.clone()),
            _ => None,
        },
        Some(DynamicValue::Bytes(b)) => Some(b.clone()),
        _ => None,
    }
}

fn extract_id(val: Option<&DynamicValue>) -> Option<u64> {
    match val {
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::Uint64(u)) => Some(*u),
            Some(DynamicValue::Int64(i)) => Some(*i as u64),
            Some(DynamicValue::Uint32(u)) => Some(*u as u64),
            Some(DynamicValue::Int32(i)) => Some(*i as u64),
            _ => None,
        },
        Some(DynamicValue::Uint64(u)) => Some(*u),
        Some(DynamicValue::Int64(i)) => Some(*i as u64),
        Some(DynamicValue::Uint32(u)) => Some(*u as u64),
        Some(DynamicValue::Int32(i)) => Some(*i as u64),
        _ => None,
    }
}
