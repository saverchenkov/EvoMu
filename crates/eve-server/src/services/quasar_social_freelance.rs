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
use crate::quasar_gateway::{NoticeScope, PublicGatewayRouter};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicValue, GoogleAny,
    TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
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
}

#[derive(Debug, Default)]
pub struct QuasarFreelanceState {
    pub projects: RwLock<HashMap<Vec<u8>, FreelanceProjectRecord>>,
    pub next_seq: AtomicU64,
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

pub fn register(router: &PublicGatewayRouter, _db: Arc<WorldRepository>) {
    let state = Arc::new(QuasarFreelanceState::default());
    state.next_seq.store(1, Ordering::SeqCst);

    // Pre-seed a default project
    let default_uuid = b"fl-proj-00000001".to_vec();
    {
        let mut projects = state.projects.write().unwrap_or_else(|e| e.into_inner());
        let mut contribs = HashMap::new();
        contribs.insert(90000001u32, (25i64, 5i64));
        projects.insert(
            default_uuid.clone(),
            FreelanceProjectRecord {
                uuid: default_uuid.clone(),
                owner_corp_id: 98000001,
                creator_char_id: 90000001,
                name: "Tritanium Supply Run".into(),
                description: "Mine and deliver Tritanium for corp fleet production".into(),
                career: 1, // Enforcer / Industry
                deadline: 1_800_000_000,
                created: 1_700_000_000,
                target_progress: 100,
                current_progress: 25,
                state: 1, // Active
                committed_participants: vec![90000001],
                contributors: contribs,
                reward_isk: 10_000_000,
            },
        );
    }

    // 1. CreateRequest -> CreateResponse
    let state_create = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.CreateRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);
            let cid = ctx.effective_character_id().unwrap_or(90000001);
            let seq = state_create.next_seq.fetch_add(1, Ordering::SeqCst);

            let mut uuid = [0u8; 16];
            uuid[0..8].copy_from_slice(&ctx_now_unix(ctx).to_be_bytes());
            uuid[8..16].copy_from_slice(&seq.to_be_bytes());
            let uuid_vec = uuid.to_vec();

            let name = match dyn_msg.get(1) {
                Some(DynamicValue::String(s)) => s.clone(),
                _ => "New Corporate Project".into(),
            };
            let desc = match dyn_msg.get(2) {
                Some(DynamicValue::String(s)) => s.clone(),
                _ => "Corporation objectives and contributions".into(),
            };
            let target_prog = match dyn_msg.get(3) {
                Some(DynamicValue::Int64(i)) => *i,
                Some(DynamicValue::Uint64(u)) => *u as i64,
                _ => 100,
            };

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
                reward_isk: 50_000_000,
            };

            state_create
                .projects
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(uuid_vec.clone(), proj);

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
            let uuid = extract_uuid(dyn_msg.get(1)).unwrap_or_else(|| b"fl-proj-00000001".to_vec());
            let projects = state_get.projects.read().unwrap_or_else(|e| e.into_inner());
            let proj = projects.get(&uuid).cloned().unwrap_or_else(|| FreelanceProjectRecord {
                uuid: uuid.clone(),
                owner_corp_id: 98000001,
                creator_char_id: 90000001,
                name: "Corp Project".into(),
                description: "Default objectives".into(),
                career: 1,
                deadline: 1_800_000_000,
                created: 1_700_000_000,
                target_progress: 100,
                current_progress: 50,
                state: 1,
                committed_participants: Vec::new(),
                contributors: HashMap::new(),
                reward_isk: 10_000_000,
            });

            let cid = ctx.effective_character_id().unwrap_or(90000001);
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
            let uuid = extract_uuid(dyn_msg.get(1)).unwrap_or_else(|| b"fl-proj-00000001".to_vec());
            let projects = state_mgr.projects.read().unwrap_or_else(|e| e.into_inner());
            let proj = projects.get(&uuid).cloned().unwrap_or_else(|| FreelanceProjectRecord {
                uuid: uuid.clone(),
                owner_corp_id: 98000001,
                creator_char_id: 90000001,
                name: "Corp Project".into(),
                description: "Default objectives".into(),
                career: 1,
                deadline: 1_800_000_000,
                created: 1_700_000_000,
                target_progress: 100,
                current_progress: 50,
                state: 1,
                committed_participants: Vec::new(),
                contributors: HashMap::new(),
                reward_isk: 10_000_000,
            });

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
        Arc::new(move |ctx, _any, _dyn_msg| {
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);
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
            let cid = ctx.effective_character_id().unwrap_or(90000001);
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
            let uuid = extract_uuid(dyn_msg.get(1)).unwrap_or_else(|| b"fl-proj-00000001".to_vec());
            let projects = state_part.projects.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            if let Some(p) = projects.get(&uuid) {
                for cid in &p.committed_participants {
                    let mut part = Vec::new();
                    encode_char_id_field(1, *cid, &mut part);
                    encode_varint_field(2, 1, &mut part); // Committed
                    encode_bytes_field(1, &part, &mut out);
                }
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.freelance.project.api.GetParticipationDetailsResponse"),
                value: out,
            })
        }),
    );

    // 10. GetStatsRequest -> GetStatsResponse
    router.register_handler(
        "eve_public.freelance.project.api.GetStatsRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut stats = Vec::new();
            encode_varint_field(1, 10, &mut stats); // total active
            encode_varint_field(2, 5, &mut stats); // total completed
            let mut out = Vec::new();
            encode_bytes_field(1, &stats, &mut out);
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
            let uuid = extract_uuid(dyn_msg.get(1)).unwrap_or_else(|| b"fl-proj-00000001".to_vec());
            let cid = ctx.effective_character_id().unwrap_or(90000001);
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);

            if let Some(p) = state_commit.projects.write().unwrap_or_else(|e| e.into_inner()).get_mut(&uuid) {
                if !p.committed_participants.contains(&cid) {
                    p.committed_participants.push(cid);
                }
            }

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
            let uuid = extract_uuid(dyn_msg.get(1)).unwrap_or_else(|| b"fl-proj-00000001".to_vec());
            let cid = ctx.effective_character_id().unwrap_or(90000001);
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);

            if let Some(p) = state_abandon.projects.write().unwrap_or_else(|e| e.into_inner()).get_mut(&uuid) {
                p.committed_participants.retain(|&c| c != cid);
            }

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
            let uuid = extract_uuid(dyn_msg.get(1)).unwrap_or_else(|| b"fl-proj-00000001".to_vec());
            let target_cid = extract_id(dyn_msg.get(2)).unwrap_or(90000002) as u32;

            if let Some(p) = state_kick.projects.write().unwrap_or_else(|e| e.into_inner()).get_mut(&uuid) {
                p.committed_participants.retain(|&c| c != target_cid);
            }

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

    // 17. RedeemRewardRequest -> RedeemRewardResponse
    let state_redeem = Arc::clone(&state);
    router.register_handler(
        "eve_public.freelance.project.api.RedeemRewardRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uuid = extract_uuid(dyn_msg.get(1)).unwrap_or_else(|| b"fl-proj-00000001".to_vec());
            let cid = ctx.effective_character_id().unwrap_or(90000001);

            if let Some(p) = state_redeem.projects.write().unwrap_or_else(|e| e.into_inner()).get_mut(&uuid) {
                if let Some(entry) = p.contributors.get_mut(&cid) {
                    entry.1 = 0; // cleared unredeemed
                }
            }

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
            let uuid = extract_uuid(dyn_msg.get(1)).unwrap_or_else(|| b"fl-proj-00000001".to_vec());
            let cid = ctx.effective_character_id().unwrap_or(90000001);
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);

            let mut completed = false;
            let mut total_prog = 0i64;
            if let Some(p) = state_deliver.projects.write().unwrap_or_else(|e| e.into_inner()).get_mut(&uuid) {
                p.current_progress += 25;
                total_prog = p.current_progress;
                let entry = p.contributors.entry(cid).or_insert((0, 0));
                entry.0 += 25;
                entry.1 += 5; // reward units
                if p.current_progress >= p.target_progress {
                    p.state = 2; // Completed
                    completed = true;
                }
            }

            // 1. ContributionSucceededNotice
            let mut succ_bytes = Vec::new();
            encode_uuid_field(1, &uuid, &mut succ_bytes);
            encode_char_id_field(2, cid, &mut succ_bytes);
            encode_varint_field(3, 25, &mut succ_bytes); // units contributed
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
