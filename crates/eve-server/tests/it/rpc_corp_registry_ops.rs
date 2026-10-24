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
            ch.gender = 1;
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

    // Reject negative role masks (-1 -> CrpAccessDenied) and unknown verbs (-> InvalidParameter)
    let neg_mask_err = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(PILOT_2)]),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("set".into()),
                PyRep::String("roles".into()),
                PyRep::Integer(-1),
            ])]),
        ],
    )
    .await
    .expect_err("negative role mask must be rejected");
    assert_eq!(neg_mask_err.code, "CrpAccessDenied");

    let bad_verb_err = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(PILOT_2)]),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("bogus_verb".into()),
                PyRep::String("roles".into()),
                PyRep::Integer(eve_db::corp_roles::ROLE_TRADER as i64),
            ])]),
        ],
    )
    .await
    .expect_err("unknown verb must be rejected");
    assert_eq!(bad_verb_err.code, "InvalidParameter");

    // Non-CEO Director cannot grant or strip ROLE_DIRECTOR via ExecuteActions
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles.insert(PILOT_2, eve_db::corp_roles::ROLE_DIRECTOR);
        }
    });
    let dir_grant_err = rpc(&ctx, &mut sess2, "corpRegistry", "ExecuteActions", &[PyRep::List(vec![PyRep::Integer(PILOT_2)]), PyRep::List(vec![PyRep::Tuple(vec![PyRep::String("add".into()), PyRep::String("roles".into()), PyRep::Integer(eve_db::corp_roles::ROLE_DIRECTOR as i64)])])]).await.expect_err("non-CEO Director cannot grant ROLE_DIRECTOR via ExecuteActions");
    assert_eq!(dir_grant_err.code, "CrpAccessDenied");
    let dir_strip_err = rpc(&ctx, &mut sess2, "corpRegistry", "ExecuteActions", &[PyRep::List(vec![PyRep::Integer(PILOT_2)]), PyRep::List(vec![PyRep::Tuple(vec![PyRep::String("remove".into()), PyRep::String("roles".into()), PyRep::Integer(eve_db::corp_roles::ROLE_DIRECTOR as i64)])])]).await.expect_err("non-CEO Director cannot strip ROLE_DIRECTOR via ExecuteActions");
    assert_eq!(dir_strip_err.code, "CrpAccessDenied");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles.insert(PILOT_2, eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT);
        }
    });

    // Granting hangar bits in grantableRoles must NOT grant held division access (R-CG2-01)
    let hangar_role: u64 = (1u64 << 20) | (1u64 << 13);
    rpc(&ctx, &mut sess, "corpRegistry", "ExecuteActions", &[PyRep::List(vec![PyRep::Integer(PILOT_2)]), PyRep::List(vec![PyRep::Tuple(vec![PyRep::String("set".into()), PyRep::String("grantableRoles".into()), PyRep::Integer(hangar_role as i64)])])]).await.expect("ExecuteActions set grantableRoles");
    assert_eq!(rpc(&ctx, &mut sess2, "invbroker", "List", &[PyRep::Integer(116)]).await.expect_err("grantableRoles must not grant held division access").code, "CorpRoleDenied");

    // Granting a hangar division role via ExecuteActions must be delayed by 24h (pending_role_grants)
    // and preserved across unrelated titleMask / non-hangar role updates (R-CG2-02)
    rpc(&ctx, &mut sess, "corpRegistry", "ExecuteActions", &[PyRep::List(vec![PyRep::Integer(PILOT_2)]), PyRep::List(vec![PyRep::Tuple(vec![PyRep::String("add".into()), PyRep::String("roles".into()), PyRep::Integer(hangar_role as i64)])])]).await.expect("ExecuteActions hangar role grant");
    rpc(&ctx, &mut sess, "corpRegistry", "ExecuteActions", &[PyRep::List(vec![PyRep::Integer(PILOT_2)]), PyRep::List(vec![PyRep::Tuple(vec![PyRep::String("set".into()), PyRep::String("titleMask".into()), PyRep::Integer(7)])])]).await.expect("ExecuteActions titleMask update must preserve pending_role_grants");
    let m2_before_24h = rpc(&ctx, &mut sess, "corpRegistry", "GetMember", &[PyRep::Integer(PILOT_2)])
        .await
        .expect("GetMember PILOT_2 before 24h");
    assert_eq!(
        field(&m2_before_24h, "roles").as_i64(),
        Some(eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT as i64),
        "hangar role granted via ExecuteActions must be pending before 24h"
    );
    let list_before_err = rpc(&ctx, &mut sess2, "invbroker", "List", &[PyRep::Integer(116)])
        .await
        .expect_err("invbroker.List on division 1 must be denied before 24h role delay expires");
    assert_eq!(list_before_err.code, "CorpRoleDenied");
    ctx.advance_secs(86_401);
    rpc(&ctx, &mut sess2, "invbroker", "List", &[PyRep::Integer(116)])
        .await
        .expect("invbroker.List on division 1 must succeed after 24h role delay expires");
    let promoted_in_db = ctx.db.with_snapshot(|snap| {
        let mr = snap.corporations.get(&corp_id).and_then(|c| c.member_roles.get(&PILOT_2)).copied().unwrap_or(0);
        let ra = snap.corp_role_assignments.get(&(corp_id, PILOT_2)).map(|r| r.roles).unwrap_or(0);
        let pending = snap.corp_governance.get(&corp_id).is_some_and(|g| g.pending_role_grants.contains_key(&PILOT_2));
        (mr, ra, pending)
    });
    assert_eq!(promoted_in_db.0, eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT | hangar_role);
    assert_eq!(promoted_in_db.1, eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT | hangar_role);
    assert!(!promoted_in_db.2, "pending_role_grants entry must be cleared once promoted");
    let m2_after_24h = rpc(&ctx, &mut sess, "corpRegistry", "GetMember", &[PyRep::Integer(PILOT_2)])
        .await
        .expect("GetMember PILOT_2 after 24h");
    assert_eq!(
        field(&m2_after_24h, "roles").as_i64(),
        Some((eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT | hangar_role) as i64),
        "hangar role granted via ExecuteActions must activate after 24h"
    );
    // Scoped rolesAtHQ hangar division 2 (1 << 21) is also delayed by 24h and enforced at HQ station (R-CG3-01)
    let hq_div2: u64 = (1u64 << 21) | (1u64 << 14);
    rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(PILOT_2)]),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("set".into()),
                PyRep::String("rolesAtHQ".into()),
                PyRep::Integer(hq_div2 as i64),
            ])]),
        ],
    )
    .await
    .expect("ExecuteActions set rolesAtHQ");
    assert_eq!(
        rpc(&ctx, &mut sess2, "invbroker", "List", &[PyRep::Integer(117)])
            .await
            .expect_err("invbroker.List(117) must be denied before 24h rolesAtHQ promotion")
            .code,
        "CorpRoleDenied"
    );
    ctx.advance_secs(86_401);
    rpc(&ctx, &mut sess2, "invbroker", "List", &[PyRep::Integer(117)])
        .await
        .expect("invbroker.List(117) must succeed at HQ station after 24h rolesAtHQ promotion");

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

    // 7. GetEveOwners: returns character member rows with SDE-resolved bloodline typeID and gender
    let owners = rpc(&ctx, &mut sess, "corpRegistry", "GetEveOwners", &[])
        .await
        .expect("GetEveOwners");
    let (o_cols, o_rows) = rowset_data(&owners);
    assert_eq!(o_cols, vec!["ownerID", "ownerName", "typeID", "gender"]);
    assert_eq!(o_rows.len(), 2);
    assert_eq!(o_rows[0][0], PyRep::Integer(PILOT_1));
    assert_eq!(o_rows[0][2], PyRep::Integer(1375), "Sebiestor bloodline_id=3 must resolve to SDE typeID 1375");
    assert_eq!(o_rows[0][3], PyRep::Integer(0));
    assert_eq!(o_rows[1][0], PyRep::Integer(PILOT_2));
    assert_eq!(o_rows[1][2], PyRep::Integer(1373), "Deteis bloodline_id=1 must resolve to SDE typeID 1373");
    assert_eq!(o_rows[1][3], PyRep::Integer(1));
    assert!(!o_rows.iter().any(|r| r[0] == PyRep::Integer(corp_id)));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch2) = snap.characters.get_mut(&PILOT_2) {
            ch2.bloodline_id = 11; // Achura
        }
    });
    let owners_achura = rpc(&ctx, &mut sess, "corpRegistry", "GetEveOwners", &[])
        .await
        .expect("GetEveOwners Achura");
    let (_, oa_rows) = rowset_data(&owners_achura);
    assert_eq!(oa_rows[1][2], PyRep::Integer(1383), "Achura bloodline_id=11 must resolve to SDE typeID 1383");
    let cross_corp_err = rpc(&ctx, &mut sess, "corpRegistry", "GetEveOwners", &[PyRep::Integer(98_999_999)])
        .await
        .expect_err("cross-corp GetEveOwners must be rejected");
    assert_eq!(cross_corp_err.code, "CrpAccessDenied");

    // 8. Real SQLite save/reload round-trip for corp contacts, labels, bulletins, and SetAccountKey
    let sqlite_path = std::env::temp_dir().join(format!("evomu_corp_ops_rt_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&sqlite_path);
    {
        let repo = std::sync::Arc::new(eve_db::WorldRepository::open_sqlite(&sqlite_path).expect("open_sqlite"));
        let seed_snap = eve_db::WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        repo.with_mut_snapshot(|snap| *snap = seed_snap);
        let ctx_db = ServerContext::with_repository(repo);
        common::pin_clock(&ctx_db);
        let mut s_db = select_pilot(&ctx_db, 1891, "demo", "demo-secret", PILOT_1).await;
        let created_db = rpc(&ctx_db, &mut s_db, "corpRegistry", "AddCorporation", &[PyRep::String("Durable Corp Ops".into()), PyRep::String("DCOP".into()), PyRep::Real(0.05)]).await.expect("AddCorporation");
        let db_corp_id = field(&created_db, "corporationID").as_i64().expect("corporationID");
        rpc(&ctx_db, &mut s_db, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(PILOT_2), PyRep::Real(10.0), PyRep::Bool(true)]).await.expect("AddCorporateContact");
        let lbl = rpc(&ctx_db, &mut s_db, "corpRegistry", "CreateLabel", &[PyRep::String("Allies".into()), PyRep::Integer(4)]).await.expect("CreateLabel");
        let lid = field(&lbl, "labelID").as_i64().unwrap_or(1);
        rpc(&ctx_db, &mut s_db, "corpRegistry", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(PILOT_2)]), PyRep::Integer(lid)]).await.expect("AssignLabels");
        rpc(&ctx_db, &mut s_db, "corpRegistry", "AddBulletin", &[PyRep::String("Doctrine".into()), PyRep::String("Armor".into())]).await.expect("AddBulletin");
        rpc(&ctx_db, &mut s_db, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1004)]).await.expect("SetAccountKey 1004");
        drop(ctx_db);

        let repo_reloaded = std::sync::Arc::new(eve_db::WorldRepository::open_sqlite(&sqlite_path).expect("reload sqlite"));
        let ctx_reloaded = ServerContext::with_repository(repo_reloaded);
        let mut s_rel = select_pilot(&ctx_reloaded, 1892, "demo", "demo-secret", PILOT_1).await;
        let contacts = rpc(&ctx_reloaded, &mut s_rel, "corpRegistry", "GetCorporateContacts", &[]).await.expect("GetCorporateContacts after reload");
        let (_, c_rows) = rowset_data(&contacts);
        assert_eq!(c_rows.len(), 1);
        let labels = rpc(&ctx_reloaded, &mut s_rel, "corpRegistry", "GetLabels", &[]).await.expect("GetLabels after reload");
        let (_, l_rows) = rowset_data(&labels);
        assert_eq!(l_rows.len(), 1);
        let bulletins = rpc(&ctx_reloaded, &mut s_rel, "corpRegistry", "GetBulletins", &[PyRep::Integer(db_corp_id)]).await.expect("GetBulletins after reload");
        let (_, b_rows) = rowset_data(&bulletins);
        assert_eq!(b_rows.len(), 1);
        let m_rel = rpc(&ctx_reloaded, &mut s_rel, "corpRegistry", "GetMember", &[PyRep::Integer(PILOT_1)]).await.expect("GetMember after reload");
        assert_eq!(field(&m_rel, "accountKey"), PyRep::Integer(1004));
    }
    let _ = std::fs::remove_file(&sqlite_path);
}
