//! Authentic state-backed integration tests for `corpRegistry` moniker binding, division account keys, kill reports, and naming RPC methods:
//! - Moniker binding: `Bind`, `UnBind`, `Unbind`
//! - Division keys, kills, naming & owners: `SetAccountKey`, `GetSuggestedTickerNames`, `GetSuggestedAllianceShortNames`, `GetRecentKills`, `GetRecentLosses`, `GetEveOwners`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;

async fn select_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn field(rep: &PyRep, key: &str) -> PyRep {
    match rep {
        PyRep::KeyVal(map) => map.get(key).cloned().unwrap_or(PyRep::None),
        PyRep::Dict(entries) => entries
            .iter()
            .find_map(|(k, v)| (k.as_str() == Some(key)).then(|| v.clone()))
            .unwrap_or(PyRep::None),
        _ => PyRep::None,
    }
}

fn rowset_data(rep: &PyRep) -> (Vec<String>, Vec<Vec<PyRep>>) {
    match rep {
        PyRep::Rowset { columns, rows } => (columns.clone(), rows.clone()),
        _ => (Vec::new(), Vec::new()),
    }
}

#[tokio::test]
async fn test_corp_registry_monikers_introspection_and_actions() {
    // parity: rpc:corpRegistry.Bind
    // parity: rpc:corpRegistry.UnBind
    // parity: rpc:corpRegistry.Unbind

    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 62_001, "demo", "demo-secret", PILOT_1).await;
    let corp_id = sess.corporation_id.unwrap_or(1_000_009);

    // 1. Bind
    let bind_res = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "Bind",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("Bind");
    let bound_id = bind_res.as_i64().expect("bound_id as i64") as u64;
    assert!(bound_id > 0);
    assert!(ctx.bound_objects.read().unwrap().contains_key(&bound_id));

    // 2. UnBind
    let unbind_res = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "UnBind",
        &[PyRep::Integer(bound_id as i64)],
    )
    .await
    .expect("UnBind");
    assert_eq!(unbind_res, PyRep::None);
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&bound_id));

    // 3. Unbind (lowercase variant)
    let bind2 = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "Bind",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("Bind 2");
    let bound2_id = bind2.as_i64().expect("bound2 as i64") as u64;
    assert!(ctx.bound_objects.read().unwrap().contains_key(&bound2_id));

    let unbind_lower = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "Unbind",
        &[PyRep::Integer(bound2_id as i64)],
    )
    .await
    .expect("Unbind");
    assert_eq!(unbind_lower, PyRep::None);
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&bound2_id));

    // 4. SetSessionCheck
    let check_res = rpc(&ctx, &mut sess, "corpRegistry", "SetSessionCheck", &[])
        .await
        .expect("SetSessionCheck");
    assert_eq!(check_res, PyRep::None);

    // 5. columns
    let cols_res = rpc(&ctx, &mut sess, "corpRegistry", "columns", &[])
        .await
        .expect("columns");
    assert_eq!(
        cols_res,
        PyRep::List(vec![
            PyRep::String("corporationID".into()),
            PyRep::String("corporationName".into()),
            PyRep::String("ticker".into()),
        ])
    );

    // 6. header
    let hdr_res = rpc(&ctx, &mut sess, "corpRegistry", "header", &[])
        .await
        .expect("header");
    assert_eq!(
        hdr_res,
        PyRep::Tuple(vec![
            PyRep::String("corporationID".into()),
            PyRep::String("corporationName".into()),
            PyRep::String("ticker".into()),
        ])
    );

    // 7. itervalues
    let iter_res = rpc(&ctx, &mut sess, "corpRegistry", "itervalues", &[])
        .await
        .expect("itervalues");
    assert_eq!(iter_res, PyRep::List(Vec::new()));

    // 8. clear_memoized
    let clr_res = rpc(&ctx, &mut sess, "corpRegistry", "clear_memoized", &[])
        .await
        .expect("clear_memoized");
    assert_eq!(clr_res, PyRep::None);
}

#[tokio::test]
async fn test_corp_registry_kills_naming_and_owners() {
    // parity: rpc:corpRegistry.SetAccountKey
    // parity: rpc:corpRegistry.GetSuggestedTickerNames
    // parity: rpc:corpRegistry.GetSuggestedAllianceShortNames
    // parity: rpc:corpRegistry.GetRecentKills
    // parity: rpc:corpRegistry.GetRecentLosses
    // parity: rpc:corpRegistry.GetEveOwners
    // parity: rpc:corpRegistry.ExecuteActions

    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 62_002, "demo", "demo-secret", PILOT_1).await;

    // Create corporation
    let created = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Frontier Security Command".into()),
            PyRep::String("FSC".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID")
        .as_i64()
        .expect("corporationID");

    // 1. SetAccountKey
    let key_res = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "SetAccountKey",
        &[PyRep::Integer(1005)],
    )
    .await
    .expect("SetAccountKey");
    assert_eq!(key_res, PyRep::None);
    {
        eve_server::services::fleet_social::fleet_social_corps::read_corp_gov(&ctx, |g| {
            assert_eq!(
                g.get(&corp_id).and_then(|s| s.member_account_keys.get(&PILOT_1)).copied(),
                Some(1005)
            );
        });
    }
    assert!(
        rpc(&ctx, &mut sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(999)])
            .await
            .is_err()
    );

    // 2. ExecuteActions (verified state-backed batch execution)
    let batch = vec![
        PyRep::Tuple(vec![
            PyRep::String("SetAccountKey".into()),
            PyRep::Integer(PILOT_1),
            PyRep::Integer(1002),
        ]),
        PyRep::Tuple(vec![
            PyRep::String("SetNote".into()),
            PyRep::Integer(PILOT_1),
            PyRep::String("Lead Fleet Commander".into()),
        ]),
        PyRep::Tuple(vec![
            PyRep::String("SetFriendlyFire".into()),
            PyRep::Bool(true),
        ]),
    ];
    let exec = rpc(&ctx, &mut sess, "corpRegistry", "ExecuteActions", &[PyRep::List(batch)])
        .await
        .expect("ExecuteActions batch");
    assert_eq!(exec, PyRep::Bool(true));
    {
        eve_server::services::fleet_social::fleet_social_corps::read_corp_gov(&ctx, |g| {
            let state = g.get(&corp_id).unwrap();
            assert_eq!(state.member_account_keys.get(&PILOT_1).copied(), Some(1002));
            assert_eq!(state.member_notes.get(&PILOT_1).map(|s| s.as_str()), Some("Lead Fleet Commander"));
            assert_eq!(state.pending_friendly_fire.map(|(ff, _)| ff), Some(true));
        });
    }
    // Denial when executing on external corporation
    assert!(
        rpc(
            &ctx,
            &mut sess,
            "corpRegistry",
            "ExecuteActions",
            &[PyRep::Integer(98_999_999), PyRep::List(vec![])],
        )
        .await
        .is_err()
    );

    // 3. GetSuggestedTickerNames
    let tickers = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetSuggestedTickerNames",
        &[PyRep::String("Frontier Security Command".into())],
    )
    .await
    .expect("GetSuggestedTickerNames");
    let PyRep::List(t_list) = tickers else { panic!("expected list") };
    assert!(t_list.contains(&PyRep::String("FSC".into())));

    // 4. GetSuggestedAllianceShortNames
    let shorts = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetSuggestedAllianceShortNames",
        &[PyRep::String("United Frontier Coalition".into())],
    )
    .await
    .expect("GetSuggestedAllianceShortNames");
    let PyRep::List(s_list) = shorts else { panic!("expected list") };
    assert!(s_list.contains(&PyRep::String("UFC".into())));

    // Seed killmails for recent kills and recent losses
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            91_001,
            eve_db::KillmailRecord {
                kill_id: 91_001,
                victim_character_id: PILOT_2,
                attacker_character_id: PILOT_1,
                solar_system_id: 30_000_142,
                ship_type_id: 587,
                total_value_isk: 1_250_000.0,
                killed_unix: 1_700_000_000,
                victim_corp_id: 98_000_099,
                victim_alliance_id: None,
                attacker_corp_id: corp_id,
                attacker_alliance_id: None,
            },
        );
        snap.killmails.insert(
            91_002,
            eve_db::KillmailRecord {
                kill_id: 91_002,
                victim_character_id: PILOT_1,
                attacker_character_id: PILOT_2,
                solar_system_id: 30_000_142,
                ship_type_id: 606,
                total_value_isk: 2_500_000.0,
                killed_unix: 1_700_000_500,
                victim_corp_id: corp_id,
                victim_alliance_id: None,
                attacker_corp_id: 98_000_099,
                attacker_alliance_id: None,
            },
        );
    });

    // 5. GetRecentKills
    let kills = rpc(&ctx, &mut sess, "corpRegistry", "GetRecentKills", &[])
        .await
        .expect("GetRecentKills");
    let (k_cols, k_rows) = rowset_data(&kills);
    assert_eq!(
        k_cols,
        vec![
            "killID",
            "victimCharacterID",
            "victimCorporationID",
            "victimShipTypeID",
            "solarSystemID",
            "killTime"
        ]
    );
    assert_eq!(k_rows.len(), 1);
    assert_eq!(k_rows[0][0], PyRep::Integer(91_001));

    // 6. GetRecentLosses
    let losses = rpc(&ctx, &mut sess, "corpRegistry", "GetRecentLosses", &[])
        .await
        .expect("GetRecentLosses");
    let (l_cols, l_rows) = rowset_data(&losses);
    assert_eq!(
        l_cols,
        vec![
            "killID",
            "victimCharacterID",
            "victimCorporationID",
            "victimShipTypeID",
            "solarSystemID",
            "killTime"
        ]
    );
    assert_eq!(l_rows.len(), 1);
    assert_eq!(l_rows[0][0], PyRep::Integer(91_002));

    // 7. GetEveOwners
    let owners = rpc(&ctx, &mut sess, "corpRegistry", "GetEveOwners", &[])
        .await
        .expect("GetEveOwners");
    let (o_cols, o_rows) = rowset_data(&owners);
    assert_eq!(
        o_cols,
        vec!["ownerID", "ownerName", "typeID", "gender", "corporationID"]
    );
    assert_eq!(o_rows.len(), 2);
    assert_eq!(o_rows[0][0], PyRep::Integer(PILOT_1));
    assert_eq!(o_rows[1][0], PyRep::Integer(corp_id));
}
