//! Unit and mutation-killing tests for `crate::quasar_grpc` RPC reply normalization,
//! notification normalization, Quasar protobuf payload coercion, and HTTP/2 gRPC transport.

use crate::quasar_grpc::*;
use eve_proto::{BridgeRpcError, PyRep};

#[test]
fn normalize_macho_rpc_reply_covers_structure_jump_chat_siphon_and_blueprints() {
    // Error passthrough
    let err_res = normalize_macho_rpc_reply(
        "structureDirectory",
        "GetJumpBridgesWithMyAccess",
        Err(BridgeRpcError::user_error("Denied")),
    );
    assert!(err_res.is_err());

    // structureDirectory.GetJumpBridgesWithMyAccess with linkedSolarSystemID fallback
    let jb = normalize_macho_rpc_reply(
        "structureDirectory",
        "GetJumpBridgesWithMyAccess",
        Ok(PyRep::List(vec![PyRep::keyval([
            ("structureID", PyRep::Integer(101)),
            ("solarSystemID", PyRep::Integer(30_000_142)),
            ("linkedStructureID", PyRep::Integer(102)),
            ("linkedSolarSystemID", PyRep::Integer(30_000_144)),
        ])])),
    )
    .unwrap();
    let PyRep::Tuple(jb_parts) = jb else {
        panic!("expected tuple");
    };
    assert_eq!(jb_parts.len(), 3);
    assert_eq!(jb_parts[1], PyRep::List(vec![PyRep::Integer(101)]));

    // jumpTimers.GetTimers with missing keys defaults to None
    let jt = normalize_macho_rpc_reply(
        "jumpTimers",
        "GetTimers",
        Ok(PyRep::Dict(Vec::new())),
    )
    .unwrap();
    assert_eq!(
        jt,
        PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None])
    );

    // XmppChatMgr.ResyncSystemChannelAccess
    let chat = normalize_macho_rpc_reply(
        "XmppChatMgr",
        "ResyncSystemChannelAccess",
        Ok(PyRep::keyval([(
            "channels",
            PyRep::List(vec![PyRep::String("local".into())]),
        )])),
    )
    .unwrap();
    assert_eq!(chat, PyRep::List(vec![PyRep::String("local".into())]));

    // beyonce.GetCapacityOfSiphon
    let siphon = normalize_macho_rpc_reply(
        "beyonce",
        "GetCapacityOfSiphon",
        Ok(PyRep::Real(0.35)),
    )
    .unwrap();
    assert_eq!(siphon.dict_get("capacity"), Some(&PyRep::Real(1_000.0)));
    assert_eq!(siphon.dict_get("used"), Some(&PyRep::Real(350.0)));

    // blueprintManager.GetBlueprintDataByOwner empty list stays empty
    let bp_empty = normalize_macho_rpc_reply(
        "blueprintManager",
        "GetBlueprintDataByOwner",
        Ok(PyRep::List(Vec::new())),
    )
    .unwrap();
    let PyRep::Tuple(bp_parts) = bp_empty else {
        panic!("expected tuple");
    };
    let PyRep::List(bp_list) = &bp_parts[0] else {
        panic!("expected list");
    };
    assert!(bp_list.is_empty());
}

#[test]
fn normalize_macho_rpc_reply_covers_char_contracts_corp_and_dev_indices() {
    // charMgr.GetOwnerNoteLabels short string, valid string, and empty list
    let notes = normalize_macho_rpc_reply(
        "charMgr",
        "GetOwnerNoteLabels",
        Ok(PyRep::List(vec![
            PyRep::String("X".into()),
            PyRep::String("N:Bookmarks".into()),
        ])),
    )
    .unwrap();
    let PyRep::List(note_items) = notes else {
        panic!("expected list");
    };
    assert_eq!(note_items.len(), 2);
    assert_eq!(
        note_items[0].dict_get("label"),
        Some(&PyRep::String("N:General".into()))
    );
    assert_eq!(
        note_items[1].dict_get("label"),
        Some(&PyRep::String("N:Bookmarks".into()))
    );

    let notes_empty = normalize_macho_rpc_reply(
        "charMgr",
        "GetOwnerNoteLabels",
        Ok(PyRep::List(Vec::new())),
    )
    .unwrap();
    if let PyRep::List(items) = notes_empty {
        assert!(items.is_empty());
    } else {
        panic!("expected list");
    }

    // charUnboundMgr.GetCharacterInfo
    let cinfo = normalize_macho_rpc_reply(
        "charUnboundMgr",
        "GetCharacterInfo",
        Ok(PyRep::keyval([
            ("characterID", PyRep::Integer(90_000_001)),
            ("name", PyRep::String("Pilot".into())),
        ])),
    )
    .unwrap();
    let PyRep::List(cinfo_list) = cinfo else {
        panic!("expected list");
    };
    assert_eq!(
        cinfo_list[0].dict_get("characterName"),
        Some(&PyRep::String("Pilot".into()))
    );
    assert_eq!(
        cinfo_list[0].dict_get("characterID"),
        Some(&PyRep::Integer(90_000_001))
    );

    // contractProxy methods
    let clist = normalize_macho_rpc_reply(
        "contractProxy",
        "GetContractListForOwner",
        Ok(PyRep::List(Vec::new())),
    )
    .unwrap();
    assert!(clist.dict_get("contracts").is_some());
    assert!(clist.dict_get("items").is_some());

    let cnt = normalize_macho_rpc_reply(
        "contractProxy",
        "NumOutstandingContracts",
        Ok(PyRep::Integer(4)),
    )
    .unwrap();
    assert_eq!(cnt.dict_get("myCharTotal"), Some(&PyRep::Integer(4)));
    assert_eq!(cnt.dict_get("nonCorpForMyChar"), Some(&PyRep::Integer(4)));

    let search_empty = normalize_macho_rpc_reply(
        "contractProxy",
        "SearchContracts",
        Ok(PyRep::List(Vec::new())),
    )
    .unwrap();
    assert_eq!(search_empty.dict_get("numFound"), Some(&PyRep::Integer(0)));

    let search_raw = normalize_macho_rpc_reply(
        "contractProxy",
        "SearchContracts",
        Ok(PyRep::List(vec![PyRep::keyval([(
            "contractID",
            PyRep::Integer(77),
        )])])),
    )
    .unwrap();
    assert_eq!(search_raw.dict_get("numFound"), Some(&PyRep::Integer(1)));

    // corpRegistry & corpmgr & corporationSvc & devIndexManager
    let leave = normalize_macho_rpc_reply(
        "corpRegistry",
        "CanLeaveCurrentCorporation",
        Ok(PyRep::keyval([("canLeave", PyRep::Bool(true))])),
    )
    .unwrap();
    if let PyRep::Tuple(parts) = leave {
        assert_eq!(parts[0], PyRep::Bool(true));
    } else {
        panic!("expected tuple");
    }

    let reinf = normalize_macho_rpc_reply(
        "corpRegistry",
        "GetStructureReinforceDefault",
        Ok(PyRep::Integer(18)),
    )
    .unwrap();
    assert_eq!(reinf, PyRep::Tuple(vec![PyRep::Integer(18), PyRep::Integer(0)]));

    let audit = normalize_macho_rpc_reply(
        "corpmgr",
        "AuditMember",
        Ok(PyRep::rowset(vec!["a"], Vec::new())),
    )
    .unwrap();
    assert!(matches!(audit, PyRep::Tuple(_)));

    let emp = normalize_macho_rpc_reply(
        "corporationSvc",
        "GetEmployementRecordAndCharacterTransfers",
        Ok(PyRep::rowset(
            vec!["recordID", "corporationID", "startDate"],
            vec![vec![PyRep::Integer(1), PyRep::Integer(98_000_009), PyRep::Integer(1_700_000_000)]],
        )),
    )
    .unwrap();
    assert!(matches!(emp, PyRep::Tuple(_)));

    let dev = normalize_macho_rpc_reply(
        "devIndexManager",
        "GetAllDevelopmentIndices",
        Ok(PyRep::List(Vec::new())),
    )
    .unwrap();
    if let PyRep::List(items) = dev {
        assert!(items.is_empty());
    } else {
        panic!("expected list");
    }
}

#[test]
fn normalize_macho_rpc_reply_covers_dogma_map_planet_raffle_skill_war_and_store() {
    assert!(matches!(
        normalize_macho_rpc_reply("dogmaIM", "OverloadRack", Ok(PyRep::Dict(Vec::new()))).unwrap(),
        PyRep::List(_)
    ));
    assert_eq!(
        normalize_macho_rpc_reply(
            "fwVictoryPointMgr",
            "GetLocalVictoryPointState",
            Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(30_000_142)),
                ("state", PyRep::Integer(1))
            ]))
        )
        .unwrap(),
        PyRep::Tuple(vec![PyRep::Integer(30_000_142), PyRep::Integer(1)])
    );
    let hs = normalize_macho_rpc_reply(
        "home_station",
        "get_home_station_candidates",
        Ok(PyRep::List(vec![PyRep::Integer(60_003_760)])),
    )
    .unwrap();
    assert!(matches!(hs, PyRep::List(_)));

    assert_eq!(
        normalize_macho_rpc_reply("invbroker", "ALSCLogGet", Ok(PyRep::None)).unwrap(),
        PyRep::None
    );
    assert_eq!(
        normalize_macho_rpc_reply("invbroker", "TrashItems", Ok(PyRep::List(vec![PyRep::Integer(42)]))).unwrap(),
        PyRep::List(vec![PyRep::Integer(42)])
    );
    let add_tgt = normalize_macho_rpc_reply(
        "dogmaIM",
        "AddTarget",
        Ok(PyRep::keyval([
            ("locked", PyRep::Bool(false)),
            ("targetID", PyRep::Integer(555)),
        ])),
    )
    .unwrap();
    assert_eq!(
        add_tgt,
        PyRep::Tuple(vec![PyRep::Integer(0), PyRep::List(vec![PyRep::Integer(555)])])
    );

    assert!(normalize_macho_rpc_reply("jumpCloneSvc", "GetCloneState", Ok(PyRep::None)).is_ok());
    assert!(normalize_macho_rpc_reply("map", "GetConstellationLPData", Ok(PyRep::None)).is_ok());
    assert!(normalize_macho_rpc_reply("map", "GetCurrentSovData", Ok(PyRep::None)).is_ok());
    assert!(normalize_macho_rpc_reply("map", "GetMyExtraMapInfoAgents", Ok(PyRep::None)).is_ok());
    assert!(normalize_macho_rpc_reply("map", "GetSecurityModifiedSystems", Ok(PyRep::None)).is_ok());
    assert!(normalize_macho_rpc_reply("paperDollServer", "GetPaperDollPortraitDataFor", Ok(PyRep::None)).is_ok());
    assert_eq!(
        normalize_macho_rpc_reply("peaceTreatyMgr", "GetPeaceTreatiesForSession", Ok(PyRep::List(Vec::new()))).unwrap(),
        PyRep::Tuple(vec![PyRep::List(Vec::new()), PyRep::List(Vec::new())])
    );

    let pet = normalize_macho_rpc_reply(
        "petitioner",
        "GetCategoryHierarchicalInfo",
        Ok(PyRep::Tuple(vec![
            PyRep::None,
            PyRep::None,
            PyRep::None,
            PyRep::None,
        ])),
    )
    .unwrap();
    if let PyRep::Tuple(parts) = pet {
        assert!(matches!(parts[3], PyRep::Dict(_)));
    }

    assert!(normalize_macho_rpc_reply("planetMgr", "GMGetSynchedServerState", Ok(PyRep::None)).is_ok());
    assert!(normalize_macho_rpc_reply("planetMgr", "GetPlanetsForChar", Ok(PyRep::None)).is_ok());
    assert!(normalize_macho_rpc_reply("raffleProxy", "GetMyRaffleHistory", Ok(PyRep::List(Vec::new()))).is_ok());
    assert!(normalize_macho_rpc_reply("reprocessingSvc", "GetQuotes", Ok(PyRep::Tuple(vec![PyRep::Real(0.03)]))).is_ok());
    assert_eq!(
        normalize_macho_rpc_reply("scanMgr", "RecoverProbes", Ok(PyRep::Dict(Vec::new()))).unwrap(),
        PyRep::List(Vec::new())
    );
    assert!(matches!(
        normalize_macho_rpc_reply("ship", "GetShipFittingInfo", Ok(PyRep::Dict(Vec::new()))).unwrap(),
        PyRep::Tuple(_)
    ));

    // skillMgr2.GetSkillQueueAndFreePoints with Rowset and List
    let sq_rowset = normalize_macho_rpc_reply(
        "skillMgr2",
        "GetSkillQueueAndFreePoints",
        Ok(PyRep::Tuple(vec![
            PyRep::rowset(
                vec!["trainingTypeID", "trainingToLevel"],
                vec![vec![PyRep::Integer(3301), PyRep::Integer(4)]],
            ),
            PyRep::Integer(50_000),
        ])),
    )
    .unwrap();
    assert!(matches!(sq_rowset, PyRep::Tuple(_)));

    let sq_list = normalize_macho_rpc_reply(
        "skillMgr2",
        "GetSkillQueueAndFreePoints",
        Ok(PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::keyval([("trainingTypeID", PyRep::Integer(3302))])]),
            PyRep::Integer(0),
        ])),
    )
    .unwrap();
    assert!(matches!(sq_list, PyRep::Tuple(_)));

    assert!(normalize_macho_rpc_reply(
        "charMgr",
        "GetContactList",
        Ok(PyRep::keyval([("contacts", PyRep::List(Vec::new()))]))
    )
    .is_ok());
    assert!(normalize_macho_rpc_reply("charMgr", "GetRecentShipKillsAndLosses", Ok(PyRep::None)).is_ok());
    assert!(normalize_macho_rpc_reply("stationSvc", "GetAllianceSystems", Ok(PyRep::List(vec![PyRep::Integer(30_000_142)]))).is_ok());
    assert!(normalize_macho_rpc_reply("stationSvc", "GetSolarSystem", Ok(PyRep::Integer(30_000_142))).is_ok());
    assert!(normalize_macho_rpc_reply("structureAssetSafety", "GetStructuresICanDeliverTo", Ok(PyRep::List(Vec::new()))).is_ok());
    assert_eq!(
        normalize_macho_rpc_reply("structureCompressionMgr", "GetMyGasDecompressionEfficiency", Ok(PyRep::Real(0.88))).unwrap(),
        PyRep::Tuple(vec![PyRep::Real(0.88), PyRep::Real(0.88)])
    );
    assert!(normalize_macho_rpc_reply("warStatisticMgr", "GetBaseInfo", Ok(PyRep::Dict(Vec::new()))).is_ok());
    assert!(matches!(
        normalize_macho_rpc_reply("warsInfoMgr", "GetPublicWarInfo", Ok(PyRep::Dict(Vec::new()))).unwrap(),
        PyRep::List(_)
    ));
    let store = normalize_macho_rpc_reply(
        "storeManager",
        "get_categories",
        Ok(PyRep::List(vec![PyRep::keyval([("id", PyRep::Integer(1))])])),
    )
    .unwrap();
    if let PyRep::List(items) = store {
        assert!(matches!(items[0], PyRep::Dict(_)));
    }
}
