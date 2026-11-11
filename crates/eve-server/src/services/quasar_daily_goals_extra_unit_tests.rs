//! Extra unit and mutation-killing tests for `services::quasar_daily_goals` (`feat:pve.daily-goals`).

use super::super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_PRECONDITION_FAILED,
};
use eve_db::{IskCents, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny,
    QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
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

fn dispatch_dyn(
    router: &PublicGatewayRouter,
    ctx: &AuthoritativeContext,
    type_name: &str,
    dyn_msg: &DynamicQuasarMessage,
) -> Result<GoogleAny, (u32, String)> {
    let handler = router
        .handlers
        .read()
        .unwrap()
        .get(type_name)
        .cloned()
        .expect("registered handler");
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value: Vec::new(),
    };
    handler(ctx, &any, dyn_msg)
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

fn seed_char(db: &WorldRepository, char_id: i64, corp_id: i64, isk_cents: i64, em: i64) {
    db.with_mut_snapshot(|s| {
        let mut rec = s.characters[&90_000_001].clone();
        rec.character_id = char_id;
        rec.corporation_id = corp_id;
        rec.balance = IskCents(isk_cents);
        rec.free_skill_points = 500.0;
        rec.loyalty_points.clear();
        if em != 0 {
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

#[test]
fn custom_template_message_ids_and_extract_uuid_edge_cases() {
    let mut tpl = default_daily_bonus_template();
    tpl.category = 1;
    tpl.name_message_id = 777_001;
    tpl.description_message_id = 777_002;
    tpl.help_text_message_id = 777_003;
    tpl.assigner_corporation_id = 1_000_125;
    assert_eq!(tpl.effective_goal_category(), 1);
    assert_eq!(tpl.effective_name_message_id(), 777_001);
    assert_eq!(tpl.effective_description_message_id(), 777_002);
    assert_eq!(tpl.effective_help_text_message_id(), 777_003);
    assert_eq!(tpl.effective_assigner_corporation_id(), 1_000_125);

    // Zero message IDs fall back to category defaults
    tpl.name_message_id = 0;
    tpl.description_message_id = 0;
    tpl.help_text_message_id = 0;
    tpl.assigner_corporation_id = 0;
    assert_eq!(tpl.effective_name_message_id(), 659_195);
    assert_eq!(tpl.effective_description_message_id(), 696_251);
    assert_eq!(tpl.effective_help_text_message_id(), 659_196);
    assert_eq!(tpl.effective_assigner_corporation_id(), 1_000_419);

    // extract_uuid edge cases: wrong byte length, non-bytes inner field, non-message outer field
    let mut m = DynamicQuasarMessage::new("test.Msg");
    assert_eq!(extract_uuid(&m, 1), None);
    m.set(1, DynamicValue::Uint64(99));
    assert_eq!(extract_uuid(&m, 1), None);

    let mut sub_short = DynamicQuasarMessage::new("test.Id");
    sub_short.set(1, DynamicValue::Bytes(vec![1u8; 15]));
    m.set(1, DynamicValue::Message(Box::new(sub_short)));
    assert_eq!(extract_uuid(&m, 1), None);

    let mut sub_long = DynamicQuasarMessage::new("test.Id");
    sub_long.set(1, DynamicValue::Bytes(vec![2u8; 17]));
    m.set(1, DynamicValue::Message(Box::new(sub_long)));
    assert_eq!(extract_uuid(&m, 1), None);

    let mut sub_non_bytes = DynamicQuasarMessage::new("test.Id");
    sub_non_bytes.set(1, DynamicValue::Uint32(16));
    m.set(1, DynamicValue::Message(Box::new(sub_non_bytes)));
    assert_eq!(extract_uuid(&m, 1), None);

    let mut sub_ok = DynamicQuasarMessage::new("test.Id");
    sub_ok.set(1, DynamicValue::Bytes(vec![0x42; 16]));
    m.set(1, DynamicValue::Message(Box::new(sub_ok)));
    assert_eq!(extract_uuid(&m, 1), Some([0x42; 16]));
}

#[test]
fn store_loading_corrupt_json_empty_store_and_day_bucket_zero_upgrade() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());

    // 1. Corrupt JSON in blob falls back to SDE templates
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs
            .insert("quasar_daily_goals".into(), "{not-valid-json".into());
    });
    let mgr_corrupt = DailyGoalManager::new_with_sde(Some(Arc::clone(&db)), Some(Arc::clone(&sde)));
    assert_eq!(mgr_corrupt.get_templates().len(), 6);
    assert!(mgr_corrupt.now_unix() > 0);

    // 2. Empty DailyGoalStore JSON falls back to SDE templates
    let empty_store = DailyGoalStore::default();
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert(
            "quasar_daily_goals".into(),
            serde_json::to_string(&empty_store).unwrap(),
        );
    });
    let mgr_empty = DailyGoalManager::new_with_sde(Some(Arc::clone(&db)), Some(Arc::clone(&sde)));
    assert_eq!(mgr_empty.get_templates().len(), 6);

    // 3. Store with existing goal at day_bucket == 0 upgrades day_bucket without resetting progress
    let tpls = build_sde_daily_goal_templates(&sde);
    let mut store = DailyGoalStore {
        templates: vec![tpls[0].clone()],
        character_goals: Default::default(),
        ..Default::default()
    };
    store.character_goals.insert(
        4001,
        vec![CharacterDailyGoal {
            uuid: tpls[0].uuid,
            template: tpls[0].clone(),
            current_progress: 12,
            completed: false,
            claimed: false,
            paid_completion: false,
            completed_at_unix: None,
            claimed_at_unix: None,
            day_bucket: 0,
            expired: false,
        }],
    );
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert(
            "quasar_daily_goals".into(),
            serde_json::to_string(&store).unwrap(),
        );
    });
    let mgr = DailyGoalManager::new(Some(Arc::clone(&db)));
    // Auto-injected bonus template since store only had category 1
    assert_eq!(mgr.get_templates().len(), 2);

    // Calling get_or_init_goals_at with now_unix > 39_600 + 86_400 upgrades day_bucket from 0 -> 1 without wiping progress 12!
    let t_day1 = 39_600 + 86_400;
    let goals_day1 = mgr.get_or_init_goals_at(4001, t_day1);
    let g0 = goals_day1.iter().find(|g| g.uuid == tpls[0].uuid).unwrap();
    assert_eq!(g0.current_progress, 12);
    assert_eq!(g0.day_bucket, 1);

    // Calling at the exact same day_bucket does not reset progress
    let goals_same = mgr.get_or_init_goals_at(4001, t_day1 + 500);
    assert_eq!(
        goals_same
            .iter()
            .find(|g| g.uuid == tpls[0].uuid)
            .unwrap()
            .current_progress,
        12
    );

    // Advancing to day_bucket 2 resets progress to 0
    let t_day2 = 39_600 + 2 * 86_400;
    let goals_day2 = mgr.get_or_init_goals_at(4001, t_day2);
    let g0_reset = goals_day2.iter().find(|g| g.uuid == tpls[0].uuid).unwrap();
    assert_eq!(g0_reset.current_progress, 0);
    assert_eq!(g0_reset.day_bucket, 2);
}

#[test]
fn add_progress_boundaries_expired_filtering_and_already_completed() {
    let (_router, db) = make_router_and_db();
    let mgr = DailyGoalManager::new_with_sde(Some(Arc::clone(&db)), Some(shared_sde()));
    let cid = 211200301u32;
    let goals = mgr.get_or_init_goals_at(cid, 200_000);
    let g1_uuid = goals[0].uuid; // target = 25

    // Progress to target - 1 (24): not completed
    let (g_24, just_comp_24) = mgr.add_progress(cid, &g1_uuid, 24, 200_001).unwrap();
    assert_eq!(g_24.current_progress, 24);
    assert!(!g_24.completed);
    assert!(!just_comp_24);
    assert_eq!(g_24.completed_at_unix, None);

    // Progress by 1 -> reaches exact target 25: just_completed = true
    let (g_25, just_comp_25) = mgr.add_progress(cid, &g1_uuid, 1, 200_002).unwrap();
    assert_eq!(g_25.current_progress, 25);
    assert!(g_25.completed);
    assert!(just_comp_25);
    assert_eq!(g_25.completed_at_unix, Some(200_002));

    // Additional progress after completion -> just_completed = false, timestamp preserved
    let (g_30, just_comp_30) = mgr.add_progress(cid, &g1_uuid, 5, 200_999).unwrap();
    assert_eq!(g_30.current_progress, 30);
    assert!(g_30.completed);
    assert!(!just_comp_30);
    assert_eq!(g_30.completed_at_unix, Some(200_002));

    // Expire g1 and verify get_or_init_goals_at filters it out and add_progress returns None
    {
        let mut map = mgr.character_goals.write().unwrap();
        let list = map.get_mut(&cid).unwrap();
        list.iter_mut().find(|g| g.uuid == g1_uuid).unwrap().expired = true;
    }
    let active = mgr.get_or_init_goals_at(cid, 200_010);
    assert_eq!(active.len(), 4);
    assert!(mgr.add_progress(cid, &g1_uuid, 1, 200_010).is_none());
}

#[test]
fn redeem_goal_rollback_on_missing_char_or_balance_overflow_and_zero_corp() {
    let (router, db) = make_router_and_db();
    let mgr = &router.progression.daily_goals;

    // 1. Character not in DB: completing goal then redeeming rolls back claimed flag
    let ghost_cid = 211299999u32;
    let goals = mgr.get_or_init_goals_at(ghost_cid, 300_000);
    let g1_uuid = goals[0].uuid;
    mgr.add_progress(ghost_cid, &g1_uuid, 25, 300_001).unwrap();
    let err = mgr.redeem_goal(ghost_cid, &g1_uuid, 300_002, &db).unwrap_err();
    assert_eq!(err.0, STATUS_BAD_REQUEST);
    let after_rollback = mgr.get_or_init_goals_at(ghost_cid, 300_002);
    let g1_ghost = after_rollback.iter().find(|g| g.uuid == g1_uuid).unwrap();
    assert!(!g1_ghost.claimed);
    assert_eq!(g1_ghost.claimed_at_unix, None);

    // 2. Balance overflow: character with i64::MAX cents cannot receive ISK reward -> rolls back claimed
    let rich_cid = 211200302u32;
    seed_char(&db, rich_cid as i64, 0, i64::MAX, 0);
    let _ = mgr.get_or_init_goals_at(rich_cid, 300_000);
    mgr.add_progress(rich_cid, &g1_uuid, 25, 300_001).unwrap();
    let err_ovf = mgr.redeem_goal(rich_cid, &g1_uuid, 300_002, &db).unwrap_err();
    assert_eq!(err_ovf.0, STATUS_BAD_REQUEST);
    let g1_rich = mgr
        .get_or_init_goals_at(rich_cid, 300_002)
        .into_iter()
        .find(|g| g.uuid == g1_uuid)
        .unwrap();
    assert!(!g1_rich.claimed);

    // 3. Character with corporation_id == 0: redeems cleanly without inserting into corp_lp_balances
    db.with_mut_snapshot(|s| {
        s.characters.get_mut(&(rich_cid as i64)).unwrap().balance = IskCents::from_isk(100_000);
    });
    let redeemed = mgr
        .redeem_goal(rich_cid, &g1_uuid, 300_005, &db)
        .expect("redeem with corp_id 0");
    assert!(redeemed.claimed);
    db.with_snapshot(|s| {
        assert!(!s.corp_lp_balances.contains_key(&(0, 1_000_419)));
        assert_eq!(
            s.characters[&(rich_cid as i64)].loyalty_points[&1_000_419],
            500
        );
    });
}

#[test]
fn pay_for_completion_exact_cost_boundaries_and_zero_cost_precondition() {
    let (router, db) = make_router_and_db();
    let mgr = &router.progression.daily_goals;

    // Configure a template with Some(0) EM and Some(0) ISK -> treated as unconfigured (PRECONDITION_FAILED)
    let mut zero_cost_tpl = default_daily_bonus_template();
    zero_cost_tpl.uuid = [0x77; 16];
    zero_cost_tpl.category = 3;
    zero_cost_tpl.completion_cost_evermarks = Some(0);
    zero_cost_tpl.completion_cost_isk = Some(0);
    mgr.configure_template(zero_cost_tpl.clone());

    let cid = 211200303u32;
    // Exact 2500 EverMarks (cost of 1st paid goal g1)
    seed_char(&db, cid as i64, 98000303, 10_000_000, 2_500);
    let goals = mgr.get_or_init_goals_at(cid, 400_000);
    let g1_uuid = goals[0].uuid; // EM=2500, ISK=None
    let g2_uuid = goals[1].uuid; // EM=2500 (progressive 4500 on 2nd paid), ISK=None

    let err_zero = mgr
        .pay_for_completion(cid, &zero_cost_tpl.uuid, 400_000, &db)
        .unwrap_err();
    assert_eq!(err_zero.0, STATUS_PRECONDITION_FAILED);

    // Paying for g1 with exact 2500 EM -> succeeds, EM becomes 0, ISK untouched
    mgr.pay_for_completion(cid, &g1_uuid, 400_001, &db)
        .expect("exact 2500 EM");
    assert_eq!(
        db.with_snapshot(|s| s.characters[&(cid as i64)].loyalty_points[&1_000_419]),
        0
    );

    // Paying for g2 with 0 EM (even with 100,000 ISK) -> fails STATUS_BAD_REQUEST (no ISK fallback), then succeeds with exact 4,500 EM
    assert_eq!(
        mgr.pay_for_completion(cid, &g2_uuid, 400_002, &db).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    db.with_mut_snapshot(|s| {
        s.characters.get_mut(&(cid as i64)).unwrap().loyalty_points.insert(1_000_419, 4_500);
    });
    mgr.pay_for_completion(cid, &g2_uuid, 400_002, &db)
        .expect("exact 4,500 EM progressive 2nd paid completion");
    assert_eq!(
        db.with_snapshot(|s| s.characters[&(cid as i64)].loyalty_points[&1_000_419]),
        0
    );
    assert_eq!(
        db.with_snapshot(|s| s.characters[&(cid as i64)].balance),
        IskCents(10_000_000)
    );

    // Ghost character not in DB -> pay_for_completion fails with STATUS_BAD_REQUEST
    let ghost = 211299998u32;
    let _ = mgr.get_or_init_goals_at(ghost, 400_000);
    let err_ghost = mgr
        .pay_for_completion(ghost, &g1_uuid, 400_003, &db)
        .unwrap_err();
    assert_eq!(err_ghost.0, STATUS_BAD_REQUEST);
}

#[test]
fn on_gameplay_event_bonus_progression_and_admin_progress_variants() {
    let (router, db) = make_router_and_db();
    db.set_clock(Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(
        300_000,
    )));
    let cid = 211200304u32;
    let other_cid = 211200305u32;
    seed_char(&db, cid as i64, 98000304, 1_000_000, 0);
    seed_char(&db, other_cid as i64, 98000304, 1_000_000, 0);

    let mgr = &router.progression.daily_goals;
    let goals = mgr.get_or_init_goals_at(cid, 300_000);
    let bonus_uuid = goals.iter().find(|g| g.template.category == 6).unwrap().uuid;

    // 1. Partial progress on category 1 (5 / 25) -> bonus goal stays at 0 (no bonus ProgressedNotice)
    router.routing_cache.write().unwrap().entries.clear();
    mgr.on_gameplay_event(cid, 1, 5, &router);
    assert_eq!(router.routing_cache.read().unwrap().entries.len(), 1);

    // 2. Complete category 1 (+20 -> 25/25) -> 1 regular goal complete, bonus progresses to 1/2 (not completed)
    router.routing_cache.write().unwrap().entries.clear();
    mgr.on_gameplay_event(cid, 1, 20, &router);
    // Notices: cat1 Progressed, cat1 Completed, bonus Progressed (3 notices total)
    assert_eq!(router.routing_cache.read().unwrap().entries.len(), 3);
    let g_after_1 = mgr.get_or_init_goals_at(cid, 300_000);
    let bonus_1 = g_after_1.iter().find(|g| g.uuid == bonus_uuid).unwrap();
    assert_eq!(bonus_1.current_progress, 1);
    assert!(!bonus_1.completed);

    // 3. Complete category 5 (+1 -> 1/1) -> 2 regular goals complete, bonus progresses to 2/2 and completes!
    router.routing_cache.write().unwrap().entries.clear();
    mgr.on_gameplay_event(cid, 5, 1, &router);
    // Notices: cat5 Progressed, cat5 Completed, bonus Progressed, bonus Completed (4 notices total)
    assert_eq!(router.routing_cache.read().unwrap().entries.len(), 4);
    let g_after_2 = mgr.get_or_init_goals_at(cid, 300_000);
    let bonus_2 = g_after_2.iter().find(|g| g.uuid == bonus_uuid).unwrap();
    assert_eq!(bonus_2.current_progress, 2);
    assert!(bonus_2.completed);

    // 4. Test eve_public.dailygoal.api.admin.ProgressRequest dynamic value variants
    let admin = make_ctx(Some(cid), ROLE_PROGRAMMER, 300_100);
    let admin_no_char = make_ctx(None, ROLE_PROGRAMMER, 300_100);
    let g3_uuid = goals.iter().find(|g| g.template.category == 3).unwrap().uuid; // ScanSignature, target 5

    // Admin without active character -> FORBIDDEN
    let mut id_sub = DynamicQuasarMessage::new("id");
    id_sub.set(1, DynamicValue::Bytes(g3_uuid.to_vec()));
    let mut d = DynamicQuasarMessage::new("req");
    d.set(1, DynamicValue::Message(Box::new(id_sub.clone())));
    assert_eq!(
        dispatch_dyn(
            &router,
            &admin_no_char,
            "eve_public.dailygoal.api.admin.ProgressRequest",
            &d
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    // Missing uuid -> BAD_REQUEST
    let empty_d = DynamicQuasarMessage::new("req");
    assert_eq!(
        dispatch_dyn(
            &router,
            &admin,
            "eve_public.dailygoal.api.admin.ProgressRequest",
            &empty_d
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Default target_char (omitted field 2 -> cid) and default delta (omitted field 3 -> 1)
    dispatch_dyn(
        &router,
        &admin,
        "eve_public.dailygoal.api.admin.ProgressRequest",
        &d,
    )
    .expect("default target_char and delta=1");
    let g3_prog = mgr
        .get_or_init_goals_at(cid, 300_100)
        .into_iter()
        .find(|g| g.uuid == g3_uuid)
        .unwrap()
        .current_progress;
    assert_eq!(g3_prog, 1);

    // Explicit target_char variants (Direct Int32, Int64, Uint32, Message(Int64)) and Int64 delta
    let _ = mgr.get_or_init_goals_at(other_cid, 300_100);
    for (char_val, delta_val) in [
        (DynamicValue::Int32(other_cid as i32), DynamicValue::Int64(1)),
        (DynamicValue::Int64(other_cid as i64), DynamicValue::Uint64(1)),
        (DynamicValue::Uint32(other_cid), DynamicValue::Int64(1)),
        (DynamicValue::Uint64(other_cid as u64), DynamicValue::Uint64(2)),
    ] {
        let mut req_d = DynamicQuasarMessage::new("req");
        req_d.set(1, DynamicValue::Message(Box::new(id_sub.clone())));
        req_d.set(2, char_val);
        req_d.set(3, delta_val);
        dispatch_dyn(
            &router,
            &admin,
            "eve_public.dailygoal.api.admin.ProgressRequest",
            &req_d,
        )
        .expect("variant progress");
    }
    let other_g3 = mgr
        .get_or_init_goals_at(other_cid, 300_100)
        .into_iter()
        .find(|g| g.uuid == g3_uuid)
        .unwrap();
    assert_eq!(other_g3.current_progress, 5);
    assert!(other_g3.completed);

    // Also test GetProgressRequest when not completed (field 3 = 1)
    let user = make_ctx(Some(cid), 0, 300_100);
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &g3_uuid, &mut id_bytes);
    let mut req_bytes = Vec::new();
    encode_bytes_field(1, &id_bytes, &mut req_bytes);
    let prog_uncomp = dispatch(
        &router,
        &user,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.GetProgressRequest"),
            value: req_bytes,
        },
    )
    .expect("GetProgressRequest uncompleted");
    let dyn_uncomp =
        crate::services::quasar_char_skill_services::decode_wire_dynamic("test.Msg", &prog_uncomp.value);
    assert_eq!(dyn_uncomp.get(1), Some(&DynamicValue::Uint64(1)));
    assert_eq!(dyn_uncomp.get(3), Some(&DynamicValue::Uint64(1)));
    assert!(dyn_uncomp.get(4).is_none());
}
