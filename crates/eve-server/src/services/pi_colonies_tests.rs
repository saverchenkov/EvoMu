use super::*;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository};
use eve_sde::{PlanetSchematicInfo, PlanetSchematicTypeEntry, SdeRegistry};
use std::collections::HashMap;
use std::sync::Arc;

fn make_ctx() -> ServerContext {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.planet_schematics.insert(
        65,
        PlanetSchematicInfo {
            schematic_id: 65,
            name: "Water".into(),
            cycle_time: 1800,
            types: vec![
                PlanetSchematicTypeEntry {
                    type_id: 2268,
                    quantity: 3000,
                    is_input: true,
                },
                PlanetSchematicTypeEntry {
                    type_id: 3645,
                    quantity: 20,
                    is_input: false,
                },
            ],
            pins: vec![2469],
        },
    );

    let db = Arc::new(WorldRepository::with_demo_seed());
    db.acquire_online_lock(1, 501, Some(90_000_001), true, 1_775_400_000)
        .unwrap();
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

#[test]
fn test_pi_colonies_user_update_network_queries_and_abandon() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let planet_id = 40_009_080; // Jita III (Temperate planet)
    let sys_id = 30_000_142;
    let ship_id = 600_001;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.skills.entry(char_id).or_default().insert(
            2505,
            TrainedSkillRecord {
                type_id: 2505,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.items.insert(
            600_002,
            InventoryItemRecord {
                item_id: 600_002,
                type_id: 2254, // Temperate Command Center
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.ship_id = Some(ship_id);
    session.solar_system_id = Some(sys_id);

    // 1. Deploy Command Center (cmd 1), Upgrade to lvl 3 (cmd 9), Place ECU (cmd 1) & Factory (cmd 1),
    //    Create Link (cmd 3), Upgrade Link (cmd 5), Set Schematic (cmd 7/8), Install Program (cmd 10),
    //    Move Head (cmd 14), Create Route (cmd 6)
    let cmds = PyRep::List(vec![
        // Place CC (client pin 1)
        PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(2254),
            PyRep::Real(0.10),
            PyRep::Real(0.10),
        ]),
        // Upgrade CC to level 3
        PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(1), PyRep::Integer(3)]),
        // Place ECU (client pin 2)
        PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Integer(2),
            PyRep::Integer(2848),
            PyRep::Real(0.12),
            PyRep::Real(0.12),
        ]),
        // Place Basic Factory (client pin 3)
        PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Integer(3),
            PyRep::Integer(2469),
            PyRep::Real(0.14),
            PyRep::Real(0.14),
        ]),
        // Create Link between ECU (2) and Factory (3) at level 0
        PyRep::Tuple(vec![
            PyRep::Integer(3),
            PyRep::Integer(2),
            PyRep::Integer(3),
            PyRep::Integer(0),
        ]),
        // Upgrade Link between 2 and 3 to level 2
        PyRep::Tuple(vec![
            PyRep::Integer(5),
            PyRep::Integer(2),
            PyRep::Integer(3),
            PyRep::Integer(2),
        ]),
        // Set schematic 65 on Factory (3)
        PyRep::Tuple(vec![PyRep::Integer(8), PyRep::Integer(3), PyRep::Integer(65)]),
        // Install extractor program on ECU (2)
        PyRep::Tuple(vec![
            PyRep::Integer(10),
            PyRep::Integer(2),
            PyRep::Integer(2268),
            PyRep::Integer(300),
            PyRep::Integer(2),
        ]),
        // Move head on ECU (2)
        PyRep::Tuple(vec![
            PyRep::Integer(14),
            PyRep::Integer(2),
            PyRep::Integer(1),
            PyRep::Real(0.13),
            PyRep::Real(0.13),
        ]),
        // Create route from ECU (2) to Factory (3)
        PyRep::Tuple(vec![
            PyRep::Integer(6),
            PyRep::Integer(100),
            PyRep::List(vec![PyRep::Integer(2), PyRep::Integer(3)]),
            PyRep::Integer(2268),
            PyRep::Integer(500),
        ]),
    ]);

    let updated = handle_pi_colonies(
        &ctx,
        &mut session,
        "planetMgr",
        "UserUpdateNetwork",
        &[PyRep::Integer(planet_id), cmds],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .expect("UserUpdateNetwork should succeed");
    assert_eq!(updated.dict_get("commandCenterLevel").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(updated.dict_get("pinsCount").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(updated.dict_get("linksCount").and_then(|v| v.as_i64()), Some(1));

    // Query GetColony, GetFullNetworkForOwner, GetCommandPinsForPlanet, GetExtractorsForPlanet, GetCommandCenterLevel, GetPin
    let col = handle_pi_colonies(
        &ctx,
        &mut session,
        "planetMgr",
        "GetColony",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(col.dict_get("planetID").and_then(|v| v.as_i64()), Some(planet_id));

    let full = handle_pi_colonies(
        &ctx,
        &mut session,
        "planetMgr",
        "GetFullNetworkForOwner",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(full.dict_get("commandCenterLevel").and_then(|v| v.as_i64()), Some(3));

    let cmd_pins = handle_pi_colonies(
        &ctx,
        &mut session,
        "planetMgr",
        "GetCommandPinsForPlanet",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(cmd_pins, PyRep::List(v) if v.len() == 1));

    let ext_pins = handle_pi_colonies(
        &ctx,
        &mut session,
        "planetMgr",
        "GetExtractorsForPlanet",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    let ext_pin_id = match ext_pins {
        PyRep::List(v) => v[0].dict_get("pinID").and_then(|x| x.as_i64()).unwrap(),
        _ => panic!("expected list"),
    };

    let pin_rep = handle_pi_colonies(
        &ctx,
        &mut session,
        "planetMgr",
        "GetPin",
        &[PyRep::Integer(planet_id), PyRep::Integer(ext_pin_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(pin_rep.dict_get("pinID").and_then(|v| v.as_i64()), Some(ext_pin_id));

    let lvl_rep = handle_pi_colonies(
        &ctx,
        &mut session,
        "planetMgr",
        "GetCommandCenterLevel",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(lvl_rep, PyRep::Integer(3));

    // Abandon planet
    let aband = handle_pi_colonies(
        &ctx,
        &mut session,
        "planetMgr",
        "UserAbandonPlanet",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(aband, PyRep::Bool(true));
}

#[test]
fn install_program_depletes_prior_program_heads_instead_of_new_heads() {
    use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
    use crate::services::industry_pi::load_planet_depletion;
    use crate::services::industry_pi::pi_helpers::load_pi_network;

    let clock = std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_776_000_000));
    let ctx = test_ctx();
    ctx.set_clock(clock.clone());

    let (char_id, ship_id, planet_id, sys_id) =
        (90_000_001_i64, 100_000_001_i64, 40_009_080_i64, 30_000_142_u32);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            600_099,
            eve_db::InventoryItemRecord {
                item_id: 600_099,
                type_id: 2254,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        let sk = snap.skills.entry(char_id).or_default();
        for tid in [2505_u32, 2495_u32] {
            sk.insert(
                tid,
                eve_db::TrainedSkillRecord {
                    type_id: tid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    let mut sess = auth_session(1, char_id, 1_000_170, sys_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    let kw = HashMap::new();

    handle_pi_colonies(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(planet_id),
            PyRep::List(vec![
                PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(101),
                    PyRep::Integer(2254),
                    PyRep::Real(0.10),
                    PyRep::Real(0.10),
                ]),
                PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(102),
                    PyRep::Integer(2848),
                    PyRep::Real(0.12),
                    PyRep::Real(0.12),
                ]),
            ]),
        ],
        &kw,
        None,
    )
    .expect("UserUpdateNetwork handled")
    .expect("CC and ECU placement ok");

    let ecu_pin_id = ctx.db.with_snapshot(|snap| {
        load_pi_network(snap, char_id, planet_id, 0)
            .pins
            .into_iter()
            .find(|p| p.type_id == 2848)
            .expect("ECU pin must exist")
            .pin_id
    });

    let install_cmd = |hlat: f64, hlon: f64| {
        PyRep::Tuple(vec![
            PyRep::Integer(10),
            PyRep::Integer(ecu_pin_id),
            PyRep::Integer(2268),
            PyRep::Integer(900),
            PyRep::Integer(1),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Real(hlat),
                PyRep::Real(hlon),
            ])]),
            PyRep::Integer(86_400),
        ])
    };

    // Install program #1 with head at (0.02, 0.02).
    handle_pi_colonies(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[PyRep::Integer(planet_id), PyRep::List(vec![install_cmd(0.02, 0.02)])],
        &kw,
        None,
    )
    .expect("UserUpdateNetwork handled")
    .expect("initial InstallProgram ok");

    // Advance clock 6 hours without running GetColony so InstallProgram #2 flushes prior elapsed cycles.
    clock.advance_secs_and_read_offset(21_600);

    // Replace program with moved head at (0.25, -0.15).
    handle_pi_colonies(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[PyRep::Integer(planet_id), PyRep::List(vec![install_cmd(0.25, -0.15)])],
        &kw,
        None,
    )
    .expect("UserUpdateNetwork handled")
    .expect("replacement InstallProgram ok");

    ctx.db.with_snapshot(|snap| {
        let depl = load_planet_depletion(snap, planet_id);
        assert!(
            depl.local_depletion.iter().any(|spot| {
                spot.resource_type_id == 2268
                    && (spot.latitude - 0.02).abs() < 1e-9
                    && (spot.longitude - 0.02).abs() < 1e-9
                    && spot.amount_depleted > 0.0
            }),
            "prior program depletion must record prior head coordinates (0.02, 0.02): {:?}",
            depl.local_depletion
        );
        assert!(
            !depl.local_depletion.iter().any(|spot| {
                (spot.latitude - 0.25).abs() < 1e-6 && (spot.longitude + 0.15).abs() < 1e-6
            }),
            "new program head coordinates (0.25, -0.15) must not leak into prior program depletion: {:?}",
            depl.local_depletion
        );
        let net = load_pi_network(snap, char_id, planet_id, 0);
        let ecu = net.pins.iter().find(|p| p.pin_id == ecu_pin_id).unwrap();
        assert_eq!(ecu.extractor_heads, vec![(1, 0.25, -0.15)]);
    });
}

#[test]
fn rejects_non_finite_and_out_of_bounds_extractor_head_coords_without_corrupting_colony() {
    use crate::services::account_services::account_services_tests::{auth_session, test_ctx};

    let ctx = test_ctx();
    let (char_id, ship_id, planet_id, sys_id) =
        (90_000_001_i64, 100_000_001_i64, 40_009_080_i64, 30_000_142_u32);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            600_099,
            eve_db::InventoryItemRecord {
                item_id: 600_099,
                type_id: 2254,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        let sk = snap.skills.entry(char_id).or_default();
        for tid in [2505_u32, 2495_u32] {
            sk.insert(
                tid,
                eve_db::TrainedSkillRecord {
                    type_id: tid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    let mut sess = auth_session(1, char_id, 1_000_170, sys_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    let kw = HashMap::new();

    handle_pi_colonies(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(planet_id),
            PyRep::List(vec![
                PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(101),
                    PyRep::Integer(2254),
                    PyRep::Real(0.10),
                    PyRep::Real(0.10),
                ]),
                PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(102),
                    PyRep::Integer(2848),
                    PyRep::Real(0.12),
                    PyRep::Real(0.12),
                ]),
            ]),
        ],
        &kw,
        None,
    )
    .expect("UserUpdateNetwork handled")
    .expect("CC and ECU placement ok");

    let ecu_pin_id = ctx.db.with_snapshot(|snap| {
        load_pi_network(snap, char_id, planet_id, 0)
            .pins
            .into_iter()
            .find(|p| p.type_id == 2848)
            .expect("ECU pin must exist")
            .pin_id
    });

    handle_pi_colonies(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(planet_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(10),
                PyRep::Integer(ecu_pin_id),
                PyRep::Integer(2268),
                PyRep::Integer(900),
                PyRep::Integer(1),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Real(0.13),
                    PyRep::Real(0.13),
                ])]),
                PyRep::Integer(86_400),
            ])]),
        ],
        &kw,
        None,
    )
    .expect("UserUpdateNetwork handled")
    .expect("initial InstallProgram ok");

    let note_id = pi_network_note_id(char_id, planet_id);
    let baseline_note = ctx.db.with_snapshot(|snap| {
        let note = snap
            .owner_notes
            .get(&note_id)
            .expect("colony owner note must exist")
            .note
            .clone();
        assert!(
            serde_json::from_str::<PiColonyNetworkState>(&note).is_ok()
                && note.contains("\"extractor_heads\":[[1,0.13,0.13]]"),
            "baseline note must be valid: {note}"
        );
        let net = load_pi_network(snap, char_id, planet_id, 0);
        assert_eq!(net.pins.len(), 2);
        let ecu = net.pins.iter().find(|p| p.pin_id == ecu_pin_id).unwrap();
        assert_eq!(ecu.extractor_heads, vec![(1, 0.13, 0.13)]);
        note
    });

    let assert_rejected = |sess: &mut SessionState, cmds: Vec<PyRep>, label: &str| {
        let err = handle_pi_colonies(
            &ctx,
            sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id), PyRep::List(cmds)],
            &kw,
            None,
        )
        .expect("UserUpdateNetwork handled")
        .expect_err(label);
        assert_eq!(err.code, "InvalidPinCoordinates", "{label}");

        ctx.db.with_snapshot(|snap| {
            let note = snap
                .owner_notes
                .get(&note_id)
                .expect("colony note must remain intact")
                .note
                .clone();
            assert_eq!(note, baseline_note, "{label}: owner_note mutated");
            assert!(
                serde_json::from_str::<PiColonyNetworkState>(&note).is_ok(),
                "{label}: corrupted JSON"
            );
            let net = load_pi_network(snap, char_id, planet_id, 0);
            assert_eq!(net.pins.len(), 2, "{label}: colony pins reset");
            let ecu = net.pins.iter().find(|p| p.pin_id == ecu_pin_id).unwrap();
            assert_eq!(ecu.extractor_heads, vec![(1, 0.13, 0.13)], "{label}");
        });
    };

    let bad_coords = [
        ("nan_lat", f64::NAN, 0.12),
        ("nan_lon", 0.12, f64::NAN),
        ("pos_inf_lat", f64::INFINITY, 0.12),
        ("neg_inf_lat", f64::NEG_INFINITY, 0.12),
        ("pos_inf_lon", 0.12, f64::INFINITY),
        ("neg_inf_lon", 0.12, f64::NEG_INFINITY),
        ("lat_above_half_pi", 2.0, 0.12),
        ("lat_below_neg_half_pi", -2.0, 0.12),
        ("lon_above_pi", 0.12, 3.5),
        ("lon_below_neg_pi", 0.12, -3.5),
    ];

    for &(label, hlat, hlon) in &bad_coords {
        // cmd 10 with 3-tuple (head_id, lat, lon)
        assert_rejected(
            &mut sess,
            vec![PyRep::Tuple(vec![
                PyRep::Integer(10),
                PyRep::Integer(ecu_pin_id),
                PyRep::Integer(2268),
                PyRep::Integer(900),
                PyRep::Integer(1),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Real(hlat),
                    PyRep::Real(hlon),
                ])]),
                PyRep::Integer(86_400),
            ])],
            &format!("cmd10_3tuple_{label}"),
        );

        // cmd 10 with 2-tuple (lat, lon)
        assert_rejected(
            &mut sess,
            vec![PyRep::Tuple(vec![
                PyRep::Integer(10),
                PyRep::Integer(ecu_pin_id),
                PyRep::Integer(2268),
                PyRep::Integer(900),
                PyRep::Integer(1),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Real(hlat),
                    PyRep::Real(hlon),
                ])]),
                PyRep::Integer(86_400),
            ])],
            &format!("cmd10_2tuple_{label}"),
        );

        // cmd 14 (AddExtractorHead) and cmd 15 (MoveExtractorHead)
        for cmd_id in [14_i64, 15_i64] {
            let head_id = if cmd_id == 14 { 2 } else { 1 };
            assert_rejected(
                &mut sess,
                vec![PyRep::Tuple(vec![
                    PyRep::Integer(cmd_id),
                    PyRep::Integer(ecu_pin_id),
                    PyRep::Integer(head_id),
                    PyRep::Real(hlat),
                    PyRep::Real(hlon),
                ])],
                &format!("cmd{cmd_id}_{label}"),
            );
        }
    }

    // Non-numeric coordinate entries in cmd 10 and cmd 14 must also fail cleanly.
    assert_rejected(
        &mut sess,
        vec![PyRep::Tuple(vec![
            PyRep::Integer(10),
            PyRep::Integer(ecu_pin_id),
            PyRep::Integer(2268),
            PyRep::Integer(900),
            PyRep::Integer(1),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::None,
                PyRep::Real(0.12),
            ])]),
            PyRep::Integer(86_400),
        ])],
        "cmd10_none_lat",
    );
    assert_rejected(
        &mut sess,
        vec![PyRep::Tuple(vec![
            PyRep::Integer(14),
            PyRep::Integer(ecu_pin_id),
            PyRep::Integer(2),
            PyRep::None,
            PyRep::Real(0.12),
        ])],
        "cmd14_none_lat",
    );

    // Transactional rollback: an earlier valid head placement in the same batch must not commit.
    assert_rejected(
        &mut sess,
        vec![
            PyRep::Tuple(vec![
                PyRep::Integer(14),
                PyRep::Integer(ecu_pin_id),
                PyRep::Integer(2),
                PyRep::Real(0.14),
                PyRep::Real(0.14),
            ]),
            PyRep::Tuple(vec![
                PyRep::Integer(14),
                PyRep::Integer(ecu_pin_id),
                PyRep::Integer(3),
                PyRep::Real(f64::NAN),
                PyRep::Real(0.12),
            ]),
        ],
        "batch_rollback_cmd14_then_nan",
    );
}

#[cfg(test)]
#[path = "pi_colonies_extra_unit_tests.rs"]
mod extra_unit_tests;
