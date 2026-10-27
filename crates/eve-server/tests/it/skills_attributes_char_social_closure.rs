//! Integration tests closing the 17 skills, attributes, character, account,
//! fitting, inventory, and social parity rows (`feat:*`, plus `plan:CH-2` in `plan_char_dogma_social_esi.rs`).

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents, MailingListRecord,
    MarketOrderRecord, TrainedSkillRecord, WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_string_field, encode_varint_field, AuthoritativeContext, EvePublicRequest, GoogleAny,
    GoogleTimestamp, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::{cache_notifications::NotificationRecord, ServerContext};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

const JITA_SYS: u32 = 30_000_142;
const JITA_STN: i64 = 60_003_760;
const MAURASI_SYS: u32 = 30_000_140; // 0.65 security (< 0.75)

fn make_quasar_req(char_id: i64, user_id: i32, role_mask: u64, type_name: &str, value: Vec<u8>, now: u64) -> EvePublicRequest {
    EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: now as i64, nanos: 0 }),
        correlation_uuid: vec![1; 16],
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }),
        external_origin: "test".into(),
        application_instance_uuid: vec![2; 16],
        authoritative_context: Some(AuthoritativeContext::for_session_at(Some(user_id), Some(char_id as u32), None, role_mask, "tranquility", vec![3; 16], now)),
        trace_context: None,
    }
}

#[tokio::test]
async fn test_skills_alpha_5m_cap_direct_purchase_weekly_sp_and_history() {
    // parity: feat:skills.alpha-5m-training-limit, feat:skills.direct-purchase-inject, feat:skills.omega-weekly-free-sp, feat:skills.skill-history-events
    eve_testkit::attest(&[
        "feat:skills.alpha-5m-training-limit",
        "feat:skills.direct-purchase-inject",
        "feat:skills.omega-weekly-free-sp",
        "feat:skills.skill-history-events",
    ]);

    let db_path = std::env::temp_dir().join(format!("evomu_skills_clos_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
    let ctx = ServerContext::with_repository(repo);
    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    ctx.db.with_mut_snapshot(|snap| *snap = demo);
    common::pin_clock(&ctx);

    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = SessionState::new(9301);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");

    // 1. feat:skills.alpha-5m-training-limit:
    // Alpha clone with 4.9M trained SP + 200k unallocated SP (>= 5.0M total SP) cannot start passive training,
    // an active queue is paused at 5M, unallocated SP can still be applied, and Omega status removes the limit.
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 1; // Alpha
        ch.skill_points = 4_900_000.0;
        ch.free_skill_points = 200_000.0;
        snap.skills.entry(char_id).or_default().insert(3300, TrainedSkillRecord { type_id: 3300, level: 1, skill_points: 250.0 });
    });
    let q_err = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "SaveSkillQueue", &[PyRep::List(vec![PyRep::List(vec![PyRep::Integer(3300), PyRep::Integer(2)])])], &kw, None).await.expect_err("Alpha with trained + unallocated SP >= 5M cannot start passive skill queue");
    assert_eq!(q_err.code, "AlphaCloneSpCapExceeded");

    // Even when passive queue is blocked by unallocated SP, ApplyFreeSkillPoints still works
    let applied = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "ApplyFreeSkillPoints", &[PyRep::Integer(3300), PyRep::Integer(1_165)], &kw, None).await.expect("ApplyFreeSkillPoints allowed while trained SP < 5M");
    assert_eq!(applied, PyRep::Real(1_415.0));

    // Active Alpha queue running across the 5M boundary pauses automatically on tick
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.skill_points = 4_990_000.0;
        ch.free_skill_points = 0.0;
    });
    ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "SaveSkillQueue", &[PyRep::List(vec![PyRep::List(vec![PyRep::Integer(3300), PyRep::Integer(3)])])], &kw, None).await.expect("SaveSkillQueue below 5M total SP");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().skill_points = 4_999_995.0;
    });
    ctx.advance_secs(120);
    assert!(ctx.db.with_snapshot(|s| s.paused_skill_queues.contains(&char_id)), "Alpha skill queue must pause automatically once total SP reaches 5,000,000");

    // 2. feat:skills.omega-weekly-free-sp:
    // Refused for Alpha; granted once per week (15,000 SP) for Omega; refused on second claim in same week; refreshes after Tuesday 11:00 UTC downtime.
    let can_alpha = ctx.dispatch_rpc(&mut sess, "storeManager", None, "can_purchase_offer", &[PyRep::Integer(1005)], &kw, None).await.expect("can_purchase_offer Alpha");
    assert_eq!(can_alpha, PyRep::Bool(false));
    let buy_alpha_err = ctx.dispatch_rpc(&mut sess, "storeManager", None, "buy_offer", &[PyRep::Integer(1005)], &kw, None).await.expect_err("Alpha cannot claim Omega weekly free SP");
    assert_eq!(buy_alpha_err.code.to_ascii_lowercase(), "omegarequired");

    // Upgrade to Omega (clone_grade = 2) -> removes the 5M SP training limit and allows SaveSkillQueue & training completion above 5M SP!
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().clone_grade = 2;
        snap.paused_skill_queues.remove(&char_id);
    });
    ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "SaveSkillQueue", &[PyRep::List(vec![PyRep::List(vec![PyRep::Integer(3300), PyRep::Integer(3)])])], &kw, None).await.expect("Omega removes the 5M SP SaveSkillQueue limit");
    ctx.advance_secs(18_000);
    assert_eq!(ctx.db.with_snapshot(|s| s.skills[&char_id][&3300].level), 3, "Omega skill queue trains past 5M SP to completion");
    assert!(ctx.db.get_character(char_id).unwrap().skill_points > 5_000_000.0);

    let free_before = ctx.db.get_character(char_id).unwrap().free_skill_points;
    let claim1 = ctx.dispatch_rpc(&mut sess, "storeManager", None, "buy_offer", &[PyRep::Integer(1005)], &kw, None).await.expect("Omega claims weekly 15,000 free SP");
    assert_eq!(claim1.dict_get("freeSkillPointsGranted"), Some(&PyRep::Integer(15_000)));
    assert_eq!(ctx.db.get_character(char_id).unwrap().free_skill_points, free_before + 15_000.0);

    let claim_dup = ctx.dispatch_rpc(&mut sess, "storeManager", None, "buy_offer", &[PyRep::Integer(1005)], &kw, None).await.expect_err("Second weekly SP claim in same week must be refused");
    assert_eq!(claim_dup.code.to_ascii_lowercase(), "weeklyofferalreadyclaimed");

    // Advance 7 days past next Tuesday 11:00 UTC downtime -> weekly window refreshes
    ctx.advance_secs(7 * 86_400);
    let claim2 = ctx.dispatch_rpc(&mut sess, "storeManager", None, "buy_offer", &[PyRep::Integer(1005)], &kw, None).await.expect("Omega claims weekly 15,000 free SP after Tuesday 11:00 UTC reset");
    assert_eq!(claim2.dict_get("freeSkillPointsGranted"), Some(&PyRep::Integer(15_000)));

    // 3. feat:skills.direct-purchase-inject:
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().remove(&3386);
        snap.skills.entry(char_id).or_default().remove(&3410);
        snap.market_orders.retain(|_, o| o.type_id != 3386 && o.type_id != 3410);
        snap.market_orders.insert(999_111_222, MarketOrderRecord {
            order_id: 999_111_222, owner_id: 1_000_035, station_id: 60_004_588, region_id: 10_000_030,
            type_id: 3386, is_bid: false, price: IskCents::from_f64_isk(100_000.0), vol_entered: 100,
            vol_remaining: 100, min_volume: 1, is_npc: true, escrow: IskCents(0),
        });
    });
    let bal_before = ctx.db.get_character(char_id).unwrap().balance.to_f64_isk();
    let buy_skill = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "PurchaseSkills", &[PyRep::List(vec![PyRep::Integer(3386)])], &kw, None).await.expect("Direct purchase of NPC-seeded skill 3386 from any location");
    assert_eq!(buy_skill.dict_get("totalCostISK"), Some(&PyRep::Real(130_000.0)));
    let bal_after = ctx.db.get_character(char_id).unwrap().balance.to_f64_isk();
    assert!((bal_before - bal_after - 130_000.0).abs() < 1e-2, "Must charge 130% of NPC price (130,000 ISK)");
    assert_eq!(ctx.db.with_snapshot(|s| s.skills[&char_id][&3386].level), 0);

    let unseeded_err = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "PurchaseSkills", &[PyRep::List(vec![PyRep::Integer(3410)])], &kw, None).await.expect_err("Non-NPC-seeded skill must be refused for direct purchase");
    assert_eq!(unseeded_err.code, "SkillUnavailableForPurchase");

    // 4. feat:skills.skill-history-events:
    // Exercise injection (36), training completion (37), extraction (39), and removal (38) in chronological order, persisted across relog.
    ctx.advance_secs(60);
    let extractor_id = 991_001_050;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().skill_points = 6_000_000.0;
        snap.skills.entry(char_id).or_default().insert(3392, TrainedSkillRecord { type_id: 3392, level: 5, skill_points: 512_000.0 });
        snap.items.insert(extractor_id, InventoryItemRecord { item_id: extractor_id, type_id: 40519, owner_id: char_id, location_id: JITA_STN, flag: InventoryFlag::Hangar, quantity: 1, singleton: false, version: 1, ..Default::default() });
    });
    ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "ExtractSkills", &[PyRep::Dict(vec![(PyRep::Integer(3392), PyRep::Integer(500_000))]), PyRep::Integer(extractor_id)], &kw, None).await.expect("Extract 500k SP from 3392");

    ctx.advance_secs(60);
    let obs_skill_id = ctx.sde.types.values().find(|t| t.category_id == 16 && !t.published).map(|t| t.type_id).unwrap_or(999_999);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(obs_skill_id, TrainedSkillRecord { type_id: obs_skill_id, level: 1, skill_points: 500.0 });
    });
    let (removed_cnt, _) = eve_server::services::character_skill_ops::migrate_obsolete_skills_for_character(&ctx, char_id);
    assert_eq!(removed_cnt, 1);

    drop(ctx);
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = ServerContext::with_repository(repo2);
    let mut sess2 = SessionState::new(9302);
    common::login_pilot(&ctx2, &mut sess2, "demo", "demo-secret").await;
    ctx2.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.unwrap();
    let hist_rep = ctx2.dispatch_rpc(&mut sess2, "skillMgr2", None, "GetSkillHistory", &[PyRep::Integer(50)], &kw, None).await.expect("GetSkillHistory after relog");
    let PyRep::Rowset { rows, .. } = hist_rep else { panic!("expected Rowset") };
    assert!(rows.iter().any(|r| r[1] == PyRep::Integer(36) && r[2] == PyRep::Integer(3386)), "Skill injection event 36 must persist");
    assert!(rows.iter().any(|r| r[1] == PyRep::Integer(37) && r[2] == PyRep::Integer(3300)), "Skill training completion event 37 must persist");
    assert!(rows.iter().any(|r| r[1] == PyRep::Integer(39) && r[2] == PyRep::Integer(3392)), "Skill extraction event 39 must persist");
    assert!(rows.iter().any(|r| r[1] == PyRep::Integer(38) && r[2] == PyRep::Integer(obs_skill_id as i64)), "Skill removal event 38 must persist");
    assert!(rows.windows(2).all(|w| w[0][0].as_i64().unwrap() <= w[1][0].as_i64().unwrap()), "GetSkillHistory rows must be in chronological logDate order");
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_attributes_boosters_biology_contraband_remap_and_age_cohorts() {
    // parity: feat:attributes.booster-duration-biology, feat:attributes.booster-highsec-contraband, feat:attributes.remap-defaults-and-order, feat:character.character-age-cohorts
    eve_testkit::attest(&[
        "feat:attributes.booster-duration-biology",
        "feat:attributes.booster-highsec-contraband",
        "feat:attributes.remap-defaults-and-order",
        "feat:character.character-age-cohorts",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = SessionState::new(9303);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.unwrap();

    // 1. feat:attributes.remap-defaults-and-order:
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().neural = eve_db::NeuralAttributes::default();
    });
    let r_info = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetRespecInfo", &[], &kw, None).await.unwrap();
    assert_eq!(r_info.dict_get("bonusRemaps"), Some(&PyRep::Integer(2)));
    let def_ch = ctx.db.get_character(char_id).unwrap();
    assert_eq!(def_ch.neural.charisma, 19);
    assert_eq!(def_ch.neural.intelligence, 20);

    // Out-of-bounds (< 17 or > 27) and wrong sum (!= 99) rejected
    assert!(ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(16), PyRep::Integer(21), PyRep::Integer(21), PyRep::Integer(21), PyRep::Integer(20)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(19), PyRep::Integer(28), PyRep::Integer(18), PyRep::Integer(17), PyRep::Integer(17)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20)], &kw, None).await.is_err());

    // First remap consumes the normal timed remap (bonusRemaps remains 2!)
    let rm1 = ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(17), PyRep::Integer(27), PyRep::Integer(21), PyRep::Integer(17), PyRep::Integer(17)], &kw, None).await.unwrap();
    assert_eq!(rm1.dict_get("bonusRemaps"), Some(&PyRep::Integer(2)), "Timed remap must be consumed before bonus remaps");
    let rm2 = ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(17), PyRep::Integer(26), PyRep::Integer(22), PyRep::Integer(17), PyRep::Integer(17)], &kw, None).await.unwrap();
    assert_eq!(rm2.dict_get("bonusRemaps"), Some(&PyRep::Integer(1)));
    let rm3 = ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(17), PyRep::Integer(25), PyRep::Integer(23), PyRep::Integer(17), PyRep::Integer(17)], &kw, None).await.unwrap();
    assert_eq!(rm3.dict_get("bonusRemaps"), Some(&PyRep::Integer(0)));
    assert!(ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(17), PyRep::Integer(24), PyRep::Integer(24), PyRep::Integer(17), PyRep::Integer(17)], &kw, None).await.is_err());

    // Advance 365 days -> normal timed remap replenishes!
    ctx.advance_secs(365 * 86_400 + 10);
    let rm4 = ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(17), PyRep::Integer(27), PyRep::Integer(21), PyRep::Integer(17), PyRep::Integer(17)], &kw, None).await.expect("Normal timed remap replenishes after 365 days");
    assert_eq!(rm4.dict_get("intelligence"), Some(&PyRep::Integer(27)));

    // 2. feat:attributes.booster-duration-biology:
    let accel_item_id = 992_001_001;
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(3402, TrainedSkillRecord { type_id: 3402, level: 1, skill_points: 250.0 }); // Science I
        sk.insert(3405, TrainedSkillRecord { type_id: 3405, level: 5, skill_points: 256_000.0 }); // Biology V
        snap.items.insert(accel_item_id, InventoryItemRecord {
            item_id: accel_item_id, type_id: 49539, owner_id: char_id, location_id: JITA_STN,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: false, version: 1, ..Default::default()
        });
    });
    let attrs_before = ctx.db.with_snapshot(|s| eve_server::services::character_skills::effective_character_attributes(&ctx.sde, s, char_id));
    let use_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "UseBooster", &[PyRep::Integer(accel_item_id)], &kw, None).await.expect("UseBooster cerebral accelerator");
    let dur_secs = use_rep.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap_or(0);
    let base_dur_secs = (ctx.sde.types.get(&49539).and_then(|t| t.default_attributes.get(&330).copied()).unwrap_or(1_800_000.0) / 1000.0).round() as i64;
    assert_eq!(dur_secs, base_dur_secs * 2, "Biology V must double booster duration to 2.0x");
    let bonus_int = ctx.sde.types.get(&49539).and_then(|t| t.default_attributes.get(&176).or_else(|| t.default_attributes.get(&165)).copied()).unwrap_or(6.0);
    let attrs_active = ctx.db.with_snapshot(|s| eve_server::services::character_skills::effective_character_attributes(&ctx.sde, s, char_id));
    assert!((attrs_active.1 - (attrs_before.1 + bonus_int)).abs() < 1e-4, "Active cerebral accelerator must add attribute bonus");

    // Advance past 2x duration -> booster expires and attribute bonus is removed
    ctx.advance_secs((dur_secs as u64) + 10);
    let attrs_expired = ctx.db.with_snapshot(|s| eve_server::services::character_skills::effective_character_attributes(&ctx.sde, s, char_id));
    assert!((attrs_expired.1 - attrs_before.1).abs() < 1e-4, "Expired cerebral accelerator bonus must be removed");

    // 3. feat:attributes.booster-highsec-contraband:
    let _ = ctx.dispatch_rpc(&mut sess, "ship", None, "Undock", &[], &kw, None).await.expect("Undock in Jita");
    let ship_id = sess.ship_id.unwrap();
    let (contraband_id, legal_booster_id, active_booster_id) = (992_001_002, 992_001_003, 992_001_004);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(contraband_id, InventoryItemRecord { item_id: contraband_id, type_id: 9947, owner_id: char_id, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 2, singleton: false, version: 1, ..Default::default() });
        snap.items.insert(legal_booster_id, InventoryItemRecord { item_id: legal_booster_id, type_id: 2838, owner_id: char_id, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1, ..Default::default() });
        snap.items.insert(active_booster_id, InventoryItemRecord { item_id: active_booster_id, type_id: 9950, owner_id: char_id, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1, ..Default::default() });
    });
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "UseBooster", &[PyRep::Integer(active_booster_id)], &kw, None).await.expect("Consume 9950");
    let bal_pre_scan = ctx.db.get_character(char_id).unwrap().balance.0;
    let _ = eve_server::services::combat_crimewatch::combat_npc_police::check_and_spawn_faction_police(&ctx, JITA_SYS).await;
    assert!(ctx.db.get_item(contraband_id).is_none(), "Unconsumed contraband booster 9947 in cargo must be confiscated in high-sec");
    assert!(ctx.db.get_item(legal_booster_id).is_some(), "Legal booster 2838 in cargo must not be confiscated");
    assert!(ctx.db.get_character(char_id).unwrap().balance.0 < bal_pre_scan, "Customs fine must be deducted from wallet");
    let active_boosters = ctx.dispatch_rpc(&mut sess, "skillMgr2", None, "GetBoosters", &[], &kw, None).await.unwrap();
    let PyRep::Dict(ab_dict) = active_boosters else { panic!("expected Dict") };
    assert!(!ab_dict.is_empty(), "Already-consumed active booster must remain active in high-sec");

    // 4. feat:character.character-age-cohorts:
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().create_date_unix = now_u - 5 * 86_400; // 5 days old (< 30d)
    });
    let char_cohorts_new = ctx.dispatch_rpc(&mut sess, "charMgr", None, "GetCohortsForCharacter", &[PyRep::Integer(char_id)], &kw, None).await.unwrap();
    assert!(format!("{char_cohorts_new:?}").contains("cohort_new_player"));
    let user_cohorts_new = ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "GetCohortsForUser", &[PyRep::Integer(1)], &kw, None).await.unwrap();
    assert!(format!("{user_cohorts_new:?}").contains("cohort_new_player"));

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().create_date_unix = now_u - 100 * 86_400; // 100 days old (> 30d)
    });
    let char_cohorts_vet = ctx.dispatch_rpc(&mut sess, "charMgr", None, "GetCohortsForCharacter", &[PyRep::Integer(char_id)], &kw, None).await.unwrap();
    assert!(format!("{char_cohorts_vet:?}").contains("cohort_veteran"));
}

#[tokio::test]
async fn test_account_character_transfer_csat_surveys_steam_epic_and_twitch_drops() {
    // parity: feat:account.character-transfer, feat:account.csat-surveys, feat:account.steam-epic-accounts, feat:account.twitch-drops
    eve_testkit::attest(&[
        "feat:account.character-transfer",
        "feat:account.csat-surveys",
        "feat:account.steam-epic-accounts",
        "feat:account.twitch-drops",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = SessionState::new(9304);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.unwrap();
    let now = ctx.now_unix();
    let gm_role = eve_machonet::service_roles::ROLE_GMH;

    // 1. feat:account.csat-surveys:
    let _ = eve_server::services::operations_manager::handle_survey_slash(&ctx, char_id, "/survey schedule 1001 86400 34:100");
    let _ = ctx.dispatch_rpc(&mut sess, "survey", None, "PerformSurveyChecks", &[], &kw, None).await.unwrap();
    let active_survey = ctx.dispatch_rpc(&mut sess, "survey", None, "GetActiveSurveyID", &[], &kw, None).await.unwrap();
    assert_eq!(active_survey, PyRep::Integer(1001));
    let _ = eve_server::services::operations_manager::mark_survey_completed(&ctx, char_id, 1001);
    let claim_survey = ctx.dispatch_rpc(&mut sess, "survey", None, "ClaimSurveyRewards", &[PyRep::Integer(1001)], &kw, None).await.expect("ClaimSurveyRewards first time");
    assert_eq!(claim_survey, PyRep::Bool(true));
    let dup_survey = ctx.dispatch_rpc(&mut sess, "survey", None, "ClaimSurveyRewards", &[PyRep::Integer(1001)], &kw, None).await.expect_err("Duplicate survey claim must fail");
    assert_eq!(dup_survey.code, "SurveyAlreadyClaimed");

    // 2. feat:account.steam-epic-accounts:
    // Unprivileged callers cannot invoke admin/vendor entitlement delivery RPCs
    let mut epic_deliv_buf = Vec::new();
    encode_string_field(91, "epic_user_99", &mut epic_deliv_buf);
    encode_string_field(92, "epic_pack_001", &mut epic_deliv_buf);
    encode_varint_field(93, 44992, &mut epic_deliv_buf); // PLEX
    encode_varint_field(94, 50, &mut epic_deliv_buf);
    let unpriv_epic = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, 0, "eve.vendors.epic.api.DeliverEntitlementRequest", epic_deliv_buf.clone(), now), None, false, now);
    assert_ne!(unpriv_epic.status_code, STATUS_OK, "Unprivileged client must not mint Epic entitlements");

    sess.role |= gm_role;
    let mut steam_link_buf = Vec::new();
    encode_string_field(91, "steam_76561198000000001", &mut steam_link_buf);
    encode_varint_field(92, 1, &mut steam_link_buf);
    let sl_rsp = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, gm_role, "eve.vendors.steam.user.api.admin.LinkRequest", steam_link_buf, now), None, false, now);
    assert_eq!(sl_rsp.status_code, STATUS_OK);

    let mut steam_auth_buf = Vec::new();
    encode_string_field(91, "steam_76561198000000001", &mut steam_auth_buf);
    let sa_rsp = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, 0, "eve_public.auth.steam.AuthenticateSteamRequest", steam_auth_buf, now), None, false, now);
    assert_eq!(sa_rsp.status_code, STATUS_OK);

    let ed_rsp = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, gm_role, "eve.vendors.epic.api.DeliverEntitlementRequest", epic_deliv_buf, now), None, false, now);
    assert_eq!(ed_rsp.status_code, STATUS_OK);

    // 3. feat:account.twitch-drops:
    let mut tw_drop_buf = Vec::new();
    encode_string_field(91, "twitch_streamer_42", &mut tw_drop_buf);
    encode_string_field(92, "drop_campaign_2026_01", &mut tw_drop_buf);
    encode_varint_field(93, 34, &mut tw_drop_buf);
    encode_varint_field(94, 1000, &mut tw_drop_buf);
    let unlinked_drop = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, gm_role, "eve.vendors.twitch.entitlement.api.DropReceived", tw_drop_buf.clone(), now), None, false, now);
    assert_ne!(unlinked_drop.status_code, STATUS_OK, "Unlinked Twitch account drop must fail");

    let mut tw_link_buf = Vec::new();
    encode_string_field(91, "twitch_streamer_42", &mut tw_link_buf);
    encode_varint_field(92, 1, &mut tw_link_buf);
    let tl_rsp = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, 0, "eve.id.federate.twitch.api.CompleteLinkingRequest", tw_link_buf, now), None, false, now);
    assert_eq!(tl_rsp.status_code, STATUS_OK);

    let linked_drop = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, gm_role, "eve.vendors.twitch.entitlement.api.DropReceived", tw_drop_buf.clone(), now), None, false, now);
    assert_eq!(linked_drop.status_code, STATUS_OK);
    let dup_drop = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, gm_role, "eve.vendors.twitch.entitlement.api.DropReceived", tw_drop_buf, now), None, false, now);
    assert_ne!(dup_drop.status_code, STATUS_OK, "Duplicate Twitch drop ID must be rejected");

    let tokens = ctx.dispatch_rpc(&mut sess, "userSvc", None, "GetRedeemTokens", &[], &kw, None).await.unwrap();
    let PyRep::List(tok_list) = tokens else { panic!("expected List") };
    assert!(tok_list.len() >= 2, "Redeem queue must contain delivered Epic and Twitch rewards");

    // 4. feat:account.character-transfer:
    // Rejects wrong-owner caller, player corp, active industry job, and unpaid fee; succeeds when paid (1,000 PLEX), preserves assets, and enforces 10-day cooldown.
    sess.role = 0;
    let asset_id = 994_001_001;
    let mut xfer_buf = Vec::new();
    encode_varint_field(91, char_id as u64, &mut xfer_buf);
    encode_varint_field(92, 2, &mut xfer_buf); // destination user_id = 2

    // Wrong-owner caller (user_id = 2 trying to steal user_id = 1's character) is rejected
    let wrong_owner = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 2, 0, "eve.user.character.transfer.QueueRequest", xfer_buf.clone(), now), None, false, now);
    assert_ne!(wrong_owner.status_code, STATUS_OK, "Non-owner cannot transfer another account's character");

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().corporation_id = 98_000_001; // player corp
        snap.items.insert(asset_id, InventoryItemRecord { item_id: asset_id, type_id: 34, owner_id: char_id, location_id: JITA_STN, flag: InventoryFlag::Hangar, quantity: 500, singleton: false, version: 1, ..Default::default() });
    });
    let corp_err = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, 0, "eve.user.character.transfer.QueueRequest", xfer_buf.clone(), now), None, false, now);
    assert_ne!(corp_err.status_code, STATUS_OK, "Character in player corp cannot be transferred");

    // Move to NPC corp, but seed an active industry job -> blocked!
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().corporation_id = 1_000_009; // NPC corp
        snap.industry_jobs.insert(9001, IndustryJobRecord { job_id: 9001, installer_id: char_id, facility_id: JITA_STN, blueprint_item_id: 1, blueprint_type_id: 687, activity_id: 1, runs: 1, product_type_id: 587, status: "Active".into(), duration_seconds: 3600, start_unix: now });
    });
    let job_err = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, 0, "eve.user.character.transfer.QueueRequest", xfer_buf.clone(), now), None, false, now);
    assert_ne!(job_err.status_code, STATUS_OK, "Active industry job must block character transfer");

    // Clear industry job and fund 1,000 PLEX transfer fee -> succeeds and preserves character assets!
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.clear();
        snap.account_plex_vault.insert(1, 1_000);
    });
    let xfer_ok = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 1, 0, "eve.user.character.transfer.QueueRequest", xfer_buf, now), None, false, now);
    assert_eq!(xfer_ok.status_code, STATUS_OK);
    assert_eq!(ctx.db.get_character(char_id).unwrap().user_id, 2, "Character must now belong to destination account 2");
    assert_eq!(ctx.db.with_snapshot(|s| s.account_plex_vault.get(&1).copied().unwrap_or(0)), 0, "1,000 PLEX transfer fee must be consumed");
    assert_eq!(ctx.db.get_item(asset_id).unwrap().owner_id, char_id, "Character assets must remain owned by the transferred character");

    // Immediate transfer back within 10-day cooldown must fail
    sess.user_id = Some(2);
    ctx.db.with_mut_snapshot(|snap| { snap.account_plex_vault.insert(2, 1_000); });
    let mut xfer_back = Vec::new();
    encode_varint_field(91, char_id as u64, &mut xfer_back);
    encode_varint_field(92, 1, &mut xfer_back);
    let cd_err = ctx.quasar_gateway.handle_public_request(&sess, &make_quasar_req(char_id, 2, 0, "eve.user.character.transfer.QueueRequest", xfer_back, now + 3600), None, false, now + 3600);
    assert_ne!(cd_err.status_code, STATUS_OK, "10-day character transfer cooldown must be enforced");
}

#[tokio::test]
async fn test_strip_fitting_jetcan_capacity_despawn_and_secure_container_anchoring() {
    // parity: feat:fitting.strip-fitting, feat:inventory.jetcan-capacity-and-despawn, feat:inventory.secure-container-anchoring
    eve_testkit::attest(&[
        "feat:fitting.strip-fitting",
        "feat:inventory.jetcan-capacity-and-despawn",
        "feat:inventory.secure-container-anchoring",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let (c1, c2) = (90_000_001, 90_000_002);
    let mut s1 = SessionState::new(9305);
    common::login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut s1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(c1)], &kw, None).await.unwrap();
    let ship_id = s1.ship_id.unwrap_or(100_000_001);

    // 1. feat:fitting.strip-fitting:
    // Seed fitted module (MedSlot0 = 19), loaded charge (HiSlot0 = 27), drone in DroneBay (87), Rig (RigSlot0 = 92), and T3 Subsystem (SubSystemSlot0 = 125).
    let (mod_id, charge_id, drone_id, rig_id, subsys_id) = (993_001_001, 993_001_002, 993_001_003, 993_001_004, 993_001_005);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mod_id, InventoryItemRecord { item_id: mod_id, type_id: 434, owner_id: c1, location_id: ship_id, flag: InventoryFlag::MedSlot0, quantity: 1, singleton: true, version: 1, ..Default::default() });
        snap.items.insert(charge_id, InventoryItemRecord { item_id: charge_id, type_id: 185, owner_id: c1, location_id: ship_id, flag: InventoryFlag::HiSlot0, quantity: 100, singleton: false, version: 1, ..Default::default() });
        snap.items.insert(drone_id, InventoryItemRecord { item_id: drone_id, type_id: 2454, owner_id: c1, location_id: ship_id, flag: InventoryFlag::DroneBay, quantity: 2, singleton: false, version: 1, ..Default::default() });
        snap.items.insert(rig_id, InventoryItemRecord { item_id: rig_id, type_id: 31716, owner_id: c1, location_id: ship_id, flag: InventoryFlag::RigSlot0, quantity: 1, singleton: true, version: 1, ..Default::default() });
        snap.items.insert(subsys_id, InventoryItemRecord { item_id: subsys_id, type_id: 45603, owner_id: c1, location_id: ship_id, flag: InventoryFlag::SubSystemSlot0, quantity: 1, singleton: true, version: 1, ..Default::default() });
    });
    let _ = ctx.drain_notifications_for_char(c1);
    let strip_rep = ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "StripFitting", &[PyRep::Integer(ship_id)], &kw, None).await.expect("StripFitting docked");
    assert_eq!(strip_rep.dict_get("shipID"), Some(&PyRep::Integer(ship_id)));
    assert_eq!(ctx.db.get_item(mod_id).unwrap().flag, InventoryFlag::Hangar);
    assert_eq!(ctx.db.get_item(charge_id).unwrap().flag, InventoryFlag::Hangar);
    assert_eq!(ctx.db.get_item(drone_id).unwrap().flag, InventoryFlag::Hangar);
    assert_eq!(ctx.db.get_item(rig_id).unwrap().flag, InventoryFlag::RigSlot0, "Rigs must remain fitted during StripFitting");
    assert_eq!(ctx.db.get_item(subsys_id).unwrap().flag, InventoryFlag::SubSystemSlot0, "T3 Subsystems must remain fitted during StripFitting");
    let strip_notifs = ctx.drain_notifications_for_char(c1);
    assert!(strip_notifs.iter().any(|n| n.event == "OnItemsChanged"), "StripFitting must emit batch OnItemsChanged");
    assert!(strip_notifs.iter().any(|n| n.event == "OnModuleUnfitted"), "StripFitting must emit OnModuleUnfitted notifications");

    // Undock -> StripFitting in space must be rejected
    ctx.dispatch_rpc(&mut s1, "ship", None, "Undock", &[], &kw, None).await.unwrap();
    let strip_space_err = ctx.dispatch_rpc(&mut s1, "dogmaIM", None, "StripFitting", &[PyRep::Integer(ship_id)], &kw, None).await.expect_err("StripFitting in space must fail");
    assert_eq!(strip_space_err.code, "CannotStripFittingInSpace");

    // 2. feat:inventory.jetcan-capacity-and-despawn:
    // Jettisoning creates a 55,000 m³ jetcan, refuses scooping into a hold, despawns as soon as emptied, and otherwise despawns when its 2h (7,200s) V24 lifetime ends.
    let (ore_40k, ore_60k, ore_lifetime) = (993_001_010, 993_001_011, 993_001_012);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) { sh.type_id = 32880; } // Venture
        snap.items.insert(ore_40k, InventoryItemRecord { item_id: ore_40k, type_id: 34, owner_id: c1, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 4_000_000, singleton: false, version: 1, ..Default::default() });
        snap.items.insert(ore_60k, InventoryItemRecord { item_id: ore_60k, type_id: 34, owner_id: c1, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 6_000_000, singleton: false, version: 1, ..Default::default() });
        snap.items.insert(ore_lifetime, InventoryItemRecord { item_id: ore_lifetime, type_id: 34, owner_id: c1, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 100, singleton: false, version: 1, ..Default::default() });
    });
    let over_cap = ctx.dispatch_rpc(&mut s1, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(ore_60k)])], &kw, None).await.expect_err("60,000 m³ > 55,000 m³ jetcan capacity must fail");
    assert_eq!(over_cap.code, "CapacityExceeded");

    let jet_ok = ctx.dispatch_rpc(&mut s1, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(ore_40k)])], &kw, None).await.expect("40,000 m³ <= 55,000 m³ jetcan capacity must succeed");
    let PyRep::Tuple(jt) = jet_ok else { panic!("expected Tuple") };
    let jetcan_id = match &jt[0] { PyRep::List(l) => l[0].as_i64().unwrap(), _ => panic!("expected List") };
    assert!(ctx.db.get_item(jetcan_id).is_some());

    // Jetcan can never be scooped into a hold
    assert!(ctx.dispatch_rpc(&mut s1, "ship", None, "Scoop", &[PyRep::Integer(jetcan_id)], &kw, None).await.is_err(), "Jettison container must never be scooped into a hold");

    // Emptying the jetcan despawns it immediately!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&ore_60k);
        if let Some(it) = snap.items.get_mut(&ore_40k) { it.quantity = 100; }
    });
    ctx.dispatch_rpc(&mut s1, "invbroker", None, "Add", &[PyRep::Integer(ore_40k), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &kw, None).await.expect("Loot last item from jetcan");
    assert!(ctx.db.get_item(jetcan_id).is_none(), "Emptied jetcan must despawn immediately");

    // Non-empty jetcan despawns when its 2-hour (7,200s) V24 lifetime expires
    ctx.advance_secs(181);
    let jet2 = ctx.dispatch_rpc(&mut s1, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(ore_lifetime)])], &kw, None).await.expect("Second Jettison after cooldown");
    let PyRep::Tuple(jt2) = jet2 else { panic!("expected Tuple") };
    let jetcan2_id = match &jt2[0] { PyRep::List(l) => l[0].as_i64().unwrap(), _ => panic!("expected List") };
    assert!(ctx.db.get_item(jetcan2_id).is_some());
    ctx.advance_secs(7_205);
    assert!(ctx.db.get_item(jetcan2_id).is_none(), "Jetcan must despawn when its V24 lifetime ends");

    // 3. feat:inventory.secure-container-anchoring:
    let sec_cont_id = 993_001_020;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(c1).or_default().remove(&11584);
        snap.items.insert(sec_cont_id, InventoryItemRecord { item_id: sec_cont_id, type_id: 3467, owner_id: c1, location_id: ship_id, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, version: 1, ..Default::default() });
    });
    let no_skill = ctx.dispatch_rpc(&mut s1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(sec_cont_id)], &kw, None).await.expect_err("Missing Anchoring I must fail");
    assert_eq!(no_skill.code, "SkillMissing");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(c1).or_default().insert(11584, TrainedSkillRecord { type_id: 11584, level: 1, skill_points: 750.0 });
    });
    let hs08 = ctx.dispatch_rpc(&mut s1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(sec_cont_id)], &kw, None).await.expect_err("Anchoring secure container in Jita (0.9) must fail");
    assert_eq!(hs08.code, "CantAnchorInHighSec08");

    // Move to a <= 0.7 high-sec system and anchor
    let hs06_sys = ctx.sde.solar_systems.values().find(|s| s.security_status >= 0.50 && s.security_status < 0.74).map(|s| s.solar_system_id).unwrap_or(MAURASI_SYS);
    s1.solar_system_id = Some(hs06_sys);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&c1).unwrap().solar_system_id = hs06_sys;
        snap.items.get_mut(&ship_id).unwrap().location_id = hs06_sys as i64;
    });
    let maurasi = ctx.systems.get_or_spawn(hs06_sys);
    let _ = maurasi.spawn_ball_with_combat(Ball { id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.5, mass: 1_200_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_type_combat_state(587)).await;
    ctx.dispatch_rpc(&mut s1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(sec_cont_id)], &kw, None).await.expect("Anchor secure container in <= 0.7 system");

    // Anchored secure containers cannot be found with probes
    let scan_res = ctx.dispatch_rpc(&mut s1, "scanMgr", None, "RequestScans", &[PyRep::Real(149_597_870_700.0), PyRep::Real(100.0)], &kw, None).await.expect("RequestScans");
    assert!(!format!("{scan_res:?}").contains(&sec_cont_id.to_string()), "Anchored secure container must not be found with probes");

    // Advance 20 days, open container -> 30-day decay timer resets so advancing another 20 days (40 days total) keeps it alive!
    ctx.advance_secs(20 * 86_400);
    ctx.dispatch_rpc(&mut s1, "invbroker", None, "ValidateItemCanBeOpened", &[PyRep::Integer(sec_cont_id)], &kw, None).await.expect("Open anchored container");
    ctx.advance_secs(20 * 86_400);
    assert!(ctx.db.get_item(sec_cont_id).is_some(), "Opening anchored container must reset its 30-day inactivity decay timer");

    // Second pilot (c2) attacks c1's anchored secure container in <= 0.7 high-sec -> Criminal flag + CONCORD response
    let mut s2 = SessionState::new(9306);
    common::login_pilot(&ctx, &mut s2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut s2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(c2)], &kw, None).await.unwrap();
    let (ship2, gun2, ammo2) = (993_001_030, 993_001_031, 993_001_032);
    s2.solar_system_id = Some(hs06_sys);
    s2.station_id = None;
    s2.ship_id = Some(ship2);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&c2).unwrap().solar_system_id = hs06_sys;
        snap.characters.get_mut(&c2).unwrap().station_id = None;
        snap.characters.get_mut(&c2).unwrap().active_ship_id = ship2;
        snap.items.insert(ship2, InventoryItemRecord { item_id: ship2, type_id: 587, owner_id: c2, location_id: hs06_sys as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default() });
        snap.items.insert(gun2, InventoryItemRecord { item_id: gun2, type_id: 2873, owner_id: c2, location_id: ship2, flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default() });
        snap.items.insert(ammo2, InventoryItemRecord { item_id: ammo2, type_id: 185, owner_id: c2, location_id: ship2, flag: InventoryFlag::HiSlot0, quantity: 100, singleton: false, version: 1, ..Default::default() });
        snap.module_online_states.insert(gun2, true);
        snap.module_active_charges.insert(gun2, 185);
    });
    let _ = maurasi.spawn_ball_with_combat(Ball { id: ship2, type_id: 587, position: DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.5, mass: 1_200_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_type_combat_state(587)).await;
    let _ = maurasi.add_target_lock(ship2, eve_destiny::TargetLock { target_id: sec_cont_id, lock_time_ms: 1000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
    ctx.dispatch_rpc(&mut s2, "dogmaIM", None, "Activate", &[PyRep::Integer(gun2), PyRep::Integer(sec_cont_id)], &kw, None).await.expect("Attack anchored secure container in high-sec");
    let is_crim = ctx.crimewatch.read().unwrap().states.get(&c2).is_some_and(|st| st.is_criminal(ctx.now_unix()) && st.pending_concord_arrival.is_some());
    assert!(is_crim, "Attacking another pilot's anchored secure container in high-sec must trigger Criminal flag and CONCORD response");
}

#[tokio::test]
async fn test_social_hyperlinks_mailbox_limits_and_wormhole_delayed_local() {
    // parity: feat:social.mail-mailbox-limits, feat:social.wormhole-local-chat-delayed
    eve_testkit::attest(&[
        "feat:social.mail-mailbox-limits",
        "feat:social.wormhole-local-chat-delayed",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let (c1, c2) = (90_000_001, 90_000_002);
    let mut s1 = SessionState::new(9307);
    common::login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut s1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(c1)], &kw, None).await.unwrap();
    let mut s2 = SessionState::new(9308);
    common::login_pilot(&ctx, &mut s2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut s2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(c2)], &kw, None).await.unwrap();

    // 1. feat:social.mail-mailbox-limits:
    // (a) Mail body length > 8,000 characters is rejected with MailBodyTooLong.
    let long_body = "x".repeat(8_001);
    let body_err = ctx.dispatch_rpc(&mut s1, "mailMgr", None, "SendMail", &[PyRep::List(vec![PyRep::Integer(c2)]), PyRep::String("TooLong".into()), PyRep::String(long_body)], &kw, None).await.expect_err("Mail body > 8,000 chars must fail");
    assert_eq!(body_err.code, "MailBodyTooLong");

    // (b) Seed 3,000 mails for c2 (with mail #1 in a mailing list and mail #2 in Trash).
    // When mail #3,001 arrives, the newest 3,000 mails (2..=3,001) are kept and the oldest mail (#1) is hidden until Trash is emptied!
    ctx.db.with_mut_snapshot(|snap| {
        snap.mails.clear();
        snap.mail_timestamps.clear();
        let mut ml = MailingListRecord {
            list_id: 777, name: "TestList".into(), owner_id: c1, default_access: 1, default_role: 1, cost: 0,
            members: HashMap::new(), entity_access: Default::default(), mail_ids: [1].into_iter().collect(), welcome_mail: None,
        };
        ml.members.insert(c2, 1);
        snap.mailing_lists.insert(777, ml);
        for i in 1..=3_000 {
            snap.mails.push(eve_db::MailRecord { message_id: i, sender_id: c1, recipient_id: if i == 1 { 777 } else { c2 }, subject: format!("Mail {i}"), body: "Body".into() });
            snap.mail_timestamps.insert(i, 1_000_000 + (i as u64));
        }
    });
    ctx.dispatch_rpc(&mut s2, "mailMgr", None, "MoveToTrash", &[PyRep::List(vec![PyRep::Integer(2)])], &kw, None).await.unwrap();
    let m3001 = ctx.dispatch_rpc(&mut s1, "mailMgr", None, "SendMail", &[PyRep::List(vec![PyRep::Integer(c2)]), PyRep::String("Mail 3001".into()), PyRep::String("Newest".into())], &kw, None).await.expect("3,001st mail must succeed");
    let m3001_id = m3001.as_i64().unwrap();

    let hdrs_before = ctx.dispatch_rpc(&mut s2, "mailMgr", None, "GetMailHeaders", &[], &kw, None).await.unwrap();
    let PyRep::Rowset { rows: h_rows_before, .. } = hdrs_before else { panic!("expected Rowset") };
    let listed_before: Vec<i64> = h_rows_before.iter().map(|r| r[0].as_i64().unwrap()).collect();
    assert!(listed_before.contains(&m3001_id), "Newest 3,001st mail must be listed");
    assert!(!listed_before.contains(&1), "Oldest mail #1 must be hidden beyond the 3,000-mail window (counting mailing-list & Trash mails)");

    // EmptyTrash permanently deletes mail #2 -> oldest mail #1 re-enters the 3,000-mail window and becomes listed!
    ctx.dispatch_rpc(&mut s2, "mailMgr", None, "EmptyTrash", &[], &kw, None).await.unwrap();
    let hdrs_after = ctx.dispatch_rpc(&mut s2, "mailMgr", None, "GetMailHeaders", &[], &kw, None).await.unwrap();
    let PyRep::Rowset { rows: h_rows_after, .. } = hdrs_after else { panic!("expected Rowset") };
    let listed_after: Vec<i64> = h_rows_after.iter().map(|r| r[0].as_i64().unwrap()).collect();
    assert!(listed_after.contains(&1), "Oldest mail #1 must become listed once a mail is permanently deleted");

    // (c) Notifications are capped at 500.
    {
        let mut n_recs = ctx.notification_records.write().unwrap();
        for i in 1..=510 {
            n_recs.push(NotificationRecord { notification_id: 800_000 + i, type_id: 10, sender_id: c1, receiver_id: c2, processed: false, created: 132_000_000_000_000_000 + i, data: "{}".into(), deleted: false, group_id: 1, interacted: false });
        }
    }
    let notifs = ctx.dispatch_rpc(&mut s2, "notificationMgr", None, "GetAllNotifications", &[], &kw, None).await.unwrap();
    let PyRep::ObjectEx { list: n_rows, .. } = notifs else { panic!("expected CRowset ObjectEx") };
    assert_eq!(n_rows.len(), 500, "Notifications returned by notificationMgr must be capped at 500");

    // 2. feat:social.wormhole-local-chat-delayed:
    let wh_chan = "solarsystemid2:31000001".to_string();
    s1.solar_system_id = Some(31_000_001);
    s2.solar_system_id = Some(31_000_001);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&c1).unwrap().solar_system_id = 31_000_001;
        snap.characters.get_mut(&c2).unwrap().solar_system_id = 31_000_001;
    });
    ctx.dispatch_rpc(&mut s1, "LSC", None, "JoinChannel", &[PyRep::String(wh_chan.clone())], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut s2, "LSC", None, "JoinChannel", &[PyRep::String(wh_chan.clone())], &kw, None).await.unwrap();

    let m_init = ctx.dispatch_rpc(&mut s2, "LSC", None, "GetMembers", &[PyRep::String(wh_chan.clone())], &kw, None).await.unwrap();
    let Some(PyRep::Rowset { rows: r0, .. }) = m_init.dict_get("members") else { panic!("expected Rowset") };
    assert!(r0.is_empty(), "No pilot is visible in wormhole Local before speaking");

    ctx.dispatch_rpc(&mut s1, "LSC", None, "SendMessage", &[PyRep::String(wh_chan.clone()), PyRep::String("Hello WH".into())], &kw, None).await.unwrap();
    let m_spoke = ctx.dispatch_rpc(&mut s2, "LSC", None, "GetMembers", &[PyRep::String(wh_chan.clone())], &kw, None).await.unwrap();
    let Some(PyRep::Rowset { rows: r1, .. }) = m_spoke.dict_get("members") else { panic!("expected Rowset") };
    assert_eq!(r1.len(), 1);
    assert_eq!(r1[0][0], PyRep::Integer(c1));

    // Speaker c1 leaves wormhole while witness c2 is still in the system -> c1 remains listed!
    ctx.dispatch_rpc(&mut s1, "LSC", None, "LeaveChannel", &[PyRep::String(wh_chan.clone())], &kw, None).await.unwrap();
    let m_c1_left = ctx.dispatch_rpc(&mut s2, "LSC", None, "GetMembers", &[PyRep::String(wh_chan.clone())], &kw, None).await.unwrap();
    let Some(PyRep::Rowset { rows: r2, .. }) = m_c1_left.dict_get("members") else { panic!("expected Rowset") };
    assert_eq!(r2.len(), 1, "Speaker c1 must remain listed while witness c2 is still in the wormhole");

    // Witness c2 also leaves wormhole -> no witnesses remain, so c1 is pruned!
    ctx.dispatch_rpc(&mut s2, "LSC", None, "LeaveChannel", &[PyRep::String(wh_chan.clone())], &kw, None).await.unwrap();
    let m_all_left = ctx.dispatch_rpc(&mut s2, "LSC", None, "GetMembers", &[PyRep::String(wh_chan)], &kw, None).await.unwrap();
    let Some(PyRep::Rowset { rows: r3, .. }) = m_all_left.dict_get("members") else { panic!("expected Rowset") };
    assert!(r3.is_empty(), "Speaker c1 must be removed once all witnesses have left the wormhole");
}
