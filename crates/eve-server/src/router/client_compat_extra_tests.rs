//! Comprehensive deterministic unit tests covering all RPC/notification normalizers
//! and helper functions in `router::client_compat`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::{UpwellStructureRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[test]
fn kv_primitives_and_rowset_dict_helpers_cover_all_types() {
    let mut entries = HashMap::new();
    entries.insert("i".into(), PyRep::Integer(42));
    entries.insert("r".into(), PyRep::Real(7.75));
    entries.insert("b_true".into(), PyRep::Bool(true));
    entries.insert("b_false".into(), PyRep::Bool(false));
    entries.insert("s".into(), PyRep::String("hello".into()));
    entries.insert("other".into(), PyRep::None);

    assert_eq!(kv_int(&entries, "i", -1), 42);
    assert_eq!(kv_int(&entries, "r", -1), 7);
    assert_eq!(kv_int(&entries, "b_true", -1), 1);
    assert_eq!(kv_int(&entries, "b_false", -1), 0);
    assert_eq!(kv_int(&entries, "other", -1), -1);
    assert_eq!(kv_int(&entries, "missing", 99), 99);

    assert_eq!(kv_str(&entries, "s"), "hello");
    assert_eq!(kv_str(&entries, "i"), "");
    assert_eq!(kv_str(&entries, "missing"), "");

    assert_eq!(kv_float(&entries, "r"), 7.75);
    assert_eq!(kv_float(&entries, "i"), 42.0);
    assert_eq!(kv_float(&entries, "s"), 0.0);
    assert_eq!(kv_float(&entries, "missing"), 0.0);

    // Non-rowset passes through unchanged.
    assert_eq!(rowset_to_dict_by_key(PyRep::Integer(5), "fleetID"), PyRep::Integer(5));

    // Rowset converts to Dict keyed by key_col, falling back when row is shorter than key_idx.
    let rs = PyRep::Rowset {
        columns: vec!["fleetID".into(), "name".into()],
        rows: vec![
            vec![PyRep::Integer(101), PyRep::String("Alpha".into())],
            vec![PyRep::Integer(102), PyRep::String("Bravo".into())],
        ],
    };
    let PyRep::Dict(pairs) = rowset_to_dict_by_key(rs, "fleetID") else {
        panic!("expected Dict");
    };
    assert_eq!(pairs.len(), 2);
    assert_eq!(pairs[0].0, PyRep::Integer(101));
    assert_eq!(pairs[1].0, PyRep::Integer(102));

    let rs_missing_col = PyRep::Rowset {
        columns: vec!["colA".into()],
        rows: vec![vec![], vec![PyRep::Integer(77)]],
    };
    let PyRep::Dict(fb_pairs) = rowset_to_dict_by_key(rs_missing_col, "nonExistent") else {
        panic!("expected Dict");
    };
    assert_eq!(fb_pairs[0].0, PyRep::Integer(1));
    assert_eq!(fb_pairs[1].0, PyRep::Integer(77));
}

#[test]
fn inv_keyval_scan_result_and_anomaly_normalizers() {
    assert_eq!(inv_keyval_to_dbrow(PyRep::None), PyRep::None);

    let kv_pos = PyRep::keyval([
        ("itemID", PyRep::Integer(10)),
        ("typeID", PyRep::Integer(34)),
        ("ownerID", PyRep::Integer(90_000_001)),
        ("locationID", PyRep::Integer(60_003_760)),
        ("flagID", PyRep::Integer(4)),
        ("quantity", PyRep::Integer(100)),
        ("groupID", PyRep::Integer(18)),
        ("categoryID", PyRep::Integer(4)),
        ("customInfo", PyRep::String("note".into())),
    ]);
    let dbrow_pos = inv_keyval_to_dbrow(kv_pos);
    assert!(matches!(dbrow_pos, PyRep::DBRow { .. }));

    // Zero/negative groupID and categoryID fall back to 25 and 7; missing customInfo falls back to empty String.
    let kv_neg = PyRep::keyval([
        ("itemID", PyRep::Integer(11)),
        ("groupID", PyRep::Integer(0)),
        ("categoryID", PyRep::Integer(-1)),
    ]);
    let dbrow_neg = inv_keyval_to_dbrow(kv_neg);
    assert!(matches!(dbrow_neg, PyRep::DBRow { .. }));

    // ensure_scan_result_data
    assert_eq!(ensure_scan_result_data(PyRep::Integer(1)), PyRep::Integer(1));

    for (target_class, expected_sg) in [
        ("CosmicAnomaly", 2),
        ("Anomaly", 2),
        ("Ship", 8),
        ("Structure", 8),
        ("AbyssalTrace", 8),
        ("Drone", 16),
        ("Probe", 16),
        ("Unknown", 4),
    ] {
        let out = ensure_scan_result_data(PyRep::keyval([
            ("targetClass", PyRep::String(target_class.into())),
            ("x", PyRep::Real(1.0)),
            ("y", PyRep::Real(2.0)),
            ("z", PyRep::Real(3.0)),
        ]));
        assert_eq!(out.dict_get("scanGroupID"), Some(&PyRep::Integer(expected_sg)));
        assert_eq!(out.dict_get("groupID"), Some(&PyRep::Integer(502)));
        assert_eq!(out.dict_get("typeID"), Some(&PyRep::Integer(502)));
        assert_eq!(
            out.dict_get("data"),
            Some(&PyRep::Tuple(vec![
                PyRep::Real(1.0),
                PyRep::Real(2.0),
                PyRep::Real(3.0),
            ]))
        );
    }

    // `pos` tuple/list takes priority when `data` is None; existing `data` is preserved.
    let with_pos_list = ensure_scan_result_data(PyRep::keyval([
        ("scanGroupID", PyRep::Integer(9)),
        ("groupID", PyRep::Integer(10)),
        ("typeID", PyRep::Integer(11)),
        ("data", PyRep::None),
        (
            "pos",
            PyRep::List(vec![
                PyRep::Real(4.0),
                PyRep::Real(5.0),
                PyRep::Real(6.0),
                PyRep::Real(7.0),
            ]),
        ),
    ]));
    assert_eq!(
        with_pos_list.dict_get("data"),
        Some(&PyRep::Tuple(vec![
            PyRep::Real(4.0),
            PyRep::Real(5.0),
            PyRep::Real(6.0),
        ]))
    );
    let existing_data = ensure_scan_result_data(PyRep::keyval([
        ("scanGroupID", PyRep::Integer(9)),
        ("groupID", PyRep::Integer(10)),
        ("typeID", PyRep::Integer(11)),
        ("data", PyRep::String("keep".into())),
    ]));
    assert_eq!(existing_data.dict_get("data"), Some(&PyRep::String("keep".into())));

    // ensure_anomaly_dungeon_id
    assert_eq!(ensure_anomaly_dungeon_id(PyRep::Bool(false)), PyRep::Bool(false));
    let anom_missing = ensure_anomaly_dungeon_id(PyRep::keyval([("dungeonID", PyRep::None)]));
    assert_eq!(anom_missing.dict_get("dungeonID"), Some(&PyRep::Integer(1001)));
    let anom_present = ensure_anomaly_dungeon_id(PyRep::keyval([("dungeonID", PyRep::Integer(4242))]));
    assert_eq!(anom_present.dict_get("dungeonID"), Some(&PyRep::Integer(4242)));
}

#[test]
fn contract_reply_and_skill_queue_rowset_normalizers() {
    assert_eq!(normalize_contract_reply(PyRep::Integer(0)), PyRep::Integer(0));
    assert_eq!(normalize_skill_queue_rowset(PyRep::Integer(0)), PyRep::Integer(0));

    for (typ, exp_type) in [("ItemExchange", 1), ("Auction", 2), ("Courier", 3), ("unknown", 1)] {
        let out = normalize_contract_reply(PyRep::keyval([
            ("type", PyRep::String(typ.into())),
            ("status", PyRep::String("Outstanding".into())),
            ("availability", PyRep::String("Private".into())),
            ("dateIssued", PyRep::Integer(134_000_000_000_000_000)),
            ("dateExpired", PyRep::Integer(0)),
            ("dateAccepted", PyRep::Integer(1_700_000_100)),
            ("items", PyRep::List(vec![PyRep::keyval([("typeID", PyRep::Integer(587)), ("quantity", PyRep::Integer(0))])])),
        ]));
        let contract = out.dict_get("contract").unwrap();
        assert_eq!(contract.dict_get("type"), Some(&PyRep::Integer(exp_type)));
        assert_eq!(contract.dict_get("availability"), Some(&PyRep::Integer(1)));
        assert_eq!(contract.dict_get("dateIssued"), Some(&PyRep::Integer(134_000_000_000_000_000)));
        assert_eq!(contract.dict_get("dateExpired"), Some(&PyRep::Integer(133_500_000_000_000_000)));
        assert_eq!(
            contract.dict_get("dateAccepted"),
            Some(&PyRep::Integer((1_700_000_100 + 11_644_473_600) * 10_000_000))
        );
        let PyRep::List(items) = out.dict_get("items").unwrap() else { panic!() };
        assert_eq!(items[0].dict_get("itemTypeID"), Some(&PyRep::Integer(587)));
        assert_eq!(items[0].dict_get("quantity"), Some(&PyRep::Integer(1)));
        assert_eq!(items[0].dict_get("inCrate"), Some(&PyRep::Bool(true)));
        assert_eq!(out.dict_get("bids"), Some(&PyRep::List(Vec::new())));
    }

    for (status_str, exp_status) in [
        ("InProgress", 1),
        ("FinishedIssuer", 2),
        ("FinishedContractor", 3),
        ("Finished", 4),
        ("Cancelled", 5),
        ("Rejected", 6),
        ("Failed", 7),
        ("Deleted", 8),
        ("Reversed", 9),
        ("other", 0),
    ] {
        let out = normalize_contract_reply(PyRep::keyval([
            ("status", PyRep::String(status_str.into())),
            ("availability", PyRep::String("Public".into())),
        ]));
        let contract = out.dict_get("contract").unwrap();
        assert_eq!(contract.dict_get("status"), Some(&PyRep::Integer(exp_status)));
        assert_eq!(contract.dict_get("availability"), Some(&PyRep::Integer(0)));
    }

    // Skill queue rowset: normalized into List of KeyVal entries.
    let already = PyRep::Rowset {
        columns: vec!["typeID".into(), "trainingEndTime".into()],
        rows: vec![vec![PyRep::Integer(3300), PyRep::Integer(123)]],
    };
    assert!(matches!(normalize_skill_queue_rowset(already), PyRep::List(v) if v.len() == 1));

    let unnorm = PyRep::Rowset {
        columns: vec!["typeID".into(), "durationSeconds".into()],
        rows: vec![
            vec![PyRep::Integer(3300), PyRep::Real(120.0)],
            vec![PyRep::Integer(3301), PyRep::Real(0.2)], // clamped to >= 1.0s
        ],
    };
    let PyRep::List(rows) = normalize_skill_queue_rowset(unnorm) else {
        panic!("expected List of KeyVals");
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].dict_get("queuePosition"), Some(&PyRep::Integer(0)));
    assert_eq!(rows[1].dict_get("queuePosition"), Some(&PyRep::Integer(1)));
    assert_eq!(
        rows[0].dict_get("trainingEndTime"),
        rows[1].dict_get("trainingStartTime")
    );
    assert!(
        rows[1].dict_get("trainingEndTime").unwrap().as_i64().unwrap()
            > rows[1].dict_get("trainingStartTime").unwrap().as_i64().unwrap()
    );
}

#[test]
fn normalize_macho_rpc_reply_covers_all_service_adapters() {
    // XmppChatMgr.ResyncSystemChannelAccess
    let chans = PyRep::keyval([(
        "channels",
        PyRep::List(vec![
            PyRep::String("solarsystemid2:30000142".into()),
            PyRep::String("corpid:98000001".into()),
            PyRep::String("allianceid:99000001".into()),
            PyRep::String("fleetid:12345".into()),
            PyRep::String("constellation:20000020".into()),
        ]),
    )]);
    let xmpp_out = normalize_macho_rpc_reply("XmppChatMgr", "ResyncSystemChannelAccess", Ok(chans)).unwrap();
    assert_eq!(
        xmpp_out,
        PyRep::List(vec![
            PyRep::String("local_30000142".into()),
            PyRep::String("corp_98000001".into()),
            PyRep::String("alliance_99000001".into()),
            PyRep::String("fleet_12345".into()),
            PyRep::String("constellation_20000020".into()),
        ])
    );

    // fleetProxy.GetAvailableFleetAds
    let ads = PyRep::Rowset {
        columns: vec!["fleetID".into(), "fleetName".into()],
        rows: vec![vec![PyRep::Integer(55), PyRep::String("Roam".into())]],
    };
    let ads_out = normalize_macho_rpc_reply("fleetProxy", "GetAvailableFleetAds", Ok(ads)).unwrap();
    assert!(matches!(ads_out, PyRep::Dict(ref v) if v.len() == 1));

    // contractProxy.GetContract & GetContractInfo
    let c_out = normalize_macho_rpc_reply(
        "contractProxy",
        "GetContract",
        Ok(PyRep::keyval([("type", PyRep::String("Auction".into()))])),
    )
    .unwrap();
    assert!(c_out.dict_get("contract").is_some());

    // skillMgr2.GetSkillQueue & GetSkillQueueAndFreePoints
    let sq = PyRep::Rowset {
        columns: vec!["typeID".into(), "durationSeconds".into()],
        rows: vec![vec![PyRep::Integer(3300), PyRep::Real(10.0)]],
    };
    let sq_out = normalize_macho_rpc_reply("skillMgr2", "GetSkillQueue", Ok(sq.clone())).unwrap();
    assert!(matches!(sq_out, PyRep::List(ref v) if v.len() == 1 && v[0].dict_get("trainingEndTime").is_some()));

    let sq_free = normalize_macho_rpc_reply(
        "skillMgr2",
        "GetSkillQueueAndFreePoints",
        Ok(PyRep::Tuple(vec![sq, PyRep::Integer(5000)])),
    )
    .unwrap();
    let PyRep::Tuple(parts) = sq_free else { panic!() };
    assert_eq!(parts[1], PyRep::Integer(5000));
    assert!(matches!(&parts[0], PyRep::List(v) if v.len() == 1 && v[0].dict_get("trainingEndTime").is_some()));

    // config.GetMultiCorpTickerNamesEx
    let tickers = normalize_macho_rpc_reply(
        "config",
        "GetMultiCorpTickerNamesEx",
        Ok(PyRep::List(vec![PyRep::Integer(98000001)])),
    )
    .unwrap();
    assert!(matches!(tickers, PyRep::ObjectEx { is_type_2: true, .. }));

    // sovMgr.GetSovStructuresInfoForLocalSolarSystem & GetSovStructuresInfoForSolarSystem
    let sov_with = normalize_macho_rpc_reply(
        "sovMgr",
        "GetSovStructuresInfoForLocalSolarSystem",
        Ok(PyRep::keyval([("structures", PyRep::List(vec![PyRep::Integer(1)]))])),
    )
    .unwrap();
    assert_eq!(sov_with, PyRep::List(vec![PyRep::Integer(1)]));

    let sov_empty = normalize_macho_rpc_reply(
        "sovMgr",
        "GetSovStructuresInfoForSolarSystem",
        Ok(PyRep::keyval(Vec::<(String, PyRep)>::new())),
    )
    .unwrap();
    assert_eq!(sov_empty, PyRep::List(Vec::new()));

    // corpmgr.GetCorporations & GetPublicInfo
    let corp_row = normalize_macho_rpc_reply(
        "corpmgr",
        "GetPublicInfo",
        Ok(PyRep::keyval([("corporationID", PyRep::Integer(1000125))])),
    )
    .unwrap();
    assert!(matches!(corp_row, PyRep::Object { ref type_name, .. } if type_name == "util.Row"));

    // Error passthrough
    let err = BridgeRpcError {
        code: "Err".into(),
        message: "boom".into(),
    };
    assert!(normalize_macho_rpc_reply("XmppChatMgr", "ResyncSystemChannelAccess", Err(err)).is_err());
}

#[test]
fn destiny_and_notification_normalizers_and_suppression_branches() {
    // normalize_damage_state_tuple
    assert_eq!(normalize_damage_state_tuple(PyRep::Integer(1)), PyRep::Integer(1));
    assert_eq!(
        normalize_damage_state_tuple(PyRep::Tuple(vec![
            PyRep::Tuple(vec![PyRep::Real(0.75), PyRep::Real(60000.0), PyRep::Integer(1)]),
            PyRep::Real(1.0),
            PyRep::Real(1.0),
        ])),
        PyRep::Tuple(vec![PyRep::Real(0.75), PyRep::Real(1.0), PyRep::Real(1.0)])
    );

    // destiny_entry_func_name + normalize_destiny_payload_arg
    assert_eq!(destiny_entry_func_name(&PyRep::Integer(0)), None);
    assert_eq!(
        destiny_entry_func_name(&PyRep::Tuple(vec![
            PyRep::String("DirectFunc".into()),
            PyRep::Integer(1),
        ])),
        Some("DirectFunc")
    );

    let mk_ev = |name: &str| {
        PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Tuple(vec![PyRep::String(name.into()), PyRep::Tuple(vec![])]),
        ])
    };
    // When SetState is present, OnSlimItemChange / OnDockingAccepted / OnClientControllerEvent are filtered out.
    let norm_with_add = normalize_destiny_payload_arg(PyRep::List(vec![
        mk_ev("SetState"),
        mk_ev("OnSlimItemChange"),
        mk_ev("OnDockingAccepted"),
        mk_ev("OnClientControllerEvent"),
        mk_ev("Orbit"),
    ]));
    assert_eq!(norm_with_add, PyRep::List(vec![mk_ev("SetState"), mk_ev("Orbit")]));

    // Without SetState/AddBalls/AddBalls2, OnSlimItemChange is preserved.
    let norm_no_add = normalize_destiny_payload_arg(PyRep::Tuple(vec![PyRep::List(vec![
        mk_ev("OnSlimItemChange"),
        mk_ev("OnDockingAccepted"),
    ])]));
    assert_eq!(
        norm_no_add,
        PyRep::Tuple(vec![PyRep::List(vec![mk_ev("OnSlimItemChange")])])
    );

    // should_suppress_macho_notification
    assert!(should_suppress_macho_notification("OnDamageStateChange", &[]));
    assert!(should_suppress_macho_notification("OnSessionChanged", &[]));
    assert!(should_suppress_macho_notification(
        "OnTradeInitiate",
        &[PyRep::Integer(1), PyRep::Integer(2)]
    ));
    assert!(!should_suppress_macho_notification(
        "OnTradeInitiate",
        &[PyRep::Integer(1), PyRep::String("ok".into())]
    ));
    assert!(should_suppress_macho_notification(
        "OnProgressionRoomJoined",
        &[PyRep::Integer(1), PyRep::Integer(1_000_005)]
    ));
    assert!(!should_suppress_macho_notification(
        "OnProgressionRoomJoined",
        &[PyRep::Integer(1), PyRep::Integer(5)]
    ));
    assert!(should_suppress_macho_notification(
        "DoDestinyUpdate",
        &[PyRep::List(vec![mk_ev("OnDockingAccepted")])]
    ));
    assert!(!should_suppress_macho_notification(
        "DoDestinyUpdate",
        &[PyRep::List(vec![mk_ev("Orbit")])]
    ));
    assert!(!should_suppress_macho_notification("OnAccountChange", &[]));

    // normalize_macho_notification arms
    assert_eq!(normalize_macho_notification("OnContractCreated", vec![PyRep::Integer(1)]), Vec::<PyRep>::new());
    assert_eq!(
        normalize_macho_notification(
            "OnWarpFinished",
            vec![PyRep::Integer(10), PyRep::Integer(20), PyRep::Integer(30)]
        ),
        vec![PyRep::Integer(10), PyRep::Integer(0)]
    );
    assert_eq!(
        normalize_macho_notification("OnWarpFinished", vec![PyRep::Integer(10)]),
        vec![PyRep::Integer(10)]
    );
    assert_eq!(
        normalize_macho_notification(
            "OnTradeOffer",
            vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(3)]
        ),
        vec![PyRep::Integer(1), PyRep::Integer(2)]
    );
    assert_eq!(
        normalize_macho_notification(
            "OnServerSkillsChanged",
            vec![PyRep::Integer(1), PyRep::String("raw_reason".into())]
        ),
        vec![PyRep::Integer(1), PyRep::None]
    );
    assert_eq!(
        normalize_macho_notification(
            "OnServerSkillsChanged",
            vec![PyRep::Integer(1), PyRep::String("OnSkillTrained".into())]
        ),
        vec![PyRep::Integer(1), PyRep::String("OnSkillTrained".into())]
    );
    let sq_notif = normalize_macho_notification(
        "OnNewSkillQueueSaved",
        vec![PyRep::Rowset {
            columns: vec!["typeID".into(), "durationSeconds".into()],
            rows: vec![vec![PyRep::Integer(3300), PyRep::Real(5.0)]],
        }],
    );
    assert!(matches!(&sq_notif[0], PyRep::List(v) if v.len() == 1 && v[0].dict_get("trainingEndTime").is_some()));

    let dmg_notif = normalize_macho_notification(
        "OnDamageStateChange",
        vec![
            PyRep::Integer(999),
            PyRep::Tuple(vec![
                PyRep::Tuple(vec![PyRep::Real(0.5)]),
                PyRep::Real(0.9),
                PyRep::Real(1.0),
            ]),
        ],
    );
    assert_eq!(
        dmg_notif[1],
        PyRep::Tuple(vec![PyRep::Real(0.5), PyRep::Real(0.9), PyRep::Real(1.0)])
    );

    let slim_notif = normalize_macho_notification(
        "OnSlimItemChange",
        vec![PyRep::Integer(100), PyRep::keyval([("itemID", PyRep::Integer(100))])],
    );
    assert_eq!(slim_notif[0], slim_notif[1]);

    // DoDestinyUpdates
    let ddu = normalize_macho_notification(
        "DoDestinyUpdates",
        vec![
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::List(vec![mk_ev("Orbit")]), PyRep::Bool(true)]),
                mk_ev("Stop"),
            ]),
            PyRep::Integer(7),
        ],
    );
    assert_eq!(ddu.len(), 2);
    assert_eq!(ddu[1], PyRep::Integer(7));

    // OnSpecialFX
    let sfx_list = normalize_macho_notification("OnSpecialFX", vec![PyRep::List(vec![PyRep::Integer(1)])]);
    assert_eq!(sfx_list.len(), 14);
    assert_eq!(sfx_list[0], PyRep::Integer(1));
    assert_eq!(sfx_list[5], PyRep::String("effects.Laser".into()));
    assert_eq!(sfx_list[6], PyRep::Bool(false));
    assert!(matches!(sfx_list[11], PyRep::Integer(ft) if ft > 0));
    let sfx_flat = normalize_macho_notification("OnSpecialFX", vec![PyRep::Integer(1), PyRep::String("fx".into())]);
    assert_eq!(sfx_flat.len(), 14);
    assert_eq!(sfx_flat[0], PyRep::Integer(1));
    assert_eq!(sfx_flat[1], PyRep::String("fx".into()));
    assert_eq!(sfx_flat[5], PyRep::String("effects.Laser".into()));
    assert!(matches!(sfx_flat[11], PyRep::Integer(ft) if ft > 0));

    // OnGodmaShipEffect empty + 10-arg with short env tuple
    assert!(normalize_macho_notification("OnGodmaShipEffect", vec![]).is_empty());
    let godma_short_false = normalize_macho_notification(
        "OnGodmaShipEffect",
        vec![PyRep::Integer(10), PyRep::Integer(20), PyRep::Bool(false)],
    );
    assert_eq!(godma_short_false[3], PyRep::Integer(0));

    let godma_10_short_env = normalize_macho_notification(
        "OnGodmaShipEffect",
        vec![
            PyRep::Integer(10),
            PyRep::Integer(20),
            PyRep::Integer(0),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Tuple(vec![PyRep::Integer(10)]), // len < 7, != 3
            PyRep::Integer(0),
            PyRep::Real(1.0),
            PyRep::Integer(0),
            PyRep::None,
        ],
    );
    assert_eq!(godma_10_short_env.len(), 11);
    assert!(matches!(&godma_10_short_env[5], PyRep::Tuple(t) if t.len() == 7 && t[6] == PyRep::Integer(20)));

    // OnGodmaPrimeItem
    let prime_out = normalize_macho_notification(
        "OnGodmaPrimeItem",
        vec![PyRep::Integer(1), PyRep::keyval([("time", PyRep::Integer(777))])],
    );
    assert_eq!(prime_out[1].dict_get("wallclockTime"), Some(&PyRep::Integer(777)));

    // OnItemChange
    let item_chg = normalize_macho_notification(
        "OnItemChange",
        vec![PyRep::keyval([("itemID", PyRep::Integer(5))]), PyRep::Dict(vec![])],
    );
    assert_eq!(item_chg.len(), 3);
    assert_eq!(item_chg[2], PyRep::None);

    // OnSystemScanStarted
    let scan_start = normalize_macho_notification(
        "OnSystemScanStarted",
        vec![
            PyRep::Integer(1),
            PyRep::Integer(2),
            PyRep::List(vec![PyRep::Integer(5001)]),
        ],
    );
    let PyRep::Dict(probes_dict) = &scan_start[2] else { panic!() };
    assert_eq!(probes_dict[0].0, PyRep::Integer(5001));
    assert_eq!(probes_dict[0].1.dict_get("state"), Some(&PyRep::Integer(4)));

    // OnSystemScanStopped
    let scan_stop = normalize_macho_notification(
        "OnSystemScanStopped",
        vec![
            PyRep::Integer(1),
            PyRep::List(vec![PyRep::keyval([("targetClass", PyRep::String("Ship".into()))])]),
        ],
    );
    let PyRep::List(scan_rows) = &scan_stop[1] else { panic!() };
    assert_eq!(scan_rows[0].dict_get("scanGroupID"), Some(&PyRep::Integer(8)));

    // OnSignalTrackerAnomalyUpdate
    let anom_upd = normalize_macho_notification(
        "OnSignalTrackerAnomalyUpdate",
        vec![
            PyRep::Integer(30_000_142),
            PyRep::Dict(vec![(
                PyRep::String("A-1".into()),
                PyRep::keyval(Vec::<(String, PyRep)>::new()),
            )]),
        ],
    );
    let PyRep::Dict(anom_dict) = &anom_upd[1] else { panic!() };
    assert_eq!(anom_dict[0].1.dict_get("dungeonID"), Some(&PyRep::Integer(1001)));

    // OnModuleAttributeChanges
    let mod_attrs = normalize_macho_notification(
        "OnModuleAttributeChanges",
        vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Integer(2),
            PyRep::Integer(3),
            PyRep::Integer(4),
            PyRep::Integer(5),
            PyRep::Real(6.0),
            PyRep::Real(7.0),
        ])])],
    );
    let PyRep::List(changes) = &mod_attrs[0] else { panic!() };
    assert!(matches!(&changes[0], PyRep::Tuple(elems) if elems.len() == 8 && elems[7] == PyRep::None));

    let mod_attrs_empty = normalize_macho_notification("OnModuleAttributeChanges", vec![PyRep::Integer(0)]);
    assert_eq!(
        mod_attrs_empty,
        vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::String("OnModuleAttributeChange".into()),
            PyRep::None,
            PyRep::Integer(0),
            PyRep::Integer(2),
            PyRep::Integer(0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::None,
        ])])]
    );
}

#[test]
fn dogma_station_or_structure_accessible_checks_all_sources() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let struct_id = 1_055_000_001_i64;
    let home_station_id = ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                owner_corp_id: 98_000_001,
                type_id: 35832,
                name: "Astrahus".into(),
                solar_system_id: 30_000_142,
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1000.0,
                armor_hp: 1000.0,
                hull_hp: 1000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.characters.get(&char_id).map(|c| c.home_station_id).unwrap_or(60_003_760)
    });

    let mut session = SessionState::new(77);
    session.solar_system_id = Some(30_000_142);

    // SDE station in system (Jita 4-4)
    assert!(is_dogma_station_or_structure_accessible(&ctx, &session, char_id, 60_003_760));
    // Structure in system
    assert!(is_dogma_station_or_structure_accessible(&ctx, &session, char_id, struct_id));
    // Character's home station
    assert!(is_dogma_station_or_structure_accessible(&ctx, &session, char_id, home_station_id));
    // Unrelated ID in another system or nonexistent
    assert!(!is_dogma_station_or_structure_accessible(&ctx, &session, char_id, 999_999_999));
}

#[test]
fn wave2_wave4_normalizer_and_effect_id_helper_edges() {
    let ctx = test_ctx();

    // 1. resolve_effect_id_pyrep: all named fallbacks, numeric string, SDE lookup, unknown, and non-string
    for (name, expected_id) in [
        ("online", 16),
        ("miningLaser", 67),
        ("miningClouds", 2726),
        ("mining", 17),
        ("targetAttack", 10),
        ("projectileFired", 34),
        ("useMissiles", 101),
        ("loPower", 11),
        ("hiPower", 12),
        ("medPower", 13),
        ("202", 202),
        ("unknown_custom_effect_xyz", 0),
    ] {
        assert_eq!(
            resolve_effect_id_pyrep(PyRep::String(name.into()), None),
            PyRep::Integer(expected_id),
            "effect fallback mismatch for {name}"
        );
    }
    assert_eq!(
        resolve_effect_id_pyrep(PyRep::Integer(67), Some(&ctx.sde)),
        PyRep::Integer(67)
    );
    if let Some(eff) = ctx.sde.effects.values().find(|e| !e.name.is_empty()) {
        assert_eq!(
            resolve_effect_id_pyrep(PyRep::String(eff.name.clone()), Some(&ctx.sde)),
            PyRep::Integer(eff.effect_id as i64)
        );
    }

    // 2. normalize_godma_environment: 3-tuple, <7-tuple padding (len 4), string at idx 6, and non-tuple fallback
    let item_arg = PyRep::Integer(10);
    let env3 = normalize_godma_environment(
        PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(20), PyRep::Integer(30)]),
        &item_arg,
        &PyRep::Integer(67),
        Some(&ctx.sde),
    );
    assert_eq!(
        env3,
        PyRep::Tuple(vec![
            PyRep::Integer(10),
            PyRep::None,
            PyRep::Integer(20),
            PyRep::None,
            PyRep::None,
            PyRep::List(Vec::new()),
            PyRep::Integer(30),
        ])
    );
    let env4 = normalize_godma_environment(
        PyRep::Tuple(vec![
            PyRep::Integer(10),
            PyRep::Integer(20),
            PyRep::Integer(30),
            PyRep::Integer(40),
        ]),
        &item_arg,
        &PyRep::Integer(67),
        Some(&ctx.sde),
    );
    assert_eq!(
        env4,
        PyRep::Tuple(vec![
            PyRep::Integer(10),
            PyRep::Integer(20),
            PyRep::Integer(30),
            PyRep::Integer(40),
            PyRep::None,
            PyRep::List(Vec::new()),
            PyRep::Integer(67),
        ])
    );
    let env7_str = normalize_godma_environment(
        PyRep::Tuple(vec![
            PyRep::Integer(10),
            PyRep::Integer(20),
            PyRep::Integer(30),
            PyRep::None,
            PyRep::None,
            PyRep::List(Vec::new()),
            PyRep::String("useMissiles".into()),
        ]),
        &item_arg,
        &PyRep::Integer(101),
        Some(&ctx.sde),
    );
    if let PyRep::Tuple(t) = env7_str {
        assert_eq!(t[6], PyRep::Integer(101));
    } else {
        panic!("expected tuple");
    }
    let env_non_tuple = normalize_godma_environment(PyRep::None, &item_arg, &PyRep::Integer(16), Some(&ctx.sde));
    assert_eq!(
        env_non_tuple,
        PyRep::Tuple(vec![
            PyRep::Integer(10),
            PyRep::None,
            PyRep::Integer(10),
            PyRep::None,
            PyRep::None,
            PyRep::List(Vec::new()),
            PyRep::Integer(16),
        ])
    );

    // 3. ensure_i4_pyrep clamping & fallbacks
    let ensure_i4_val = |mut r: PyRep| {
        ensure_i4_pyrep(&mut r);
        r
    };
    assert_eq!(ensure_i4_val(PyRep::Real(1e15)), PyRep::Integer(i64::from(i32::MAX)));
    assert_eq!(ensure_i4_val(PyRep::Real(-1e15)), PyRep::Integer(i64::from(i32::MIN)));
    assert_eq!(ensure_i4_val(PyRep::Integer(i64::MAX)), PyRep::Integer(i64::from(i32::MAX)));
    assert_eq!(ensure_i4_val(PyRep::Integer(i64::MIN)), PyRep::Integer(i64::from(i32::MIN)));
    assert_eq!(ensure_i4_val(PyRep::None), PyRep::None);
    assert_eq!(ensure_i4_val(PyRep::String("bad".into())), PyRep::Integer(0));

    // 4. normalize_on_special_fx: empty, 1-arg List([Tuple]), 1-arg Tuple, 1-arg scalar + padding slots 5..13
    assert!(normalize_on_special_fx(Vec::new()).is_empty());
    for raw_one in [
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(55)])]),
        PyRep::Tuple(vec![PyRep::Integer(55)]),
        PyRep::Integer(55),
    ] {
        let padded = normalize_on_special_fx(vec![raw_one]);
        assert_eq!(padded.len(), 14);
        assert_eq!(padded[0], PyRep::Integer(55));
        assert_eq!(padded[4], PyRep::None);
        assert_eq!(padded[5], PyRep::String("effects.Laser".into()));
        assert_eq!(padded[6], PyRep::Bool(false));
        assert_eq!(padded[7], PyRep::Integer(1));
        assert_eq!(padded[8], PyRep::Integer(1));
        assert_eq!(padded[9], PyRep::Real(-1.0));
        assert_eq!(padded[10], PyRep::Integer(0));
        assert!(padded[11].as_i64().is_some_and(|v| v > i32::MAX as i64));
        assert_eq!(padded[12], PyRep::Real(0.0));
        assert_eq!(padded[13], PyRep::None);
    }

    // 5. normalize_destiny_update_arg: OnDockingAccepted 3-tuple -> 1-tuple (station_id,)
    let dock_ev = normalize_destiny_update_arg(PyRep::List(vec![
        PyRep::Tuple(vec![
            PyRep::String("OnDockingAccepted".into()),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(60_003_760)]),
        ]),
        PyRep::Tuple(vec![
            PyRep::String("OnDockingAccepted".into()),
            PyRep::Tuple(vec![PyRep::Integer(60_003_760)]),
        ]),
    ]));
    assert_eq!(
        dock_ev,
        PyRep::List(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnDockingAccepted".into()),
                PyRep::Tuple(vec![PyRep::Integer(60_003_760)]),
            ]),
            PyRep::Tuple(vec![
                PyRep::String("OnDockingAccepted".into()),
                PyRep::Tuple(vec![PyRep::Integer(60_003_760)]),
            ]),
        ])
    );

    // 6. normalize_agent_do_action_rep: passthroughs, state/missionState branches, integer dialogue entry
    let tuple2 = PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]);
    assert_eq!(normalize_agent_do_action_rep(tuple2.clone()), tuple2);
    assert_eq!(normalize_agent_do_action_rep(PyRep::Integer(7)), PyRep::Integer(7));

    for (state_val, ms_val, exp_substr, exp_comp, exp_decl, exp_quit) in [
        ("Completed", 0, "Well done!", true, false, false),
        ("", 4, "Well done!", true, false, false),
        ("Accepted", 0, "now active in your journal", false, false, false),
        ("", 2, "now active in your journal", false, false, false),
        ("Declined", 0, "another assignment", false, true, false),
        ("", 5, "another assignment", false, true, false),
        ("Quit", 0, "another assignment", false, false, true),
        ("", 6, "another assignment", false, false, true),
        ("Offered", 1, "let us proceed", false, false, false),
    ] {
        let rep = normalize_agent_do_action_rep(PyRep::keyval([
            ("state", PyRep::String(state_val.into())),
            ("missionState", PyRep::Integer(ms_val)),
            ("missionID", PyRep::Integer(444)),
            ("lpReward", PyRep::Integer(250)),
            (
                "dialogue",
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                    PyRep::Integer(3),
                    PyRep::String("ignored".into()),
                ]),
            ),
        ]));
        let PyRep::Tuple(outer) = rep else { panic!("expected 2-tuple") };
        let PyRep::Tuple(left) = &outer[0] else { panic!("expected inner 2-tuple") };
        let PyRep::Tuple(says) = &left[0] else { panic!("expected says tuple") };
        assert!(says[0].as_str().unwrap_or("").contains(exp_substr));
        assert_eq!(says[1], PyRep::Integer(444));
        assert_eq!(
            left[1],
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]),
                PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(3)]),
            ])
        );
        assert_eq!(outer[1].dict_get("missionCompleted"), Some(&PyRep::Bool(exp_comp)));
        assert_eq!(outer[1].dict_get("missionDeclined"), Some(&PyRep::Bool(exp_decl)));
        assert_eq!(outer[1].dict_get("missionQuit"), Some(&PyRep::Bool(exp_quit)));
        assert_eq!(outer[1].dict_get("loyaltyPoints"), Some(&PyRep::Integer(250)));
    }

    // 7. normalize_agent_mission_objective_rep: transport objective hasCargo & dungeons branches
    assert_eq!(normalize_agent_mission_objective_rep(PyRep::Integer(5)), PyRep::Integer(5));
    for (comp_status, m_state, exp_has_cargo) in [(0, 1, false), (0, 2, true), (1, 1, true)] {
        let obj_rep = normalize_agent_mission_objective_rep(PyRep::keyval([
            ("destinationStationID", PyRep::Integer(60_003_761)),
            ("requiredTypeID", PyRep::Integer(35)),
            ("requiredQuantity", PyRep::Integer(50)),
            ("completionStatus", PyRep::Integer(comp_status)),
            ("missionState", PyRep::Integer(m_state)),
        ]));
        assert_eq!(obj_rep.dict_get("dungeons"), Some(&PyRep::List(Vec::new())));
        let Some(PyRep::List(objs)) = obj_rep.dict_get("objectives") else {
            panic!("expected objectives list");
        };
        assert_eq!(objs.len(), 2);
        let PyRep::Tuple(trans) = &objs[1] else { panic!() };
        assert_eq!(trans[0], PyRep::String("transport".into()));
        let PyRep::Tuple(trans_args) = &trans[1] else { panic!() };
        assert_eq!(trans_args[4].dict_get("hasCargo"), Some(&PyRep::Bool(exp_has_cargo)));
        assert_eq!(trans_args[4].dict_get("typeID"), Some(&PyRep::Integer(35)));
        assert_eq!(trans_args[4].dict_get("quantity"), Some(&PyRep::Integer(50)));
    }

    // 8. build_v24_reprocessing_quotes_tuple: stationBaseYield override, duplicate typeID deduplication, outputs -> CRowset
    let quotes = build_v24_reprocessing_quotes_tuple(
        PyRep::Real(0.03),
        PyRep::Real(0.55),
        vec![
            PyRep::keyval([
                ("itemID", PyRep::Integer(1001)),
                ("typeID", PyRep::Integer(1228)),
                ("quantityToProcess", PyRep::Integer(200)),
                ("stationBaseYield", PyRep::Real(0.72)),
                (
                    "outputs",
                    PyRep::List(vec![PyRep::keyval([
                        ("typeID", PyRep::Integer(34)),
                        ("quantity", PyRep::Integer(400)),
                    ])]),
                ),
            ]),
            PyRep::keyval([
                ("itemID", PyRep::Integer(1002)),
                ("typeID", PyRep::Integer(1228)),
                ("quantityToProcess", PyRep::Integer(100)),
                ("stationBaseYield", PyRep::Real(0.72)),
            ]),
        ],
    );
    let PyRep::Tuple(q_parts) = quotes else { panic!("expected 3-tuple") };
    assert_eq!(q_parts.len(), 3);
    assert_eq!(q_parts[0], PyRep::Real(0.03));
    let PyRep::Dict(eff_map) = &q_parts[1] else { panic!("expected eff dict") };
    assert_eq!(eff_map.len(), 3, "None + 25 + deduplicated typeID 1228");
    let PyRep::Dict(q_dict) = &q_parts[2] else { panic!("expected quotes dict") };
    assert_eq!(q_dict.len(), 2);
}

#[test]
fn w4_mutant_killers_untouched_services_helpers_and_defaults() {
    let ctx = test_ctx();

    // 1. inv_keyval_to_dbrow_with_sde: SDE group/category lookup, flag fallback, singleton!=0 int -> -1, non-singleton stacksize, customInfo passthrough
    let dbrow_sde = inv_keyval_to_dbrow_with_sde(
        PyRep::keyval([
            ("itemID", PyRep::Integer(7001)),
            ("typeID", PyRep::Integer(587)), // Rifter (group 25, category 6)
            ("ownerID", PyRep::Integer(90_000_001)),
            ("locationID", PyRep::Integer(60_003_760)),
            ("flag", PyRep::Integer(4)),
            ("singleton", PyRep::Integer(2)),
            ("stacksize", PyRep::Integer(3)),
            ("customInfo", PyRep::String("rifter".into())),
        ]),
        Some(&ctx.sde),
    );
    assert_eq!(row_field(&dbrow_sde, "itemID"), Some(PyRep::Integer(7001)));
    assert_eq!(row_field(&dbrow_sde, "flagID"), Some(PyRep::Integer(4)));
    assert_eq!(row_field(&dbrow_sde, "quantity"), Some(PyRep::Integer(-1)));
    assert_eq!(
        row_field(&dbrow_sde, "groupID"),
        Some(PyRep::Integer(i64::from(ctx.sde.types[&587].group_id)))
    );
    assert_eq!(
        row_field(&dbrow_sde, "categoryID"),
        Some(PyRep::Integer(i64::from(ctx.sde.types[&587].category_id)))
    );
    assert_eq!(
        row_field(&dbrow_sde, "customInfo"),
        Some(PyRep::String("rifter".into()))
    );

    let dbrow_stack = inv_keyval_to_dbrow_with_sde(
        PyRep::keyval([
            ("itemID", PyRep::Integer(7002)),
            ("typeID", PyRep::Integer(34)),
            ("flagID", PyRep::Integer(5)),
            ("singleton", PyRep::Integer(0)),
            ("stacksize", PyRep::Integer(250)),
        ]),
        None,
    );
    assert_eq!(row_field(&dbrow_stack, "flagID"), Some(PyRep::Integer(5)));
    assert_eq!(row_field(&dbrow_stack, "quantity"), Some(PyRep::Integer(250)));
    assert_eq!(row_field(&dbrow_stack, "groupID"), Some(PyRep::Integer(0)));
    assert_eq!(row_field(&dbrow_stack, "categoryID"), Some(PyRep::Integer(0)));
    assert_eq!(inv_keyval_to_dbrow(PyRep::Integer(99)), PyRep::Integer(99));

    // 2. extract_table_rows on ObjectEx and non-collection fallback
    let objex = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::None),
        list: vec![PyRep::Integer(11), PyRep::Integer(22)],
        dict: Vec::new(),
    };
    assert_eq!(extract_table_rows(&objex), vec![PyRep::Integer(11), PyRep::Integer(22)]);
    assert!(extract_table_rows(&PyRep::Integer(5)).is_empty());

    // 3. contract default fallbacks (startSolarSystemID=0, itemTypeID=0, non-zero negative dateExpired)
    let c_def = normalize_contract_reply(PyRep::keyval([(
        "items",
        PyRep::List(vec![PyRep::keyval(Vec::<(String, PyRep)>::new())]),
    )]));
    let c_inner = c_def.dict_get("contract").unwrap();
    assert_eq!(c_inner.dict_get("startSolarSystemID"), Some(&PyRep::Integer(0)));
    assert_eq!(c_inner.dict_get("endSolarSystemID"), Some(&PyRep::Integer(0)));
    assert_eq!(c_inner.dict_get("startRegionID"), Some(&PyRep::Integer(0)));
    assert_eq!(c_inner.dict_get("endRegionID"), Some(&PyRep::Integer(0)));
    let PyRep::List(c_items) = c_def.dict_get("items").unwrap() else { panic!() };
    assert_eq!(c_items[0].dict_get("itemTypeID"), Some(&PyRep::Integer(0)));

    // 4. Untouched service/method pairs: fleetProxy.GetFleetAdsForChar, facilityManager.GetFacilitiesByID/GetFacility,
    //    map.GetIncursionGlobalReport, paperDollServer.GetPaperDollData, storeManager.get_categories/get_offers,
    //    agentMgr.GetOffer / GetAgentLocationWrap / GetMissionBriefingInfo / GetMissionObjectiveInfo
    let fleet_ads = normalize_macho_rpc_reply(
        "fleetProxy",
        "GetFleetAdsForChar",
        Ok(PyRep::Rowset {
            columns: vec!["fleetID".into(), "fleetName".into()],
            rows: vec![vec![PyRep::Integer(88), PyRep::String("Alpha".into())]],
        }),
    )
    .unwrap();
    assert!(matches!(fleet_ads, PyRep::Dict(ref d) if d.len() == 1));

    let fac_list = normalize_macho_rpc_reply(
        "facilityManager",
        "GetFacilitiesByID",
        Ok(PyRep::List(vec![
            PyRep::keyval([
                ("facilityID", PyRep::Integer(60_003_760)),
                ("activities", PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(9)])),
            ]),
            PyRep::Integer(123),
        ])),
    )
    .unwrap();
    let PyRep::List(facs) = fac_list else { panic!() };
    assert_eq!(facs[0].dict_get("sccTaxModifier"), Some(&PyRep::Real(1.0)));
    assert!(matches!(facs[0].dict_get("activities"), Some(PyRep::Dict(d)) if d.len() == 2));
    assert!(facs[0].dict_get("serviceAccess").is_some());
    assert!(facs[0].dict_get("rigModifiers").is_some());
    assert!(facs[0].dict_get("globalModifiers").is_some());
    assert_eq!(facs[1], PyRep::Integer(123));

    let single_fac = normalize_macho_rpc_reply(
        "facilityManager",
        "GetFacility",
        Ok(PyRep::keyval([("facilityID", PyRep::Integer(1))])),
    )
    .unwrap();
    assert_eq!(single_fac.dict_get("sccTaxModifier"), Some(&PyRep::Real(1.0)));

    let incs = normalize_macho_rpc_reply(
        "map",
        "GetIncursionGlobalReport",
        Ok(PyRep::List(vec![
            PyRep::keyval([("state", PyRep::String("Withdrawing".into())), ("hasBoss", PyRep::Bool(false))]),
            PyRep::keyval([("state", PyRep::String("Mobilizing".into())), ("hasBoss", PyRep::Bool(true))]),
            PyRep::keyval([("state", PyRep::String("Established".into())), ("factionID", PyRep::Integer(500_020))]),
        ])),
    )
    .unwrap();
    let PyRep::List(inc_rows) = incs else { panic!() };
    assert_eq!(inc_rows[0].dict_get("state"), Some(&PyRep::Integer(0)));
    assert_eq!(inc_rows[0].dict_get("aggressorFactionID"), Some(&PyRep::Integer(500_019)));
    assert_eq!(inc_rows[0].dict_get("hasFinalEncounter"), Some(&PyRep::Bool(false)));
    assert_eq!(inc_rows[1].dict_get("state"), Some(&PyRep::Integer(1)));
    assert_eq!(inc_rows[1].dict_get("hasFinalEncounter"), Some(&PyRep::Bool(true)));
    assert_eq!(inc_rows[2].dict_get("state"), Some(&PyRep::Integer(2)));
    assert_eq!(inc_rows[2].dict_get("aggressorFactionID"), Some(&PyRep::Integer(500_020)));

    assert_eq!(
        normalize_macho_rpc_reply(
            "paperDollServer",
            "GetPaperDollData",
            Ok(PyRep::keyval([("gender", PyRep::Integer(0))])),
        )
        .unwrap(),
        PyRep::List(vec![PyRep::String("female".into())])
    );
    assert_eq!(
        normalize_macho_rpc_reply(
            "paperDollServer",
            "GetPaperDollData",
            Ok(PyRep::keyval([("gender", PyRep::Integer(1))])),
        )
        .unwrap(),
        PyRep::List(vec![PyRep::String("male".into())])
    );

    let offer = normalize_macho_rpc_reply(
        "agentMgr",
        "GetOffer",
        Ok(PyRep::keyval([("state", PyRep::String("Offered".into())), ("missionID", PyRep::Integer(9))])),
    )
    .unwrap();
    assert!(matches!(offer, PyRep::Tuple(t) if t.len() == 2));

    let loc_sys = normalize_macho_rpc_reply(
        "agentMgr",
        "GetAgentLocationWrap",
        Ok(PyRep::keyval([("agentID", PyRep::Integer(10)), ("solarsystemID", PyRep::Integer(30_000_142))])),
    )
    .unwrap();
    assert_eq!(loc_sys.dict_get("locationID"), Some(&PyRep::Integer(30_000_142)));
    assert_eq!(loc_sys.dict_get("typeID"), Some(&PyRep::Integer(5)));
    let loc_st = normalize_macho_rpc_reply(
        "agentMgr",
        "GetAgentLocationWrap",
        Ok(PyRep::keyval([("agentID", PyRep::Integer(10)), ("stationID", PyRep::Integer(60_003_760))])),
    )
    .unwrap();
    assert_eq!(loc_st.dict_get("locationID"), Some(&PyRep::Integer(60_003_760)));
    assert_eq!(normalize_agent_location_wrap_rep(PyRep::Integer(1)), PyRep::Integer(1));

    assert_eq!(
        normalize_macho_rpc_reply(
            "agentMgr",
            "GetMissionBriefingInfo",
            Err(BridgeRpcError { code: "MissionNotActive".into(), message: "x".into() }),
        )
        .unwrap(),
        PyRep::None
    );
    let brief = normalize_macho_rpc_reply(
        "agentMgr",
        "GetMissionBriefingInfo",
        Ok(PyRep::keyval([("title", PyRep::String("Scan".into())), ("missionID", PyRep::Integer(12))])),
    )
    .unwrap();
    assert_eq!(brief.dict_get("Mission Title ID"), Some(&PyRep::String("Scan".into())));
    assert_eq!(brief.dict_get("ContentID"), Some(&PyRep::Integer(12)));
    assert_eq!(brief.dict_get("Decline Time"), Some(&PyRep::Integer(-1)));

    assert_eq!(
        normalize_macho_rpc_reply(
            "agentMgr",
            "GetMissionObjectiveInfo",
            Err(BridgeRpcError { code: "MissionNotActive".into(), message: "x".into() }),
        )
        .unwrap(),
        PyRep::None
    );
    let obj_dungeon = normalize_macho_rpc_reply(
        "agentMgr",
        "GetMissionObjectiveInfo",
        Ok(PyRep::keyval([
            ("missionState", PyRep::Integer(4)),
            ("iskReward", PyRep::Integer(50_000)),
            ("bonusReward", PyRep::Integer(10_000)),
            ("missionID", PyRep::Integer(77)),
        ])),
    )
    .unwrap();
    assert_eq!(obj_dungeon.dict_get("completionStatus"), Some(&PyRep::Integer(1)));
    assert!(matches!(obj_dungeon.dict_get("dungeons"), Some(PyRep::List(d)) if d.len() == 1));
    assert!(matches!(obj_dungeon.dict_get("normalRewards"), Some(PyRep::List(r)) if r.len() == 1));
    assert!(matches!(obj_dungeon.dict_get("bonusRewards"), Some(PyRep::List(r)) if r.len() == 1));

    for method in ["get_categories", "get_products", "get_offers"] {
        let converted = normalize_macho_rpc_reply(
            "storeManager",
            method,
            Ok(PyRep::List(vec![PyRep::keyval([("id", PyRep::Integer(3))])])),
        )
        .unwrap();
        assert!(matches!(converted, PyRep::List(ref v) if matches!(&v[0], PyRep::Dict(_))));
    }
}

#[test]
fn w4_mutant_killers_all_remaining_notifications() {
    let ctx = test_ctx();

    assert_eq!(
        normalize_macho_notification("OnChannelJoined", vec![PyRep::Integer(9), PyRep::String("x".into())]),
        vec![PyRep::Integer(9)]
    );
    assert!(normalize_macho_notification("OnChannelJoined", Vec::new()).is_empty());

    let bm = normalize_macho_notification(
        "OnSharedBookmarksFolderUpdated",
        vec![PyRep::List(vec![
            PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(2),
                PyRep::List(vec![PyRep::keyval([("bookmarkID", PyRep::Integer(10))]), PyRep::Integer(20)]),
            ]),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(3), PyRep::None]),
        ])],
    );
    let PyRep::List(bm_entries) = &bm[0] else { panic!() };
    let PyRep::Tuple(bm0) = &bm_entries[0] else { panic!() };
    let PyRep::Tuple(bm0_p2) = &bm0[2] else { panic!() };
    let PyRep::List(bm0_list) = &bm0_p2[0] else { panic!() };
    assert_eq!(bm0_list[0].dict_get("bookmarkID"), Some(&PyRep::Integer(10)));
    assert_eq!(bm0_list[1].dict_get("bookmarkID"), Some(&PyRep::Integer(20)));
    let PyRep::Tuple(bm1) = &bm_entries[1] else { panic!() };
    assert!(matches!(&bm1[2], PyRep::Tuple(t) if t.len() == 2));

    let bean_list = normalize_macho_notification(
        "OnBeanPrime",
        vec![PyRep::List(vec![
            PyRep::Integer(7),
            PyRep::Dict(vec![(PyRep::String("errorID".into()), PyRep::Integer(99))]),
            PyRep::keyval([("logMode", PyRep::Integer(3))]),
            PyRep::String("skip".into()),
        ])],
    );
    let PyRep::List(beans) = &bean_list[0] else { panic!() };
    assert_eq!(beans.len(), 3);
    assert_eq!(beans[0].dict_get("errorID"), Some(&PyRep::Integer(7)));
    assert_eq!(beans[1].dict_get("errorID"), Some(&PyRep::Integer(99)));
    assert_eq!(beans[2].dict_get("logMode"), Some(&PyRep::Integer(3)));
    let bean_multi = normalize_macho_notification("OnBeanPrime", vec![PyRep::Integer(1), PyRep::Integer(2)]);
    assert!(matches!(&bean_multi[0], PyRep::List(v) if v.len() == 2));

    assert!(normalize_macho_notification("OnCorporationRecruitmentAdChanged", vec![PyRep::Integer(1)]).is_empty());
    assert!(normalize_macho_notification("OnOrganizationContactsUpdated", Vec::new()).is_empty());
    let org_c = normalize_macho_notification(
        "OnOrganizationContactsUpdated",
        vec![
            PyRep::Integer(98_000_001),
            PyRep::List(vec![
                PyRep::keyval([("contactID", PyRep::Integer(1)), ("standing", PyRep::Real(5.0)), ("labelMask", PyRep::Integer(2))]),
                PyRep::keyval([("contactID", PyRep::Integer(2)), ("relationshipID", PyRep::Real(-10.0))]),
            ]),
        ],
    );
    assert_eq!(
        org_c[1],
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(5.0), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Real(-10.0), PyRep::Integer(0)]),
        ])
    );

    assert_eq!(normalize_macho_notification("OnTargetClear", Vec::new()), vec![PyRep::List(Vec::new())]);
    assert_eq!(normalize_macho_notification("OnTargetClear", vec![PyRep::List(vec![PyRep::Integer(1)])]), vec![PyRep::List(vec![PyRep::Integer(1)])]);

    assert_eq!(normalize_macho_notification("OnHeatAdded", vec![PyRep::Integer(55)]), vec![PyRep::Integer(1211), PyRep::Integer(55)]);
    assert_eq!(normalize_macho_notification("OnHeatRemoved", vec![PyRep::Integer(55)]), vec![PyRep::Integer(1211), PyRep::Integer(55)]);
    assert_eq!(normalize_macho_notification("OnHeatRemoved", vec![PyRep::Integer(99), PyRep::Integer(55)]), vec![PyRep::Integer(99), PyRep::Integer(55)]);

    assert_eq!(
        normalize_macho_notification("OnRemoteMessage", vec![PyRep::String("k".into()), PyRep::Dict(Vec::new())]),
        vec![PyRep::String("k".into()), PyRep::Dict(Vec::new()), PyRep::None]
    );

    let proc_inv = normalize_macho_notification(
        "ProcessInventoryChange",
        vec![PyRep::Integer(501), PyRep::Integer(60_003_760), PyRep::Integer(4)],
    );
    assert_eq!(proc_inv.len(), 4);
    let PyRep::List(proc_items) = &proc_inv[0] else { panic!() };
    assert_eq!(proc_items[0].dict_get("itemID"), Some(&PyRep::Integer(501)));
    assert_eq!(proc_inv[2], PyRep::Bool(true));

    assert_eq!(
        normalize_macho_notification("OnNotificationReceived", Vec::new()),
        vec![PyRep::Integer(0), PyRep::Integer(0), PyRep::None, PyRep::Integer(0), PyRep::None]
    );
    assert_eq!(normalize_macho_notification("OnNotificationReceived", vec![PyRep::Integer(1)]), vec![PyRep::Integer(1)]);

    for (raw_w, exp_w) in [
        ("locked", "add"),
        ("add", "add"),
        ("added", "add"),
        ("targetedBy", "otheradd"),
        ("otheradd", "otheradd"),
        ("unlock", "lost"),
        ("unlocked", "lost"),
        ("lost", "lost"),
        ("clear", "clear"),
        ("custom", "custom"),
    ] {
        assert_eq!(
            normalize_macho_notification("OnTarget", vec![PyRep::Integer(88), PyRep::String(raw_w.into())]),
            vec![PyRep::String(exp_w.into()), PyRep::Integer(88), PyRep::None]
        );
    }
    assert_eq!(
        normalize_macho_notification("OnTarget", vec![PyRep::String("add".into()), PyRep::Integer(88)]),
        vec![PyRep::String("add".into()), PyRep::Integer(88), PyRep::None]
    );
    assert_eq!(
        normalize_macho_notification("OnTarget", vec![PyRep::Integer(88)]),
        vec![PyRep::Integer(88)]
    );

    assert_eq!(
        normalize_macho_notification("OnTradeMoneyOffer", vec![PyRep::Integer(1), PyRep::Integer(500), PyRep::Integer(999)]),
        vec![PyRep::Integer(1), PyRep::Integer(500)]
    );

    let mult = normalize_macho_notification_with_sde(
        "OnMultipleItemChange",
        vec![PyRep::List(vec![
            PyRep::Tuple(vec![
                PyRep::keyval([("itemID", PyRep::Integer(1)), ("typeID", PyRep::Integer(587))]),
                PyRep::Dict(Vec::new()),
            ]),
            PyRep::keyval([("itemID", PyRep::Integer(2)), ("typeID", PyRep::Integer(34)), ("quantity", PyRep::Integer(5))]),
        ])],
        Some(&ctx.sde),
    );
    let PyRep::List(m_rows) = &mult[0] else { panic!() };
    let PyRep::Tuple(m_pair) = &m_rows[0] else { panic!() };
    assert_eq!(
        row_field(&m_pair[0], "groupID"),
        Some(PyRep::Integer(i64::from(ctx.sde.types[&587].group_id)))
    );
    assert_eq!(row_field(&m_rows[1], "quantity"), Some(PyRep::Integer(5)));

    let mac = normalize_macho_notification(
        "OnModuleAttributeChanges",
        vec![PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(0); 8]),
            PyRep::Tuple(vec![PyRep::Integer(0); 7]),
        ])],
    );
    let PyRep::List(mac_rows) = &mac[0] else { panic!() };
    let PyRep::Tuple(t8) = &mac_rows[0] else { panic!() };
    assert_eq!(t8[3], PyRep::Integer(2));
    assert_eq!(t8[7], PyRep::Integer(0));
    let PyRep::Tuple(t7_padded) = &mac_rows[1] else { panic!() };
    assert_eq!(t7_padded[3], PyRep::Integer(2));
    assert_eq!(t7_padded[7], PyRep::None);
    let mac_bool = normalize_macho_notification(
        "OnModuleAttributeChanges",
        vec![PyRep::Tuple(vec![
            PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(4),
            PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0), PyRep::Bool(true),
        ])],
    );
    let PyRep::List(mb_rows) = &mac_bool[0] else { panic!() };
    let PyRep::Tuple(mb0) = &mb_rows[0] else { panic!() };
    assert_eq!(mb0[3], PyRep::Integer(4));
    assert_eq!(mb0[7], PyRep::None);

    assert_eq!(
        normalize_macho_notification("OnDockingAccepted", vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(60_003_760)]),
        vec![PyRep::Integer(60_003_760)]
    );
    assert_eq!(
        normalize_macho_notification("OnDockingAccepted", vec![PyRep::Integer(60_003_760)]),
        vec![PyRep::Integer(60_003_760)]
    );

    let ddu_dock = normalize_macho_notification(
        "DoDestinyUpdate",
        vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::String("OnDockingAccepted".into()),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(60_003_760)]),
        ])])],
    );
    assert_eq!(
        ddu_dock,
        vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::String("OnDockingAccepted".into()),
            PyRep::Tuple(vec![PyRep::Integer(60_003_760)]),
        ])])]
    );
}

#[test]
fn issue_935_and_969_contract_wire_compat_shapes() {
    // #935: contractProxy.CreateContract must return a sequence of contractIDs for V24 ShowCreatedContracts.
    let created = normalize_macho_rpc_reply(
        "contractProxy",
        "CreateContract",
        Ok(PyRep::keyval([
            ("contractID", PyRep::Integer(42_001)),
            ("status", PyRep::String("Outstanding".into())),
        ])),
    )
    .expect("CreateContract keyval");
    assert_eq!(created, PyRep::List(vec![PyRep::Integer(42_001)]));

    let created_scalar = normalize_macho_rpc_reply(
        "contractProxy",
        "CreateContract",
        Ok(PyRep::Integer(42_002)),
    )
    .expect("CreateContract scalar");
    assert_eq!(created_scalar, PyRep::List(vec![PyRep::Integer(42_002)]));

    // #969: contract_to_pyrep emits dateIssuedUnix/expirationUnix, string enums, None acceptorID,
    // items with typeID (no itemTypeID/inCrate), requestedItems, and bids with amountISK (no amount).
    let inner_kv = PyRep::keyval([
        ("contractID", PyRep::Integer(55_001)),
        ("type", PyRep::String("Auction".into())),
        ("status", PyRep::String("Outstanding".into())),
        ("availability", PyRep::String("Public".into())),
        ("issuerID", PyRep::Integer(90_000_001)),
        ("issuerCorpID", PyRep::Integer(98_000_001)),
        ("assigneeID", PyRep::Integer(0)),
        ("acceptorID", PyRep::None),
        ("forCorp", PyRep::Bool(false)),
        ("startStationID", PyRep::Integer(60_003_760)),
        ("endStationID", PyRep::Integer(60_003_760)),
        ("startSolarSystemID", PyRep::Integer(30_000_142)),
        ("endSolarSystemID", PyRep::Integer(0)),
        ("dateIssuedUnix", PyRep::Integer(1_794_485_538)),
        ("expirationUnix", PyRep::Integer(1_794_571_938)),
    ]);
    let raw_contract = PyRep::keyval([
        ("contractID", PyRep::Integer(55_001)),
        ("type", PyRep::String("Auction".into())),
        ("status", PyRep::String("Outstanding".into())),
        ("availability", PyRep::String("Public".into())),
        ("issuerID", PyRep::Integer(90_000_001)),
        ("issuerCorpID", PyRep::Integer(98_000_001)),
        ("assigneeID", PyRep::Integer(0)),
        ("acceptorID", PyRep::None),
        ("forCorp", PyRep::Bool(false)),
        ("startStationID", PyRep::Integer(60_003_760)),
        ("endStationID", PyRep::Integer(60_003_760)),
        ("startSolarSystemID", PyRep::Integer(30_000_142)),
        ("endSolarSystemID", PyRep::Integer(0)),
        ("dateIssuedUnix", PyRep::Integer(1_794_485_538)),
        ("expirationUnix", PyRep::Integer(1_794_571_938)),
        (
            "items",
            PyRep::List(vec![PyRep::keyval([
                ("itemID", PyRep::Integer(200_246_971)),
                ("typeID", PyRep::Integer(34)),
                ("quantity", PyRep::Integer(100)),
            ])]),
        ),
        (
            "requestedItems",
            PyRep::List(vec![PyRep::keyval([
                ("typeID", PyRep::Integer(35)),
                ("quantity", PyRep::Integer(5)),
            ])]),
        ),
        (
            "bids",
            PyRep::List(vec![PyRep::keyval([
                ("bidderID", PyRep::Integer(90_000_002)),
                ("amountISK", PyRep::Real(5_000.0)),
            ])]),
        ),
        ("contract", inner_kv),
    ]);

    let expected_issued_ft = (1_794_485_538_i64 + 11_644_473_600) * 10_000_000;
    let expected_expired_ft = (1_794_571_938_i64 + 11_644_473_600) * 10_000_000;

    for method in [
        "GetContractListForOwner",
        "GetMyCurrentContractList",
        "GetMyExpiredContractList",
        "GetMyBids",
    ] {
        let rep = normalize_macho_rpc_reply(
            "contractProxy",
            method,
            Ok(PyRep::List(vec![raw_contract.clone()])),
        )
        .unwrap_or_else(|e| panic!("{method} failed: {e:?}"));
        let Some(PyRep::List(contracts)) = rep.dict_get("contracts") else {
            panic!("{method} must return contracts list");
        };
        assert_eq!(contracts.len(), 1);
        let c = &contracts[0];
        assert_eq!(c.dict_get("dateIssued"), Some(&PyRep::Integer(expected_issued_ft)));
        assert_eq!(c.dict_get("dateExpired"), Some(&PyRep::Integer(expected_expired_ft)));
        assert_eq!(c.dict_get("type"), Some(&PyRep::Integer(2)));
        assert_eq!(c.dict_get("status"), Some(&PyRep::Integer(0)));
        assert_eq!(c.dict_get("availability"), Some(&PyRep::Integer(0)));
        assert_eq!(c.dict_get("acceptorID"), Some(&PyRep::Integer(0)));
        assert_eq!(c.dict_get("endSolarSystemID"), Some(&PyRep::Integer(30_000_142)));
        assert_eq!(c.dict_get("numBids"), Some(&PyRep::Integer(1)));

        let Some(PyRep::Dict(items_map)) = rep.dict_get("items") else {
            panic!("{method} must return items dict keyed by contractID");
        };
        let Some((_, PyRep::List(cid_items))) = items_map
            .iter()
            .find(|(k, _)| *k == PyRep::Integer(55_001))
        else {
            panic!("{method} items dict missing contractID 55001");
        };
        assert_eq!(cid_items.len(), 2);
        assert_eq!(cid_items[0].dict_get("itemTypeID"), Some(&PyRep::Integer(34)));
        assert_eq!(cid_items[0].dict_get("inCrate"), Some(&PyRep::Bool(true)));
        assert_eq!(cid_items[1].dict_get("itemTypeID"), Some(&PyRep::Integer(35)));
        assert_eq!(cid_items[1].dict_get("inCrate"), Some(&PyRep::Bool(false)));
    }

    let search = normalize_macho_rpc_reply(
        "contractProxy",
        "SearchContracts",
        Ok(PyRep::List(vec![raw_contract])),
    )
    .expect("SearchContracts");
    let Some(PyRep::List(search_contracts)) = search.dict_get("contracts") else {
        panic!("SearchContracts must return contracts list");
    };
    assert_eq!(search_contracts.len(), 1);
    let entry = &search_contracts[0];
    let inner = entry.dict_get("contract").expect("contract inner");
    assert_eq!(inner.dict_get("dateIssued"), Some(&PyRep::Integer(expected_issued_ft)));
    assert_eq!(inner.dict_get("dateExpired"), Some(&PyRep::Integer(expected_expired_ft)));
    assert_eq!(inner.dict_get("type"), Some(&PyRep::Integer(2)));
    assert_eq!(inner.dict_get("acceptorID"), Some(&PyRep::Integer(0)));
    let Some(PyRep::List(search_items)) = entry.dict_get("items") else {
        panic!("SearchContracts entry missing items list");
    };
    assert_eq!(search_items[0].dict_get("itemTypeID"), Some(&PyRep::Integer(34)));
    assert_eq!(search_items[0].dict_get("inCrate"), Some(&PyRep::Bool(true)));
    let Some(PyRep::List(search_bids)) = entry.dict_get("bids") else {
        panic!("SearchContracts entry missing bids list");
    };
    assert_eq!(search_bids[0].dict_get("amount"), Some(&PyRep::Real(5_000.0)));
    assert_eq!(entry.dict_get("numBids"), Some(&PyRep::Integer(1)));
}
