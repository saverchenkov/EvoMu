//! Integration tests covering open RPC parity rows across Bounty Proxy and Crimewatch services.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{with_combat_runtime, SafetyLevel};
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
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

fn unpack_hunter_rows(rep: PyRep) -> Vec<PyRep> {
    match rep {
        PyRep::Tuple(t) => match t.into_iter().next() {
            Some(PyRep::List(l)) => l,
            _ => vec![],
        },
        PyRep::List(l) => l,
        _ => vec![],
    }
}

#[tokio::test]
async fn test_bounty_proxy_operations() {
    // parity: rpc:bountyProxy.GetTopAllianceBountyHunters
    // parity: rpc:bountyProxy.GetTopCorpBounties
    // parity: rpc:bountyProxy.GetTopCorporationBountyHunters
    // parity: rpc:bountyProxy.GetTopPilotBounties
    // parity: rpc:bountyProxy.GetTopPilotBountyHunters
    // parity: rpc:bountyProxy.SearchAllianceBounties
    // parity: rpc:bountyProxy.SearchCharBounties
    // parity: rpc:bountyProxy.SearchCorpBounties
    // parity: rpc:bountyProxy.SellKillRight
    // parity: rpc:bountyProxy.clear_memoized
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 1101, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 1102, "other", "other-secret", P2).await;
    let kw = HashMap::new();

    // 1. clear_memoized returns PyNone
    let clear_res = ctx
        .dispatch_rpc(&mut s1, "bountyProxy", None, "clear_memoized", &[], &kw, None)
        .await
        .expect("clear_memoized");
    assert_eq!(clear_res, PyRep::None);

    // Seed test corp, alliance, and ISK
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.balance = eve_db::IskCents::from_isk(50_000_000);
            ch.corporation_id = CORP_ID;
            ch.alliance_id = Some(ALLIANCE_ID);
        }
        if let Some(ch) = snap.characters.get_mut(&P2) {
            ch.balance = eve_db::IskCents::from_isk(50_000_000);
            ch.corporation_id = CORP_ID;
            ch.alliance_id = Some(ALLIANCE_ID);
        }
        snap.corporations.entry(CORP_ID).or_insert_with(|| eve_db::CorporationRecord {
            corporation_id: CORP_ID,
            name: "Vanguard Federal Navy".into(),
            ticker: "VFN".into(),
            ceo_id: P2,
            alliance_id: Some(ALLIANCE_ID),
            ..Default::default()
        });
        snap.alliances.entry(ALLIANCE_ID).or_insert_with(|| eve_db::AllianceRecord {
            alliance_id: ALLIANCE_ID,
            name: "Northern Coalition Federation".into(),
            short_name: "NCF".into(),
            executor_corp_id: CORP_ID,
            member_corp_ids: [CORP_ID].into_iter().collect(),
        });
    });

    // 2. Add bounties: P1 places 500,000 ISK bounty on P2
    ctx.dispatch_rpc(&mut s1, "bountyProxy", None, "AddToBounty", &[PyRep::Integer(P2), PyRep::Real(500_000.0)], &kw, None).await.expect("AddToBounty on pilot");

    // Seed pilot, corporation, and alliance hunter leaderboard entries via a real bounty-claiming kill
    let claimed_isk = eve_server::services::combat_crimewatch::claim_player_bounty_on_kill(&ctx, P1, P2, 603);
    assert!(claimed_isk > 0.0, "claim_player_bounty_on_kill must reward bounty ISK to killer");

    // Place bounties on Corp and Alliance via AddToBounty RPC
    ctx.dispatch_rpc(&mut s1, "bountyProxy", None, "AddToBounty", &[PyRep::Integer(CORP_ID), PyRep::Real(2_000_000.0)], &kw, None).await.expect("AddToBounty on corp");
    ctx.dispatch_rpc(&mut s1, "bountyProxy", None, "AddToBounty", &[PyRep::Integer(ALLIANCE_ID), PyRep::Real(5_000_000.0)], &kw, None).await.expect("AddToBounty on alliance");

    // 3. SearchCharBounties: search pilot bounties by string or ID
    let char_bounties = ctx.dispatch_rpc(&mut s1, "bountyProxy", None, "SearchCharBounties", &[PyRep::Integer(P2)], &kw, None).await.expect("SearchCharBounties");
    let PyRep::List(char_rows) = char_bounties else { panic!("Expected PyList from SearchCharBounties"); };
    assert!(!char_rows.is_empty(), "SearchCharBounties should return target P2");
    let p2_bounty = char_rows.iter().map(kv_map).find(|m| m.get("targetID") == Some(&PyRep::Integer(P2))).expect("P2 found in char bounties");
    assert_eq!(p2_bounty.get("bountyISK"), Some(&PyRep::Real(500_000.0 - claimed_isk)));

    // 4. SearchCorpBounties: search corp bounties by corp ID
    let corp_bounties = ctx.dispatch_rpc(&mut s1, "bountyProxy", None, "SearchCorpBounties", &[PyRep::Integer(CORP_ID)], &kw, None).await.expect("SearchCorpBounties");
    let PyRep::List(corp_rows) = corp_bounties else { panic!("Expected PyList from SearchCorpBounties"); };
    let corp_bounty = corp_rows.iter().map(kv_map).find(|m| m.get("targetID") == Some(&PyRep::Integer(CORP_ID))).expect("Corp found in corp bounties");
    assert_eq!(corp_bounty.get("bountyISK"), Some(&PyRep::Real(2_000_000.0)));

    // 5. SearchAllianceBounties: search alliance bounties by alliance ID
    let all_bounties = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "SearchAllianceBounties",
            &[PyRep::Integer(ALLIANCE_ID)],
            &kw,
            None,
        )
        .await
        .expect("SearchAllianceBounties");
    let PyRep::List(all_rows) = all_bounties else {
        panic!("Expected PyList from SearchAllianceBounties");
    };
    let all_bounty = all_rows
        .iter()
        .map(kv_map)
        .find(|m| m.get("targetID") == Some(&PyRep::Integer(ALLIANCE_ID)))
        .expect("Alliance found in alliance bounties");
    assert_eq!(all_bounty.get("bountyISK"), Some(&PyRep::Real(5_000_000.0)));

    // 6. GetTopPilotBounties
    let top_pilots = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetTopPilotBounties",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetTopPilotBounties");
    let PyRep::List(top_pilot_rows) = top_pilots else {
        panic!("Expected PyList from GetTopPilotBounties");
    };
    assert!(
        top_pilot_rows
            .iter()
            .map(kv_map)
            .any(|m| m.get("targetID") == Some(&PyRep::Integer(P2))),
        "P2 must be in top pilot bounties"
    );

    // 7. GetTopCorpBounties
    let top_corps = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetTopCorpBounties",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetTopCorpBounties");
    let PyRep::List(top_corp_rows) = top_corps else {
        panic!("Expected PyList from GetTopCorpBounties");
    };
    assert!(
        top_corp_rows
            .iter()
            .map(kv_map)
            .any(|m| m.get("targetID") == Some(&PyRep::Integer(CORP_ID))),
        "CORP_ID must be in top corp bounties"
    );

    // 8. GetTopPilotBountyHunters
    let top_pilot_hunters = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetTopPilotBountyHunters",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetTopPilotBountyHunters");
    let hunter_rows = unpack_hunter_rows(top_pilot_hunters);
    assert!(
        hunter_rows
            .iter()
            .map(kv_map)
            .any(|m| m.get("targetID") == Some(&PyRep::Integer(P1))),
        "P1 must be in top pilot bounty hunters"
    );

    // Verify hunter leaderboard persists across snapshot reload
    let snap_data = ctx.db.with_snapshot(|s| s.clone());
    let ctx2 = ServerContext::new();
    ctx2.db.with_mut_snapshot(|s| *s = snap_data);
    let mut s1_reloaded = setup_pilot(&ctx2, 1103, "demo", "demo-secret", P1).await;
    let top_pilot_hunters2 = ctx2
        .dispatch_rpc(&mut s1_reloaded, "bountyProxy", None, "GetTopPilotBountyHunters", &[], &kw, None)
        .await
        .expect("GetTopPilotBountyHunters reloaded");
    let hunter_rows2 = unpack_hunter_rows(top_pilot_hunters2);
    assert!(hunter_rows2.iter().map(kv_map).any(|m| m.get("targetID") == Some(&PyRep::Integer(P1))));

    // 9. GetTopCorporationBountyHunters
    let top_corp_hunters = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetTopCorporationBountyHunters",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetTopCorporationBountyHunters");
    let corp_hunter_rows = unpack_hunter_rows(top_corp_hunters);
    assert!(
        corp_hunter_rows
            .iter()
            .map(kv_map)
            .any(|m| m.get("targetID") == Some(&PyRep::Integer(CORP_ID)) && m.get("bountyISK") == Some(&PyRep::Real(claimed_isk))),
        "CORP_ID must be in top corp bounty hunters with claimed_isk from pilot kill"
    );

    // 10. GetTopAllianceBountyHunters
    let top_all_hunters = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetTopAllianceBountyHunters",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetTopAllianceBountyHunters");
    let all_hunter_rows = unpack_hunter_rows(top_all_hunters);
    assert!(
        all_hunter_rows
            .iter()
            .map(kv_map)
            .any(|m| m.get("targetID") == Some(&PyRep::Integer(ALLIANCE_ID)) && m.get("bountyISK") == Some(&PyRep::Real(claimed_isk))),
        "ALLIANCE_ID must be in top alliance bounty hunters with claimed_isk from pilot kill"
    );

    // 11. SellKillRight: P1 lists kill right targeting P2 for public sale
    let kr_id = 770_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.kill_rights.insert(
            kr_id,
            eve_db::KillRightRecord {
                kill_right_id: kr_id,
                owner_id: P1,
                target_id: P2,
                activation_fee: eve_db::IskCents(0),
                is_active_suspect: false,
            },
        );
    });

    let sell_res = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "SellKillRight",
            &[PyRep::Integer(kr_id), PyRep::Real(3_500_000.0)],
            &kw,
            None,
        )
        .await
        .expect("SellKillRight");
    let sell_kv = kv_map(&sell_res);
    assert_eq!(sell_kv.get("killRightID"), Some(&PyRep::Integer(kr_id)));
    assert_eq!(sell_kv.get("priceISK"), Some(&PyRep::Real(3_500_000.0)));
    assert_eq!(sell_kv.get("forSale"), Some(&PyRep::Bool(true)));

    // Verify DB snapshot reflects updated fee
    let fee_in_db = ctx.db.with_snapshot(|snap| {
        snap.kill_rights.get(&kr_id).map(|kr| kr.activation_fee.to_wire_f64())
    });
    assert_eq!(fee_in_db, Some(3_500_000.0));

    // 12. SellKillRight with invalid restricted_to target fails
    let bad_restr = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "SellKillRight",
            &[
                PyRep::Integer(kr_id),
                PyRep::Real(3_500_000.0),
                PyRep::Integer(999_999_999),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("unknown restricted_to entity must be rejected");
    assert!(bad_restr.message.contains("InvalidKillRightRestrictionTarget") || bad_restr.code == "InvalidKillRightRestrictionTarget");

    // 13. SellKillRight restricted to a specific corporation enforces restriction on BuyKillRight & ActivateKillRight
    let victim_id = 90_000_099;
    let outsider_id = 90_000_098;
    ctx.db.provision_gm_account("victim_acct", "pass", victim_id, "Outlaw Target");
    ctx.db.provision_gm_account("outsider_acct", "pass", outsider_id, "Outsider Pilot");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(kr) = snap.kill_rights.get_mut(&kr_id) {
            kr.target_id = victim_id;
        }
        if let Some(v) = snap.characters.get_mut(&victim_id) {
            v.corporation_id = 1_000_125;
            v.solar_system_id = 30_000_142;
            v.station_id = None;
            v.structure_id = None;
        }
        if let Some(c2) = snap.characters.get_mut(&P2) {
            c2.station_id = None;
            c2.structure_id = None;
            c2.solar_system_id = 30_000_142;
        }
        if let Some(co) = snap.characters.get_mut(&outsider_id) {
            co.corporation_id = 1_000_125;
            co.station_id = None;
            co.structure_id = None;
            co.solar_system_id = 30_000_142;
            co.balance = eve_db::IskCents::from_isk(50_000_000);
        }
    });
    let mut s_outsider = setup_pilot(&ctx, 1103, "outsider_acct", "pass", outsider_id).await;

    ctx.dispatch_rpc(
        &mut s1,
        "bountyProxy",
        None,
        "SellKillRight",
        &[
            PyRep::Integer(kr_id),
            PyRep::Real(3_500_000.0),
            PyRep::Integer(CORP_ID),
        ],
        &kw,
        None,
    )
    .await
    .expect("SellKillRight restricted to CORP_ID");

    let buy_unauth = ctx
        .dispatch_rpc(
            &mut s_outsider,
            "killRightMgr",
            None,
            "BuyKillRight",
            &[PyRep::Integer(kr_id)],
            &kw,
            None,
        )
        .await
        .expect_err("outsider cannot buy corp-restricted kill right");
    assert_eq!(buy_unauth.code, "KillRightRestricted");

    let act_unauth = ctx
        .dispatch_rpc(
            &mut s_outsider,
            "killRightMgr",
            None,
            "ActivateKillRight",
            &[PyRep::Integer(kr_id)],
            &kw,
            None,
        )
        .await
        .expect_err("outsider cannot activate corp-restricted kill right");
    assert_eq!(act_unauth.code, "KillRightRestricted");

    let buy_auth = ctx
        .dispatch_rpc(
            &mut s2,
            "killRightMgr",
            None,
            "BuyKillRight",
            &[PyRep::Integer(kr_id)],
            &kw,
            None,
        )
        .await
        .expect("CORP_ID member P2 can buy corp-restricted kill right");
    assert_eq!(kv_map(&buy_auth).get("suspectFlagActive"), Some(&PyRep::Bool(true)));
}

#[tokio::test]
async fn test_crimewatch_security_and_duels() {
    // parity: rpc:crimewatch.ApplyTagsForSecStatus
    // parity: rpc:crimewatch.GetCharacterSecurityStatus
    // parity: rpc:crimewatch.GetSecurityStatusTransactions
    // parity: rpc:crimewatch.RespondToDuelChallenge
    // parity: rpc:crimewatch.SetSafetyLevel
    // parity: rpc:crimewatch.StartDuelChallenge
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 1201, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 1202, "other", "other-secret", P2).await;
    let kw = HashMap::new();

    // 1. SetSafetyLevel: configure SafetyLevel::Red (0), Yellow (1), Green (2)
    let safety_red = ctx
        .dispatch_rpc(
            &mut s1,
            "crimewatch",
            None,
            "SetSafetyLevel",
            &[PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("SetSafetyLevel Red");
    let red_kv = kv_map(&safety_red);
    assert_eq!(red_kv.get("safetyLevel"), Some(&PyRep::Integer(0)));
    assert_eq!(
        with_combat_runtime(&ctx, |rt| rt.safety_levels.get(&P1).copied()),
        Some(SafetyLevel::Red)
    );

    let safety_green = ctx
        .dispatch_rpc(
            &mut s1,
            "crimewatch",
            None,
            "SetSafetyLevel",
            &[PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("SetSafetyLevel Green");
    let green_kv = kv_map(&safety_green);
    assert_eq!(green_kv.get("safetyLevel"), Some(&PyRep::Integer(2)));
    assert_eq!(
        with_combat_runtime(&ctx, |rt| rt.safety_levels.get(&P1).copied()),
        Some(SafetyLevel::Green)
    );

    // 2. GetCharacterSecurityStatus: query explicit target character
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.security_status = -8.5;
        }
    });

    let sec_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "crimewatch",
            None,
            "GetCharacterSecurityStatus",
            &[PyRep::Integer(P1)],
            &kw,
            None,
        )
        .await
        .expect("GetCharacterSecurityStatus");
    assert_eq!(sec_rep, PyRep::Real(-8.5));

    // 3. StartDuelChallenge: P1 challenges P2
    let challenge_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "crimewatch",
            None,
            "StartDuelChallenge",
            &[PyRep::Integer(P2)],
            &kw,
            None,
        )
        .await
        .expect("StartDuelChallenge");
    let ch_kv = kv_map(&challenge_rep);
    assert_eq!(ch_kv.get("challengerID"), Some(&PyRep::Integer(P1)));
    assert_eq!(ch_kv.get("targetID"), Some(&PyRep::Integer(P2)));
    assert_eq!(ch_kv.get("challengeIssued"), Some(&PyRep::Bool(true)));

    let has_challenge = with_combat_runtime(&ctx, |rt| rt.duel_challenges.contains_key(&(P1, P2)));
    assert!(has_challenge, "Pending duel challenge must exist in runtime");

    // 4. RespondToDuelChallenge: P2 accepts duel challenge from P1
    let respond_rep = ctx
        .dispatch_rpc(
            &mut s2,
            "crimewatch",
            None,
            "RespondToDuelChallenge",
            &[PyRep::Integer(P1), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("RespondToDuelChallenge");
    let resp_kv = kv_map(&respond_rep);
    assert_eq!(resp_kv.get("accepted"), Some(&PyRep::Bool(true)));
    assert_eq!(
        resp_kv.get("limitedEngagementSeconds"),
        Some(&PyRep::Integer(300))
    );

    // Verify mutual limited engagement created in runtime
    let now_u = ctx.now_unix();
    let has_le = with_combat_runtime(&ctx, |rt| {
        rt.has_limited_engagement_with(P1, P2, now_u)
            && rt.has_limited_engagement_with(P2, P1, now_u)
    });
    assert!(has_le, "Mutual limited engagement must be active for 300 seconds");

    // 5. ApplyTagsForSecStatus: P1 docked in station turns in Clone Soldier Trainer Tag (35661)
    // Valid for security band [-10.0, -8.0); P1 is at -8.5.
    s1.station_id = Some(60_003_760);
    s1.solar_system_id = Some(30_000_142);
    let tag_item_id = 880_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            tag_item_id,
            eve_db::InventoryItemRecord {
                item_id: tag_item_id,
                type_id: 35661, // Clone Soldier Trainer Tag
                owner_id: P1,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let apply_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "crimewatch",
            None,
            "ApplyTagsForSecStatus",
            &[PyRep::Integer(35661), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("ApplyTagsForSecStatus");
    let apply_kv = kv_map(&apply_rep);
    assert_eq!(apply_kv.get("oldSecurityStatus"), Some(&PyRep::Real(-8.5)));
    assert_eq!(apply_kv.get("newSecurityStatus"), Some(&PyRep::Real(-8.0)));

    // Verify tag item was consumed from DB
    let tag_exists = ctx.db.with_snapshot(|snap| snap.items.contains_key(&tag_item_id));
    assert!(!tag_exists, "Clone Soldier Tag must be consumed on turn-in");

    // Verify character security status in DB was updated to -8.0
    let updated_sec = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&P1).map(|c| c.security_status)
    });
    assert_eq!(updated_sec, Some(-8.0));

    // 6. GetSecurityStatusTransactions: verify audit history log
    let tx_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "crimewatch",
            None,
            "GetSecurityStatusTransactions",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetSecurityStatusTransactions");
    let PyRep::Rowset { rows, columns, .. } = tx_rep else {
        panic!("Expected PyRowset from GetSecurityStatusTransactions");
    };
    assert!(!rows.is_empty(), "Security status transaction log must not be empty");
    assert!(columns.contains(&"oldSecurityStatus".into()));
    assert!(columns.contains(&"newSecurityStatus".into()));
    assert!(columns.contains(&"modification".into()));
}
