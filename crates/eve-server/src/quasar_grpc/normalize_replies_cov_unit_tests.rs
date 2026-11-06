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

    let siphon =
        normalize_macho_rpc_reply("beyonce", "GetCapacityOfSiphon", Ok(PyRep::Real(0.25)))
            .expect("siphon");
    assert_eq!(siphon.dict_get("capacity"), Some(&PyRep::Real(1_000.0)));
    assert_eq!(siphon.dict_get("used"), Some(&PyRep::Real(250.0)));

    // 4. blueprintManager.GetBlueprintDataByOwner
    let bps = normalize_macho_rpc_reply(
        "blueprintManager",
        "GetBlueprintDataByOwner",
        Ok(PyRep::List(vec![
            PyRep::keyval([("locationID", PyRep::Integer(60_003_760))]),
            PyRep::keyval([("locationID", PyRep::Integer(60_003_760))]),
        ])),
    )
    .expect("bps");
    let PyRep::Tuple(bp_parts) = bps else {
        panic!("expected tuple");
    };
    assert_eq!(bp_parts.len(), 2);

    // 5. charMgr.GetOwnerNoteLabels & GetRecentShipKillsAndLosses
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

    let kills = normalize_macho_rpc_reply(
        "charMgr",
        "GetRecentShipKillsAndLosses",
        Ok(PyRep::List(vec![PyRep::DBRow {
            header: Box::new(PyRep::None),
            columns: vec![
                ("killID".into(), PyRep::Integer(55)),
                ("victimCharacterID".into(), PyRep::Integer(90_000_001)),
                ("attackerID".into(), PyRep::Integer(90_000_002)),
            ],
            raw_rle: Vec::new(),
        }])),
    )
    .expect("kills");
    assert!(matches!(kills, PyRep::Rowset { .. }));

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
    // contractProxy arms
    let cl = normalize_macho_rpc_reply(
        "contractProxy",
        "GetContractListForOwner",
        Ok(PyRep::List(vec![PyRep::Integer(1)])),
    )
    .expect("contracts list");
    assert_eq!(cl.dict_get("contracts"), Some(&PyRep::List(vec![PyRep::Integer(1)])));

    let oc = normalize_macho_rpc_reply(
        "contractProxy",
        "NumOutstandingContracts",
        Ok(PyRep::Integer(4)),
    )
    .expect("outstanding");
    assert_eq!(oc.dict_get("myCharTotal"), Some(&PyRep::Integer(4)));

    let sc = normalize_macho_rpc_reply(
        "contractProxy",
        "SearchContracts",
        Ok(PyRep::List(vec![
            PyRep::keyval([("contractID", PyRep::Integer(10))]),
            PyRep::keyval([
                ("contract", PyRep::Integer(11)),
                ("bids", PyRep::List(Vec::new())),
            ]),
        ])),
    )
    .expect("search contracts");
    assert_eq!(sc.dict_get("numFound"), Some(&PyRep::Integer(2)));

    // corpRegistry, corpmgr, corporationSvc, devIndexManager
    let leave = normalize_macho_rpc_reply(
        "corpRegistry",
        "CanLeaveCurrentCorporation",
        Ok(PyRep::keyval([("canLeave", PyRep::Bool(false))])),
    )
    .expect("can leave");
    assert!(matches!(leave, PyRep::Tuple(t) if t.len() == 3));

    let reinf = normalize_macho_rpc_reply(
        "corpRegistry",
        "GetStructureReinforceDefault",
        Ok(PyRep::Integer(18)),
    )
    .expect("reinforce");
    assert_eq!(
        reinf,
        PyRep::Tuple(vec![PyRep::Integer(18), PyRep::Integer(0)])
    );

    let audit = normalize_macho_rpc_reply("corpmgr", "AuditMember", Ok(PyRep::Integer(9)))
        .expect("audit");
    assert_eq!(audit, PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(9)]));

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

    let dev = normalize_macho_rpc_reply(
        "devIndexManager",
        "GetAllDevelopmentIndices",
        Ok(PyRep::List(vec![PyRep::keyval([
            ("solarSystemID", PyRep::Integer(30_000_142)),
            ("military", PyRep::Integer(5)),
            ("industrial", PyRep::Integer(4)),
            ("strategic", PyRep::Integer(3)),
            ("adm", PyRep::Real(4.2)),
        ])])),
    )
    .expect("dev index");
    assert!(matches!(dev, PyRep::List(l) if l.len() == 1));

    // dogmaIM.OverloadRack, fwVictoryPointMgr, home_station, invbroker, jumpCloneSvc
    let ov = normalize_macho_rpc_reply(
        "dogmaIM",
        "OverloadRack",
        Ok(PyRep::keyval([
            ("rackIndex", PyRep::Integer(27)),
            ("moduleIDs", PyRep::List(vec![PyRep::Integer(1001)])),
        ])),
    )
    .expect("overload");
    assert_eq!(
        ov,
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1001), PyRep::Integer(27)])])
    );

    assert_eq!(
        normalize_macho_rpc_reply("fwVictoryPointMgr", "GetLocalVictoryPointState", Ok(PyRep::None))
            .expect("fw none"),
        PyRep::Tuple(vec![PyRep::None, PyRep::Integer(0)])
    );
    assert_eq!(
        normalize_macho_rpc_reply(
            "fwVictoryPointMgr",
            "GetLocalVictoryPointState",
            Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(30_002_001)),
                ("state", PyRep::Integer(2)),
            ]))
        )
        .expect("fw dict"),
        PyRep::Tuple(vec![PyRep::Integer(30_002_001), PyRep::Integer(2)])
    );

    let hs = normalize_macho_rpc_reply(
        "home_station",
        "get_home_station_candidates",
        Ok(PyRep::List(vec![PyRep::Integer(60_003_760), PyRep::Bool(false)])),
    )
    .expect("home station");
    assert!(matches!(hs, PyRep::List(l) if l.len() == 2));

    let alsc = normalize_macho_rpc_reply(
        "invbroker",
        "ALSCLogGet",
        Ok(PyRep::keyval([("entries", PyRep::List(vec![PyRep::Integer(8)]))])),
    )
    .expect("alsc");
    assert_eq!(alsc, PyRep::List(vec![PyRep::Integer(8)]));

    assert_eq!(
        normalize_macho_rpc_reply("invbroker", "TrashItems", Ok(PyRep::None)).expect("trash none"),
        PyRep::List(Vec::new())
    );
    assert!(matches!(
        normalize_macho_rpc_reply("invbroker", "TrashItems", Ok(PyRep::Dict(Vec::new())))
            .expect("trash dict"),
        PyRep::List(l) if l.len() == 1
    ));

    let jc = normalize_macho_rpc_reply(
        "jumpCloneSvc",
        "GetCloneState",
        Ok(PyRep::keyval([
            ("clones", PyRep::List(vec![PyRep::keyval([("jumpCloneID", PyRep::Integer(1))])])),
            ("implants", PyRep::List(vec![PyRep::keyval([("implantID", PyRep::Integer(2))])])),
        ])),
    )
    .expect("clone state");
    assert!(jc.dict_get("clones").is_some());
    assert!(jc.dict_get("implants").is_some());
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

    // paperDollServer, dogmaIM.AddTarget, peaceTreatyMgr
    assert!(matches!(
        normalize_macho_rpc_reply("paperDollServer", "GetPaperDollPortraitDataFor", Ok(PyRep::Dict(Vec::new()))).expect("pd"),
        PyRep::Tuple(t) if t.len() == 2
    ));
    assert_eq!(
        normalize_macho_rpc_reply(
            "dogmaIM",
            "AddTarget",
            Ok(PyRep::keyval([("locked", PyRep::Bool(true)), ("targetID", PyRep::Integer(77))])),
        )
        .expect("add target"),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(77)])])
    );
    assert_eq!(
        normalize_macho_rpc_reply("peaceTreatyMgr", "GetPeaceTreatiesForSession", Ok(PyRep::List(Vec::new())))
            .expect("peace"),
        PyRep::Tuple(vec![PyRep::List(Vec::new()), PyRep::List(Vec::new())])
    );

    // petitioner.GetCategoryHierarchicalInfo (None, Dict, List variants)
    for fourth in [
        PyRep::None,
        PyRep::Dict(vec![
            (PyRep::Integer(1), PyRep::String("Billing".into())),
            (
                PyRep::Integer(2),
                PyRep::Tuple(vec![PyRep::String("Parent".into()), PyRep::String("Stuck".into())]),
            ),
        ]),
        PyRep::List(vec![PyRep::Integer(5)]),
    ] {
        let pet = normalize_macho_rpc_reply(
            "petitioner",
            "GetCategoryHierarchicalInfo",
            Ok(PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None, fourth])),
        )
        .expect("petitioner");
        assert!(matches!(pet, PyRep::Tuple(t) if t.len() == 4));
    }

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

    let raffles = normalize_macho_rpc_reply(
        "raffleProxy",
        "GetMyRaffleHistory",
        Ok(PyRep::List(vec![
            PyRep::keyval([("running_id", PyRep::Integer(99))]),
            PyRep::keyval([("type_id", PyRep::Integer(587))]),
        ])),
    )
    .expect("raffle");
    assert!(matches!(raffles, PyRep::Tuple(t) if t.len() == 2));

    let quotes = normalize_macho_rpc_reply(
        "reprocessingSvc",
        "GetQuotes",
        Ok(PyRep::Tuple(vec![
            PyRep::Real(0.05),
            PyRep::Real(0.55),
            PyRep::List(vec![PyRep::keyval([
                ("itemID", PyRep::Integer(700)),
                (
                    "outputs",
                    PyRep::List(vec![PyRep::keyval([
                        ("typeID", PyRep::Integer(34)),
                        ("quantity", PyRep::Integer(100)),
                    ])]),
                ),
            ])]),
        ])),
    )
    .expect("quotes");
    assert!(matches!(quotes, PyRep::Tuple(t) if t.len() == 3));

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

    let sq_rowset = normalize_macho_rpc_reply(
        "skillMgr2",
        "GetSkillQueueAndFreePoints",
        Ok(PyRep::Tuple(vec![
            PyRep::rowset(vec!["trainingTypeID", "trainingToLevel"], vec![vec![PyRep::Integer(3329), PyRep::Integer(2)]]),
            PyRep::Integer(50_000),
        ])),
    )
    .expect("skill queue rowset");
    assert!(matches!(sq_rowset, PyRep::Tuple(t) if t.len() == 2));

    let sq_list = normalize_macho_rpc_reply(
        "skillMgr2",
        "GetSkillQueueAndFreePoints",
        Ok(PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::keyval([("trainingTypeID", PyRep::Integer(3329))])]),
            PyRep::Integer(0),
        ])),
    )
    .expect("skill queue list");
    assert!(matches!(sq_list, PyRep::Tuple(t) if t.len() == 2));
}

#[test]
fn normalize_contact_station_safety_compression_wars_store_and_err_passthrough() {
    let cl = normalize_macho_rpc_reply("charMgr", "GetContactList", Ok(PyRep::KeyVal(HashMap::new()))).expect("cl");
    assert!(cl.dict_get("blocked").is_some());
    let asys = normalize_macho_rpc_reply("stationSvc", "GetAllianceSystems", Ok(PyRep::List(vec![PyRep::Integer(30_000_142), PyRep::None]))).expect("asys");
    assert!(matches!(asys, PyRep::List(l) if l.len() == 2));
    let ss_int = normalize_macho_rpc_reply("stationSvc", "GetSolarSystem", Ok(PyRep::Integer(30_000_142))).expect("ss_int");
    assert_eq!(ss_int.dict_get("factionID"), Some(&PyRep::None));
    let ss_dict = normalize_macho_rpc_reply("stationSvc", "GetSolarSystem", Ok(PyRep::Dict(vec![(PyRep::String("solarSystemID".into()), PyRep::Integer(30_000_142))]))).expect("ss_dict");
    assert_eq!(ss_dict.dict_get("factionID"), Some(&PyRep::None));
    let safety = normalize_macho_rpc_reply("structureAssetSafety", "GetStructuresICanDeliverTo", Ok(PyRep::List(vec![PyRep::Integer(1_000_000_000_001)]))).expect("safety");
    assert_eq!(safety, PyRep::Tuple(vec![PyRep::List(vec![PyRep::Integer(1_000_000_000_001)]), PyRep::Integer(1_000_000_000_001)]));
    let comp = normalize_macho_rpc_reply("structureCompressionMgr", "GetMyGasDecompressionEfficiency", Ok(PyRep::Real(0.85))).expect("comp");
    assert_eq!(comp, PyRep::Tuple(vec![PyRep::Real(0.85), PyRep::Real(0.85)]));
    let war_base = normalize_macho_rpc_reply("warStatisticMgr", "GetBaseInfo", Ok(PyRep::keyval([("declaredByID", PyRep::Integer(1_000_001)), ("againstID", PyRep::Integer(1_000_002))]))).expect("war_base");
    assert!(matches!(war_base, PyRep::Tuple(t) if t.len() == 4));
    let pub_war = normalize_macho_rpc_reply("warsInfoMgr", "GetPublicWarInfo", Ok(PyRep::keyval([("warID", PyRep::Integer(10))]))).expect("pub_war");
    assert!(matches!(pub_war, PyRep::List(l) if l.len() == 1));
    let store = normalize_macho_rpc_reply("storeManager", "get_products", Ok(PyRep::keyval([("id", PyRep::Integer(7))]))).expect("store");
    assert!(matches!(store, PyRep::Dict(_)));
    let err_pass = normalize_macho_rpc_reply("storeManager", "get_products", Err(BridgeRpcError { code: "store_offline".into(), message: "offline".into() })).expect_err("err");
    assert_eq!(err_pass.code, "store_offline");
}
