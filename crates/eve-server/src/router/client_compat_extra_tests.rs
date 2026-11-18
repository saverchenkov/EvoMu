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
        ("CosmicAnomaly", 8),
        ("Anomaly", 8),
        ("Ship", 4),
        ("Structure", 5),
        ("Drone", 6),
        ("Probe", 9),
        ("Deployable", 12),
        ("Sovereignty", 13),
        ("AbyssalTrace", 14),
        ("CosmicSignature", 3),
        ("Unknown", 3),
    ] {
        let out = ensure_scan_result_data(PyRep::keyval([
            ("targetClass", PyRep::String(target_class.into())),
            ("x", PyRep::Real(1.0)),
            ("y", PyRep::Real(2.0)),
            ("z", PyRep::Real(3.0)),
        ]));
        assert_eq!(out.dict_get("scanGroupID"), Some(&PyRep::Integer(expected_sg)));
        assert_eq!(out.dict_get("groupID"), Some(&PyRep::Integer(502)));
        assert_eq!(out.dict_get("typeID"), Some(&PyRep::None));
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
    assert_eq!(normalize_skill_queue_rowset(PyRep::Integer(0)), PyRep::Integer(0));

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

    // Error passthrough
    let err = BridgeRpcError {
        code: "Err".into(),
        message: "boom".into(),
        client_error: None,
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
    // When SetState is present, OnSlimItemChange / SetBallInteractive / PackagedAction are filtered out (#992),
    // while OnDockingAccepted, OnClientControllerEvent, and OnDroneActivityChange pass through (#1188).
    let norm_with_add = normalize_destiny_payload_arg(PyRep::List(vec![
        mk_ev("SetState"),
        mk_ev("OnSlimItemChange"),
        mk_ev("SetBallInteractive"),
        mk_ev("PackagedAction"),
        mk_ev("OnDockingAccepted"),
        mk_ev("OnClientControllerEvent"),
        mk_ev("OnDroneActivityChange"),
        mk_ev("Orbit"),
    ]));
    assert_eq!(
        norm_with_add,
        PyRep::List(vec![
            mk_ev("SetState"),
            mk_ev("OnDockingAccepted"),
            mk_ev("OnClientControllerEvent"),
            mk_ev("OnDroneActivityChange"),
            mk_ev("Orbit"),
        ])
    );

    // Without SetState (including alongside AddBalls2), SetBallInteractive and PackagedAction are preserved,
    // while OnSlimItemChange is only filtered when AddBalls/AddBalls2/SetState is present.
    let norm_add_balls2 = normalize_destiny_payload_arg(PyRep::List(vec![
        mk_ev("AddBalls2"),
        mk_ev("OnSlimItemChange"),
        mk_ev("SetBallInteractive"),
        mk_ev("PackagedAction"),
    ]));
    assert_eq!(
        norm_add_balls2,
        PyRep::List(vec![
            mk_ev("AddBalls2"),
            mk_ev("SetBallInteractive"),
            mk_ev("PackagedAction"),
        ])
    );

    // Without SetState/AddBalls/AddBalls2, OnSlimItemChange, SetBallInteractive, PackagedAction, and OnDockingAccepted are preserved.
    let norm_no_add = normalize_destiny_payload_arg(PyRep::Tuple(vec![PyRep::List(vec![
        mk_ev("OnSlimItemChange"),
        mk_ev("SetBallInteractive"),
        mk_ev("PackagedAction"),
        mk_ev("OnDockingAccepted"),
    ])]));
    assert_eq!(
        norm_no_add,
        PyRep::Tuple(vec![PyRep::List(vec![
            mk_ev("OnSlimItemChange"),
            mk_ev("SetBallInteractive"),
            mk_ev("PackagedAction"),
            mk_ev("OnDockingAccepted"),
        ])])
    );

    // should_suppress_macho_notification
    assert!(should_suppress_macho_notification("OnDamageStateChange", &[]));
    assert!(should_suppress_macho_notification("OnSessionChanged", &[]));
    // #1188: OnTradeInitiate suppress rule removed; never suppressed regardless of arg1 shape.
    assert!(!should_suppress_macho_notification(
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
        &[PyRep::List(vec![])]
    ));
    assert!(!should_suppress_macho_notification(
        "DoDestinyUpdate",
        &[PyRep::List(vec![mk_ev("OnClientControllerEvent"), mk_ev("Orbit")])]
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
    assert_eq!(scan_rows[0].dict_get("scanGroupID"), Some(&PyRep::Integer(4)));
    assert_eq!(scan_rows[0].dict_get("typeID"), Some(&PyRep::None));

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
    let norm_mod_attrs = |args| normalize_macho_notification("OnModuleAttributeChanges", args);
    let mod_attrs = norm_mod_attrs(vec![PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(1),
        PyRep::Integer(2),
        PyRep::Integer(3),
        PyRep::Integer(4),
        PyRep::Integer(5),
        PyRep::Real(6.0),
        PyRep::Real(7.0),
    ])])]);
    let PyRep::List(changes) = &mod_attrs[0] else { panic!() };
    assert!(matches!(&changes[0], PyRep::Tuple(elems) if elems.len() == 8 && elems[7] == PyRep::None));

    let mod_attrs_empty = norm_mod_attrs(vec![PyRep::Integer(0)]);
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
        ("superWeapon", 4489),
        ("superWeaponAmarr", 4489),
        ("superWeaponCaldari", 4490),
        ("superWeaponGallente", 4491),
        ("superWeaponMinmatar", 4492),
        ("superWeaponImmobilize", 4489),
        ("superWeaponCone", 6473),
        ("doomsdayConeDOT", 6473),
        ("superWeaponSpatial", 6474),
        ("doomsdayHOG", 6474),
        ("superWeaponMultiPoint", 6201),
        ("doomsdaySlash", 6201),
        ("superWeaponLanceWarmup", 6472),
        ("superWeaponLanceBeam", 6472),
        ("doomsdayBeamDOT", 6472),
        ("doomsdayAOEBalance", 11691),
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
    let norm_short = normalize_on_godma_ship_effect(
        vec![
            PyRep::Integer(555),
            PyRep::String("superWeapon".into()),
            PyRep::Bool(true),
        ],
        Some(&ctx.sde),
    );
    assert_eq!(norm_short[1], PyRep::Integer(4489));
    let norm_full = normalize_on_godma_ship_effect(
        vec![
            PyRep::Integer(555),
            PyRep::String("superWeaponCone".into()),
            PyRep::Integer(0),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Tuple(vec![
                PyRep::Integer(555),
                PyRep::Integer(999),
                PyRep::Integer(777),
            ]),
            PyRep::Integer(0),
            PyRep::Real(30_000.0),
            PyRep::Integer(0),
            PyRep::None,
        ],
        Some(&ctx.sde),
    );
    assert_eq!(norm_full[1], PyRep::Integer(6473));

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

    // 5. normalize_destiny_update_arg: D01 removed (#1188); passes entries through unchanged.
    let dock_ev_in = PyRep::List(vec![
        PyRep::Tuple(vec![
            PyRep::String("OnDockingAccepted".into()),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(60_003_760)]),
        ]),
        PyRep::Tuple(vec![
            PyRep::String("OnDockingAccepted".into()),
            PyRep::Tuple(vec![PyRep::Integer(60_003_760)]),
        ]),
    ]);
    assert_eq!(normalize_destiny_update_arg(dock_ev_in.clone()), dock_ev_in);

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

    // 4. Untouched service/method pairs:
    //    map.GetIncursionGlobalReport, paperDollServer.GetPaperDollData, storeManager.get_categories/get_offers,
    //    agentMgr.GetOffer / GetAgentLocationWrap / GetMissionBriefingInfo / GetMissionObjectiveInfo

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

    let female_doll = PyRep::keyval([
        ("gender", PyRep::Integer(0)),
        ("modifiers", PyRep::List(Vec::new())),
        ("colors", PyRep::List(Vec::new())),
    ]);
    assert_eq!(
        normalize_macho_rpc_reply(
            "paperDollServer",
            "GetPaperDollData",
            Ok(female_doll.clone()),
        )
        .unwrap(),
        female_doll
    );
    let male_doll = PyRep::keyval([
        ("gender", PyRep::Integer(1)),
        ("modifiers", PyRep::List(Vec::new())),
        ("colors", PyRep::List(Vec::new())),
    ]);
    assert_eq!(
        normalize_macho_rpc_reply(
            "paperDollServer",
            "GetPaperDollData",
            Ok(male_doll.clone()),
        )
        .unwrap(),
        male_doll
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
            Err(BridgeRpcError { code: "MissionNotActive".into(), message: "x".into(), client_error: None }),
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
            Err(BridgeRpcError { code: "MissionNotActive".into(), message: "x".into(), client_error: None }),
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

    // #1188: OnBeanPrime rule removed; typed List[KeyVal] and arbitrary args pass through unchanged.
    let bean_in = vec![PyRep::List(vec![PyRep::keyval([
        ("errorID", PyRep::Integer(99)),
        ("errorKeyHash", PyRep::Integer(0x1234)),
        ("logMode", PyRep::String("B".into())),
    ])])];
    assert_eq!(normalize_macho_notification("OnBeanPrime", bean_in.clone()), bean_in);
    assert_eq!(
        normalize_macho_notification("OnBeanPrime", vec![PyRep::Integer(1), PyRep::Integer(2)]),
        vec![PyRep::Integer(1), PyRep::Integer(2)]
    );

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

    assert_eq!(normalize_macho_notification("OnHeatAdded", vec![PyRep::Integer(1175), PyRep::Integer(55)]), vec![PyRep::Integer(1175), PyRep::Integer(55)]);
    assert_eq!(normalize_macho_notification("OnHeatRemoved", vec![PyRep::Integer(1176), PyRep::Integer(55)]), vec![PyRep::Integer(1176), PyRep::Integer(55)]);

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

    let ddu_dock_in = vec![PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::String("OnDockingAccepted".into()),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(60_003_760)]),
    ])])];
    assert_eq!(
        normalize_macho_notification("DoDestinyUpdate", ddu_dock_in.clone()),
        ddu_dock_in
    );
}

#[test]
fn gh998_and_gh999_agent_journal_bookmarks_and_dungeon_owner_compat() {
    // #998: GetMyJournalDetails bookmark dicts/KeyVals (both from KeyVal rows and 10-tuple rows)
    // must contain non-empty `hint`, positive `locationID`, and `0`-based `locationNumber`.
    let raw_journal = PyRep::Tuple(vec![
        PyRep::List(vec![
            PyRep::keyval([
                ("missionID", PyRep::Integer(101)),
                ("agentID", PyRep::Integer(3_008_416)),
                ("solarSystemID", PyRep::Integer(30_000_142)),
                ("missionType", PyRep::String("Courier".into())),
                (
                    "bookmarks",
                    PyRep::List(vec![PyRep::keyval([
                        ("bookmarkID", PyRep::Integer(900_001)),
                        ("solarSystemID", PyRep::Integer(30_000_142)),
                        ("label", PyRep::String("Mission: Courier Run".into())),
                        ("coords", PyRep::None),
                    ])]),
                ),
            ]),
            PyRep::Tuple(vec![
                PyRep::Integer(2),
                PyRep::Bool(false),
                PyRep::String("UI/Agents/MissionTypes/Encounter".into()),
                PyRep::String("Encounter Run".into()),
                PyRep::Integer(3_008_416),
                PyRep::None,
                PyRep::List(vec![PyRep::keyval([
                    ("bookmarkID", PyRep::Integer(900_002)),
                    ("solarSystemID", PyRep::Integer(30_000_144)),
                    ("itemID", PyRep::Integer(60_003_760)),
                    ("typeID", PyRep::Integer(1529)),
                ])]),
                PyRep::Bool(false),
                PyRep::Bool(false),
                PyRep::Integer(102),
            ]),
        ]),
        PyRep::List(Vec::new()),
        PyRep::List(Vec::new()),
    ]);
    let norm_journal = normalize_macho_rpc_reply("agentMgr", "GetMyJournalDetails", Ok(raw_journal))
        .expect("normalize GetMyJournalDetails");
    let PyRep::Tuple(parts) = norm_journal else {
        panic!("expected tuple from GetMyJournalDetails");
    };
    let PyRep::List(ref m_rows) = parts[0] else {
        panic!("expected missions list");
    };
    assert_eq!(m_rows.len(), 2);
    for (row_idx, expected_loc, expected_hint, expected_loc_type, expected_item, expected_type) in [
        (
            0usize,
            30_000_142_i64,
            "UI/Agents/MissionTypes/Courier",
            "objective.destination",
            PyRep::None,
            5_i64,
        ),
        (
            1usize,
            30_000_144_i64,
            "UI/Agents/MissionTypes/Encounter",
            "dungeon",
            PyRep::Integer(60_003_760),
            1529_i64,
        ),
    ] {
        let PyRep::Tuple(ref cols) = m_rows[row_idx] else {
            panic!("expected 10-tuple row");
        };
        let PyRep::List(ref bms) = cols[6] else {
            panic!("expected bookmarks list");
        };
        assert_eq!(bms.len(), 1);
        assert!(matches!(bms[0], PyRep::KeyVal(_)));
        assert_eq!(bms[0].dict_get("hint"), Some(&PyRep::String(expected_hint.into())));
        assert_eq!(bms[0].dict_get("locationID"), Some(&PyRep::Integer(expected_loc)));
        assert_eq!(bms[0].dict_get("solarSystemID"), Some(&PyRep::Integer(expected_loc)));
        assert_eq!(bms[0].dict_get("solarsystemID"), Some(&PyRep::Integer(expected_loc)));
        assert_eq!(bms[0].dict_get("locationNumber"), Some(&PyRep::Integer(0)));
        assert_eq!(bms[0].dict_get("agentID"), Some(&PyRep::Integer(3_008_416)));
        assert_eq!(bms[0].dict_get("locationType"), Some(&PyRep::String(expected_loc_type.into())));
        assert_eq!(bms[0].dict_get("itemID"), Some(&expected_item));
        assert_eq!(bms[0].dict_get("typeID"), Some(&PyRep::Integer(expected_type)));
        assert!(bms[0].dict_get("coords").is_none());
    }

    // #999: GetMissionObjectiveInfo dungeons[].ownerID must never be an agent/character ID
    // (3_000_000..4_000_000 or player char), while preserving valid NPC faction (500_010) and corp (1_000_035) IDs.
    let obj_bad_owner = normalize_macho_rpc_reply(
        "agentMgr",
        "GetMissionObjectiveInfo",
        Ok(PyRep::keyval([
            ("missionID", PyRep::Integer(502)),
            ("agentID", PyRep::Integer(3_008_416)),
            ("solarSystemID", PyRep::Integer(30_000_142)),
            ("originStationID", PyRep::Integer(60_003_760)),
            ("dungeons", PyRep::List(vec![PyRep::Dict(vec![
                (PyRep::String("dungeonID".into()), PyRep::Integer(213)),
                (PyRep::String("ownerID".into()), PyRep::Integer(3_008_416)),
            ])])),
        ])),
    )
    .expect("normalize bad dungeon ownerID");
    let Some(PyRep::List(bad_duns)) = obj_bad_owner.dict_get("dungeons") else {
        panic!("expected dungeons");
    };
    assert_eq!(bad_duns[0].dict_get("ownerID"), Some(&PyRep::Integer(1_000_002)));

    let obj_faction_owner = normalize_macho_rpc_reply(
        "agentMgr",
        "GetMissionObjectiveInfo",
        Ok(PyRep::keyval([
            ("missionID", PyRep::Integer(503)),
            ("agentID", PyRep::Integer(3_008_416)),
            ("solarSystemID", PyRep::Integer(30_000_142)),
            ("originStationID", PyRep::Integer(60_003_760)),
            ("dungeons", PyRep::List(vec![PyRep::Dict(vec![
                (PyRep::String("dungeonID".into()), PyRep::Integer(213)),
                (PyRep::String("ownerID".into()), PyRep::Integer(500_010)),
            ])])),
        ])),
    )
    .expect("normalize faction dungeon ownerID");
    let Some(PyRep::List(fac_duns)) = obj_faction_owner.dict_get("dungeons") else {
        panic!("expected dungeons");
    };
    assert_eq!(fac_duns[0].dict_get("ownerID"), Some(&PyRep::Integer(500_010)));
}

#[test]
fn gh1023_mission_objective_owner_ids_reject_agent_and_character_ids() {
    // 1. Synthesized transport objective rejects character/agent IDs in pickupOwnerID/dropoffOwnerID
    //    and falls back to corporationID / factionID / 1_000_002, while keeping agent_id on "agent".
    let courier_with_agent_owners = normalize_macho_rpc_reply(
        "agentMgr",
        "GetMissionObjectiveInfo",
        Ok(PyRep::keyval([
            ("missionID", PyRep::Integer(504)),
            ("agentID", PyRep::Integer(3_016_134)),
            ("corporationID", PyRep::Integer(1_000_004)),
            ("pickupOwnerID", PyRep::Integer(3_016_134)),
            ("dropoffOwnerID", PyRep::Integer(90_000_001)),
            ("solarSystemID", PyRep::Integer(30_000_138)),
            ("originStationID", PyRep::Integer(60_000_403)),
            ("destinationStationID", PyRep::Integer(60_001_807)),
            ("missionState", PyRep::Integer(2)),
        ])),
    )
    .expect("normalize courier with bad owner ids");
    let Some(PyRep::List(objs)) = courier_with_agent_owners.dict_get("objectives") else {
        panic!("expected objectives list");
    };
    assert_eq!(objs.len(), 2);
    let PyRep::Tuple(agent_entry) = &objs[0] else { panic!("expected agent entry") };
    assert_eq!(agent_entry[0], PyRep::String("agent".into()));
    let PyRep::Tuple(agent_vals) = &agent_entry[1] else { panic!("expected agent tuple") };
    assert_eq!(agent_vals[0], PyRep::Integer(3_016_134));
    let PyRep::Tuple(trans_entry) = &objs[1] else { panic!("expected transport entry") };
    assert_eq!(trans_entry[0], PyRep::String("transport".into()));
    let PyRep::Tuple(trans_vals) = &trans_entry[1] else { panic!("expected transport tuple") };
    assert_eq!(trans_vals[0], PyRep::Integer(1_000_004));
    assert_eq!(trans_vals[2], PyRep::Integer(1_000_004));

    // 2. Valid corp/faction/player-corp IDs in pickupOwnerID & dropoffOwnerID are preserved.
    for (p_owner, d_owner) in [
        (1_000_004_i64, 1_000_016_i64),
        (500_001, 98_100_001),
    ] {
        let valid = normalize_macho_rpc_reply(
            "agentMgr",
            "GetMissionObjectiveInfo",
            Ok(PyRep::keyval([
                ("missionID", PyRep::Integer(505)),
                ("agentID", PyRep::Integer(3_016_134)),
                ("pickupOwnerID", PyRep::Integer(p_owner)),
                ("dropoffOwnerID", PyRep::Integer(d_owner)),
                ("originStationID", PyRep::Integer(60_000_403)),
                ("destinationStationID", PyRep::Integer(60_001_807)),
                ("missionState", PyRep::Integer(2)),
            ])),
        )
        .expect("normalize valid corp/faction owners");
        let Some(PyRep::List(v_objs)) = valid.dict_get("objectives") else { panic!() };
        let PyRep::Tuple(t_entry) = &v_objs[1] else { panic!() };
        let PyRep::Tuple(t_vals) = &t_entry[1] else { panic!() };
        assert_eq!(t_vals[0], PyRep::Integer(p_owner));
        assert_eq!(t_vals[2], PyRep::Integer(d_owner));
    }

    // 3. Pre-built objectives list ("transport" and "fetch") also replaces agent/char/alliance IDs
    //    with corporationID/factionID/1_000_002, while preserving "agent" tuple's agent_id.
    let loc_stub = PyRep::Dict(vec![
        (PyRep::String("locationID".into()), PyRep::Integer(60_000_403)),
        (PyRep::String("solarsystemID".into()), PyRep::Integer(30_000_138)),
    ]);
    let cargo_stub = PyRep::Dict(vec![
        (PyRep::String("typeID".into()), PyRep::Integer(34)),
        (PyRep::String("quantity".into()), PyRep::Integer(6)),
        (PyRep::String("volume".into()), PyRep::Real(6.0)),
        (PyRep::String("hasCargo".into()), PyRep::Bool(true)),
    ]);
    let prebuilt = normalize_macho_rpc_reply(
        "agentMgr",
        "GetMissionObjectiveInfo",
        Ok(PyRep::keyval([
            ("missionID", PyRep::Integer(506)),
            ("agentID", PyRep::Integer(3_016_134)),
            ("factionID", PyRep::Integer(500_001)),
            (
                "objectives",
                PyRep::List(vec![
                    PyRep::Tuple(vec![
                        PyRep::String("agent".into()),
                        PyRep::Tuple(vec![PyRep::Integer(3_016_134), loc_stub.clone()]),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::String("transport".into()),
                        PyRep::Tuple(vec![
                            PyRep::Integer(3_016_134),
                            loc_stub.clone(),
                            PyRep::Integer(99_000_001),
                            loc_stub.clone(),
                            cargo_stub.clone(),
                        ]),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::String("fetch".into()),
                        PyRep::Tuple(vec![
                            PyRep::Integer(3_016_134),
                            loc_stub,
                            cargo_stub,
                        ]),
                    ]),
                ]),
            ),
        ])),
    )
    .expect("normalize prebuilt objectives");
    let Some(PyRep::List(p_objs)) = prebuilt.dict_get("objectives") else { panic!() };
    let PyRep::Tuple(pa) = &p_objs[0] else { panic!() };
    let PyRep::Tuple(pa_v) = &pa[1] else { panic!() };
    assert_eq!(pa_v[0], PyRep::Integer(3_016_134));
    let PyRep::Tuple(pt) = &p_objs[1] else { panic!() };
    let PyRep::Tuple(pt_v) = &pt[1] else { panic!() };
    assert_eq!(pt_v[0], PyRep::Integer(500_001));
    assert_eq!(pt_v[2], PyRep::Integer(500_001));
    let PyRep::Tuple(pf) = &p_objs[2] else { panic!() };
    let PyRep::Tuple(pf_v) = &pf[1] else { panic!() };
    assert_eq!(pf_v[0], PyRep::Integer(500_001));
}

#[tokio::test]
async fn c9_typed_handlers_fleet_sov_corpmgr_emit_canonical_wire_shapes() {
    let ctx = test_ctx();
    let struct_id = 1_055_000_933_i64;
    let (uid, corp_id) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get(&90_000_001).unwrap();
        let uid = ch.user_id;
        let cid = ch.corporation_id;
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                owner_corp_id: cid,
                type_id: 32458,
                name: "IHub".into(),
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
        (uid, cid)
    });
    let fleet_id = 77_000_933_i64;
    ctx.fleet_extras.lock().unwrap().adverts.insert(
        fleet_id,
        crate::services::fleet_social::fleet_social_fleets::FleetAdvertState {
            fleet_id,
            leader_id: 90_000_001,
            fleet_name: "Roam".into(),
            description: "C9 fleet advert".into(),
            solar_system_id: 30_000_142,
            invite_scope: 0,
            auto_join: true,
            min_standing: -10.0,
            min_security: -10.0,
            allowed_entities: std::collections::HashSet::new(),
            _created_unix: 1_700_000_000,
        },
    );

    let mut sess = SessionState {
        user_id: Some(uid),
        character_id: Some(90_000_001),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        ..Default::default()
    };
    let kw = HashMap::new();

    for method in ["GetAvailableFleetAds", "GetFleetAdsForChar"] {
        let ads_out = ctx
            .dispatch_rpc(
                &mut sess,
                "fleetProxy",
                None,
                method,
                &[],
                &kw,
                Some("machoNet:26000"),
            )
            .await
            .unwrap();
        let PyRep::Dict(entries) = ads_out else {
            panic!("expected Dict from {method}");
        };
        let (_, ad_rep) = entries
            .iter()
            .find(|(k, _)| *k == PyRep::Integer(fleet_id))
            .expect("advert present by fleetID");
        assert_eq!(ad_rep.dict_get("fleetName"), Some(&PyRep::String("Roam".into())));
        assert_eq!(ad_rep.dict_get("joinNeedsApproval"), Some(&PyRep::Bool(false)));
        assert_eq!(ad_rep.dict_get("autoJoin"), Some(&PyRep::Bool(true)));
        assert!(ad_rep.dict_get("leader").is_some());
        assert!(ad_rep.dict_get("dateCreated").is_some());
    }

    let sov_local = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "GetSovStructuresInfoForLocalSolarSystem",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap();
    let PyRep::List(local_items) = sov_local else {
        panic!("expected List from GetSovStructuresInfoForLocalSolarSystem");
    };
    assert!(local_items
        .iter()
        .any(|it| it.dict_get("structureID") == Some(&PyRep::Integer(struct_id))));

    let sov_empty = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "GetSovStructuresInfoForSolarSystem",
            &[PyRep::Integer(30_000_144)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap();
    assert_eq!(sov_empty, PyRep::List(Vec::new()));

    for method in ["GetPublicInfo", "GetCorporations"] {
        let corp_row = ctx
            .dispatch_rpc(
                &mut sess,
                "corpmgr",
                None,
                method,
                &[PyRep::Integer(1_000_035)],
                &kw,
                Some("machoNet:26000"),
            )
            .await
            .unwrap();
        assert!(
            matches!(&corp_row, PyRep::Object { type_name, .. } if type_name == "util.Row"),
            "expected util.Row from corpmgr.{method}"
        );
        assert_eq!(
            corp_row.dict_get("corporationID"),
            Some(&PyRep::Integer(1_000_035))
        );
        assert!(corp_row
            .dict_get("ceoID")
            .and_then(PyRep::as_i64)
            .is_some_and(|id| id > 0));
    }
}

#[test]
fn module_attribute_changes_normalizes_list_entries_without_mangling() {
    let cases = [
        (
            "7_elem_list_nonzero_attr",
            vec![
                PyRep::String("OnModuleAttributeChange".into()),
                PyRep::Integer(100_001),
                PyRep::Integer(700_002),
                PyRep::Integer(9),
                PyRep::Integer(123_456),
                PyRep::Real(120.0),
                PyRep::Real(90.0),
            ],
            vec![
                PyRep::String("OnModuleAttributeChange".into()),
                PyRep::Integer(100_001),
                PyRep::Integer(700_002),
                PyRep::Integer(9),
                PyRep::Integer(123_456),
                PyRep::Real(120.0),
                PyRep::Real(90.0),
                PyRep::None,
            ],
        ),
        (
            "7_elem_list_zero_attr",
            vec![
                PyRep::String("OnModuleAttributeChange".into()),
                PyRep::None,
                PyRep::Integer(700_003),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Real(50.0),
                PyRep::Real(25.0),
            ],
            vec![
                PyRep::String("OnModuleAttributeChange".into()),
                PyRep::None,
                PyRep::Integer(700_003),
                PyRep::Integer(2),
                PyRep::Integer(0),
                PyRep::Real(50.0),
                PyRep::Real(25.0),
                PyRep::None,
            ],
        ),
        (
            "8_elem_list_bool_cause_and_zero_attr",
            vec![
                PyRep::String("OnModuleAttributeChange".into()),
                PyRep::Integer(100_002),
                PyRep::Integer(700_004),
                PyRep::Integer(0),
                PyRep::Integer(987_654),
                PyRep::Real(10.5),
                PyRep::Real(5.25),
                PyRep::Bool(true),
            ],
            vec![
                PyRep::String("OnModuleAttributeChange".into()),
                PyRep::Integer(100_002),
                PyRep::Integer(700_004),
                PyRep::Integer(2),
                PyRep::Integer(987_654),
                PyRep::Real(10.5),
                PyRep::Real(5.25),
                PyRep::None,
            ],
        ),
        (
            "8_elem_list_preserves_all_fields",
            vec![
                PyRep::String("OnModuleAttributeChange".into()),
                PyRep::Integer(100_003),
                PyRep::Integer(700_005),
                PyRep::Integer(268),
                PyRep::Integer(555_000),
                PyRep::Real(200.0),
                PyRep::Real(150.0),
                PyRep::Integer(42),
            ],
            vec![
                PyRep::String("OnModuleAttributeChange".into()),
                PyRep::Integer(100_003),
                PyRep::Integer(700_005),
                PyRep::Integer(268),
                PyRep::Integer(555_000),
                PyRep::Real(200.0),
                PyRep::Real(150.0),
                PyRep::Integer(42),
            ],
        ),
    ];

    for (label, input_elems, expected_elems) in cases {
        let out = normalize_macho_notification(
            "OnModuleAttributeChanges",
            vec![PyRep::List(vec![PyRep::List(input_elems)])],
        );
        assert_eq!(
            out,
            vec![PyRep::List(vec![PyRep::Tuple(expected_elems)])],
            "case {label}"
        );
    }
}

#[tokio::test]
async fn agent_mgr_bonus_rewards_wire_timer_and_post_expiry_omission() {
    // 1. Direct unit coverage for `normalize_agent_mission_objective_rep_at` with custom `bonusWindowSecs` & `bonusExpiryUnix`
    let now0 = 1_800_000_000_u64;
    let rep_64m = normalize_agent_mission_objective_rep_at(
        PyRep::keyval([
            ("missionID", PyRep::Integer(13_514)),
            ("missionState", PyRep::Integer(2)),
            ("iskReward", PyRep::Integer(29_000)),
            ("bonusReward", PyRep::Integer(37_000)),
            ("bonusWindowSecs", PyRep::Integer(3_840)),
            ("bonusExpiryUnix", PyRep::Integer((now0 + 2_000) as i64)),
        ]),
        Some(now0),
    );
    assert_eq!(
        rep_64m.dict_get("bonusRewards"),
        Some(&PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(2_000 * 10_000_000),
            PyRep::Integer(29),
            PyRep::Integer(37_000),
            PyRep::None,
            PyRep::Integer(64),
        ])])),
        "Active 64m mission must carry exact remaining 100ns ticks and 64-minute window"
    );

    let rep_expired = normalize_agent_mission_objective_rep_at(
        PyRep::keyval([
            ("missionID", PyRep::Integer(13_514)),
            ("missionState", PyRep::Integer(2)),
            ("iskReward", PyRep::Integer(29_000)),
            ("bonusReward", PyRep::Integer(37_000)),
            ("bonusWindowSecs", PyRep::Integer(3_840)),
            ("bonusExpiryUnix", PyRep::Integer((now0 + 2_000) as i64)),
        ]),
        Some(now0 + 2_001),
    );
    assert_eq!(
        rep_expired.dict_get("bonusRewards"),
        Some(&PyRep::List(Vec::new())),
        "Expired bonus window must omit bonusRewards over MachoNet"
    );

    // 2. End-to-end MachoNet dispatch (`Some("machoNet:26000")`) with pinned virtual clock
    let ctx = test_ctx();
    let clock = Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(now0));
    ctx.set_clock(clock.clone());
    let char_id = 90_000_001_i64;
    let mut ag1 = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 2 && a.division_id == 24 && a.level == 1 && a.solar_system_id.is_some() && a.station_id.is_some())
        .expect("L1 security agent")
        .clone();
    ag1.solar_system_id = Some(30_000_142);
    let ag4 = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 2 && a.division_id == 24 && a.level == 4 && a.solar_system_id.is_some() && a.station_id.is_some())
        .expect("L4 security agent")
        .clone();
    let (uid, corp_id) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 2;
        ch.solar_system_id = 30_000_142;
        ch.station_id = Some(60_003_760);
        snap.skills.remove(&char_id);
        for (mid, aid, sys_id, win_s) in [
            (13_514_u32, ag1.agent_id, ag1.solar_system_id.unwrap(), 3_840_u64),
            (900_u32, ag4.agent_id, ag4.solar_system_id.unwrap(), 14_400_u64),
        ] {
            snap.active_missions.insert(
                (char_id, mid),
                eve_db::ActiveMissionRecord {
                    character_id: char_id,
                    mission_id: mid,
                    agent_id: aid,
                    solar_system_id: sys_id,
                    state: "Accepted".into(),
                    current_room_id: 1,
                    npcs_remaining: 0,
                    spawned_npc_ball_ids: Vec::new(),
                    bookmark_id: None,
                },
            );
            crate::services::agents_missions::save_mission_meta(
                snap,
                char_id,
                &eve_db::MissionTimingAndCargoMeta {
                    mission_id: mid,
                    agent_id: aid,
                    offered_at_unix: now0,
                    offer_expiry_unix: now0 + 86_400,
                    accepted_at_unix: Some(now0),
                    accept_expiry_unix: Some(now0 + 604_800),
                    bonus_expiry_unix: Some(now0 + win_s),
                    offered_level: Some(if mid == 900 { 4 } else { 1 }),
                    ..Default::default()
                },
            );
        }
        (ch.user_id, ch.corporation_id)
    });
    let mut sess = SessionState {
        user_id: Some(uid),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        station_id: Some(60_003_760),
        ..Default::default()
    };
    let kw = HashMap::new();

    // Initial 64m (13514 -> 3840s, 37_000 ISK) and 4h (900 -> 14400s, 510_000 ISK)
    let wire_13514 = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetMissionObjectiveInfo",
            &[PyRep::Integer(ag1.agent_id), PyRep::Integer(13_514)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("wire 13514 initial");
    assert_eq!(
        wire_13514.dict_get("bonusRewards"),
        Some(&PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(3_840 * 10_000_000),
            PyRep::Integer(29),
            PyRep::Integer(37_000),
            PyRep::None,
            PyRep::Integer(64),
        ])])),
        "64-minute mission must preserve 3840s ticks and 64m duration on wire"
    );

    let wire_900 = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetMissionObjectiveInfo",
            &[PyRep::Integer(ag4.agent_id), PyRep::Integer(900)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("wire 900 initial");
    assert_eq!(
        wire_900.dict_get("bonusRewards"),
        Some(&PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(14_400 * 10_000_000),
            PyRep::Integer(29),
            PyRep::Integer(510_000),
            PyRep::None,
            PyRep::Integer(240),
        ])])),
        "4-hour mission must preserve 14400s ticks and 240m duration on wire"
    );

    // Advance 900s: countdown ticks drop to 2940s while window remains 64m; 900 drops to 13500s / 240m
    eve_db::ServerClock::advance_secs(&*clock, 900);
    let wire_13514_mid = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetMissionObjectiveInfo",
            &[PyRep::Integer(ag1.agent_id), PyRep::Integer(13_514)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("wire 13514 mid");
    assert_eq!(
        wire_13514_mid.dict_get("bonusRewards"),
        Some(&PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(2_940 * 10_000_000),
            PyRep::Integer(29),
            PyRep::Integer(37_000),
            PyRep::None,
            PyRep::Integer(64),
        ])])),
    );

    // Advance past 13514 expiry (2941s more -> total +3841s): 13514 omits bonusRewards while 900 still has 10559s
    eve_db::ServerClock::advance_secs(&*clock, 2_941);
    let wire_13514_exp = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetMissionObjectiveInfo",
            &[PyRep::Integer(ag1.agent_id), PyRep::Integer(13_514)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("wire 13514 expired");
    assert_eq!(wire_13514_exp.dict_get("bonusRewards"), Some(&PyRep::List(Vec::new())));

    let wire_900_still_live = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetMissionObjectiveInfo",
            &[PyRep::Integer(ag4.agent_id), PyRep::Integer(900)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("wire 900 still live");
    assert_eq!(
        wire_900_still_live.dict_get("bonusRewards"),
        Some(&PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(10_559 * 10_000_000),
            PyRep::Integer(29),
            PyRep::Integer(510_000),
            PyRep::None,
            PyRep::Integer(240),
        ])])),
    );
}
