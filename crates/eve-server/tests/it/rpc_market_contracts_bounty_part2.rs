//! Part 2 of integration tests covering bountyProxy and kill rights.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

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

#[tokio::test]
async fn test_bounty_proxy_and_kill_rights_rpcs() {
    // parity: rpc:bountyProxy.AddToBounty, rpc:bountyProxy.CancelSellKillRight, rpc:bountyProxy.GMClearBountyCache, rpc:bountyProxy.GMReimburseBounties, rpc:bountyProxy.GetBounties, rpc:bountyProxy.GetBountiesAndKillRights, rpc:bountyProxy.GetKillRightsOnCharacters, rpc:bountyProxy.GetMyBounties, rpc:bountyProxy.GetMyKillRights, rpc:bountyProxy.GetTopAllianceBounties
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 301, "demo", "demo-secret", 90_000_001).await;
    let _s2 = setup_pilot(&ctx, 302, "other", "other-secret", 90_000_002).await;
    let empty_kw = HashMap::new();

    // Give pilots ISK
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.balance = eve_db::IskCents::from_isk(10_000_000);
        }
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.balance = eve_db::IskCents::from_isk(10_000_000);
        }
    });

    let bal_before = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&90_000_001).unwrap().balance.0);

    // 1. AddToBounty: pilot 1 places 500,000 ISK bounty on pilot 2
    let add_res = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "AddToBounty",
            &[PyRep::Integer(90_000_002), PyRep::Real(500_000.0)],
            &empty_kw,
            None,
        )
        .await
        .expect("AddToBounty");
    let add_kv = kv_map(&add_res);
    assert_eq!(add_kv.get("targetID"), Some(&PyRep::Integer(90_000_002)));
    assert_eq!(add_kv.get("addedISK"), Some(&PyRep::Real(500_000.0)));

    let bal_after = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&90_000_001).unwrap().balance.0);
    assert_eq!(bal_before - bal_after, 50_000_000);

    // 2. GetBounties (respects owner filter)
    let bounties = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "GetBounties", &[PyRep::List(vec![PyRep::Integer(90_000_002)])], &empty_kw, None)
        .await
        .expect("GetBounties with filter");
    let PyRep::List(b_list) = bounties else {
        panic!("Expected list from GetBounties");
    };
    assert!(b_list
        .iter()
        .any(|b| kv_map(b).get("targetID") == Some(&PyRep::Integer(90_000_002))));

    // 3. GetMyBounties
    let my_bounties = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "GetMyBounties", &[], &empty_kw, None)
        .await
        .expect("GetMyBounties");
    let PyRep::List(mb_list) = my_bounties else {
        panic!("Expected list from GetMyBounties");
    };
    assert!(mb_list
        .iter()
        .any(|b| kv_map(b).get("targetID") == Some(&PyRep::Integer(90_000_002))));

    // 4. GetTopAllianceBounties
    let top_all = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetTopAllianceBounties",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("GetTopAllianceBounties");
    assert!(matches!(top_all, PyRep::List(_)));

    // 5. Create a kill right from pilot 1 against pilot 2, sell it, verify, then CancelSellKillRight
    let kr_id = 77_001;
    eve_server::services::combat_crimewatch::create_kill_right(
        &ctx,
        kr_id,
        90_000_001,
        90_000_002,
        1_000_000.0,
    );

    // GetMyKillRights: returns expiryTime and restrictedTo
    let my_kr = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "GetMyKillRights", &[], &empty_kw, None)
        .await
        .expect("GetMyKillRights");
    let PyRep::List(kr_list) = my_kr else {
        panic!("Expected list from GetMyKillRights");
    };
    let found_kr = kr_list
        .iter()
        .map(kv_map)
        .find(|k| k.get("killRightID") == Some(&PyRep::Integer(kr_id)))
        .expect("Found kill right in GetMyKillRights");
    assert!(found_kr.contains_key("expiryTime"));
    assert!(found_kr.contains_key("restrictedTo"));

    // GetKillRightsOnCharacters
    let kr_on_char = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetKillRightsOnCharacters",
            &[PyRep::List(vec![PyRep::Integer(90_000_002)])],
            &empty_kw,
            None,
        )
        .await
        .expect("GetKillRightsOnCharacters");
    let PyRep::List(krc_list) = kr_on_char else {
        panic!("Expected list from GetKillRightsOnCharacters");
    };
    assert!(krc_list
        .iter()
        .any(|k| kv_map(k).get("killRightID") == Some(&PyRep::Integer(kr_id))));

    // CancelSellKillRight
    let cancel_kr = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "CancelSellKillRight",
            &[PyRep::Integer(kr_id)],
            &empty_kw,
            None,
        )
        .await
        .expect("CancelSellKillRight");
    let cancel_kv = kv_map(&cancel_kr);
    assert_eq!(cancel_kv.get("forSale"), Some(&PyRep::Bool(false)));

    let db_kr_fee = ctx
        .db
        .with_snapshot(|snap| snap.kill_rights.get(&kr_id).map(|k| k.activation_fee.0));
    assert_eq!(db_kr_fee, Some(0));

    // 6. GetBountiesAndKillRights: returns ([(ownerID, bounty)], [killRight rows])
    let bakr = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetBountiesAndKillRights",
            &[
                PyRep::List(vec![PyRep::Integer(90_000_002)]),
                PyRep::List(vec![PyRep::Integer(90_000_002)]),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("GetBountiesAndKillRights");
    let PyRep::Tuple(bakr_tuple) = bakr else {
        panic!("Expected tuple from GetBountiesAndKillRights");
    };
    assert_eq!(bakr_tuple.len(), 2);
    let PyRep::List(b_entries) = &bakr_tuple[0] else { panic!("expected bounties list") };
    assert!(!b_entries.is_empty());
    let PyRep::List(kr_entries) = &bakr_tuple[1] else { panic!("expected kill rights list") };
    assert!(!kr_entries.is_empty());

    // 7. GMReimburseBounties
    ctx.db.provision_gm_account("gm_bounty", "gm-pass", 90_000_003, "GM Bounty");
    let mut gm = SessionState::new(303);
    common::login_pilot(&ctx, &mut gm, "gm_bounty", "gm-pass").await;
    ctx.dispatch_rpc(
        &mut gm,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_003)],
        &empty_kw,
        None,
    )
    .await
    .expect("SelectCharacterID GM");

    let p1_bal_before_refund = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&90_000_001).unwrap().balance.0);
    let reimb = ctx
        .dispatch_rpc(
            &mut gm,
            "bountyProxy",
            None,
            "GMReimburseBounties",
            &[PyRep::Integer(90_000_002)],
            &empty_kw,
            None,
        )
        .await
        .expect("GMReimburseBounties");
    let reimb_kv = kv_map(&reimb);
    assert_eq!(reimb_kv.get("amountISK"), Some(&PyRep::Real(500_000.0)));

    let p1_bal_after_refund = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&90_000_001).unwrap().balance.0);
    assert_eq!(p1_bal_after_refund - p1_bal_before_refund, 50_000_000);

    // Verify wallet journal has bounty_reimbursement entry
    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.wallet_journal
                .iter()
                .any(|e| e.character_id == 90_000_001 && e.entry_type == "bounty_reimbursement"),
            "Must have bounty_reimbursement in wallet journal"
        );
    });

    // 8. GMClearBountyCache: GM receives None, non-GM is rejected
    let clear = ctx
        .dispatch_rpc(
            &mut gm,
            "bountyProxy",
            None,
            "GMClearBountyCache",
            &[PyRep::Integer(0)],
            &empty_kw,
            None,
        )
        .await
        .expect("GMClearBountyCache");
    assert_eq!(clear, PyRep::None);

    let non_gm_clear = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GMClearBountyCache",
            &[PyRep::Integer(0)],
            &empty_kw,
            None,
        )
        .await;
    assert_eq!(non_gm_clear.unwrap_err().code, "CustomError");
}
