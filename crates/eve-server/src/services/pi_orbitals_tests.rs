use super::*;
use crate::router::ServerContext;
use eve_db::{
    InventoryFlag, InventoryItemRecord, PlanetaryColonyRecord, TrainedSkillRecord, WorldRepository,
};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::Arc;

fn make_ctx() -> ServerContext {
    let sde = SdeRegistry::with_canonical_seed();
    let db = Arc::new(WorldRepository::with_demo_seed());
    db.acquire_online_lock(1, 501, Some(90_000_001), true, 1_775_400_000)
        .unwrap();
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

#[test]
fn test_pi_commodity_tier_base_cost_and_parse_extractor_heads() {
    let ctx = make_ctx();
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 2268), 5.0);
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 3645), 400.0);
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 2329), 7_200.0);
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 2344), 60_000.0);
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 2867), 1_200_000.0);
    assert_eq!(pi_commodity_tier_base_cost(&ctx.sde, 999_999), 400.0);

    let rep = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Real(0.1), PyRep::Real(0.2)]),
        PyRep::Tuple(vec![PyRep::Real(0.3), PyRep::Real(0.4)]),
        PyRep::Integer(25),
    ]);
    let parsed = parse_extractor_heads_rep(Some(&rep)).unwrap();
    assert_eq!(parsed, vec![(10, 0.1, 0.2), (1, 0.3, 0.4), (25, 0.0, 0.0)]);
    assert!(parse_extractor_heads_rep(None).is_none());
}

#[tokio::test]
async fn test_pi_orbitals_flush_launch_import_and_program_rpcs() {
    let ctx = make_ctx();
    let char_id = 90_000_001;
    let planet_id = 40_009_082;
    let sys_id = 30_000_142;

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            2505,
            TrainedSkillRecord {
                type_id: 2505,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: sys_id,
                command_center_level: 1,
                pins_count: 2,
                extracted_commodities: HashMap::from([(3645, 200)]),
                last_extracted_unix: 0,
            },
        );
        let net = PiColonyNetworkState {
            cpu_max: 7_057.0,
            pg_max: 9_000.0,
            pins: vec![
                PiPinState {
                    pin_id: 301,
                    type_id: 2254, // Command Center
                    contents: HashMap::from([(3645, 200)]),
                    ..Default::default()
                },
                PiPinState {
                    pin_id: 302,
                    type_id: 2848, // ECU
                    extractor_resource_type_id: Some(2268),
                    extractor_head_count: 3,
                    extractor_cycle_seconds: 300,
                    extractor_head_radius: 0.02,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        save_pi_network(snap, char_id, planet_id, &net);
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);

    // Flush
    let flush = handle_pi_orbitals(
        &ctx,
        &mut session,
        "planetMgr",
        "Flush",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .expect("Flush should succeed");
    assert_eq!(flush.dict_get("planetID").and_then(|v| v.as_i64()), Some(planet_id));
    assert!(flush.dict_get("producedQuantity").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    // UpgradeCommandCenter
    let upg = handle_pi_orbitals(
        &ctx,
        &mut session,
        "planetMgr",
        "UpgradeCommandCenter",
        &[PyRep::Integer(planet_id), PyRep::Integer(3)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .expect("UpgradeCommandCenter should succeed");
    assert_eq!(upg.dict_get("commandCenterLevel").and_then(|v| v.as_i64()), Some(3));

    // UserLaunchCommodities (CommandCenterLaunch) -> creates canister in space
    let launch = handle_pi_orbitals(
        &ctx,
        &mut session,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(planet_id),
            PyRep::Integer(301),
            PyRep::Integer(3645),
            PyRep::Integer(50),
        ],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .expect("UserLaunchCommodities should succeed");
    let launch_id = launch.dict_get("launchID").and_then(|v| v.as_i64()).unwrap();
    assert!(launch_id > 0);

    // GetMyLaunchesDetails & DeleteLaunch
    let details = handle_pi_orbitals(
        &ctx,
        &mut session,
        "planetMgr",
        "GetMyLaunchesDetails",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(details, PyRep::List(v) if v.len() == 1));

    let del = handle_pi_orbitals(
        &ctx,
        &mut session,
        "planetMgr",
        "DeleteLaunch",
        &[PyRep::Integer(launch_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(del, PyRep::Bool(true));

    // ImportToColony
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            777_001,
            InventoryItemRecord {
                item_id: 777_001,
                type_id: 3645,
                owner_id: char_id,
                location_id: planet_id,
                flag: InventoryFlag::Hangar,
                quantity: 40,
                ..Default::default()
            },
        );
    });
    let imp = handle_pi_orbitals(
        &ctx,
        &mut session,
        "planetMgr",
        "UserLaunchCommodities",
        &[
            PyRep::Integer(planet_id),
            PyRep::Integer(3645),
            PyRep::Integer(25),
            PyRep::String("ImportToColony".into()),
        ],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .expect("ImportToColony should succeed");
    assert_eq!(imp.dict_get("routedQuantity").and_then(|v| v.as_i64()), Some(25));

    // GetProgramResultInfo, GetLocalDistributionReport, StartTicking, RevertChanges, CancelInstallProgram
    let prog = handle_pi_orbitals(
        &ctx,
        &mut session,
        "planetMgr",
        "GetProgramResultInfo",
        &[
            PyRep::Integer(planet_id),
            PyRep::Integer(302),
            PyRep::Integer(2268),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Real(0.1),
                PyRep::Real(0.2),
            ])]),
            PyRep::Real(0.02),
        ],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(prog, PyRep::Tuple(v) if v.len() == 3));

    let dist = handle_pi_orbitals(
        &ctx,
        &mut session,
        "planetMgr",
        "GetLocalDistributionReport",
        &[PyRep::Integer(planet_id), PyRep::Integer(302)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dist.dict_get("planetID").and_then(|v| v.as_i64()), Some(planet_id));

    for m in ["StartTicking", "RevertChanges", "CancelInstallProgram"] {
        let r = handle_pi_orbitals(
            &ctx,
            &mut session,
            "planetMgr",
            m,
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(r, PyRep::Bool(true));
    }

    assert_eq!(
        handle_pi_orbitals(
            &ctx,
            &mut session,
            "planetMgr",
            "celestialIndex",
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(4)
    );
    assert_eq!(
        handle_pi_orbitals(
            &ctx,
            &mut session,
            "planetMgr",
            "planetID",
            &[PyRep::Integer(planet_id)],
            &HashMap::new(),
            None
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(planet_id)
    );
    let forged_err = handle_pi_orbitals(
        &ctx,
        &mut session,
        "planetMgr",
        "planetID",
        &[PyRep::Integer(99_999_999)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(forged_err.code, "InvalidPlanetID");
    assert_eq!(
        handle_pi_orbitals(&ctx, &mut session, "planetMgr", "append", &[], &HashMap::new(), None)
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle_pi_orbitals(
            &ctx,
            &mut session,
            "planetMgr",
            "remoteHandler",
            &[],
            &HashMap::new(),
            None
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
}
