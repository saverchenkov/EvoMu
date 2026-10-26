//! Integration tests for account features:
//! - `feat:account.multi-char-training`
//! - `feat:account.character-resculpt`
//! - `feat:account.banned-words`
//! - `feat:account.in-game-reports`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::ingamereport_ops::{self, InGameReportRecord};

use crate::common;

const PILOT_1: i64 = 90_000_001;
const PILOT_TARGET: i64 = 90_000_002;
const CORP_1: i64 = 98_000_001;
const JITA_STATION_ID: i64 = 60003760;

async fn login_and_select(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    s
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn seed_item(ctx: &ServerContext, type_id: u32, owner_id: i64, qty: i32) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            eve_db::InventoryItemRecord {
                item_id: id,
                type_id,
                owner_id,
                location_id: JITA_STATION_ID,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: qty,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        id
    })
}

#[tokio::test]
async fn test_feat_account_multi_char_training() {
    // parity: feat:account.multi-char-training
    eve_testkit::attest(&["feat:account.multi-char-training"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_and_select(&ctx, 60_001, "demo", "demo-secret", PILOT_1).await;
    let uid = sess.user_id.expect("user_id");

    // 1. Initial slot listing is empty
    let slots_init = rpc(&ctx, &mut sess, "userSvc", "GetMultiCharactersTrainingSlots", &[])
        .await
        .expect("GetMultiCharactersTrainingSlots init");
    assert_eq!(slots_init, PyRep::Dict(Vec::new()));

    // 2. Activation fails without MCT certificate
    let no_cert_err = rpc(&ctx, &mut sess, "userSvc", "ActivateMultiTraining", &[])
        .await
        .expect_err("Activate without cert must fail");
    assert_eq!(no_cert_err.code, "UserError");
    assert_eq!(no_cert_err.message, "NoMultiTrainingCertificateFound");

    // 3. Seed MCT certificate (type 33027) with a stack of 2
    let mct_item_id = seed_item(&ctx, 33027, PILOT_1, 2);
    let base_now = ctx.now_unix();
    let expected_expiry_1 = base_now + 30 * 86_400;

    // 4. Activate first MCT slot: decrements stack from 2 to 1
    let act1 = rpc(&ctx, &mut sess, "userSvc", "ActivateMultiTraining", &[])
        .await
        .expect("ActivateMultiTraining #1");
    let PyRep::KeyVal(act1_kv) = act1 else {
        panic!("expected KeyVal");
    };
    assert_eq!(act1_kv.get("slotActivated"), Some(&PyRep::Bool(true)));
    assert_eq!(
        act1_kv.get("expiryDate"),
        Some(&PyRep::Integer(expected_expiry_1 as i64))
    );

    // Verify certificate stack decremented to 1 in inventory
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&mct_item_id).expect("cert item exists");
        assert_eq!(it.quantity, 1, "Stack must be decremented to 1");
    });

    // Verify active slot persisted in service_state_blobs["mct_slots:<uid>"]
    ctx.db.with_snapshot(|snap| {
        let blob = snap
            .service_state_blobs
            .get(&format!("mct_slots:{uid}"))
            .expect("mct_slots blob recorded");
        let parsed: Vec<u64> = serde_json::from_str(blob).expect("valid json slots");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0], expected_expiry_1);
    });

    // 5. GetMultiCharactersTrainingSlots returns 1 active slot
    let slots_1 = rpc(&ctx, &mut sess, "userSvc", "GetMultiCharactersTrainingSlots", &[])
        .await
        .expect("GetMultiCharactersTrainingSlots 1");
    let PyRep::Dict(dict_1) = slots_1 else {
        panic!("expected Dict");
    };
    assert_eq!(dict_1.len(), 1);
    let (slot_id_rep, slot_data_rep) = &dict_1[0];
    assert_eq!(slot_id_rep, &PyRep::Integer(1));
    let PyRep::KeyVal(sd1) = slot_data_rep else {
        panic!("expected KeyVal");
    };
    assert_eq!(sd1.get("slotID"), Some(&PyRep::Integer(1)));
    assert_eq!(sd1.get("expiryDate"), Some(&PyRep::Integer(expected_expiry_1 as i64)));
    assert_eq!(sd1.get("active"), Some(&PyRep::Bool(true)));

    // 6. Activate second MCT slot: consumes remaining certificate (stack drops to 0, item deleted)
    let expected_expiry_2 = base_now + 30 * 86_400;
    let act2 = rpc(&ctx, &mut sess, "userSvc", "ActivateMultiTraining", &[])
        .await
        .expect("ActivateMultiTraining #2");
    let PyRep::KeyVal(act2_kv) = act2 else {
        panic!("expected KeyVal");
    };
    assert_eq!(act2_kv.get("slotActivated"), Some(&PyRep::Bool(true)));
    assert_eq!(
        act2_kv.get("expiryDate"),
        Some(&PyRep::Integer(expected_expiry_2 as i64))
    );

    // Verify certificate item was completely removed
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&mct_item_id), "Cert item must be removed when stack reaches 0");
    });

    // 7. GetMultiCharactersTrainingSlots returns 2 active slots
    let slots_2 = rpc(&ctx, &mut sess, "userSvc", "GetMultiCharactersTrainingSlots", &[])
        .await
        .expect("GetMultiCharactersTrainingSlots 2");
    let PyRep::Dict(dict_2) = slots_2 else {
        panic!("expected Dict");
    };
    assert_eq!(dict_2.len(), 2);

    // 8. Attempting a 3rd activation fails with NoMultiTrainingCertificateFound
    let act3_err = rpc(&ctx, &mut sess, "userSvc", "ActivateMultiTraining", &[])
        .await
        .expect_err("Activate with no cert remaining must fail");
    assert_eq!(act3_err.code, "UserError");
    assert_eq!(act3_err.message, "NoMultiTrainingCertificateFound");
}

#[tokio::test]
async fn test_feat_account_character_resculpt() {
    // parity: feat:account.character-resculpt
    eve_testkit::attest(&["feat:account.character-resculpt"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_and_select(&ctx, 60_002, "demo", "demo-secret", PILOT_1).await;
    let uid = sess.user_id.expect("user_id");

    // 1. Initial activation without token fails
    let no_resculpt_err = rpc(&ctx, &mut sess, "userSvc", "ActivateCharacterReSculpt", &[])
        .await
        .expect_err("Activate without cert must fail");
    assert_eq!(no_resculpt_err.code, "UserError");
    assert_eq!(no_resculpt_err.message, "NoResculptCertificateFound");

    // 2. Seed character resculpt token (type 28662) with quantity 2
    let resculpt_item_id = seed_item(&ctx, 28662, PILOT_1, 2);

    // 3. First activation: decrements stack from 2 to 1, grants full resculpt
    let resc1 = rpc(&ctx, &mut sess, "userSvc", "ActivateCharacterReSculpt", &[])
        .await
        .expect("ActivateCharacterReSculpt #1");
    assert_eq!(resc1, PyRep::Bool(true));

    // Verify stack decremented to 1 in inventory
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&resculpt_item_id).expect("resculpt item exists");
        assert_eq!(it.quantity, 1, "Stack must be decremented to 1");
    });

    // Verify resculpt token persisted across state queries in service_state_blobs
    ctx.db.with_snapshot(|snap| {
        let token_count = snap
            .service_state_blobs
            .get(&format!("account_resculpt_tokens:{uid}"))
            .expect("account_resculpt_tokens blob present");
        assert_eq!(token_count, "1");
        let pending = snap
            .service_state_blobs
            .get(&format!("resculpt_pending:{PILOT_1}"))
            .expect("resculpt_pending blob present");
        assert_eq!(pending, "true");
    });

    // 4. Second activation: consumes remaining token (stack drops to 0, item removed)
    let resc2 = rpc(&ctx, &mut sess, "userSvc", "ActivateCharacterReSculpt", &[])
        .await
        .expect("ActivateCharacterReSculpt #2");
    assert_eq!(resc2, PyRep::Bool(true));

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&resculpt_item_id), "Token item must be removed");
        let token_count = snap
            .service_state_blobs
            .get(&format!("account_resculpt_tokens:{uid}"))
            .expect("account_resculpt_tokens blob present");
        assert_eq!(token_count, "2");
    });

    // 5. Third activation without token fails
    let resc3_err = rpc(&ctx, &mut sess, "userSvc", "ActivateCharacterReSculpt", &[])
        .await
        .expect_err("Third activate must fail");
    assert_eq!(resc3_err.code, "UserError");
    assert_eq!(resc3_err.message, "NoResculptCertificateFound");
}

#[tokio::test]
async fn test_feat_account_banned_words() {
    // parity: feat:account.banned-words
    eve_testkit::attest(&["feat:account.banned-words"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_and_select(&ctx, 60_003, "demo", "demo-secret", PILOT_1).await;

    // 1. Clean single strings allowed
    for clean_word in ["FriendlyTrader", "TritaniumMiner", "JitaStationMaster", "SafeCargo"] {
        let rep = rpc(&ctx, &mut sess, "bannedwords", "check_words_allowed", &[PyRep::String(clean_word.into())])
            .await
            .expect("check_words_allowed clean");
        assert_eq!(rep, PyRep::Bool(true), "Clean word '{clean_word}' should be allowed");
    }

    // 2. Disallowed single strings with banned substrings and case-insensitivity
    for bad_word in ["scam", "SCAMMER_99", "BadWord", "Anti-Exploit", "server_hack_tool", "extreme_profanity"] {
        let rep = rpc(&ctx, &mut sess, "bannedwords", "check_words_allowed", &[PyRep::String(bad_word.into())])
            .await
            .expect("check_words_allowed bad");
        assert_eq!(rep, PyRep::Bool(false), "Bad word '{bad_word}' should be disallowed");
    }

    // 3. Candidate list validation: returns List of Bools matching each candidate
    let list_input = vec![
        PyRep::String("CleanPilot".into()),
        PyRep::String("SCAM".into()),
        PyRep::String("MinerCorp".into()),
        PyRep::String("bannedword".into()),
    ];
    let rep_list = rpc(&ctx, &mut sess, "bannedwords", "check_words_allowed", &[PyRep::List(list_input)])
        .await
        .expect("check_words_allowed list");
    let PyRep::List(results) = rep_list else {
        panic!("expected List of bools");
    };
    assert_eq!(results.len(), 4);
    assert_eq!(results[0], PyRep::Bool(true));
    assert_eq!(results[1], PyRep::Bool(false));
    assert_eq!(results[2], PyRep::Bool(true));
    assert_eq!(results[3], PyRep::Bool(false));

    // 4. check_words_allowed with options argument
    let clean_cand = "Valid Name";
    let opt_clean = rpc(
        &ctx,
        &mut sess,
        "bannedwords",
        "check_words_allowed",
        &[PyRep::String(clean_cand.into()), PyRep::Dict(vec![])],
    )
    .await
    .expect("check_words_allowed with options");
    assert_eq!(opt_clean, PyRep::Bool(true));

    let bad_cand = "hack client";
    let opt_bad = rpc(
        &ctx,
        &mut sess,
        "bannedwords",
        "check_words_allowed",
        &[PyRep::String(bad_cand.into()), PyRep::Dict(vec![])],
    )
    .await
    .expect("check_words_allowed with options bad");
    assert_eq!(opt_bad, PyRep::Bool(false));

    // 5. check_search_words_allowed validates search strings
    let search_clean = rpc(
        &ctx,
        &mut sess,
        "bannedwords",
        "check_search_words_allowed",
        &[PyRep::List(vec![
            PyRep::String("Raven".into()),
            PyRep::String("Torpedo".into()),
        ])],
    )
    .await
    .expect("check_search_words_allowed clean");
    assert_eq!(search_clean, PyRep::Bool(true));

    let search_bad = rpc(
        &ctx,
        &mut sess,
        "bannedwords",
        "check_search_words_allowed",
        &[PyRep::List(vec![
            PyRep::String("Raven".into()),
            PyRep::String("ScamBargain".into()),
        ])],
    )
    .await
    .expect("check_search_words_allowed bad");
    assert_eq!(search_bad, PyRep::Bool(false));
}

#[tokio::test]
async fn test_feat_account_in_game_reports() {
    // parity: feat:account.in-game-reports
    eve_testkit::attest(&["feat:account.in-game-reports"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // 1. Unauthenticated session rejected
    let mut unauth = SessionState::new(60_010);
    let unauth_err = rpc(&ctx, &mut unauth, "ingamereport", "report_macro_use", &[PyRep::Integer(PILOT_TARGET)])
        .await
        .expect_err("Unauthenticated report must fail");
    assert_eq!(unauth_err.code, "Unauthorized");

    // 2. Authenticate reporting pilot
    let mut sess = login_and_select(&ctx, 60_011, "demo", "demo-secret", PILOT_1).await;
    sess.corporation_id = Some(CORP_1);

    // 3. Submit reports across all 8 moderation categories
    let report_methods_and_data = [
        ("report_exploit_abuse", "exploit_abuse", PILOT_TARGET, "Citadel clipping", "Undocking inside tether boundary"),
        ("report_gambling", "gambling", PILOT_TARGET, "Third-party casino", "Spamming external casino URLs in local"),
        ("report_inappropriate_character_name", "inappropriate_character_name", PILOT_TARGET, "Offensive pilot name", "Breaches EULA character naming policy"),
        ("report_inappropriate_language", "inappropriate_language", PILOT_TARGET, "Harassment", "Repeated abusive hate speech in fleet chat"),
        ("report_macro_use", "macro_use", PILOT_TARGET, "Autopilot mining macro", "24/7 robotic mining loop without player response"),
        ("report_offensive_mail", "offensive_mail", 450_001, "Harassment eve-mail", "Threatening messages sent via in-game mail"),
        ("report_other", "other", PILOT_TARGET, "Undock griefing", "Spamming deployable containers on undock"),
        ("report_questionable_transaction", "questionable_transaction", 777_888, "Suspected RMT", "Tritanium sold for 50 billion ISK"),
    ];

    for (method, _cat, target_id, reason, details) in report_methods_and_data {
        let rep = rpc(
            &ctx,
            &mut sess,
            "ingamereport",
            method,
            &[
                PyRep::Integer(target_id),
                PyRep::String(reason.into()),
                PyRep::String(details.into()),
            ],
        )
        .await
        .expect(method);
        assert_eq!(rep, PyRep::None, "{method} must return PyRep::None");
    }

    // 4. Verify all 8 structured tickets are recorded in snap.service_state_blobs["ingamereport_tickets"]
    ctx.db.with_snapshot(|snap| {
        let blob = snap
            .service_state_blobs
            .get("ingamereport_tickets")
            .expect("ingamereport_tickets blob must exist");
        let tickets: Vec<InGameReportRecord> =
            serde_json::from_str(blob).expect("valid InGameReportRecord JSON array");
        assert_eq!(tickets.len(), 8, "Must contain exactly 8 recorded tickets");

        for (idx, (_, expected_cat, expected_target, expected_reason, expected_details)) in
            report_methods_and_data.iter().enumerate()
        {
            let t = &tickets[idx];
            assert_eq!(t.report_id, 700_001 + idx as i64);
            assert_eq!(t.reporter_char_id, PILOT_1);
            assert_eq!(t.reporter_corp_id, CORP_1);
            assert_eq!(t.reported_entity_id, *expected_target);
            assert_eq!(t.category, *expected_cat);
            assert_eq!(t.reason, *expected_reason);
            assert_eq!(t.details, *expected_details);
            assert!(t.created_at > 0);
        }
    });

    // 5. Verify get_persisted_reports returns all 8 reports
    let persisted = ingamereport_ops::get_persisted_reports(&ctx);
    assert_eq!(persisted.len(), 8, "get_persisted_reports must return all 8 reports");
    for (idx, r) in persisted.iter().enumerate() {
        assert_eq!(r.report_id, 700_001 + idx as i64);
        assert_eq!(r.reporter_char_id, PILOT_1);
        assert_eq!(r.reporter_corp_id, CORP_1);
    }
}
