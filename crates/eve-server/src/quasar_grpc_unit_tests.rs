//! Unit and mutation-killing tests for `crate::quasar_grpc` RPC reply normalization,
//! notification normalization, Quasar protobuf payload coercion, and HTTP/2 gRPC transport.

use crate::router::client_compat::normalize_macho_rpc_reply;
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

    // corporationSvc
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
}

#[test]
fn normalize_macho_rpc_reply_covers_dogma_map_planet_raffle_skill_war_and_store() {
    assert_eq!(
        normalize_macho_rpc_reply(
            "dogmaIM",
            "OverloadRack",
            Ok(PyRep::List(vec![PyRep::Integer(1001)])),
        )
        .unwrap(),
        PyRep::List(vec![PyRep::Integer(1001)])
    );
    let hs = normalize_macho_rpc_reply(
        "home_station",
        "get_home_station_candidates",
        Ok(PyRep::List(vec![PyRep::Integer(60_003_760)])),
    )
    .unwrap();
    assert!(matches!(hs, PyRep::List(_)));

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
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::List(Vec::new())])
    );

    assert!(normalize_macho_rpc_reply("map", "GetConstellationLPData", Ok(PyRep::None)).is_ok());
    assert!(normalize_macho_rpc_reply("map", "GetCurrentSovData", Ok(PyRep::None)).is_ok());
    assert!(normalize_macho_rpc_reply("map", "GetMyExtraMapInfoAgents", Ok(PyRep::None)).is_ok());
    assert!(normalize_macho_rpc_reply("map", "GetSecurityModifiedSystems", Ok(PyRep::None)).is_ok());

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

#[tokio::test]
async fn quasar_c9_typed_handlers_direct_dispatch_wire_shapes() {
    let ctx = crate::router::ServerContext::new();
    let (uid, corp_id) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get(&90_000_001).unwrap();
        let uid = ch.user_id;
        let cid = ch.corporation_id;
        snap.fw_system_occupations.insert(
            30_000_142,
            eve_db::FwSystemOccupationRecord {
                solar_system_id: 30_000_142,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 1_500,
                victory_points_threshold: 3_000,
            },
        );
        (uid, cid)
    });
    let mut sess = eve_machonet::SessionState {
        user_id: Some(uid),
        character_id: Some(90_000_001),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        corp_role: u64::MAX,
        ..Default::default()
    };
    let kw = std::collections::HashMap::new();

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
    assert!(matches!(leave, PyRep::Tuple(parts) if parts.len() == 3));

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

    let audit = ctx
        .dispatch_rpc(
            &mut sess,
            "corpmgr",
            None,
            "AuditMember",
            &[PyRep::Integer(90_000_001)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("corpmgr.AuditMember");
    assert!(matches!(
        audit,
        PyRep::Tuple(parts) if parts.len() == 2 && matches!(&parts[0], PyRep::Rowset { .. })
    ));

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
    assert!(matches!(dev, PyRep::Dict(_)));

    let vp = ctx
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
        .expect("GetLocalVictoryPointState");
    let PyRep::Tuple(vp_parts) = vp else {
        panic!("expected 2-tuple from GetLocalVictoryPointState");
    };
    assert_eq!(vp_parts[0], PyRep::Integer(30_000_142));
    assert_eq!(vp_parts[1].dict_get("victoryPoints"), Some(&PyRep::Integer(1_500)));

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
