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

    // Add PILOT_2 as a regular member with no roles
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.members.insert(PILOT_2);
            c.member_roles.insert(PILOT_2, 0);
        }
        if let Some(ch) = snap.characters.get_mut(&PILOT_2) {
            ch.corporation_id = corp_id;
        }
    });
    let mut sess2 = select_pilot(&ctx, 62_003, "other", "other-secret", PILOT_2).await;

    // 1. SetAccountKey: updates session.corp_account_key, emits OnSessionChanged, persists across re-select, and rejects unauthorized division
    let _ = ctx.drain_notifications_for_char(PILOT_1);
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
    assert_eq!(sess.corp_account_key, 1005);
    let key_notifs = ctx.drain_notifications_for_char(PILOT_1);
    assert!(
        key_notifs.iter().any(|n| n.event == "OnSessionChanged"),
        "expected OnSessionChanged on SetAccountKey"
    );
    sess = select_pilot(&ctx, 62_004, "demo", "demo-secret", PILOT_1).await;
    assert_eq!(sess.corp_account_key, 1005);
    assert!(
        rpc(&ctx, &mut sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(999)])
            .await
            .is_err()
    );
    assert!(
        rpc(&ctx, &mut sess2, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1005)])
            .await
            .is_err(),
        "member without wallet division access must be denied SetAccountKey"
    );

    // 2. ExecuteActions: V24 (targetIDs, actions) bulk member mutation + role gate
    assert!(
        rpc(
            &ctx,
            &mut sess2,
            "corpRegistry",
            "ExecuteActions",
            &[
                PyRep::List(vec![PyRep::Integer(PILOT_2)]),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::String("set".into()),
                    PyRep::String("titleMask".into()),
                    PyRep::Integer(1),
                ])]),
            ],
        )
        .await
        .is_err(),
        "non-director must be denied ExecuteActions"
    );
    let v24_actions = vec![
        PyRep::Tuple(vec![
            PyRep::String("grant".into()),
            PyRep::String("roles".into()),
            PyRep::Integer(eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT as i64),
        ]),
        PyRep::Tuple(vec![
            PyRep::String("set".into()),
            PyRep::String("titleMask".into()),
            PyRep::Integer(3),
        ]),
        PyRep::Tuple(vec![
            PyRep::String("set".into()),
            PyRep::String("divisionID".into()),
            PyRep::Integer(2),
        ]),
    ];
    let exec = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(PILOT_1), PyRep::Integer(PILOT_2)]),
            PyRep::List(v24_actions),
        ],
    )
    .await
    .expect("ExecuteActions V24 (targetIDs, actions)");
    assert_eq!(exec, PyRep::Bool(true));
    let m2 = rpc(&ctx, &mut sess, "corpRegistry", "GetMember", &[PyRep::Integer(PILOT_2)])
        .await
        .expect("GetMember PILOT_2");
    assert_eq!(
        field(&m2, "roles").as_i64(),
        Some(eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT as i64)
    );
    assert_eq!(field(&m2, "titleMask").as_i64(), Some(3));
    assert_eq!(field(&m2, "divisionID").as_i64(), Some(2));
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

    // 3. GetSuggestedTickerNames: returns List of KeyVal(tickerName=...), excludes already-taken "FSC"
    let tickers_taken = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetSuggestedTickerNames",
        &[PyRep::String("Frontier Security Command".into())],
    )
    .await
    .expect("GetSuggestedTickerNames taken");
    let PyRep::List(t_rows) = tickers_taken else { panic!("expected List from GetSuggestedTickerNames") };
    assert!(!t_rows.is_empty());
    assert!(
        !t_rows.iter().any(|r| field(r, "tickerName").as_str() == Some("FSC")),
        "already-taken ticker FSC must be filtered out"
    );
    let tickers_fresh = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetSuggestedTickerNames",
        &[PyRep::String("Gallente Star Works".into())],
    )
    .await
    .expect("GetSuggestedTickerNames fresh");
    let PyRep::List(tf_rows) = tickers_fresh else { panic!("expected List from GetSuggestedTickerNames") };
    assert!(tf_rows.iter().any(|r| field(r, "tickerName").as_str() == Some("GSW")));

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

    // 7. GetEveOwners: returns only character member rows (no corporation row mixed in)
    let owners = rpc(&ctx, &mut sess, "corpRegistry", "GetEveOwners", &[])
        .await
        .expect("GetEveOwners");
    let (o_cols, o_rows) = rowset_data(&owners);
    assert_eq!(
        o_cols,
        vec!["ownerID", "ownerName", "typeID", "gender"]
    );
    assert_eq!(o_rows.len(), 2);
    assert_eq!(o_rows[0][0], PyRep::Integer(PILOT_1));
    assert_eq!(o_rows[1][0], PyRep::Integer(PILOT_2));
    assert!(!o_rows.iter().any(|r| r[0] == PyRep::Integer(corp_id)));
}
