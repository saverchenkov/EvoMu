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
