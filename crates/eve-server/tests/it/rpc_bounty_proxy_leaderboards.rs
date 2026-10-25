//! Integration tests covering bountyProxy leaderboard RPC parity rows.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_mut_combat_runtime;
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
        snap.corporations
            .entry(CORP_ID)
            .or_insert_with(|| eve_db::CorporationRecord {
                corporation_id: CORP_ID,
                name: "Leaderboard Corp".into(),
                ticker: "LBC".into(),
                ceo_id: P1,
                alliance_id: Some(ALLIANCE_ID),
                ..Default::default()
            });
        snap.alliances
            .entry(ALLIANCE_ID)
            .or_insert_with(|| eve_db::AllianceRecord {
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
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetTopPilotBounties",
            &[PyRep::Integer(1)],
            &empty_kw,
            None,
        )
        .await
        .expect("GetTopPilotBounties");
    let PyRep::List(tp_list) = top_pilots else {
        panic!("expected list")
    };
    assert!(!tp_list.is_empty());
    let tp0 = kv_map(&tp_list[0]);
    assert_eq!(tp0.get("targetID"), Some(&PyRep::Integer(P2)));
    assert_eq!(tp0.get("bountyISK"), Some(&PyRep::Real(25_000_000.0)));

    // 2. GetTopCorpBounties (page=1, returns List)
    let top_corps = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetTopCorpBounties",
            &[PyRep::Integer(1)],
            &empty_kw,
            None,
        )
        .await
        .expect("GetTopCorpBounties");
    let PyRep::List(tc_list) = top_corps else {
        panic!("expected list")
    };
    assert!(tc_list
        .iter()
        .map(kv_map)
        .any(|k| k.get("targetID") == Some(&PyRep::Integer(CORP_ID))));

    // 3. GetTopAllianceBounties (page=1, returns List)
    let top_alliances = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetTopAllianceBounties",
            &[PyRep::Integer(1)],
            &empty_kw,
            None,
        )
        .await
        .expect("GetTopAllianceBounties");
    let PyRep::List(ta_list) = top_alliances else {
        panic!("expected list")
    };
    assert!(ta_list
        .iter()
        .map(kv_map)
        .any(|k| k.get("targetID") == Some(&PyRep::Integer(ALLIANCE_ID))));

    // 4. GetTopPilotBountyHunters (page=1, V24 wire: 2-tuple `(rows, timestamp)`, client subscripts `result[0]`)
    let top_hunters = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetTopPilotBountyHunters",
            &[PyRep::Integer(1)],
            &empty_kw,
            None,
        )
        .await
        .expect("GetTopPilotBountyHunters");
    let PyRep::Tuple(th_tuple) = top_hunters else {
        panic!("expected 2-tuple")
    };
    assert_eq!(th_tuple.len(), 2);
    let PyRep::List(th_list) = &th_tuple[0] else {
        panic!("expected list in result[0]")
    };
    assert!(!th_list.is_empty());
    let th0 = kv_map(&th_list[0]);
    assert_eq!(th0.get("targetID"), Some(&PyRep::Integer(P2)));
    assert_eq!(th0.get("bountyISK"), Some(&PyRep::Real(70_000_000.0)));
    assert!(th_tuple[1].as_i64().unwrap_or(0) > 0);

    // 5. GetTopCorporationBountyHunters (page=1, V24 wire: 2-tuple `(rows, timestamp)`)
    let top_corp_hunters = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetTopCorporationBountyHunters",
            &[PyRep::Integer(1)],
            &empty_kw,
            None,
        )
        .await
        .expect("GetTopCorporationBountyHunters");
    let PyRep::Tuple(tch_tuple) = top_corp_hunters else {
        panic!("expected 2-tuple")
    };
    assert_eq!(tch_tuple.len(), 2);
    let PyRep::List(tch_list) = &tch_tuple[0] else {
        panic!("expected list in result[0]")
    };
    assert!(tch_list
        .iter()
        .map(kv_map)
        .any(|k| k.get("targetID") == Some(&PyRep::Integer(CORP_ID))));

    // 6. GetTopAllianceBountyHunters (page=1, V24 wire: 2-tuple `(rows, timestamp)`)
    let top_alli_hunters = ctx
        .dispatch_rpc(
            &mut s1,
            "bountyProxy",
            None,
            "GetTopAllianceBountyHunters",
            &[PyRep::Integer(1)],
            &empty_kw,
            None,
        )
        .await
        .expect("GetTopAllianceBountyHunters");
    let PyRep::Tuple(tah_tuple) = top_alli_hunters else {
        panic!("expected 2-tuple")
    };
    assert_eq!(tah_tuple.len(), 2);
    let PyRep::List(tah_list) = &tah_tuple[0] else {
        panic!("expected list in result[0]")
    };
    assert!(tah_list
        .iter()
        .map(kv_map)
        .any(|k| k.get("targetID") == Some(&PyRep::Integer(ALLIANCE_ID))));
}
