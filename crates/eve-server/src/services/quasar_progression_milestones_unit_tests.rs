//! Semantic unit tests split from `quasar_progression_extra_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::quasar_progression_extra_extra_unit_tests::*;

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_CONFLICT, STATUS_FORBIDDEN,
};
use crate::services::quasar_progression_extra::*;
use crate::services::quasar_social_corp::QuasarCorpState;
use crate::services::social_corp_fleet::social_corp_projects;
use eve_db::{
    corp_roles,
    d6_types::{CorpGovState, PersistedProjectDetails}, CorporationRecord, IskCents, WorldRepository,
};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, QuasarDescriptorRegistry,
    WireMessage,
};
use std::sync::Arc;

#[test]
fn generic_goals_escrow_roles_states_cross_corp_and_contributions() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let corp_state = QuasarCorpState::from_db(Arc::clone(&db));
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    let mgr = Arc::new(ProgressionExtraManager::new_with_sde(
        Some(Arc::clone(&db)),
        Some(Arc::clone(&sde)),
        Some(Arc::clone(&corp_state)),
    ));
    register_progression_extra_handlers(&router, Arc::clone(&db), Arc::clone(&mgr));

    let corp_a = 1_000_009_u32;
    let corp_b = 1_000_010_u32;
    let ceo_a = 90_000_001_u32;
    let pm_a = 90_000_002_u32;
    let outsider_b = 90_000_003_u32;

    db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.get(&(ceo_a as i64)).unwrap().clone();
        for (cid, c_id) in [(ceo_a, corp_a), (pm_a, corp_a), (outsider_b, corp_b)] {
            let mut ch = base_ch.clone();
            ch.character_id = cid as i64;
            ch.corporation_id = c_id as i64;
            snap.characters.insert(cid as i64, ch);
        }
        let mut corp = CorporationRecord {
            corporation_id: corp_a as i64,
            name: "Corp A".into(),
            ticker: "CRPA".into(),
            ceo_id: ceo_a as i64,
            master_wallet: IskCents::from_isk(10_000_000),
            ..Default::default()
        };
        corp.members.insert(ceo_a as i64);
        corp.members.insert(pm_a as i64);
        corp.member_roles
            .insert(pm_a as i64, corp_roles::ROLE_PROJECT_MANAGER);
        corp.set_division_balance(3, IskCents::from_isk(5_000_000));
        snap.corporations.insert(corp_a as i64, corp);
        let mut gov = CorpGovState::default();
        gov.member_account_keys.insert(pm_a as i64, 1002); // division 3 (1002 - 999 = 3)
        snap.corp_governance.insert(corp_a as i64, gov);
    });

    let anon_ctx = AuthoritativeContext::default();
    let pm_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(pm_a),
        Some(corp_a),
        corp_roles::ROLE_PROJECT_MANAGER,
        "tranquility",
        vec![1],
    );
    let pm_with_take_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(pm_a),
        Some(corp_a),
        corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2],
        "tranquility",
        vec![1],
    );
    let outsider_ctx = AuthoritativeContext::for_session(
        Some(3),
        Some(outsider_b),
        Some(corp_b),
        0,
        "tranquility",
        vec![1],
    );
    let mut gm_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(ceo_a),
        Some(corp_a),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    gm_ctx.service_role_mask = u64::MAX;

    // 1. Unauthenticated rejected on Create / Close / Delete / GetCapacity / Contributions / Seasonal
    let empty_msg = DynamicQuasarMessage::new("Empty");
    for ep in [
        "eve_public.goal.api.CreateRequest",
        "eve_public.goal.api.CloseRequest",
        "eve_public.goal.api.DeleteRequest",
        "eve_public.goal.api.GetCapacityRequest",
        "eve_public.goal.contribution.api.GetContributorSummariesForGoalRequest",
        "eve_public.goal.contribution.api.GetMyContributorSummaryForGoalRequest",
        "eve_public.goal.contribution.api.GetMyContributorSummariesRequest",
    ] {
        assert_eq!(call_gw(&router, ep, &anon_ctx, &empty_msg).unwrap_err().0, STATUS_FORBIDDEN);
    }
    assert_eq!(
        call_gw(
            &router,
            "eve.seasonalevent.challenge.GetChallengesForCharacterRequest",
            &anon_ctx,
            &empty_msg,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // 2. Non-admin Project Manager cannot seed initial progress (field 5 > 0)
    let mut seed_prog_msg = DynamicQuasarMessage::new("Create");
    seed_prog_msg.fields.insert(3, DynamicValue::Int64(50));
    seed_prog_msg.fields.insert(5, DynamicValue::Int64(10));
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.CreateRequest", &pm_ctx, &seed_prog_msg)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // 3. Project Manager without wallet division 3 take role rejected when escrow > 0, succeeds with take role
    let mut escrow_msg = DynamicQuasarMessage::new("Create");
    escrow_msg.fields.insert(1, DynamicValue::String("   ".into())); // blank -> defaults to "Organization Goal"
    escrow_msg.fields.insert(3, DynamicValue::Uint64(10));
    escrow_msg.fields.insert(8, DynamicValue::Uint64(1_000)); // 1000 ISK escrow
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.CreateRequest", &pm_ctx, &escrow_msg)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&(corp_a as i64))
            .unwrap()
            .member_roles
            .insert(
                pm_a as i64,
                corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2],
            );
    });
    call_gw(&router, "eve_public.goal.api.CreateRequest", &pm_with_take_ctx, &escrow_msg)
        .expect("PM with take role creates goal with escrow");

    // 4. GM creates goal with seeded initial progress (clamped to target)
    call_gw(&router, "eve_public.goal.api.CreateRequest", &gm_ctx, &seed_prog_msg)
        .expect("GM creates goal with initial progress");

    let pm_goal = mgr
        .generic_goals
        .read()
        .unwrap()
        .values()
        .find(|g| g.creator_id == pm_a)
        .unwrap()
        .clone();
    assert_eq!(pm_goal.name, "Organization Goal");
    let pid = social_corp_projects::goal_uuid_to_project_id(&pm_goal.uuid).unwrap();

    // 5. Cross-corp access denied on GetRequest & GetContributorSummariesForGoalRequest for non-admin outsider
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.GetRequest", &outsider_ctx, &uuid_msg(1, pm_goal.uuid))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call_gw(
            &router,
            "eve_public.goal.contribution.api.GetContributorSummariesForGoalRequest",
            &outsider_ctx,
            &uuid_msg(1, pm_goal.uuid),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    // GetAllRequest for outsider omits Corp A goals
    let out_all = call_gw(&router, "eve_public.goal.api.GetAllRequest", &outsider_ctx, &empty_msg).unwrap();
    assert!(out_all.value.is_empty());

    // 6. Seed char_contributions & test Expired (state 4) and Completed (state 3) in GetRequest
    db.with_mut_snapshot(|snap| {
        let det = snap
            .alliance_war_state
            .project_details
            .entry(pid)
            .or_insert_with(PersistedProjectDetails::default);
        det.char_contributions.insert(pm_a as i64, 7);
        det.expired = true;
    });
    let get_exp = call_gw(&router, "eve_public.goal.api.GetRequest", &pm_ctx, &uuid_msg(1, pm_goal.uuid)).unwrap();
    let wm_exp = WireMessage::decode(&get_exp.value).unwrap();
    let attr_exp = WireMessage::decode(wm_exp.fields[0].value.as_bytes().unwrap()).unwrap();
    assert!(attr_exp.fields.iter().any(|f| f.field_number == 6 && f.value.as_varint() == Some(4)));

    // Clear expired, complete goal via SetCurrentProgressRequest (0 -> 10) and verify state == 3
    db.with_mut_snapshot(|snap| {
        if let Some(det) = snap.alliance_war_state.project_details.get_mut(&pid) {
            det.expired = false;
            det.status = "Active".into();
        }
    });
    let mut prog_done = uuid_msg(1, pm_goal.uuid);
    prog_done.fields.insert(2, DynamicValue::Int64(0));
    prog_done.fields.insert(3, DynamicValue::Int64(10));
    call_gw(&router, "eve_public.goal.api.SetCurrentProgressRequest", &gm_ctx, &prog_done).unwrap();

    let get_done = call_gw(&router, "eve_public.goal.api.GetRequest", &pm_ctx, &uuid_msg(1, pm_goal.uuid)).unwrap();
    let wm_done = WireMessage::decode(&get_done.value).unwrap();
    let attr_done = WireMessage::decode(wm_done.fields[0].value.as_bytes().unwrap()).unwrap();
    assert!(attr_done.fields.iter().any(|f| f.field_number == 6 && f.value.as_varint() == Some(3)));

    // Verify contributor summaries use char_contributions
    let my_sum = call_gw(
        &router,
        "eve_public.goal.contribution.api.GetMyContributorSummaryForGoalRequest",
        &pm_ctx,
        &uuid_msg(1, pm_goal.uuid),
    )
    .unwrap();
    assert!(!my_sum.value.is_empty());
    let all_my = call_gw(
        &router,
        "eve_public.goal.contribution.api.GetMyContributorSummariesRequest",
        &pm_ctx,
        &empty_msg,
    )
    .unwrap();
    assert!(!all_my.value.is_empty());

    // 7. SetNameRequest rejects empty name; Outsider rejected on CloseRequest & DeleteRequest
    let mut bad_name = uuid_msg(1, pm_goal.uuid);
    bad_name.fields.insert(2, DynamicValue::String("   ".into()));
    assert_eq!(
        call_gw(&router, "eve.goal.api.SetNameRequest", &gm_ctx, &bad_name).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.CloseRequest", &outsider_ctx, &uuid_msg(1, pm_goal.uuid))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.DeleteRequest", &outsider_ctx, &uuid_msg(1, pm_goal.uuid))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // CAS mismatch on SetCurrentProgressRequest
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.SetCurrentProgressRequest", &gm_ctx, &prog_done)
            .unwrap_err()
            .0,
        STATUS_CONFLICT
    );
}
