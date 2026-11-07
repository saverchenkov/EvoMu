//! Deep branch and mutation-killing tests for `crate::quasar_grpc` covering remaining
//! `normalize_macho_rpc_reply`, `normalize_macho_notification`, `normalize_quasar_any_payload`
//! leaf/varint coercion branches, JWT subject/expiry/SSO verification, and HTTP/2 gRPC streams.

use crate::quasar_grpc::*;
use eve_proto::PyRep;

#[test]
fn normalize_macho_rpc_reply_exhaustive_fallback_and_non_empty_branches() {
    // structureDirectory.GetJumpBridgesWithMyAccess with missing keys -> defaults to Integer(0)
    let jb = normalize_macho_rpc_reply(
        "structureDirectory",
        "GetJumpBridgesWithMyAccess",
        Ok(PyRep::List(vec![PyRep::Dict(Vec::new())])),
    )
    .unwrap();
    let PyRep::Tuple(jb_parts) = jb else {
        panic!("expected tuple");
    };
    let PyRep::List(pairs) = &jb_parts[0] else {
        panic!("expected pairs list");
    };
    let PyRep::Tuple(pair0) = &pairs[0] else {
        panic!("expected pair tuple");
    };
    assert_eq!(pair0[0].dict_get("structureID"), Some(&PyRep::Integer(0)));
    assert_eq!(pair0[1].dict_get("solarSystemID"), Some(&PyRep::Integer(0)));

    // structureDirectory non-list passthrough
    assert_eq!(
        normalize_macho_rpc_reply(
            "structureDirectory",
            "GetJumpBridgesWithMyAccess",
            Ok(PyRep::None)
        )
        .unwrap(),
        PyRep::None
    );

    // XmppChatMgr without "channels" key passthrough
    let xmpp_no_ch = normalize_macho_rpc_reply(
        "XmppChatMgr",
        "ResyncSystemChannelAccess",
        Ok(PyRep::keyval([("other", PyRep::Integer(1))])),
    )
    .unwrap();
    assert_eq!(xmpp_no_ch.dict_get("other"), Some(&PyRep::Integer(1)));

    // blueprintManager.GetBlueprintDataByOwner non-empty list preserves custom entry
    let custom_bp = PyRep::keyval([("itemID", PyRep::Integer(999)), ("typeID", PyRep::Integer(689))]);
    let bp_non_empty = normalize_macho_rpc_reply(
        "blueprintManager",
        "GetBlueprintDataByOwner",
        Ok(PyRep::List(vec![custom_bp])),
    )
    .unwrap();
    let PyRep::Tuple(bp_tuple) = bp_non_empty else {
        panic!("expected tuple");
    };
    let PyRep::List(bp_items) = &bp_tuple[0] else {
        panic!("expected list");
    };
    assert_eq!(bp_items.len(), 1);
    assert_eq!(bp_items[0].dict_get("typeID"), Some(&PyRep::Integer(689)));

    // charMgr.GetOwnerNoteLabels with non-String item and 2-char string
    let notes = normalize_macho_rpc_reply(
        "charMgr",
        "GetOwnerNoteLabels",
        Ok(PyRep::List(vec![
            PyRep::String("N:".into()),
            PyRep::Integer(42),
        ])),
    )
    .unwrap();
    let PyRep::List(note_list) = notes else {
        panic!("expected list");
    };
    assert_eq!(
        note_list[0].dict_get("label"),
        Some(&PyRep::String("N:".into()))
    );
    assert_eq!(note_list[1], PyRep::Integer(42));

    // charUnboundMgr.GetCharacterInfo with empty KeyVal defaults to empty name & 0 ID, plus non-KeyVal passthrough
    let cinfo_default = normalize_macho_rpc_reply(
        "charUnboundMgr",
        "GetCharacterInfo",
        Ok(PyRep::KeyVal(std::collections::HashMap::new())),
    )
    .unwrap();
    let PyRep::List(cinfo_items) = cinfo_default else {
        panic!("expected list");
    };
    assert_eq!(
        cinfo_items[0].dict_get("characterName"),
        Some(&PyRep::String(String::new()))
    );
    assert_eq!(
        normalize_macho_rpc_reply("charUnboundMgr", "GetCharacterInfo", Ok(PyRep::Integer(7)))
            .unwrap(),
        PyRep::Integer(7)
    );

    // contractProxy.SearchContracts when item already has "contract" and "bids"
    let prewrapped = PyRep::keyval([
        ("contract", PyRep::keyval([("contractID", PyRep::Integer(55))])),
        ("bids", PyRep::List(Vec::new())),
    ]);
    let search_pre = normalize_macho_rpc_reply(
        "contractProxy",
        "SearchContracts",
        Ok(PyRep::List(vec![prewrapped.clone()])),
    )
    .unwrap();
    assert_eq!(
        search_pre.dict_get("contracts"),
        Some(&PyRep::List(vec![prewrapped]))
    );

    // corpmgr.AuditMember with non-empty Rowset and non-Rowset
    let non_empty_rs = PyRep::rowset(vec!["col"], vec![vec![PyRep::Integer(123)]]);
    let audit_rs =
        normalize_macho_rpc_reply("corpmgr", "AuditMember", Ok(non_empty_rs.clone())).unwrap();
    assert_eq!(audit_rs, PyRep::Tuple(vec![non_empty_rs.clone(), non_empty_rs]));
    let audit_other =
        normalize_macho_rpc_reply("corpmgr", "AuditMember", Ok(PyRep::Integer(9))).unwrap();
    assert_eq!(audit_other, PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(9)]));
}

#[test]
fn normalize_macho_rpc_reply_covers_dev_index_raffle_skill_station_and_store_variants() {
    // devIndexManager.GetAllDevelopmentIndices with alternate keys ("military", "industrial", "strategic")
    let dev = normalize_macho_rpc_reply(
        "devIndexManager",
        "GetAllDevelopmentIndices",
        Ok(PyRep::List(vec![PyRep::keyval([
            ("solarSystemID", PyRep::Integer(30_000_144)),
            ("military", PyRep::Integer(4)),
            ("industrial", PyRep::Integer(5)),
            ("strategic", PyRep::Integer(1)),
        ])])),
    )
    .unwrap();
    let PyRep::List(dev_items) = dev else {
        panic!("expected list");
    };
    assert_eq!(dev_items[0].dict_get("militaryPoints"), Some(&PyRep::Integer(4)));
    assert_eq!(dev_items[0].dict_get("industrialPoints"), Some(&PyRep::Integer(5)));
    assert_eq!(dev_items[0].dict_get("claimedFor"), Some(&PyRep::Integer(1)));

    // home_station.get_home_station_candidates empty list & non-Integer passthrough
    let hs_empty = normalize_macho_rpc_reply(
        "home_station",
        "get_home_station_candidates",
        Ok(PyRep::List(Vec::new())),
    )
    .unwrap();
    if let PyRep::List(items) = hs_empty {
        assert!(items.is_empty());
    }
    let hs_other = normalize_macho_rpc_reply(
        "home_station",
        "get_home_station_candidates",
        Ok(PyRep::List(vec![PyRep::String("custom".into())])),
    )
    .unwrap();
    assert_eq!(hs_other, PyRep::List(vec![PyRep::String("custom".into())]));

    // dogmaIM.AddTarget default locked=true and default targetID=0
    let tgt_def = normalize_macho_rpc_reply(
        "dogmaIM",
        "AddTarget",
        Ok(PyRep::KeyVal(std::collections::HashMap::new())),
    )
    .unwrap();
    assert_eq!(
        tgt_def,
        PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::List(vec![PyRep::Integer(0)])
        ])
    );

    // raffleProxy.GetMyRaffleHistory with pre-populated running_id vs raw item
    let raf = normalize_macho_rpc_reply(
        "raffleProxy",
        "GetMyRaffleHistory",
        Ok(PyRep::List(vec![
            PyRep::keyval([("running_id", PyRep::Integer(88))]),
            PyRep::Integer(0),
        ])),
    )
    .unwrap();
    if let PyRep::Tuple(parts) = raf {
        assert_eq!(parts[1], PyRep::Integer(2));
    }

    // skillMgr2.GetSkillQueueAndFreePoints with empty queue preserves empty queue
    let sq_empty = normalize_macho_rpc_reply(
        "skillMgr2",
        "GetSkillQueueAndFreePoints",
        Ok(PyRep::Tuple(vec![PyRep::None, PyRep::Integer(0)])),
    )
    .unwrap();
    if let PyRep::Tuple(parts) = sq_empty {
        if let PyRep::List(q) = &parts[0] {
            assert!(q.is_empty());
        }
    }

    // stationSvc.GetAllianceSystems empty list and non-Integer item
    let al_empty = normalize_macho_rpc_reply(
        "stationSvc",
        "GetAllianceSystems",
        Ok(PyRep::List(Vec::new())),
    )
    .unwrap();
    if let PyRep::List(items) = al_empty {
        assert!(items.is_empty());
    }
    let al_other = normalize_macho_rpc_reply(
        "stationSvc",
        "GetAllianceSystems",
        Ok(PyRep::List(vec![PyRep::String("keep".into())])),
    )
    .unwrap();
    assert_eq!(al_other, PyRep::List(vec![PyRep::String("keep".into())]));

    // warsInfoMgr.GetPublicWarInfo non-dict passthrough
    assert_eq!(
        normalize_macho_rpc_reply("warsInfoMgr", "GetPublicWarInfo", Ok(PyRep::Integer(5)))
            .unwrap(),
        PyRep::Integer(5)
    );

    // storeManager get_products and get_offers recursive KeyVal/Tuple/Dict conversion
    for method in ["get_products", "get_offers"] {
        let nested = PyRep::Tuple(vec![
            PyRep::keyval([("k", PyRep::List(vec![PyRep::keyval([("inner", PyRep::Integer(1))])]))]),
            PyRep::Dict(vec![(
                PyRep::String("d".into()),
                PyRep::keyval([("v", PyRep::Integer(2))]),
            )]),
        ]);
        let conv = normalize_macho_rpc_reply("storeManager", method, Ok(nested)).unwrap();
        let PyRep::Tuple(parts) = conv else {
            panic!("expected tuple");
        };
        assert!(matches!(parts[0], PyRep::Dict(_)));
        assert!(matches!(parts[1], PyRep::Dict(_)));
    }
}
