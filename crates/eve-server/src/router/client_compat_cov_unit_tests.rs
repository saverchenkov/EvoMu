//! Unit tests covering all reply normalization arms and helpers in `normalize_replies`.

use super::*;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

#[test]
fn normalize_helpers_cover_dbrow_rowset_objectex_and_keyval_recursive() {
    let dbrow = PyRep::DBRow {
        header: Box::new(PyRep::None),
        columns: vec![
            ("killID".into(), PyRep::Integer(42)),
            ("solarSystemID".into(), PyRep::Integer(30_000_142)),
        ],
        raw_rle: Vec::new(),
    };
    assert_eq!(row_field(&dbrow, "killID"), Some(PyRep::Integer(42)));
    assert_eq!(row_field(&dbrow, "missing"), None);

    let rowset = PyRep::rowset(
        vec!["killID", "victimID"],
        vec![vec![PyRep::Integer(7), PyRep::Integer(90_000_001)]],
    );
    let extracted_rs = extract_table_rows(&rowset);
    assert_eq!(extracted_rs.len(), 1);
    assert_eq!(extracted_rs[0].dict_get("killID"), Some(&PyRep::Integer(7)));

    let objex = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::None),
        list: vec![PyRep::keyval([("id", PyRep::Integer(99))])],
        dict: Vec::new(),
    };
    let extracted_ox = extract_table_rows(&objex);
    assert_eq!(extracted_ox.len(), 1);
    assert!(extract_table_rows(&PyRep::Integer(1)).is_empty());

    let nested = PyRep::Tuple(vec![
        PyRep::List(vec![PyRep::keyval([("a", PyRep::Integer(1))])]),
        PyRep::Dict(vec![(
            PyRep::String("k".into()),
            PyRep::keyval([("b", PyRep::Bool(true))]),
        )]),
        PyRep::String("leaf".into()),
    ]);
    let converted = keyval_to_dict_recursive(nested);
    let PyRep::Tuple(parts) = converted else {
        panic!("expected tuple");
    };
    assert!(matches!(parts[0], PyRep::List(_)));
    assert!(matches!(parts[1], PyRep::Dict(_)));
    assert_eq!(parts[2], PyRep::String("leaf".into()));
}

#[test]
fn normalize_structure_jump_xmpp_beyonce_blueprint_and_char_branches() {
    // 1. structureDirectory.GetJumpBridgesWithMyAccess
    let jb = normalize_macho_rpc_reply(
        "structureDirectory",
        "GetJumpBridgesWithMyAccess",
        Ok(PyRep::List(vec![PyRep::keyval([
            ("structureID", PyRep::Integer(1_000_000_000_101)),
            ("solarSystemID", PyRep::Integer(30_000_142)),
            ("targetStructureID", PyRep::Integer(1_000_000_000_102)),
            ("targetSolarSystemID", PyRep::Integer(30_000_144)),
        ])])),
    )
    .expect("jump bridges");
    let PyRep::Tuple(jb_parts) = jb else {
        panic!("expected 3-tuple");
    };
    assert_eq!(jb_parts.len(), 3);

    // 2. jumpTimers.GetTimers
    let jt = normalize_macho_rpc_reply(
        "jumpTimers",
        "GetTimers",
        Ok(PyRep::keyval([
            ("jumpActivation", PyRep::Integer(100)),
            ("jumpFatigue", PyRep::Integer(200)),
            ("lastUpdated", PyRep::Integer(300)),
        ])),
    )
    .expect("jump timers");
    assert_eq!(
        jt,
        PyRep::Tuple(vec![
            PyRep::Integer(100),
            PyRep::Integer(200),
            PyRep::Integer(300)
        ])
    );

    // 3. XmppChatMgr.ResyncSystemChannelAccess & beyonce.GetCapacityOfSiphon
    let xmpp = normalize_macho_rpc_reply(
        "XmppChatMgr",
        "ResyncSystemChannelAccess",
        Ok(PyRep::keyval([(
            "channels",
            PyRep::List(vec![PyRep::String("local_30000142".into())]),
        )])),
    )
    .expect("xmpp");
    assert_eq!(xmpp, PyRep::List(vec![PyRep::String("local_30000142".into())]));

    let xmpp_create = normalize_macho_rpc_reply(
        "XmppChatMgr",
        "CreatePlayerOwnedChannel",
        Ok(PyRep::keyval([
            ("channelID", PyRep::String("channel:playtest".into())),
            ("name", PyRep::String("playtest".into())),
            ("ownerID", PyRep::Integer(90_000_001)),
        ])),
    )
    .expect("xmpp create player channel");
    assert_eq!(xmpp_create, PyRep::String("player_playtest".into()));

    // 4. charMgr.GetOwnerNoteLabels & GetRecentShipKillsAndLosses
    let notes = normalize_macho_rpc_reply(
        "charMgr",
        "GetOwnerNoteLabels",
        Ok(PyRep::List(vec![
            PyRep::String("A".into()),
            PyRep::String("N:Mining".into()),
            PyRep::Integer(3),
        ])),
    )
    .expect("notes");
    let PyRep::List(note_items) = notes else {
        panic!("expected list");
    };
    assert_eq!(
        note_items[0].dict_get("label"),
        Some(&PyRep::String("N:General".into()))
    );
    assert_eq!(
        note_items[1].dict_get("label"),
        Some(&PyRep::String("N:Mining".into()))
    );

    // Issue #895 & #1359: charMgr.GetRecentShipKillsAndLosses preserves moonID and all V24.02 KillReportWnd columns
    let kills = normalize_macho_rpc_reply(
        "charMgr",
        "GetRecentShipKillsAndLosses",
        Ok(PyRep::List(vec![
            PyRep::DBRow {
                header: Box::new(PyRep::None),
                columns: vec![
                    ("killID".into(), PyRep::Integer(55)),
                    ("victimCharacterID".into(), PyRep::Integer(90_000_001)),
                    ("attackerID".into(), PyRep::Integer(90_000_002)),
                ],
                raw_rle: Vec::new(),
            },
            PyRep::DBRow {
                header: Box::new(PyRep::None),
                columns: vec![
                    ("killID".into(), PyRep::Integer(56)),
                    ("victimCharacterID".into(), PyRep::Integer(90_000_001)),
                    ("finalCharacterID".into(), PyRep::Integer(90_000_002)),
                    ("warID".into(), PyRep::Integer(42)),
                    ("iskLost".into(), PyRep::Real(1_500_000.0)),
                    ("iskDestroyed".into(), PyRep::Real(900_000.0)),
                    ("bountyClaimed".into(), PyRep::Real(50_000.0)),
                    ("loyaltyPoints".into(), PyRep::Integer(250)),
                    ("killRightSupplied".into(), PyRep::Integer(90_000_009)),
                ],
                raw_rle: Vec::new(),
            },
        ])),
    )
    .expect("kills");
    let PyRep::Rowset {
        ref columns,
        ref rows,
    } = kills
    else {
        panic!("expected Rowset");
    };
    assert!(columns.iter().any(|c| c == "moonID"));
    assert!(columns.iter().any(|c| c == "victimShipTypeID"));
    assert!(columns.iter().any(|c| c == "finalShipTypeID"));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].len(), columns.len());
    assert_eq!(rows[1].len(), columns.len());
    let col_idx = |name: &str| {
        columns
            .iter()
            .position(|c| c == name)
            .unwrap_or_else(|| panic!("missing column {name}"))
    };
    for required in [
        "warID",
        "iskLost",
        "iskDestroyed",
        "bountyClaimed",
        "loyaltyPoints",
        "killRightSupplied",
    ] {
        assert_eq!(
            rows[0][col_idx(required)],
            PyRep::None,
            "missing upstream column {required} should default to None"
        );
    }
    assert_eq!(rows[1][col_idx("warID")], PyRep::Integer(42));
    assert_eq!(rows[1][col_idx("iskLost")], PyRep::Real(1_500_000.0));
    assert_eq!(rows[1][col_idx("iskDestroyed")], PyRep::Real(900_000.0));
    assert_eq!(rows[1][col_idx("bountyClaimed")], PyRep::Real(50_000.0));
    assert_eq!(rows[1][col_idx("loyaltyPoints")], PyRep::Integer(250));
    assert_eq!(
        rows[1][col_idx("killRightSupplied")],
        PyRep::Integer(90_000_009)
    );

    // 6. charUnboundMgr.GetCharacterInfo (KeyVal + non-KeyVal fallback)
    let cinfo = normalize_macho_rpc_reply(
        "charUnboundMgr",
        "GetCharacterInfo",
        Ok(PyRep::keyval([
            ("characterID", PyRep::Integer(90_000_001)),
            ("name", PyRep::String("Pilot One".into())),
        ])),
    )
    .expect("char info");
    assert!(matches!(cinfo, PyRep::List(_)));
    let cinfo_none =
        normalize_macho_rpc_reply("charUnboundMgr", "GetCharacterInfo", Ok(PyRep::None))
            .expect("char info none");
    assert_eq!(cinfo_none, PyRep::None);
}

#[test]
fn normalize_contract_corp_devindex_dogma_fw_homestation_inv_and_clones() {
    // corporationSvc
    let emp1 = normalize_macho_rpc_reply(
        "corporationSvc",
        "GetEmployementRecordAndCharacterTransfers",
        Ok(PyRep::List(Vec::new())),
    )
    .expect("emp list");
    assert!(matches!(emp1, PyRep::Tuple(t) if t.len() == 2));
    let emp2 = normalize_macho_rpc_reply(
        "corporationSvc",
        "GetEmployementRecordAndCharacterTransfers",
        Ok(PyRep::Tuple(vec![PyRep::Integer(1)])),
    )
    .expect("emp tuple");
    assert_eq!(emp2, PyRep::Tuple(vec![PyRep::Integer(1)]));

    // dogmaIM.OverloadRack passes through flat integer list, home_station, invbroker, jumpCloneSvc
    let ov = normalize_macho_rpc_reply(
        "dogmaIM",
        "OverloadRack",
        Ok(PyRep::List(vec![PyRep::Integer(1001)])),
    )
    .expect("overload");
    assert_eq!(ov, PyRep::List(vec![PyRep::Integer(1001)]));

    let hs = normalize_macho_rpc_reply(
        "home_station",
        "get_home_station_candidates",
        Ok(PyRep::List(vec![PyRep::Integer(60_003_760), PyRep::Bool(false)])),
    )
    .expect("home station");
    assert!(matches!(hs, PyRep::List(l) if l.len() == 2));
}

#[test]
fn normalize_map_paperdoll_peacetreaty_petitioner_planet_pos_raffle_reprocess_scan_ship_skill() {
    // map arms
    assert!(matches!(
        normalize_macho_rpc_reply(
            "map",
            "GetConstellationLPData",
            Ok(PyRep::List(vec![PyRep::keyval([("points", PyRep::Integer(10))])])),
        )
        .expect("lp"),
        PyRep::Rowset { .. }
    ));
    assert!(matches!(
        normalize_macho_rpc_reply(
            "map",
            "GetCurrentSovData",
            Ok(PyRep::List(vec![PyRep::keyval([("solarSystemID", PyRep::Integer(30_000_142))])])),
        )
        .expect("sov"),
        PyRep::Rowset { .. }
    ));
    assert!(matches!(
        normalize_macho_rpc_reply(
            "map",
            "GetMyExtraMapInfoAgents",
            Ok(PyRep::List(vec![PyRep::keyval([("agentID", PyRep::Integer(3_000_001))])])),
        )
        .expect("agents"),
        PyRep::Rowset { .. }
    ));
    assert!(matches!(
        normalize_macho_rpc_reply(
            "map",
            "GetSecurityModifiedSystems",
            Ok(PyRep::List(vec![PyRep::keyval([("solarSystemID", PyRep::Integer(30_000_142))])])),
        )
        .expect("sec mod"),
        PyRep::Rowset { .. }
    ));

    // dogmaIM.AddTarget, peaceTreatyMgr
    assert_eq!(
        normalize_macho_rpc_reply(
            "dogmaIM",
            "AddTarget",
            Ok(PyRep::keyval([("locked", PyRep::Bool(true)), ("targetID", PyRep::Integer(77))])),
        )
        .expect("add target already locked"),
        PyRep::Tuple(vec![PyRep::Integer(0), PyRep::List(vec![PyRep::Integer(77)])])
    );
    assert_eq!(
        normalize_macho_rpc_reply(
            "dogmaIM",
            "AddTarget",
            Ok(PyRep::keyval([("locked", PyRep::Bool(false)), ("targetID", PyRep::Integer(77))])),
        )
        .expect("add target async lock in progress"),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::List(Vec::new())])
    );
    assert_eq!(
        normalize_macho_rpc_reply(
            "dogmaIM",
            "AddTarget",
            Ok(PyRep::keyval([
                ("locked", PyRep::Bool(true)),
                ("targetID", PyRep::Integer(77)),
                ("targetIDs", PyRep::List(vec![PyRep::Integer(76), PyRep::Integer(77)])),
            ])),
        )
        .expect("add target reconciled list"),
        PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::List(vec![PyRep::Integer(76), PyRep::Integer(77)]),
        ])
    );

    // planetMgr.GMGetSynchedServerState & GetPlanetsForChar (KeyVal & Dict variants)
    let gm_kv = normalize_macho_rpc_reply(
        "planetMgr",
        "GMGetSynchedServerState",
        Ok(PyRep::keyval([
            ("lastTick", PyRep::Integer(123)),
            (
                "pins",
                PyRep::List(vec![
                    PyRep::keyval([("pinID", PyRep::Integer(1))]),
                    PyRep::Dict(vec![(PyRep::String("pinID".into()), PyRep::Integer(2))]),
                ]),
            ),
        ])),
    )
    .expect("gm planet kv");
    assert!(matches!(gm_kv, PyRep::Tuple(t) if t.len() == 2));

    let gm_dict = normalize_macho_rpc_reply(
        "planetMgr",
        "GMGetSynchedServerState",
        Ok(PyRep::Dict(vec![
            (PyRep::String("pins".into()), PyRep::List(Vec::new())),
        ])),
    )
    .expect("gm planet dict");
    assert!(matches!(gm_dict, PyRep::Tuple(t) if t.len() == 2));

    let planets = normalize_macho_rpc_reply(
        "planetMgr",
        "GetPlanetsForChar",
        Ok(PyRep::List(vec![
            PyRep::keyval([("pinsCount", PyRep::Integer(3))]),
            PyRep::Dict(vec![(PyRep::String("pinsCount".into()), PyRep::Integer(4))]),
        ])),
    )
    .expect("get planets");
    assert!(matches!(planets, PyRep::List(l) if l.len() == 2));

    // posMgr, raffleProxy, reprocessingSvc, scanMgr, ship, skillMgr2
    let towers = normalize_macho_rpc_reply(
        "posMgr",
        "GetControlTowers",
        Ok(PyRep::List(vec![
            PyRep::keyval([("structureID", PyRep::Integer(500)), ("typeID", PyRep::Integer(12235))]),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::None]),
        ])),
    )
    .expect("towers");
    assert!(matches!(towers, PyRep::List(l) if l.len() == 2));

    let quotes = normalize_macho_rpc_reply(
        "reprocessingSvc",
        "GetQuotes",
        Ok(PyRep::Tuple(vec![
            PyRep::Real(0.05),
            PyRep::Real(0.55),
            PyRep::List(vec![PyRep::keyval([
                ("itemID", PyRep::Integer(700)),
                ("typeID", PyRep::Integer(1230)),
                ("stationBaseYield", PyRep::Real(0.55)),
                ("quantityToProcess", PyRep::Integer(1000)),
                ("leftOvers", PyRep::Integer(0)),
                ("iskCost", PyRep::Real(0.0)),
                ("totalISKCost", PyRep::Real(0.0)),
                (
                    "outputs",
                    PyRep::List(vec![PyRep::keyval([
                        ("typeID", PyRep::Integer(34)),
                        ("quantity", PyRep::Integer(2288)),
                        ("unrecoverable", PyRep::Integer(1872)),
                        ("station", PyRep::Integer(0)),
                        ("iskCost", PyRep::Real(0.0)),
                    ])]),
                ),
            ])]),
        ])),
    )
    .expect("quotes");
    let PyRep::Tuple(ref qt) = quotes else {
        panic!("expected tuple");
    };
    assert_eq!(qt.len(), 3);
    let PyRep::Dict(ref eff_entries) = qt[1] else {
        panic!("expected stationEfficiency Dict");
    };
    assert!(eff_entries.iter().any(|(k, v)| *k == PyRep::None && *v == PyRep::Real(0.55)));
    assert!(eff_entries.iter().any(|(k, v)| *k == PyRep::Integer(1230) && *v == PyRep::Real(0.55)));
    let PyRep::Dict(ref qentries) = qt[2] else {
        panic!("expected normalized quotes Dict");
    };
    assert_eq!(qentries[0].0, PyRep::Integer(700));
    assert_eq!(
        qentries[0].1.dict_get("typeID").and_then(|v| v.as_i64()),
        Some(1230)
    );
    assert_eq!(
        qentries[0].1.dict_get("quantityToProcess").and_then(|v| v.as_i64()),
        Some(1000)
    );
    assert_eq!(
        qentries[0].1.dict_get("leftOvers").and_then(|v| v.as_i64()),
        Some(0)
    );
    let recov = qentries[0]
        .1
        .dict_get("recoverables")
        .expect("recoverables CRowset");
    assert!(recov.is_crowset(), "recoverables must be dbutil.CRowset, got {recov:?}");
    let recov_rows = recov.crowset_rows().expect("crowset rows");
    assert_eq!(recov_rows.len(), 1);
    assert_eq!(recov_rows[0].dbrow_get("typeID").and_then(|v| v.as_i64()), Some(34));
    assert_eq!(recov_rows[0].dbrow_get("client").and_then(|v| v.as_i64()), Some(2288));
    assert_eq!(recov_rows[0].dbrow_get("unrecoverable").and_then(|v| v.as_i64()), Some(1872));
    assert_eq!(recov_rows[0].dbrow_get("station").and_then(|v| v.as_i64()), Some(0));
    assert_eq!(recov_rows[0].dbrow_get("iskCost").and_then(|v| v.as_f64()), Some(0.0));
    let wire = eve_proto::marshal::encode(&quotes);
    let decoded = eve_proto::marshal::decode(&wire).expect("decode GetQuotes");
    let PyRep::Tuple(dt) = decoded else {
        panic!("decoded GetQuotes tuple");
    };
    let PyRep::Dict(d_entries) = &dt[2] else {
        panic!("decoded GetQuotes quotes dict");
    };
    let d_recov = d_entries[0].1.dict_get("recoverables").expect("decoded recoverables");
    assert!(d_recov.is_crowset());
    assert_eq!(d_recov.crowset_rows().unwrap()[0].dbrow_get("client").and_then(|v| v.as_i64()), Some(2288));

    // `< 2` args GetQuotes replies (`PyRep::List` and summary `PyRep::KeyVal`) also normalize to `(tax, eff_dict, {itemID: KeyVal})`
    let single_arg_list_quotes = normalize_macho_rpc_reply(
        "reprocessingSvc",
        "GetQuotes",
        Ok(PyRep::List(vec![PyRep::keyval([
            ("itemID", PyRep::Integer(701)),
            ("typeID", PyRep::Integer(1230)),
            ("stationBaseYield", PyRep::Real(0.50)),
            ("stationTaxRate", PyRep::Real(0.05)),
            ("quantityToProcess", PyRep::Integer(1000)),
            ("leftOvers", PyRep::Integer(0)),
            (
                "outputs",
                PyRep::List(vec![PyRep::keyval([
                    ("typeID", PyRep::Integer(34)),
                    ("quantity", PyRep::Integer(2000)),
                ])]),
            ),
        ])])),
    )
    .expect("1-arg list quotes normalized");
    let PyRep::Tuple(ref sq_tup) = single_arg_list_quotes else {
        panic!("expected 3-tuple from 1-arg list GetQuotes");
    };
    let PyRep::Dict(ref sq_dict) = sq_tup[2] else {
        panic!("expected quotes Dict from 1-arg list GetQuotes");
    };
    assert_eq!(sq_dict.len(), 1);
    assert_eq!(sq_dict[0].0, PyRep::Integer(701));
    assert_eq!(sq_dict[0].1.dict_get("typeID").and_then(PyRep::as_i64), Some(1230));

    let zero_arg_summary_quotes = normalize_macho_rpc_reply(
        "reprocessingSvc",
        "GetQuotes",
        Ok(PyRep::keyval([
            ("stationBaseYield", PyRep::Real(0.50)),
            ("stationTaxRate", PyRep::Real(0.05)),
        ])),
    )
    .expect("0-arg summary quotes normalized");
    assert!(matches!(
        zero_arg_summary_quotes,
        PyRep::Tuple(ref t) if t.len() == 3 && matches!(t[2], PyRep::Dict(ref d) if d.is_empty())
    ));

    let rec = normalize_macho_rpc_reply(
        "scanMgr",
        "RecoverProbes",
        Ok(PyRep::keyval([("recoveredProbeIDs", PyRep::List(vec![PyRep::Integer(9)]))])),
    )
    .expect("recover probes");
    assert_eq!(rec, PyRep::List(vec![PyRep::Integer(9)]));

    let fit = normalize_macho_rpc_reply(
        "ship",
        "GetShipFittingInfo",
        Ok(PyRep::keyval([
            ("cpuLoad", PyRep::Real(10.0)),
            ("cpuOutput", PyRep::Real(100.0)),
            ("powerLoad", PyRep::Real(20.0)),
            ("powerOutput", PyRep::Real(200.0)),
        ])),
    )
    .expect("ship fitting");
    assert!(matches!(fit, PyRep::Tuple(t) if t.len() == 4));
}

#[test]
fn normalize_contact_and_err_passthrough() {
    let cl = normalize_macho_rpc_reply("charMgr", "GetContactList", Ok(PyRep::KeyVal(HashMap::new()))).expect("cl");
    assert!(cl.dict_get("blocked").is_some());
    let err_pass = normalize_macho_rpc_reply("charMgr", "GetContactList", Err(BridgeRpcError { code: "contact_offline".into(), message: "offline".into(), client_error: None })).expect_err("err");
    assert_eq!(err_pass.code, "contact_offline");
}

#[test]
fn issue_771_and_776_get_members_paged_crowset_and_reprocess_list_compat() {
    // #771: PagedResultSet wraps a dbutil.CRowset (not util.Rowset) with 24 member columns.
    let cols = vec![
        "characterID", "name", "role", "title", "corporationID", "locationID", "shipTypeID",
        "logonDateTime", "roles", "grantableRoles", "rolesAtHQ", "grantableRolesAtHQ",
        "rolesAtBase", "grantableRolesAtBase", "rolesAtOther", "grantableRolesAtOther",
        "titleMask", "baseID", "startDateTime", "accountKey", "divisionID", "squadronID",
        "blockRoles", "blocksRoles",
    ];
    let row = vec![
        PyRep::Integer(90_000_001),
        PyRep::String("Pilot One".into()),
        PyRep::Integer(1),
        PyRep::String(String::new()),
        PyRep::Integer(98_000_001),
        PyRep::Integer(60_003_760),
        PyRep::Integer(587),
        PyRep::Integer(133_000_000_000_000_000),
        PyRep::Integer(1),
        PyRep::Integer(1),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::None,
        PyRep::Integer(133_000_000_000_000_000),
        PyRep::Integer(1000),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Integer(0),
        PyRep::Integer(0),
    ];
    let paged = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("eve.common.script.util.pagedCollection.PagedResultSet".into()),
            PyRep::Tuple(vec![
                PyRep::rowset(cols, vec![row]),
                PyRep::Integer(1),
                PyRep::Integer(0),
                PyRep::Integer(50),
            ]),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    let normalized = normalize_macho_rpc_reply("corpRegistry", "GetMembersPaged", Ok(paged))
        .expect("normalize GetMembersPaged");
    let wire = eve_proto::marshal::encode(&normalized);
    let roundtrip = eve_proto::marshal::decode(&wire).expect("decode GetMembersPaged");
    let PyRep::ObjectEx { is_type_2: false, header, .. } = roundtrip else {
        panic!("expected PagedResultSet ObjectEx");
    };
    let PyRep::Tuple(h) = header.as_ref() else { panic!("expected Tuple header") };
    let PyRep::Tuple(args) = &h[1] else { panic!("expected Tuple args") };
    let PyRep::ObjectEx { is_type_2: true, list, .. } = &args[0] else {
        panic!("expected dbutil.CRowset inside PagedResultSet, got {:?}", args[0]);
    };
    assert_eq!(list.len(), 1);
    assert!(matches!(&list[0], PyRep::DBRow { columns, .. } if columns.len() == 24));

    // #776: 1-arg batch Reprocess list normalized on wire to (inputItemIDs, outputByTypeID)
    let batch_reply = PyRep::List(vec![
        PyRep::keyval([
            ("itemID", PyRep::Integer(1001)),
            (
                "outputs",
                PyRep::List(vec![PyRep::keyval([
                    ("typeID", PyRep::Integer(34)),
                    ("quantity", PyRep::Integer(120)),
                ])]),
            ),
        ]),
        PyRep::keyval([
            ("itemID", PyRep::Integer(1002)),
            (
                "outputs",
                PyRep::List(vec![PyRep::keyval([
                    ("typeID", PyRep::Integer(34)),
                    ("quantity", PyRep::Integer(80)),
                ])]),
            ),
        ]),
    ]);
    let rep_norm = normalize_macho_rpc_reply("reprocessingSvc", "Reprocess", Ok(batch_reply))
        .expect("normalize Reprocess");
    let PyRep::Tuple(t) = rep_norm else {
        panic!("expected 2-tuple from Reprocess compat");
    };
    assert_eq!(t[0], PyRep::List(vec![PyRep::Integer(1001), PyRep::Integer(1002)]));
    assert_eq!(t[1], PyRep::Dict(vec![(PyRep::Integer(34), PyRep::Integer(200))]));

    // #955: dungeonInstanceCacheMgr.GetFactionWarfareBattlefieldInstances converts List -> Dict by solarSystemID
    let fw_bf = normalize_macho_rpc_reply(
        "dungeonInstanceCacheMgr",
        "GetFactionWarfareBattlefieldInstances",
        Ok(PyRep::List(vec![PyRep::keyval([
            ("instanceID", PyRep::Integer(7001)),
            ("solarSystemID", PyRep::Integer(30_002_537)),
            ("factionID", PyRep::Integer(500_003)),
        ])])),
    )
    .expect("normalize GetFactionWarfareBattlefieldInstances");
    let PyRep::Dict(bf_entries) = fw_bf else {
        panic!("expected Dict from GetFactionWarfareBattlefieldInstances");
    };
    assert_eq!(bf_entries.len(), 1);
    assert_eq!(bf_entries[0].0, PyRep::Integer(30_002_537));
    let PyRep::List(ref sys_bfs) = bf_entries[0].1 else {
        panic!("expected List of battlefield KeyVals");
    };
    assert_eq!(sys_bfs[0].dict_get("dungeonID"), Some(&PyRep::Integer(10_485)));
    assert_eq!(sys_bfs[0].dict_get("dungeonNameID"), Some(&PyRep::Integer(0)));

    // #952, #960, #963: OnDamageMessage, OnDamageMessages, OnCombatMessage, and OnGodmaPrimeItem sublocation 805
    assert!(should_suppress_macho_notification(
        "OnCombatMessage",
        &[PyRep::String("AttackHit".into()), PyRep::Dict(Vec::new())]
    ));
    assert!(should_suppress_macho_notification(
        "OnCombatMessage",
        &[PyRep::String("AttackedBy".into()), PyRep::Dict(Vec::new())]
    ));
    let cm_norm = normalize_macho_notification(
        "OnCombatMessage",
        vec![PyRep::String("AttackedBy".into()), PyRep::Dict(Vec::new())],
    );
    assert_eq!(cm_norm[0], PyRep::String("AttackHitsR".into()));

    let dmg_norm = normalize_macho_notification(
        "OnDamageMessage",
        vec![
            PyRep::String("AttackHit".into()),
            PyRep::Dict(vec![
                (PyRep::String("attackType".into()), PyRep::String("other".into())),
                (PyRep::String("damage".into()), PyRep::Real(42.5)),
                (PyRep::String("typeID".into()), PyRep::Integer(561)),
            ]),
        ],
    );
    assert_eq!(dmg_norm.len(), 1);
    assert_eq!(
        dmg_norm[0].dict_get("attackType"),
        Some(&PyRep::String("otherPlayerWeapons".into()))
    );
    assert_eq!(dmg_norm[0].dict_get("isBanked"), Some(&PyRep::Bool(false)));
    assert_eq!(dmg_norm[0].dict_get("hitQuality"), Some(&PyRep::Integer(3)));
    assert_eq!(dmg_norm[0].dict_get("weapon"), Some(&PyRep::Integer(561)));

    let dmgs_norm = normalize_macho_notification(
        "OnDamageMessages",
        vec![PyRep::List(vec![PyRep::Dict(vec![(
            PyRep::String("attackType".into()),
            PyRep::String("you".into()),
        )])])],
    );
    let PyRep::List(ref dmgs_list) = dmgs_norm[0] else {
        panic!("expected List in OnDamageMessages");
    };
    let PyRep::Tuple(ref dmg_pair) = dmgs_list[0] else {
        panic!("expected 2-tuple in OnDamageMessages");
    };
    assert_eq!(
        dmg_pair[1].dict_get("attackType"),
        Some(&PyRep::String("me".into()))
    );

    let subloc_prime = normalize_macho_notification(
        "OnGodmaPrimeItem",
        vec![
            PyRep::Integer(200_001),
            PyRep::keyval([
                (
                    "itemID",
                    PyRep::Tuple(vec![
                        PyRep::Integer(200_001),
                        PyRep::Integer(27),
                        PyRep::Integer(222),
                    ]),
                ),
                ("invItem", PyRep::None),
                (
                    "attributes",
                    PyRep::Dict(vec![(PyRep::Integer(80), PyRep::Real(120.0))]),
                ),
                ("time", PyRep::Integer(1_000)),
            ]),
        ],
    );
    let Some(PyRep::Dict(subloc_attrs)) = subloc_prime[1].dict_get("attributes") else {
        panic!("expected sublocation attributes dict");
    };
    assert!(subloc_attrs
        .iter()
        .any(|(k, v)| k.as_i64() == Some(805) && v.as_f64() == Some(120.0)));

    // #897: OnNewProbe includes scanBonuses with strength/deviation -> modules/ship
    let probe_norm = normalize_macho_notification(
        "OnNewProbe",
        vec![PyRep::keyval([
            ("probeID", PyRep::Integer(900_001)),
            ("typeID", PyRep::Integer(30_013)),
        ])],
    );
    let Some(PyRep::Dict(scan_bonuses)) = probe_norm[0].dict_get("scanBonuses") else {
        panic!("expected scanBonuses Dict on OnNewProbe");
    };
    assert!(scan_bonuses.iter().any(|(k, _)| k.as_str() == Some("strength")));
    assert!(scan_bonuses.iter().any(|(k, _)| k.as_str() == Some("deviation")));

    // #977: entity.CmdEngage / CmdReturnHome / CmdReturnBay normalize non-Dict replies to {}
    // (#1188: DoDestinyUpdate filter for OnDroneActivityChange removed; server emits notify only).
    for cmd in ["CmdEngage", "CmdReturnHome", "CmdReturnAndOrbit", "CmdReturnBay"] {
        let cmd_rep = normalize_macho_rpc_reply(
            "entity",
            cmd,
            Ok(PyRep::keyval([("ok", PyRep::Bool(true))])),
        )
        .expect("normalize drone cmd");
        assert_eq!(cmd_rep, PyRep::Dict(Vec::new()));
    }
    let destiny_norm = normalize_macho_notification(
        "DoDestinyUpdate",
        vec![
            PyRep::List(vec![
                PyRep::Tuple(vec![
                    PyRep::Integer(2069),
                    PyRep::Tuple(vec![
                        PyRep::String("OnDroneActivityChange".into()),
                        PyRep::Tuple(vec![
                            PyRep::Integer(1_896_919_139),
                            PyRep::Integer(1),
                            PyRep::String("attack".into()),
                        ]),
                    ]),
                ]),
                PyRep::Tuple(vec![
                    PyRep::Integer(2070),
                    PyRep::Tuple(vec![
                        PyRep::String("OnDroneStateChange".into()),
                        PyRep::Tuple(vec![PyRep::Integer(1_896_919_139)]),
                    ]),
                ]),
            ]),
            PyRep::Bool(false),
        ],
    );
    let PyRep::List(ref d_items) = destiny_norm[0] else {
        panic!("expected List in DoDestinyUpdate");
    };
    assert_eq!(d_items.len(), 2);

    // #978: industryManager.GetJob / GetJobsByOwner normalize V24.01 JobData fields and integer status
    let job_norm = normalize_macho_rpc_reply(
        "industryManager",
        "GetJob",
        Ok(PyRep::keyval([
            ("jobID", PyRep::Integer(501)),
            ("installerID", PyRep::Integer(90_000_001)),
            ("facilityID", PyRep::Integer(60_003_760)),
            ("blueprintItemID", PyRep::Integer(200_123_456)),
            ("blueprintTypeID", PyRep::Integer(967)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(3)),
            ("status", PyRep::String("Active".into())),
            ("durationSeconds", PyRep::Integer(600)),
            ("startUnix", PyRep::Integer(1_700_000_000)),
            ("endUnix", PyRep::Integer(1_700_000_600)),
            ("totalJobCostISK", PyRep::Real(1_250.0)),
        ])),
    )
    .expect("normalize industryManager.GetJob");
    assert_eq!(job_norm.dict_get("blueprintID"), Some(&PyRep::Integer(200_123_456)));
    assert_eq!(job_norm.dict_get("blueprintCopy"), Some(&PyRep::Integer(0)));
    assert_eq!(job_norm.dict_get("blueprintLocationID"), Some(&PyRep::Integer(60_003_760)));
    assert_eq!(job_norm.dict_get("blueprintLocationFlagID"), Some(&PyRep::Integer(4)));
    assert_eq!(job_norm.dict_get("status"), Some(&PyRep::Integer(1)));
    assert_eq!(job_norm.dict_get("licensedRuns"), Some(&PyRep::Integer(3)));
    assert_eq!(job_norm.dict_get("successfulRuns"), Some(&PyRep::Integer(3)));
    assert_eq!(job_norm.dict_get("timeInSeconds"), Some(&PyRep::Integer(600)));
    assert_eq!(job_norm.dict_get("cost"), Some(&PyRep::Real(1_250.0)));
    assert_eq!(job_norm.dict_get("outputLocationID"), Some(&PyRep::Integer(60_003_760)));
    assert_eq!(job_norm.dict_get("outputFlagID"), Some(&PyRep::Integer(4)));
    assert!(job_norm.dict_get("startDate").and_then(|v| v.as_i64()).unwrap_or(0) > 130_000_000_000_000_000);
    assert!(job_norm.dict_get("endDate").and_then(|v| v.as_i64()).unwrap_or(0) > 130_000_000_000_000_000);
}

#[test]
fn normalize_industry_job_promotes_elapsed_active_and_ready_paused_to_status_ready() {
    let mk_job = |job_id: i64, status: &str, pause_unix: Option<i64>| {
        let mut entries = vec![
            ("jobID", PyRep::Integer(job_id)),
            ("installerID", PyRep::Integer(90_000_001)),
            ("facilityID", PyRep::Integer(60_003_760)),
            ("blueprintItemID", PyRep::Integer(200_000_000 + job_id)),
            ("blueprintTypeID", PyRep::Integer(967)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            ("status", PyRep::String(status.into())),
            ("durationSeconds", PyRep::Integer(600)),
            ("startUnix", PyRep::Integer(1_700_000_000)),
            ("endUnix", PyRep::Integer(1_700_000_600)),
        ];
        if let Some(p) = pause_unix {
            entries.push(("pauseUnix", PyRep::Integer(p)));
        }
        PyRep::keyval(entries)
    };

    // Before endUnix (1_700_000_599 < 1_700_000_600), an active job stays STATUS_INSTALLED (1).
    let before_end = normalize_macho_rpc_reply_at(
        "industryManager",
        "GetJob",
        Ok(mk_job(1, "Active", None)),
        Some(1_700_000_599),
    )
    .expect("GetJob before endUnix");
    assert_eq!(before_end.dict_get("status"), Some(&PyRep::Integer(1)));

    // At and past endUnix, an active job promotes to STATUS_READY (3) (#1052).
    for now in [1_700_000_600_u64, 1_700_001_000_u64] {
        let at_or_past = normalize_macho_rpc_reply_at(
            "industryManager",
            "GetJob",
            Ok(mk_job(2, "Active", None)),
            Some(now),
        )
        .expect("GetJob at or past endUnix");
        assert_eq!(at_or_past.dict_get("status"), Some(&PyRep::Integer(3)));
    }

    // Paused jobs stay STATUS_PAUSED (2) unless paused at/after endUnix (`industryCommon.JobStatus`).
    let paused_early = normalize_macho_rpc_reply_at(
        "industryManager",
        "GetJob",
        Ok(mk_job(3, "Paused", Some(1_700_000_599))),
        Some(1_700_001_000),
    )
    .expect("GetJob paused before endUnix");
    assert_eq!(paused_early.dict_get("status"), Some(&PyRep::Integer(2)));

    let paused_ready = normalize_macho_rpc_reply_at(
        "industryManager",
        "GetJob",
        Ok(mk_job(4, "Paused", Some(1_700_000_600))),
        Some(1_700_001_000),
    )
    .expect("GetJob paused at endUnix");
    assert_eq!(paused_ready.dict_get("status"), Some(&PyRep::Integer(3)));

    // Terminal statuses must remain unchanged even past endUnix.
    let delivered = normalize_macho_rpc_reply_at(
        "industryManager",
        "GetJob",
        Ok(mk_job(5, "Delivered", None)),
        Some(1_700_001_000),
    )
    .expect("GetJob delivered");
    assert_eq!(delivered.dict_get("status"), Some(&PyRep::Integer(101)));

    let cancelled = normalize_macho_rpc_reply_at(
        "industryManager",
        "GetJob",
        Ok(mk_job(6, "Cancelled", None)),
        Some(1_700_001_000),
    )
    .expect("GetJob cancelled");
    assert_eq!(cancelled.dict_get("status"), Some(&PyRep::Integer(102)));

    // GetJobsByOwner applies the same elapsed promotion across a mixed list.
    let mut future_active = mk_job(7, "Active", None);
    if let PyRep::KeyVal(ref mut map) = future_active {
        map.insert("endUnix".into(), PyRep::Integer(1_700_001_200));
    }
    let owner_list = normalize_macho_rpc_reply_at(
        "industryManager",
        "GetJobsByOwner",
        Ok(PyRep::List(vec![
            mk_job(8, "Active", None),
            future_active,
            mk_job(9, "Delivered", None),
        ])),
        Some(1_700_000_600),
    )
    .expect("GetJobsByOwner mixed list");
    let PyRep::List(rows) = owner_list else {
        panic!("expected List from GetJobsByOwner normalization");
    };
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].dict_get("status"), Some(&PyRep::Integer(3)));
    assert_eq!(rows[1].dict_get("status"), Some(&PyRep::Integer(1)));
    assert_eq!(rows[2].dict_get("status"), Some(&PyRep::Integer(101)));
}

#[tokio::test]
async fn c9_typed_handlers_corp_dev_fw_peace_emit_canonical_wire_shapes() {
    let ctx = crate::router::ServerContext::new();
    let fw_sys = 30_002_537_u32;
    let non_fw_sys = 30_000_142_u32;
    let (uid, corp_id) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get(&90_000_001).unwrap();
        let uid = ch.user_id;
        let cid = ch.corporation_id;
        snap.corporations
            .entry(cid)
            .or_insert_with(|| eve_db::CorporationRecord::new(cid, "Alpha", "ALPH", 90_000_001))
            .members
            .extend([90_000_001, 90_000_002]);
        let gov = snap.corp_governance.entry(cid).or_default();
        gov.reinforce_hour = 18;
        gov.reinforce_weekday = 5;
        gov.role_audit_log.push((
            90_000_002,
            90_000_001,
            0,
            8,
            1_700_000_500,
        ));
        let st = snap.system_states.entry(non_fw_sys).or_default();
        st.military_index = 5;
        st.industrial_index = 4;
        st.strategic_index = 3;
        snap.fw_system_occupations.insert(
            fw_sys,
            eve_db::FwSystemOccupationRecord {
                solar_system_id: fw_sys,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 1_500,
                victory_points_threshold: 3_000,
            },
        );
        snap.system_states.entry(fw_sys).or_default().fw_contested_pct = 50.0;
        (uid, cid)
    });
    let mut sess = eve_machonet::SessionState {
        user_id: Some(uid),
        character_id: Some(90_000_001),
        corporation_id: Some(corp_id),
        solar_system_id: Some(non_fw_sys),
        corp_role: u64::MAX,
        ..Default::default()
    };
    let kw = HashMap::new();

    // corpRegistry.CanLeaveCurrentCorporation -> (bool, UserError KeyVal, Dict)
    let leave = ctx
        .dispatch_rpc(
            &mut sess,
            "corpRegistry",
            None,
            "CanLeaveCurrentCorporation",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("CanLeaveCurrentCorporation");
    let PyRep::Tuple(leave_parts) = leave else {
        panic!("expected 3-tuple from CanLeaveCurrentCorporation");
    };
    assert_eq!(leave_parts.len(), 3);
    assert_eq!(leave_parts[0], PyRep::Bool(false));
    assert_eq!(
        leave_parts[1].dict_get("msg").and_then(PyRep::as_str),
        Some("CrpCantQuitNotInStasis")
    );
    assert_eq!(
        leave_parts[1].dict_get("__class__").and_then(PyRep::as_str),
        Some("UserError")
    );

    // corpRegistry.GetStructureReinforceDefault -> (hour, 0), or scalar weekday
    let reinf = ctx
        .dispatch_rpc(
            &mut sess,
            "corpRegistry",
            None,
            "GetStructureReinforceDefault",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetStructureReinforceDefault");
    assert_eq!(reinf, PyRep::Tuple(vec![PyRep::Integer(18), PyRep::Integer(0)]));
    let reinf_day = ctx
        .dispatch_rpc(
            &mut sess,
            "corpRegistry",
            None,
            "GetStructureReinforceDefault",
            &[PyRep::String("weekday".into())],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetStructureReinforceDefault weekday");
    assert_eq!(reinf_day, PyRep::Integer(5));

    // corpmgr.AuditMember -> (Rowset, Rowset)
    let audit = ctx
        .dispatch_rpc(
            &mut sess,
            "corpmgr",
            None,
            "AuditMember",
            &[PyRep::Integer(90_000_002)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("AuditMember");
    let PyRep::Tuple(audit_parts) = audit else {
        panic!("expected 2-tuple from AuditMember");
    };
    assert_eq!(audit_parts.len(), 2);
    assert_eq!(audit_parts[0], audit_parts[1]);
    assert!(matches!(&audit_parts[0], PyRep::Rowset { rows, .. } if rows.len() == 1));

    // devIndexManager.GetAllDevelopmentIndices -> Dict keyed by solarSystemID
    let dev = ctx
        .dispatch_rpc(
            &mut sess,
            "devIndexManager",
            None,
            "GetAllDevelopmentIndices",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetAllDevelopmentIndices");
    let PyRep::Dict(dev_entries) = dev else {
        panic!("expected Dict from GetAllDevelopmentIndices");
    };
    let (_, jita_dev) = dev_entries
        .iter()
        .find(|(k, _)| *k == PyRep::Integer(i64::from(non_fw_sys)))
        .expect("non_fw_sys dev index entry");
    assert_eq!(jita_dev.dict_get("militaryPoints"), Some(&PyRep::Integer(5_000)));
    assert_eq!(jita_dev.dict_get("industrialPoints"), Some(&PyRep::Integer(4_000)));
    assert_eq!(jita_dev.dict_get("claimedFor"), Some(&PyRep::Integer(3)));
    assert!(jita_dev.dict_get("adm").and_then(PyRep::as_f64).is_some());

    // fwVictoryPointMgr.GetLocalVictoryPointState -> (None, 0) outside warzone, (sys_id, KeyVal) inside
    let vp_none = ctx
        .dispatch_rpc(
            &mut sess,
            "fwVictoryPointMgr",
            None,
            "GetLocalVictoryPointState",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetLocalVictoryPointState non-FW");
    assert_eq!(vp_none, PyRep::Tuple(vec![PyRep::None, PyRep::Integer(0)]));

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = fw_sys;
    });
    sess.solar_system_id = Some(fw_sys);
    let vp_local = ctx
        .dispatch_rpc(
            &mut sess,
            "fwVictoryPointMgr",
            None,
            "GetLocalVictoryPointState",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetLocalVictoryPointState FW");
    let PyRep::Tuple(vp_pair) = vp_local else {
        panic!("expected 2-tuple from GetLocalVictoryPointState");
    };
    assert_eq!(vp_pair[0], PyRep::Integer(i64::from(fw_sys)));
    assert_eq!(vp_pair[1].dict_get("score"), Some(&PyRep::Integer(1_500)));
    assert_eq!(vp_pair[1].dict_get("threshold"), Some(&PyRep::Integer(3_000)));
    assert_eq!(vp_pair[1].dict_get("contestedFraction"), Some(&PyRep::Real(0.5)));
    assert_eq!(vp_pair[1].dict_get("isContested"), Some(&PyRep::Bool(true)));
    assert_eq!(vp_pair[1].dict_get("isVulnerable"), Some(&PyRep::Bool(false)));

    let vp_all = ctx
        .dispatch_rpc(
            &mut sess,
            "fwVictoryPointMgr",
            None,
            "GetAllVictoryPointStates",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetAllVictoryPointStates");
    let PyRep::Dict(vp_all_entries) = vp_all else {
        panic!("expected Dict from GetAllVictoryPointStates");
    };
    let (_, fw_entry) = vp_all_entries
        .iter()
        .find(|(k, _)| *k == PyRep::Integer(i64::from(fw_sys)))
        .expect("fw_sys entry in GetAllVictoryPointStates");
    assert_eq!(fw_entry.dict_get("contestedFraction"), Some(&PyRep::Real(0.5)));
    assert_eq!(fw_entry.dict_get("isContested"), Some(&PyRep::Bool(true)));
    assert_eq!(fw_entry.dict_get("isVulnerable"), Some(&PyRep::Bool(false)));

    // peaceTreatyMgr.GetPeaceTreatiesForSession -> (outgoing, incoming) 2-tuple
    let peace = ctx
        .dispatch_rpc(
            &mut sess,
            "peaceTreatyMgr",
            None,
            "GetPeaceTreatiesForSession",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetPeaceTreatiesForSession");
    assert_eq!(
        peace,
        PyRep::Tuple(vec![PyRep::List(Vec::new()), PyRep::List(Vec::new())])
    );
}

#[test]
fn normalize_on_godma_ship_effect_ensures_non_zero_filetime_and_seven_tuple_env() {
    let short = normalize_macho_notification(
        "OnGodmaShipEffect",
        vec![
            PyRep::Integer(200_018_015),
            PyRep::Integer(101),
            PyRep::Bool(true),
        ],
    );
    assert_eq!(short.len(), 11);
    assert!(short[2].as_i64().unwrap() > i32::MAX as i64);
    assert!(short[6].as_i64().unwrap() > i32::MAX as i64);
    assert!(matches!(&short[5], PyRep::Tuple(t) if t.len() == 7));

    let valid_ft: i64 = 134_384_360_643_585_500;
    let ten_zero_ft = normalize_macho_notification(
        "OnGodmaShipEffect",
        vec![
            PyRep::Integer(200_018_015),
            PyRep::Integer(101),
            PyRep::Integer(0),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Tuple(vec![
                PyRep::Integer(200_018_015),
                PyRep::Integer(700_000_001),
                PyRep::Integer(101),
            ]),
            PyRep::Integer(0),
            PyRep::Real(4000.0),
            PyRep::Integer(0),
            PyRep::None,
        ],
    );
    assert_eq!(ten_zero_ft.len(), 11);
    assert!(ten_zero_ft[2].as_i64().unwrap() > i32::MAX as i64);
    assert!(ten_zero_ft[6].as_i64().unwrap() > i32::MAX as i64);
    assert_eq!(ten_zero_ft[7], PyRep::Real(4000.0));
    assert_eq!(ten_zero_ft[8], PyRep::Integer(0));
    assert!(matches!(&ten_zero_ft[5], PyRep::Tuple(t) if t.len() == 7));

    // Sub-2000ms durations must remain PyRep::Real (never coerced to int) so V24 Python 2
    // `ModuleButton.DoActivateRampsThread` avoids `500 / 1000 == 0` ZeroDivisionError.
    let sub_2s_real = normalize_macho_notification(
        "OnGodmaShipEffect",
        vec![
            PyRep::Integer(200_018_015),
            PyRep::Integer(101),
            PyRep::Integer(valid_ft),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Tuple(vec![PyRep::Integer(200_018_015)]),
            PyRep::Integer(valid_ft),
            PyRep::Real(1000.0),
            PyRep::Integer(1000),
            PyRep::None,
        ],
    );
    assert_eq!(sub_2s_real[7], PyRep::Real(1000.0));
    let mut sub_2s_int_in = sub_2s_real.clone();
    sub_2s_int_in[7] = PyRep::Integer(1000);
    let sub_2s_int = normalize_macho_notification("OnGodmaShipEffect", sub_2s_int_in);
    assert_eq!(sub_2s_int[7], PyRep::Real(1000.0));
    assert_eq!(short[7], PyRep::Real(0.0));

    let preserved = normalize_macho_notification(
        "OnGodmaShipEffect",
        vec![
            PyRep::Integer(200_018_015),
            PyRep::Integer(101),
            PyRep::Integer(valid_ft),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Tuple(vec![
                PyRep::Integer(200_018_015),
                PyRep::Integer(90_000_001),
                PyRep::Integer(700_000_001),
                PyRep::None,
                PyRep::None,
                PyRep::List(Vec::new()),
                PyRep::Integer(101),
            ]),
            PyRep::Integer(valid_ft),
            PyRep::Real(4000.0),
            PyRep::Integer(0),
            PyRep::None,
            PyRep::None,
        ],
    );
    assert_eq!(preserved[2].as_i64(), Some(valid_ft));
    assert_eq!(preserved[6].as_i64(), Some(valid_ft));

    let str_eff = normalize_macho_notification(
        "OnGodmaShipEffect",
        vec![
            PyRep::Integer(200_165_898),
            PyRep::String("miningLaser".into()),
            PyRep::Bool(true),
        ],
    );
    assert_eq!(str_eff[1], PyRep::Integer(67));
    assert!(matches!(&str_eff[5], PyRep::Tuple(t) if t.get(6) == Some(&PyRep::Integer(67))));

    let sfx = normalize_macho_notification(
        "OnSpecialFX",
        vec![
            PyRep::Integer(200_164_799),
            PyRep::Integer(200_165_898),
            PyRep::Integer(483),
            PyRep::Integer(200_185_369),
            PyRep::None,
            PyRep::String("effects.Laser".into()),
            PyRep::Bool(false),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Real(60_000.0),
            PyRep::Integer(1000),
            PyRep::Integer(valid_ft),
            PyRep::Real(0.0),
            PyRep::None,
        ],
    );
    assert_eq!(sfx.len(), 14);
    assert_eq!(sfx[5], PyRep::String("effects.Laser".into()));
    assert_eq!(sfx[11], PyRep::Integer(valid_ft));
}

#[test]
fn wrapped_named_user_error_body_emits_unmacho_destination_for_moniker_retry() {
    let body = eve_machonet::wrapped_named_user_error_body(
        6,
        "UnMachoDestination",
        "InvalidBoundObject: Bound object 10136 does not exist",
    );
    let PyRep::Tuple(parts) = body else {
        panic!("expected ErrorResponse tuple body");
    };
    assert_eq!(parts[0].as_i64(), Some(6));
    assert_eq!(parts[1].as_i64(), Some(eve_machonet::MACHONETERR_WRAPPEDEXCEPTION));
    let PyRep::Tuple(inner) = &parts[2] else {
        panic!("expected inner tuple");
    };
    let PyRep::Bytes(raw) = &inner[0] else {
        panic!("expected marshaled exception bytes");
    };
    let decoded = eve_proto::marshal::decode(raw).expect("valid marshal bytes");
    let PyRep::ObjectEx { header, .. } = decoded else {
        panic!("expected ObjectEx UserError");
    };
    let PyRep::Tuple(h_parts) = *header else {
        panic!("expected ObjectEx header tuple");
    };
    assert_eq!(h_parts[0], PyRep::Token("eveexceptions.UserError".into()));
    let PyRep::Tuple(u_args) = &h_parts[1] else {
        panic!("expected UserError args tuple");
    };
    assert_eq!(u_args[0].as_str(), Some("UnMachoDestination"));
}

#[test]
fn corp_info_keyval_normalizes_to_util_row_for_live_client() {
    let kv = PyRep::KeyVal(
        [
            ("corporationID".to_string(), PyRep::Integer(98_100_001)),
            (
                "corporationName".to_string(),
                PyRep::String("Demo Corp".into()),
            ),
        ]
        .into_iter()
        .collect(),
    );
    for svc in ["corpRegistry", "machoNet"] {
        let res = normalize_macho_rpc_reply(svc, "GetCorporation", Ok(kv.clone())).unwrap();
        let PyRep::Object { type_name, args } = res else {
            panic!("expected util.Row PyRep::Object for {svc}");
        };
        assert_eq!(type_name, "util.Row");
        let PyRep::Tuple(state) = *args else {
            panic!("expected 2-tuple (header, line)");
        };
        let (PyRep::List(hdr), PyRep::List(row)) = (&state[0], &state[1]) else {
            panic!("expected header and line lists");
        };
        let pairs: HashMap<&str, &PyRep> = hdr
            .iter()
            .zip(row.iter())
            .map(|(k, v)| (k.as_str().unwrap(), v))
            .collect();
        assert_eq!(pairs.get("corporationID"), Some(&&PyRep::Integer(98_100_001)));
        assert_eq!(
            pairs.get("corporationName"),
            Some(&&PyRep::String("Demo Corp".into()))
        );
    }
}

#[test]
fn signature_instances_normalize_nested_dungeon_map_to_system_counts() {
    let input = PyRep::Dict(vec![
        (
            PyRep::Integer(30_000_142),
            PyRep::Dict(vec![
                (
                    PyRep::Integer(100),
                    PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                ),
                (PyRep::Integer(101), PyRep::List(vec![PyRep::Integer(3)])),
            ]),
        ),
        (PyRep::Integer(30_002_187), PyRep::Dict(Vec::new())),
    ]);
    let res = normalize_macho_rpc_reply(
        "dungeonInstanceCacheMgr",
        "GetSignatureInstances",
        Ok(input),
    )
    .unwrap();
    let PyRep::Dict(entries) = res else {
        panic!("expected Dict");
    };
    let by_sys: HashMap<i64, i64> = entries
        .into_iter()
        .filter_map(|(k, v)| Some((k.as_i64()?, v.as_i64()?)))
        .collect();
    assert_eq!(by_sys.get(&30_000_142), Some(&3));
    assert_eq!(by_sys.get(&30_002_187), Some(&0));

    // #806: GetIceBeltInstances / GetCombatAnomalyInstances / GetOreAnomalyInstances /
    // GetFactionWarfareInstances must normalize `{sys_id: {did: [iid, ...]}}` into
    // `{sys_id: [KeyVal(dungeonID=..., instanceID=..., solarSystemID=..., factionID=...), ...]}`.
    for method in [
        "GetIceBeltInstances",
        "GetCombatAnomalyInstances",
        "GetOreAnomalyInstances",
        "GetFactionWarfareInstances",
    ] {
        let raw = PyRep::Dict(vec![(
            PyRep::Integer(30_000_142),
            PyRep::Dict(vec![(
                PyRep::Integer(2868),
                PyRep::List(vec![PyRep::Integer(9001), PyRep::Integer(9002)]),
            )]),
        )]);
        let norm =
            normalize_macho_rpc_reply("dungeonInstanceCacheMgr", method, Ok(raw)).unwrap();
        let PyRep::Dict(sys_entries) = norm else {
            panic!("expected Dict from {method}");
        };
        assert_eq!(sys_entries[0].0, PyRep::Integer(30_000_142));
        let (_, PyRep::List(dng_list)) = &sys_entries[0] else {
            panic!("expected List of KeyVals per solarSystemID from {method}");
        };
        assert_eq!(dng_list.len(), 2);
        assert_eq!(
            dng_list[0].dict_get("dungeonID"),
            Some(&PyRep::Integer(2868))
        );
        assert_eq!(
            dng_list[0].dict_get("instanceID"),
            Some(&PyRep::Integer(9001))
        );
        assert_eq!(
            dng_list[0].dict_get("solarSystemID"),
            Some(&PyRep::Integer(30_000_142))
        );
        assert_eq!(
            dng_list[0].dict_get("factionID"),
            Some(&PyRep::Integer(500_004))
        );
    }

    // Verify every FW faction_id mapping arm in GetFactionWarfareInstances normalization.
    for &(did, expected_fac) in &[
        (2894_i64, 500_002_i64),
        (2900, 500_002),
        (2908, 500_002),
        (2911, 500_002),
        (9892, 500_002),
        (9909, 500_002),
        (2774, 500_003),
        (2789, 500_003),
        (2799, 500_003),
        (2805, 500_003),
        (9578, 500_003),
        (9736, 500_003),
        (2868, 500_004),
        (2879, 500_004),
        (2893, 500_004),
        (2903, 500_004),
        (9893, 500_004),
        (9911, 500_004),
        (10485, 500_011),
        (2869, 500_001),
        (100, 500_001),
    ] {
        let raw = PyRep::Dict(vec![(
            PyRep::Integer(30_002_813),
            PyRep::Dict(vec![(
                PyRep::Integer(did),
                PyRep::List(vec![PyRep::Integer(1)]),
            )]),
        )]);
        let norm = normalize_macho_rpc_reply(
            "dungeonInstanceCacheMgr",
            "GetFactionWarfareInstances",
            Ok(raw),
        )
        .unwrap();
        let PyRep::Dict(entries) = norm else {
            panic!("expected Dict");
        };
        let (_, PyRep::List(items)) = &entries[0] else {
            panic!("expected List");
        };
        assert_eq!(
            items[0].dict_get("factionID"),
            Some(&PyRep::Integer(expected_fac)),
            "did {did} must map to faction {expected_fac}"
        );
    }

    // #806: GetPirateInsurgencyInstances and GetPirateInsurgencyIceHeistInstances must group
    // flat KeyVal lists by solarSystemID and populate default dungeonID and factionID.
    for (method, expected_did) in [
        ("GetPirateInsurgencyInstances", 10_477_i64),
        ("GetPirateInsurgencyIceHeistInstances", 10_497_i64),
    ] {
        let ins_raw = PyRep::List(vec![
            PyRep::keyval([
                ("instanceID", PyRep::Integer(7001)),
                ("solarSystemID", PyRep::Integer(30_002_813)),
            ]),
            PyRep::keyval([("instanceID", PyRep::Integer(7002))]),
        ]);
        let ins_norm =
            normalize_macho_rpc_reply("dungeonInstanceCacheMgr", method, Ok(ins_raw)).unwrap();
        let PyRep::Dict(ins_sys) = ins_norm else {
            panic!("expected Dict from {method}");
        };
        assert_eq!(ins_sys.len(), 1);
        assert_eq!(ins_sys[0].0, PyRep::Integer(30_002_813));
        let (_, PyRep::List(ins_items)) = &ins_sys[0] else {
            panic!("expected List under solarSystemID");
        };
        assert_eq!(
            ins_items[0].dict_get("dungeonID"),
            Some(&PyRep::Integer(expected_did))
        );
        assert_eq!(
            ins_items[0].dict_get("factionID"),
            Some(&PyRep::Integer(500_010))
        );
        assert_eq!(
            ins_items[0].dict_get("instanceID"),
            Some(&PyRep::Integer(7001))
        );
    }
}

#[test]
fn get_titles_rowset_normalizes_to_dict_of_util_rows_and_empty_dict() {
    let cols = vec![
        "titleID",
        "titleName",
        "roles",
        "grantableRoles",
        "rolesAtHQ",
        "grantableRolesAtHQ",
        "rolesAtBase",
        "grantableRolesAtBase",
        "rolesAtOther",
        "grantableRolesAtOther",
    ];
    let rowset = PyRep::rowset(
        cols.clone(),
        vec![
            vec![
                PyRep::Integer(1),
                PyRep::String("Commander".into()),
                PyRep::Integer(256),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
            ],
            vec![
                PyRep::Integer(2),
                PyRep::String("Quartermaster".into()),
                PyRep::Integer(8192),
                PyRep::Integer(128),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
            ],
        ],
    );

    for svc in ["corpRegistry", "machoNet"] {
        let res = normalize_macho_rpc_reply(svc, "GetTitles", Ok(rowset.clone())).unwrap();
        let PyRep::Dict(entries) = res else {
            panic!("expected Dict for {svc}.GetTitles");
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, PyRep::Integer(1));
        assert_eq!(entries[1].0, PyRep::Integer(2));

        let PyRep::Object { type_name, args } = &entries[0].1 else {
            panic!("expected util.Row object");
        };
        assert_eq!(type_name, "util.Row");
        let PyRep::Tuple(parts) = args.as_ref() else {
            panic!("expected (header, line) tuple");
        };
        let (PyRep::List(hdr), PyRep::List(line)) = (&parts[0], &parts[1]) else {
            panic!("expected header & line lists");
        };
        assert_eq!(hdr.len(), 10);
        assert_eq!(line.len(), 10);
        assert_eq!(line[1], PyRep::String("Commander".into()));
        assert_eq!(line[2], PyRep::Integer(256));
    }

    let empty_rs = PyRep::rowset(cols, vec![]);
    let empty_res = normalize_macho_rpc_reply("corpRegistry", "GetTitles", Ok(empty_rs)).unwrap();
    assert_eq!(empty_res, PyRep::Dict(vec![]));
}

#[test]
fn cloak_ball_and_special_fx_uncloak_normalize_to_v24_client_format() {
    let ship_id = 100_000_042_i64;

    let uncloak_bool = normalize_destiny_payload_arg(PyRep::Tuple(vec![
        PyRep::String("CloakBall".into()),
        PyRep::Tuple(vec![PyRep::Integer(ship_id), PyRep::Bool(false)]),
    ]));
    assert_eq!(
        uncloak_bool,
        PyRep::Tuple(vec![
            PyRep::String("UncloakBall".into()),
            PyRep::Tuple(vec![PyRep::Integer(ship_id)]),
        ])
    );

    let uncloak_int = normalize_destiny_payload_arg(PyRep::Tuple(vec![
        PyRep::String("CloakBall".into()),
        PyRep::Tuple(vec![PyRep::Integer(ship_id), PyRep::Integer(0)]),
    ]));
    assert_eq!(
        uncloak_int,
        PyRep::Tuple(vec![
            PyRep::String("UncloakBall".into()),
            PyRep::Tuple(vec![PyRep::Integer(ship_id)]),
        ])
    );

    let cloak_on = PyRep::Tuple(vec![
        PyRep::String("CloakBall".into()),
        PyRep::Tuple(vec![PyRep::Integer(ship_id), PyRep::Bool(true)]),
    ]);
    assert_eq!(normalize_destiny_payload_arg(cloak_on.clone()), cloak_on);

    let fx_uncloak = normalize_destiny_payload_arg(PyRep::Tuple(vec![
        PyRep::String("OnSpecialFX".into()),
        PyRep::Tuple(vec![
            PyRep::Integer(ship_id),
            PyRep::None,
            PyRep::None,
            PyRep::None,
            PyRep::None,
            PyRep::String("effects.Cloak".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ]),
    ]));
    let PyRep::Tuple(outer) = fx_uncloak else {
        panic!("expected OnSpecialFX tuple");
    };
    assert_eq!(outer[0], PyRep::String("OnSpecialFX".into()));
    let PyRep::Tuple(args) = &outer[1] else {
        panic!("expected FX args tuple");
    };
    assert_eq!(args[5], PyRep::String("effects.Uncloak".into()));
    assert_eq!(args[7], PyRep::Integer(1));
    assert_eq!(args[8], PyRep::Integer(0));
    assert_eq!(args[9], PyRep::Integer(6000));
    assert_eq!(args[10], PyRep::Integer(1));
}

#[test]
fn issue_766_agent_mgr_v24_dialogue_briefing_objective_and_location_normalization() {
    // 1. DoAction offered mission -> ((agentSaysMsg, availableActions), lastActionInfo)
    let do_action = normalize_macho_rpc_reply(
        "agentMgr",
        "DoAction",
        Ok(PyRep::keyval([
            ("missionID", PyRep::Integer(501)),
            ("contentID", PyRep::Integer(501)),
            ("agentID", PyRep::Integer(3_018_910)),
            ("state", PyRep::String("Offered".into())),
            ("missionState", PyRep::Integer(1)),
            ("briefing", PyRep::String("Clear pirates".into())),
            (
                "dialogue",
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(3), PyRep::String("Accept".into())]),
                    PyRep::Tuple(vec![PyRep::Integer(9), PyRep::String("Decline".into())]),
                ]),
            ),
            ("lpReward", PyRep::Integer(450)),
        ])),
    )
    .expect("DoAction ok");

    let PyRep::Tuple(outer) = do_action else {
        panic!("DoAction must return 2-tuple");
    };
    assert_eq!(outer.len(), 2);
    let PyRep::Tuple(inner) = &outer[0] else {
        panic!("first element must be (agentSaysMsg, availableActions)");
    };
    assert_eq!(
        inner[0],
        PyRep::Tuple(vec![
            PyRep::String("Clear pirates".into()),
            PyRep::Integer(501),
        ])
    );
    assert_eq!(
        inner[1],
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(3)]),
            PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(9)]),
        ])
    );
    assert!(matches!(outer[1], PyRep::Dict(_)), "lastActionInfo must be Dict");
    assert_eq!(outer[1].dict_get("missionCompleted"), Some(&PyRep::Bool(false)));
    assert_eq!(outer[1].dict_get("missionDeclined"), Some(&PyRep::Bool(false)));
    assert_eq!(outer[1].dict_get("missionQuit"), Some(&PyRep::Bool(false)));
    assert_eq!(outer[1].dict_get("missionCantReplay"), Some(&PyRep::None));
    assert_eq!(outer[1].dict_get("loyaltyPoints"), Some(&PyRep::Integer(450)));

    // Idempotent if already a 2-tuple
    assert!(matches!(
        normalize_macho_rpc_reply("agentMgr", "DoAction", Ok(PyRep::Tuple(outer.clone()))),
        Ok(PyRep::Tuple(_))
    ));

    // Completed mission sets missionCompleted=true and fallback message
    let completed = normalize_macho_rpc_reply(
        "agentMgr",
        "DoAction",
        Ok(PyRep::keyval([
            ("missionID", PyRep::Integer(501)),
            ("state", PyRep::String("Completed".into())),
            ("missionState", PyRep::Integer(4)),
        ])),
    )
    .unwrap();
    let PyRep::Tuple(c_outer) = completed else { panic!("tuple") };
    assert_eq!(c_outer[1].dict_get("missionCompleted"), Some(&PyRep::Bool(true)));

    // 2. GetAgentLocationWrap -> Dict with solarsystemID, locationID, typeID
    let loc = normalize_macho_rpc_reply(
        "agentMgr",
        "GetAgentLocationWrap",
        Ok(PyRep::keyval([
            ("agentID", PyRep::Integer(3_018_910)),
            ("solarSystemID", PyRep::Integer(30_000_142)),
            ("stationID", PyRep::Integer(60_003_760)),
        ])),
    )
    .unwrap();
    assert!(matches!(loc, PyRep::Dict(_)));
    assert_eq!(loc.dict_get("solarsystemID"), Some(&PyRep::Integer(30_000_142)));
    assert_eq!(loc.dict_get("locationID"), Some(&PyRep::Integer(60_003_760)));
    assert_eq!(loc.dict_get("typeID"), Some(&PyRep::Integer(5)));

    // 3. GetMissionBriefingInfo -> Dict keys + MissionNotActive -> None
    let no_briefing = normalize_macho_rpc_reply(
        "agentMgr",
        "GetMissionBriefingInfo",
        Err(eve_proto::BridgeRpcError {
            code: "MissionNotActive".into(),
            message: "none".into(),
            client_error: None,
        }),
    )
    .unwrap();
    assert_eq!(no_briefing, PyRep::None);

    let briefing = normalize_macho_rpc_reply(
        "agentMgr",
        "GetMissionBriefingInfo",
        Ok(PyRep::keyval([
            ("missionID", PyRep::Integer(501)),
            ("title", PyRep::String("Eliminate Pirates".into())),
            ("briefing", PyRep::String("Go there.".into())),
        ])),
    )
    .unwrap();
    assert!(matches!(briefing, PyRep::Dict(_)));
    assert_eq!(
        briefing.dict_get("Mission Title ID"),
        Some(&PyRep::String("Eliminate Pirates".into()))
    );
    assert_eq!(briefing.dict_get("ContentID"), Some(&PyRep::Integer(501)));
    assert_eq!(briefing.dict_get("Decline Time"), Some(&PyRep::Integer(-1)));
    assert!(matches!(briefing.dict_get("Mission Keywords"), Some(PyRep::Dict(_))));
}
