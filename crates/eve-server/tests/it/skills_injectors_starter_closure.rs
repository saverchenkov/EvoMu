//! Client-RPC integration tests closing Starter Skills, Skillbook Market/Direct Injection,
//! and Large/Small/Daily-Alpha/Non-Diminishing Skill Injectors:
//! - `feat:character.starter-skills`
//! - `feat:skills.inject-skillbook`
//! - `feat:skills.direct-purchase-inject`
//! - `feat:skills.skill-injector-large`
//! - `feat:skills.skill-injector-small`
//! - `feat:skills.alpha-injector`
//! - `feat:skills.nondiminishing-injector`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::formulas::sp_for_skill_level;
use eve_server::router::ServerContext;

use crate::common;

const JITA_SYS: u32 = 30_000_142;
const JITA_STN: i64 = 60_003_760;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, m: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, m, args, &HashMap::new(), None).await
}

async fn login_and_select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    rpc(ctx, &mut s, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
    s
}

fn doll_pair() -> (PyRep, PyRep) {
    (
        PyRep::keyval([("costume", PyRep::Integer(301)), ("gender", PyRep::Integer(1)), ("hair", PyRep::Integer(101)), ("skin", PyRep::Integer(201))]),
        PyRep::keyval([("background", PyRep::Integer(42)), ("camera", PyRep::Integer(1)), ("pose", PyRep::Integer(7))]),
    )
}

async fn create_pilot_in_jita(ctx: &ServerContext, sid: u64, user: &str, pass: &str, name: &str, bl: i64, anc: i64, school: i64) -> (SessionState, i64) {
    ctx.db.provision_player_account(user, pass);
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    let (doll, portrait) = doll_pair();
    let cid = rpc(
        ctx, &mut sess, "charUnboundMgr", "CreateCharacterWithDoll",
        &[PyRep::String(name.into()), PyRep::Integer(bl), PyRep::Integer(1), PyRep::Integer(anc), doll, portrait, PyRep::Integer(school), PyRep::None, PyRep::Integer(JITA_SYS as i64)],
    ).await.expect("CreateCharacterWithDoll").as_i64().expect("char_id");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(cid)]).await.expect("SelectCharacterID");
    (sess, cid)
}

fn place_hangar_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, qty: i32) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(item_id, InventoryItemRecord {
            item_id, type_id, owner_id, location_id: JITA_STN, flag: InventoryFlag::Hangar,
            quantity: qty, singleton: false, version: 1, ..Default::default()
        });
    });
}

async fn upgrade_to_omega_via_nes(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, plex_item_id: i64) {
    place_hangar_item(ctx, plex_item_id, 44_992, char_id, 500);
    rpc(ctx, sess, "invbroker", "DepositPlexToVault", &[PyRep::Integer(JITA_STN), PyRep::Integer(plex_item_id), PyRep::Integer(500), PyRep::Integer(0)]).await.expect("DepositPlexToVault");
    rpc(ctx, sess, "storeManager", "buy_offer", &[PyRep::Integer(1001)]).await.expect("buy_offer 1001");
}

#[tokio::test]
async fn test_starter_skills_all_empires_and_validation() {
    // parity: feat:character.starter-skills, rpc:charUnboundMgr.CreateCharacterWithDoll
    eve_testkit::attest(&["feat:character.starter-skills"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // Verify all 4 EVE empire lineages receive exact SDE racial starter skills, school corps, and rookie corvettes:
    // (race_id, bloodline_id, ancestry_id, school_id, expected_corp_id, expected_corvette_id, racial_frigate_skill_id)
    let empires = [
        (1u32, 1i64, 11i64, 17i64, 1_000_167i64, 601u32, 3330u32, "Caldari Cadet"),
        (2, 4, 22, 14, 1_000_170, 588, 3329, "Minmatar Cadet"),
        (4, 5, 1, 11, 1_000_166, 596, 3331, "Amarr Cadet"),
        (8, 7, 13, 20, 1_000_168, 606, 3328, "Gallente Cadet"),
    ];

    for (idx, (race_id, bl, anc, school, exp_corp, exp_corvette, frigate_sk, name)) in empires.into_iter().enumerate() {
        let user = format!("emp_user_{idx}");
        let (mut sess, cid) = create_pilot_in_jita(&ctx, 2100 + idx as u64, &user, "secret", name, bl, anc, school).await;

        let info = rpc(&ctx, &mut sess, "charUnboundMgr", "GetCharacterInfo", &[PyRep::Integer(cid)]).await.expect("GetCharacterInfo");
        assert_eq!(info.dict_get("corporationID"), Some(&PyRep::Integer(exp_corp)));
        assert_eq!(info.dict_get("raceID"), Some(&PyRep::Integer(race_id as i64)));

        let PyRep::Rowset { rows, .. } = rpc(&ctx, &mut sess, "skillMgr2", "GetSkills", &[]).await.expect("GetSkills") else {
            panic!("Expected Rowset from GetSkills");
        };
        let returned_skills: HashMap<u32, (u8, f64)> = rows.iter().map(|r| {
            (r[0].as_i64().unwrap() as u32, (r[1].as_i64().unwrap() as u8, r[2].as_f64().unwrap()))
        }).collect();

        let expected_sde_skills = &ctx.sde.races[&race_id].starter_skills;
        let mut expected_total_sp = 0.0;
        for (&sk_id, &lvl) in expected_sde_skills {
            if lvl > 0 {
                let (got_lvl, got_sp) = returned_skills.get(&sk_id).copied().unwrap_or_else(|| panic!("Missing starter skill {sk_id} for race {race_id}"));
                assert_eq!(got_lvl, lvl, "Starter skill {sk_id} level mismatch for race {race_id}");
                let rank = ctx.sde.types.get(&sk_id).and_then(|t| t.default_attributes.get(&275).copied()).unwrap_or(1.0);
                let exp_sp = sp_for_skill_level(rank, lvl);
                assert!((got_sp - exp_sp).abs() < 1e-3, "Starter skill {sk_id} SP mismatch");
                expected_total_sp += exp_sp;
            }
        }
        assert_eq!(returned_skills.get(&frigate_sk).map(|v| v.0), Some(1), "Racial frigate skill {frigate_sk} must start at level 1");
        let ch = ctx.db.get_character(cid).unwrap();
        assert!((ch.skill_points - expected_total_sp).abs() < 1e-3);
        assert!(ctx.db.with_snapshot(|s| s.items.values().any(|i| i.owner_id == cid && i.type_id == exp_corvette)));
    }

    // Negative validations: cross-race school mismatch, duplicate character name, and 4th slot limit (`MaxCharacterSlotsReached`)
    ctx.db.provision_player_account("slot_test_u", "secret");
    let mut s_neg = SessionState::new(2199);
    common::login_pilot(&ctx, &mut s_neg, "slot_test_u", "secret").await;
    let (d1, p1) = doll_pair();
    let mismatch_err = rpc(&ctx, &mut s_neg, "charUnboundMgr", "CreateCharacterWithDoll", &[
        PyRep::String("Bad Lineage".into()), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(11), d1, p1, PyRep::Integer(11), // Caldari bloodline 1 + Amarr school 11
    ]).await;
    assert!(mismatch_err.is_err(), "Cross-race bloodline/school mismatch must be rejected");

    let (d2, p2) = doll_pair();
    let dup_err = rpc(&ctx, &mut s_neg, "charUnboundMgr", "CreateCharacterWithDoll", &[
        PyRep::String("Caldari Cadet".into()), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(11), d2, p2, PyRep::Integer(17),
    ]).await;
    assert!(dup_err.is_err(), "Duplicate character name must be rejected");

    for slot in 1..=3 {
        let (d, p) = doll_pair();
        rpc(&ctx, &mut s_neg, "charUnboundMgr", "CreateCharacterWithDoll", &[
            PyRep::String(format!("Slot Pilot {slot}")), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(11), d, p, PyRep::Integer(17),
        ]).await.expect("Create character within 3-slot cap");
    }
    let (d4, p4) = doll_pair();
    let slot4_err = rpc(&ctx, &mut s_neg, "charUnboundMgr", "CreateCharacterWithDoll", &[
        PyRep::String("Slot Pilot Four".into()), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(11), d4, p4, PyRep::Integer(17),
    ]).await.unwrap_err();
    assert_eq!(slot4_err.code, "MaxCharacterSlotsReached");
}

#[tokio::test]
async fn test_skillbook_market_buy_inject_and_remote_purchase() {
    // parity: feat:skills.inject-skillbook, feat:skills.direct-purchase-inject
    eve_testkit::attest(&["feat:skills.inject-skillbook", "feat:skills.direct-purchase-inject"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    eve_server::world_seeder::seed_dual_anchor_markets(&ctx.db, &ctx.sde);

    // Seller `demo` (90_000_001) donates ISK to fresh buyer AND lists a player sell order for Cloaking (11579) on Jita 4-4
    let mut seller = login_and_select(&ctx, 2201, "demo", "demo-secret", 90_000_001).await;
    let (mut buyer, buyer_cid) = create_pilot_in_jita(&ctx, 2202, "book_buyer_u", "secret", "Book Buyer", 1, 11, 17).await;
    rpc(&ctx, &mut seller, "account", "GiveCash", &[PyRep::Integer(buyer_cid), PyRep::Real(10_000_000.0), PyRep::String("market funds".into())]).await.unwrap();

    place_hangar_item(&ctx, 992_100_001, 11_579, 90_000_001, 1);
    rpc(&ctx, &mut seller, "marketProxy", "PlaceMultiSellOrder", &[
        PyRep::List(vec![PyRep::keyval([
            ("stationID", PyRep::Integer(JITA_STN)),
            ("typeID", PyRep::Integer(11_579)),
            ("itemID", PyRep::Integer(992_100_001)),
            ("price", PyRep::Real(500_000.0)),
            ("quantity", PyRep::Integer(1)),
        ])]),
        PyRep::Bool(false),
        PyRep::Integer(90),
        PyRep::Real(0.0),
    ]).await.expect("Seller lists Cloaking skillbook on Jita market");

    // 1. Buyer purchases NPC-seeded Leadership (3348) & Corp Management (3363) AND player-listed Cloaking (11579) via `marketProxy.PlaceBuyOrder`
    for (tid, max_price) in [(3348i64, 50_000.0), (3363, 50_000.0), (11_579, 500_000.0)] {
        let buy_rep = rpc(&ctx, &mut buyer, "marketProxy", "PlaceBuyOrder", &[
            PyRep::Integer(JITA_STN), PyRep::Integer(tid), PyRep::Real(max_price), PyRep::Integer(1), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(0),
        ]).await.expect("Immediate PlaceBuyOrder for skillbook");
        assert_eq!(buy_rep.dict_get("filledImmediately"), Some(&PyRep::Bool(true)));
    }

    let find_book = |tid: u32| -> i64 {
        ctx.db.with_snapshot(|s| {
            s.items.values().find(|i| i.owner_id == buyer_cid && i.location_id == JITA_STN && i.flag == InventoryFlag::Hangar && i.type_id == tid).expect("bought book in hangar").item_id
        })
    };
    let (lead_iid, corp_iid, cloak_iid) = (find_book(3348), find_book(3363), find_book(11_579));

    // 2. Scalar `InjectSkillIntoBrain(itemID)` on Leadership (3348): consumes book, adds level 0 skill, emits OnServerSkillsChanged, logs event 36
    let _ = ctx.drain_notifications_for_char(buyer_cid);
    rpc(&ctx, &mut buyer, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::Integer(lead_iid)]).await.expect("Scalar InjectSkillIntoBrain");
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&lead_iid)), "Injected skillbook must be consumed");
    assert_eq!(ctx.db.with_snapshot(|s| s.skills[&buyer_cid][&3348].level), 0);
    assert!(ctx.drain_notifications_for_char(buyer_cid).iter().any(|n| n.event == "OnServerSkillsChanged"));

    // 3. Batch V24 `InjectSkillIntoBrain([item1, item2])` on Corp Management (3363) + Cloaking (11579)
    rpc(&ctx, &mut buyer, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::List(vec![PyRep::Integer(corp_iid), PyRep::Integer(cloak_iid)])]).await.expect("Batch InjectSkillIntoBrain");
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&corp_iid) && !s.items.contains_key(&cloak_iid)));
    assert_eq!(ctx.db.with_snapshot(|s| s.skills[&buyer_cid][&3363].level), 0);
    assert_eq!(ctx.db.with_snapshot(|s| s.skills[&buyer_cid][&11_579].level), 0);

    // 4. Negative rejections on `InjectSkillIntoBrain`: duplicate skill, non-skill item, unowned item
    place_hangar_item(&ctx, 992_100_010, 3348, buyer_cid, 1);
    let dup_err = rpc(&ctx, &mut buyer, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::List(vec![PyRep::Integer(992_100_010)])]).await.unwrap_err();
    assert_eq!(dup_err.code, "SkillAlreadyInjected");
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&992_100_010)), "Failed duplicate injection must not consume book");

    place_hangar_item(&ctx, 992_100_011, 34, buyer_cid, 10); // Tritanium (not a skillbook)
    assert_eq!(rpc(&ctx, &mut buyer, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::List(vec![PyRep::Integer(992_100_011)])]).await.unwrap_err().code, "InvalidSkillType");

    place_hangar_item(&ctx, 992_100_012, 26_252, 90_000_001, 1); // Owned by seller, not buyer
    assert!(rpc(&ctx, &mut buyer, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::List(vec![PyRep::Integer(992_100_012)])]).await.is_err());

    // 5. Remote direct purchase (`feat:skills.direct-purchase-inject`) WHILE UNDOCKED IN SPACE:
    //    Jury Rigging (26252, base_price = 75,000 ISK -> 130% = 97,500 ISK); unseeded Astrogeology (3410) rejected!
    rpc(&ctx, &mut buyer, "stationSvc", "Undock", &[]).await.expect("Undock into space");
    assert!(buyer.station_id.is_none());
    let bal_before = ctx.db.get_character(buyer_cid).unwrap().balance.to_f64_isk();
    let direct_rep = rpc(&ctx, &mut buyer, "skillMgr2", "PurchaseSkills", &[PyRep::List(vec![PyRep::Integer(26_252)])]).await.expect("Remote PurchaseSkills in space");
    let cost = direct_rep.dict_get("totalCostISK").and_then(|v| v.as_f64()).expect("totalCostISK");
    let expected_npc_ask = ctx.db.with_snapshot(|s| {
        s.market_orders.values().filter(|o| o.is_npc && !o.is_bid && o.type_id == 26_252).map(|o| o.price.to_f64_isk()).fold(f64::INFINITY, f64::min)
    });
    assert!((cost - expected_npc_ask * 1.30).abs() < 1e-2, "Remote PurchaseSkills must charge 130% of NPC sell price");
    let bal_after = ctx.db.get_character(buyer_cid).unwrap().balance.to_f64_isk();
    assert!((bal_before - bal_after - cost).abs() < 1e-2);
    assert_eq!(ctx.db.with_snapshot(|s| s.skills[&buyer_cid][&26_252].level), 0);

    let unseeded_skill_tid = ctx.db.with_snapshot(|s| {
        ctx.sde.types.values()
            .filter(|t| t.category_id == 16 && t.published && !s.skills[&buyer_cid].contains_key(&t.type_id))
            .find(|t| t.base_price <= 0.0 || !s.market_orders.values().any(|o| !o.is_bid && o.is_npc && o.vol_remaining > 0 && o.type_id == t.type_id))
            .expect("published drop-only / non-NPC-purchasable skill (e.g. Sleeper Technology 21789)")
            .type_id
    });
    let unseeded_err = rpc(&ctx, &mut buyer, "skillMgr2", "PurchaseSkills", &[PyRep::List(vec![PyRep::Integer(unseeded_skill_tid as i64)])]).await.unwrap_err();
    assert_eq!(unseeded_err.code, "SkillUnavailableForPurchase");
}

#[tokio::test]
async fn test_skill_injectors_large_small_alpha_and_nondiminishing() {
    // parity: feat:skills.skill-injector-large, feat:skills.skill-injector-small, feat:skills.alpha-injector, feat:skills.nondiminishing-injector
    eve_testkit::attest(&[
        "feat:skills.skill-injector-large",
        "feat:skills.skill-injector-small",
        "feat:skills.alpha-injector",
        "feat:skills.nondiminishing-injector",
    ]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    common::seed_test_nes_store_catalog(&ctx);

    // --- Part A: Daily Alpha Injector (`46375`, 50,000 SP, Alpha-only, 1 per 24h) ---
    let (mut alpha_sess, alpha_cid) = create_pilot_in_jita(&ctx, 2301, "alpha_inj_u", "secret", "Alpha Injector Pilot", 1, 11, 17).await;
    place_hangar_item(&ctx, 992_200_001, 46_375, alpha_cid, 3);
    place_hangar_item(&ctx, 992_200_002, 40_519, alpha_cid, 1);

    // Alpha clone cannot use Skill Extractor (`OmegaCloneRequired`)
    let alpha_ext_err = rpc(&ctx, &mut alpha_sess, "skillMgr2", "ExtractSkills", &[
        PyRep::Dict(vec![(PyRep::Integer(25_739), PyRep::Integer(500_000))]), PyRep::Integer(992_200_002),
    ]).await.unwrap_err();
    assert_eq!(alpha_ext_err.code, "OmegaCloneRequired");

    assert_eq!(rpc(&ctx, &mut alpha_sess, "alphaInjectorMgr", "GetNextAvailableInjection", &[]).await.unwrap(), PyRep::Integer(0));
    let daily_1 = rpc(&ctx, &mut alpha_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_001)]).await.expect("First Daily Alpha Injector");
    assert_eq!(daily_1, PyRep::Real(50_000.0));
    assert_eq!(ctx.db.get_character(alpha_cid).unwrap().free_skill_points, 50_000.0);

    let next_avail = rpc(&ctx, &mut alpha_sess, "alphaInjectorMgr", "GetNextAvailableInjection", &[]).await.unwrap().as_i64().unwrap();
    assert!(next_avail > 0, "24h Daily Alpha Injector cooldown timestamp must be active");

    let cd_err = rpc(&ctx, &mut alpha_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_001)]).await.unwrap_err();
    assert_eq!(cd_err.code, "DailyAlphaInjectorCooldownActive");

    ctx.advance_secs(86_400 + 5);
    assert_eq!(rpc(&ctx, &mut alpha_sess, "alphaInjectorMgr", "GetNextAvailableInjection", &[]).await.unwrap(), PyRep::Integer(0));
    let daily_2 = rpc(&ctx, &mut alpha_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_001)]).await.expect("Daily Alpha Injector after 24h");
    assert_eq!(daily_2, PyRep::Real(50_000.0));

    // Upgrade to Omega -> Daily Alpha Injector rejected with `AlphaCloneRequired`!
    upgrade_to_omega_via_nes(&ctx, &mut alpha_sess, alpha_cid, 992_200_003).await;
    ctx.advance_secs(86_400 + 5);
    let omega_daily_err = rpc(&ctx, &mut alpha_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_001)]).await.unwrap_err();
    assert_eq!(omega_daily_err.code, "AlphaCloneRequired");

    // --- Part B: Extraction + Large (40520) & Small (45635) Diminishing Brackets + Non-Diminishing (43630, 49704) ---
    let (mut omega_sess, omega_cid) = create_pilot_in_jita(&ctx, 2302, "omega_inj_u", "secret", "Omega Injector Pilot", 1, 11, 17).await;
    upgrade_to_omega_via_nes(&ctx, &mut omega_sess, omega_cid, 992_200_010).await;

    // Bracket 1 (< 5,000,000 total SP): Large = 500,000 SP, Small = 100,000 SP
    place_hangar_item(&ctx, 992_200_011, 40_520, omega_cid, 230);
    place_hangar_item(&ctx, 992_200_012, 45_635, omega_cid, 4);
    assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_011)]).await.unwrap(), PyRep::Real(500_000.0));
    assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_012)]).await.unwrap(), PyRep::Real(100_000.0));

    // Below 5.5M allocated SP, ExtractSkills fails with `InsufficientSkillPoints`
    place_hangar_item(&ctx, 992_200_013, 40_519, omega_cid, 1);
    let low_sp_ext = rpc(&ctx, &mut omega_sess, "skillMgr2", "ExtractSkills", &[
        PyRep::Dict(vec![(PyRep::Integer(25_739), PyRep::Integer(500_000))]), PyRep::Integer(992_200_013),
    ]).await.unwrap_err();
    assert_eq!(low_sp_ext.code, "InsufficientSkillPoints");

    // Inject to Bracket 2 (5M..50M total SP): 9 more Large Injectors -> > 5M total SP -> Large = 400k, Small = 80k!
    for _ in 0..9 {
        rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_011)]).await.unwrap();
    }
    let ch_b2 = ctx.db.get_character(omega_cid).unwrap();
    assert!((5_000_000.0..50_000_000.0).contains(&(ch_b2.skill_points + ch_b2.free_skill_points)));
    assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_011)]).await.unwrap(), PyRep::Real(400_000.0));
    assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_012)]).await.unwrap(), PyRep::Real(80_000.0));

    // Allocate >= 5.5M SP via real RPCs and extract 500k SP -> produces a new Large Skill Injector (40520) item!
    for (tid, iid) in [(11_579u32, 992_200_014i64), (21_603, 992_200_015), (4_411, 992_200_016)] {
        place_hangar_item(&ctx, iid, tid, omega_cid, 1);
        rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::List(vec![PyRep::Integer(iid)])]).await.unwrap();
    }
    rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_011)]).await.unwrap();
    for (tid, sp_needed) in [(3_426i64, 210_745i64), (25_739, 2_046_000), (11_579, 1_536_000), (21_603, 1_280_000), (4_411, 1_280_000)] {
        rpc(&ctx, &mut omega_sess, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(tid), PyRep::Integer(sp_needed)]).await.unwrap();
    }
    // Non-extractable Cybernetics (3411) rejected (`SkillCannotBeExtracted`)
    assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "ExtractSkills", &[
        PyRep::Dict(vec![(PyRep::Integer(3411), PyRep::Integer(500_000))]), PyRep::Integer(992_200_013),
    ]).await.unwrap_err().code, "SkillCannotBeExtracted");

    let ext_rep = rpc(&ctx, &mut omega_sess, "skillMgr2", "ExtractSkills", &[
        PyRep::Dict(vec![(PyRep::Integer(25_739), PyRep::Integer(500_000))]), PyRep::Integer(992_200_013),
    ]).await.expect("ExtractSkills produces Large Skill Injector");
    let produced_lsi_id = ext_rep.dict_get("injectorItemID").or_else(|| ext_rep.dict_get("itemID")).and_then(|v| v.as_i64()).unwrap_or_else(|| {
        ctx.db.with_snapshot(|s| s.items.values().find(|i| i.owner_id == omega_cid && i.type_id == 40_520 && i.item_id != 992_200_011).unwrap().item_id)
    });
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&produced_lsi_id].type_id), 40_520);

    // Inject to Bracket 3 (50M..80M total SP): 112 Large Injectors (+44.8M SP) -> Large = 300k, Small = 60k!
    for _ in 0..112 {
        rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_011)]).await.unwrap();
    }
    let ch_b3 = ctx.db.get_character(omega_cid).unwrap();
    assert!((50_000_000.0..80_000_000.0).contains(&(ch_b3.skill_points + ch_b3.free_skill_points)));
    assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_011)]).await.unwrap(), PyRep::Real(300_000.0));
    assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_012)]).await.unwrap(), PyRep::Real(60_000.0));

    // Inject to Bracket 4 (>= 80M total SP): 99 Large Injectors (+29.7M SP) -> Large = 150k, Small = 30k!
    for _ in 0..99 {
        rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_011)]).await.unwrap();
    }
    let ch_b4 = ctx.db.get_character(omega_cid).unwrap();
    assert!(ch_b4.skill_points + ch_b4.free_skill_points >= 80_000_000.0);
    assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(produced_lsi_id)]).await.unwrap(), PyRep::Real(150_000.0));
    assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_012)]).await.unwrap(), PyRep::Real(30_000.0));

    // Even at >= 80M total SP, Non-Diminishing Injectors (`43630` = 25,000 SP, `49704` = 256,000 SP via SDE attr 2461) grant 100% fixed SP!
    place_hangar_item(&ctx, 992_200_020, 43_630, omega_cid, 1);
    place_hangar_item(&ctx, 992_200_021, 49_704, omega_cid, 1);
    let PyRep::Dict(avail_nd) = rpc(&ctx, &mut omega_sess, "nonDiminishingInjectionMgr", "GetAvailableNonDiminishingInjections", &[]).await.unwrap() else {
        panic!("Expected Dict from GetAvailableNonDiminishingInjections");
    };
    assert!(avail_nd.contains(&(PyRep::Integer(43_630), PyRep::Integer(1))));
    assert!(avail_nd.contains(&(PyRep::Integer(49_704), PyRep::Integer(1))));

    let _ = ctx.drain_notifications_for_char(omega_cid);
    assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_020)]).await.unwrap(), PyRep::Real(25_000.0));
    assert_eq!(rpc(&ctx, &mut omega_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(992_200_021)]).await.unwrap(), PyRep::Real(256_000.0));
    assert!(ctx.drain_notifications_for_char(omega_cid).iter().any(|n| n.event == "OnNonDiminishingInjectionsUsed"));
}
