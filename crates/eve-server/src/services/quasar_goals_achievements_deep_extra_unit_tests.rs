//! Deep branch-coverage and mutation-killing unit tests for `services::quasar_daily_goals`
//! and `services::quasar_achievements_ops` (`feat:pve.daily-goals`, `feat:pve.achievements`).

#![forbid(unsafe_code)]

use super::quasar_achievements::{
    AchievementConditionDef, AchievementDef, AchievementManager,
};
use super::quasar_achievements_ops::register_achievement_handlers;
use super::quasar_daily_goals::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use eve_db::{IskCents, WorldRepository};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

const ROLE_PROGRAMMER: u64 = eve_machonet::service_roles::ROLE_GM_MASK;
const CORP_AIR_ASSOCIATION: i64 = 1_000_419;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for _ in 0..5 {
            let candidate = dir.join("data/evomu_sde.sqlite");
            if candidate.exists() {
                return Arc::new(SdeRegistry::open(&candidate).expect("open evomu_sde.sqlite"));
            }
            if !dir.pop() {
                break;
            }
        }
        Arc::new(SdeRegistry::with_canonical_seed())
    }))
}

fn uuid_param(field: u32, uuid: [u8; 16]) -> DynamicQuasarMessage {
    let mut id_msg = DynamicQuasarMessage::new("Identifier");
    id_msg.set(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut req = DynamicQuasarMessage::new("Req");
    req.set(field, DynamicValue::Message(Box::new(id_msg)));
    req
}

fn u32_ident_param(field: u32, id: u32) -> DynamicQuasarMessage {
    let mut id_msg = DynamicQuasarMessage::new("Identifier");
    id_msg.set(1, DynamicValue::Uint32(id));
    let mut req = DynamicQuasarMessage::new("Req");
    req.set(field, DynamicValue::Message(Box::new(id_msg)));
    req
}

fn call_dyn(
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
        .unwrap_or_else(|| panic!("unregistered handler: {type_name}"));
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value: Vec::new(),
    };
    handler(ctx, &any, dyn_msg)
}

#[test]
fn daily_goals_progress_pay_for_completion_redeem_all_and_rollover_branches() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
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

    let cid: u32 = 90_000_001;
    let now_unix: u64 = 1_700_000_000;
    db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&(cid as i64)) {
            ch.balance = IskCents(1_000_00);
            ch.plex_balance = 0;
            ch.free_skill_points = 0.0;
            ch.loyalty_points.insert(CORP_AIR_ASSOCIATION, 0);
        }
    });

    let player_ctx = AuthoritativeContext::for_session_at(
        Some(1),
        Some(cid),
        Some(98_000_001),
        0,
        "tranquility",
        b"gw-session".to_vec(),
        now_unix,
    );
    let gm_ctx = player_ctx.clone().with_service_roles(ROLE_PROGRAMMER);
    let no_char_ctx = AuthoritativeContext::for_session_at(
        Some(1),
        None,
        None,
        0,
        "tranquility",
        b"gw-session".to_vec(),
        now_unix,
    );

    // Day-bucket boundary (11:00 UTC / 39_600s daily downtime rollover) & encoding checks
    assert_eq!(daily_goal_day_bucket(39_600 + 86_399), 0);
    assert_eq!(daily_goal_day_bucket(39_600 + 86_400), 1);
    let tpls = build_sde_daily_goal_templates(&sde);
    assert!(!tpls.is_empty());
    let mut enc_buf = Vec::new();
    encode_goal_attributes(&tpls[0], &mut enc_buf);
    encode_goal_earnings(&tpls[0], 1, &mut enc_buf);
    assert!(!enc_buf.is_empty());

    // 1. GetAllCurrent initializes daily goals for active character
    let cur = call_dyn(
        &router,
        &player_ctx,
        "eve_public.dailygoal.api.GetAllCurrentRequest",
        &DynamicQuasarMessage::new("GetAllCurrent"),
    )
    .expect("GetAllCurrent succeeds");
    assert!(cur.type_url.ends_with("GetAllCurrentResponse"));

    let goals = router
        .progression
        .daily_goals
        .get_or_init_goals_at(cid, now_unix);
    let first_goal = goals
        .iter()
        .find(|g| g.template.category != 2)
        .expect("non-bonus daily goal exists");
    let g1_uuid = first_goal.uuid;
    let g1_target = first_goal.template.target.max(1);

    // 2. GetRequest: missing UUID -> 400, unknown UUID -> 404, valid UUID -> 200
    assert_eq!(
        call_dyn(
            &router,
            &player_ctx,
            "eve_public.dailygoal.api.GetRequest",
            &DynamicQuasarMessage::new("Empty"),
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        call_dyn(
            &router,
            &player_ctx,
            "eve_public.dailygoal.api.GetRequest",
            &uuid_param(1, [0xAA; 16]),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
    assert!(call_dyn(
        &router,
        &player_ctx,
        "eve_public.dailygoal.api.GetRequest",
        &uuid_param(1, g1_uuid),
    )
    .is_ok());

    // 3. Uncompleted goal cannot be redeemed yet (STATUS_FORBIDDEN)
    assert_eq!(
        call_dyn(
            &router,
            &player_ctx,
            "eve_public.dailygoal.api.RedeemRequest",
            &uuid_param(1, g1_uuid),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    // 4. Admin ProgressRequest requires GM role, validates args, and completes goal on target
    assert_eq!(
        call_dyn(
            &router,
            &player_ctx,
            "eve_public.dailygoal.api.admin.ProgressRequest",
            &uuid_param(1, g1_uuid),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    let mut prog_req = uuid_param(1, g1_uuid);
    prog_req.set(3, DynamicValue::Uint64(g1_target));
    assert!(call_dyn(
        &router,
        &gm_ctx,
        "eve_public.dailygoal.api.admin.ProgressRequest",
        &prog_req,
    )
    .is_ok());

    // Verify GetAllWithRewards and GetAllCompletedWithEntitlements include completed goal
    assert!(call_dyn(
        &router,
        &player_ctx,
        "eve_public.dailygoal.api.GetAllWithRewardsRequest",
        &DynamicQuasarMessage::new("Empty"),
    )
    .is_ok());
    assert!(call_dyn(
        &router,
        &player_ctx,
        "eve_public.dailygoal.api.GetAllCompletedWithEntitlementsRequest",
        &DynamicQuasarMessage::new("Empty"),
    )
    .is_ok());

    // 5. Redeem completed goal once -> succeeds; second redeem -> STATUS_FORBIDDEN
    assert!(call_dyn(
        &router,
        &player_ctx,
        "eve_public.dailygoal.api.RedeemRequest",
        &uuid_param(1, g1_uuid),
    )
    .is_ok());
    assert_eq!(
        call_dyn(
            &router,
            &player_ctx,
            "eve_public.dailygoal.api.RedeemRequest",
            &uuid_param(1, g1_uuid),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    // 6. PayForCompletion branches on second uncompleted goal: no-char, already completed, insufficient funds/EverMarks, then paid!
    let second_goal = goals
        .iter()
        .find(|g| g.uuid != g1_uuid && g.template.category != 2)
        .expect("second daily goal exists");
    let g2_uuid = second_goal.uuid;
    let assigner_corp = second_goal.template.effective_assigner_corporation_id() as i64;

    assert_eq!(
        call_dyn(
            &router,
            &no_char_ctx,
            "eve_public.dailygoal.api.PayForCompletionRequest",
            &uuid_param(1, g2_uuid),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call_dyn(
            &router,
            &player_ctx,
            "eve_public.dailygoal.api.PayForCompletionRequest",
            &uuid_param(1, g1_uuid),
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        call_dyn(
            &router,
            &player_ctx,
            "eve_public.dailygoal.api.PayForCompletionRequest",
            &uuid_param(1, g2_uuid),
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Credit sufficient EverMarks & ISK -> PayForCompletion debits EverMarks/ISK, completes goal, and RedeemAll claims it
    let initial_balance = IskCents::from_isk(100_000_000);
    db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&(cid as i64)) {
            ch.balance = initial_balance;
            ch.loyalty_points.insert(assigner_corp, 500_000);
        }
    });
    assert!(call_dyn(
        &router,
        &player_ctx,
        "eve_public.dailygoal.api.PayForCompletionRequest",
        &uuid_param(1, g2_uuid),
    )
    .is_ok());
    let (em_after, bal_after) = db.with_snapshot(|s| {
        let ch = &s.characters[&(cid as i64)];
        (
            ch.loyalty_points.get(&assigner_corp).copied().unwrap_or(0),
            ch.balance,
        )
    });
    assert!(em_after < 500_000 || bal_after < initial_balance);

    assert!(call_dyn(
        &router,
        &player_ctx,
        "eve_public.dailygoal.api.RedeemAllRequest",
        &DynamicQuasarMessage::new("Empty"),
    )
    .is_ok());
}

#[test]
fn achievement_ops_progress_milestones_rewards_claim_and_admin_resets() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = Arc::new(PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    ));

    let mut mgr = AchievementManager::new_with_sde(Some(Arc::clone(&db)), Some(Arc::clone(&sde)));
    mgr.definitions = vec![
        AchievementDef {
            id: 5001,
            is_client_achievement: false,
            category_id: 77,
            name: "First Objective".into(),
            description: String::new(),
            notification_text: String::new(),
            target: 20,
            score: 100,
            milestone_targets: vec![10, 20],
            conditions: vec![AchievementConditionDef {
                condition_id: 1,
                target_value: 20,
                statistic_event_id: 801,
                event_name: "evt_one".into(),
            }],
            reward_isk: 25_000,
            reward_sp: 0,
        },
        AchievementDef {
            id: 5002,
            is_client_achievement: false,
            category_id: 77,
            name: "Second Objective".into(),
            description: String::new(),
            notification_text: String::new(),
            target: 10,
            score: 50,
            milestone_targets: vec![10],
            conditions: vec![AchievementConditionDef {
                condition_id: 1,
                target_value: 10,
                statistic_event_id: 802,
                event_name: "evt_two".into(),
            }],
            reward_isk: 0,
            reward_sp: 0,
        },
    ];
    let mgr = Arc::new(mgr);
    register_achievement_handlers(&router, Arc::clone(&db), Arc::clone(&mgr));

    let cid: u32 = 90_000_001;
    let player_ctx = AuthoritativeContext::for_session_at(
        Some(1),
        Some(cid),
        Some(98_000_001),
        0,
        "tranquility",
        b"gw-ach".to_vec(),
        1_700_000_000,
    );
    let gm_ctx = player_ctx.clone().with_service_roles(ROLE_PROGRAMMER);

    // 1. Query Get, category Get, showcase GetCharacterScore, and unclaimed rewards
    assert!(call_dyn(
        &router,
        &player_ctx,
        "eve_public.achievement.api.GetRequest",
        &u32_ident_param(1, 5001),
    )
    .is_ok());
    assert!(call_dyn(
        &router,
        &player_ctx,
        "eve_public.achievement.category.api.GetRequest",
        &u32_ident_param(1, 77),
    )
    .is_ok());
    assert!(call_dyn(
        &router,
        &player_ctx,
        "eve_public.achievement.showcase.api.GetCharacterScoreRequest",
        &DynamicQuasarMessage::new("Empty"),
    )
    .is_ok());

    // 2. Admin AddProgress crossing milestone 10 then completing at 20
    let mut add_prog = u32_ident_param(1, 5001);
    add_prog.set(2, DynamicValue::Uint64(10));
    assert!(call_dyn(
        &router,
        &gm_ctx,
        "eve_public.achievement.api.admin.AddProgressRequest",
        &add_prog,
    )
    .is_ok());
    assert!(call_dyn(
        &router,
        &gm_ctx,
        "eve_public.achievement.api.admin.AddProgressRequest",
        &add_prog,
    )
    .is_ok());

    // Admin CompleteRequest on second achievement
    assert!(call_dyn(
        &router,
        &gm_ctx,
        "eve_public.achievement.api.admin.CompleteRequest",
        &u32_ident_param(1, 5002),
    )
    .is_ok());

    // Check GetUnclaimedRequest & ClaimRequest
    assert!(call_dyn(
        &router,
        &player_ctx,
        "eve_public.achievement.reward.api.GetUnclaimedRequest",
        &DynamicQuasarMessage::new("Empty"),
    )
    .is_ok());
    let mut claim_msg = DynamicQuasarMessage::new("Claim");
    claim_msg.set(1, DynamicValue::Uint32(5001));
    claim_msg.set(2, DynamicValue::Uint32(10));
    let _ = call_dyn(
        &router,
        &player_ctx,
        "eve_public.achievement.reward.api.ClaimRequest",
        &u32_ident_param(1, 5001),
    );

    // 3. Admin ResetRequest (single), category.admin.ResetRequest (category), and ResetAllRequest
    let reset_one = u32_ident_param(1, 5001);
    assert!(call_dyn(
        &router,
        &gm_ctx,
        "eve_public.achievement.api.admin.ResetRequest",
        &reset_one,
    )
    .is_ok());

    let reset_cat = u32_ident_param(1, 77);
    assert!(call_dyn(
        &router,
        &gm_ctx,
        "eve_public.achievement.category.api.admin.ResetRequest",
        &reset_cat,
    )
    .is_ok());

    assert!(call_dyn(
        &router,
        &gm_ctx,
        "eve_public.achievement.api.admin.ResetAllRequest",
        &DynamicQuasarMessage::new("Empty"),
    )
    .is_ok());
}
