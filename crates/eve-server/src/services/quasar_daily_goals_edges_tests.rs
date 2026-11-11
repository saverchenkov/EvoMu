//! Edge-case behavioral tests for `quasar_daily_goals`.

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_PRECONDITION_FAILED,
};
use eve_db::{IskCents, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, AuthoritativeContext, GoogleAny,
    QuasarDescriptorRegistry, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

const ROLE_PROGRAMMER: u64 = eve_machonet::service_roles::ROLE_GM_MASK;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn dispatch(
    router: &PublicGatewayRouter,
    ctx: &AuthoritativeContext,
    req: &GoogleAny,
) -> Result<GoogleAny, (u32, String)> {
    let norm = req.normalized_type_name();
    let handler = router
        .handlers
        .read()
        .unwrap()
        .get(norm)
        .cloned()
        .expect("registered handler");
    let dyn_msg = router
        .descriptors
        .decode_dynamic_or_wire(norm, &req.value)
        .unwrap_or_else(|_| {
            crate::services::quasar_char_skill_services::decode_wire_dynamic(norm, &req.value)
        });
    handler(ctx, req, &dyn_msg)
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
        rec.free_skill_points = 500.0;
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
fn redeem_goal_rollback_on_missing_db_char_and_precondition_checks() {
    let (router, db) = make_router_and_db();
    let mgr = &router.progression.daily_goals;
    let missing_cid = 211_999_001u32;
    let now = mgr.now_unix();

    // Missing char never initialized -> STATUS_NOT_FOUND
    assert_eq!(mgr.redeem_goal(missing_cid, &[0xAA; 16], now, &db).unwrap_err().0, STATUS_NOT_FOUND);

    let goals = mgr.get_or_init_goals_at(missing_cid, now);
    let first_uuid = goals[0].uuid;
    let target = goals[0].template.target;

    // Not completed -> STATUS_FORBIDDEN
    assert_eq!(mgr.redeem_goal(missing_cid, &first_uuid, now, &db).unwrap_err().0, STATUS_FORBIDDEN);

    // Unknown UUID -> STATUS_NOT_FOUND
    assert_eq!(mgr.redeem_goal(missing_cid, &[0xFE; 16], now, &db).unwrap_err().0, STATUS_NOT_FOUND);

    // Complete goal while char is missing in DB -> DB snapshot failure triggers rollback of `claimed`
    mgr.add_progress(missing_cid, &first_uuid, target, now);
    let err = mgr.redeem_goal(missing_cid, &first_uuid, now, &db).unwrap_err();
    assert_eq!(err.0, STATUS_BAD_REQUEST);
    let after = mgr.get_or_init_goals_at(missing_cid, now);
    let g = after.iter().find(|g| g.uuid == first_uuid).unwrap();
    assert!(!g.claimed, "claimed flag must roll back on DB failure");
    assert_eq!(g.claimed_at_unix, None, "claimed_at_unix must roll back on DB failure");

    // Seed character -> redeem succeeds, credits character EverMarks (500 EM), and second redeem fails
    seed_char(&db, missing_cid as i64, 98_123_001, 1_000_000, 0);
    let redeemed = mgr.redeem_goal(missing_cid, &first_uuid, now, &db).unwrap();
    assert!(redeemed.claimed);
    assert_eq!(mgr.redeem_goal(missing_cid, &first_uuid, now, &db).unwrap_err().0, STATUS_FORBIDDEN);
    db.with_snapshot(|s| {
        assert_eq!(
            s.characters[&(missing_cid as i64)].loyalty_points.get(&1_000_419).copied(),
            Some(500)
        );
    });
}

#[test]
fn pay_for_completion_isk_fallback_unconfigured_cost_and_bonus_progression() {
    let (router, db) = make_router_and_db();
    let mgr = &router.progression.daily_goals;
    let cid = 211_999_002u32;
    let now = mgr.now_unix();
    // Seed 2_500 EverMarks (1st progressive completion cost)
    seed_char(&db, cid as i64, 98_123_002, 500_000, 2_500);

    let goals = mgr.get_or_init_goals_at(cid, now);
    let em_goal = goals
        .iter()
        .find(|g| g.template.category != 6 && g.template.completion_cost_evermarks.is_some())
        .unwrap();
    let other_reg = goals
        .iter()
        .find(|g| g.template.category != 6 && g.uuid != em_goal.uuid)
        .unwrap();
    let bonus_uuid = goals.iter().find(|g| g.template.category == 6).unwrap().uuid;

    // Bonus goal (category 6) has no completion cost configured -> STATUS_PRECONDITION_FAILED
    assert_eq!(
        mgr.pay_for_completion(cid, &bonus_uuid, now, &db).unwrap_err().0,
        STATUS_PRECONDITION_FAILED
    );

    // Unknown UUID -> STATUS_NOT_FOUND
    assert_eq!(
        mgr.pay_for_completion(cid, &[0xDD; 16], now, &db).unwrap_err().0,
        STATUS_NOT_FOUND
    );

    // Pay using EverMarks for em_goal -> increments DailyBonus (1/2) and leaves 0 EM
    let g1 = em_goal.uuid;
    let paid1 = mgr.pay_for_completion(cid, &g1, now, &db).unwrap();
    assert!(paid1.completed && paid1.paid_completion);
    // Already completed -> STATUS_BAD_REQUEST
    assert_eq!(mgr.pay_for_completion(cid, &g1, now, &db).unwrap_err().0, STATUS_BAD_REQUEST);

    db.with_snapshot(|s| {
        assert_eq!(
            s.characters[&(cid as i64)].loyalty_points.get(&1_000_419).copied().unwrap_or(0),
            0
        );
    });

    // Insufficient EverMarks (0 < 4_500) on another uncompleted goal before DailyBonus completes -> STATUS_BAD_REQUEST
    let g3 = goals
        .iter()
        .find(|g| g.template.category != 6 && g.uuid != g1 && g.uuid != other_reg.uuid)
        .unwrap()
        .uuid;
    assert_eq!(mgr.pay_for_completion(cid, &g3, now, &db).unwrap_err().0, STATUS_BAD_REQUEST);

    // Complete a second regular goal via gameplay event -> completes DailyBonus (target=2)
    mgr.on_gameplay_event(cid, other_reg.template.category, other_reg.template.target, &router);
    let updated = mgr.get_or_init_goals_at(cid, now);
    let bonus = updated.iter().find(|g| g.uuid == bonus_uuid).unwrap();
    assert_eq!(bonus.current_progress, 2);
    assert!(bonus.completed);

    // Once DailyBonus (2/2) is completed -> STATUS_PRECONDITION_FAILED
    assert_eq!(mgr.pay_for_completion(cid, &g3, now, &db).unwrap_err().0, STATUS_PRECONDITION_FAILED);
}

#[test]
fn gameplay_events_and_handler_rpc_edges() {
    let (router, db) = make_router_and_db();
    let mgr = &router.progression.daily_goals;
    let cid = 211_999_003u32;
    let other_cid = 211_999_004u32;
    let now = mgr.now_unix();
    seed_char(&db, cid as i64, 98_123_003, 1_000_000, 5_000);
    seed_char(&db, other_cid as i64, 98_123_003, 1_000_000, 5_000);

    // Unauthenticated caller -> STATUS_FORBIDDEN
    let anon = make_ctx(None, 0, now);
    let unauth_req = make_uuid_req("eve_public.dailygoal.api.GetRequest", &[1u8; 16]);
    assert_eq!(dispatch(&router, &anon, &unauth_req).unwrap_err().0, STATUS_FORBIDDEN);

    let user_ctx = make_ctx(Some(cid), 0, now);
    let goals = mgr.get_or_init_goals_at(cid, now);
    let reg = goals.iter().find(|g| g.template.category == 1).unwrap();
    let cat1_uuid = reg.uuid;
    let target = reg.template.target;

    // eve.dailygoal.api.GetProgressRequest before completion -> field 3 set (not_completed = 1)
    let prog_req = make_uuid_req("eve.dailygoal.api.GetProgressRequest", &cat1_uuid);
    let prog_incomplete = dispatch(&router, &user_ctx, &prog_req).unwrap();
    let wm_inc = WireMessage::decode(&prog_incomplete.value).unwrap();
    assert!(wm_inc.fields.iter().any(|f| f.field_number == 3));
    assert!(!wm_inc.fields.iter().any(|f| f.field_number == 4));

    // Missing goal UUID on GetRequest -> STATUS_NOT_FOUND
    assert_eq!(
        dispatch(&router, &user_ctx, &make_uuid_req("eve_public.dailygoal.api.GetRequest", &[0x99; 16]))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );

    // Empty UUID on GetRequest -> STATUS_BAD_REQUEST
    let empty_req = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.GetRequest"),
        value: Vec::new(),
    };
    assert_eq!(dispatch(&router, &user_ctx, &empty_req).unwrap_err().0, STATUS_BAD_REQUEST);

    // on_gameplay_event increments matching category & marks completed when reaching target
    mgr.on_gameplay_event(cid, 1, target, &router);
    let prog_done = dispatch(&router, &user_ctx, &prog_req).unwrap();
    let wm_done = WireMessage::decode(&prog_done.value).unwrap();
    assert!(wm_done.fields.iter().any(|f| f.field_number == 4));

    // admin.ProgressRequest: field 1=goal Identifier, field 2=character Identifier, field 3=progress
    let other_goals = mgr.get_or_init_goals_at(other_cid, now);
    let other_uuid = other_goals[0].uuid;
    let mut admin_val = Vec::new();
    let mut id_msg = Vec::new();
    encode_bytes_field(1, &other_uuid, &mut id_msg);
    encode_bytes_field(1, &id_msg, &mut admin_val);
    let mut char_msg = Vec::new();
    encode_varint_field(1, other_cid as u64, &mut char_msg);
    encode_bytes_field(2, &char_msg, &mut admin_val);
    encode_varint_field(3, 5, &mut admin_val);
    let admin_req = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.admin.ProgressRequest"),
        value: admin_val,
    };
    assert_eq!(dispatch(&router, &user_ctx, &admin_req).unwrap_err().0, STATUS_FORBIDDEN);

    let admin_ctx = make_ctx(Some(cid), ROLE_PROGRAMMER, now);
    assert!(dispatch(&router, &admin_ctx, &admin_req).is_ok());
    let after_other = mgr.get_or_init_goals_at(other_cid, now);
    assert_eq!(after_other[0].current_progress, 5);

    // RedeemAllRequest redeems completed unclaimed goal for cid (leaving incomplete goals unclaimed)
    let redeem_all_req = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.RedeemAllRequest"),
        value: Vec::new(),
    };
    assert!(dispatch(&router, &user_ctx, &redeem_all_req).is_ok());
    let final_goals = mgr.get_or_init_goals_at(cid, now);
    assert!(final_goals.iter().find(|g| g.uuid == cat1_uuid).unwrap().claimed);
    assert_eq!(final_goals.iter().filter(|g| g.claimed).count(), 1);

    // Direct helper assertions: daily_goal_window_unix, is_sde_rotating_regular_template,
    // encode_goal_identifier, encode_loc_message_id, encode_isk_unit, and encode_sp_unit
    assert_eq!(daily_goal_window_unix(0), (39_600, 126_000));
    assert_eq!(daily_goal_window_unix(2), (39_600 + 2 * 86_400, 39_600 + 3 * 86_400));

    let mut reg_sde_tpl = reg.template.clone();
    reg_sde_tpl.uuid[..5].copy_from_slice(b"SDEDG");
    reg_sde_tpl.category = 1;
    assert!(super::is_sde_rotating_regular_template(&reg_sde_tpl));
    reg_sde_tpl.category = 6;
    assert!(!super::is_sde_rotating_regular_template(&reg_sde_tpl));
    reg_sde_tpl.category = 1;
    reg_sde_tpl.uuid[..5].copy_from_slice(b"OTHER");
    assert!(!super::is_sde_rotating_regular_template(&reg_sde_tpl));

    let mut id_out = Vec::new();
    encode_goal_identifier(&cat1_uuid, &mut id_out);
    assert_eq!(id_out.len(), 18);
    assert_eq!(&id_out[2..], &cat1_uuid);

    let loc_bytes = super::encode_loc_message_id(500_123);
    assert!(!loc_bytes.is_empty());
    let isk_bytes = super::encode_isk_unit(50_000);
    assert!(!isk_bytes.is_empty());
    let sp_bytes = super::encode_sp_unit(10_000);
    assert!(!sp_bytes.is_empty());
    assert_ne!(isk_bytes, sp_bytes);
}
