use super::*;
use crate::router::ServerContext;
use eve_db::{PlanetaryColonyRecord, WorldRepository};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::Arc;

fn make_ctx() -> ServerContext {
    let sde = SdeRegistry::with_canonical_seed();
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

#[test]
fn test_pi_orbitals_gm_rpcs_and_access_checks() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let planet_id = 40_009_082;

    ctx.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: 30_000_142,
                command_center_level: 1,
                pins_count: 1,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 1000,
            },
        );
        let net = PiColonyNetworkState {
            cpu_max: 7057.0,
            pg_max: 9000.0,
            pins: vec![PiPinState {
                pin_id: 501,
                type_id: 2254, // Command Center
                ..Default::default()
            }],
            ..Default::default()
        };
        save_pi_network(snap, char_id, planet_id, &net);
    });

    let mut non_gm = SessionState::default();
    non_gm.user_id = Some(1);
    non_gm.character_id = Some(char_id);
    non_gm.role = 0;

    for method in [
        "GMAddCommodity",
        "GMConvertCommandCenter",
        "GMForceInstallProgram",
        "GMVerifySimulation",
        "GMGetLocalDistributionReport",
        "GMGetSynchedServerState",
        "GMRunDepletionSim",
        "GMCreateNuggetLayer",
        "GMGetCompleteResource",
    ] {
        let err = handle_pi_orbitals_gm(
            &ctx,
            &mut non_gm,
            "planetMgr",
            method,
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap_err();
        assert!(err.code.eq_ignore_ascii_case("adminaccessdenied"));
    }

    let mut gm = non_gm;
    gm.role = eve_db::service_roles::ROLE_GMH;

    let add_ok = handle_pi_orbitals_gm(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMAddCommodity",
        &[
            PyRep::Integer(planet_id),
            PyRep::Integer(501),
            PyRep::Integer(2268),
            PyRep::Integer(500),
        ],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(add_ok, PyRep::Bool(true));

    let conv_ok = handle_pi_orbitals_gm(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMConvertCommandCenter",
        &[PyRep::Integer(planet_id), PyRep::Integer(2524), PyRep::Integer(4)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(conv_ok, PyRep::Bool(true));

    let force_ok = handle_pi_orbitals_gm(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMForceInstallProgram",
        &[
            PyRep::Integer(planet_id),
            PyRep::Integer(501),
            PyRep::Integer(2268),
            PyRep::Integer(4),
        ],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(force_ok, PyRep::Bool(true));

    let verify = handle_pi_orbitals_gm(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMVerifySimulation",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(verify.dict_get("valid").and_then(|v| v.as_bool()), Some(true));

    let dist = handle_pi_orbitals_gm(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMGetLocalDistributionReport",
        &[PyRep::Integer(planet_id), PyRep::Real(0.1), PyRep::Real(0.2)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dist.dict_get("planetID").and_then(|v| v.as_i64()), Some(planet_id));

    let synched = handle_pi_orbitals_gm(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMGetSynchedServerState",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(synched.dict_get("colonyCount").and_then(|v| v.as_i64()), Some(1));

    let depl = handle_pi_orbitals_gm(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMRunDepletionSim",
        &[PyRep::Integer(planet_id), PyRep::Integer(10)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(depl.dict_get("durationHours").and_then(|v| v.as_i64()), Some(10));

    let before_density = ctx
        .db
        .with_snapshot(|snap| compute_planet_resource_density(&ctx, snap, planet_id, 2268, 0.25, 0.5));
    assert_eq!(
        handle_pi_orbitals_gm(
            &ctx,
            &mut gm,
            "planetMgr",
            "GMCreateNuggetLayer",
            &[
                PyRep::Integer(planet_id),
                PyRep::Integer(2268),
                PyRep::Real(0.25),
                PyRep::Real(0.5),
                PyRep::Real(0.4),
                PyRep::Real(0.35),
            ],
            &HashMap::new(),
            None
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    let after_density = ctx
        .db
        .with_snapshot(|snap| compute_planet_resource_density(&ctx, snap, planet_id, 2268, 0.25, 0.5));
    assert!(
        after_density > before_density,
        "GMCreateNuggetLayer must boost density: before={before_density}, after={after_density}"
    );
    assert_eq!(
        handle_pi_orbitals_gm(&ctx, &mut gm, "planetMgr", "GMCreateNuggetLayer", &[], &HashMap::new(), None)
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    let comp = handle_pi_orbitals_gm(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMGetCompleteResource",
        &[PyRep::Integer(planet_id), PyRep::Integer(2268)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(comp.dict_get("numBands").and_then(|v| v.as_i64()), Some(3));
    assert_eq!(comp.dict_get("planetID").and_then(|v| v.as_i64()), Some(planet_id));
    let Some(PyRep::Bytes(sh_bytes)) = comp.dict_get("data") else {
        panic!("GMGetCompleteResource data must be Bytes");
    };
    assert_eq!(sh_bytes.len(), 36);
    assert!(
        sh_bytes.iter().any(|&b| b != 0),
        "GMGetCompleteResource must return real non-zero SH coefficients"
    );
}

#[test]
fn test_pi_orbitals_gm_edge_cases_bound_objects_and_error_branches() {
    let ctx = make_ctx();
    let char_id = 90_000_001_i64;
    let planet_id = 40_009_082_i64;

    // 1. Unauthenticated fails require_character; unknown service/method returns None
    let mut unauth = SessionState::default();
    for meth in [
        "GMAddCommodity",
        "GMConvertCommandCenter",
        "GMForceInstallProgram",
        "GMVerifySimulation",
        "GMGetLocalDistributionReport",
        "GMGetSynchedServerState",
        "GMRunDepletionSim",
        "GMCreateNuggetLayer",
        "GMGetCompleteResource",
    ] {
        assert!(handle_pi_orbitals_gm(
            &ctx,
            &mut unauth,
            "planetMgr",
            meth,
            &[],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .is_err());
    }
    let mut gm = SessionState::default();
    gm.user_id = Some(1);
    gm.character_id = Some(char_id);
    gm.role = eve_db::service_roles::ROLE_GMH;

    assert!(handle_pi_orbitals_gm(
        &ctx,
        &mut gm,
        "planetMgr",
        "__unknown_method__",
        &[],
        &HashMap::new(),
        None,
    )
    .is_none());
    assert!(handle_pi_orbitals_gm(
        &ctx,
        &mut gm,
        "__other_svc__",
        "GMAddCommodity",
        &[],
        &HashMap::new(),
        None,
    )
    .is_none());

    // 2. Missing planetID (no arg, no bound_planet) -> InvalidPlanetID for all planet-requiring methods
    let missing_planet_err = ["Invalid", "PlanetID"].concat();
    for meth in [
        "GMAddCommodity",
        "GMConvertCommandCenter",
        "GMForceInstallProgram",
        "GMVerifySimulation",
        "GMGetLocalDistributionReport",
        "GMGetSynchedServerState",
        "GMRunDepletionSim",
    ] {
        let err = handle_pi_orbitals_gm(
            &ctx,
            &mut gm,
            "planetMgr",
            meth,
            &[],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, missing_planet_err, "expected InvalidPlanetID for {meth}");
    }

    // 3. Unknown planet in SDE -> InvalidPlanetID for GMGetLocalDistributionReport
    let bad_planet_err = handle_pi_orbitals_gm(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMGetLocalDistributionReport",
        &[PyRep::Integer(999_999_999)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_planet_err.code, "InvalidPlanetID");

    // 4. Missing colony -> GMConvertCommandCenter & GMForceInstallProgram return Bool(false);
    // GMGetSynchedServerState returns lastTick = 0
    assert_eq!(
        handle_pi_orbitals_gm(
            &ctx,
            &mut gm,
            "planetMgr",
            "GMConvertCommandCenter",
            &[PyRep::Integer(planet_id), PyRep::Integer(3)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle_pi_orbitals_gm(
            &ctx,
            &mut gm,
            "planetMgr",
            "GMForceInstallProgram",
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    let synch_empty = handle_pi_orbitals_gm(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMGetSynchedServerState",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(synch_empty.dict_get("lastTick").and_then(|v| v.as_i64()), Some(0));

    // 5. GMAddCommodity 3-arg form (without colony) creates inventory item and subsequent call stacks quantity
    assert_eq!(
        handle_pi_orbitals_gm(
            &ctx,
            &mut gm,
            "planetMgr",
            "GMAddCommodity",
            &[PyRep::Integer(planet_id), PyRep::Integer(2268), PyRep::Integer(100)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle_pi_orbitals_gm(
            &ctx,
            &mut gm,
            "planetMgr",
            "GMAddCommodity",
            &[PyRep::Integer(planet_id), PyRep::Integer(2268), PyRep::Integer(50)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        let it = snap
            .items
            .values()
            .find(|i| i.owner_id == char_id && i.location_id == planet_id && i.type_id == 2268)
            .expect("stacked commodity item");
        assert_eq!(it.quantity, 150);
        assert_eq!(it.version, 2);
    });

    // 6. Seed colony with empty pins -> load_pi_network synthesizes default CC pin (pins[0]),
    // so GMForceInstallProgram falls back to Some(0), installs default resource/heads, and returns true.
    ctx.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: 30_000_142,
                command_center_level: 1,
                pins_count: 0,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 555,
            },
        );
        save_pi_network(snap, char_id, planet_id, &PiColonyNetworkState::default());
    });
    assert_eq!(
        handle_pi_orbitals_gm(
            &ctx,
            &mut gm,
            "planetMgr",
            "GMForceInstallProgram",
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        let net = load_pi_network(snap, char_id, planet_id, 1);
        assert_eq!(net.pins.len(), 1);
        assert_eq!(net.pins[0].extractor_resource_type_id, Some(2268));
        assert_eq!(net.pins[0].extractor_head_count, 3);
    });

    // 7. Seed colony with CC + extractor pin (2848) + broken link + overloaded CPU/PG
    ctx.db.with_mut_snapshot(|snap| {
        let net = PiColonyNetworkState {
            cpu_used: 99_999.0,
            cpu_max: 5000.0,
            pg_used: 99_999.0,
            pg_max: 5000.0,
            pins: vec![
                PiPinState {
                    pin_id: 10,
                    type_id: 2254,
                    ..Default::default()
                },
                PiPinState {
                    pin_id: 20,
                    type_id: 2848, // Extractor Control Unit
                    ..Default::default()
                },
            ],
            links: vec![PiLinkState {
                source_pin_id: 10,
                dest_pin_id: 999, // missing dest pin
                level: 0,
                length_km: 1.0,
                cpu_usage: 0.0,
                pg_usage: 0.0,
                bandwidth: 0.0,
            }],
            ..Default::default()
        };
        save_pi_network(snap, char_id, planet_id, &net);
    });

    // GMVerifySimulation reports cpuOk=false, powergridOk=false, linksOk=false, valid=false
    let bad_sim = handle_pi_orbitals_gm(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMVerifySimulation",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(bad_sim.dict_get("valid").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(bad_sim.dict_get("cpuOk").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(bad_sim.dict_get("powergridOk").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(bad_sim.dict_get("linksOk").and_then(|v| v.as_bool()), Some(false));

    // GMConvertCommandCenter 2-arg form (without target_type_id) updates level & caps
    assert_eq!(
        handle_pi_orbitals_gm(
            &ctx,
            &mut gm,
            "planetMgr",
            "GMConvertCommandCenter",
            &[PyRep::Integer(planet_id), PyRep::Integer(5)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.planetary_colonies[&(char_id, planet_id)].command_center_level,
            5
        );
        let net = load_pi_network(snap, char_id, planet_id, 5);
        assert_eq!(net.cpu_max, cc_level_cpu_pg(5).0);
    });

    // GMForceInstallProgram with pin_id = 0 falls back to extractor pin 20 (type 2848)
    assert_eq!(
        handle_pi_orbitals_gm(
            &ctx,
            &mut gm,
            "planetMgr",
            "GMForceInstallProgram",
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // Test bound_planet with GMRunDepletionSim (omitting duration -> default 24) & GMGetLocalDistributionReport
    let bound = crate::router::BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: 1,
        character_id: char_id,
        service: "planetMgr".into(),
        object_id: planet_id,
        oid_str: String::new(),
        ref_id: 1,
    };
    let depl_bound = handle_pi_orbitals_gm(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMRunDepletionSim",
        &[],
        &HashMap::new(),
        Some(&bound),
    )
    .unwrap()
    .unwrap();
    assert_eq!(depl_bound.dict_get("durationHours").and_then(|v| v.as_i64()), Some(24));
    assert_eq!(depl_bound.dict_get("totalDuration").and_then(|v| v.as_i64()), Some(24 * 3600));
    let Some(PyRep::List(pins_hist)) = depl_bound.dict_get("pins") else {
        panic!("expected pins list");
    };
    assert_eq!(pins_hist.len(), 1);
    assert_eq!(pins_hist[0].dict_get("pinID").and_then(|v| v.as_i64()), Some(20));

    let dist_bound = handle_pi_orbitals_gm(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMGetLocalDistributionReport",
        &[PyRep::Real(0.5), PyRep::Real(1.0)],
        &HashMap::new(),
        Some(&bound),
    )
    .unwrap()
    .unwrap();
    assert_eq!(dist_bound.dict_get("planetID").and_then(|v| v.as_i64()), Some(planet_id));
    assert_eq!(dist_bound.dict_get("characterID").and_then(|v| v.as_i64()), Some(char_id));
}
