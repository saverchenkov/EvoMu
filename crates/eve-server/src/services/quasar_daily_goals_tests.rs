//! Unit and mutation-killing tests for `quasar_daily_goals` (`feat:pve.daily-goals`).

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_PRECONDITION_FAILED,
};
use eve_db::{IskCents, WorldRepository};
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

const ROLE_PROGRAMMER: u64 = eve_machonet::service_roles::ROLE_GM_MASK;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

trait TestRouterExt {
    fn dispatch(
        &self,
        ctx: &AuthoritativeContext,
        req: &GoogleAny,
    ) -> Result<GoogleAny, (u32, String)>;
}
impl TestRouterExt for PublicGatewayRouter {
    fn dispatch(
        &self,
        ctx: &AuthoritativeContext,
        req: &GoogleAny,
    ) -> Result<GoogleAny, (u32, String)> {
        let norm = req.normalized_type_name();
        let handler = self
            .handlers
            .read()
            .unwrap()
            .get(norm)
            .cloned()
            .expect("registered handler");
        let dyn_msg = self
            .descriptors
            .decode_dynamic_or_wire(norm, &req.value)
            .unwrap_or_else(|_| {
                crate::services::quasar_char_skill_services::decode_wire_dynamic(norm, &req.value)
            });
        handler(ctx, req, &dyn_msg)
    }
}

fn make_router_and_db() -> (Arc<PublicGatewayRouter>, Arc<WorldRepository>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    db.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(600_000)));
    let sde = shared_sde();
    let router = Arc::new(PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    ));
    register_daily_goal_handlers(
        &router,
        Arc::clone(&db),
        Arc::clone(&router.progression.daily_goals),
    );
    (router, db)
}

fn seed_char(db: &WorldRepository, char_id: i64, corp_id: i64, isk: i64, em: i64) {
    db.with_mut_snapshot(|s| {
        let mut rec = s.characters[&90_000_001].clone();
        rec.character_id = char_id;
        rec.corporation_id = corp_id;
        rec.balance = IskCents::from_isk(isk);
        rec.free_skill_points = 1000.0;
        rec.loyalty_points.clear();
        if em > 0 {
            rec.loyalty_points.insert(1_000_419, em);
        }
        s.characters.insert(char_id, rec);
    });
}

fn make_ctx(char_id: Option<u32>, role: u64, now_unix: u64) -> AuthoritativeContext {
    AuthoritativeContext::for_session_at(
        Some(1),
        char_id,
        None,
        0,
        "tranquility".to_string(),
        b"evomu-quasar-gw1".to_vec(),
        now_unix,
    )
    .with_service_roles(role)
}

fn make_uuid_req(type_name: &str, uuid: &[u8; 16]) -> GoogleAny {
    pb::message(type_name)
        .set("goal.uuid", uuid)
        .must_to_any()
}

#[test]
fn template_effective_getters_and_day_bucket_boundaries() {
    // parity: feat:pve.daily-goals
    assert_eq!(daily_goal_day_bucket(0), 0);
    assert_eq!(daily_goal_day_bucket(39_599), 0);
    assert_eq!(daily_goal_day_bucket(39_600), 0);
    assert_eq!(daily_goal_day_bucket(39_600 + 86_399), 0);
    assert_eq!(daily_goal_day_bucket(39_600 + 86_400), 1);
    assert_eq!(daily_goal_day_bucket(39_600 + 172_800), 2);

    let mut tpl = default_daily_bonus_template();
    assert_eq!(tpl.effective_goal_category(), 2);
    assert_eq!(tpl.effective_name_message_id(), 722_674);
    assert_eq!(tpl.effective_description_message_id(), 698_433);
    assert_eq!(tpl.effective_help_text_message_id(), 698_433);
    assert_eq!(tpl.effective_assigner_corporation_id(), 1_000_419);

    tpl.name_message_id = 0;
    tpl.description_message_id = 0;
    tpl.help_text_message_id = 0;
    tpl.assigner_corporation_id = 0;

    let expected = [
        (1, 1, 659_195, 696_251, 659_196),
        (2, 1, 659_218, 696_247, 659_219),
        (3, 1, 665_435, 696_245, 665_436),
        (4, 1, 541_824, 1_041_584, 1_004_955),
        (5, 1, 665_394, 696_246, 665_412),
        (6, 2, 722_674, 698_433, 698_433),
        (99, 1, 582_520, 582_521, 582_521),
    ];
    for (cat, eff_cat, name_id, desc_id, help_id) in expected {
        tpl.category = cat;
        assert_eq!(tpl.effective_goal_category(), eff_cat);
        assert_eq!(tpl.effective_name_message_id(), name_id);
        assert_eq!(tpl.effective_description_message_id(), desc_id);
        assert_eq!(tpl.effective_help_text_id_or_default_check(&tpl), help_id);
        assert_eq!(tpl.effective_assigner_corporation_id(), 1_000_419);
    }

    tpl.assigner_corporation_id = 1_000_125;
    assert_eq!(tpl.effective_assigner_corporation_id(), 1_000_125);
}

trait HelpCheck {
    fn effective_help_text_id_or_default_check(&self, tpl: &DailyGoalTemplate) -> u32 {
        tpl.effective_help_text_message_id()
    }
}
impl HelpCheck for DailyGoalTemplate {}

#[test]
fn build_sde_templates_and_manager_persistence_roundtrip() {
    // parity: feat:pve.daily-goals
    let sde = shared_sde();
    let templates = build_sde_daily_goal_templates(&sde);
    assert_eq!(templates.len(), 6);
    assert_eq!(templates[0].category, 1);
    assert_eq!(templates[0].target, 25);
    assert_eq!(templates[1].category, 2);
    assert_eq!(templates[1].completion_cost_isk, None);
    assert_eq!(templates[1].completion_cost_evermarks, Some(2_500));
    assert_eq!(templates[5].category, 6);
    assert_eq!(templates[5].effective_goal_category(), 2);

    let db = Arc::new(WorldRepository::with_demo_seed());
    let mgr = DailyGoalManager::new_with_sde(Some(Arc::clone(&db)), Some(sde));
    assert_eq!(mgr.get_templates().len(), 6);

    let goals_init = mgr.get_or_init_goals_at(5001, 200_000);
    assert_eq!(goals_init.len(), 5);
    let expected_bucket = daily_goal_day_bucket(200_000);
    assert!(goals_init.iter().all(|g| g.day_bucket == expected_bucket));

    // Add progress and configure a custom template
    let first_uuid = goals_init[0].uuid;
    let (prog_goal, just_completed) = mgr
        .add_progress(5001, &first_uuid, 10, 200_010)
        .expect("progress added");
    assert_eq!(prog_goal.current_progress, 10);
    assert!(!just_completed);

    let mut custom_tpl = templates[0].clone();
    custom_tpl.uuid = [0xAA; 16];
    custom_tpl.target = 3;
    mgr.configure_template(custom_tpl.clone());
    assert_eq!(mgr.get_templates().len(), 7);

    // Update existing template via configure_template
    custom_tpl.target = 4;
    mgr.configure_template(custom_tpl.clone());
    assert_eq!(mgr.get_templates().len(), 7);
    assert_eq!(
        mgr.get_templates()
            .iter()
            .find(|t| t.uuid == [0xAA; 16])
            .unwrap()
            .target,
        4
    );

    // Reload from DB snapshot
    let mgr_reloaded = DailyGoalManager::new(Some(Arc::clone(&db)));
    assert_eq!(mgr_reloaded.get_templates().len(), 7);
    let reloaded_goals = mgr_reloaded.get_or_init_goals_at(5001, 200_020);
    assert_eq!(reloaded_goals.len(), 6);

    // Test legacy HashMap<u32, Vec<CharacterDailyGoal>> deserialization fallback + auto bonus injection
    let only_reg_goal = CharacterDailyGoal {
        uuid: templates[0].uuid,
        template: templates[0].clone(),
        current_progress: 5,
        completed: false,
        claimed: false,
        paid_completion: false,
        completed_at_unix: None,
        claimed_at_unix: None,
        day_bucket: 0,
        expired: false,
    };
    let mut legacy_map = HashMap::new();
    legacy_map.insert(7777u32, vec![only_reg_goal]);
    let legacy_json = serde_json::to_string(&legacy_map).unwrap();
    db.try_with_mut_snapshot(|snap| {
        snap.quasar_state_blobs
            .insert("quasar_daily_goals".to_string(), legacy_json);
        Ok::<(), String>(())
    })
    .unwrap();
    let mgr_legacy = DailyGoalManager::new(Some(Arc::clone(&db)));
    // Should have extracted the 1 template + appended default_daily_bonus_template()
    assert_eq!(mgr_legacy.get_templates().len(), 2);
    let goals_7777 = mgr_legacy.get_or_init_goals_at(7777, 300_000);
    assert_eq!(goals_7777.len(), 2);
    assert!(goals_7777.iter().all(|g| g.day_bucket == daily_goal_day_bucket(300_000)));

    // Test downtime rotation reset
    let rotated = mgr_legacy.reset_daily_goals_on_downtime(300_000 + 86_400);
    assert_eq!(rotated, 1);
    let rotated_again = mgr_legacy.reset_daily_goals_on_downtime(300_000 + 86_400);
    assert_eq!(rotated_again, 0);
    let after_rot = mgr_legacy.get_or_init_goals_at(7777, 300_000 + 86_400);
    assert_eq!(after_rot[0].current_progress, 0);
}

#[test]
fn set_templates_and_encode_attributes_all_categories() {
    // parity: feat:pve.daily-goals
    let mgr = DailyGoalManager::default();
    assert_eq!(mgr.now_unix(), 0);
    let mut tpls = Vec::new();
    for cat in 1..=7 {
        let mut t = default_daily_bonus_template();
        t.uuid = [cat as u8; 16];
        t.category = cat;
        t.career = cat;
        t.isk_reward = if cat % 2 == 1 { 100_000 } else { 0 };
        t.sp_reward = if cat % 2 == 0 { 2_500.0 } else { 0.0 };
        let mut encoded = Vec::new();
        encode_goal_attributes_for_day_bucket(&t, 20_548, &mut encoded);
        assert!(!encoded.is_empty());
        // Verify fields 10 (active_after) and 11 (active_until) are encoded as non-epoch timestamps
        let decoded_attrs = QuasarDescriptorRegistry::empty_with_builtins()
            .decode_dynamic_or_wire("", &encoded)
            .expect("decode Attributes");
        let after_secs = match decoded_attrs.get(10) {
            Some(DynamicValue::Message(m)) => match m.get(1) {
                Some(DynamicValue::Uint64(s)) => *s,
                other => panic!("unexpected active_after.seconds: {other:?}"),
            },
            other => panic!("missing field 10 active_after: {other:?}"),
        };
        let until_secs = match decoded_attrs.get(11) {
            Some(DynamicValue::Message(m)) => match m.get(1) {
                Some(DynamicValue::Uint64(s)) => *s,
                other => panic!("unexpected active_until.seconds: {other:?}"),
            },
            other => panic!("missing field 11 active_until: {other:?}"),
        };
        assert!(after_secs > 1_700_000_000 && until_secs == after_secs + 86_400);
        let mut earn = Vec::new();
        encode_goal_earnings(&t, 4, &mut earn);
        if t.isk_reward > 0 || t.sp_reward > 0.0 {
            assert!(!earn.is_empty());
        }
        tpls.push(t);
    }
    mgr.set_templates(tpls);
    assert_eq!(mgr.get_templates().len(), 7);
    let init = mgr.get_or_init_goals(9001);
    assert_eq!(init.len(), 7);
}

#[test]
fn redeem_and_pay_for_completion_happy_and_error_paths() {
    // parity: feat:pve.daily-goals
    let (router, db) = make_router_and_db();
    let char_id = 211200010u32;
    let corp_id = 98000010i64;
    seed_char(&db, char_id as i64, corp_id, 5_000_000, 3_000);

    let mgr = &router.progression.daily_goals;
    let goals = mgr.get_or_init_goals_at(char_id, 300_000);
    assert_eq!(goals.len(), 5);
    let g1_uuid = goals[0].uuid; // KillNPC (target 25, cost EM=2500, ISK=None)
    let g2_uuid = goals[1].uuid; // MineOre (target 2000, cost EM=2500, ISK=None)
    let g3_uuid = goals[2].uuid; // ScanSignature (target 5, cost EM=2500, ISK=None)
    let bonus_uuid = goals[4].uuid; // DailyBonus (target 2, cost EM=None, ISK=None)

    // Redeem before completion -> FORBIDDEN
    let err = mgr.redeem_goal(char_id, &g1_uuid, 300_000, &db).unwrap_err();
    assert_eq!(err.0, STATUS_FORBIDDEN);

    // Redeem unknown char -> NOT_FOUND
    let err = mgr.redeem_goal(999999, &g1_uuid, 300_000, &db).unwrap_err();
    assert_eq!(err.0, STATUS_NOT_FOUND);

    // Redeem unknown goal -> NOT_FOUND
    let err = mgr.redeem_goal(char_id, &[0xFF; 16], 300_000, &db).unwrap_err();
    assert_eq!(err.0, STATUS_NOT_FOUND);

    // Pay for completion on bonus goal (no completion cost configured) -> PRECONDITION_FAILED
    let err = mgr.pay_for_completion(char_id, &bonus_uuid, 300_000, &db).unwrap_err();
    assert_eq!(err.0, STATUS_PRECONDITION_FAILED);

    // Pay for completion on unknown goal -> NOT_FOUND
    let err = mgr.pay_for_completion(char_id, &[0xEE; 16], 300_000, &db).unwrap_err();
    assert_eq!(err.0, STATUS_NOT_FOUND);

    // Pay for completion on g1 using EverMarks (char has 3000 EM, 1st paid cost is 2500 EM -> 500 EM left)
    let paid1 = mgr.pay_for_completion(char_id, &g1_uuid, 300_010, &db).expect("pay g1 with EM");
    assert!(paid1.completed);
    assert!(paid1.paid_completion);
    assert_eq!(paid1.current_progress, 25);

    // Paying again on already-completed g1 -> BAD_REQUEST
    let err = mgr.pay_for_completion(char_id, &g1_uuid, 300_011, &db).unwrap_err();
    assert_eq!(err.0, STATUS_BAD_REQUEST);

    // Insufficient EM on g3 (500 EM left < 4500 EM progressive 2nd cost, no ISK fallback) -> BAD_REQUEST
    let err = mgr.pay_for_completion(char_id, &g3_uuid, 300_015, &db).unwrap_err();
    assert_eq!(err.0, STATUS_BAD_REQUEST);

    // Top up +4,000 EM (to 4,500 EM) so 2nd paid completion succeeds at progressive 4,500 EM cost
    db.with_mut_snapshot(|snap| {
        *snap
            .characters
            .get_mut(&(char_id as i64))
            .unwrap()
            .loyalty_points
            .entry(1_000_419)
            .or_insert(0) += 4_000;
    });
    let paid2 = mgr.pay_for_completion(char_id, &g2_uuid, 300_020, &db).expect("pay g2 with 4,500 EM");
    assert!(paid2.completed);
    assert!(paid2.paid_completion);

    // Completing 2 regular goals via pay_for_completion also completes the bonus goal (target 2) and advances Monthly Track!
    let current = mgr.get_or_init_goals_at(char_id, 300_020);
    let bonus = current.iter().find(|g| g.uuid == bonus_uuid).unwrap();
    assert_eq!(bonus.current_progress, 2);
    assert!(bonus.completed);
    assert_eq!(mgr.get_monthly_progress(char_id, 300_020).steps_completed, 1);

    // Paid completion is unavailable once the Daily SP bonus has been reached -> PRECONDITION_FAILED
    let err = mgr.pay_for_completion(char_id, &g3_uuid, 300_025, &db).unwrap_err();
    assert_eq!(err.0, STATUS_PRECONDITION_FAILED);

    // Redeem g1 -> credits 500,000 ISK, 0 SP, 500 EverMarks to char + 500 EverMarks to corp
    let redeemed1 = mgr.redeem_goal(char_id, &g1_uuid, 300_030, &db).expect("redeem g1");
    assert!(redeemed1.claimed);
    assert_eq!(redeemed1.claimed_at_unix, Some(300_030));

    // Double redeem -> FORBIDDEN
    let err = mgr.redeem_goal(char_id, &g1_uuid, 300_031, &db).unwrap_err();
    assert_eq!(err.0, STATUS_FORBIDDEN);

    // Redeem bonus goal -> credits 10,000 SP, 0 ISK, 0 EverMarks
    let redeemed_bonus = mgr.redeem_goal(char_id, &bonus_uuid, 300_032, &db).expect("redeem bonus");
    assert!(redeemed_bonus.claimed);

    let (bal, sp, char_em, corp_em) = db.with_snapshot(|snap| {
        let ch = snap.characters.get(&(char_id as i64)).unwrap();
        let ce = snap.corp_lp_balances.get(&(corp_id, 1_000_419)).copied().unwrap_or(0);
        (
            ch.balance,
            ch.free_skill_points,
            ch.loyalty_points.get(&1_000_419).copied().unwrap_or(0),
            ce,
        )
    });
    // Started 5,000,000 + 500,000 (g1 reward) + 0 (bonus reward) = 5,500,000 ISK (no ISK debited for paid completion)
    assert_eq!(bal, IskCents::from_isk(5_500_000));
    // Started 1,000 + 0 (g1 individual) + 10,000 (DailyBonus) = 11,000 SP
    assert_eq!(sp, 11_000.0);
    // Started 3,000 + 4,000 - 2,500 (g1 paid) - 4,500 (g2 paid) + 500 (g1 reward) + 0 (bonus reward) = 500 EM
    assert_eq!(char_em, 500);
    assert_eq!(corp_em, 500);
    // SpaceJump default target is 3 (`AIR_Daily_Goals.md`) when rotated in on next day
    let next_day_goals = mgr.get_or_init_goals_at(char_id, 300_000 + 86_400);
    assert_eq!(
        next_day_goals
            .iter()
            .find(|g| g.template.category == 4)
            .expect("SpaceJump active on day+1")
            .template
            .target,
        3
    );

    // Monthly Progress Track advances once per UTC day up to 12 and awards milestones at step 3
    let base_day = 1_735_732_800u64; // 2025-01-01 12:00:00 UTC
    mgr.record_daily_bonus_completion(char_id, base_day);
    mgr.record_daily_bonus_completion(char_id, base_day + 3_600); // same day bucket -> idempotent
    assert_eq!(mgr.get_monthly_progress(char_id, base_day).steps_completed, 1);
    mgr.record_daily_bonus_completion(char_id, base_day + 86_400);
    mgr.record_daily_bonus_completion(char_id, base_day + 2 * 86_400);
    let m3 = mgr.get_monthly_progress(char_id, base_day + 2 * 86_400);
    assert_eq!(m3.steps_completed, 3);
    let bal_before_m3 = db.with_snapshot(|s| s.characters[&(char_id as i64)].balance);
    let m3_claimed = mgr
        .claim_monthly_milestone(char_id, 3, true, base_day + 2 * 86_400, &db)
        .expect("claim milestone 3");
    assert_eq!(m3_claimed.claimed_milestones, vec![3]);
    let bal_after_m3 = db.with_snapshot(|s| s.characters[&(char_id as i64)].balance);
    assert_eq!(bal_after_m3, IskCents(bal_before_m3.0 + IskCents::from_isk(1_200_000).0));
}

#[test]
fn gameplay_events_and_all_12_rpc_handlers() {
    // parity: feat:pve.daily-goals
    let (router, db) = make_router_and_db();
    let char_id = 211200020u32;
    seed_char(&db, char_id as i64, 98000020, 10_000_000, 10_000);

    let anon = make_ctx(None, 0, 600_000);
    let user = make_ctx(Some(char_id), 0, 600_000);
    let admin = make_ctx(Some(char_id), ROLE_PROGRAMMER, 600_000);

    // Unauthenticated checks
    for rpc in [
        "eve_public.dailygoal.api.GetAllCurrentRequest",
        "eve_public.dailygoal.api.GetRequest",
        "eve_public.dailygoal.api.GetAllCompletedWithEntitlementsRequest",
        "eve_public.dailygoal.api.GetAllWithRewardsRequest",
        "eve_public.dailygoal.api.RedeemRequest",
        "eve_public.dailygoal.api.RedeemAllRequest",
        "eve_public.dailygoal.api.PayForCompletionRequest",
        "eve.dailygoal.api.GetRequest",
        "eve.dailygoal.api.GetAllRequest",
        "eve.dailygoal.api.GetProgressRequest",
    ] {
        let req = GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rpc}"),
            value: vec![],
        };
        assert_eq!(router.dispatch(&anon, &req).unwrap_err().0, STATUS_FORBIDDEN);
    }

    // 1. GetAllCurrentRequest & 10. eve.dailygoal.api.GetAllRequest
    let get_all_curr = router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.GetAllCurrentRequest"),
                value: vec![],
            },
        )
        .expect("GetAllCurrentRequest");
    assert!(!get_all_curr.value.is_empty());

    let get_all_internal = router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.GetAllRequest"),
                value: vec![],
            },
        )
        .expect("eve.dailygoal.api.GetAllRequest");
    let cur_dyn = router
        .descriptors
        .decode_dynamic("eve_public.dailygoal.api.GetAllCurrentResponse", &get_all_curr.value)
        .expect("decode GetAllCurrentResponse");
    let int_dyn = router
        .descriptors
        .decode_dynamic("eve.dailygoal.api.GetAllResponse", &get_all_internal.value)
        .expect("decode eve.dailygoal.api.GetAllResponse");
    let cur_uuids: Vec<Vec<u8>> = match cur_dyn.get(1) {
        Some(DynamicValue::Repeated(items)) => items
            .iter()
            .filter_map(|it| match it {
                DynamicValue::Message(g) => match g.get(1) {
                    Some(DynamicValue::Message(id)) => match id.get(1) {
                        Some(DynamicValue::Bytes(b)) => Some(b.clone()),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let int_uuids: Vec<Vec<u8>> = match int_dyn.get(1) {
        Some(DynamicValue::Repeated(items)) => items
            .iter()
            .filter_map(|it| match it {
                DynamicValue::Message(id) => match id.get(1) {
                    Some(DynamicValue::Bytes(b)) => Some(b.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    assert_eq!(cur_uuids, int_uuids);

    let goals = router.progression.daily_goals.get_or_init_goals_at(char_id, 600_000);
    let g1_uuid = goals.iter().find(|g| g.template.category == 1).unwrap().uuid; // KillNPC (cat 1, target 25)
    let g4_uuid = goals.iter().find(|g| g.template.category == 4).unwrap().uuid; // SpaceJump (cat 4, target 3)
    let g5_uuid = goals.iter().find(|g| g.template.category == 5).unwrap().uuid; // ManufactureItem (cat 5, target 1)

    // 2. GetRequest & 9. eve.dailygoal.api.GetRequest & 11. eve.dailygoal.api.GetProgressRequest
    for rpc in [
        "eve_public.dailygoal.api.GetRequest",
        "eve.dailygoal.api.GetRequest",
        "eve.dailygoal.api.GetProgressRequest",
    ] {
        // Missing uuid -> BAD_REQUEST
        let bad = GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rpc}"),
            value: vec![],
        };
        assert_eq!(router.dispatch(&user, &bad).unwrap_err().0, STATUS_BAD_REQUEST);
        // Unknown uuid -> NOT_FOUND
        let nf = make_uuid_req(rpc, &[0x99; 16]);
        assert_eq!(router.dispatch(&user, &nf).unwrap_err().0, STATUS_NOT_FOUND);
        // Valid uuid -> OK
        let ok = router.dispatch(&user, &make_uuid_req(rpc, &g1_uuid)).expect(rpc);
        assert!(!ok.value.is_empty());
    }

    // Trigger gameplay event for category 5 (target 1) -> completes g5
    router
        .progression
        .daily_goals
        .on_gameplay_event(char_id, 5, 1, &router);
    let routed: Vec<String> = router
        .routing_cache
        .read()
        .unwrap()
        .entries
        .iter()
        .map(|e| e.notice.payload.as_ref().unwrap().type_url.clone())
        .collect();
    assert!(routed.iter().any(|u| u.ends_with("ProgressedNotice")));
    assert!(routed.iter().any(|u| u.ends_with("CompletedNotice")));

    // Check GetProgressRequest when completed (encodes timestamp at field 4 instead of field 3)
    let prog_done = router
        .dispatch(&user, &make_uuid_req("eve.dailygoal.api.GetProgressRequest", &g5_uuid))
        .expect("GetProgressRequest completed");
    let prog_dyn =
        crate::services::quasar_char_skill_services::decode_wire_dynamic("test.Msg", &prog_done.value);
    assert_eq!(prog_dyn.get(1), Some(&DynamicValue::Uint64(1)));
    assert!(prog_dyn.get(4).is_some());

    // 7. PayForCompletionRequest on g4
    let pay_resp = router
        .dispatch(
            &user,
            &make_uuid_req("eve_public.dailygoal.api.PayForCompletionRequest", &g4_uuid),
        )
        .expect("PayForCompletionRequest");
    assert!(pay_resp.type_url.ends_with("PayForCompletionResponse"));

    // 3. GetAllCompletedWithEntitlementsRequest & 4. GetAllWithRewardsRequest
    let comp_resp = router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.GetAllCompletedWithEntitlementsRequest"
                ),
                value: vec![],
            },
        )
        .expect("GetAllCompletedWithEntitlementsRequest");
    assert!(!comp_resp.value.is_empty());

    let rew_resp = router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.GetAllWithRewardsRequest"),
                value: vec![],
            },
        )
        .expect("GetAllWithRewardsRequest");
    assert!(!rew_resp.value.is_empty());

    // 5. RedeemRequest on g5
    let red_resp = router
        .dispatch(&user, &make_uuid_req("eve_public.dailygoal.api.RedeemRequest", &g5_uuid))
        .expect("RedeemRequest");
    assert!(red_resp.type_url.ends_with("RedeemResponse"));

    // 6. RedeemAllRequest (redeems remaining completed goals: g4 + bonus goal)
    let red_all = router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.RedeemAllRequest"),
                value: vec![],
            },
        )
        .expect("RedeemAllRequest");
    assert!(red_all.type_url.ends_with("RedeemAllResponse"));

    // 8. eve_public.dailygoal.api.admin.ProgressRequest
    let admin_prog_req = pb::message("eve_public.dailygoal.api.admin.ProgressRequest")
        .set("goal.uuid", &g1_uuid)
        .set("character.sequential", char_id)
        .set("progress", 25u64)
        .must_to_any();
    assert_eq!(router.dispatch(&user, &admin_prog_req).unwrap_err().0, STATUS_FORBIDDEN);
    let admin_prog_resp = router.dispatch(&admin, &admin_prog_req).expect("admin ProgressRequest");
    assert!(admin_prog_resp.type_url.ends_with("ProgressResponse"));

    // 12. eve.dailygoal.api.admin.AutoRedeemRequest
    let auto_req = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.AutoRedeemRequest"),
        value: vec![],
    };
    assert_eq!(router.dispatch(&user, &auto_req).unwrap_err().0, STATUS_FORBIDDEN);
    let auto_resp = router.dispatch(&admin, &auto_req).expect("admin AutoRedeemRequest");
    assert!(auto_resp.type_url.ends_with("AutoRedeemResponse"));
}

#[cfg(test)]
#[path = "quasar_daily_goals_extra_unit_tests.rs"]
mod quasar_daily_goals_extra_unit_tests;

#[test]
fn daily_goals_select_four_rotating_plus_bonus_across_five_day_cycle() {
    let (router, _db) = make_router_and_db();
    let mgr = &router.progression.daily_goals;
    let char_id = 211200030u32;
    let mut category_appearances: HashMap<u32, usize> = HashMap::new();
    let mut omitted_categories = Vec::new();

    let day0_ts = 1_735_732_800u64;
    for day in 0u64..5 {
        let active = mgr.get_or_init_goals_at(char_id, day0_ts + day * 86_400);
        assert_eq!(
            active.len(),
            5,
            "each day must return 4 rotating daily goals + 1 DailyBonus (5 total)"
        );
        let regular: Vec<u32> = active
            .iter()
            .filter(|g| g.template.category != 6)
            .map(|g| g.template.category)
            .collect();
        let bonus_cnt = active.iter().filter(|g| g.template.category == 6).count();
        assert_eq!(regular.len(), 4);
        assert_eq!(bonus_cnt, 1);

        for &cat in &regular {
            *category_appearances.entry(cat).or_insert(0) += 1;
        }
        for cat in 1u32..=5 {
            if !regular.contains(&cat) {
                omitted_categories.push(cat);
            }
        }
    }

    assert_eq!(category_appearances.len(), 5);
    for cat in 1u32..=5 {
        assert_eq!(
            category_appearances.get(&cat).copied(),
            Some(4),
            "category {cat} should appear 4 of 5 days"
        );
    }
    omitted_categories.sort_unstable();
    assert_eq!(omitted_categories, vec![1, 2, 3, 4, 5]);
}

#[test]
fn pay_for_completion_evermarks_progressive_pricing_and_bonus_cap() {
    let (_router, db) = make_router_and_db();
    let mgr = DailyGoalManager::new_with_sde(
        Some(Arc::clone(&db)),
        Some(Arc::new(eve_sde::SdeRegistry::default())),
    );
    let char_id = 211_200_035u32;
    let t0 = 1_735_800_000u64;
    // Exact sum of the 2 allowed daily paid completions: 2,500 + 4,500 = 7,000 EM
    seed_char(&db, i64::from(char_id), 1_000_009, 10_000_000, 7_000);

    let active = mgr.get_or_init_goals_at(char_id, t0);
    let regular: Vec<[u8; 16]> = active
        .iter()
        .filter(|g| g.template.category != 6)
        .map(|g| g.uuid)
        .collect();
    let bonus_uuid = active
        .iter()
        .find(|g| g.template.category == 6)
        .unwrap()
        .uuid;

    // Bonus goal (category 6) cannot be paid-completed -> STATUS_PRECONDITION_FAILED
    assert_eq!(
        mgr.pay_for_completion(char_id, &bonus_uuid, t0, &db)
            .unwrap_err()
            .0,
        STATUS_PRECONDITION_FAILED
    );

    // Missing character in DB -> STATUS_BAD_REQUEST
    let _ = mgr.get_or_init_goals_at(211_200_999, t0);
    assert_eq!(
        mgr.pay_for_completion(211_200_999, &regular[0], t0, &db)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // Template with `completion_cost_evermarks = Some(0)` -> STATUS_PRECONDITION_FAILED
    let mut no_em_tpl = active
        .iter()
        .find(|g| g.uuid == regular[3])
        .unwrap()
        .template
        .clone();
    no_em_tpl.uuid = [0xEE; 16];
    no_em_tpl.category = 7;
    no_em_tpl.completion_cost_evermarks = Some(0);
    mgr.configure_template(no_em_tpl);
    let _ = mgr.get_or_init_goals_at(char_id, t0);
    assert_eq!(
        mgr.pay_for_completion(char_id, &[0xEE; 16], t0, &db)
            .unwrap_err()
            .0,
        STATUS_PRECONDITION_FAILED
    );

    // 1st paid completion costs 2,500 EM -> remaining 4,500 EM
    let g0_done = mgr
        .pay_for_completion(char_id, &regular[0], t0, &db)
        .expect("1st EM completion (2,500)");
    assert!(g0_done.completed && g0_done.paid_completion);
    assert_eq!(
        db.with_snapshot(|s| s.characters[&i64::from(char_id)].loyalty_points[&1_000_419]),
        4_500
    );

    // Paying again for already-completed goal -> STATUS_BAD_REQUEST
    assert_eq!(
        mgr.pay_for_completion(char_id, &regular[0], t0, &db)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // 2nd paid completion at exact boundary where balance == cost (4,500 == 4,500) succeeds -> remaining 0 EM
    let g1_done = mgr
        .pay_for_completion(char_id, &regular[1], t0 + 100, &db)
        .expect("2nd EM completion at exact balance == 4,500");
    assert!(g1_done.completed && g1_done.paid_completion);
    assert_eq!(
        db.with_snapshot(|s| s.characters[&i64::from(char_id)].loyalty_points[&1_000_419]),
        0
    );

    // Daily bonus is now completed (2/2) -> 3rd paid completion is rejected with STATUS_PRECONDITION_FAILED
    assert_eq!(
        mgr.pay_for_completion(char_id, &regular[2], t0 + 200, &db)
            .unwrap_err()
            .0,
        STATUS_PRECONDITION_FAILED
    );
}

#[test]
fn monthly_milestone_rewards_utc_year_month_and_claim_lifecycle() {
    assert_eq!(
        monthly_milestone_rewards(3),
        Some(MonthlyMilestoneReward {
            alpha_isk: 200_000,
            omega_isk: 1_000_000,
            ..Default::default()
        })
    );
    assert_eq!(
        monthly_milestone_rewards(6),
        Some(MonthlyMilestoneReward {
            alpha_em: 2_250,
            omega_em: 9_000,
            ..Default::default()
        })
    );
    assert_eq!(
        monthly_milestone_rewards(9),
        Some(MonthlyMilestoneReward {
            alpha_sp: 25_000.0,
            omega_sp: 50_000.0,
            ..Default::default()
        })
    );
    assert_eq!(
        monthly_milestone_rewards(12),
        Some(MonthlyMilestoneReward {
            omega_sp: 75_000.0,
            ..Default::default()
        })
    );
    for invalid in [0_u32, 1, 2, 4, 5, 7, 8, 10, 11, 13] {
        assert_eq!(monthly_milestone_rewards(invalid), None);
    }

    // Verify civil-from-days UTC (year, month) across Jan, Feb, Mar, Dec
    assert_eq!(utc_year_month(1_735_689_600), (2025, 1));  // 2025-01-01 00:00 UTC
    assert_eq!(utc_year_month(1_738_368_000), (2025, 2));  // 2025-02-01 00:00 UTC
    assert_eq!(utc_year_month(1_740_787_200), (2025, 3));  // 2025-03-01 00:00 UTC
    assert_eq!(utc_year_month(1_764_547_200), (2025, 12)); // 2025-12-01 00:00 UTC

    let (_router, db) = make_router_and_db();
    let mgr = DailyGoalManager::new(Some(Arc::clone(&db)));
    let alpha_cid = 211_200_040_u32;
    let omega_cid = 211_200_041_u32;
    seed_char(&db, i64::from(alpha_cid), 1_000_009, 0, 0);
    seed_char(&db, i64::from(omega_cid), 1_000_009, 0, 0);

    let jan_day0 = 1_735_732_800_u64; // 2025-01-01 12:00 UTC
    // Invalid milestone -> STATUS_BAD_REQUEST
    assert_eq!(
        mgr.claim_monthly_milestone(alpha_cid, 5, false, jan_day0, &db)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // Duplicate completion in same downtime day bucket only increments once
    assert_eq!(mgr.record_daily_bonus_completion(alpha_cid, jan_day0).steps_completed, 1);
    assert_eq!(mgr.record_daily_bonus_completion(alpha_cid, jan_day0 + 3_600).steps_completed, 1);
    assert_eq!(mgr.record_daily_bonus_completion(alpha_cid, jan_day0 + 86_400).steps_completed, 2);

    // Precondition failed when steps_completed (2) < milestone (3)
    assert_eq!(
        mgr.claim_monthly_milestone(alpha_cid, 3, false, jan_day0 + 86_400, &db)
            .unwrap_err()
            .0,
        STATUS_PRECONDITION_FAILED
    );

    // Step 3 reached -> Alpha claim credits 200,000 ISK; repeat claim is forbidden
    assert_eq!(mgr.record_daily_bonus_completion(alpha_cid, jan_day0 + 2 * 86_400).steps_completed, 3);
    let claimed_alpha = mgr
        .claim_monthly_milestone(alpha_cid, 3, false, jan_day0 + 2 * 86_400, &db)
        .expect("alpha milestone 3");
    assert_eq!(claimed_alpha.claimed_milestones, vec![3]);
    assert_eq!(
        db.with_snapshot(|s| s.characters[&i64::from(alpha_cid)].balance.0 / 100),
        200_000
    );
    assert_eq!(
        mgr.claim_monthly_milestone(alpha_cid, 3, false, jan_day0 + 2 * 86_400, &db)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // Cap at 12 steps across 14 distinct days in January; Omega claims 3, 6, 9, 12
    for d in 0..14_u64 {
        let p = mgr.record_daily_bonus_completion(omega_cid, jan_day0 + d * 86_400);
        assert_eq!(p.steps_completed, ((d + 1) as u32).min(12));
    }
    for m in [3_u32, 6, 9, 12] {
        mgr.claim_monthly_milestone(omega_cid, m, true, jan_day0 + 13 * 86_400, &db)
            .expect("omega milestone claim");
    }
    db.with_snapshot(|s| {
        let ch = &s.characters[&i64::from(omega_cid)];
        assert_eq!(ch.balance.0 / 100, 1_200_000);
        assert_eq!(ch.loyalty_points[&1_000_419], 11_250);
        assert_eq!(ch.free_skill_points, 151_000.0);
    });

    // Missing character in DB rolls back claimed_milestones
    let ghost_cid = 211_200_998_u32;
    for d in 0..3_u64 {
        mgr.record_daily_bonus_completion(ghost_cid, jan_day0 + d * 86_400);
    }
    assert_eq!(
        mgr.claim_monthly_milestone(ghost_cid, 3, true, jan_day0 + 2 * 86_400, &db)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert!(mgr
        .get_monthly_progress(ghost_cid, jan_day0 + 2 * 86_400)
        .claimed_milestones
        .is_empty());

    // Month rollover (February) resets steps_completed to 0 (or 1 on completion) and clears claimed_milestones
    let feb_day0 = 1_738_411_200_u64; // 2025-02-01 12:00 UTC
    let feb_prog = mgr.get_monthly_progress(omega_cid, feb_day0);
    assert_eq!(feb_prog.year_month, (2025, 2));
    assert_eq!(feb_prog.steps_completed, 0);
    assert!(feb_prog.claimed_milestones.is_empty());
    let feb_rec = mgr.record_daily_bonus_completion(omega_cid, feb_day0);
    assert_eq!(feb_rec.steps_completed, 1);
}

#[test]
fn utc_year_month_all_calendar_and_leap_century_boundaries() {
    let cases = [
        (0_u64, (1970, 1)),
        (5_097_599_u64, (1970, 2)),
        (5_097_600_u64, (1970, 3)),
        (68_212_800_u64, (1972, 2)),
        (946_684_799_u64, (1999, 12)),
        (951_782_399_u64, (2000, 2)),
        (951_782_400_u64, (2000, 2)),
        (951_868_799_u64, (2000, 2)), // doe == 146_096 (end of 400-yr civil era)
        (951_868_800_u64, (2000, 3)),
        (1_709_251_199_u64, (2024, 2)),
        (1_709_251_200_u64, (2024, 3)),
        (1_738_367_999_u64, (2025, 1)),
        (1_746_014_400_u64, (2025, 4)),
        (1_748_692_800_u64, (2025, 5)),
        (1_751_284_800_u64, (2025, 6)),
        (1_753_963_200_u64, (2025, 7)),
        (1_756_641_600_u64, (2025, 8)),
        (1_759_233_600_u64, (2025, 9)),
        (1_761_912_000_u64, (2025, 10)),
        (1_764_504_000_u64, (2025, 11)),
        (1_767_225_599_u64, (2025, 12)),
        (4_107_542_399_u64, (2100, 2)), // non-leap century year
        (4_107_542_400_u64, (2100, 3)),
        (13_574_606_400_u64, (2400, 2)),
        (13_574_649_600_u64, (2400, 3)),
    ];
    for (ts, expected) in cases {
        assert_eq!(utc_year_month(ts), expected, "utc_year_month({ts})");
    }
}

#[test]
fn ensure_goals_sde_rotation_len_day_bucket_zero_and_rehydrate_edges() {
    let make_tpl = |prefix: &[u8; 5], suffix: u8, category: u32| -> DailyGoalTemplate {
        let mut uuid = [0u8; 16];
        uuid[0..5].copy_from_slice(prefix);
        uuid[15] = suffix;
        DailyGoalTemplate {
            uuid,
            name_message: format!("G{suffix}"),
            description_message: String::new(),
            category,
            career: 1,
            target: 5,
            isk_reward: 100_000,
            sp_reward: 0.0,
            completion_cost_isk: None,
            completion_cost_evermarks: Some(2_500),
            name_message_id: 0,
            description_message_id: 0,
            help_text_message_id: 0,
            assigner_corporation_id: 0,
        }
    };

    // 1. is_sde_rotating_regular_template checks both prefix == b"SDEDG" and category != 6
    let sde_1 = make_tpl(b"SDEDG", 1, 1);
    let sde_2 = make_tpl(b"SDEDG", 2, 2);
    let sde_3 = make_tpl(b"SDEDG", 3, 3);
    let sde_4 = make_tpl(b"SDEDG", 4, 4);
    let sde_5 = make_tpl(b"SDEDG", 5, 5);
    let sde_6 = make_tpl(b"SDEDG", 7, 1);
    let sde_bonus = make_tpl(b"SDEDG", 6, 6);
    let custom_reg = make_tpl(b"CUSTM", 8, 1);
    let custom_cat6 = make_tpl(b"CUSTM", 9, 6);
    assert!(is_sde_rotating_regular_template(&sde_1));
    assert!(!is_sde_rotating_regular_template(&sde_bonus));
    assert!(!is_sde_rotating_regular_template(&custom_reg));
    assert!(!is_sde_rotating_regular_template(&custom_cat6));

    // 2. With 4 rotating SDE templates (+ bonus + custom_reg), NONE are omitted
    let tpls_4 = vec![
        sde_1.clone(),
        sde_2.clone(),
        sde_3.clone(),
        sde_4.clone(),
        sde_bonus.clone(),
        custom_reg.clone(),
    ];
    let day5_ts = 39_600 + 5 * 86_400 + 100; // day_bucket == 5
    let day6_ts = 39_600 + 6 * 86_400 + 100; // day_bucket == 6
    let mut goals_4 = Vec::new();
    assert!(ensure_character_daily_goals_with_templates(&mut goals_4, &tpls_4, day5_ts));
    assert!(!ensure_character_daily_goals_with_templates(&mut goals_4, &tpls_4, day5_ts));
    assert!(goals_4.iter().all(|g| !g.expired && g.day_bucket == 5));

    // 3. With 5 rotating SDE templates, day_bucket == 5 omits idx 0 (sde_1) and day_bucket == 6 omits idx 1 (sde_2);
    // calling again with now_unix == 0 preserves existing max(day_bucket) and marks newly omitted non-expired goal expired
    let tpls_5 = vec![
        sde_1.clone(),
        sde_2.clone(),
        sde_3.clone(),
        sde_4.clone(),
        sde_5.clone(),
        sde_bonus.clone(),
    ];
    let mut goals_5 = Vec::new();
    assert!(ensure_character_daily_goals_with_templates(&mut goals_5, &tpls_5, day5_ts));
    assert!(goals_5.iter().find(|g| g.uuid == sde_1.uuid).unwrap().expired);
    assert!(!goals_5.iter().find(|g| g.uuid == sde_2.uuid).unwrap().expired);
    // Flip expired manually to verify same-day `!is_active_today && !existing.expired` under now_unix == 0 (max day_bucket = 5)
    goals_5.iter_mut().find(|g| g.uuid == sde_1.uuid).unwrap().expired = false;
    assert!(ensure_character_daily_goals_with_templates(&mut goals_5, &tpls_5, 0));
    assert!(goals_5.iter().find(|g| g.uuid == sde_1.uuid).unwrap().expired);
    assert_eq!(goals_5.iter().find(|g| g.uuid == sde_2.uuid).unwrap().day_bucket, 5);
    assert!(!ensure_character_daily_goals_with_templates(&mut goals_5, &tpls_5, 0));

    // Rollover to day 6 omits sde_2 (idx = 6 % 5 = 1) and un-expires sde_1
    assert!(ensure_character_daily_goals_with_templates(&mut goals_5, &tpls_5, day6_ts));
    assert!(!goals_5.iter().find(|g| g.uuid == sde_1.uuid).unwrap().expired);
    assert!(goals_5.iter().find(|g| g.uuid == sde_2.uuid).unwrap().expired);

    // With 6 rotating SDE templates (> 4, != 5), day_bucket == 6 omits idx 6 % 6 == 0 (sde_1)
    let tpls_6 = vec![
        sde_1.clone(),
        sde_2.clone(),
        sde_3.clone(),
        sde_4.clone(),
        sde_5.clone(),
        sde_6.clone(),
    ];
    let mut goals_6 = Vec::new();
    assert!(ensure_character_daily_goals_with_templates(&mut goals_6, &tpls_6, day6_ts));
    assert!(goals_6.iter().find(|g| g.uuid == sde_1.uuid).unwrap().expired);
    assert_eq!(goals_6.iter().filter(|g| g.expired).count(), 1);

    // 4. Legacy `day_bucket == 0` migration with `day_bucket > 0`:
    // active goal with expired=true & zero/clean state un-expires; any individual activity flag preserves expired=true;
    // omitted goal (!is_active_today) is forced expired=true even if progressed & completed.
    let mut legacy_goals = vec![
        CharacterDailyGoal {
            uuid: sde_2.uuid, // active on day 5
            template: sde_2.clone(),
            current_progress: 0,
            completed: false,
            claimed: false,
            paid_completion: false,
            completed_at_unix: None,
            claimed_at_unix: None,
            day_bucket: 0,
            expired: true, // un-expires because untouched & active
        },
        CharacterDailyGoal {
            uuid: sde_3.uuid, // active on day 5
            template: sde_3.clone(),
            current_progress: 1,
            completed: false,
            claimed: false,
            paid_completion: false,
            completed_at_unix: None,
            claimed_at_unix: None,
            day_bucket: 0,
            expired: true, // keeps expired=true due to progress > 0
        },
        CharacterDailyGoal {
            uuid: sde_4.uuid, // active on day 5
            template: sde_4.clone(),
            current_progress: 0,
            completed: true,
            claimed: false,
            paid_completion: false,
            completed_at_unix: Some(10),
            claimed_at_unix: None,
            day_bucket: 0,
            expired: true, // keeps expired=true due to completed
        },
        CharacterDailyGoal {
            uuid: sde_5.uuid, // active on day 5
            template: sde_5.clone(),
            current_progress: 0,
            completed: false,
            claimed: true,
            paid_completion: false,
            completed_at_unix: None,
            claimed_at_unix: Some(10),
            day_bucket: 0,
            expired: true, // keeps expired=true due to claimed
        },
        CharacterDailyGoal {
            uuid: sde_bonus.uuid, // active on day 5
            template: sde_bonus.clone(),
            current_progress: 0,
            completed: false,
            claimed: false,
            paid_completion: true,
            completed_at_unix: None,
            claimed_at_unix: None,
            day_bucket: 0,
            expired: true, // keeps expired=true due to paid_completion
        },
        CharacterDailyGoal {
            uuid: sde_1.uuid, // omitted on day 5
            template: sde_1.clone(),
            current_progress: 4,
            completed: true,
            claimed: false,
            paid_completion: false,
            completed_at_unix: Some(10),
            claimed_at_unix: None,
            day_bucket: 0,
            expired: false, // forced expired=true because !is_active_today
        },
    ];
    assert!(ensure_character_daily_goals_with_templates(&mut legacy_goals, &tpls_5, day5_ts));
    assert!(!legacy_goals.iter().find(|g| g.uuid == sde_2.uuid).unwrap().expired);
    assert!(legacy_goals.iter().find(|g| g.uuid == sde_3.uuid).unwrap().expired);
    assert!(legacy_goals.iter().find(|g| g.uuid == sde_4.uuid).unwrap().expired);
    assert!(legacy_goals.iter().find(|g| g.uuid == sde_5.uuid).unwrap().expired);
    assert!(legacy_goals.iter().find(|g| g.uuid == sde_bonus.uuid).unwrap().expired);
    assert!(legacy_goals.iter().find(|g| g.uuid == sde_1.uuid).unwrap().expired);

    // 5. rehydrate_from_db store filter & deduplication edge cases
    let db = Arc::new(WorldRepository::with_demo_seed());
    let mgr = DailyGoalManager::new(Some(Arc::clone(&db)));
    let char_a = 71_001u32;
    let char_b = 71_002u32;

    // Empty DailyGoalStore clears in-memory state & does NOT add bonus template
    mgr.record_daily_bonus_completion(char_a, day5_ts);
    let empty_store = DailyGoalStore::default();
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert(
            "quasar_daily_goals".into(),
            serde_json::to_string(&empty_store).unwrap(),
        );
    });
    mgr.rehydrate_from_db();
    assert!(mgr.get_templates().is_empty());
    assert!(mgr.monthly_progress.read().unwrap().is_empty());

    // Store with templates=[] and non-empty character_goals still loads character_goals
    let cg_only_store = DailyGoalStore {
        templates: Vec::new(),
        character_goals: HashMap::from([(
            char_a,
            vec![CharacterDailyGoal {
                uuid: sde_2.uuid,
                template: sde_2.clone(),
                current_progress: 3,
                completed: false,
                claimed: false,
                paid_completion: false,
                completed_at_unix: None,
                claimed_at_unix: None,
                day_bucket: 5,
                expired: false,
            }],
        )]),
        monthly_progress: HashMap::from([(
            char_a,
            MonthlyGoalProgress {
                year_month: (2025, 1),
                steps_completed: 2,
                last_bonus_day_bucket: Some(5),
                claimed_milestones: Vec::new(),
            },
        )]),
    };
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert(
            "quasar_daily_goals".into(),
            serde_json::to_string(&cg_only_store).unwrap(),
        );
    });
    mgr.rehydrate_from_db();
    assert_eq!(mgr.character_goals.read().unwrap()[&char_a][0].current_progress, 3);
    assert_eq!(mgr.monthly_progress.read().unwrap()[&char_a].steps_completed, 2);

    // Store with non-empty templates including bonus (cat 2 effective) and empty character_goals loads without duplicating bonus
    let tpls_only_store = DailyGoalStore {
        templates: vec![sde_2.clone(), sde_bonus.clone()],
        character_goals: HashMap::new(),
        monthly_progress: HashMap::new(),
    };
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert(
            "quasar_daily_goals".into(),
            serde_json::to_string(&tpls_only_store).unwrap(),
        );
    });
    mgr.rehydrate_from_db();
    assert_eq!(mgr.get_templates().len(), 2);

    // Legacy HashMap across 2 characters sharing the same template UUID deduplicates to 1 regular + 1 injected bonus = 2
    let shared_goal = CharacterDailyGoal {
        uuid: sde_2.uuid,
        template: sde_2.clone(),
        current_progress: 1,
        completed: false,
        claimed: false,
        paid_completion: false,
        completed_at_unix: None,
        claimed_at_unix: None,
        day_bucket: 5,
        expired: false,
    };
    let legacy_two_chars = HashMap::from([
        (char_a, vec![shared_goal.clone()]),
        (char_b, vec![shared_goal]),
    ]);
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert(
            "quasar_daily_goals".into(),
            serde_json::to_string(&legacy_two_chars).unwrap(),
        );
    });
    mgr.rehydrate_from_db();
    assert_eq!(mgr.get_templates().len(), 2);
}

#[test]
fn monthly_alpha_claims_clock_fallback_add_progress_redeem_and_pay_corp_fallback_edges() {
    let (_router, db) = make_router_and_db();
    let jan_clock_ts = 1_735_732_800_u64; // 2025-01-01 12:00 UTC
    db.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(jan_clock_ts)));

    let mgr = DailyGoalManager::new_with_sde(
        Some(Arc::clone(&db)),
        Some(shared_sde()),
    );
    let alpha_cid = 211_200_050_u32;
    let pay_cid = 211_200_051_u32;
    let custom_corp = 1_000_125_i64;
    seed_char(&db, i64::from(alpha_cid), 1_000_009, 0, 0);
    seed_char(&db, i64::from(pay_cid), 1_000_009, 0, 10_000);
    db.with_mut_snapshot(|s| {
        s.characters
            .get_mut(&i64::from(pay_cid))
            .unwrap()
            .loyalty_points
            .insert(custom_corp, 3_000);
    });

    // 1. now_unix == 0 fallback uses clock (2025-01) while future now_unix (2099-01) isolates another month
    let future_ts = 4_070_908_800_u64; // 2099-01-01 00:00 UTC
    assert_eq!(mgr.get_monthly_progress(alpha_cid, future_ts).year_month, (2099, 1));
    assert_eq!(mgr.get_monthly_progress(alpha_cid, 0).year_month, (2025, 1));
    assert_eq!(mgr.record_daily_bonus_completion(alpha_cid, 0).steps_completed, 1);
    for d in 1..12_u64 {
        assert_eq!(
            mgr.record_daily_bonus_completion(alpha_cid, jan_clock_ts + d * 86_400)
                .steps_completed,
            (d + 1) as u32
        );
    }
    // Alpha claims 6 (+2,250 EM only), 9 (+25,000 SP only), 12 (+0 everything for Alpha); now_unix=0 uses clock
    mgr.claim_monthly_milestone(alpha_cid, 6, false, 0, &db).unwrap();
    mgr.claim_monthly_milestone(alpha_cid, 9, false, 0, &db).unwrap();
    mgr.claim_monthly_milestone(alpha_cid, 12, false, 0, &db).unwrap();
    db.with_snapshot(|s| {
        let ch = &s.characters[&i64::from(alpha_cid)];
        assert_eq!(ch.balance.0, 0);
        assert_eq!(ch.loyalty_points[&1_000_419], 2_250);
        assert_eq!(ch.free_skill_points, 26_000.0); // initial 1,000 + Alpha 25,000 (not +75k Omega)
    });
    // Rollover check when existing entry year_month (2025, 1) != future (2099, 1)
    assert_eq!(
        mgr.claim_monthly_milestone(alpha_cid, 3, false, future_ts, &db)
            .unwrap_err()
            .0,
        STATUS_PRECONDITION_FAILED
    );

    // 2. add_progress & redeem_goal edge paths
    let active = mgr.get_or_init_goals_at(alpha_cid, jan_clock_ts);
    let reg_uuids: Vec<[u8; 16]> = active
        .iter()
        .filter(|g| g.template.category != 6)
        .map(|g| g.uuid)
        .collect();
    let bonus_uuid = active
        .iter()
        .find(|g| g.template.category == 6)
        .unwrap()
        .uuid;
    assert!(mgr.add_progress(alpha_cid, &[0xAB; 16], 1, jan_clock_ts).is_none());

    // Mark an expired completed regular goal and verify it does NOT count toward bonus progress
    let omitted_uuid = mgr
        .character_goals
        .read()
        .unwrap()[&alpha_cid]
        .iter()
        .find(|g| g.expired)
        .unwrap()
        .uuid;
    {
        let mut map = mgr.character_goals.write().unwrap();
        let g = map.get_mut(&alpha_cid).unwrap().iter_mut().find(|g| g.uuid == omitted_uuid).unwrap();
        g.completed = true;
    }
    assert!(mgr.add_progress(alpha_cid, &omitted_uuid, 1, jan_clock_ts).is_none());

    let (g0_first, jc0) = mgr
        .add_progress(alpha_cid, &reg_uuids[0], active[0].template.target + 100, jan_clock_ts)
        .unwrap();
    assert!(jc0);
    assert!(g0_first.completed);
    // Repeat add_progress on already completed goal returns just_completed = false
    let (_g0_repeat, jc0_repeat) = mgr
        .add_progress(alpha_cid, &reg_uuids[0], 1, jan_clock_ts)
        .unwrap();
    assert!(!jc0_repeat);
    // Bonus should have progress == 1 (only reg_uuids[0], not omitted_uuid), not yet completed
    let bonus_mid = mgr
        .get_or_init_goals_at(alpha_cid, jan_clock_ts)
        .into_iter()
        .find(|g| g.uuid == bonus_uuid)
        .unwrap();
    assert_eq!(bonus_mid.current_progress, 1);
    assert!(!bonus_mid.completed);

    // Completing 2nd regular goal completes bonus & increments monthly track on next day
    let day_20_ts = jan_clock_ts + 20 * 86_400;
    let goals_d20 = mgr.get_or_init_goals_at(alpha_cid, day_20_ts);
    let d20_regs: Vec<[u8; 16]> = goals_d20
        .iter()
        .filter(|g| g.template.category != 6)
        .map(|g| g.uuid)
        .collect();
    let d20_bonus = goals_d20.iter().find(|g| g.template.category == 6).unwrap().uuid;
    assert_eq!(mgr.record_daily_bonus_completion(alpha_cid, day_20_ts).steps_completed, 1);
    let _ = mgr.add_progress(alpha_cid, &d20_regs[0], 100, day_20_ts).unwrap();
    let _ = mgr.add_progress(alpha_cid, &d20_regs[1], 100, day_20_ts).unwrap();
    // Completing 3rd regular goal does not re-complete bonus
    let _ = mgr.add_progress(alpha_cid, &d20_regs[2], 100, day_20_ts).unwrap();

    // redeem_goal: regular awards +500 Paragon EM while bonus (cat 6) awards +0 EM
    let em_before = db.with_snapshot(|s| s.characters[&i64::from(alpha_cid)].loyalty_points[&1_000_419]);
    mgr.redeem_goal(alpha_cid, &d20_regs[0], day_20_ts, &db).unwrap();
    let em_after_reg = db.with_snapshot(|s| s.characters[&i64::from(alpha_cid)].loyalty_points[&1_000_419]);
    assert_eq!(em_after_reg - em_before, 500);
    mgr.redeem_goal(alpha_cid, &d20_bonus, day_20_ts, &db).unwrap();
    let em_after_bonus = db.with_snapshot(|s| s.characters[&i64::from(alpha_cid)].loyalty_points[&1_000_419]);
    assert_eq!(em_after_bonus, em_after_reg);

    // 3. pay_for_completion: custom assigner_corp debit first, then Paragon fallback when assigner EM insufficient
    let mut cust_tpl_1 = mgr.get_templates()[0].clone();
    cust_tpl_1.uuid = [0xC1; 16];
    cust_tpl_1.category = 7;
    cust_tpl_1.assigner_corporation_id = custom_corp as u32;
    cust_tpl_1.completion_cost_evermarks = Some(2_500);
    let mut cust_tpl_2 = cust_tpl_1.clone();
    cust_tpl_2.uuid = [0xC2; 16];
    mgr.configure_template(cust_tpl_1.clone());
    mgr.configure_template(cust_tpl_2.clone());
    let _ = mgr.get_or_init_goals_at(pay_cid, jan_clock_ts);

    // 1st payment debits 2,500 from custom_corp (3,000 -> 500) without touching Paragon (10,000)
    mgr.pay_for_completion(pay_cid, &cust_tpl_1.uuid, jan_clock_ts, &db).unwrap();
    db.with_snapshot(|s| {
        let lp = &s.characters[&i64::from(pay_cid)].loyalty_points;
        assert_eq!(lp[&custom_corp], 500);
        assert_eq!(lp[&1_000_419], 10_000);
    });

    // 2nd payment costs 4,500 (> 500 in custom_corp) -> falls back to Paragon (10,000 -> 5,500) and completes daily bonus
    mgr.pay_for_completion(pay_cid, &cust_tpl_2.uuid, jan_clock_ts, &db).unwrap();
    db.with_snapshot(|s| {
        let lp = &s.characters[&i64::from(pay_cid)].loyalty_points;
        assert_eq!(lp[&custom_corp], 500);
        assert_eq!(lp[&1_000_419], 5_500);
    });
    assert_eq!(mgr.get_monthly_progress(pay_cid, jan_clock_ts).steps_completed, 1);

    // Insufficient EM boundary (2,499 < 2,500) and organic progress=1 -> 1st paid completion reaches bonus cap (2/2)
    let partial_cid = 211_200_052_u32;
    seed_char(&db, i64::from(partial_cid), 1_000_009, 0, 2_499);
    let partial_goals = mgr.get_or_init_goals_at(partial_cid, jan_clock_ts);
    let partial_regs: Vec<[u8; 16]> = partial_goals
        .iter()
        .filter(|g| g.template.category != 6)
        .map(|g| g.uuid)
        .collect();
    assert_eq!(
        mgr.pay_for_completion(partial_cid, &partial_regs[0], jan_clock_ts, &db)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    db.with_mut_snapshot(|s| {
        s.characters
            .get_mut(&i64::from(partial_cid))
            .unwrap()
            .loyalty_points
            .insert(1_000_419, 2_500);
    });
    let _ = mgr.add_progress(partial_cid, &partial_regs[0], 100, jan_clock_ts).unwrap();
    // 1 organic + 1 paid (2,500) -> completes bonus -> 2nd paid rejected even though paid_count == 1
    mgr.pay_for_completion(partial_cid, &partial_regs[1], jan_clock_ts, &db).unwrap();
    assert_eq!(
        db.with_snapshot(|s| s.characters[&i64::from(partial_cid)].loyalty_points[&1_000_419]),
        0
    );
    assert_eq!(
        mgr.pay_for_completion(partial_cid, &partial_regs[2], jan_clock_ts, &db)
            .unwrap_err()
            .0,
        STATUS_PRECONDITION_FAILED
    );
}

#[test]
fn daily_goal_pb_builders_decode_strictly_with_descriptors() {
    let reg = QuasarDescriptorRegistry::open_default().expect("open default descriptor DB");
    for cat in 1..=6 {
        let mut tpl = default_daily_bonus_template();
        tpl.uuid = [cat as u8; 16];
        tpl.category = cat;
        tpl.career = cat.min(4);
        tpl.isk_reward = 25_000;
        tpl.sp_reward = 1_500.0;

        let mut buf = Vec::new();
        encode_goal_attributes_for_day_bucket(&tpl, 20_548, &mut buf);
        let dec = reg
            .decode_dynamic("eve_public.dailygoal.Attributes", &buf)
            .unwrap_or_else(|e| panic!("decode dailygoal.Attributes cat {cat}: {e}"));
        assert_eq!(reg.encode_dynamic(&dec).unwrap(), buf);
    }

    let (router, db) = make_router_and_db();
    let char_id = 211_200_077u32;
    seed_char(&db, char_id as i64, 98_000_077, 1_000_000, 3_000);
    let now = 1_760_000_000;
    let user = make_ctx(Some(char_id), 0, now);

    let all_any = router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.GetAllCurrentRequest"),
                value: Vec::new(),
            },
        )
        .expect("eve_public.dailygoal.api.GetAllCurrentRequest");
    reg.decode_dynamic(&all_any.type_url, &all_any.value)
        .expect("decode GetAllCurrentResponse");

    let int_all_any = router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.GetAllRequest"),
                value: Vec::new(),
            },
        )
        .expect("eve.dailygoal.api.GetAllRequest");
    reg.decode_dynamic(&int_all_any.type_url, &int_all_any.value)
        .expect("decode eve.dailygoal.api.GetAllResponse");

    let goals = router.progression.daily_goals.get_or_init_goals_at(char_id, now);
    let first_uuid = goals[0].uuid;
    let get_req = eve_proto::pb::message("eve.dailygoal.api.GetRequest")
        .set("goal.uuid", first_uuid)
        .must_encode();
    let int_get_any = router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.GetRequest"),
                value: get_req.clone(),
            },
        )
        .expect("eve.dailygoal.api.GetRequest");
    reg.decode_dynamic(&int_get_any.type_url, &int_get_any.value)
        .expect("decode eve.dailygoal.api.GetResponse");

    let int_prog_any = router
        .dispatch(
            &user,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.GetProgressRequest"),
                value: get_req,
            },
        )
        .expect("eve.dailygoal.api.GetProgressRequest");
    reg.decode_dynamic(&int_prog_any.type_url, &int_prog_any.value)
        .expect("decode eve.dailygoal.api.GetProgressResponse");
}
