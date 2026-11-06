//! Unit tests for `quasar_social_corp` (Corp Skill Plans, Milestones, Corporation Goals, Rewards).

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

fn corp_sp_msg(field: u32, id: u64) -> DynamicQuasarMessage {
    let mut uuid = [0u8; 16];
    uuid[0..8].copy_from_slice(b"CORPSPLN");
    uuid[8..16].copy_from_slice(&id.to_be_bytes());
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

fn corp_goal_msg(field: u32, id: u64) -> DynamicQuasarMessage {
    let mut uuid = [0u8; 16];
    uuid[0..8].copy_from_slice(b"CORPGOAL");
    uuid[8..16].copy_from_slice(&id.to_be_bytes());
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

#[test]
fn quasar_corp_state_and_id_extractors() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let state = QuasarCorpState::from_db(Arc::clone(&db));

    let pid1 = state.alloc_plan_id();
    let pid2 = state.alloc_plan_id();
    assert_eq!(pid2, pid1 + 1);

    let mid1 = state.alloc_milestone_id();
    let mid2 = state.alloc_milestone_id();
    assert_eq!(mid2, mid1 + 1);

    assert_eq!(state.create_goal(1_000_009, "Bad".into(), "".into(), 0, 0).unwrap_err().0, STATUS_BAD_REQUEST);
    let gid = state
        .create_goal(1_000_009, "Mine Ore".into(), "Deliver Veldspar".into(), 500, 0)
        .expect("create goal");
    assert!(gid >= 101);

    state.sync_to_db();
    let reloaded = QuasarCorpState::from_db(Arc::clone(&db));
    assert!(reloaded.alloc_plan_id() > pid2);

    state.close_or_delete_goal(gid, false, 1_000);
    state.close_or_delete_goal(gid, true, 1_000);

    let sp_m = corp_sp_msg(1, 77);
    assert_eq!(extract_id(sp_m.get(1)), Some(77));
    assert_eq!(extract_goal_id(sp_m.get(1)), None);

    let cg_m = corp_goal_msg(1, 888);
    assert_eq!(extract_goal_id(cg_m.get(1)), Some(888));
    assert_eq!(extract_id(cg_m.get(1)), None);
}

#[test]
fn corp_skill_plan_and_milestone_rpc_handlers() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register(&router, Arc::clone(&db));

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let cid = 90_000_001_u32;
    let corp_id = 1_000_009_u32;
    db.with_mut_snapshot(|snap| {
        for id in [90_000_001_i64, 90_000_002_i64] {
            if let Some(ch) = snap.characters.get_mut(&id) {
                ch.corporation_id = corp_id as i64;
            }
        }
        let c = snap
            .corporations
            .entry(corp_id as i64)
            .or_insert_with(|| eve_db::CorporationRecord {
                corporation_id: corp_id as i64,
                ceo_id: cid as i64,
                ..Default::default()
            });
        c.ceo_id = cid as i64;
        c.members.insert(90_000_001);
        c.members.insert(90_000_002);
    });
    let dir_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid),
        Some(corp_id),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let grunt_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(90_000_002),
        Some(corp_id),
        0,
        "tranquility",
        vec![1],
    );

    // Non-director cannot create corp skill plan
    let mut attr = DynamicQuasarMessage::new("Attr");
    attr.fields.insert(1, DynamicValue::String("Fleet Doctrine I".into()));
    attr.fields.insert(2, DynamicValue::String("Logi + DPS".into()));
    let mut create_msg = DynamicQuasarMessage::new("Create");
    create_msg.fields.insert(1, DynamicValue::Message(Box::new(attr)));

    assert_eq!(
        handlers["eve_public.corporation.skill.plan.CreateRequest"](&grunt_ctx, &dummy_any, &create_msg)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    let created = handlers["eve_public.corporation.skill.plan.CreateRequest"](&dir_ctx, &dummy_any, &create_msg)
        .expect("director creates corp skill plan");
    assert!(created
        .type_url
        .ends_with("eve_public.corporation.skill.plan.CreateResponse"));

    let plan_id = *router
        .progression
        .corp_state
        .skill_plans
        .read()
        .unwrap()
        .keys()
        .next()
        .unwrap();

    // GetAll, Get, GetShared
    let all_resp = handlers["eve_public.corporation.skill.plan.GetAllRequest"](
        &grunt_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("All"),
    )
    .expect("get all corp plans");
    assert!(all_resp
        .type_url
        .ends_with("eve_public.corporation.skill.plan.GetAllResponse"));
    assert!(!all_resp.value.is_empty());

    let get_resp = handlers["eve_public.corporation.skill.plan.GetRequest"](
        &grunt_ctx,
        &dummy_any,
        &corp_sp_msg(1, plan_id),
    )
    .expect("get corp plan");
    assert!(get_resp
        .type_url
        .ends_with("eve_public.corporation.skill.plan.GetResponse"));

    let mut shared_any = GoogleAny::default();
    shared_any.type_url = "type.googleapis.com/eve_public.corporation.skill.plan.GetSharedRequest".into();
    let other_corp_ctx = AuthoritativeContext::for_session(
        Some(3),
        Some(90_000_099),
        Some(1_000_010),
        0,
        "tranquility",
        vec![1],
    );
    assert_eq!(
        handlers["eve_public.corporation.skill.plan.GetRequest"](
            &other_corp_ctx,
            &dummy_any,
            &corp_sp_msg(1, plan_id),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    handlers["eve_public.corporation.skill.plan.GetSharedRequest"](
        &other_corp_ctx,
        &shared_any,
        &corp_sp_msg(1, plan_id),
    )
    .expect("shared get across corp");

    // SetName, SetDescription, SetCategory, SetSkillRequirements
    let mut set_name = corp_sp_msg(1, plan_id);
    set_name.fields.insert(2, DynamicValue::String("Fleet Doctrine II".into()));
    handlers["eve_public.corporation.skill.plan.SetNameRequest"](&dir_ctx, &dummy_any, &set_name)
        .expect("set corp plan name");

    let mut set_desc = corp_sp_msg(1, plan_id);
    set_desc.fields.insert(2, DynamicValue::String("Updated doctrine".into()));
    handlers["eve_public.corporation.skill.plan.SetDescriptionRequest"](&dir_ctx, &dummy_any, &set_desc)
        .expect("set corp plan desc");

    let mut set_cat = corp_sp_msg(1, plan_id);
    set_cat.fields.insert(2, DynamicValue::Uint32(3));
    handlers["eve_public.corporation.skill.plan.SetCategoryRequest"](&dir_ctx, &dummy_any, &set_cat)
        .expect("set corp plan category");

    let mut req_entry = DynamicQuasarMessage::new("ReqEntry");
    req_entry.fields.insert(1, DynamicValue::Uint32(3300));
    req_entry.fields.insert(2, DynamicValue::Uint32(4));
    let mut set_reqs = corp_sp_msg(1, plan_id);
    set_reqs.fields.insert(
        2,
        DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(req_entry))]),
    );
    handlers["eve_public.corporation.skill.plan.SetSkillRequirementsRequest"](&dir_ctx, &dummy_any, &set_reqs)
        .expect("set corp plan requirements");

    // Milestones: Create, GetAll, SetDescription, Delete
    let mut ms_attr = corp_sp_msg(1, plan_id);
    ms_attr.fields.insert(4, DynamicValue::String("Reach Gunnery IV".into()));
    let mut ms_create = DynamicQuasarMessage::new("MsCreate");
    ms_create.fields.insert(1, DynamicValue::Message(Box::new(ms_attr)));
    handlers["eve_public.corporation.skill.plan.milestone.CreateRequest"](&dir_ctx, &dummy_any, &ms_create)
        .expect("create corp milestone");

    let mid = router.progression.corp_state.skill_plans.read().unwrap()[&plan_id].milestones[0].0;

    let ms_all = handlers["eve_public.corporation.skill.plan.milestone.GetAllRequest"](
        &grunt_ctx,
        &dummy_any,
        &corp_sp_msg(1, plan_id),
    )
    .expect("get corp milestones");
    assert!(ms_all
        .type_url
        .ends_with("eve_public.corporation.skill.plan.milestone.GetAllResponse"));
    assert!(!ms_all.value.is_empty());

    let mut ms_desc = corp_sp_msg(1, mid);
    ms_desc.fields.insert(2, DynamicValue::String("Reach Gunnery V".into()));
    handlers["eve_public.corporation.skill.plan.milestone.SetDescriptionRequest"](&dir_ctx, &dummy_any, &ms_desc)
        .expect("set corp milestone desc");

    handlers["eve_public.corporation.skill.plan.milestone.DeleteRequest"](
        &dir_ctx,
        &dummy_any,
        &corp_sp_msg(1, mid),
    )
    .expect("delete corp milestone");

    handlers["eve_public.corporation.skill.plan.DeleteRequest"](&dir_ctx, &dummy_any, &corp_sp_msg(1, plan_id))
        .expect("delete corp plan");
}

#[test]
fn corporation_goal_and_reward_rpc_handlers() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register(&router, Arc::clone(&db));

    let cid = 90_000_001_u32;
    let corp_id = 1_000_009_u32;
    db.with_mut_snapshot(|snap| {
        for id in [90_000_001_i64, 90_000_002_i64] {
            if let Some(ch) = snap.characters.get_mut(&id) {
                ch.corporation_id = corp_id as i64;
            }
        }
        let c = snap
            .corporations
            .entry(corp_id as i64)
            .or_insert_with(|| eve_db::CorporationRecord {
                corporation_id: corp_id as i64,
                ceo_id: cid as i64,
                ..Default::default()
            });
        c.ceo_id = cid as i64;
        c.members.insert(90_000_001);
        c.members.insert(90_000_002);
    });
    let gid = router
        .progression
        .corp_state
        .create_goal(corp_id, "Corp Goal Alpha".into(), "Deliver minerals".into(), 100, 0)
        .expect("seed corp goal");

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid),
        Some(corp_id),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );

    let mut goal_msg = corp_goal_msg(1, gid);
    goal_msg.fields.insert(2, DynamicValue::Uint32(cid));

    for (req, suffix) in [
        ("eve_public.corporationgoal.api.GetAllRequest", "eve_public.corporationgoal.api.GetAllResponse"),
        ("eve_public.corporationgoal.api.GetActiveRequest", "eve_public.corporationgoal.api.GetActiveResponse"),
        ("eve_public.corporationgoal.api.GetInactiveRequest", "eve_public.corporationgoal.api.GetInactiveResponse"),
        (
            "eve_public.corporationgoal.api.GetContributorSummariesForGoalRequest",
            "eve_public.corporationgoal.api.GetContributorSummariesForGoalResponse",
        ),
        (
            "eve_public.corporationgoal.api.GetMyContributorSummaryForGoalRequest",
            "eve_public.corporationgoal.api.GetMyContributorSummaryForGoalResponse",
        ),
        (
            "eve_public.corporationgoal.api.GetMyContributorSummariesRequest",
            "eve_public.corporationgoal.api.GetMyContributorSummariesResponse",
        ),
        (
            "eve_public.corporationgoal.api.GetMineWithRewardsRequest",
            "eve_public.corporationgoal.api.GetMineWithRewardsResponse",
        ),
        (
            "eve_public.corporationgoal.api.RedeemAllMyRewardsRequest",
            "eve_public.corporationgoal.api.RedeemAllMyRewardsResponse",
        ),
        ("eve.corporationgoal.api.GetRequest", "eve.corporationgoal.api.GetResponse"),
        ("eve.corporationgoal.api.GetCapacityRequest", "eve.corporationgoal.api.GetCapacityResponse"),
        (
            "eve.corporationgoal.contribution.api.GetContributorSummaryForGoalRequest",
            "eve.corporationgoal.contribution.api.GetContributorSummaryForGoalResponse",
        ),
    ] {
        let r = handlers[req](&ctx, &dummy_any, &goal_msg).expect(req);
        assert!(r.type_url.ends_with(suffix));
    }

    let mut corp_id_msg = DynamicQuasarMessage::new("CorpId");
    corp_id_msg.fields.insert(1, DynamicValue::Uint32(corp_id));
    let int_all = handlers["eve.corporationgoal.api.GetAllRequest"](&ctx, &dummy_any, &corp_id_msg)
        .expect("internal corp goal GetAll");
    assert!(int_all.type_url.ends_with("eve.corporationgoal.api.GetAllResponse"));
}
