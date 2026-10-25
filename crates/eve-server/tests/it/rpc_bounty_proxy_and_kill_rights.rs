//! Integration tests covering all 19 RPC parity rows of bountyProxy and kill rights.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_mut_combat_runtime;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const P3: i64 = 90_000_003;
const GM_ID: i64 = 90_000_099;
const CORP_ID: i64 = 98_000_001;
const ALLIANCE_ID: i64 = 99_000_001;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn kv_map(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.clone(),
        PyRep::Tuple(t) => t.get(1).map(kv_map).unwrap_or_default(),
        PyRep::List(l) => l.first().map(kv_map).unwrap_or_default(),
        PyRep::Dict(entries) => entries
            .iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(s) | PyRep::Token(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect(),
        _ => HashMap::new(),
    }
}

fn unpack_rank_entry(rep: &PyRep) -> (i64, HashMap<String, PyRep>) {
    match rep {
        PyRep::Tuple(pair) if pair.len() == 2 => {
            let rank = pair[0].as_i64().unwrap_or(0);
            let obj = kv_map(&pair[1]);
            (rank, obj)
        }
        _ => panic!("Expected 2-tuple (rank, bountyObj)"),
    }
}

#[tokio::test]
async fn test_bounty_proxy_bounties_and_pool_search() {
    // parity: rpc:bountyProxy.AddToBounty
    // parity: rpc:bountyProxy.GetBounties
    // parity: rpc:bountyProxy.GetBountiesAndKillRights
    // parity: rpc:bountyProxy.GetMyBounties
    // parity: rpc:bountyProxy.SearchCharBounties
    // parity: rpc:bountyProxy.SearchCorpBounties
    // parity: rpc:bountyProxy.SearchAllianceBounties
    // parity: rpc:bountyProxy.GMClearBountyCache
    // parity: rpc:bountyProxy.GMReimburseBounties
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let empty_kw = HashMap::new();
    let mut s1 = setup_pilot(&ctx, 401, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 402, "other", "other-secret", P2).await;

    // Seed pilot balances, corporations, and alliances
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.balance = eve_db::IskCents::from_isk(50_000_000);
            ch.corporation_id = CORP_ID;
            ch.alliance_id = Some(ALLIANCE_ID);
        }
        if let Some(ch) = snap.characters.get_mut(&P2) {
            ch.balance = eve_db::IskCents::from_isk(10_000_000);
            ch.corporation_id = CORP_ID;
            ch.alliance_id = Some(ALLIANCE_ID);
            ch.name = "Target Pilot".into();
        }
        if let Some(c) = snap.corporations.get_mut(&CORP_ID) {
            c.name = "Bounty Target Corp".into();
            c.ticker = "BTC".into();
            c.ceo_id = P2;
            c.alliance_id = Some(ALLIANCE_ID);
        } else {
            snap.corporations.insert(CORP_ID, eve_db::CorporationRecord {
                corporation_id: CORP_ID,
                name: "Bounty Target Corp".into(),
                ticker: "BTC".into(),
                ceo_id: P2,
                alliance_id: Some(ALLIANCE_ID),
                ..Default::default()
            });
        }
        if let Some(a) = snap.alliances.get_mut(&ALLIANCE_ID) {
            a.name = "Bounty Alliance".into();
            a.short_name = "BALLI".into();
            a.executor_corp_id = CORP_ID;
            a.member_corp_ids = [CORP_ID].into_iter().collect();
        } else {
            snap.alliances.insert(ALLIANCE_ID, eve_db::AllianceRecord {
                alliance_id: ALLIANCE_ID,
                name: "Bounty Alliance".into(),
                short_name: "BALLI".into(),
                executor_corp_id: CORP_ID,
                member_corp_ids: [CORP_ID].into_iter().collect(),
            });
        }
    });

    // 1. AddToBounty validation & success
    let err_low = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "AddToBounty", &[PyRep::Integer(P2), PyRep::Real(50_000.0)], &empty_kw, None)
        .await
        .expect_err("bounty below minimum 100k ISK must fail");
    assert_eq!(err_low.code, "BountyAmountTooLow");

    let err_neg = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "AddToBounty", &[PyRep::Integer(P2), PyRep::Real(-500_000.0)], &empty_kw, None)
        .await
        .expect_err("negative bounty must fail");
    assert_eq!(err_neg.code, "BountyAmountTooLow");

    let err_zero_target = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "AddToBounty", &[PyRep::Integer(0), PyRep::Real(500_000.0)], &empty_kw, None)
        .await
        .expect_err("zero target ID must fail");
    assert_eq!(err_zero_target.code, "BountyAmountTooLow");

    let err_funds = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "AddToBounty", &[PyRep::Integer(P2), PyRep::Real(100_000_000.0)], &empty_kw, None)
        .await
        .expect_err("insufficient funds must fail");
    assert_eq!(err_funds.code, "InsufficientFunds");

    let add1 = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "AddToBounty", &[PyRep::Integer(P2), PyRep::Real(2_000_000.0)], &empty_kw, None)
        .await
        .expect("AddToBounty on pilot P2");
    let kv1 = kv_map(&add1);
    assert_eq!(kv1.get("targetID"), Some(&PyRep::Integer(P2)));
    assert_eq!(kv1.get("addedISK"), Some(&PyRep::Real(2_000_000.0)));
    assert_eq!(kv1.get("totalBountyISK"), Some(&PyRep::Real(2_000_000.0)));

    let add2 = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "AddToBounty", &[PyRep::Integer(P2), PyRep::Real(3_000_000.0)], &empty_kw, None)
        .await
        .expect("AddToBounty cumulative on pilot P2");
    let kv2 = kv_map(&add2);
    assert_eq!(kv2.get("totalBountyISK"), Some(&PyRep::Real(5_000_000.0)));

    // Also add bounties to corporation and alliance for search tests
    let _ = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "AddToBounty", &[PyRep::Integer(CORP_ID), PyRep::Real(4_000_000.0)], &empty_kw, None)
        .await
        .expect("AddToBounty on corp");
    let _ = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "AddToBounty", &[PyRep::Integer(ALLIANCE_ID), PyRep::Real(8_000_000.0)], &empty_kw, None)
        .await
        .expect("AddToBounty on alliance");

    // 2. GetBounties (V24 wire: list of 2-tuples `(targetID, bountyObj)`)
    let b_res = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "GetBounties", &[PyRep::List(vec![PyRep::Integer(P2)])], &empty_kw, None)
        .await
        .expect("GetBounties for P2");
    let PyRep::List(b_list) = b_res else { panic!("expected list from GetBounties") };
    assert!(!b_list.is_empty());
    let (target_id, p2_entry) = match &b_list[0] {
        PyRep::Tuple(pair) => (pair[0].as_i64().unwrap(), kv_map(&pair[1])),
        _ => panic!("expected 2-tuple (targetID, bountyObj)"),
    };
    assert_eq!(target_id, P2);
    assert_eq!(p2_entry.get("targetID"), Some(&PyRep::Integer(P2)));
    assert_eq!(p2_entry.get("bountyISK"), Some(&PyRep::Real(5_000_000.0)));

    // 3. GetMyBounties (V24 wire: objects have targetID, corporationID, allianceID, amount)
    let my_bounties_p1 = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "GetMyBounties", &[], &empty_kw, None)
        .await
        .expect("GetMyBounties for contributor P1");
    let PyRep::List(mb1) = my_bounties_p1 else { panic!("expected list from GetMyBounties") };
    let p2_my = mb1.iter().map(kv_map).find(|k| k.get("targetID") == Some(&PyRep::Integer(P2))).expect("P2 found in my bounties");
    assert_eq!(p2_my.get("amount"), Some(&PyRep::Real(5_000_000.0)));
    assert_eq!(p2_my.get("corporationID"), Some(&PyRep::Integer(CORP_ID)));
    assert_eq!(p2_my.get("allianceID"), Some(&PyRep::Integer(ALLIANCE_ID)));

    let my_bounties_p2 = ctx
        .dispatch_rpc(&mut s2, "bountyProxy", None, "GetMyBounties", &[], &empty_kw, None)
        .await
        .expect("GetMyBounties for target P2");
    let PyRep::List(mb2) = my_bounties_p2 else { panic!("expected list from GetMyBounties") };
    assert!(mb2.iter().map(kv_map).any(|k| k.get("targetID") == Some(&PyRep::Integer(P2))));

    // 4. SearchCharBounties (V24 wire: list of 2-tuples `(rank, bountyObj)`)
    let s_char_id = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "SearchCharBounties", &[PyRep::Integer(P2)], &empty_kw, None)
        .await
        .expect("SearchCharBounties by ID");
    let PyRep::List(sc_id_list) = s_char_id else { panic!("expected list") };
    let (rank1, sc_obj1) = unpack_rank_entry(&sc_id_list[0]);
    assert_eq!(rank1, 1);
    assert_eq!(sc_obj1.get("targetID"), Some(&PyRep::Integer(P2)));
    assert_eq!(sc_obj1.get("name"), Some(&PyRep::String("Target Pilot".into())));
    assert_eq!(sc_obj1.get("bountyISK"), Some(&PyRep::Real(5_000_000.0)));
    assert_eq!(sc_obj1.get("bounty"), Some(&PyRep::Real(5_000_000.0)));

    let s_char_name = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "SearchCharBounties", &[PyRep::String("target".into())], &empty_kw, None)
        .await
        .expect("SearchCharBounties by name");
    let PyRep::List(sc_name_list) = s_char_name else { panic!("expected list") };
    let (_, sc_obj2) = unpack_rank_entry(&sc_name_list[0]);
    assert_eq!(sc_obj2.get("targetID"), Some(&PyRep::Integer(P2)));

    // 5. SearchCorpBounties (V24 wire: list of 2-tuples `(rank, bountyObj)`)
    let s_corp_id = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "SearchCorpBounties", &[PyRep::Integer(CORP_ID)], &empty_kw, None)
        .await
        .expect("SearchCorpBounties by ID");
    let PyRep::List(scorp_list) = s_corp_id else { panic!("expected list") };
    let (crank1, scorp_obj1) = unpack_rank_entry(&scorp_list[0]);
    assert_eq!(crank1, 1);
    assert_eq!(scorp_obj1.get("targetID"), Some(&PyRep::Integer(CORP_ID)));
    assert_eq!(scorp_obj1.get("name"), Some(&PyRep::String("Bounty Target Corp".into())));

    let s_corp_name = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "SearchCorpBounties", &[PyRep::String("bounty target".into())], &empty_kw, None)
        .await
        .expect("SearchCorpBounties by name");
    let PyRep::List(scorp_name_list) = s_corp_name else { panic!("expected list") };
    let (_, scorp_obj2) = unpack_rank_entry(&scorp_name_list[0]);
    assert_eq!(scorp_obj2.get("targetID"), Some(&PyRep::Integer(CORP_ID)));

    // 6. SearchAllianceBounties (V24 wire: list of 2-tuples `(rank, bountyObj)`)
    let s_alli_id = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "SearchAllianceBounties", &[PyRep::Integer(ALLIANCE_ID)], &empty_kw, None)
        .await
        .expect("SearchAllianceBounties by ID");
    let PyRep::List(salli_list) = s_alli_id else { panic!("expected list") };
    let (arank1, salli_obj1) = unpack_rank_entry(&salli_list[0]);
    assert_eq!(arank1, 1);
    assert_eq!(salli_obj1.get("targetID"), Some(&PyRep::Integer(ALLIANCE_ID)));
    assert_eq!(salli_obj1.get("name"), Some(&PyRep::String("Bounty Alliance".into())));

    let s_alli_name = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "SearchAllianceBounties", &[PyRep::String("bounty alliance".into())], &empty_kw, None)
        .await
        .expect("SearchAllianceBounties by name");
    let PyRep::List(salli_name_list) = s_alli_name else { panic!("expected list") };
    let (_, salli_obj2) = unpack_rank_entry(&salli_name_list[0]);
    assert_eq!(salli_obj2.get("targetID"), Some(&PyRep::Integer(ALLIANCE_ID)));

    // 7. GetBountiesAndKillRights: takes (bountyTargetIDs, killRightTargetIDs), returns (bounties, killRights)
    let bakr = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetBountiesAndKillRights",
            &[
                PyRep::List(vec![PyRep::Integer(P2), PyRep::Integer(CORP_ID)]),
                PyRep::List(vec![PyRep::Integer(P2)]),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("GetBountiesAndKillRights");
    let PyRep::Tuple(bakr_tuple) = bakr else { panic!("expected tuple") };
    assert_eq!(bakr_tuple.len(), 2);
    let PyRep::List(b_entries) = &bakr_tuple[0] else { panic!("expected bounties list") };
    assert!(b_entries.iter().any(|b| matches!(b, PyRep::Tuple(t) if t.len() == 2 && t[0] == PyRep::Integer(P2))));

    // 8. GMClearBountyCache & GMReimburseBounties
    ctx.db.provision_gm_account("gm_bounty_user", "gm-secret-pass", GM_ID, "GM Bounty Officer");
    let mut gm_sess = SessionState::new(403);
    common::login_pilot(&ctx, &mut gm_sess, "gm_bounty_user", "gm-secret-pass").await;
    ctx.dispatch_rpc(
        &mut gm_sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(GM_ID)],
        &empty_kw,
        None,
    )
    .await
    .expect("SelectCharacterID for GM");

    // Non-GM rejected for GMClearBountyCache
    let non_gm_cache = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "GMClearBountyCache", &[], &empty_kw, None)
        .await;
    assert!(non_gm_cache.is_err(), "non-GM must be denied GMClearBountyCache");

    // GM succeeds for GMClearBountyCache
    let gm_cache_res = ctx
        .dispatch_rpc(&mut gm_sess, "bountyProxy", None, "GMClearBountyCache", &[], &empty_kw, None)
        .await
        .expect("GM clear bounty cache");
    assert_eq!(gm_cache_res, PyRep::None);

    // Non-GM rejected for GMReimburseBounties
    let non_gm_reimb = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "GMReimburseBounties", &[PyRep::Integer(P2)], &empty_kw, None)
        .await;
    assert!(non_gm_reimb.is_err(), "non-GM must be denied GMReimburseBounties");

    // GM reimburses bounty on P2 -> P1 gets refunded 5,000,000 ISK
    let p1_bal_before = ctx.db.with_snapshot(|snap| snap.characters.get(&P1).unwrap().balance.0);
    let reimb = ctx
        .dispatch_rpc(&mut gm_sess, "bountyProxy", None, "GMReimburseBounties", &[PyRep::Integer(P2)], &empty_kw, None)
        .await
        .expect("GMReimburseBounties on P2");
    let reimb_kv = kv_map(&reimb);
    assert_eq!(reimb_kv.get("action"), Some(&PyRep::String("reimburse".into())));
    assert_eq!(reimb_kv.get("targetID"), Some(&PyRep::Integer(P2)));
    assert_eq!(reimb_kv.get("amountISK"), Some(&PyRep::Real(5_000_000.0)));

    let p1_bal_after = ctx.db.with_snapshot(|snap| snap.characters.get(&P1).unwrap().balance.0);
    assert_eq!(p1_bal_after - p1_bal_before, 500_000_000); // 5M ISK in cents

    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.wallet_journal
                .iter()
                .any(|e| e.character_id == P1 && e.entry_type == "bounty_reimbursement"),
            "wallet journal must record bounty_reimbursement"
        );
    });
}

#[tokio::test]
async fn test_bounty_proxy_kill_rights_and_marketplace() {
    // parity: rpc:bountyProxy.SellKillRight
    // parity: rpc:bountyProxy.CancelSellKillRight
    // parity: rpc:bountyProxy.GetMyKillRights
    // parity: rpc:bountyProxy.GetKillRightsOnCharacters
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let p3_uid = ctx.db.provision_player_account("third", "third-secret");
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&P1).cloned().unwrap();
        ch.character_id = P3;
        ch.user_id = p3_uid;
        ch.name = "Third Pilot".into();
        snap.characters.insert(P3, ch);
    });

    let empty_kw = HashMap::new();
    let mut s1 = setup_pilot(&ctx, 501, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 502, "other", "other-secret", P2).await;
    let mut s3 = setup_pilot(&ctx, 503, "third", "third-secret", P3).await;

    // Create a kill right against P2 owned by P1
    let kr_id = 88_001;
    eve_server::services::combat_crimewatch::create_kill_right(&ctx, kr_id, P1, P2, 0.0);

    // 1. GetMyKillRights
    let my_kr1 = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "GetMyKillRights", &[], &empty_kw, None)
        .await
        .expect("GetMyKillRights for P1");
    let PyRep::List(kr1_list) = my_kr1 else { panic!("expected list") };
    let found_kr = kr1_list
        .iter()
        .map(kv_map)
        .find(|k| k.get("killRightID") == Some(&PyRep::Integer(kr_id)))
        .expect("found kill right in GetMyKillRights");
    assert_eq!(found_kr.get("fromID"), Some(&PyRep::Integer(P1)));
    assert_eq!(found_kr.get("toID"), Some(&PyRep::Integer(P2)));
    assert_eq!(found_kr.get("price"), Some(&PyRep::Real(0.0)));
    assert_eq!(found_kr.get("activeSuspect"), Some(&PyRep::Bool(false)));
    assert!(found_kr.contains_key("expiryTime"));
    assert_eq!(found_kr.get("restrictedTo"), Some(&PyRep::None));

    // Target pilot P2 also sees it in GetMyKillRights
    let my_kr2 = ctx
        .dispatch_rpc(&mut s2, "bountyProxy", None, "GetMyKillRights", &[], &empty_kw, None)
        .await
        .expect("GetMyKillRights for P2");
    let PyRep::List(kr2_list) = my_kr2 else { panic!("expected list") };
    assert!(kr2_list.iter().map(kv_map).any(|k| k.get("killRightID") == Some(&PyRep::Integer(kr_id))));

    // 2. GetKillRightsOnCharacters
    let kr_on_chars = ctx
        .dispatch_rpc(
            &mut s3,
            "bountyProxy",
            None,
            "GetKillRightsOnCharacters",
            &[PyRep::List(vec![PyRep::Integer(P2)])],
            &empty_kw,
            None,
        )
        .await
        .expect("GetKillRightsOnCharacters for P2");
    let PyRep::List(krc_list) = kr_on_chars else { panic!("expected list") };
    assert!(krc_list.iter().map(kv_map).any(|k| k.get("killRightID") == Some(&PyRep::Integer(kr_id))));

    // 3. SellKillRight (V24 wire: returns List of killRight objects)
    let not_owner_err = ctx
        .dispatch_rpc(
            &mut s2,
            "bountyProxy",
            None,
            "SellKillRight",
            &[PyRep::Integer(kr_id), PyRep::Real(2_000_000.0)],
            &empty_kw,
            None,
        )
        .await
        .expect_err("non-owner sell must fail");
    assert_eq!(not_owner_err.code, "NotKillRightOwner");

    let invalid_target_err = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "SellKillRight",
            &[PyRep::Integer(kr_id), PyRep::Real(2_000_000.0), PyRep::Integer(99_999_999)],
            &empty_kw,
            None,
        )
        .await
        .expect_err("invalid restricted target must fail");
    assert_eq!(invalid_target_err.code, "InvalidKillRightRestrictionTarget");

    let sell1 = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "SellKillRight",
            &[PyRep::Integer(kr_id), PyRep::Real(2_500_000.0)],
            &empty_kw,
            None,
        )
        .await
        .expect("SellKillRight public");
    let PyRep::List(sell1_list) = sell1 else { panic!("expected list from SellKillRight") };
    assert_eq!(sell1_list.len(), 1);
    let sell1_kv = kv_map(&sell1_list[0]);
    assert_eq!(sell1_kv.get("killRightID"), Some(&PyRep::Integer(kr_id)));
    assert_eq!(sell1_kv.get("priceISK"), Some(&PyRep::Real(2_500_000.0)));
    assert_eq!(sell1_kv.get("forSale"), Some(&PyRep::Bool(true)));
    assert_eq!(sell1_kv.get("restrictedTo"), Some(&PyRep::None));

    let sell2 = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "SellKillRight",
            &[PyRep::Integer(kr_id), PyRep::Real(1_500_000.0), PyRep::Integer(P3)],
            &empty_kw,
            None,
        )
        .await
        .expect("SellKillRight restricted to P3");
    let PyRep::List(sell2_list) = sell2 else { panic!("expected list from SellKillRight restricted") };
    assert_eq!(sell2_list.len(), 1);
    let sell2_kv = kv_map(&sell2_list[0]);
    assert_eq!(sell2_kv.get("priceISK"), Some(&PyRep::Real(1_500_000.0)));
    assert_eq!(sell2_kv.get("restrictedTo"), Some(&PyRep::Integer(P3)));

    // 4. CancelSellKillRight (takes killRightID and toID)
    let not_owner_cancel = ctx
        .dispatch_rpc(
            &mut s2,
            "bountyProxy",
            None,
            "CancelSellKillRight",
            &[PyRep::Integer(kr_id), PyRep::Integer(P2)],
            &empty_kw,
            None,
        )
        .await
        .expect_err("non-owner cancel must fail");
    assert_eq!(not_owner_cancel.code, "NotKillRightOwner");

    let cancel = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "CancelSellKillRight",
            &[PyRep::Integer(kr_id), PyRep::Integer(P2)],
            &empty_kw,
            None,
        )
        .await
        .expect("CancelSellKillRight");
    let cancel_kv = kv_map(&cancel);
    assert_eq!(cancel_kv.get("killRightID"), Some(&PyRep::Integer(kr_id)));
    assert_eq!(cancel_kv.get("forSale"), Some(&PyRep::Bool(false)));

    ctx.db.with_snapshot(|snap| {
        let kr = snap.kill_rights.get(&kr_id).expect("kr exists");
        assert_eq!(kr.activation_fee.0, 0);
        assert!(!snap.combat_crimewatch_state.kill_right_restricted_to.contains_key(&kr_id));
    });
}

#[tokio::test]
async fn test_bounty_proxy_leaderboards() {
    // parity: rpc:bountyProxy.GetTopPilotBounties
    // parity: rpc:bountyProxy.GetTopCorpBounties
    // parity: rpc:bountyProxy.GetTopAllianceBounties
    // parity: rpc:bountyProxy.GetTopPilotBountyHunters
    // parity: rpc:bountyProxy.GetTopCorporationBountyHunters
    // parity: rpc:bountyProxy.GetTopAllianceBountyHunters
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let empty_kw = HashMap::new();
    let mut s1 = setup_pilot(&ctx, 601, "demo", "demo-secret", P1).await;

    // Seed characters, corp, alliance
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.corporation_id = CORP_ID;
            ch.alliance_id = Some(ALLIANCE_ID);
        }
        if let Some(ch) = snap.characters.get_mut(&P2) {
            ch.corporation_id = CORP_ID;
            ch.alliance_id = Some(ALLIANCE_ID);
        }
        snap.corporations.entry(CORP_ID).or_insert_with(|| eve_db::CorporationRecord {
            corporation_id: CORP_ID,
            name: "Leaderboard Corp".into(),
            ticker: "LBC".into(),
            ceo_id: P1,
            alliance_id: Some(ALLIANCE_ID),
            ..Default::default()
        });
        snap.alliances.entry(ALLIANCE_ID).or_insert_with(|| eve_db::AllianceRecord {
            alliance_id: ALLIANCE_ID,
            name: "Leaderboard Alliance".into(),
            short_name: "LBA".into(),
            executor_corp_id: CORP_ID,
            member_corp_ids: [CORP_ID].into_iter().collect(),
        });
    });

    // Populate bounty pools and hunter claimed pools directly in combat runtime
    with_mut_combat_runtime(&ctx, |rt| {
        rt.player_bounty_pool.insert(P1, 10_000_000.0);
        rt.player_bounty_pool.insert(P2, 25_000_000.0);
        rt.player_bounty_pool.insert(CORP_ID, 50_000_000.0);
        rt.player_bounty_pool.insert(ALLIANCE_ID, 100_000_000.0);

        rt.bounty_hunter_claimed.insert(P1, 30_000_000.0);
        rt.bounty_hunter_claimed.insert(P2, 70_000_000.0);
    });

    // 1. GetTopPilotBounties (page=1, returns List sorted desc: P2 first with 25M, P1 second with 10M)
    let top_pilots = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "GetTopPilotBounties", &[PyRep::Integer(1)], &empty_kw, None)
        .await
        .expect("GetTopPilotBounties");
    let PyRep::List(tp_list) = top_pilots else { panic!("expected list") };
    assert!(!tp_list.is_empty());
    let tp0 = kv_map(&tp_list[0]);
    assert_eq!(tp0.get("targetID"), Some(&PyRep::Integer(P2)));
    assert_eq!(tp0.get("bountyISK"), Some(&PyRep::Real(25_000_000.0)));

    // 2. GetTopCorpBounties (page=1, returns List)
    let top_corps = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "GetTopCorpBounties", &[PyRep::Integer(1)], &empty_kw, None)
        .await
        .expect("GetTopCorpBounties");
    let PyRep::List(tc_list) = top_corps else { panic!("expected list") };
    assert!(tc_list.iter().map(kv_map).any(|k| k.get("targetID") == Some(&PyRep::Integer(CORP_ID))));

    // 3. GetTopAllianceBounties (page=1, returns List)
    let top_alliances = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "GetTopAllianceBounties", &[PyRep::Integer(1)], &empty_kw, None)
        .await
        .expect("GetTopAllianceBounties");
    let PyRep::List(ta_list) = top_alliances else { panic!("expected list") };
    assert!(ta_list.iter().map(kv_map).any(|k| k.get("targetID") == Some(&PyRep::Integer(ALLIANCE_ID))));

    // 4. GetTopPilotBountyHunters (page=1, V24 wire: 2-tuple `(rows, timestamp)`, client subscripts `result[0]`)
    let top_hunters = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "GetTopPilotBountyHunters", &[PyRep::Integer(1)], &empty_kw, None)
        .await
        .expect("GetTopPilotBountyHunters");
    let PyRep::Tuple(th_tuple) = top_hunters else { panic!("expected 2-tuple") };
    assert_eq!(th_tuple.len(), 2);
    let PyRep::List(th_list) = &th_tuple[0] else { panic!("expected list in result[0]") };
    assert!(!th_list.is_empty());
    let th0 = kv_map(&th_list[0]);
    assert_eq!(th0.get("targetID"), Some(&PyRep::Integer(P2)));
    assert_eq!(th0.get("bountyISK"), Some(&PyRep::Real(70_000_000.0)));
    assert!(th_tuple[1].as_i64().unwrap_or(0) > 0);

    // 5. GetTopCorporationBountyHunters (page=1, V24 wire: 2-tuple `(rows, timestamp)`)
    let top_corp_hunters = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "GetTopCorporationBountyHunters", &[PyRep::Integer(1)], &empty_kw, None)
        .await
        .expect("GetTopCorporationBountyHunters");
    let PyRep::Tuple(tch_tuple) = top_corp_hunters else { panic!("expected 2-tuple") };
    assert_eq!(tch_tuple.len(), 2);
    let PyRep::List(tch_list) = &tch_tuple[0] else { panic!("expected list in result[0]") };
    assert!(tch_list.iter().map(kv_map).any(|k| k.get("targetID") == Some(&PyRep::Integer(CORP_ID))));

    // 6. GetTopAllianceBountyHunters (page=1, V24 wire: 2-tuple `(rows, timestamp)`)
    let top_alli_hunters = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "GetTopAllianceBountyHunters", &[PyRep::Integer(1)], &empty_kw, None)
        .await
        .expect("GetTopAllianceBountyHunters");
    let PyRep::Tuple(tah_tuple) = top_alli_hunters else { panic!("expected 2-tuple") };
    assert_eq!(tah_tuple.len(), 2);
    let PyRep::List(tah_list) = &tah_tuple[0] else { panic!("expected list in result[0]") };
    assert!(tah_list.iter().map(kv_map).any(|k| k.get("targetID") == Some(&PyRep::Integer(ALLIANCE_ID))));
}
