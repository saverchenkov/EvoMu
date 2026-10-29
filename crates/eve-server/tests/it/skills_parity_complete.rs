//! Integration tests for Skill Parity features driven through live V24 client RPCs:
//! `feat:skills.alpha-skill-caps`, `feat:skills.skill-history`, `feat:skills.skill-remove-obsolete`,
//! `feat:skills.accelerators`, `feat:skills.certificates`, `feat:skills.skill-plans`.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};
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

async fn create_and_select_alpha_pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_name: &str) -> (SessionState, i64) {
    ctx.db.provision_player_account(user, pass);
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    let doll = PyRep::keyval([("costume", PyRep::Integer(301)), ("gender", PyRep::Integer(1)), ("hair", PyRep::Integer(101)), ("skin", PyRep::Integer(201))]);
    let portrait = PyRep::keyval([("background", PyRep::Integer(42)), ("camera", PyRep::Integer(1)), ("pose", PyRep::Integer(7))]);
    let cid = rpc(
        ctx, &mut sess, "charUnboundMgr", "CreateCharacterWithDoll",
        &[PyRep::String(char_name.into()), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(11), doll, portrait, PyRep::Integer(17), PyRep::None, PyRep::Integer(JITA_SYS as i64)],
    ).await.expect("CreateCharacterWithDoll").as_i64().expect("char_id");
    // Verify R-QA-SYS-01: normal account cannot choose starter system
    let created_char = ctx.db.get_character(cid).expect("created character");
    assert_ne!(created_char.solar_system_id, JITA_SYS, "Normal account cannot choose starter system (R-QA-SYS-01)");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(cid)]).await.expect("SelectCharacterID");
    (sess, cid)
}

fn place_hangar_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, qty: i32) {
    let loc_id = ctx.db.get_character(owner_id).ok().and_then(|c| c.station_id).unwrap_or(JITA_STN);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(item_id, InventoryItemRecord {
            item_id, type_id, owner_id, location_id: loc_id, flag: InventoryFlag::Hangar,
            quantity: qty, singleton: false, version: 1, ..Default::default()
        });
    });
}

async fn upgrade_to_omega_via_nes(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, plex_item_id: i64) {
    let loc_id = ctx.db.get_character(char_id).ok().and_then(|c| c.station_id).or(sess.station_id).unwrap_or(JITA_STN);
    place_hangar_item(ctx, plex_item_id, 44_992, char_id, 500);
    rpc(ctx, sess, "invbroker", "DepositPlexToVault", &[PyRep::Integer(loc_id), PyRep::Integer(plex_item_id), PyRep::Integer(500), PyRep::Integer(0)]).await.expect("DepositPlexToVault");
    rpc(ctx, sess, "storeManager", "buy_offer", &[PyRep::Integer(1001)]).await.expect("buy_offer 1001 30d Omega");
}

async fn quasar_rpc(ctx: &ServerContext, sess: &mut SessionState, type_url: &str, payload_value: Vec<u8>) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: b"skillplan-test-1".to_vec(),
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) { type_url.to_string() } else { format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}") },
            value: payload_value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-inst-test".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = rpc(ctx, sess, "publicGatewaySvc", "SendRequest", &[PyRep::Bytes(req.encode_proto())]).await.expect("SendRequest");
    if let PyRep::KeyVal(entries) = &rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                }
            }
        }
    }
    panic!("Expected response_proto, got {rep:?}");
}

#[tokio::test]
async fn test_feat_skills_alpha_skill_caps() {
    // parity: feat:skills.alpha-skill-caps
    eve_testkit::attest(&["feat:skills.alpha-skill-caps"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    common::seed_test_nes_store_catalog(&ctx);

    let (mut sess, char_id) = create_and_select_alpha_pilot(&ctx, 1001, "alpha_cap_u", "alpha-pass", "Alpha Cap Pilot").await;
    assert_eq!(ctx.db.get_character(char_id).unwrap().clone_grade, 1);

    // 1. Science (3402) has Alpha cap 4 in SDE clonegrades; queueing level 5 fails for Alpha
    let queue_over_cap = PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3402), PyRep::Integer(5)])]);
    assert_eq!(rpc(&ctx, &mut sess, "skillMgr2", "SaveSkillQueue", &[queue_over_cap]).await.unwrap_err().code, "AlphaCloneSkillLevelCapExceeded");

    // 2. Upgrade to 30d Omega via NES, inject SP, train Science V, then lapse back to Alpha after 31 days
    upgrade_to_omega_via_nes(&ctx, &mut sess, char_id, 991_000_001).await;
    assert_eq!(ctx.db.get_character(char_id).unwrap().clone_grade, 2);

    place_hangar_item(&ctx, 991_000_002, 40_520, char_id, 11);
    assert_eq!(rpc(&ctx, &mut sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(991_000_002)]).await.unwrap(), PyRep::Real(500_000.0));
    rpc(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(3402), PyRep::Integer(210_745)]).await.expect("Train Science V");
    assert_eq!(ctx.character_skill_levels(char_id).get(&3402).copied(), Some(5));

    ctx.advance_secs(31 * 86_400);
    rpc(&ctx, &mut sess, "skillMgr2", "CheckAndSendNotifications", &[]).await.expect("CheckAndSendNotifications lapses Omega");
    assert_eq!(ctx.db.get_character(char_id).unwrap().clone_grade, 1);
    assert_eq!(ctx.character_skill_levels(char_id).get(&3402).copied(), Some(4), "Alpha clone SP above cap must be inactive");
    assert_eq!(ctx.db.with_snapshot(|s| s.skills[&char_id][&3402].skill_points), 256_000.0);

    // 3. Inject past 5M total SP -> SaveSkillQueue rejected, but ApplyFreeSkillPoints still works up to Alpha caps
    for _ in 0..10 {
        rpc(&ctx, &mut sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(991_000_002)]).await.expect("InjectSkillpoints");
    }
    let ch_now = ctx.db.get_character(char_id).unwrap();
    assert!(ch_now.skill_points + ch_now.free_skill_points > 5_000_000.0);

    let queue_sc4 = PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3327), PyRep::Integer(4)])]);
    assert_eq!(rpc(&ctx, &mut sess, "skillMgr2", "SaveSkillQueue", &[queue_sc4]).await.unwrap_err().code, "AlphaCloneSpCapExceeded");
    assert_eq!(rpc(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(3327), PyRep::Integer(10_000)]).await.unwrap(), PyRep::Real(18_000.0));

    // 4. Re-subscribe to Omega via NES: Science V active again and passive queue above 5M SP succeeds
    upgrade_to_omega_via_nes(&ctx, &mut sess, char_id, 991_000_003).await;
    assert_eq!(ctx.character_skill_levels(char_id).get(&3402).copied(), Some(5));
    let omega_q = PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3327), PyRep::Integer(5)])]);
    rpc(&ctx, &mut sess, "skillMgr2", "SaveSkillQueue", &[omega_q]).await.expect("Omega removes Alpha 5M SP queue cap");
}

#[tokio::test]
async fn test_feat_skills_history_events_and_obsolete_refund() {
    // parity: feat:skills.skill-history, feat:skills.skill-remove-obsolete
    eve_testkit::attest(&["feat:skills.skill-history", "feat:skills.skill-remove-obsolete"]);
    let db_path = std::env::temp_dir().join(format!("evomu_skill_hist_rem_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
    let ctx = ServerContext::with_repository(repo);
    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    ctx.db.with_mut_snapshot(|snap| *snap = demo);
    common::pin_clock(&ctx);
    common::seed_test_nes_store_catalog(&ctx);
    eve_server::world_seeder::seed_dual_anchor_markets(&ctx.db, &ctx.sde);

    let mut donor = login_and_select(&ctx, 1010, "demo", "demo-secret", 90_000_001).await;
    let (mut sess, char_id) = create_and_select_alpha_pilot(&ctx, 1011, "hist_pilot_u", "hist-pass", "History Pilot").await;
    rpc(&ctx, &mut donor, "account", "GiveCash", &[PyRep::Integer(char_id), PyRep::Real(5_000_000.0), PyRep::String("stipend".into())]).await.unwrap();
    upgrade_to_omega_via_nes(&ctx, &mut sess, char_id, 991_000_101).await;

    // 1. Event 36 (Skill Injection) via PurchaseSkills (Leadership 3348)
    ctx.advance_secs(30);
    rpc(&ctx, &mut sess, "skillMgr2", "PurchaseSkills", &[PyRep::List(vec![PyRep::Integer(3348)])]).await.expect("PurchaseSkills 3348");

    // 2. Event 37 (Skill Training Complete) via SaveSkillQueue + clock advance
    ctx.advance_secs(30);
    rpc(&ctx, &mut sess, "skillMgr2", "SaveSkillQueue", &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3348), PyRep::Integer(1)])])]).await.unwrap();
    ctx.advance_secs(1_200);
    rpc(&ctx, &mut sess, "skillMgr2", "CheckAndSendNotifications", &[]).await.unwrap();
    assert_eq!(ctx.character_skill_levels(char_id).get(&3348).copied(), Some(1));

    // 3. Event 39 (Skill Extraction) via real InjectSkillIntoBrain + InjectSkillpoints + ApplyFreeSkillPoints + ExtractSkills
    ctx.advance_secs(30);
    place_hangar_item(&ctx, 991_000_102, 40_520, char_id, 14);
    for (tid, iid) in [(11_579u32, 991_000_103i64), (21_603, 991_000_104), (4_411, 991_000_105)] {
        place_hangar_item(&ctx, iid, tid, char_id, 1);
        rpc(&ctx, &mut sess, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::List(vec![PyRep::Integer(iid)])]).await.unwrap();
    }
    for _ in 0..14 {
        rpc(&ctx, &mut sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(991_000_102)]).await.unwrap();
    }
    for (tid, sp_needed) in [(3_426i64, 210_745i64), (25_739, 2_046_000), (11_579, 1_536_000), (21_603, 1_280_000), (4_411, 1_280_000)] {
        rpc(&ctx, &mut sess, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(tid), PyRep::Integer(sp_needed)]).await.unwrap();
    }
    assert!(ctx.db.get_character(char_id).unwrap().skill_points >= 5_500_000.0);

    place_hangar_item(&ctx, 991_000_106, 40_519, char_id, 1);
    rpc(&ctx, &mut sess, "skillMgr2", "ExtractSkills", &[PyRep::Dict(vec![(PyRep::Integer(25_739), PyRep::Integer(500_000))]), PyRep::Integer(991_000_106)]).await.expect("ExtractSkills");

    // 4. Event 38 (`feat:skills.skill-remove-obsolete`): legacy pre-V24 skill 999_999 migrated on SelectCharacterID after SQLite reopen
    ctx.advance_secs(30);
    let (sp_pre_obs, free_pre_obs) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.skill_points += 1_500.0;
        let pre = (ch.skill_points, ch.free_skill_points);
        snap.skills.entry(char_id).or_default().insert(999_999, TrainedSkillRecord { type_id: 999_999, level: 2, skill_points: 1_500.0 });
        pre
    });

    drop(ctx);
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = ServerContext::with_repository(repo2);
    common::pin_clock(&ctx2);
    ctx2.advance_secs(1_500);
    let mut sess2 = login_and_select(&ctx2, 1012, "hist_pilot_u", "hist-pass", char_id).await;

    let ch_post = ctx2.db.get_character(char_id).unwrap();
    assert!(!ctx2.db.with_snapshot(|s| s.skills[&char_id].contains_key(&999_999)));
    assert!((ch_post.skill_points - (sp_pre_obs - 1_500.0)).abs() < 1e-3);
    assert!((ch_post.free_skill_points - (free_pre_obs + 1_500.0)).abs() < 1e-3);
    let notifs = ctx2.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnServerSkillsRemoved"));
    assert!(notifs.iter().any(|n| n.event == "OnFreeSkillPointsChanged"));

    let PyRep::Rowset { rows, .. } = rpc(&ctx2, &mut sess2, "skillMgr2", "GetSkillHistory", &[]).await.unwrap() else { panic!("Expected Rowset") };
    assert!(rows.windows(2).all(|w| w[0][0].as_i64().unwrap() <= w[1][0].as_i64().unwrap()));
    let ev_types: Vec<i64> = rows.iter().map(|r| r[1].as_i64().unwrap()).collect();
    for expected_ev in [33, 36, 37, 38, 39] {
        assert!(ev_types.contains(&expected_ev), "Missing skill history event {expected_ev} in {ev_types:?}");
    }
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_feat_skills_certificates_and_plans_and_accelerators() {
    // parity: feat:skills.accelerators, feat:skills.certificates, feat:skills.skill-plans
    eve_testkit::attest(&["feat:skills.accelerators", "feat:skills.certificates", "feat:skills.skill-plans"]);
    let db_path = std::env::temp_dir().join(format!("evomu_cert_plan_acc_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
    let ctx = ServerContext::with_repository(repo);
    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    ctx.db.with_mut_snapshot(|snap| *snap = demo);
    common::pin_clock(&ctx);
    common::seed_test_nes_store_catalog(&ctx);

    let mut ceo_sess = login_and_select(&ctx, 1020, "demo", "demo-secret", 90_000_001).await;
    let mut member_sess = login_and_select(&ctx, 1021, "other", "other-secret", 90_000_002).await;
    ctx.db.clock().advance_secs(15);

    // Found real player corporation via corpRegistry.AddCorporation and accept member 90_000_002 via InsertApplication + UpdateApplicationOffer
    let created_corp = rpc(&ctx, &mut ceo_sess, "corpRegistry", "AddCorporation", &[PyRep::String("Skill Plan Corp".into()), PyRep::String("SPCP".into()), PyRep::Real(0.05)]).await.expect("AddCorporation");
    let corp_id = created_corp.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corporationID");
    ctx.sync_session(&mut ceo_sess);
    ctx.db.clock().advance_secs(15);
    let app_rep = rpc(&ctx, &mut member_sess, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Join".into())]).await.expect("InsertApplication");
    let app_id = app_rep.dict_get("applicationID").and_then(|v| v.as_i64()).expect("applicationID");
    rpc(&ctx, &mut ceo_sess, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())]).await.expect("UpdateApplicationOffer");
    ctx.sync_session(&mut member_sess);
    assert_eq!(member_sess.corporation_id, Some(corp_id));

    let (mut alpha_sess, char_id) = create_and_select_alpha_pilot(&ctx, 1022, "cert_acc_u", "cert-pass", "Cert Accel Pilot").await;
    upgrade_to_omega_via_nes(&ctx, &mut alpha_sess, char_id, 991_000_201).await;

    // --- 1. Certificates (`feat:skills.certificates`) ---
    // rpc-lint: allow crucible legacy service certificateMgr
    let PyRep::List(cats) = rpc(&ctx, &mut alpha_sess, "certificateMgr", "GetCertificateCategories", &[]).await.unwrap() else { panic!("expected List") };
    assert!(!cats.is_empty());
    // rpc-lint: allow crucible legacy service certificateMgr
    let PyRep::List(classes) = rpc(&ctx, &mut alpha_sess, "certificateMgr", "GetCertificateClasses", &[]).await.unwrap() else { panic!("expected List") };
    assert!(!classes.is_empty());
    // rpc-lint: allow crucible legacy service certificateMgr
    let PyRep::KeyVal(recs) = rpc(&ctx, &mut alpha_sess, "certificateMgr", "GetAllShipCertificateRecommendations", &[]).await.unwrap() else { panic!("expected KeyVal") };
    assert!(!recs.is_empty());

    // Certificate 196 requires Science (3402) [1..=5]: starter Science IV -> cert level 4; train V -> cert level 5!
    // Certificate 2269 requires Gas Cloud Harvesting (25544) [1..=5]: untrained -> 0; train I -> 1; train V -> 5!
    // rpc-lint: allow crucible legacy service certificateMgr
    let c_before = rpc(&ctx, &mut alpha_sess, "certificateMgr", "GetMyCertificates", &[]).await.unwrap();
    let PyRep::Dict(d_before) = c_before.dict_get("certificates").unwrap() else { panic!("expected Dict") };
    assert_eq!(d_before.iter().find(|(k, _)| k.as_i64() == Some(196)).and_then(|(_, v)| v.as_i64()), Some(4));
    assert_eq!(d_before.iter().find(|(k, _)| k.as_i64() == Some(2269)), None, "Untrained Gas Cloud Harvesting must yield cert 2269 level 0");
    assert_eq!(eve_server::services::character_certificates::derive_ship_mastery_level(&ctx, char_id, 587), 0);

    place_hangar_item(&ctx, 991_000_202, 40_520, char_id, 2);
    rpc(&ctx, &mut alpha_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(991_000_202)]).await.unwrap();
    rpc(&ctx, &mut alpha_sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(991_000_202)]).await.unwrap();
    rpc(&ctx, &mut alpha_sess, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(3402), PyRep::Integer(210_745)]).await.unwrap();
    // Train prerequisite Mining (3386) IV, then inject Gas Cloud Harvesting (25544) and advance level 0 -> 1 -> 5
    rpc(&ctx, &mut alpha_sess, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(3386), PyRep::Integer(45_255)]).await.unwrap();
    place_hangar_item(&ctx, 991_000_204, 25_544, char_id, 1);
    rpc(&ctx, &mut alpha_sess, "skillMgr2", "InjectSkillIntoBrain", &[PyRep::List(vec![PyRep::Integer(991_000_204)])]).await.unwrap();
    rpc(&ctx, &mut alpha_sess, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(25_544), PyRep::Integer(250)]).await.unwrap();
    // rpc-lint: allow crucible legacy service certificateMgr
    let c_mid = rpc(&ctx, &mut alpha_sess, "certificateMgr", "GetMyCertificates", &[]).await.unwrap();
    let PyRep::Dict(d_mid) = c_mid.dict_get("certificates").unwrap() else { panic!("expected Dict") };
    assert_eq!(d_mid.iter().find(|(k, _)| k.as_i64() == Some(2269)).and_then(|(_, v)| v.as_i64()), Some(1));

    rpc(&ctx, &mut alpha_sess, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(25_544), PyRep::Integer(255_750)]).await.unwrap();
    // rpc-lint: allow crucible legacy service certificateMgr
    let c_after = rpc(&ctx, &mut alpha_sess, "certificateMgr", "GetMyCertificates", &[]).await.unwrap();
    let PyRep::Dict(d_after) = c_after.dict_get("certificates").unwrap() else { panic!("expected Dict") };
    assert_eq!(d_after.iter().find(|(k, _)| k.as_i64() == Some(196)).and_then(|(_, v)| v.as_i64()), Some(5));
    assert_eq!(d_after.iter().find(|(k, _)| k.as_i64() == Some(2269)).and_then(|(_, v)| v.as_i64()), Some(5));

    // Train all Rifter (587) Mastery I prerequisite skills -> ship mastery advances 0 -> 1
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for (stid, lvl) in [
            (3402u32, 5u8), (3413, 1), (3417, 1), (3426, 3), (3435, 1), (3449, 2),
            (3300, 3), (3302, 1), (3310, 1), (3311, 1), (3312, 1), (3429, 1),
            (3318, 1), (3392, 1), (3393, 1), (3394, 1), (3327, 1),
        ] {
            sk.insert(stid, TrainedSkillRecord { type_id: stid, level: lvl, skill_points: 256_000.0 });
        }
    });
    assert_eq!(eve_server::services::character_certificates::derive_ship_mastery_level(&ctx, char_id, 587), 1);

    // Non-GM players cannot grant certificates (R-CERT-01: AccessDenied)
    // rpc-lint: allow crucible legacy service certificateMgr
    assert_eq!(rpc(&ctx, &mut alpha_sess, "certificateMgr", "GrantCertificate", &[PyRep::Integer(char_id), PyRep::Integer(101), PyRep::Integer(3)]).await.unwrap_err().code, "AccessDenied");
    // rpc-lint: allow crucible legacy service certificateMgr
    assert_eq!(rpc(&ctx, &mut alpha_sess, "certificateMgr", "UpdateCertificateFlags", &[PyRep::Integer(101), PyRep::Integer(7)]).await.unwrap().as_bool(), Some(true));
    // GM session can grant certificates (clamped to 0..=5)
    let mut gm_cert_sess = alpha_sess.clone();
    gm_cert_sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    // rpc-lint: allow crucible legacy service certificateMgr
    assert_eq!(rpc(&ctx, &mut gm_cert_sess, "certificateMgr", "GrantCertificate", &[PyRep::Integer(char_id), PyRep::Integer(101), PyRep::Integer(3)]).await.unwrap().as_bool(), Some(true));

    // --- 2. Skill Plans (`feat:skills.skill-plans`) ---
    let (mut sp_attr, mut sp_req, mut req_entry, mut req_tid) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, "Frigate Mastery", &mut sp_attr);
    encode_varint_field(1, 3327, &mut req_tid);
    encode_bytes_field(1, &req_tid, &mut req_entry);
    encode_varint_field(2, 4, &mut req_entry);
    encode_bytes_field(2, &req_entry, &mut sp_attr);
    encode_string_field(3, "Core combat frigate skills", &mut sp_attr);
    encode_bytes_field(1, &sp_attr, &mut sp_req);
    let create_rsp = quasar_rpc(&ctx, &mut ceo_sess, "eve_public.character.skill.plan.CreateRequest", sp_req).await;
    assert_eq!(create_rsp.status_code, 200);
    let create_wm = WireMessage::decode(&create_rsp.payload.unwrap().value).unwrap();
    let id_wm = WireMessage::decode(create_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    let plan_uuid: [u8; 16] = id_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap().try_into().unwrap();

    let (mut f2_id, mut f2_req, mut sh_req) = (Vec::new(), Vec::new(), Vec::new());
    encode_bytes_field(1, &plan_uuid, &mut f2_id);
    encode_bytes_field(2, &f2_id, &mut f2_req);
    encode_bytes_field(1, &f2_id, &mut sh_req);
    assert_eq!(quasar_rpc(&ctx, &mut ceo_sess, "eve_public.character.skill.plan.SetActiveRequest", f2_req).await.status_code, 200);
    let shared_rsp = quasar_rpc(&ctx, &mut member_sess, "eve_public.character.skill.plan.GetSharedRequest", sh_req).await;
    assert_eq!(shared_rsp.status_code, 200);
    assert!(!shared_rsp.payload.unwrap().value.is_empty());

    // Corporation skill plans: unprivileged member gets 403 on CreateRequest, CEO creates (200), member reads via GetAllRequest (200)
    let (mut corp_attr, mut corp_val) = (Vec::new(), Vec::new());
    encode_string_field(1, "Corp Doctrine", &mut corp_attr);
    encode_string_field(2, "Fleet doctrine plan", &mut corp_attr);
    encode_bytes_field(1, &corp_attr, &mut corp_val);
    assert_eq!(quasar_rpc(&ctx, &mut member_sess, "eve_public.corporation.skill.plan.CreateRequest", corp_val.clone()).await.status_code, 403);
    assert_eq!(quasar_rpc(&ctx, &mut ceo_sess, "eve_public.corporation.skill.plan.CreateRequest", corp_val).await.status_code, 200);
    let member_corp_all = quasar_rpc(&ctx, &mut member_sess, "eve_public.corporation.skill.plan.GetAllRequest", Vec::new()).await;
    assert_eq!(member_corp_all.status_code, 200);
    assert!(!member_corp_all.payload.unwrap().value.is_empty());

    // Certified doctrine skill plans from SDE (`skillplans.fsdbinary`)
    let all_plans_rsp = quasar_rpc(&ctx, &mut ceo_sess, "eve_public.skill.plan.GetAllRequest", Vec::new()).await;
    assert_eq!(all_plans_rsp.status_code, 200);
    let all_wm = WireMessage::decode(&all_plans_rsp.payload.unwrap().value).unwrap();
    assert!(all_wm.fields.iter().any(|f| {
        WireMessage::decode(f.value.as_bytes().unwrap_or(&[]))
            .ok()
            .and_then(|w| w.fields.first().and_then(|id_f| id_f.value.as_bytes()).map(|u| u.starts_with(b"CERT")))
            .unwrap_or(false)
    }));

    // Fetch certified SDE skill plan 33 ("AIR - Repair Systems") via `eve_public.skill.plan.GetRequest`
    let mut cert_plan_uuid = [0u8; 16];
    cert_plan_uuid[0..4].copy_from_slice(b"CERT");
    cert_plan_uuid[4..8].copy_from_slice(&33u32.to_be_bytes());
    let (mut cert_id_msg, mut cert_get_req) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &cert_plan_uuid, &mut cert_id_msg);
    encode_bytes_field(1, &cert_id_msg, &mut cert_get_req);
    let cert_get_rsp = quasar_rpc(&ctx, &mut ceo_sess, "eve_public.skill.plan.GetRequest", cert_get_req).await;
    assert_eq!(cert_get_rsp.status_code, 200);
    let cert_get_wm = WireMessage::decode(&cert_get_rsp.payload.unwrap().value).unwrap();
    let cert_attr_wm = WireMessage::decode(cert_get_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(cert_attr_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_str(), Ok(Some("AIR - Repair Systems")));
    let req_count = cert_attr_wm.fields.iter().filter(|f| f.field_number == 2).count();
    assert_eq!(req_count, 3, "AIR - Repair Systems must contain 3 SDE skill requirements");

    // Skill Type & Category handlers against SDE (`eve.skilltype.GetRequest`, `eve.skillcategory.GetAllRequest`)
    let mut id_msg = Vec::new();
    encode_varint_field(1, 3327, &mut id_msg);
    let mut sk_type_req = Vec::new();
    encode_bytes_field(1, &id_msg, &mut sk_type_req);
    let sk_type_rsp = quasar_rpc(&ctx, &mut ceo_sess, "eve.skilltype.GetRequest", sk_type_req).await;
    assert_eq!(sk_type_rsp.status_code, 200, "status message: {}", sk_type_rsp.status_message);
    let sk_wm = WireMessage::decode(&sk_type_rsp.payload.unwrap().value).unwrap();
    let sk_attr_bytes = sk_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
    let sk_inner = WireMessage::decode(sk_attr_bytes).unwrap();
    let sk_tid = sk_inner.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_varint().unwrap();
    assert_eq!(sk_tid, 3327, "Must retrieve requested skill type ID 3327");
    let rank = sk_inner.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_varint().unwrap();
    assert_eq!(rank, 1, "Spaceship Command must have rank 1");

    // Invalid skill type returns 404 Not Found
    let mut invalid_id_msg = Vec::new();
    encode_varint_field(1, 999_999, &mut invalid_id_msg);
    let mut invalid_req = Vec::new();
    encode_bytes_field(1, &invalid_id_msg, &mut invalid_req);
    let not_found_rsp = quasar_rpc(&ctx, &mut ceo_sess, "eve.skilltype.GetRequest", invalid_req).await;
    assert_eq!(not_found_rsp.status_code, 404, "Nonexistent skill type must return 404");

    let cat_all_rsp = quasar_rpc(&ctx, &mut ceo_sess, "eve.skillcategory.GetAllRequest", Vec::new()).await;
    assert_eq!(cat_all_rsp.status_code, 200);
    let cat_wm = WireMessage::decode(&cat_all_rsp.payload.unwrap().value).unwrap();
    assert!(!cat_wm.fields.is_empty(), "Skill categories must return published skill groups");
    assert!(cat_wm.fields.iter().any(|f| {
        WireMessage::decode(f.value.as_bytes().unwrap_or(&[]))
            .ok()
            .and_then(|w| w.fields.iter().find(|fl| fl.field_number == 1).and_then(|id_f| id_f.value.as_varint()))
            .map(|gid| gid == 257)
            .unwrap_or(false)
    }), "Skill categories must include Spaceship Command group (257)");

    // --- 3. Cerebral Accelerators (`feat:skills.accelerators`) via real `dogmaIM.UseBooster` + clock ---
    rpc(&ctx, &mut alpha_sess, "skillMgr2", "SaveSkillQueue", &[PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(25_739), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(25_739), PyRep::Integer(3)]),
        PyRep::Tuple(vec![PyRep::Integer(25_739), PyRep::Integer(4)]),
    ])]).await.expect("SaveSkillQueue 25739");

    ctx.advance_secs(60);
    rpc(&ctx, &mut alpha_sess, "skillMgr2", "CheckAndSendNotifications", &[]).await.unwrap();
    let sp_base = ctx.db.with_snapshot(|s| s.skills[&char_id][&25_739].skill_points);
    assert!(((sp_base - 2_000.0) - 30.0).abs() < 1e-3, "Base 20/20 attributes must train 30 SP/min, got {}", sp_base - 2_000.0);

    place_hangar_item(&ctx, 991_000_203, 48_582, char_id, 1);
    let use_rep = rpc(&ctx, &mut alpha_sess, "dogmaIM", "UseBooster", &[PyRep::Integer(991_000_203)]).await.expect("UseBooster 48582");
    assert_eq!(use_rep.dict_get("durationSeconds"), Some(&PyRep::Integer(86_400)));

    ctx.advance_secs(60);
    rpc(&ctx, &mut alpha_sess, "skillMgr2", "CheckAndSendNotifications", &[]).await.unwrap();
    let sp_boosted = ctx.db.with_snapshot(|s| s.skills[&char_id][&25_739].skill_points);
    assert!(((sp_boosted - sp_base) - 45.0).abs() < 1e-3, "Active +10 Cerebral Accelerator must train 45 SP/min, got {}", sp_boosted - sp_base);

    ctx.advance_secs(86_500);
    let PyRep::Dict(active_boosters) = rpc(&ctx, &mut alpha_sess, "skillMgr2", "GetBoosters", &[]).await.unwrap() else { panic!("expected Dict") };
    assert!(active_boosters.is_empty(), "Expired Cerebral Accelerator must be purged");

    let sp_post_exp_0 = ctx.db.with_snapshot(|s| s.skills[&char_id][&25_739].skill_points);
    ctx.advance_secs(60);
    rpc(&ctx, &mut alpha_sess, "skillMgr2", "CheckAndSendNotifications", &[]).await.unwrap();
    let sp_post_exp_1 = ctx.db.with_snapshot(|s| s.skills[&char_id][&25_739].skill_points);
    assert!(((sp_post_exp_1 - sp_post_exp_0) - 30.0).abs() < 1e-3, "Expired accelerator must revert to 30 SP/min");

    // Verify SQLite reload persistence of active personal & corporation skill plans
    drop(ctx);
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = ServerContext::with_repository(repo2);
    common::pin_clock(&ctx2);
    let mut ceo2 = login_and_select(&ctx2, 1025, "demo", "demo-secret", 90_000_001).await;
    let active_reloaded = quasar_rpc(&ctx2, &mut ceo2, "eve_public.character.skill.plan.GetActiveRequest", Vec::new()).await;
    assert_eq!(active_reloaded.status_code, 200);
    assert!(!active_reloaded.payload.unwrap().value.is_empty());
    let _ = std::fs::remove_file(&db_path);
}
