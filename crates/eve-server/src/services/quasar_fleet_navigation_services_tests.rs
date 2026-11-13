//! Unit and mutation-killing tests for `quasar_fleet_navigation_services` (`feat:quasar.dailygoal`, `feat:quasar.loyaltypoints`, `feat:pve.ess`).

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_NOT_IMPLEMENTED, STATUS_PRECONDITION_FAILED,
};
use eve_db::{IskCents, WorldRepository};
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
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
        let proto_name = norm.replace(".wallet.assetholding.", ".assetholding.");
        let dyn_msg = self
            .descriptors
            .decode_dynamic_or_wire(&proto_name, &req.value)
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
    (router, db)
}

fn seed_char(db: &WorldRepository, char_id: i64, corp_id: i64, isk: i64, lp: i64) {
    db.with_mut_snapshot(|s| {
        let mut rec = s.characters[&90_000_001].clone();
        rec.character_id = char_id;
        rec.corporation_id = corp_id;
        rec.balance = IskCents::from_isk(isk);
        rec.loyalty_points.clear();
        rec.loyalty_points.insert(1_000_419, lp);
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

#[test]
fn wire_helpers_and_ess_notice_emitters() {
    // parity: feat:pve.ess
    let isk_zero = build_isk_currency(pb::message("eve.isk.Currency"), IskCents(0)).must_encode();
    assert!(isk_zero.is_empty());
    let isk_nonzero =
        build_isk_currency(pb::message("eve.isk.Currency"), IskCents(12_345)).must_encode();
    assert!(!isk_nonzero.is_empty());

    let seq_zero = build_u32_id(pb::message("eve.character.Identifier"), 0).must_encode();
    assert!(seq_zero.is_empty());
    let seq_nonzero = build_u32_id(pb::message("eve.character.Identifier"), 42).must_encode();
    assert!(!seq_nonzero.is_empty());

    let reg1 = encode_period_regenerated(1, 1_000);
    let reg2 = encode_period_regenerated(2, 1_000);
    assert!(!reg1.is_empty());
    assert!(!reg2.is_empty());
    assert_ne!(reg1, reg2);

    let (router, _db) = make_router_and_db();
    let goals = router
        .progression
        .daily_goals
        .get_or_init_goals_at(100, 100_000);
    let comp_paid = encode_daily_goal_completed(100, &goals[0], true);
    let comp_unpaid = encode_daily_goal_completed(100, &goals[0], false);
    assert!(comp_paid.len() > comp_unpaid.len());

    let red_auto = encode_daily_goal_redeemed(100, &goals[0], true);
    let red_man = encode_daily_goal_redeemed(100, &goals[0], false);
    assert!(red_auto.len() > red_man.len());

    let sys_id = 30000142u32;
    let take_last_notice = |r: &PublicGatewayRouter| {
        r.routing_cache.read().unwrap().entries.back().unwrap().notice.payload.clone().unwrap()
    };
    emit_ess_main_bank_primed(&router, sys_id, IskCents::from_isk(1_000_000), 100_000);
    let n1 = take_last_notice(&router);
    assert!(n1.type_url.ends_with("MainBankPrimed"));

    emit_ess_reserve_bank_primed(&router, sys_id, IskCents::from_isk(2_000_000), 100_001);
    let n2 = take_last_notice(&router);
    assert!(n2.type_url.ends_with("ReserveBankPrimed"));

    emit_ess_main_bank_link_established(
        &router,
        211200001,
        sys_id,
        0,
        IskCents::from_isk(1_000_000),
        IskCents::from_isk(2_000_000),
        100_002,
    );
    let n3_no_ship = take_last_notice(&router);
    emit_ess_main_bank_link_established(
        &router,
        211200001,
        sys_id,
        1000000001234,
        IskCents::from_isk(1_000_000),
        IskCents::from_isk(2_000_000),
        100_003,
    );
    let n3_ship = take_last_notice(&router);
    assert!(n3_ship.value.len() > n3_no_ship.value.len());

    emit_ess_main_bank_theft_completed(
        &router,
        211200001,
        sys_id,
        IskCents::from_isk(2_000_000),
        IskCents::from_isk(900_000),
        IskCents::from_isk(100_000),
        100_004,
    );
    let n4 = take_last_notice(&router);
    assert!(n4.type_url.ends_with("MainBankTheftCompleted"));

    emit_ess_solar_system_persisted(
        &router,
        sys_id,
        IskCents::from_isk(500_000),
        IskCents::from_isk(1_500_000),
        100_005,
    );
    let n5 = take_last_notice(&router);
    assert!(n5.type_url.ends_with("SolarSystemPersisted"));
}

#[test]
fn admin_daily_goal_lifecycle_handlers() {
    // parity: feat:pve.daily-goals
    let (router, db) = make_router_and_db();
    let cid = 211200050u32;
    seed_char(&db, cid as i64, 98000050, 5_000_000, 1_000);

    let user = make_ctx(Some(cid), 0, 200_000);
    let admin_no_char = make_ctx(None, ROLE_PROGRAMMER, 200_000);
    let admin = make_ctx(Some(cid), ROLE_PROGRAMMER, 200_000);

    // 1. RegenerateCurrentGoalsRequest
    let regen_req = GoogleAny {
        type_url: format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.RegenerateCurrentGoalsRequest"
        ),
        value: vec![],
    };
    assert_eq!(router.dispatch(&user, &regen_req).unwrap_err().0, STATUS_FORBIDDEN);
    assert_eq!(
        router.dispatch(&admin_no_char, &regen_req).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    let regen_resp = router.dispatch(&admin, &regen_req).expect("RegenerateCurrentGoals");
    assert!(regen_resp.type_url.ends_with("RegenerateCurrentGoalsResponse"));
    assert!(!regen_resp.value.is_empty());

    // 2. RegenerateCurrentMonthlyGoalsRequest -> STATUS_NOT_IMPLEMENTED
    let monthly_req = GoogleAny {
        type_url: format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.RegenerateCurrentMonthlyGoalsRequest"
        ),
        value: vec![],
    };
    assert_eq!(router.dispatch(&user, &monthly_req).unwrap_err().0, STATUS_FORBIDDEN);
    assert_eq!(
        router.dispatch(&admin, &monthly_req).unwrap_err().0,
        STATUS_NOT_IMPLEMENTED
    );

    let goals = router
        .progression
        .daily_goals
        .get_or_init_goals_at(cid, 200_000);
    let g1_uuid = goals[0].uuid; // target 25
    let g2_uuid = goals[1].uuid; // target 2000

    // 3. eve.dailygoal.api.admin.ProgressRequest
    let empty_prog = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.ProgressRequest"),
        value: vec![],
    };
    assert_eq!(router.dispatch(&user, &empty_prog).unwrap_err().0, STATUS_FORBIDDEN);
    assert_eq!(router.dispatch(&admin, &empty_prog).unwrap_err().0, STATUS_BAD_REQUEST);

    let no_delta = pb::message("eve.dailygoal.api.admin.ProgressRequest")
        .set("goal.uuid", g1_uuid.as_slice())
        .must_encode();
    let no_delta_req = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.ProgressRequest"),
        value: no_delta,
    };
    assert_eq!(
        router.dispatch(&admin_no_char, &no_delta_req).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(router.dispatch(&admin, &no_delta_req).unwrap_err().0, STATUS_BAD_REQUEST);

    // Valid progress completing g1
    let valid_prog = pb::message("eve.dailygoal.api.admin.ProgressRequest")
        .set("goal.uuid", g1_uuid.as_slice())
        .set("progress", 30u64)
        .must_encode();
    let prog_resp = router
        .dispatch(
            &admin,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.ProgressRequest"),
                value: valid_prog,
            },
        )
        .expect("admin ProgressRequest");
    assert!(prog_resp.type_url.ends_with("ProgressResponse"));

    // 4. eve.dailygoal.api.admin.ExpireRequest on g2
    let exp_bytes = pb::message("eve.dailygoal.api.admin.ExpireRequest")
        .set("goal.uuid", g2_uuid.as_slice())
        .must_encode();
    let exp_req = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.ExpireRequest"),
        value: exp_bytes,
    };
    assert_eq!(router.dispatch(&user, &exp_req).unwrap_err().0, STATUS_FORBIDDEN);
    let exp_resp = router.dispatch(&admin, &exp_req).expect("ExpireRequest");
    assert!(exp_resp.type_url.ends_with("ExpireResponse"));
    // Expiring again -> NOT_FOUND
    assert_eq!(router.dispatch(&admin, &exp_req).unwrap_err().0, STATUS_NOT_FOUND);

    // 5. eve.dailygoal.api.admin.AutoRedeemRequest (redeems g1)
    let auto_req = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.AutoRedeemRequest"),
        value: vec![],
    };
    assert_eq!(router.dispatch(&user, &auto_req).unwrap_err().0, STATUS_FORBIDDEN);
    let auto_resp = router.dispatch(&admin, &auto_req).expect("AutoRedeemRequest");
    assert!(auto_resp.type_url.ends_with("AutoRedeemResponse"));
    let after_goals = router
        .progression
        .daily_goals
        .get_or_init_goals_at(cid, 200_000);
    let g1_after = after_goals.iter().find(|g| g.uuid == g1_uuid).unwrap();
    assert!(g1_after.claimed);
}

fn build_lp_op_bytes(
    op_uuid: &[u8; 16],
    char_id: Option<u32>,
    corp_id: Option<u32>,
    issuer_corp_id: u32,
    amount: u64,
    context: &[u8],
) -> Vec<u8> {
    pb::message("eve.loyaltypoints.assetholding.operation.api.DepositRequest")
        .set("identifier.uuid", op_uuid.as_slice())
        .set_opt("operation.wallet.character.sequential", char_id)
        .set_opt("operation.wallet.corporation.sequential", corp_id)
        .when(issuer_corp_id > 0, |b| b.set("operation.wallet.issuer.sequential", issuer_corp_id))
        .when(amount > 0, |b| b.set("operation.amount.amount", amount))
        .when(!context.is_empty(), |b| b.set("context.action", 1i32))
        .must_encode()
}

#[test]
fn lp_assetholding_deposit_withdraw_and_idempotency() {
    // parity: feat:quasar.loyaltypoints
    let (router, db) = make_router_and_db();
    let cid = 211200060u32;
    let corp_id = 98000060u32;
    let paragon = crate::services::lp_store::PARAGON_CORP_ID as u32;
    seed_char(&db, cid as i64, corp_id as i64, 1_000_000, 1_000);

    let user = make_ctx(Some(cid), 0, 300_000);
    let admin = make_ctx(Some(cid), ROLE_PROGRAMMER, 300_000);

    let dep_type = format!(
        "{TYPE_GOOGLEAPIS_PREFIX}eve.loyaltypoints.assetholding.operation.api.DepositRequest"
    );
    let wd_type = format!(
        "{TYPE_GOOGLEAPIS_PREFIX}eve.loyaltypoints.assetholding.operation.api.WithdrawRequest"
    );

    // Non-admin -> FORBIDDEN
    assert_eq!(
        router
            .dispatch(&user, &GoogleAny { type_url: dep_type.clone(), value: vec![] })
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    // Validation errors on DepositRequest
    assert_eq!(
        router
            .dispatch(&admin, &GoogleAny { type_url: dep_type.clone(), value: vec![] })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    // Missing issuer corp -> BAD_REQUEST
    let no_issuer = build_lp_op_bytes(&[1u8; 16], Some(cid), None, 0, 500, b"ctx");
    assert_eq!(
        router
            .dispatch(&admin, &GoogleAny { type_url: dep_type.clone(), value: no_issuer })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    // Amount 0 -> BAD_REQUEST
    let zero_amt = build_lp_op_bytes(&[1u8; 16], Some(cid), None, paragon, 0, b"ctx");
    assert_eq!(
        router
            .dispatch(&admin, &GoogleAny { type_url: dep_type.clone(), value: zero_amt })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // Character deposit of 500 Paragon EverMarks -> credits char AND duplicates to player corp (>= 98_000_000)
    let dep_char = build_lp_op_bytes(&[0x11; 16], Some(cid), None, paragon, 500, b"char_dep");
    let resp = router
        .dispatch(&admin, &GoogleAny { type_url: dep_type.clone(), value: dep_char.clone() })
        .expect("Deposit char");
    assert!(resp.type_url.ends_with("DepositResponse"));

    // Idempotent duplicate of same op_uuid does not double-credit
    router
        .dispatch(&admin, &GoogleAny { type_url: dep_type.clone(), value: dep_char })
        .expect("Deposit char idempotent");

    let (char_lp, corp_lp) = db.with_snapshot(|snap| {
        let ch = snap.characters.get(&(cid as i64)).unwrap();
        let clp = ch.loyalty_points.get(&(paragon as i64)).copied().unwrap_or(0);
        let co = snap
            .corp_lp_balances
            .get(&(corp_id as i64, paragon as i64))
            .copied()
            .unwrap_or(0);
        (clp, co)
    });
    assert_eq!(char_lp, 1_500);
    assert_eq!(corp_lp, 500);

    // Corporation deposit of 700 LP
    let dep_corp = build_lp_op_bytes(&[0x22; 16], None, Some(corp_id), paragon, 700, b"corp_dep");
    router
        .dispatch(&admin, &GoogleAny { type_url: dep_type, value: dep_corp })
        .expect("Deposit corp");

    // Withdraw from character: 600 LP
    let wd_char = build_lp_op_bytes(&[0x33; 16], Some(cid), None, paragon, 600, b"char_wd");
    let wd_resp = router
        .dispatch(&admin, &GoogleAny { type_url: wd_type.clone(), value: wd_char.clone() })
        .expect("Withdraw char");
    assert!(wd_resp.type_url.ends_with("WithdrawResponse"));
    // Idempotent duplicate withdraw
    router
        .dispatch(&admin, &GoogleAny { type_url: wd_type.clone(), value: wd_char })
        .expect("Withdraw char idempotent");

    // Withdraw exceeding character balance -> PRECONDITION_FAILED
    let wd_char_over = build_lp_op_bytes(&[0x34; 16], Some(cid), None, paragon, 9_999, b"over");
    assert_eq!(
        router
            .dispatch(&admin, &GoogleAny { type_url: wd_type.clone(), value: wd_char_over })
            .unwrap_err()
            .0,
        STATUS_PRECONDITION_FAILED
    );

    // Withdraw from corporation: 400 LP (out of 1200)
    let wd_corp = build_lp_op_bytes(&[0x44; 16], None, Some(corp_id), paragon, 400, b"corp_wd");
    router
        .dispatch(&admin, &GoogleAny { type_url: wd_type.clone(), value: wd_corp })
        .expect("Withdraw corp");

    // Withdraw exceeding corporation balance -> PRECONDITION_FAILED
    let wd_corp_over =
        build_lp_op_bytes(&[0x45; 16], None, Some(corp_id), paragon, 9_999, b"over_corp");
    assert_eq!(
        router
            .dispatch(&admin, &GoogleAny { type_url: wd_type, value: wd_corp_over })
            .unwrap_err()
            .0,
        STATUS_PRECONDITION_FAILED
    );

    let (char_lp_end, corp_lp_end) = db.with_snapshot(|snap| {
        let ch = snap.characters.get(&(cid as i64)).unwrap();
        let clp = ch.loyalty_points.get(&(paragon as i64)).copied().unwrap_or(0);
        let co = snap
            .corp_lp_balances
            .get(&(corp_id as i64, paragon as i64))
            .copied()
            .unwrap_or(0);
        (clp, co)
    });
    assert_eq!(char_lp_end, 900);
    assert_eq!(corp_lp_end, 800);
}

#[test]
fn apply_bounty_multiplier_factor_handler() {
    // parity: feat:pve.ess
    let (router, db) = make_router_and_db();
    let user = make_ctx(Some(211200070), 0, 400_000);
    let admin = make_ctx(Some(211200070), ROLE_PROGRAMMER, 400_000);
    let rpc = format!(
        "{TYPE_GOOGLEAPIS_PREFIX}eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest"
    );

    assert_eq!(
        router
            .dispatch(&user, &GoogleAny { type_url: rpc.clone(), value: vec![] })
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        router
            .dispatch(&admin, &GoogleAny { type_url: rpc.clone(), value: vec![] })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // Valid percentage 155 -> 1.55x
    let sys_id = 30000142u32;
    let req1 = pb::message("eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest")
        .set("solar_system.sequential", u64::from(sys_id))
        .set("input_multiplier_percentage_value", 155u32)
        .must_encode();
    let resp1 = router
        .dispatch(&admin, &GoogleAny { type_url: rpc.clone(), value: req1 })
        .expect("ApplyMultiplierFactor pct 155");
    assert!(resp1.type_url.ends_with("ApplyMultiplierFactorResponse"));
    let mult1 = db.with_snapshot(|snap| snap.system_states.get(&sys_id).unwrap().dbs_multiplier);
    assert!((mult1 - 1.55).abs() < 1e-6);

    // Explicit detail message (units=2, nanos=500_000_000 -> 2.5x)
    let req2 = pb::message("eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest")
        .set("solar_system.sequential", u64::from(sys_id))
        .set("calculated_multiplier.units", 2u64)
        .set("calculated_multiplier.nanos", 500_000_000u32)
        .must_encode();
    router
        .dispatch(&admin, &GoogleAny { type_url: rpc.clone(), value: req2 })
        .expect("ApplyMultiplierFactor explicit 2.5");
    let mult2 = db.with_snapshot(|snap| snap.system_states.get(&sys_id).unwrap().dbs_multiplier);
    assert!((mult2 - 2.5).abs() < 1e-6);

    // Out-of-range multiplier (units=11 > 10.0) -> BAD_REQUEST
    let req_bad = pb::message("eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest")
        .set("solar_system.sequential", u64::from(sys_id))
        .set("calculated_multiplier.units", 11u64)
        .must_encode();
    assert_eq!(
        router
            .dispatch(&admin, &GoogleAny { type_url: rpc, value: req_bad })
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    let _ = DynamicQuasarMessage::new("test.Msg");
    let _ = DynamicValue::Bool(true);
}

#[test]
fn fleet_navigation_builders_decode_strictly_with_descriptors() {
    let reg = QuasarDescriptorRegistry::open_default().expect("descriptor registry");
    let roundtrip = |type_name: &str, wire: &[u8]| {
        let decoded = reg
            .decode_dynamic(type_name, wire)
            .unwrap_or_else(|e| panic!("decode {type_name}: {e}"));
        let reencoded = reg
            .encode_dynamic(&decoded)
            .unwrap_or_else(|e| panic!("encode {type_name}: {e}"));
        let decoded2 = reg
            .decode_dynamic(type_name, &reencoded)
            .unwrap_or_else(|e| panic!("redecode {type_name}: {e}"));
        assert_eq!(decoded, decoded2, "roundtrip mismatch for {type_name}");
    };

    let (router, db) = make_router_and_db();
    let cid = 211_200_777_u32;
    let corp_id = 98_000_777_u32;
    let sys_id = 30_000_142_u32;
    let paragon = crate::services::lp_store::PARAGON_CORP_ID as u32;
    seed_char(&db, cid as i64, corp_id as i64, 5_000_000, 1_000);
    let admin = make_ctx(Some(cid), ROLE_PROGRAMMER, 200_000);

    // 1. Standalone DailyGoal event helpers
    let goals = router.progression.daily_goals.get_or_init_goals_at(cid, 200_000);
    roundtrip("eve.dailygoal.api.Created", &encode_daily_goal_created(&goals[0]));
    roundtrip(
        "eve.dailygoal.api.PrematurelyDeactivated",
        &encode_daily_goal_prematurely_deactivated(&goals[0].uuid, 200_000),
    );
    roundtrip("eve.dailygoal.api.Removed", &encode_daily_goal_removed(&goals[0].uuid));
    roundtrip(
        "eve.dailygoal.api.Progressed",
        &encode_daily_goal_progressed(cid, &goals[0].uuid, 0, 5, 200_000),
    );
    roundtrip("eve.dailygoal.api.Completed", &encode_daily_goal_completed(cid, &goals[0], true));
    roundtrip("eve.dailygoal.api.Redeemed", &encode_daily_goal_redeemed(cid, &goals[0], false));
    roundtrip("eve.dailygoal.api.PeriodRegenerated", &encode_period_regenerated(1, 200_000));

    // 2. ESS notice emitters
    let mut sess = eve_machonet::SessionState::new(1);
    sess.character_id = Some(cid.into());
    sess.solar_system_id = Some(sys_id);
    let mut rx = router.subscribe_session(&sess, Vec::new(), 200_000);

    emit_ess_main_bank_primed(&router, sys_id, IskCents::from_isk(1_000_000), 200_000);
    emit_ess_reserve_bank_primed(&router, sys_id, IskCents::from_isk(2_000_000), 200_001);
    emit_ess_main_bank_link_established(
        &router,
        cid,
        sys_id,
        0,
        IskCents::from_isk(1_000_000),
        IskCents::from_isk(2_000_000),
        200_002,
    );
    emit_ess_main_bank_link_established(
        &router,
        cid,
        sys_id,
        1_000_000_001_234,
        IskCents::from_isk(1_000_000),
        IskCents::from_isk(2_000_000),
        200_003,
    );
    emit_ess_main_bank_theft_completed(
        &router,
        cid,
        sys_id,
        IskCents::from_isk(2_000_000),
        IskCents::from_isk(900_000),
        IskCents::from_isk(100_000),
        200_004,
    );
    emit_ess_solar_system_persisted(
        &router,
        sys_id,
        IskCents::from_isk(500_000),
        IskCents::from_isk(1_500_000),
        200_005,
    );
    let mut ess_seen = 0usize;
    while let Ok(notice) = rx.try_recv() {
        let payload = notice.payload.as_ref().expect("notice payload");
        let short_type = payload
            .type_url
            .rsplit('/')
            .next()
            .unwrap_or(&payload.type_url);
        roundtrip(short_type, &payload.value);
        ess_seen += 1;
    }
    assert_eq!(ess_seen, 6);

    // 3. Registered RPC handlers + queued notices
    let _ = crate::services::quasar_social_chat::drain_local_notices();

    let regen_resp = router
        .dispatch(
            &admin,
            &GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.RegenerateCurrentGoalsRequest"
                ),
                value: Vec::new(),
            },
        )
        .unwrap();
    roundtrip("eve.dailygoal.api.admin.RegenerateCurrentGoalsResponse", &regen_resp.value);

    let after = router.progression.daily_goals.get_or_init_goals_at(cid, 200_000);
    let prog_req = pb::message("eve.dailygoal.api.admin.ProgressRequest")
        .set("goal.uuid", after[0].uuid.as_slice())
        .set("progress", after[0].template.target)
        .must_encode();
    let prog_resp = router
        .dispatch(
            &admin,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.ProgressRequest"),
                value: prog_req,
            },
        )
        .unwrap();
    roundtrip("eve.dailygoal.api.admin.ProgressResponse", &prog_resp.value);

    let auto_resp = router
        .dispatch(
            &admin,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.AutoRedeemRequest"),
                value: Vec::new(),
            },
        )
        .unwrap();
    roundtrip("eve.dailygoal.api.admin.AutoRedeemResponse", &auto_resp.value);

    let exp_req = pb::message("eve.dailygoal.api.admin.ExpireRequest")
        .set("goal.uuid", after[1].uuid.as_slice())
        .must_encode();
    let exp_resp = router
        .dispatch(
            &admin,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.ExpireRequest"),
                value: exp_req,
            },
        )
        .unwrap();
    roundtrip("eve.dailygoal.api.admin.ExpireResponse", &exp_resp.value);

    let dep_resp = router
        .dispatch(
            &admin,
            &GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.loyaltypoints.assetholding.operation.api.DepositRequest"
                ),
                value: build_lp_op_bytes(&[0x55; 16], Some(cid), None, paragon, 500, b"ctx"),
            },
        )
        .unwrap();
    roundtrip("eve.loyaltypoints.assetholding.operation.api.DepositResponse", &dep_resp.value);

    let wd_resp = router
        .dispatch(
            &admin,
            &GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.loyaltypoints.assetholding.operation.api.WithdrawRequest"
                ),
                value: build_lp_op_bytes(&[0x66; 16], Some(cid), None, paragon, 200, b"ctx"),
            },
        )
        .unwrap();
    roundtrip("eve.loyaltypoints.assetholding.operation.api.WithdrawResponse", &wd_resp.value);

    let bounty_req = pb::message("eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest")
        .set("solar_system.sequential", u64::from(sys_id))
        .set("input_multiplier_percentage_value", 150u32)
        .must_encode();
    let bounty_resp = router
        .dispatch(
            &admin,
            &GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest"
                ),
                value: bounty_req,
            },
        )
        .unwrap();
    roundtrip("eve.solarsystem.bounty.npc.ApplyMultiplierFactorResponse", &bounty_resp.value);

    let notices = crate::services::quasar_social_chat::drain_local_notices();
    assert!(notices.len() >= 12);
    for (_, any_msg, _) in &notices {
        let short_type = any_msg
            .type_url
            .rsplit('/')
            .next()
            .unwrap_or(&any_msg.type_url);
        roundtrip(short_type, &any_msg.value);
    }
}

#[cfg(test)]
#[path = "quasar_fleet_navigation_services_extra_unit_tests.rs"]
mod quasar_fleet_navigation_services_extra_unit_tests;
