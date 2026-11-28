//! Comprehensive deterministic unit tests covering all RPC/notification normalizers
//! and helper functions in `router::client_compat`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::mission_fixture::{self, MissionFixture};
use eve_db::{UpwellStructureRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
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

fn inv_keyval_to_dbrow(rep: PyRep) -> PyRep {
    super::inv_keyval_to_dbrow_with_sde(rep, None)
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

    // ensure_anomaly_dungeon_id
    assert_eq!(ensure_anomaly_dungeon_id(PyRep::Bool(false)), PyRep::Bool(false));
    let anom_missing = ensure_anomaly_dungeon_id(PyRep::keyval([("dungeonID", PyRep::None)]));
    assert_eq!(anom_missing.dict_get("dungeonID"), Some(&PyRep::Integer(1001)));
    let anom_present = ensure_anomaly_dungeon_id(PyRep::keyval([("dungeonID", PyRep::Integer(4242))]));
    assert_eq!(anom_present.dict_get("dungeonID"), Some(&PyRep::Integer(4242)));
}

#[test]
fn destiny_and_notification_normalizers_and_suppression_branches() {
    let mk_ev = |name: &str| {
        PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Tuple(vec![PyRep::String(name.into()), PyRep::Tuple(vec![])]),
        ])
    };

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

    // 7. build_v24_reprocessing_quotes_tuple: stationBaseYield override, duplicate typeID deduplication, outputs -> CRowset
    let quotes = crate::services::space_entity_mining::build_v24_reprocessing_quotes_tuple(
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
    assert_eq!(dbrow_sde.dbrow_get("itemID"), Some(&PyRep::Integer(7001)));
    assert_eq!(dbrow_sde.dbrow_get("flagID"), Some(&PyRep::Integer(4)));
    assert_eq!(dbrow_sde.dbrow_get("quantity"), Some(&PyRep::Integer(-1)));
    assert_eq!(
        dbrow_sde.dbrow_get("groupID"),
        Some(&PyRep::Integer(i64::from(ctx.sde.types[&587].group_id)))
    );
    assert_eq!(
        dbrow_sde.dbrow_get("categoryID"),
        Some(&PyRep::Integer(i64::from(ctx.sde.types[&587].category_id)))
    );
    assert_eq!(
        dbrow_sde.dbrow_get("customInfo"),
        Some(&PyRep::String("rifter".into()))
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
    assert_eq!(dbrow_stack.dbrow_get("flagID"), Some(&PyRep::Integer(5)));
    assert_eq!(dbrow_stack.dbrow_get("quantity"), Some(&PyRep::Integer(250)));
    assert_eq!(dbrow_stack.dbrow_get("groupID"), Some(&PyRep::Integer(0)));
    assert_eq!(dbrow_stack.dbrow_get("categoryID"), Some(&PyRep::Integer(0)));
    assert_eq!(inv_keyval_to_dbrow(PyRep::Integer(99)), PyRep::Integer(99));

    // 4. Untouched service/method pairs & agentMgr.GetOffer:
    //    paperDollServer.GetPaperDollData, agentMgr.GetOffer

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
}

#[test]
fn w4_mutant_killers_all_remaining_notifications() {
    let ctx = test_ctx();

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
        m_pair[0].dbrow_get("groupID"),
        Some(&PyRep::Integer(i64::from(ctx.sde.types[&587].group_id)))
    );
    assert_eq!(m_rows[1].dbrow_get("quantity"), Some(&PyRep::Integer(5)));

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

    let ddu_dock_in = vec![PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::String("OnDockingAccepted".into()),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(60_003_760)]),
    ])])];
    assert_eq!(
        normalize_macho_notification("DoDestinyUpdate", ddu_dock_in.clone()),
        ddu_dock_in
    );
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
    // End-to-end MachoNet dispatch (`Some("machoNet:26000")`) with pinned virtual clock
    let now0 = 1_800_000_000_u64;
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
        let (uid, corp_id) = {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.clone_grade = 2;
            ch.solar_system_id = 30_000_142;
            ch.station_id = Some(60_003_760);
            (ch.user_id, ch.corporation_id)
        };
        snap.skills.remove(&char_id);
        for (mid, aid, sys_id, win_s) in [
            (13_514_u32, ag1.agent_id, ag1.solar_system_id.unwrap(), 3_840_u64),
            (900_u32, ag4.agent_id, ag4.solar_system_id.unwrap(), 14_400_u64),
        ] {
            mission_fixture::insert(
                snap,
                MissionFixture::accepted(char_id, mid, aid, sys_id)
                    .current_room_id(1)
                    .offered_at_unix(now0)
                    .offer_expiry_unix(now0 + 86_400)
                    .accepted_at_unix(Some(now0))
                    .accept_expiry_unix(Some(now0 + 604_800))
                    .bonus_expiry_unix(Some(now0 + win_s))
                    .offered_level(Some(if mid == 900 { 4 } else { 1 })),
            );
        }
        (uid, corp_id)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn map_get_current_sov_data_preserves_claim_structure_id_and_location_filter_over_wire() {
    use eve_db::{AllianceRecord, SystemDynamicState};

    // Real MachoNet dispatch_rpc path (`Some("machoNet:26000")`) with regionID / solarSystemID filter
    // and raw dispatch check confirming both `claimStructureID` and `infrastructureHubID` are emitted.
    let ctx = test_ctx();
    let mut null_systems: Vec<(u32, u32)> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status < 0.0 && s.solar_system_id < 31_000_000)
        .map(|s| (s.solar_system_id, s.region_id))
        .collect();
    null_systems.sort_unstable();
    let (sys_a, reg_a) = null_systems[0];
    let (sys_b, _) = *null_systems.iter().find(|&&(_, r)| r != reg_a).unwrap();
    let corp_id = 98_950_001_i64;
    let ally_id = 99_950_001_i64;
    let hub_a = 1_095_000_001_i64;
    let hub_b = 1_095_000_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "Wire Sov".into(),
                short_name: "WSOV".into(),
                executor_corp_id: corp_id,
                member_corp_ids: [corp_id].into_iter().collect(),
            },
        );
        for (sid, hid) in [(sys_a, hub_a), (sys_b, hub_b)] {
            snap.system_states.insert(
                sid,
                SystemDynamicState {
                    solar_system_id: sid,
                    sov_alliance_id: Some(ally_id),
                    ..Default::default()
                },
            );
            snap.structures.insert(
                hid,
                UpwellStructureRecord {
                    structure_id: hid,
                    type_id: crate::services::sov_services::TYPE_SOVEREIGNTY_HUB,
                    owner_corp_id: corp_id,
                    solar_system_id: sid,
                    name: format!("Wire Hub {sid}"),
                    state: "ShieldVulnerable".into(),
                    ..Default::default()
                },
            );
        }
    });

    let mut sess = SessionState::new(26_001);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    let kw = HashMap::new();
    let wire_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "map",
            None,
            "GetCurrentSovData",
            &[PyRep::Integer(i64::from(reg_a))],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("dispatch map.GetCurrentSovData wire");
    let PyRep::Rowset {
        columns: w_hdr,
        rows: w_lines,
    } = wire_rep else {
        panic!("expected wire Rowset from map.GetCurrentSovData");
    };
    assert_eq!(w_hdr[0], "locationID");
    assert_eq!(w_hdr[4], "claimStructureID");
    assert_eq!(w_lines.len(), 1);
    assert_eq!(w_lines[0][0], PyRep::Integer(i64::from(sys_a)));
    assert_eq!(w_lines[0][1], PyRep::Integer(i64::from(sys_a)));
    assert_eq!(w_lines[0][2], PyRep::Integer(ally_id));
    assert_eq!(w_lines[0][3], PyRep::Integer(corp_id));
    assert_eq!(w_lines[0][4], PyRep::Integer(hub_a));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn arch_933_t2_migrated_rpcs_direct_equals_wire() {
    use crate::router::client_wire::ClientWireSession;
    use eve_db::{
        AllianceRecord, IndustryJobRecord, PlanetaryColonyRecord, SystemDynamicState,
        VirtualServerClock,
    };
    use eve_machonet::service_roles;

    let ctx = test_ctx();
    let base_unix = 1_800_000_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));

    let char_id = 90_000_001_i64;
    let corp_id = 1_000_170_i64;
    let jita_sys_id = 30_000_142_u32;
    let jita_station_id = 60_003_760_i64;
    let jita_sys = ctx
        .sde
        .solar_systems
        .get(&jita_sys_id)
        .expect("Jita solar system in SDE");
    let jita_constellation_id = jita_sys.constellation_id;
    let jita_region_id = jita_sys.region_id;
    let jita_planet_id = *jita_sys
        .planet_ids
        .first()
        .expect("Jita must have at least one planet in SDE");
    let sample_agent_id = *ctx.sde.agents.keys().min().expect("at least one SDE agent");
    let sample_npc_corp_id = *ctx
        .sde
        .npc_corporations
        .keys()
        .min()
        .expect("at least one SDE NPC corporation");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.standings.insert(sample_agent_id, 3.25);
        }
        snap.alliances.insert(
            99_933_101,
            AllianceRecord {
                alliance_id: 99_933_101,
                name: "Wire Direct Sov".into(),
                short_name: "WDS".into(),
                executor_corp_id: corp_id,
                member_corp_ids: [corp_id].into_iter().collect(),
            },
        );
        let sys_st = snap
            .system_states
            .entry(jita_sys_id)
            .or_insert_with(|| SystemDynamicState {
                solar_system_id: jita_sys_id,
                ..Default::default()
            });
        sys_st.sov_alliance_id = Some(99_933_101);
        sys_st.insurgency_corruption_stage = 5;

        snap.structures.insert(
            1_093_310_001,
            UpwellStructureRecord {
                structure_id: 1_093_310_001,
                type_id: crate::services::sov_services::TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_id,
                solar_system_id: jita_sys_id,
                name: "Direct Wire Hub".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            1_093_310_002,
            UpwellStructureRecord {
                structure_id: 1_093_310_002,
                type_id: 12_235,
                owner_corp_id: corp_id,
                solar_system_id: jita_sys_id,
                name: "Direct Wire Tower".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.planetary_colonies.insert(
            (char_id, jita_planet_id),
            PlanetaryColonyRecord {
                planet_id: jita_planet_id,
                owner_id: char_id,
                solar_system_id: jita_sys_id,
                command_center_level: 2,
                pins_count: 1,
                last_extracted_unix: base_unix,
                extracted_commodities: HashMap::new(),
            },
        );
        snap.industry_jobs.insert(
            93_311,
            IndustryJobRecord {
                job_id: 93_311,
                installer_id: char_id,
                facility_id: jita_station_id,
                blueprint_item_id: 88_101,
                blueprint_type_id: 688,
                activity_id: 1,
                runs: 1,
                product_type_id: 587,
                status: "Active".into(),
                duration_seconds: 600,
                start_unix: base_unix,
                blueprint_location_id: jita_station_id,
                output_location_id: jita_station_id,
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            93_312,
            IndustryJobRecord {
                job_id: 93_312,
                installer_id: char_id,
                facility_id: jita_station_id,
                blueprint_item_id: 88_102,
                blueprint_type_id: 688,
                activity_id: 8,
                runs: 2,
                product_type_id: 11372,
                status: "Failed".into(),
                duration_seconds: 600,
                start_unix: base_unix,
                blueprint_location_id: jita_station_id,
                output_location_id: jita_station_id,
                completed_unix: Some(base_unix + 600),
                completed_character_id: Some(char_id),
                ..Default::default()
            },
        );
    });

    let mut dir_sess = SessionState::new(93_301);
    dir_sess.user_id = Some(1);
    dir_sess.character_id = Some(char_id);
    dir_sess.corporation_id = Some(corp_id);
    dir_sess.solar_system_id = Some(jita_sys_id);
    dir_sess.station_id = Some(jita_station_id);
    dir_sess.role = service_roles::ROLE_PLAYER_DEFAULT | service_roles::ROLE_ADMIN;

    let mut wire_sess = dir_sess.clone();
    wire_sess.session_id = 93_302;
    let mut wire = ClientWireSession::from_session(&ctx, wire_sess);
    let kw = HashMap::new();

    // Seed owner notes (both short fallback and valid prefixed) and a contact via direct RPC.
    for (label, body) in [("A", "short"), ("N:Tactical", "Hold")] {
        ctx.dispatch_rpc(
            &mut dir_sess,
            "charMgr",
            None,
            "AddOwnerNote",
            &[
                PyRep::Integer(char_id),
                PyRep::String(label.into()),
                PyRep::String(body.into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("AddOwnerNote");
    }
    ctx.dispatch_rpc(
        &mut dir_sess,
        "charMgr",
        None,
        "AddContact",
        &[
            PyRep::Integer(90_000_002),
            PyRep::Real(5.0),
            PyRep::Bool(true),
        ],
        &kw,
        None,
    )
    .await
    .expect("AddContact");
    let block_wire = wire
        .call_wire(
            "charMgr",
            None,
            "BlockOwners",
            &[PyRep::List(vec![PyRep::Integer(90_000_003)])],
            &kw,
        )
        .await;
    assert!(block_wire.marshal_errors.is_empty());
    assert_eq!(
        block_wire
            .rpc_result
            .expect("BlockOwners")
            .dict_get("blockedCount"),
        Some(&PyRep::Integer(1))
    );

    let cases: [(&str, &str, Vec<PyRep>); 12] = [
        (
            "config",
            "GetMultiCorpTickerNamesEx",
            vec![PyRep::List(vec![PyRep::Integer(sample_npc_corp_id)])],
        ),
        (
            "map",
            "GetConstellationLPData",
            vec![PyRep::Integer(i64::from(jita_constellation_id))],
        ),
        (
            "map",
            "GetCurrentSovData",
            vec![PyRep::Integer(i64::from(jita_region_id))],
        ),
        ("map", "GetMyExtraMapInfoAgents", vec![]),
        ("map", "GetSecurityModifiedSystems", vec![]),
        ("planetMgr", "GetPlanetsForChar", vec![]),
        (
            "planetMgr",
            "GMGetSynchedServerState",
            vec![PyRep::Integer(jita_planet_id)],
        ),
        ("industryManager", "GetJob", vec![PyRep::Integer(93_311)]),
        (
            "industryManager",
            "GetJobsByOwner",
            vec![PyRep::Integer(char_id)],
        ),
        ("posMgr", "GetControlTowers", vec![]),
        ("charMgr", "GetOwnerNoteLabels", vec![]),
        ("charMgr", "GetContactList", vec![]),
    ];

    for (svc, method, args) in &cases {
        let dir_rep = ctx
            .dispatch_rpc(&mut dir_sess, svc, None, method, args, &kw, None)
            .await
            .unwrap_or_else(|e| panic!("direct {svc}.{method} failed: {e:?}"));
        let wire_out = wire.call_wire(svc, None, method, args, &kw).await;
        assert!(
            wire_out.marshal_errors.is_empty(),
            "{svc}.{method} marshal_errors: {:?}",
            wire_out.marshal_errors
        );
        let wire_rep = wire_out
            .rpc_result
            .unwrap_or_else(|e| panic!("wire {svc}.{method} failed: {e:?}"));
        assert_eq!(
            dir_rep, wire_rep,
            "{svc}.{method} must satisfy direct == wire natively"
        );
    }

    // Also confirm industry job status promotion (1 -> 3 Ready) and terminal failed invention job
    // (101 Delivered, successfulRuns=0, never promoted to 3) match direct == wire past end_unix.
    ctx.advance_secs(600);
    let extract_job = |rep: &PyRep, jid: i64| -> PyRep {
        match rep {
            PyRep::List(items) => items
                .iter()
                .find(|item| item.dict_get("jobID") == Some(&PyRep::Integer(jid)))
                .cloned()
                .unwrap_or_else(|| panic!("missing jobID {jid} in {items:?}")),
            _ => rep.clone(),
        }
    };
    for (method, arg, jid, expected_status, expected_ok_runs) in [
        ("GetJob", 93_311_i64, 93_311_i64, 3_i64, 1_i64),
        ("GetJob", 93_312_i64, 93_312_i64, 101_i64, 0_i64),
        ("GetJobsByOwner", char_id, 93_311_i64, 3_i64, 1_i64),
        ("GetJobsByOwner", char_id, 93_312_i64, 101_i64, 0_i64),
    ] {
        let args = [PyRep::Integer(arg)];
        let dir_rep = ctx
            .dispatch_rpc(&mut dir_sess, "industryManager", None, method, &args, &kw, None)
            .await
            .expect("direct job query");
        let wire_out = wire
            .call_wire("industryManager", None, method, &args, &kw)
            .await;
        assert!(wire_out.marshal_errors.is_empty());
        let wire_rep = wire_out.rpc_result.expect("wire job query");
        assert_eq!(dir_rep, wire_rep);
        let job_rep = extract_job(&dir_rep, jid);
        assert_eq!(job_rep.dict_get("status"), Some(&PyRep::Integer(expected_status)));
        assert_eq!(
            job_rep.dict_get("successfulRuns"),
            Some(&PyRep::Integer(expected_ok_runs))
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn arch_933_t3_migrated_rpcs_and_notifies_direct_equals_wire_native() {
    use crate::outbound_sequencer::OutFrame;
    use crate::router::client_wire::ClientWireSession;
    use crate::services::space_probe_scanning::tick_probe_warp_end;
    use eve_db::{
        CosmicSignatureRecord, InventoryFlag, InventoryItemRecord, KillmailRecord,
        VirtualServerClock,
    };
    use eve_machonet::service_roles;

    let base_unix = 1_800_000_000_u64;
    let char_id = 90_000_001_i64;
    let corp_id = 1_000_170_i64;
    let jita_sys_id = 30_000_142_u32;
    let ship_id = 93_320_001_i64;
    let launcher_id = 93_320_010_i64;
    let probe_stack_id = 93_320_011_i64;

    let setup_world = || -> (ServerContext, SessionState) {
        let ctx = test_ctx().with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
        ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.corporation_id = corp_id;
                ch.jump_cooldown_until_unix = base_unix + 600;
                ch.jump_fatigue_until_unix = base_unix + 3_600;
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = jita_sys_id;
                ch.active_ship_id = ship_id;
            }
            snap.character_last_jump_unix
                .insert(char_id, base_unix - 10);
            snap.killmails.insert(
                93_321,
                KillmailRecord {
                    kill_id: 93_321,
                    victim_character_id: 90_000_002,
                    attacker_character_id: char_id,
                    solar_system_id: jita_sys_id,
                    ship_type_id: 587,
                    total_value_isk: 500_000.0,
                    killed_unix: base_unix - 300,
                    victim_corp_id: corp_id,
                    attacker_corp_id: corp_id,
                    ..Default::default()
                },
            );
            snap.killmails.insert(
                93_322,
                KillmailRecord {
                    kill_id: 93_322,
                    victim_character_id: char_id,
                    attacker_character_id: 90_000_002,
                    solar_system_id: jita_sys_id,
                    ship_type_id: 587,
                    total_value_isk: 250_000.0,
                    killed_unix: base_unix - 120,
                    victim_corp_id: corp_id,
                    attacker_corp_id: corp_id,
                    ..Default::default()
                },
            );
            let mut ship = InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(jita_sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            };
            ship.dynamic_attributes.insert(48, 2_000.0);
            ship.dynamic_attributes.insert(11, 2_000.0);
            snap.items.insert(ship_id, ship);
            snap.items.insert(
                launcher_id,
                InventoryItemRecord {
                    item_id: launcher_id,
                    type_id: 17_901,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(launcher_id, true);
            snap.items.insert(
                probe_stack_id,
                InventoryItemRecord {
                    item_id: probe_stack_id,
                    type_id: 30_013,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 8,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.cosmic_signatures
                .entry(jita_sys_id)
                .or_default()
                .push(CosmicSignatureRecord {
                    signature_id: "SIG-933T".into(),
                    solar_system_id: jita_sys_id,
                    sig_group: "Relic Site".into(),
                    type_name: "Desolate Collapsed Conduit".into(),
                    position: [500_000.0, 2_000.0, 0.0],
                    created_unix: base_unix,
                    lifetime_seconds: 86_400,
                    ..Default::default()
                });
        });
        ctx.invalidate_ship_fitting(ship_id);
        ctx.db
            .acquire_online_lock(1, 93_320, Some(char_id), true, base_unix)
            .expect("online lock");
        let mut sess = SessionState::new(93_320);
        sess.user_id = Some(1);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);
        sess.solar_system_id = Some(jita_sys_id);
        sess.station_id = None;
        sess.ship_id = Some(ship_id);
        sess.role = service_roles::ROLE_PLAYER_DEFAULT;
        (ctx, sess)
    };

    let (ctx_dir, mut dir_sess) = setup_world();
    let (ctx_wire, wire_sess) = setup_world();
    for ctx_ref in [&ctx_dir, &ctx_wire] {
        let sys_h = ctx_ref.systems.get_or_spawn(jita_sys_id);
        sys_h
            .spawn_ball_with_meta(
                eve_destiny::Ball {
                    id: ship_id,
                    type_id: 587,
                    position: glam::DVec3::ZERO,
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Rigid,
                },
                ctx_ref.build_ship_combat_state(char_id, ship_id, 587),
                eve_destiny::SlimItemMeta {
                    group_id: 25,
                    category_id: 6,
                    owner_id: char_id,
                    char_id: Some(char_id),
                    corp_id,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                    ..Default::default()
                },
            )
            .await
            .expect("spawn probe ship ball");
    }
    let mut wire = ClientWireSession::from_session(&ctx_wire, wire_sess);
    let kw = HashMap::new();

    // 1. Read-only migrated RPCs: direct == wire natively.
    let read_cases: [(&str, &str, Vec<PyRep>); 6] = [
        ("jumpTimers", "GetTimers", vec![PyRep::Integer(char_id)]),
        ("charMgr", "GetRecentShipKillsAndLosses", vec![]),
        (
            "charUnboundMgr",
            "GetCharacterInfo",
            vec![PyRep::Integer(char_id)],
        ),
        ("charMgr", "GetCharacterInfo", vec![PyRep::Integer(char_id)]),
        (
            "corporationSvc",
            "GetEmployementRecordAndCharacterTransfers",
            vec![PyRep::Integer(char_id)],
        ),
        (
            "charMgr",
            "GetEmployementRecordAndCharacterTransfers",
            vec![PyRep::Integer(char_id)],
        ),
    ];
    for (svc, method, args) in &read_cases {
        let dir_rep = ctx_dir
            .dispatch_rpc(&mut dir_sess, svc, None, method, args, &kw, None)
            .await
            .unwrap_or_else(|e| panic!("direct {svc}.{method} failed: {e:?}"));
        let wire_out = wire.call_wire(svc, None, method, args, &kw).await;
        assert!(
            wire_out.marshal_errors.is_empty(),
            "{svc}.{method} marshal_errors: {:?}",
            wire_out.marshal_errors
        );
        let wire_rep = wire_out
            .rpc_result
            .unwrap_or_else(|e| panic!("wire {svc}.{method} failed: {e:?}"));
        assert_eq!(
            dir_rep, wire_rep,
            "{svc}.{method} must satisfy direct == wire natively"
        );
    }

    // 2. Launch 8 probes -> OnNewProbe direct == wire with native scanBonuses.
    let _ = ctx_dir.drain_notifications_for_char(char_id);
    let _ = wire.drain_pending_frames();
    let launch_args = [
        PyRep::Integer(launcher_id),
        PyRep::String("launchProbe".into()),
    ];
    let dir_launch = ctx_dir
        .dispatch_rpc(
            &mut dir_sess,
            "dogmaIM",
            None,
            "Activate",
            &launch_args,
            &kw,
            None,
        )
        .await
        .expect("direct launchProbe");
    let wire_launch = wire
        .call_wire("dogmaIM", None, "Activate", &launch_args, &kw)
        .await;
    assert!(wire_launch.marshal_errors.is_empty());
    assert_eq!(
        dir_launch,
        wire_launch.rpc_result.expect("wire launchProbe")
    );
    let dir_new_probes: Vec<Vec<PyRep>> = ctx_dir
        .drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n| n.event == "OnNewProbe")
        .map(|n| n.args)
        .collect();
    let wire_new_probes: Vec<Vec<PyRep>> = wire_launch
        .notifications
        .into_iter()
        .filter(|(ev, _)| ev == "OnNewProbe")
        .map(|(_, args)| args)
        .collect();
    assert_eq!(dir_new_probes.len(), 8);
    assert_eq!(dir_new_probes, wire_new_probes);
    assert!(dir_new_probes[0][0].dict_get("scanBonuses").is_some());

    // 3. RequestScans + advance + tick_probe_warp_end -> OnSystemScanStopped direct == wire natively.
    let coords = PyRep::List(
        (0..8)
            .map(|i| {
                PyRep::List(vec![
                    PyRep::Real(500_000.0 + (i as f64) * 500.0),
                    PyRep::Real(2_000.0),
                    PyRep::Real(0.0),
                    PyRep::Integer(2),
                ])
            })
            .collect(),
    );
    ctx_dir
        .dispatch_rpc(
            &mut dir_sess,
            "scanMgr",
            None,
            "RequestScans",
            &[coords.clone()],
            &kw,
            None,
        )
        .await
        .expect("direct RequestScans");
    let scan_wire = wire
        .call_wire("scanMgr", None, "RequestScans", &[coords], &kw)
        .await;
    assert!(scan_wire.marshal_errors.is_empty());
    assert!(scan_wire.rpc_result.is_ok());

    let _ = ctx_dir.drain_notifications_for_char(char_id);
    let _ = wire.drain_pending_frames();
    ctx_dir.advance_secs(60);
    ctx_wire.advance_secs(60);
    tick_probe_warp_end(&ctx_dir);
    tick_probe_warp_end(&ctx_wire);

    let dir_scan_stopped: Vec<Vec<PyRep>> = ctx_dir
        .drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n| n.event == "OnSystemScanStopped")
        .map(|n| n.args)
        .collect();
    let (wire_frames, _) = wire.drain_pending_frames();
    let wire_scan_stopped: Vec<Vec<PyRep>> = wire_frames
        .into_iter()
        .filter_map(|f| match f {
            OutFrame::Notification { event, args } if event == "OnSystemScanStopped" => Some(args),
            _ => None,
        })
        .collect();
    let sort_int_list = |rep: &PyRep| -> Vec<i64> {
        let PyRep::List(items) = rep else {
            panic!("expected List of probe IDs, got {rep:?}");
        };
        let mut ids: Vec<i64> = items
            .iter()
            .map(|v| v.as_i64().expect("integer probe ID"))
            .collect();
        ids.sort_unstable();
        ids
    };
    assert_eq!(dir_scan_stopped.len(), 1);
    assert_eq!(wire_scan_stopped.len(), 1);
    assert_eq!(dir_scan_stopped[0].len(), 3);
    assert_eq!(wire_scan_stopped[0].len(), 3);
    assert_eq!(
        sort_int_list(&dir_scan_stopped[0][0]),
        sort_int_list(&wire_scan_stopped[0][0])
    );
    assert_eq!(dir_scan_stopped[0][1..], wire_scan_stopped[0][1..]);
    let PyRep::List(scan_results) = &dir_scan_stopped[0][1] else {
        panic!("expected OnSystemScanStopped results list");
    };
    assert!(!scan_results.is_empty());
    assert!(scan_results[0].dict_get("groupID").is_some());
    assert!(scan_results[0].dict_get("typeID").is_some());
    assert!(matches!(
        scan_results[0].dict_get("data"),
        Some(PyRep::Tuple(t)) if t.len() == 3
    ));

    // 4. scanMgr.RecoverProbes returns native PyRep::List(probe_ids) with direct == wire.
    let dir_recover = ctx_dir
        .dispatch_rpc(&mut dir_sess, "scanMgr", None, "RecoverProbes", &[], &kw, None)
        .await
        .expect("direct RecoverProbes");
    let wire_recover_out = wire.call_wire("scanMgr", None, "RecoverProbes", &[], &kw).await;
    assert!(wire_recover_out.marshal_errors.is_empty());
    let wire_recover = wire_recover_out.rpc_result.expect("wire RecoverProbes");
    let dir_rec_ids = sort_int_list(&dir_recover);
    let wire_rec_ids = sort_int_list(&wire_recover);
    assert_eq!(dir_rec_ids.len(), 8);
    assert_eq!(dir_rec_ids, wire_rec_ids);
}

#[tokio::test]
async fn arch_933_t7b_canonical_emitters_shape_checked_and_compat_noop() {
    use crate::router::notify_registry::all_registered_notifications;
    use crate::router::notify_shape_check::check_notification_shape;
    use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents};
    use eve_machonet::service_roles;
    use std::collections::HashSet;

    let ctx = test_ctx();
    let char_a = 90_000_001_i64;
    let char_b = 90_000_002_i64;
    let corp_id = 98_933_701_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let trade_item_id = 933_700_001_i64;

    let (uid_a, uid_b) = ctx.db.with_mut_snapshot(|snap| {
        let mut ca = snap.characters[&char_a].clone();
        ca.corporation_id = corp_id;
        ca.station_id = Some(station_id);
        ca.solar_system_id = sys_id;
        ca.balance = IskCents::from_isk(100_000);
        let uid_a = ca.user_id;
        snap.characters.insert(char_a, ca.clone());

        let uid_b = snap.characters.get(&char_b).map_or(502, |c| c.user_id);
        let mut cb = snap.characters.get(&char_b).cloned().unwrap_or(ca);
        cb.character_id = char_b;
        cb.user_id = uid_b;
        cb.name = "TradePartner".into();
        cb.corporation_id = corp_id;
        cb.station_id = Some(station_id);
        cb.solar_system_id = sys_id;
        cb.balance = IskCents::from_isk(100_000);
        snap.characters.insert(char_b, cb);

        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "T7b Corp".into();
        corp.ticker = "T7B".into();
        corp.ceo_id = char_a;
        corp.members = HashSet::from([char_a, char_b]);
        corp.member_roles = HashMap::from([(char_a, u64::MAX)]);
        corp.hq_station_id = Some(station_id);
        corp.set_division_balance(1, IskCents::from_isk(1_000_000));
        snap.corporations.insert(corp_id, corp);

        snap.items.insert(
            trade_item_id,
            InventoryItemRecord {
                item_id: trade_item_id,
                type_id: 34,
                owner_id: char_a,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        (uid_a, uid_b)
    });

    let now = ctx.now_unix();
    ctx.db
        .acquire_online_lock(uid_a, 93_371, Some(char_a), false, now)
        .expect("online lock A");
    ctx.db
        .acquire_online_lock(uid_b, 93_372, Some(char_b), false, now)
        .expect("online lock B");

    let mut sess_a = SessionState::new(93_371);
    sess_a.user_id = Some(uid_a);
    sess_a.character_id = Some(char_a);
    sess_a.corporation_id = Some(corp_id);
    sess_a.solar_system_id = Some(sys_id);
    sess_a.station_id = Some(station_id);
    sess_a.role = service_roles::ROLE_PLAYER_DEFAULT;

    let mut sess_b = SessionState::new(93_372);
    sess_b.user_id = Some(uid_b);
    sess_b.character_id = Some(char_b);
    sess_b.corporation_id = Some(corp_id);
    sess_b.solar_system_id = Some(sys_id);
    sess_b.station_id = Some(station_id);
    sess_b.role = service_roles::ROLE_PLAYER_DEFAULT;

    let kw = HashMap::new();
    ctx.drain_notifications_for_char(char_a);
    ctx.drain_notifications_for_char(char_b);

    // 1. OnChannelJoined (sync_character_local_chat + LSC.JoinChannel)
    ctx.sync_character_local_chat(char_a, None, sys_id);
    ctx.dispatch_rpc(
        &mut sess_a,
        "LSC",
        None,
        "JoinChannel",
        &[PyRep::String("channel:t7b-room".into())],
        &kw,
        None,
    )
    .await
    .expect("LSC.JoinChannel");

    // 2. OnCorporationRecruitmentAdChanged (corpRecProxy.CreateRecruitmentAd)
    ctx.dispatch_rpc(
        &mut sess_a,
        "corpRecProxy",
        None,
        "CreateRecruitmentAd",
        &[
            PyRep::String("T7b Pilots".into()),
            PyRep::String("Canonical ad".into()),
            PyRep::Integer(7),
        ],
        &kw,
        None,
    )
    .await
    .expect("CreateRecruitmentAd");

    // 3. OnOrganizationContactsUpdated (corpRegistry.AddCorporateContact)
    ctx.dispatch_rpc(
        &mut sess_a,
        "corpRegistry",
        None,
        "AddCorporateContact",
        &[PyRep::Integer(char_b), PyRep::Real(5.0), PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect("AddCorporateContact");

    // 4 & 5. OnTradeOffer & OnTradeMoneyOffer (trademgr.InitiateTrade, AddItem, OfferMoney, MakeOffer)
    let init_bid = ctx
        .dispatch_rpc(
            &mut sess_a,
            "trademgr",
            None,
            "InitiateTrade",
            &[PyRep::Integer(char_b)],
            &kw,
            None,
        )
        .await
        .expect("InitiateTrade")
        .as_i64()
        .expect("bound trade id");
    let trade_id = ctx
        .dispatch_rpc(
            &mut sess_a,
            "trademgr",
            None,
            "List",
            &[PyRep::Integer(init_bid)],
            &kw,
            None,
        )
        .await
        .expect("trademgr.List")
        .dict_get("tradeContainerID")
        .and_then(PyRep::as_i64)
        .expect("tradeContainerID");
    ctx.dispatch_rpc(
        &mut sess_a,
        "trademgr",
        None,
        "AddItem",
        &[
            PyRep::Integer(trade_id),
            PyRep::Integer(trade_item_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("AddItem");
    ctx.dispatch_rpc(
        &mut sess_b,
        "trademgr",
        None,
        "OfferMoney",
        &[PyRep::Integer(trade_id), PyRep::Real(2_500.0)],
        &kw,
        None,
    )
    .await
    .expect("OfferMoney");
    ctx.dispatch_rpc(
        &mut sess_b,
        "trademgr",
        None,
        "MakeOffer",
        &[PyRep::Integer(trade_id), PyRep::Bool(true)],
        &kw,
        None,
    )
    .await
    .expect("MakeOffer");

    let mut notifs = ctx.drain_notifications_for_char(char_a);
    notifs.extend(ctx.drain_notifications_for_char(char_b));

    let registry = all_registered_notifications();
    let target_events = [
        "OnChannelJoined",
        "OnCorporationRecruitmentAdChanged",
        "OnOrganizationContactsUpdated",
        "OnTradeOffer",
        "OnTradeMoneyOffer",
    ];
    for ev in target_events {
        let entry = registry
            .iter()
            .find(|e| e.event == ev)
            .unwrap_or_else(|| panic!("missing registry entry for {ev}"));
        let matched: Vec<_> = notifs.iter().filter(|n| n.event == ev).collect();
        assert!(
            !matched.is_empty(),
            "expected at least one handler-emitted {ev} notification"
        );
        for n in matched {
            check_notification_shape(entry, &n.args)
                .unwrap_or_else(|e| panic!("{ev} failed notify_shape_check: {e}"));
            let normalized =
                normalize_macho_notification_with_sde(&n.event, n.args.clone(), Some(&ctx.sde));
            assert_eq!(
                normalized, n.args,
                "{ev} args mutated across normalize_macho_notification_with_sde"
            );
            let recipient_uid = if n.recipient_char_id == char_a {
                Some(uid_a)
            } else {
                Some(uid_b)
            };
            let (_, re_encoded) = ServerContext::build_macho_notification_packet(
                n.recipient_char_id,
                recipient_uid,
                &n.event,
                &n.id_type,
                normalized,
            );
            assert_eq!(
                re_encoded, n.marshaled_packet,
                "{ev} wire bytes mutated across normalize_macho_notification_with_sde"
            );
        }
    }
}
