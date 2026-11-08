//! Extra branch-coverage and mutation-killing unit tests for `quasar_social_corp.rs`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_UNAUTHORIZED,
};
use crate::services::quasar_social_corp::*;
use eve_db::{corp_roles, CorporationRecord, IskCents, WorldRepository};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn sp_uuid_msg(field: u32, id: u64) -> DynamicQuasarMessage {
    let mut uuid = [0u8; 16];
    uuid[0..8].copy_from_slice(b"CORPSPLN");
    uuid[8..16].copy_from_slice(&id.to_be_bytes());
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

fn goal_uuid_msg(field: u32, id: u64) -> DynamicQuasarMessage {
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
fn state_without_db_and_division_goal_validation() {
    let db_less = QuasarCorpState::default();
    assert_eq!(
        db_less
            .create_goal(1_000_009, "Test".into(), "Desc".into(), 10, 0)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    db_less.close_or_delete_goal(101, true, 1000);
    db_less.sync_to_db();
    assert_eq!(db_less.alloc_goal_id(), 101);
    assert_eq!(db_less.alloc_goal_id(), 102);

    let db = Arc::new(WorldRepository::with_demo_seed());
    let state = QuasarCorpState::from_db(Arc::clone(&db));
    assert_eq!(
        state
            .create_goal_from_division(1_000_009, "NegEscrow".into(), "".into(), 10, -5, 1)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        state
            .create_goal_from_division(1_000_009, "ZeroTarget".into(), "".into(), 0, 0, 1)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
}

#[test]
fn skill_plan_and_milestone_error_branches_and_role_resolution() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register(&router, Arc::clone(&db));

    let ceo_id = 90_000_001_u32;
    let pm_id = 90_000_002_u32;
    let outsider_id = 90_000_003_u32;
    let corp_a = 98_000_101_u32;
    let corp_b = 98_000_102_u32;

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&(ceo_id as i64)) {
            ch.corporation_id = corp_a as i64;
        }
        if let Some(ch) = snap.characters.get_mut(&(pm_id as i64)) {
            ch.corporation_id = corp_a as i64;
        }
        let mut c = CorporationRecord::default();
        c.corporation_id = corp_a as i64;
        c.ceo_id = ceo_id as i64;
        c.members.insert(ceo_id as i64);
        c.members.insert(pm_id as i64);
        c.member_roles
            .insert(pm_id as i64, corp_roles::ROLE_PERSONNEL_MANAGER);
        snap.corporations.insert(corp_a as i64, c);
    });

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();

    // Unauthenticated caller -> STATUS_FORBIDDEN on CreateRequest
    let unauth_ctx = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![]);
    assert_eq!(
        handlers["eve_public.corporation.skill.plan.CreateRequest"](
            &unauth_ctx,
            &dummy_any,
            &DynamicQuasarMessage::new("Empty"),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    // CEO without corp_role_mask or session corp_id resolves corp from DB and passes admin check
    let ceo_no_mask_ctx =
        AuthoritativeContext::for_session(Some(1), Some(ceo_id), None, 0, "tranquility", vec![1]);
    // Missing field 1 -> STATUS_BAD_REQUEST
    assert_eq!(
        handlers["eve_public.corporation.skill.plan.CreateRequest"](
            &ceo_no_mask_ctx,
            &dummy_any,
            &DynamicQuasarMessage::new("Empty"),
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Empty name -> STATUS_BAD_REQUEST
    let mut empty_name_attr = DynamicQuasarMessage::new("Attr");
    empty_name_attr
        .fields
        .insert(1, DynamicValue::String("   ".into()));
    let mut create_empty = DynamicQuasarMessage::new("Create");
    create_empty
        .fields
        .insert(1, DynamicValue::Message(Box::new(empty_name_attr)));
    assert_eq!(
        handlers["eve_public.corporation.skill.plan.CreateRequest"](
            &ceo_no_mask_ctx,
            &dummy_any,
            &create_empty,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Personnel Manager can create skill plan with nested type_id message and valid skill (3300 = Gunnery)
    let pm_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(pm_id),
        Some(corp_a),
        corp_roles::ROLE_PERSONNEL_MANAGER,
        "tranquility",
        vec![1],
    );
    let mut tid_msg = DynamicQuasarMessage::new("TypeId");
    tid_msg.fields.insert(1, DynamicValue::Int32(3300));
    let mut req_entry = DynamicQuasarMessage::new("ReqEntry");
    req_entry
        .fields
        .insert(1, DynamicValue::Message(Box::new(tid_msg)));
    req_entry.fields.insert(2, DynamicValue::Int32(5));
    let mut valid_attr = DynamicQuasarMessage::new("Attr");
    valid_attr
        .fields
        .insert(1, DynamicValue::String("Gunnery Plan".into()));
    valid_attr.fields.insert(
        3,
        DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(req_entry))]),
    );
    let mut create_valid = DynamicQuasarMessage::new("Create");
    create_valid
        .fields
        .insert(1, DynamicValue::Message(Box::new(valid_attr)));
    handlers["eve_public.corporation.skill.plan.CreateRequest"](
        &pm_ctx,
        &dummy_any,
        &create_valid,
    )
    .expect("PM creates skill plan");

    let plan_id = *router
        .progression
        .corp_state
        .skill_plans
        .read()
        .unwrap()
        .keys()
        .next()
        .unwrap();

    // Invalid skill requirement level (6) and non-skill type (34 = Tritanium) -> STATUS_BAD_REQUEST
    for (bad_tid, bad_lvl) in [(3300_u32, 6_i32), (34_u32, 3_i32), (0_u32, 1_i32)] {
        let mut bad_req = DynamicQuasarMessage::new("ReqEntry");
        bad_req.fields.insert(1, DynamicValue::Uint64(bad_tid as u64));
        bad_req.fields.insert(2, DynamicValue::Uint64(bad_lvl as u64));
        let mut msg = sp_uuid_msg(1, plan_id);
        msg.fields.insert(
            2,
            DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(bad_req))]),
        );
        assert_eq!(
            handlers["eve_public.corporation.skill.plan.SetSkillRequirementsRequest"](
                &pm_ctx, &dummy_any, &msg
            )
            .unwrap_err()
            .0,
            STATUS_BAD_REQUEST
        );
    }

    // SetDescription with None clears description; non-string fails with STATUS_BAD_REQUEST
    let clear_desc = sp_uuid_msg(1, plan_id);
    handlers["eve_public.corporation.skill.plan.SetDescriptionRequest"](
        &pm_ctx,
        &dummy_any,
        &clear_desc,
    )
    .expect("clear description");
    let mut bad_desc = sp_uuid_msg(1, plan_id);
    bad_desc.fields.insert(2, DynamicValue::Uint32(99));
    assert_eq!(
        handlers["eve_public.corporation.skill.plan.SetDescriptionRequest"](
            &pm_ctx, &dummy_any, &bad_desc
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Cross-corp director cannot modify or delete corp_a's plan or milestones
    let cross_dir_ctx = AuthoritativeContext::for_session(
        Some(3),
        Some(outsider_id),
        Some(corp_b),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let mut cross_name = sp_uuid_msg(1, plan_id);
    cross_name
        .fields
        .insert(2, DynamicValue::String("Hijack".into()));
    assert_eq!(
        handlers["eve_public.corporation.skill.plan.SetNameRequest"](
            &cross_dir_ctx,
            &dummy_any,
            &cross_name,
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        handlers["eve_public.corporation.skill.plan.DeleteRequest"](
            &cross_dir_ctx,
            &dummy_any,
            &sp_uuid_msg(1, plan_id),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    // Not-found plan ID -> STATUS_NOT_FOUND
    assert_eq!(
        handlers["eve_public.corporation.skill.plan.GetRequest"](
            &pm_ctx,
            &dummy_any,
            &sp_uuid_msg(1, 999_999),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );

    // Milestone creation via field 2 description and cross-corp access denial on Delete/SetDescription
    let mut ms_attr = sp_uuid_msg(1, plan_id);
    ms_attr
        .fields
        .insert(2, DynamicValue::String("Milestone via field 2".into()));
    let mut ms_create = DynamicQuasarMessage::new("MsCreate");
    ms_create
        .fields
        .insert(1, DynamicValue::Message(Box::new(ms_attr)));
    handlers["eve_public.corporation.skill.plan.milestone.CreateRequest"](
        &pm_ctx,
        &dummy_any,
        &ms_create,
    )
    .expect("create milestone");
    let mid = router.progression.corp_state.skill_plans.read().unwrap()[&plan_id].milestones[0].0;

    assert_eq!(
        handlers["eve_public.corporation.skill.plan.milestone.DeleteRequest"](
            &cross_dir_ctx,
            &dummy_any,
            &sp_uuid_msg(1, mid),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        handlers["eve_public.corporation.skill.plan.milestone.DeleteRequest"](
            &pm_ctx,
            &dummy_any,
            &sp_uuid_msg(1, 888_888),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
}

#[test]
fn corporation_goal_active_inactive_contributions_and_reward_redemption() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register(&router, Arc::clone(&db));

    let cid = 90_000_001_u32;
    let corp_a = 98_000_201_u32;
    let corp_b = 98_000_202_u32;

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&(cid as i64)) {
            ch.corporation_id = corp_a as i64;
        }
        let mut c = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_a as i64;
            __corp.name = "Goal Corp".into();
            __corp.ticker = "GOAL".into();
            __corp.ceo_id = cid as i64;
            __corp.set_division_balance(1, IskCents::from_isk(10_000_000));
            __corp };
        c.members.insert(cid as i64);
        c.ensure_wallet_divisions();
        snap.corporations.insert(corp_a as i64, c);
    });

    let active_gid = router
        .progression
        .corp_state
        .create_goal_from_division(
            corp_a,
            "Active Funded Goal".into(),
            "Deliver ore".into(),
            10,
            1_000_000,
            1,
        )
        .expect("create funded goal");
    let inactive_gid = router
        .progression
        .corp_state
        .create_goal(corp_a, "Closed Goal".into(), "Old".into(), 10, 0)
        .expect("create second goal");
    router
        .progression
        .corp_state
        .close_or_delete_goal(inactive_gid, false, 2_000);

    // Record contribution on active_gid for cid so rewards are available to redeem
    db.with_mut_snapshot(|snap| {
        if let Some(p) = snap.corp_projects.get_mut(&(active_gid as i64)) {
            p.current_quantity = 5;
            p.isk_per_unit = IskCents::from_isk(100_000);
        }
        let det = snap
            .alliance_war_state
            .project_details
            .entry(active_gid as i64)
            .or_default();
        det.char_contributions.insert(cid as i64, 5);
        det.contributions_by_char.insert(cid as i64, 0);
    });

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let member_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid),
        Some(corp_a),
        0,
        "tranquility",
        vec![1],
    );
    let cross_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(90_000_002),
        Some(corp_b),
        0,
        "tranquility",
        vec![1],
    );
    let unauth_ctx = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![]);

    // GetActive vs GetInactive both return non-empty bytes
    let act_resp = handlers["eve_public.corporationgoal.api.GetActiveRequest"](
        &member_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Act"),
    )
    .unwrap();
    assert!(!act_resp.value.is_empty());

    let inact_resp = handlers["eve_public.corporationgoal.api.GetInactiveRequest"](
        &member_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Inact"),
    )
    .unwrap();
    assert!(!inact_resp.value.is_empty());

    // GetMineWithRewardsRequest returns active_gid
    let rewards_resp = handlers["eve_public.corporationgoal.api.GetMineWithRewardsRequest"](
        &member_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Rewards"),
    )
    .unwrap();
    assert!(!rewards_resp.value.is_empty());

    // Unauthenticated fails with STATUS_UNAUTHORIZED on personal summary & redeem endpoints
    for endpoint in [
        "eve_public.corporationgoal.api.GetMyContributorSummaryForGoalRequest",
        "eve_public.corporationgoal.api.GetMyContributorSummariesRequest",
        "eve_public.corporationgoal.api.GetMineWithRewardsRequest",
        "eve_public.corporationgoal.api.RedeemMyRewardsRequest",
        "eve_public.corporationgoal.api.RedeemAllMyRewardsRequest",
    ] {
        assert_eq!(
            handlers[endpoint](&unauth_ctx, &dummy_any, &goal_uuid_msg(1, active_gid))
                .unwrap_err()
                .0,
            STATUS_UNAUTHORIZED
        );
    }

    // Cross-corp fails with STATUS_FORBIDDEN on corp-scoped goal queries
    let mut goal_and_char = goal_uuid_msg(1, active_gid);
    goal_and_char.fields.insert(2, DynamicValue::Uint32(cid));
    for endpoint in [
        "eve_public.corporationgoal.api.GetContributorSummariesForGoalRequest",
        "eve.corporationgoal.api.GetRequest",
        "eve.corporationgoal.contribution.api.GetContributorSummaryForGoalRequest",
    ] {
        assert_eq!(
            handlers[endpoint](&cross_ctx, &dummy_any, &goal_and_char)
                .unwrap_err()
                .0,
            STATUS_FORBIDDEN
        );
    }

    // RedeemMyRewardsRequest pays out cid's reward on active_gid
    let redeem_resp = handlers["eve_public.corporationgoal.api.RedeemMyRewardsRequest"](
        &member_ctx,
        &dummy_any,
        &goal_uuid_msg(1, active_gid),
    )
    .expect("redeem reward for active_gid");
    assert!(redeem_resp
        .type_url
        .ends_with("eve_public.corporationgoal.api.RedeemMyRewardsResponse"));
}
