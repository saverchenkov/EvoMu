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

use super::quasar_freelance_defs::{
    encode_next_page, extract_currency_isk, paginate_slice, register_item_delivery_and_definitions,
};
use super::quasar_social_chat::{ctx_now_unix, ctx_queue_notice};
use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicValue, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

const STATUS_UNAUTHENTICATED: u32 = STATUS_UNAUTHORIZED;
const STATUS_PERMISSION_DENIED: u32 = STATUS_FORBIDDEN;

use super::social_corp_fleet::social_corp_projects::{
    self as corp_projects, CanonicalProjectSpec, SERVER_TRACKED_MATCHERS,
};

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
    pub state: i32, // 1 = Active, 2 = Completed, 3 = Closed, 4 = Expired
    pub committed_participants: Vec<u32>,
    pub contributors: HashMap<u32, (i64, i64)>, // char_id -> (progress, unredeemed_units)
    pub reward_isk: i64,
    #[serde(default)]
    pub initial_reward_isk: Option<i64>,
    #[serde(default = "legacy_serde_type_id")]
    pub required_type_id: u32,
    #[serde(default = "legacy_serde_batch_size")]
    pub batch_size: i64,
}

fn build_freelance_map_from_snap(
    snap: &eve_db::WorldStateSnapshot,
) -> (HashMap<Vec<u8>, FreelanceProjectRecord>, u64) {
    let mut projs = HashMap::new();
    let mut max_pid = 100i64;
    for (&pid, p) in &snap.corp_projects {
        max_pid = max_pid.max(pid);
        if !snap.alliance_war_state.project_meta.get(&pid).is_some_and(|m| m.1) {
            continue;
        }
        let det = snap
            .alliance_war_state
            .project_details
            .get(&pid)
            .cloned()
            .unwrap_or_default();
        let uuid = if det.uuid_bytes.len() == 16 {
            det.uuid_bytes.clone()
        } else {
            corp_projects::freelance_uuid(det.created_unix, pid).to_vec()
        };
        let state = if det.expired || det.status == "Expired" {
            4
        } else if det.closed || det.status == "Closed" {
            3
        } else if p.current_quantity >= p.target_quantity || det.status == "Completed" {
            2
        } else {
            1
        };
        let batch = det.batch_size.max(1);
        let mut contributors = HashMap::new();
        for (&cid_i, &prog_u) in &det.char_contributions {
            if cid_i <= 0 {
                continue;
            }
            let total_batches = (prog_u / batch) as i64;
            let redeemed = det.contributions_by_char.get(&cid_i).copied().unwrap_or(0) as i64;
            contributors.insert(
                cid_i as u32,
                (prog_u as i64, (total_batches - redeemed).max(0)),
            );
        }
        let committed_participants = det
            .committed_participants
            .iter()
            .filter_map(|&c| u32::try_from(c).ok())
            .collect();
        let remaining_reward = (p.escrow_remaining.raw_cents() / 100).max(0);
        let init_reward = if det.initial_escrow.raw_cents() > 0 {
            det.initial_escrow.raw_cents() / 100
        } else {
            remaining_reward
        };
        projs.insert(
            uuid.clone(),
            FreelanceProjectRecord {
                uuid,
                owner_corp_id: p.corporation_id.max(0) as u32,
                creator_char_id: det.creator_id.max(0) as u32,
                name: p.name.clone(),
                description: det.description,
                career: 1,
                deadline: det.expires_unix.unwrap_or(det.created_unix.saturating_add(604_800)),
                created: det.created_unix,
                target_progress: p.target_quantity as i64,
                current_progress: p.current_quantity as i64,
                state,
                committed_participants,
                contributors,
                reward_isk: remaining_reward,
                initial_reward_isk: Some(init_reward),
                required_type_id: det.filter_id.unwrap_or(0).max(0) as u32,
                batch_size: batch as i64,
            },
        );
    }
    (projs, (max_pid + 1) as u64)
}

#[derive(Default)]
pub struct QuasarFreelanceState {
    pub db: Option<Arc<WorldRepository>>,
    pub projects: RwLock<HashMap<Vec<u8>, FreelanceProjectRecord>>,
    pub next_seq: AtomicU64,
}

impl QuasarFreelanceState {
    pub fn from_db(db: Arc<WorldRepository>) -> Arc<Self> {
        let (projects_map, next_id) = db.with_snapshot(build_freelance_map_from_snap);
        Arc::new(Self {
            db: Some(db),
            projects: RwLock::new(projects_map),
            next_seq: AtomicU64::new(next_id.max(1)),
        })
    }

    pub fn refresh_from_db(&self, now_unix: u64) {
        if let Some(ref db) = self.db {
            if now_unix > 0 {
                let _ = db.with_mut_snapshot(|s| corp_projects::sweep_expired_corp_projects_in_snap(s, now_unix));
            }
            let (map, next_id) = db.with_snapshot(build_freelance_map_from_snap);
            *self.projects.write().unwrap_or_else(|e| e.into_inner()) = map;
            let _ = self.next_seq.fetch_max(next_id, Ordering::SeqCst);
        }
    }

    pub fn sync_to_db(&self) {
        self.refresh_from_db(0);
    }

    fn owner_corp_of(&self, uuid: &[u8], now_u: u64) -> Result<u32, (u32, String)> {
        self.refresh_from_db(now_u);
        self.projects
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(uuid)
            .map(|p| p.owner_corp_id)
            .ok_or_else(|| (STATUS_NOT_FOUND, "Freelance project not found".into()))
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

fn encode_project_attributes(proj: &FreelanceProjectRecord) -> Vec<u8> {
    let mut attr = Vec::new();
    encode_corp_id_field(1, proj.owner_corp_id, &mut attr);
    encode_char_id_field(2, proj.creator_char_id, &mut attr);
    encode_varint_field(3, proj.state as u64, &mut attr);
    encode_string_field(4, &proj.name, &mut attr);
    encode_string_field(5, &proj.description, &mut attr);
    encode_varint_field(11, proj.target_progress.max(0) as u64, &mut attr);
    encode_varint_field(12, proj.current_progress.max(0) as u64, &mut attr);
    encode_varint_field(13, proj.committed_participants.len() as u64, &mut attr);
    encode_varint_field(
        14,
        proj.committed_participants.len().max(proj.contributors.len()) as u64,
        &mut attr,
    );
    encode_varint_field(15, proj.contributors.len() as u64, &mut attr);
    attr
}

fn check_corp_admin_role(
    ctx: &AuthoritativeContext,
    db: Option<&Arc<WorldRepository>>,
    cid: u32,
    corpid: u32,
) -> Result<(bool, u64, u8), (u32, String)> {
    let sess_role = ctx.identity.as_ref().and_then(|id| id.roles.as_ref().map(|r| r.bitmask)).unwrap_or(ctx.corp_role_mask);
    let now_u = ctx_now_unix(ctx);
    let (member_ok, is_ceo, eff_roles, wallet_div) = db.map_or((true, false, sess_role, 1u8), |db| {
        db.with_snapshot(|snap| {
            let cid_i64 = cid as i64;
            let corp_i64 = corpid as i64;
            let in_corp = match snap.characters.get(&cid_i64) {
                Some(ch) if ch.corporation_id == corp_i64 => true,
                Some(ch) if (1_000_000..2_000_000).contains(&ch.corporation_id) => snap
                    .corporations
                    .get(&corp_i64)
                    .is_some_and(|c| c.ceo_id == cid_i64 || c.members.contains(&cid_i64)),
                Some(_) => false,
                None => true,
            };
            if !in_corp {
                return (false, false, 0u64, 1u8);
            }
            let (ceo, eff) = corp_projects::effective_corp_member_roles(snap, corp_i64, cid_i64, 0, now_u);
            let acct = snap.corp_governance.get(&corp_i64).and_then(|g| g.member_account_keys.get(&cid_i64).copied()).unwrap_or(1000);
            let div = if (1000..=1006).contains(&acct) { (acct - 999) as u8 } else { 1 };
            (true, ceo, eff, div)
        })
    });
    if !member_ok {
        return Err((STATUS_FORBIDDEN, "Not a member of the target corporation".into()));
    }
    if !is_ceo && (eff_roles & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_PROJECT_MANAGER)) == 0 {
        return Err((STATUS_FORBIDDEN, "Insufficient corporation role".into()));
    }
    Ok((is_ceo, eff_roles, wallet_div))
}

pub fn register(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    let state = QuasarFreelanceState::from_db(db);
    let state_rehydrate = Arc::clone(&state);
    router.register_persistence_hooks(
        Arc::new(move || state_rehydrate.refresh_from_db(0)),
        Arc::new(|| {}),
    );
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
            let (is_ceo, eff_roles, wallet_div) =
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

            let mut method_name = String::new();
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
                if let Some(m_id) = cfg.get(1) {
                    match m_id {
                        DynamicValue::String(s) => method_name = s.trim().to_string(),
                        DynamicValue::Message(sub) => {
                            if let Some(DynamicValue::String(s)) = sub.get(1) {
                                method_name = s.trim().to_string();
                            }
                        }
                        _ => {}
                    }
                }
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
            let mut max_per_char = None;
            // V24 `ContributionModifiers`: 2 = contributions_per_participant_limit (oneof with 1 = unlimited).
            if let Some(DynamicValue::Message(mods)) = dyn_msg.get(7) {
                max_per_char = mods.get(2).and_then(|v| match v {
                    DynamicValue::Uint64(u) if *u > 0 => Some(*u),
                    DynamicValue::Int64(i) if *i > 0 => Some(*i as u64),
                    _ => None,
                });
            }

            let tracked_matcher = SERVER_TRACKED_MATCHERS
                .iter()
                .copied()
                .find(|m| m.eq_ignore_ascii_case(&method_name));
            let (matcher_type, filter_id) = if let Some(tm) = tracked_matcher {
                batch_size = batch_size.max(1);
                (tm.to_string(), (required_type_id > 0).then_some(required_type_id as i64))
            } else {
                batch_size = batch_size.max(1);
                if required_type_id == 0 || !sde_create.types.contains_key(&required_type_id) {
                    return Err((STATUS_BAD_REQUEST, "Missing or invalid item delivery item type".into()));
                }
                ("DeliverItem".to_string(), Some(required_type_id as i64))
            };

            let unit_rate_opt = match dyn_msg.get(10) {
                Some(DynamicValue::Message(rew)) => {
                    extract_currency_isk(rew.get(1)).or_else(|| extract_currency_isk(Some(DynamicValue::Message(rew.clone())).as_ref()))
                }
                other => extract_currency_isk(other),
            };
            let deposit_opt = match dyn_msg.get(11) {
                Some(DynamicValue::Message(ec)) => {
                    extract_currency_isk(ec.get(3)).or_else(|| extract_currency_isk(ec.get(4)))
                }
                _ => None,
            };
            let (budget_isk, unit_rate_isk): (i64, i64) = match (unit_rate_opt, deposit_opt) {
                (Some(unit), Some(dep)) if dep > 0 => (dep, unit),
                (Some(unit), _) => (unit.saturating_mul(target_prog), unit),
                (None, Some(dep)) => (dep, dep / target_prog.max(1)),
                (None, None) => (0, 0),
            };
            if budget_isk < 0 || unit_rate_isk < 0 {
                return Err((STATUS_BAD_REQUEST, "Invalid project budget".into()));
            }
            if budget_isk > 0 && !is_ceo && (eff_roles & eve_db::corp_roles::ROLE_DIRECTOR) == 0 {
                let take_bit = eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[(wallet_div.clamp(1, 7) - 1) as usize];
                if (eff_roles & take_bit) == 0 {
                    return Err((STATUS_FORBIDDEN, "CorpRoleDenied: Project Manager lacks wallet division take role for escrow".into()));
                }
            }

            let now_u = ctx_now_unix(ctx);
            let deadline = dyn_msg.get(5).or_else(|| dyn_msg.get(4)).and_then(|v| match v {
                DynamicValue::Message(m) => m.get(1).and_then(|s| match s {
                    DynamicValue::Uint64(u) if *u > 0 => Some(*u),
                    DynamicValue::Int64(i) | DynamicValue::Sint64(i) if *i > 0 => Some(*i as u64),
                    _ => None,
                }),
                _ => None,
            }).unwrap_or_else(|| now_u.saturating_add(604_800));

            let Some(ref db) = state_create.db else {
                return Err((STATUS_BAD_REQUEST, "Database not available".into()));
            };
            let min_id = state_create.next_seq.fetch_add(1, Ordering::SeqCst) as i64;
            let (_pid, uuid_arr) = corp_projects::create_canonical_project(
                db,
                CanonicalProjectSpec {
                    corp_id: corpid as i64,
                    creator_id: cid as i64,
                    name,
                    description: desc,
                    target_quantity: target_prog as u64,
                    initial_quantity: 0,
                    escrow: eve_db::IskCents::from_isk(budget_isk),
                    isk_per_unit: eve_db::IskCents::from_isk(unit_rate_isk),
                    wallet_division: wallet_div,
                    matcher_type,
                    is_freelance: true,
                    auto_payout_on_contribution: false,
                    max_per_character: max_per_char,
                    expires_unix: Some(deadline),
                    filter_id,
                    batch_size: batch_size.max(1) as u64,
                    now_unix: now_u,
                    min_project_id: min_id,
                },
            )?;
            let uuid_vec = uuid_arr.to_vec();
            state_create.refresh_from_db(now_u);

            let mut notice_bytes = Vec::new();
            encode_uuid_field(1, &uuid_vec, &mut notice_bytes);
            if let Some(created_proj) = state_create
                .projects
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&uuid_vec)
            {
                let attr = encode_project_attributes(created_proj);
                encode_bytes_field(2, &attr, &mut notice_bytes);
            }
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.CreatedNotice"),
                    value: notice_bytes,
                },
                now_u,
            );

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
            state_get.refresh_from_db(ctx_now_unix(ctx));
            let projects = state_get.projects.read().unwrap_or_else(|e| e.into_inner());
            let Some(proj) = projects.get(&uuid).cloned() else {
                return Err((STATUS_NOT_FOUND, "Freelance project not found".to_string()));
            };
            drop(projects);

            let cid = ctx.effective_character_id().unwrap_or(0);
            let (char_prog, unredeemed) = proj.contributors.get(&cid).copied().unwrap_or((0, 0));
            // V24 `ParticipationState`: 1 = COMMITTED, 0 = UNSPECIFIED (not participating).
            let participant_state = u64::from(proj.committed_participants.contains(&cid));
            let attr = encode_project_attributes(&proj);

            let mut out = Vec::new();
            encode_bytes_field(1, &attr, &mut out);
            encode_varint_field(2, participant_state, &mut out);
            encode_varint_field(3, char_prog as u64, &mut out);
            encode_varint_field(4, unredeemed as u64, &mut out);

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
        Arc::new(move |ctx, _any, dyn_msg| {
            let uuid = extract_uuid(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".to_string()))?;
            state_mgr.refresh_from_db(ctx_now_unix(ctx));
            let projects = state_mgr.projects.read().unwrap_or_else(|e| e.into_inner());
            let Some(proj) = projects.get(&uuid).cloned() else {
                return Err((STATUS_NOT_FOUND, "Freelance project not found".to_string()));
            };
            drop(projects);

            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_UNAUTHENTICATED, "Unauthenticated".to_string()));
            };
            if crate::quasar_gateway::require_admin_role(ctx).is_err() {
                check_corp_admin_role(ctx, state_mgr.db.as_ref(), cid, proj.owner_corp_id)?;
            }
            let (char_prog, unredeemed) = proj.contributors.get(&cid).copied().unwrap_or((0, 0));
            let attr = encode_project_attributes(&proj);
            let mut admin = Vec::new();
            encode_varint_field(1, 1, &mut admin); // AdminDetails.no_acl
            let mut out = Vec::new();
            encode_bytes_field(1, &attr, &mut out);
            encode_bytes_field(2, &admin, &mut out);
            encode_varint_field(3, u64::from(proj.committed_participants.contains(&cid)), &mut out);
            encode_varint_field(4, char_prog.max(0) as u64, &mut out);
            encode_varint_field(5, unredeemed.max(0) as u64, &mut out);

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
            state_all_corp.refresh_from_db(ctx_now_unix(ctx));
            let projects = state_all_corp.projects.read().unwrap_or_else(|e| e.into_inner());

            let mut uuids: Vec<Vec<u8>> = projects
                .values()
                .filter(|p| p.owner_corp_id == corpid && p.state == 1)
                .map(|p| p.uuid.clone())
                .collect();
            uuids.sort();
            let mut out = Vec::new();
            for u in &uuids {
                encode_uuid_field(1, u, &mut out);
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
        Arc::new(move |ctx, _any, _dyn_msg| {
            state_all_act.refresh_from_db(ctx_now_unix(ctx));
            let projects = state_all_act.projects.read().unwrap_or_else(|e| e.into_inner());
            let mut uuids: Vec<Vec<u8>> = projects
                .values()
                .filter(|p| p.state == 1)
                .map(|p| p.uuid.clone())
                .collect();
            uuids.sort();
            let mut out = Vec::new();
            for u in &uuids {
                encode_uuid_field(1, u, &mut out);
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
        Arc::new(move |ctx, _any, dyn_msg| {
            state_all_inact.refresh_from_db(ctx_now_unix(ctx));
            let projects = state_all_inact.projects.read().unwrap_or_else(|e| e.into_inner());
            let mut uuids: Vec<Vec<u8>> = projects
                .values()
                .filter(|p| p.state != 1)
                .map(|p| p.uuid.clone())
                .collect();
            uuids.sort();
            let (slice, next_tok) = paginate_slice(dyn_msg.get(3), &uuids);
            let mut out = Vec::new();
            for u in &slice {
                encode_uuid_field(1, u, &mut out);
            }
            encode_next_page(2, next_tok.as_deref(), &mut out);
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
        Arc::new(move |ctx, _any, dyn_msg| {
            state_bcast.refresh_from_db(ctx_now_unix(ctx));
            let projects = state_bcast.projects.read().unwrap_or_else(|e| e.into_inner());
            let Some(sys_id) = (match dyn_msg.get(1) {
                Some(DynamicValue::Repeated(list)) => list.first().and_then(|v| extract_id(Some(v))),
                other => extract_id(other),
            })
            .filter(|&v| v > 0)
            .or_else(|| {
                let cid = ctx.effective_character_id()? as i64;
                state_bcast.db.as_ref()?.with_snapshot(|snap| {
                    snap.characters.get(&cid).map(|c| c.solar_system_id as u64)
                })
            }) else {
                return Err((STATUS_BAD_REQUEST, "Solar system ID or active character required".into()));
            };
            let mut sys_b = Vec::new();
            encode_varint_field(1, sys_id, &mut sys_b);
            let mut bcast = Vec::new();
            encode_bytes_field(1, &sys_b, &mut bcast);
            let mut uuids: Vec<Vec<u8>> = projects
                .values()
                .filter(|p| p.state == 1)
                .map(|p| p.uuid.clone())
                .collect();
            uuids.sort();
            for u in &uuids {
                encode_uuid_field(2, u, &mut bcast);
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
            state_comm.refresh_from_db(ctx_now_unix(ctx));
            let projects = state_comm.projects.read().unwrap_or_else(|e| e.into_inner());

            let mut uuids: Vec<Vec<u8>> = projects
                .values()
                .filter(|p| p.committed_participants.contains(&cid))
                .map(|p| p.uuid.clone())
                .collect();
            uuids.sort();
            let mut out = Vec::new();
            for u in &uuids {
                encode_uuid_field(1, u, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.GetAllCommittedResponse"),
                value: out,
            })
        }),
    );

    // 8b. GetAllUnredeemedRequest -> GetAllUnredeemedResponse
    let state_unred = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.GetAllUnredeemedRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let cid = ctx
                .effective_character_id()
                .ok_or_else(|| (STATUS_FORBIDDEN, "Character required".to_string()))?;
            state_unred.refresh_from_db(ctx_now_unix(ctx));
            let projects = state_unred.projects.read().unwrap_or_else(|e| e.into_inner());

            let mut uuids: Vec<Vec<u8>> = projects
                .values()
                .filter(|p| p.contributors.get(&cid).is_some_and(|(_, unredeemed)| *unredeemed > 0))
                .map(|p| p.uuid.clone())
                .collect();
            uuids.sort();
            let (slice, next_tok) = paginate_slice(dyn_msg.get(1), &uuids);
            let mut out = Vec::new();
            for u in &slice {
                encode_uuid_field(1, u, &mut out);
            }
            encode_next_page(2, next_tok.as_deref(), &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.GetAllUnredeemedResponse"),
                value: out,
            })
        }),
    );

    // 9. GetParticipationDetailsRequest -> GetParticipationDetailsResponse
    let state_entry = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.GetParticipationDetailsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uuid = extract_uuid(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".to_string()))?;
            state_entry.refresh_from_db(ctx_now_unix(ctx));
            let projects = state_entry.projects.read().unwrap_or_else(|e| e.into_inner());
            let p = projects
                .get(&uuid)
                .cloned()
                .ok_or_else(|| (STATUS_NOT_FOUND, "Freelance project not found".to_string()))?;
            drop(projects);

            let Some(caller_cid) = ctx.effective_character_id() else {
                return Err((STATUS_UNAUTHENTICATED, "Unauthenticated".to_string()));
            };
            if crate::quasar_gateway::require_admin_role(ctx).is_err() {
                check_corp_admin_role(ctx, state_entry.db.as_ref(), caller_cid, p.owner_corp_id)?;
            }

            let (slice, next_tok) = paginate_slice(dyn_msg.get(2), &p.committed_participants);
            let mut out = Vec::new();
            for cid in &slice {
                let mut part = Vec::new();
                encode_char_id_field(1, *cid, &mut part);
                encode_varint_field(2, 1, &mut part);
                if let Some(&(prog, _)) = p.contributors.get(cid) {
                    encode_varint_field(3, prog.max(0) as u64, &mut part);
                }
                encode_bytes_field(1, &part, &mut out);
            }
            encode_next_page(2, next_tok.as_deref(), &mut out);
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
        Arc::new(move |ctx, _any, dyn_msg| {
            state_stats.refresh_from_db(ctx_now_unix(ctx));
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
            encode_varint_field(1, 100, &mut out);
            encode_varint_field(2, 50, &mut out);
            encode_varint_field(3, 30, &mut out);
            encode_varint_field(4, 10, &mut out);
            encode_varint_field(5, 500, &mut out);
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
            encode_varint_field(1, 10_000_000, &mut prices);
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
            encode_varint_field(1, 10, &mut out);
            encode_varint_field(2, 500, &mut out);
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
            let now_u = ctx_now_unix(ctx);
            let corpid = state_commit.owner_corp_of(&uuid, now_u)?;
            let Some(pid) = corp_projects::freelance_uuid_to_project_id(&uuid) else {
                return Err((STATUS_NOT_FOUND, "Freelance project not found".into()));
            };
            let mut active = false;
            if let Some(ref db) = state_commit.db {
                active = db.with_mut_snapshot(|snap| {
                    let Some(det) = snap.alliance_war_state.project_details.get_mut(&pid) else {
                        return false;
                    };
                    if det.status != "Active" {
                        return false;
                    }
                    if !det.committed_participants.contains(&(cid as i64)) {
                        det.committed_participants.push(cid as i64);
                    }
                    true
                });
            }
            if !active {
                // V24 `CommitResponse.invalid_project_state = 3`.
                let mut out = Vec::new();
                encode_bytes_field(3, &[], &mut out);
                return Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.CommitResponse"),
                    value: out,
                });
            }
            state_commit.refresh_from_db(now_u);

            let (comm_cnt, tot_cnt) = state_commit
                .projects
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&uuid)
                .map(|p| {
                    let c = p.committed_participants.len();
                    (c as u64, c.max(p.contributors.len()) as u64)
                })
                .unwrap_or((1, 1));
            let mut notice_bytes = Vec::new();
            encode_uuid_field(1, &uuid, &mut notice_bytes);
            encode_varint_field(2, comm_cnt, &mut notice_bytes);
            encode_varint_field(3, tot_cnt, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.ParticipantAddedNotice"),
                    value: notice_bytes,
                },
                now_u,
            );

            let mut out = Vec::new();
            encode_bytes_field(1, &[], &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.CommitResponse"),
                value: out,
            })
        }),
    );

    // 15. ResignRequest -> ResignResponse
    let state_abandon = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.ResignRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uuid = extract_uuid(dyn_msg.get(1)).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".into()))?;
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Character required".into()));
            };
            let now_u = ctx_now_unix(ctx);
            let corpid = state_abandon.owner_corp_of(&uuid, now_u)?;
            if let (Some(pid), Some(ref db)) = (corp_projects::freelance_uuid_to_project_id(&uuid), &state_abandon.db) {
                db.with_mut_snapshot(|snap| {
                    if let Some(det) = snap.alliance_war_state.project_details.get_mut(&pid) {
                        det.committed_participants.retain(|&c| c != cid as i64);
                    }
                });
            }
            state_abandon.refresh_from_db(now_u);

            let comm_cnt = state_abandon
                .projects
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&uuid)
                .map(|p| p.committed_participants.len() as u64)
                .unwrap_or(0);
            let mut notice_bytes = Vec::new();
            encode_uuid_field(1, &uuid, &mut notice_bytes);
            encode_varint_field(2, comm_cnt, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.ParticipantRemovedNotice"),
                    value: notice_bytes,
                },
                now_u,
            );

            let mut out = Vec::new();
            encode_bytes_field(1, &[], &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.ResignResponse"),
                value: out,
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
            let uuid = extract_uuid(dyn_msg.get(1)).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".into()))?;
            let target_cid = extract_id(dyn_msg.get(2)).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing target character ID".into()))? as u32;
            let now_u = ctx_now_unix(ctx);
            let owner_corp = state_kick.owner_corp_of(&uuid, now_u)?;
            if caller_corp != Some(owner_corp) {
                return Err((STATUS_PERMISSION_DENIED, "Permission denied".into()));
            }
            check_corp_admin_role(ctx, state_kick.db.as_ref(), cid, owner_corp)?;
            if let (Some(pid), Some(ref db)) = (corp_projects::freelance_uuid_to_project_id(&uuid), &state_kick.db) {
                db.with_mut_snapshot(|snap| {
                    if let Some(det) = snap.alliance_war_state.project_details.get_mut(&pid) {
                        det.committed_participants.retain(|&c| c != target_cid as i64);
                    }
                });
            }
            state_kick.refresh_from_db(now_u);

            let mut notice_bytes = Vec::new();
            encode_uuid_field(1, &uuid, &mut notice_bytes);
            encode_char_id_field(2, target_cid, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Character(target_cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.ParticipantKickedNotice"),
                    value: notice_bytes,
                },
                now_u,
            );

            let mut out = Vec::new();
            encode_bytes_field(1, &[], &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.KickResponse"),
                value: out,
            })
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
            let uuid = extract_uuid(dyn_msg.get(1)).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".into()))?;
            let now_u = ctx_now_unix(ctx);
            let corpid = state_close_cmd.owner_corp_of(&uuid, now_u)?;
            if caller_corp != Some(corpid) {
                return Err((STATUS_PERMISSION_DENIED, "Permission denied".into()));
            }
            let (is_ceo, eff_roles, wallet_div) =
                check_corp_admin_role(ctx, state_close_cmd.db.as_ref(), cid, corpid)?;
            if let (Some(pid), Some(ref db)) = (corp_projects::freelance_uuid_to_project_id(&uuid), &state_close_cmd.db) {
                let (has_escrow, proj_div) = db.with_snapshot(|snap| {
                    let rem = snap.corp_projects.get(&pid).map_or(0, |p| p.escrow_remaining.raw_cents());
                    let div = snap.alliance_war_state.project_details.get(&pid).map_or(wallet_div, |d| d.wallet_division.clamp(1, 7));
                    (rem > 0, div)
                });
                if has_escrow && !is_ceo && (eff_roles & eve_db::corp_roles::ROLE_DIRECTOR) == 0 {
                    let take_bit = eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[(proj_div.clamp(1, 7) - 1) as usize];
                    if (eff_roles & take_bit) == 0 {
                        return Err((STATUS_FORBIDDEN, "CorpRoleDenied: Project Manager lacks wallet division take role for escrow refund".into()));
                    }
                }
                let _ = corp_projects::close_or_delete_canonical_project(db, pid, false, now_u);
            }
            state_close_cmd.refresh_from_db(now_u);

            let mut notice_bytes = Vec::new();
            encode_uuid_field(1, &uuid, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.ClosedNotice"),
                    value: notice_bytes,
                },
                now_u,
            );
            let mut out = Vec::new();
            encode_bytes_field(1, &[], &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.CloseResponse"),
                value: out,
            })
        }),
    );

    // DeleteRequest -> DeleteResponse (+ DeletedNotice)
    let state_del_cmd = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.DeleteRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_UNAUTHENTICATED, "Unauthenticated".into()));
            };
            let caller_corp = ctx.effective_corporation_id();
            let uuid = extract_uuid(dyn_msg.get(1)).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".into()))?;
            let now_u = ctx_now_unix(ctx);
            let corpid = state_del_cmd.owner_corp_of(&uuid, now_u)?;
            if caller_corp != Some(corpid) {
                return Err((STATUS_PERMISSION_DENIED, "Permission denied".into()));
            }
            check_corp_admin_role(ctx, state_del_cmd.db.as_ref(), cid, corpid)?;
            if let (Some(pid), Some(ref db)) = (corp_projects::freelance_uuid_to_project_id(&uuid), &state_del_cmd.db) {
                let _ = corp_projects::close_or_delete_canonical_project(db, pid, true, now_u);
            }
            state_del_cmd.refresh_from_db(now_u);

            let mut notice_bytes = Vec::new();
            encode_uuid_field(1, &uuid, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.DeletedNotice"),
                    value: notice_bytes,
                },
                now_u,
            );
            let mut out = Vec::new();
            encode_bytes_field(1, &[], &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.DeleteResponse"),
                value: out,
            })
        }),
    );

    // Admin/Manager SetNameRequest & SetDescriptionRequest
    let state_set_name = Arc::clone(&state);
    router.register_handler(
        "eve.freelance.project.api.admin.SetNameRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_UNAUTHENTICATED, "Unauthenticated".into()));
            };
            let uuid = extract_uuid(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".into()))?;
            let Some(DynamicValue::String(raw_name)) = dyn_msg.get(2) else {
                return Err((STATUS_BAD_REQUEST, "Project name required".into()));
            };
            let new_name = raw_name.trim().to_string();
            if new_name.is_empty() {
                return Err((STATUS_BAD_REQUEST, "Project name required".into()));
            }
            let now_u = ctx_now_unix(ctx);
            let corpid = state_set_name.owner_corp_of(&uuid, now_u)?;
            if ctx.effective_corporation_id() != Some(corpid) {
                return Err((STATUS_PERMISSION_DENIED, "Permission denied".into()));
            }
            check_corp_admin_role(ctx, state_set_name.db.as_ref(), cid, corpid)?;
            if let (Some(pid), Some(ref db)) = (
                corp_projects::freelance_uuid_to_project_id(&uuid),
                &state_set_name.db,
            ) {
                db.with_mut_snapshot(|snap| {
                    if let Some(p) = snap.corp_projects.get_mut(&pid) {
                        p.name = new_name;
                    }
                });
            }
            state_set_name.refresh_from_db(now_u);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.freelance.project.api.admin.SetNameResponse"),
                value: Vec::new(),
            })
        }),
    );

    let state_set_desc = Arc::clone(&state);
    router.register_handler(
        "eve.freelance.project.api.admin.SetDescriptionRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_UNAUTHENTICATED, "Unauthenticated".into()));
            };
            let uuid = extract_uuid(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing project UUID".into()))?;
            let new_desc = match dyn_msg.get(2) {
                Some(DynamicValue::String(s)) => s.clone(),
                _ => return Err((STATUS_BAD_REQUEST, "Project description required".into())),
            };
            let now_u = ctx_now_unix(ctx);
            let corpid = state_set_desc.owner_corp_of(&uuid, now_u)?;
            if ctx.effective_corporation_id() != Some(corpid) {
                return Err((STATUS_PERMISSION_DENIED, "Permission denied".into()));
            }
            check_corp_admin_role(ctx, state_set_desc.db.as_ref(), cid, corpid)?;
            if let (Some(pid), Some(ref db)) = (
                corp_projects::freelance_uuid_to_project_id(&uuid),
                &state_set_desc.db,
            ) {
                db.with_mut_snapshot(|snap| {
                    if let Some(det) = snap.alliance_war_state.project_details.get_mut(&pid) {
                        det.description = new_desc;
                    }
                });
            }
            state_set_desc.refresh_from_db(now_u);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.freelance.project.api.admin.SetDescriptionResponse"),
                value: Vec::new(),
            })
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
            let now_u = ctx_now_unix(ctx);
            state_redeem.refresh_from_db(now_u);
            let Some(pid) = corp_projects::freelance_uuid_to_project_id(&uuid) else {
                return Err((STATUS_NOT_FOUND, "Freelance project not found".into()));
            };
            let Some(ref db) = state_redeem.db else {
                return Err((STATUS_BAD_REQUEST, "Database not available".into()));
            };
            let _ = corp_projects::redeem_freelance_reward_for_char(db, pid, cid as i64, now_u)?;
            state_redeem.refresh_from_db(now_u);

            let mut notice_bytes = Vec::new();
            encode_uuid_field(1, &uuid, &mut notice_bytes);
            encode_char_id_field(2, cid, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Character(cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.ParticipantRewardRedeemedNotice"),
                    value: notice_bytes,
                },
                now_u,
            );

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.RedeemRewardResponse"),
                value: Vec::new(),
            })
        }),
    );

    register_item_delivery_and_definitions(router, state);
}

fn extract_uuid(val: Option<&DynamicValue>) -> Option<Vec<u8>> {
    match val {
        Some(DynamicValue::Message(sub)) => extract_uuid(sub.get(1)),
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

#[cfg(test)]
#[path = "quasar_social_freelance_tests.rs"]
mod quasar_social_freelance_tests;

#[path = "quasar_social_freelance_cov_unit_tests.rs"]
#[cfg(test)]
mod quasar_social_freelance_cov_unit_tests;
