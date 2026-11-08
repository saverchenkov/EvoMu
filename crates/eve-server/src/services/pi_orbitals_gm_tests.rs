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
