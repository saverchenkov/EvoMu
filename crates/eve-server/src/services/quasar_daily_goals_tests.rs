//! Unit and mutation-killing tests for `quasar_daily_goals` (`feat:pve.daily-goals`).

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_PRECONDITION_FAILED,
};
use eve_db::{IskCents, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, AuthoritativeContext, DynamicValue, GoogleAny,
    QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
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
    let mut id_msg = Vec::new();
    encode_bytes_field(1, uuid, &mut id_msg);
    let mut req = Vec::new();
    encode_bytes_field(1, &id_msg, &mut req);
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value: req,
    }
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
    assert_eq!(tpl.effective_name_message_id(), 659_195);
    assert_eq!(tpl.effective_description_message_id(), 696_260);
    assert_eq!(tpl.effective_help_text_message_id(), 665_456);
    assert_eq!(tpl.effective_assigner_corporation_id(), 1_000_419);

    tpl.name_message_id = 0;
    tpl.description_message_id = 0;
    tpl.help_text_message_id = 0;
    tpl.assigner_corporation_id = 0;

    let expected = [
        (1, 1, 665_455, 696_260, 665_456),
        (2, 1, 665_390, 696_245, 665_395),
        (3, 1, 665_435, 696_251, 665_401),
        (4, 1, 665_465, 696_262, 665_466),
        (5, 1, 665_394, 696_246, 665_412),
        (6, 2, 582_520, 582_521, 582_521),
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
    assert_eq!(templates[1].completion_cost_isk, Some(100_000));
    assert_eq!(templates[1].completion_cost_evermarks, Some(2_500));
    assert_eq!(templates[5].category, 6);
    assert_eq!(templates[5].effective_goal_category(), 2);

    let db = Arc::new(WorldRepository::with_demo_seed());
    let mgr = DailyGoalManager::new_with_sde(Some(Arc::clone(&db)), Some(sde));
    assert_eq!(mgr.get_templates().len(), 6);

    let goals_init = mgr.get_or_init_goals_at(5001, 200_000);
    assert_eq!(goals_init.len(), 6);
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
    assert_eq!(reloaded_goals.len(), 7);

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
        encode_goal_attributes(&t, &mut encoded);
        assert!(!encoded.is_empty());
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
    let goals = mgr.get_or_init_goals_at(char_id, 500_000);
    assert_eq!(goals.len(), 6);
    let g1_uuid = goals[0].uuid; // KillNPC (target 25, cost EM=2500, ISK=None)
    let g2_uuid = goals[1].uuid; // MineOre (target 2000, cost EM=2500, ISK=100_000)
    let bonus_uuid = goals[5].uuid; // DailyBonus (target 2, cost EM=None, ISK=None)

    // Redeem before completion -> FORBIDDEN
    let err = mgr.redeem_goal(char_id, &g1_uuid, 500_000, &db).unwrap_err();
    assert_eq!(err.0, STATUS_FORBIDDEN);

    // Redeem unknown char -> NOT_FOUND
    let err = mgr.redeem_goal(999999, &g1_uuid, 500_000, &db).unwrap_err();
    assert_eq!(err.0, STATUS_NOT_FOUND);

    // Redeem unknown goal -> NOT_FOUND
    let err = mgr.redeem_goal(char_id, &[0xFF; 16], 500_000, &db).unwrap_err();
    assert_eq!(err.0, STATUS_NOT_FOUND);

    // Pay for completion on bonus goal (no completion cost configured) -> PRECONDITION_FAILED
    let err = mgr.pay_for_completion(char_id, &bonus_uuid, 500_000, &db).unwrap_err();
    assert_eq!(err.0, STATUS_PRECONDITION_FAILED);

    // Pay for completion on unknown goal -> NOT_FOUND
    let err = mgr.pay_for_completion(char_id, &[0xEE; 16], 500_000, &db).unwrap_err();
    assert_eq!(err.0, STATUS_NOT_FOUND);

    // Pay for completion on g1 using EverMarks (char has 3000 EM, cost is 2500 EM)
    let paid1 = mgr.pay_for_completion(char_id, &g1_uuid, 500_010, &db).expect("pay g1 with EM");
    assert!(paid1.completed);
    assert!(paid1.paid_completion);
    assert_eq!(paid1.current_progress, 25);

    // Paying again on already-completed g1 -> BAD_REQUEST
    let err = mgr.pay_for_completion(char_id, &g1_uuid, 500_011, &db).unwrap_err();
    assert_eq!(err.0, STATUS_BAD_REQUEST);

    // Now char has 500 EM left (< 2500), so paying for g2 falls back to ISK (cost 100,000 ISK)
    let paid2 = mgr.pay_for_completion(char_id, &g2_uuid, 500_020, &db).expect("pay g2 with ISK");
    assert!(paid2.completed);
    assert!(paid2.paid_completion);

    // Completing 2 regular goals via pay_for_completion also completes the bonus goal (target 2)!
    let current = mgr.get_or_init_goals_at(char_id, 500_020);
    let bonus = current.iter().find(|g| g.uuid == bonus_uuid).unwrap();
    assert_eq!(bonus.current_progress, 2);
    assert!(bonus.completed);

    // Insufficient funds + insufficient EM on g3 (cost EM=2500, ISK=None) -> BAD_REQUEST
    let g3_uuid = goals[2].uuid;
    let err = mgr.pay_for_completion(char_id, &g3_uuid, 500_025, &db).unwrap_err();
    assert_eq!(err.0, STATUS_BAD_REQUEST);

    // Redeem g1 -> credits 500,000 ISK, 5,000 SP, 500 EverMarks to char + 500 EverMarks to corp
    let redeemed1 = mgr.redeem_goal(char_id, &g1_uuid, 500_030, &db).expect("redeem g1");
    assert!(redeemed1.claimed);
    assert_eq!(redeemed1.claimed_at_unix, Some(500_030));

    // Double redeem -> FORBIDDEN
    let err = mgr.redeem_goal(char_id, &g1_uuid, 500_031, &db).unwrap_err();
    assert_eq!(err.0, STATUS_FORBIDDEN);

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
    // Started 5,000,000 - 100,000 (g2 paid) + 500,000 (g1 reward) = 5,400,000 ISK
    assert_eq!(bal, IskCents::from_isk(5_400_000));
    assert_eq!(sp, 6_000.0);
    // Started 3,000 - 2,500 (g1 paid) + 500 (g1 reward) = 1,000 EM
    assert_eq!(char_em, 1_000);
    assert_eq!(corp_em, 500);
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
    assert_eq!(get_all_curr.value, get_all_internal.value);

    let goals = router.progression.daily_goals.get_or_init_goals_at(char_id, 600_000);
    let g1_uuid = goals[0].uuid; // KillNPC (cat 1, target 25)
    let g4_uuid = goals[3].uuid; // SpaceJump (cat 4, target 10)
    let g5_uuid = goals[4].uuid; // ManufactureItem (cat 5, target 1)

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
    let mut admin_prog_bytes = Vec::new();
    let mut id_msg = Vec::new();
    encode_bytes_field(1, &g1_uuid, &mut id_msg);
    encode_bytes_field(1, &id_msg, &mut admin_prog_bytes);
    let mut char_msg = Vec::new();
    encode_varint_field(1, char_id as u64, &mut char_msg);
    encode_bytes_field(2, &char_msg, &mut admin_prog_bytes);
    encode_varint_field(3, 25, &mut admin_prog_bytes);
    let admin_prog_req = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.admin.ProgressRequest"),
        value: admin_prog_bytes,
    };
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
