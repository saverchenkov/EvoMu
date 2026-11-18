//! Edge-case behavioral tests for `quasar_daily_goals`.

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_PRECONDITION_FAILED,
};
use eve_db::{IskCents, WorldRepository};
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, GoogleAny, QuasarDescriptorRegistry, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
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
    pb::message(type_name)
        .set("goal.uuid", uuid)
        .must_to_any()
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
    let reg = goals
        .iter()
        .find(|g| (1..=5).contains(&g.template.category))
        .expect("active rotating regular daily goal");
    let reg_cat = reg.template.category;
    let reg_uuid = reg.uuid;
    let target = reg.template.target;

    // eve.dailygoal.api.GetProgressRequest before completion -> field 3 set (not_completed = 1)
    let prog_req = make_uuid_req("eve.dailygoal.api.GetProgressRequest", &reg_uuid);
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
    mgr.on_gameplay_event(cid, reg_cat, target, &router);
    let prog_done = dispatch(&router, &user_ctx, &prog_req).unwrap();
    let wm_done = WireMessage::decode(&prog_done.value).unwrap();
    assert!(wm_done.fields.iter().any(|f| f.field_number == 4));

    // admin.ProgressRequest: field 1=goal Identifier, field 2=character Identifier, field 3=progress
    let other_goals = mgr.get_or_init_goals_at(other_cid, now);
    let other_uuid = other_goals[0].uuid;
    let admin_req = pb::message("eve_public.dailygoal.api.admin.ProgressRequest")
        .set("goal.uuid", &other_uuid)
        .set("character.sequential", other_cid)
        .set("progress", 5u64)
        .must_to_any();
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
    assert!(final_goals.iter().find(|g| g.uuid == reg_uuid).unwrap().claimed);
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
    encode_goal_identifier(&reg_uuid, &mut id_out);
    assert_eq!(id_out.len(), 18);
    assert_eq!(&id_out[2..], &reg_uuid);

    let loc_bytes = super::encode_loc_message_id(500_123);
    assert!(!loc_bytes.is_empty());
    let isk_bytes = super::encode_isk_unit(50_000);
    assert!(!isk_bytes.is_empty());
    let sp_bytes = super::encode_sp_unit(10_000);
    assert!(!sp_bytes.is_empty());
    assert_ne!(isk_bytes, sp_bytes);
}

fn decode_get_response_window_and_earnings(any: &GoogleAny) -> (u64, u64, u64, usize) {
    let wm = WireMessage::decode(&any.value).expect("decode GetResponse wire");
    let attrs_bytes = wm
        .fields
        .iter()
        .find(|f| f.field_number == 1)
        .and_then(|f| f.value.as_bytes())
        .expect("GetResponse.goal");
    let attrs_wm = WireMessage::decode(attrs_bytes).expect("decode Attributes wire");
    let ts_secs = |field_num: u32| -> u64 {
        let ts_bytes = attrs_wm
            .fields
            .iter()
            .find(|f| f.field_number == field_num)
            .and_then(|f| f.value.as_bytes())
            .expect("Timestamp field");
        let ts_wm = WireMessage::decode(ts_bytes).expect("decode Timestamp wire");
        ts_wm
            .fields
            .iter()
            .find(|f| f.field_number == 1)
            .and_then(|f| f.value.as_varint())
            .unwrap_or(0)
    };
    let progress = wm
        .fields
        .iter()
        .find(|f| f.field_number == 2)
        .and_then(|f| f.value.as_varint())
        .unwrap_or(0);
    let earnings_count = wm.fields.iter().filter(|f| f.field_number == 4).count();
    (ts_secs(10), ts_secs(11), progress, earnings_count)
}

fn collect_list_uuids(any: &GoogleAny, nested_id: bool) -> Vec<[u8; 16]> {
    let wm = WireMessage::decode(&any.value).expect("decode list response wire");
    wm.fields
        .iter()
        .filter(|f| f.field_number == 1)
        .filter_map(|f| f.value.as_bytes())
        .filter_map(|entry_bytes| {
            let entry_wm = WireMessage::decode(entry_bytes).ok()?;
            let id_bytes = if nested_id {
                entry_wm
                    .fields
                    .iter()
                    .find(|f| f.field_number == 1)
                    .and_then(|f| f.value.as_bytes())?
            } else {
                entry_bytes
            };
            let id_wm = WireMessage::decode(id_bytes).ok()?;
            let uuid_bytes = id_wm
                .fields
                .iter()
                .find(|f| f.field_number == 1)
                .and_then(|f| f.value.as_bytes())?;
            uuid_bytes.try_into().ok()
        })
        .collect()
}

#[test]
fn get_request_serves_expired_rotated_and_persisted_goals_while_active_lists_exclude_them() {
    let (router, db) = make_router_and_db();
    let mgr = &router.progression.daily_goals;
    let cid = 211_338_001u32;
    // Odd day_bucket (20789) from GH #1338: `SDEDG` #2 (MineOre) rotates out in favor of `SDEDG` #5.
    let odd_day_now: u64 = 1_796_209_417;
    assert_eq!(daily_goal_day_bucket(odd_day_now) % 2, 1);
    seed_char(&db, cid as i64, 98_133_801, 1_000_000, 5_000);
    let user_ctx = make_ctx(Some(cid), 0, odd_day_now);

    let mut sdedg2_uuid = [0u8; 16];
    sdedg2_uuid[..5].copy_from_slice(b"SDEDG");
    sdedg2_uuid[15] = 2;

    // 1. Fresh character calling `eve_public.dailygoal.api.GetRequest` on rotated-out `SDEDG` #2 -> 200 OK
    // with past `active_after`/`active_until` and omitted `earnings`.
    let pub_rsp = dispatch(
        &router,
        &user_ctx,
        &make_uuid_req("eve_public.dailygoal.api.GetRequest", &sdedg2_uuid),
    )
    .expect("eve_public GetRequest for rotated-out SDEDG #2 must succeed");
    let (pub_after, pub_until, pub_prog, pub_earnings) =
        decode_get_response_window_and_earnings(&pub_rsp);
    assert!(pub_after < pub_until && pub_until < odd_day_now);
    assert_eq!(pub_prog, 0);
    assert_eq!(pub_earnings, 0, "expired goal GetResponse must omit earnings");

    // 2. Internal `eve.dailygoal.api.GetRequest` matches expired window consistently.
    let int_rsp = dispatch(
        &router,
        &user_ctx,
        &make_uuid_req("eve.dailygoal.api.GetRequest", &sdedg2_uuid),
    )
    .expect("eve GetRequest for rotated-out SDEDG #2 must succeed");
    let (int_after, int_until, _, _) = decode_get_response_window_and_earnings(&int_rsp);
    assert_eq!((int_after, int_until), (pub_after, pub_until));

    // 3. Active lists and redeem/progress RPCs MUST still exclude rotated-out `SDEDG` #2.
    let active_goals = mgr.get_or_init_goals_at(cid, odd_day_now);
    assert!(!active_goals.iter().any(|g| g.uuid == sdedg2_uuid));
    let active_uuid = active_goals[0].uuid;

    let all_curr_rsp = dispatch(
        &router,
        &user_ctx,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.GetAllCurrentRequest"),
            value: Vec::new(),
        },
    )
    .unwrap();
    assert!(!collect_list_uuids(&all_curr_rsp, true).contains(&sdedg2_uuid));

    let all_int_rsp = dispatch(
        &router,
        &user_ctx,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.GetAllRequest"),
            value: Vec::new(),
        },
    )
    .unwrap();
    assert!(!collect_list_uuids(&all_int_rsp, false).contains(&sdedg2_uuid));

    assert_eq!(
        dispatch(
            &router,
            &user_ctx,
            &make_uuid_req("eve.dailygoal.api.GetProgressRequest", &sdedg2_uuid),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        dispatch(
            &router,
            &user_ctx,
            &make_uuid_req("eve_public.dailygoal.api.RedeemRequest", &sdedg2_uuid),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );

    // Active goal GetRequest still advertises active window and earnings.
    let active_rsp = dispatch(
        &router,
        &user_ctx,
        &make_uuid_req("eve_public.dailygoal.api.GetRequest", &active_uuid),
    )
    .unwrap();
    let (act_after, act_until, _, act_earnings) =
        decode_get_response_window_and_earnings(&active_rsp);
    assert!(act_after <= odd_day_now && odd_day_now < act_until);
    assert!(act_earnings > 0);

    // 4. Historical expired persisted goal (not in templates, from an earlier day bucket) returns 200 OK
    // with its historical expired window, preserves stored progress, omits earnings, and stays excluded from reward lists.
    let hist_uuid = [0x44u8; 16];
    let hist_bucket = daily_goal_day_bucket(odd_day_now) - 3;
    let mut hist_tpl = active_goals[0].template.clone();
    hist_tpl.uuid = hist_uuid;
    hist_tpl.isk_reward = 250_000;
    {
        let mut map = mgr.character_goals.write().unwrap();
        map.get_mut(&cid).unwrap().push(CharacterDailyGoal {
            uuid: hist_uuid,
            template: hist_tpl,
            current_progress: 10,
            completed: true,
            claimed: false,
            paid_completion: false,
            completed_at_unix: Some(odd_day_now - 259_200),
            claimed_at_unix: None,
            day_bucket: hist_bucket,
            expired: true,
            prematurely_deactivated: false,
        });
    }

    let hist_pub_rsp = dispatch(
        &router,
        &user_ctx,
        &make_uuid_req("eve_public.dailygoal.api.GetRequest", &hist_uuid),
    )
    .expect("expired persisted goal must resolve on eve_public.GetRequest");
    let (h_after, h_until, h_prog, h_earnings) =
        decode_get_response_window_and_earnings(&hist_pub_rsp);
    assert_eq!((h_after, h_until), daily_goal_window_unix(hist_bucket));
    assert!(h_until < odd_day_now);
    assert_eq!(h_prog, 10);
    assert_eq!(h_earnings, 0, "expired completed goal must omit earnings");

    let hist_int_rsp = dispatch(
        &router,
        &user_ctx,
        &make_uuid_req("eve.dailygoal.api.GetRequest", &hist_uuid),
    )
    .expect("expired persisted goal must resolve on eve.GetRequest");
    let (hi_after, hi_until, _, _) = decode_get_response_window_and_earnings(&hist_int_rsp);
    assert_eq!((hi_after, hi_until), (h_after, h_until));

    let rewards_rsp = dispatch(
        &router,
        &user_ctx,
        &GoogleAny {
            type_url: format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.GetAllWithRewardsRequest"
            ),
            value: Vec::new(),
        },
    )
    .unwrap();
    assert!(!collect_list_uuids(&rewards_rsp, false).contains(&hist_uuid));

    let ent_rsp = dispatch(
        &router,
        &user_ctx,
        &GoogleAny {
            type_url: format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.GetAllCompletedWithEntitlementsRequest"
            ),
            value: Vec::new(),
        },
    )
    .unwrap();
    assert!(!collect_list_uuids(&ent_rsp, true).contains(&hist_uuid));

    // 5. Unknown random UUID still returns STATUS_NOT_FOUND on both GetRequest handlers.
    for rpc in ["eve_public.dailygoal.api.GetRequest", "eve.dailygoal.api.GetRequest"] {
        assert_eq!(
            dispatch(&router, &user_ctx, &make_uuid_req(rpc, &[0xAB; 16]))
                .unwrap_err()
                .0,
            STATUS_NOT_FOUND
        );
    }
}

#[test]
fn get_request_template_fallback_and_prematurely_deactivated_stays_not_found() {
    let (router, db) = make_router_and_db();
    let mgr = &router.progression.daily_goals;
    let even_day_now: u64 = 1_796_123_017; // day_bucket = 20788 (even): SDEDG #2 is active, SDEDG #5 rotates out
    assert_eq!(daily_goal_day_bucket(even_day_now) % 2, 0);

    // 1. Direct template-catalog fallback when `character_goals` has no entry for character/UUID.
    let uninit_cid = 211_338_002u32;
    let mut sdedg2_uuid = [0u8; 16];
    sdedg2_uuid[..5].copy_from_slice(b"SDEDG");
    sdedg2_uuid[15] = 2;
    let cat_resolved = mgr
        .find_expired_or_template_goal(uninit_cid, &sdedg2_uuid, even_day_now)
        .expect("template catalog fallback must resolve uninstantiated SDEDG UUID");
    assert!(cat_resolved.is_expired_or_catalog);
    assert_eq!(cat_resolved.current_progress, 0);
    let (cat_after, cat_until) = resolved_goal_window_unix(&cat_resolved, even_day_now);
    assert!(cat_after < cat_until && cat_until < even_day_now);

    // Verify boundary timestamps in `resolved_goal_window_unix` always expire strictly before `now`.
    for edge_now in [1u64, 50_000, 39_600 + 86_400, even_day_now] {
        let (ea, eu) = resolved_goal_window_unix(&cat_resolved, edge_now);
        assert!(ea <= eu && eu < edge_now, "edge_now={edge_now} got ({ea}, {eu})");
    }

    // 2. Admin-deactivated (`prematurely_deactivated = true`) goal MUST return 404 on BOTH GetRequest RPCs
    // and MUST NOT resurrect via `mgr.get_templates()` fallback on the same day.
    let cid = 211_338_003u32;
    seed_char(&db, cid as i64, 98_133_803, 1_000_000, 5_000);
    let user_ctx = make_ctx(Some(cid), 0, even_day_now);
    let active_goals = mgr.get_or_init_goals_at(cid, even_day_now);
    assert!(active_goals.iter().any(|g| g.uuid == sdedg2_uuid));

    {
        let mut map = mgr.character_goals.write().unwrap();
        let g = map
            .get_mut(&cid)
            .unwrap()
            .iter_mut()
            .find(|g| g.uuid == sdedg2_uuid)
            .unwrap();
        g.expired = true;
        g.prematurely_deactivated = true;
    }

    for rpc in ["eve_public.dailygoal.api.GetRequest", "eve.dailygoal.api.GetRequest"] {
        assert_eq!(
            dispatch(&router, &user_ctx, &make_uuid_req(rpc, &sdedg2_uuid))
                .unwrap_err()
                .0,
            STATUS_NOT_FOUND,
            "prematurely deactivated goal must remain 404 on {rpc}"
        );
    }

    // 3. Rolling forward to the next day resets `prematurely_deactivated` so rotated/new-day history works normally.
    let next_day_now = even_day_now + 86_400;
    let next_day_ctx = make_ctx(Some(cid), 0, next_day_now);
    let next_rsp = dispatch(
        &router,
        &next_day_ctx,
        &make_uuid_req("eve_public.dailygoal.api.GetRequest", &sdedg2_uuid),
    )
    .expect("next-day rollover clears previous day's premature deactivation");
    let (nd_after, nd_until, _, nd_earnings) = decode_get_response_window_and_earnings(&next_rsp);
    assert!(nd_after < nd_until && nd_until < next_day_now);
    assert_eq!(nd_earnings, 0);
}
