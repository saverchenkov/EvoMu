//! Unit tests for `quasar_social_freelance` (Freelance Projects, Participation, Admin Rename, Rewards).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use eve_db::corp_roles;
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue, QuasarDescriptorRegistry};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn uuid_msg(field: u32, uuid: &[u8]) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

#[test]
fn freelance_state_and_helpers() {
    assert_eq!(legacy_serde_type_id(), 34);
    assert_eq!(legacy_serde_batch_size(), 5);

    let db = Arc::new(WorldRepository::with_demo_seed());
    let state = QuasarFreelanceState::from_db(Arc::clone(&db));
    state.sync_to_db();
    assert_eq!(state.owner_corp_of(&[0u8; 16], 1_000).unwrap_err().0, STATUS_NOT_FOUND);

    let mut out = Vec::new();
    encode_uuid_field(1, &[1u8; 16], &mut out);
    encode_char_id_field(2, 90_000_001, &mut out);
    encode_corp_id_field(3, 1_000_009, &mut out);
    assert!(!out.is_empty());

    let msg = uuid_msg(1, &[9u8; 16]);
    assert_eq!(extract_uuid(msg.get(1)), Some(vec![9u8; 16]));
    assert_eq!(extract_id(Some(&DynamicValue::Uint64(55))), Some(55));
}

#[test]
fn freelance_project_lifecycle_and_rpc_handlers() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register(&router, Arc::clone(&db));

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();

    let dir_cid = 90_000_001_u32;
    let worker_cid = 90_000_002_u32;
    let corp_id = 1_000_009_u32;
    db.with_mut_snapshot(|snap| {
        for id in [dir_cid as i64, worker_cid as i64] {
            if let Some(ch) = snap.characters.get_mut(&id) {
                ch.corporation_id = corp_id as i64;
            }
        }
        let c = snap
            .corporations
            .entry(corp_id as i64)
            .or_insert_with(|| { let mut __corp = eve_db::CorporationRecord::default();
                __corp.corporation_id = corp_id as i64;
                __corp.ceo_id = dir_cid as i64;
                __corp });
        c.ceo_id = dir_cid as i64;
        c.members.insert(dir_cid as i64);
        c.members.insert(worker_cid as i64);
    });

    let dir_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(dir_cid),
        Some(corp_id),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let worker_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(worker_cid),
        Some(corp_id),
        0,
        "tranquility",
        vec![1],
    );

    // Non-director cannot create freelance project
    let mut cfg = DynamicQuasarMessage::new("Cfg");
    cfg.fields.insert(1, DynamicValue::String("DestroyNPCShips".into()));
    let mut create_msg = DynamicQuasarMessage::new("Create");
    create_msg.fields.insert(1, DynamicValue::String("Clear Pirate Rats".into()));
    create_msg.fields.insert(2, DynamicValue::String("Bounty hunting in system".into()));
    create_msg.fields.insert(3, DynamicValue::Uint64(25));
    create_msg.fields.insert(6, DynamicValue::Message(Box::new(cfg)));

    assert_eq!(
        handlers["eve_public.freelance.project.api.CreateRequest"](&worker_ctx, &dummy_any, &create_msg)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    let created = handlers["eve_public.freelance.project.api.CreateRequest"](&dir_ctx, &dummy_any, &create_msg)
        .expect("director creates freelance project");
    assert!(created
        .type_url
        .ends_with("eve_public.freelance.project.api.CreateResponse"));

    // Extract created project uuid from canonical db
    let (_, proj_uuid) = db.with_snapshot(|snap| {
        let (&pid, _) = snap
            .alliance_war_state
            .project_meta
            .iter()
            .find(|(_, m)| m.1)
            .expect("freelance project exists");
        let u = snap.alliance_war_state.project_details[&pid].uuid_bytes.clone();
        (pid, u)
    });

    // Static limits & prices
    for (req, suffix) in [
        (
            "eve_public.freelance.project.api.GetCreationLimitsRequest",
            "eve_public.freelance.project.api.GetCreationLimitsResponse",
        ),
        (
            "eve_public.freelance.project.api.GetCreationPricesRequest",
            "eve_public.freelance.project.api.GetCreationPricesResponse",
        ),
        (
            "eve_public.freelance.project.api.GetCommitLimitsRequest",
            "eve_public.freelance.project.api.GetCommitLimitsResponse",
        ),
    ] {
        let r = handlers[req](&worker_ctx, &dummy_any, &DynamicQuasarMessage::new("Req")).expect(req);
        assert!(r.type_url.ends_with(suffix));
    }

    // Worker commits to project
    let commit_resp = handlers["eve_public.freelance.project.api.CommitRequest"](
        &worker_ctx,
        &dummy_any,
        &uuid_msg(1, &proj_uuid),
    )
    .expect("worker commits");
    assert!(commit_resp
        .type_url
        .ends_with("eve_public.freelance.project.api.CommitResponse"));

    // Read endpoints (Get, GetForManager, GetAll*, GetParticipationDetails, GetStats)
    for (req, suffix) in [
        ("eve_public.freelance.project.api.GetRequest", "eve_public.freelance.project.api.GetResponse"),
        (
            "eve_public.freelance.project.api.GetForManagerRequest",
            "eve_public.freelance.project.api.GetForManagerResponse",
        ),
        (
            "eve_public.freelance.project.api.GetAllActiveForCorporationRequest",
            "eve_public.freelance.project.api.GetAllActiveForCorporationResponse",
        ),
        (
            "eve_public.freelance.project.api.GetAllActiveRequest",
            "eve_public.freelance.project.api.GetAllActiveResponse",
        ),
        (
            "eve_public.freelance.project.api.GetAllInactiveRequest",
            "eve_public.freelance.project.api.GetAllInactiveResponse",
        ),
        (
            "eve_public.freelance.project.api.GetAllBroadcastedRequest",
            "eve_public.freelance.project.api.GetAllBroadcastedResponse",
        ),
        (
            "eve_public.freelance.project.api.GetAllCommittedRequest",
            "eve_public.freelance.project.api.GetAllCommittedResponse",
        ),
        (
            "eve_public.freelance.project.api.GetAllUnredeemedRequest",
            "eve_public.freelance.project.api.GetAllUnredeemedResponse",
        ),
        (
            "eve_public.freelance.project.api.GetParticipationDetailsRequest",
            "eve_public.freelance.project.api.GetParticipationDetailsResponse",
        ),
        (
            "eve_public.freelance.project.api.GetStatsRequest",
            "eve_public.freelance.project.api.GetStatsResponse",
        ),
    ] {
        let caller_ctx = if matches!(
            req,
            "eve_public.freelance.project.api.GetForManagerRequest"
                | "eve_public.freelance.project.api.GetParticipationDetailsRequest"
        ) {
            assert_eq!(
                handlers[req](&worker_ctx, &dummy_any, &uuid_msg(1, &proj_uuid))
                    .unwrap_err()
                    .0,
                STATUS_FORBIDDEN
            );
            &dir_ctx
        } else {
            &worker_ctx
        };
        let r = handlers[req](caller_ctx, &dummy_any, &uuid_msg(1, &proj_uuid)).expect(req);
        assert!(r.type_url.ends_with(suffix));
    }

    // Worker resigns, re-commits, then director kicks worker
    handlers["eve_public.freelance.project.api.ResignRequest"](
        &worker_ctx,
        &dummy_any,
        &uuid_msg(1, &proj_uuid),
    )
    .expect("worker resigns");
    handlers["eve_public.freelance.project.api.CommitRequest"](
        &worker_ctx,
        &dummy_any,
        &uuid_msg(1, &proj_uuid),
    )
    .expect("worker recommits");

    let mut kick_msg = uuid_msg(1, &proj_uuid);
    kick_msg.fields.insert(2, DynamicValue::Uint32(worker_cid));
    handlers["eve_public.freelance.project.api.KickRequest"](&dir_ctx, &dummy_any, &kick_msg)
        .expect("director kicks worker");

    // Admin SetName & SetDescription
    let mut set_name = uuid_msg(1, &proj_uuid);
    set_name.fields.insert(2, DynamicValue::String("Renamed Freelance Project".into()));
    handlers["eve.freelance.project.api.admin.SetNameRequest"](&dir_ctx, &dummy_any, &set_name)
        .expect("admin set name");

    let mut set_desc = uuid_msg(1, &proj_uuid);
    set_desc.fields.insert(2, DynamicValue::String("New freelance desc".into()));
    handlers["eve.freelance.project.api.admin.SetDescriptionRequest"](&dir_ctx, &dummy_any, &set_desc)
        .expect("admin set desc");

    // Close project, then CommitRequest returns invalid_project_state
    handlers["eve_public.freelance.project.api.CloseRequest"](
        &dir_ctx,
        &dummy_any,
        &uuid_msg(1, &proj_uuid),
    )
    .expect("director closes project");

    let closed_commit = handlers["eve_public.freelance.project.api.CommitRequest"](
        &worker_ctx,
        &dummy_any,
        &uuid_msg(1, &proj_uuid),
    )
    .expect("commit on closed project returns invalid_project_state");
    assert!(!closed_commit.value.is_empty());
}
