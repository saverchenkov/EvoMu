//! Edge-case behavioral unit tests for `pi_colonies` (`planetMgr`).

#![forbid(unsafe_code)]

use super::*;
use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::clock::VirtualServerClock;
use eve_db::{
    AccountRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents,
    PlanetaryColonyRecord, TrainedSkillRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;
use std::sync::Arc;

fn make_pi_ctx() -> (ServerContext, Arc<VirtualServerClock>) {
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_776_000_000));
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(test_ctx().sde.clone(), db);
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

fn seed_char(ctx: &ServerContext, char_id: i64, sys_id: u32, ship_id: i64, isk: i64, ipc_lvl: u8, ccu_lvl: u8) {
    let uid = char_id as u64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            format!("pi_{uid}"),
            AccountRecord {
                user_id: uid,
                username: format!("pi_{uid}"),
                password_token: "pw".into(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.characters.insert(
            char_id,
            CharacterRecord {
                activity_status: None,
                character_id: char_id,
                user_id: char_id as u64,
                name: format!("PiTester_{char_id}"),
                corporation_id: 98_000_001,
                alliance_id: None,
                war_faction_id: None,
                station_id: None,
                structure_id: None,
                solar_system_id: sys_id,
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: ship_id,
                balance: IskCents::from_isk(isk),
                plex_balance: 0,
                loyalty_points: Default::default(),
                standings: Default::default(),
                security_status: 0.0,
                clone_grade: 2,
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 1,
                school_id: 11,
                neural: eve_db::NeuralAttributes::default(),
                skill_points: 0.0,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 1,
                create_date_unix: 0,
                fleet_id: None,
                autopilot_waypoints: Vec::new(),
                version: 1,
                system_epoch: 1,
            },
        );
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(2495, TrainedSkillRecord { type_id: 2495, level: ipc_lvl, skill_points: 256_000.0 });
        sk.insert(2505, TrainedSkillRecord { type_id: 2505, level: ccu_lvl, skill_points: 256_000.0 });
    });
    ctx.db
        .acquire_online_lock(uid, uid + 1000, Some(char_id), true, ctx.now_unix())
        .unwrap();
}

/// Finds `(sys_id, planet_id, matching_cc_tid, mismatched_cc_tid, factory_tid, schematic_id, ecu_tid, storage_tid, p0_res_tid)`
/// directly from real SDE data.
fn find_sde_pi_fixtures(ctx: &ServerContext) -> (u32, i64, u32, u32, u32, u32, u32, u32, u32) {
    let sys_id = 30_000_142_u32;
    let planet_ids = &ctx.sde.solar_systems.get(&sys_id).unwrap().planet_ids;
    let mut chosen_planet = None;
    for &pid in planet_ids {
        if let Ok((res_tid, _, _, _)) = resolve_sde_planet_p0_extraction(ctx, sys_id, pid, None) {
            if let Some(cel) = ctx
                .sde
                .celestials_in_system(sys_id)
                .into_iter()
                .find(|c| c.celestial_id == pid && c.group_id == 7)
            {
                let pt = ctx
                    .sde
                    .types
                    .get(&cel.type_id)
                    .map(|t| t.variation_parent_type_id.unwrap_or(t.type_id))
                    .unwrap_or(cel.type_id);
                chosen_planet = Some((pid, pt, res_tid));
                break;
            }
        }
    }
    let (planet_id, planet_type, p0_res) = chosen_planet.expect("Jita planet with P0 resource");

    let mut match_cc = None;
    let mut mismatch_cc = None;
    for (&tid, t) in &ctx.sde.types {
        if is_command_center_type(ctx, tid) {
            let req = t.default_attributes.get(&ATTR_PLANET_RESTRICTION).copied().unwrap_or(0.0) as u32;
            if req == planet_type && match_cc.is_none() {
                match_cc = Some(tid);
            } else if req > 0 && req != planet_type && mismatch_cc.is_none() {
                mismatch_cc = Some(tid);
            }
        }
    }
    let (schem_id, factory_tid) = ctx
        .sde
        .planet_schematics
        .iter()
        .filter_map(|(&sid, si)| {
            let ftid = si
                .pins
                .iter()
                .copied()
                .find(|pid| ctx.sde.types.get(pid).is_some_and(|t| t.group_id == 1028))?;
            Some((sid, ftid))
        })
        .min_by_key(|&(sid, _)| sid)
        .expect("SDE planet schematic with factory pin");

    let ecu_tid = ctx
        .sde
        .types
        .iter()
        .find(|(&tid, t)| tid == 2848 || t.group_id == 1026 || t.group_id == 1063)
        .map(|(&tid, _)| tid)
        .unwrap_or(2848);
    let storage_tid = ctx
        .sde
        .types
        .iter()
        .find(|(_, t)| t.group_id == 1029 && !is_command_center_type(ctx, t.type_id))
        .map(|(&tid, _)| tid)
        .unwrap_or(2257);

    (
        sys_id,
        planet_id,
        match_cc.expect("matching CC type"),
        mismatch_cc.expect("mismatched CC type"),
        factory_tid,
        schem_id,
        ecu_tid,
        storage_tid,
        p0_res,
    )
}

#[test]
fn query_methods_auth_bound_monikers_and_abandon_edges() {
    let (ctx, _) = make_pi_ctx();
    let (sys_id, planet_id, cc_tid, _, factory_tid, schem_id, ecu_tid, _, _) =
        find_sde_pi_fixtures(&ctx);
    let kw = HashMap::new();

    // Unknown service / method -> None; unauth errors across all 8 methods
    let mut unauth = SessionState::default();
    assert!(handle_pi_colonies(&ctx, &mut unauth, "other_service", "GetColony", &[], &kw, None).is_none());
    assert!(handle_pi_colonies(&ctx, &mut unauth, "planetMgr", "unknown_method_fallback", &[], &kw, None).is_none());
    for method in [
        "GetColony",
        "GetFullNetworkForOwner",
        "GetCommandPinsForPlanet",
        "GetExtractorsForPlanet",
        "GetCommandCenterLevel",
        "GetPin",
        "UserAbandonPlanet",
        "UserUpdateNetwork",
    ] {
        assert!(
            handle_pi_colonies(&ctx, &mut unauth, "planetMgr", method, &[], &kw, None)
                .unwrap()
                .is_err(),
            "{method} must reject unauth"
        );
    }

    let c1 = 90_500_001_i64;
    let c2 = 90_500_002_i64;
    seed_char(&ctx, c1, sys_id, 1_000_500_001, 10_000_000, 4, 4);
    seed_char(&ctx, c2, sys_id, 1_000_500_002, 10_000_000, 4, 4);
    let mut sess = auth_session(1, c1, 98_000_001, sys_id);

    // Seed colonies for c1 (planet_id and planet_id+1) and c2 (planet_id)
    ctx.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert(
            (c1, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: c1,
                solar_system_id: sys_id,
                command_center_level: 2,
                pins_count: 3,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 0,
            },
        );
        save_pi_network(
            snap,
            c1,
            planet_id,
            &PiColonyNetworkState {
                pins: vec![
                    PiPinState { pin_id: 101, type_id: cc_tid, ..Default::default() },
                    PiPinState {
                        pin_id: 102,
                        type_id: ecu_tid,
                        extractor_head_count: 2,
                        extractor_cycle_seconds: 600,
                        ..Default::default()
                    },
                    PiPinState {
                        pin_id: 103,
                        type_id: factory_tid,
                        schematic_id: Some(schem_id),
                        extractor_cycle_seconds: 0,
                        ..Default::default()
                    },
                    PiPinState {
                        pin_id: 104,
                        type_id: factory_tid,
                        schematic_id: None,
                        extractor_cycle_seconds: 0,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        );
        snap.planetary_colonies.insert(
            (c1, planet_id + 1),
            PlanetaryColonyRecord {
                planet_id: planet_id + 1,
                owner_id: c1,
                solar_system_id: sys_id,
                command_center_level: 1,
                pins_count: 0,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 0,
            },
        );
        snap.planetary_colonies.insert(
            (c2, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: c2,
                solar_system_id: sys_id,
                command_center_level: 5,
                pins_count: 1,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: 0,
            },
        );
    });

    // GetFullNetworkForOwner without planet arg aggregates c1's colonies only (ignores c2)
    let full_all = handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetFullNetworkForOwner", &[], &kw, None)
        .unwrap()
        .unwrap();
    assert_eq!(full_all.dict_get("commandCenterLevel"), Some(&PyRep::Integer(2)));
    // With specific planet arg (including non-owned planet -> sets planetID)
    let full_none = handle_pi_colonies(
        &ctx,
        &mut sess,
        "planetMgr",
        "GetFullNetworkForOwner",
        &[PyRep::Integer(planet_id + 99)],
        &kw,
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(full_none.dict_get("planetID"), Some(&PyRep::Integer(planet_id + 99)));

    // GetCommandPinsForPlanet / GetExtractorsForPlanet / GetCommandCenterLevel / GetPin
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetCommandPinsForPlanet", &[], &kw, None)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidPlanetID"
    );
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetExtractorsForPlanet", &[], &kw, None)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidPlanetID"
    );
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetCommandCenterLevel", &[], &kw, None)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidPlanetID"
    );
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "GetPin", &[], &kw, None)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidPlanetID"
    );
    assert_eq!(
        handle_pi_colonies(&ctx, &mut sess, "planetMgr", "UserAbandonPlanet", &[], &kw, None)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidPlanetID"
    );

    let cmds = handle_pi_colonies(
        &ctx,
        &mut sess,
        "planetMgr",
        "GetCommandPinsForPlanet",
        &[PyRep::Integer(planet_id)],
        &kw,
        None,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(cmds, PyRep::List(ref v) if v.len() == 1));
    let exts = handle_pi_colonies(
        &ctx,
        &mut sess,
        "planetMgr",
        "GetExtractorsForPlanet",
        &[PyRep::Integer(planet_id)],
        &kw,
        None,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(exts, PyRep::List(ref v) if v.len() == 1));
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "GetCommandCenterLevel",
            &[PyRep::Integer(planet_id)],
            &kw,
            None,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(2)
    );

    // GetPin unbound with schematic cycle fallback & bound moniker without schematic
    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: 1,
        character_id: c1,
        service: "planetMgr".into(),
        object_id: planet_id,
        oid_str: format!("N=1:{planet_id}"),
        ref_id: 1,
    };
    let expected_cycle = ctx.sde.planet_schematics.get(&schem_id).unwrap().cycle_time as i64;
    let pin103 = handle_pi_colonies(
        &ctx,
        &mut sess,
        "planetMgr",
        "GetPin",
        &[PyRep::Integer(planet_id), PyRep::Integer(103)],
        &kw,
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(pin103.dict_get("cycleTimeSeconds"), Some(&PyRep::Integer(expected_cycle)));
    let pin104 = handle_pi_colonies(
        &ctx,
        &mut sess,
        "planetMgr",
        "GetPin",
        &[PyRep::Integer(104)],
        &kw,
        Some(&bound),
    )
    .unwrap()
    .unwrap();
    assert_eq!(pin104.dict_get("cycleTimeSeconds"), Some(&PyRep::Integer(0)));

    // UserAbandonPlanet success + OnMajorPlanetStateUpdate notification
    let _ = ctx.drain_notifications_for_char(c1);
    let ab = handle_pi_colonies(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserAbandonPlanet",
        &[PyRep::Integer(planet_id)],
        &kw,
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(ab, PyRep::Bool(true));
    assert!(ctx
        .drain_notifications_for_char(c1)
        .iter()
        .any(|n| n.event == "OnMajorPlanetStateUpdate"));
}

#[test]
fn user_update_network_validation_commands_and_atomic_rollback() {
    let (ctx, _) = make_pi_ctx();
    let (sys_id, planet_id, cc_tid, mismatch_cc_tid, factory_tid, schem_id, ecu_tid, storage_tid, p0_res) =
        find_sde_pi_fixtures(&ctx);
    let cid = 90_500_010_i64;
    let ship_id = 1_000_500_010_i64;
    let kw = HashMap::new();
    seed_char(&ctx, cid, sys_id, ship_id, 50_000_000, 0, 0);

    let mut sess = auth_session(1, cid, 98_000_001, sys_id);
    sess.ship_id = Some(ship_id);

    // Header validation errors
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id), PyRep::Integer(42)],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ClientSchematicOverrideForbidden"
    );
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id)],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidCommandStream"
    );

    sess.solar_system_id = None;
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1)])])],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NoSolarSystem"
    );
    sess.solar_system_id = Some(sys_id);

    // Bad command item inside stream
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id), PyRep::List(vec![PyRep::Integer(99)])],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidCommandStream"
    );

    // Command 1 CC errors: InvalidPinType (-1), ship_id == None, incompatible planet, missing CC item
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(-1), PyRep::Real(0.0), PyRep::Real(0.0)])])],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPinType"
    );
    sess.ship_id = None;
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(i64::from(cc_tid)), PyRep::Real(0.0), PyRep::Real(0.0)])])],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CommandCenterItemRequired"
    );
    sess.ship_id = Some(ship_id);
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(i64::from(mismatch_cc_tid)), PyRep::Real(0.0), PyRep::Real(0.0)])])],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "IncompatibleCommandCenterPlanetType"
    );

    // Give 2 CCs in cargo so first succeeds and second hits ColonyAlreadyExists / MaxColoniesReached
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            850_001,
            InventoryItemRecord {
                item_id: 850_001,
                type_id: cc_tid,
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                ..Default::default()
            },
        );
    });

    // Same-batch rollback: place valid CC (101) + invalid command 999 -> CC item, colony, note must roll back!
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[
                PyRep::Integer(planet_id),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(101), PyRep::Integer(i64::from(cc_tid)), PyRep::Real(0.1), PyRep::Real(0.1)]),
                    PyRep::Tuple(vec![PyRep::Integer(999)]),
                ]),
            ],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "UnsupportedPlanetCommand"
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items.get(&850_001).unwrap().quantity, 2);
        assert!(!s.planetary_colonies.contains_key(&(cid, planet_id)));
    });

    // Non-CC commands on missing colony -> ColonyNotFound across cmds 1,2,3,4,5,6,9,10,14
    for cmd in [
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(i64::from(storage_tid)), PyRep::Real(0.0), PyRep::Real(0.0)]),
        PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(0)]),
        PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Integer(1), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2)]), PyRep::Integer(i64::from(p0_res)), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(300), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(14), PyRep::Integer(1), PyRep::Integer(0), PyRep::Real(0.0), PyRep::Real(0.0)]),
    ] {
        assert_eq!(
            handle_pi_colonies(
                &ctx,
                &mut sess,
                "planetMgr",
                "UserUpdateNetwork",
                &[PyRep::Integer(planet_id), PyRep::List(vec![cmd])],
                &kw,
                None,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "ColonyNotFound"
        );
    }

    // Deploy CC + place ECU (102), factory (103), storage (104), links, route, schematic, ECU program, head ops!
    let bound = BoundObjectEntry {
        bound_id: 2,
        session_id: 1,
        user_id: 1,
        character_id: cid,
        service: "planetMgr".into(),
        object_id: planet_id,
        oid_str: format!("N=2:{planet_id}"),
        ref_id: 2,
    };
    let deploy_ok = handle_pi_colonies(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(101), PyRep::Integer(i64::from(cc_tid)), PyRep::Real(0.0), PyRep::Real(0.0)]),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(i64::from(ecu_tid)), PyRep::Real(0.05), PyRep::Real(0.05)]),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(103), PyRep::Integer(i64::from(factory_tid)), PyRep::Real(0.06), PyRep::Real(0.05)]),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(104), PyRep::Integer(i64::from(storage_tid)), PyRep::Real(0.07), PyRep::Real(0.05)]),
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(102), PyRep::Integer(103), PyRep::Integer(0)]),
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(103), PyRep::Integer(104), PyRep::Integer(0)]),
            // Command 5 upgrade link to level 2
            PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(102), PyRep::Integer(103), PyRep::Integer(2)]),
            // Command 6 route 500 across 102->103->104, then overwrite route 500 with different qty
            PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(500), PyRep::List(vec![PyRep::Integer(102), PyRep::Integer(103), PyRep::Integer(104)]), PyRep::Integer(i64::from(p0_res)), PyRep::Integer(10)]),
            PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(500), PyRep::List(vec![PyRep::Integer(102), PyRep::Integer(103), PyRep::Integer(104)]), PyRep::Integer(i64::from(p0_res)), PyRep::Integer(5)]),
            // Command 7 with 3 args sets schematic; Command 8 overrides it
            PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(103), PyRep::Integer(i64::from(schem_id))]),
            PyRep::Tuple(vec![PyRep::Integer(8), PyRep::Integer(103), PyRep::Integer(i64::from(schem_id))]),
            // Command 10 with 2-tuple head + absolute expiry (> now_unix)
            PyRep::Tuple(vec![
                PyRep::Integer(10),
                PyRep::Integer(102),
                PyRep::Integer(i64::from(p0_res)),
                PyRep::Integer(300),
                PyRep::Integer(1),
                PyRep::List(vec![PyRep::Tuple(vec![PyRep::Real(0.06), PyRep::Real(0.06)])]),
                PyRep::Integer(1_776_003_600),
            ]),
        ])],
        &kw,
        Some(&bound),
    )
    .unwrap()
    .unwrap();
    assert_eq!(deploy_ok.dict_get("pinsCount"), Some(&PyRep::Integer(4)));
    assert_eq!(deploy_ok.dict_get("linksCount"), Some(&PyRep::Integer(2)));

    // Fetch real server pin IDs
    let col_net = ctx.db.with_snapshot(|s| load_pi_network(s, cid, planet_id, 0));
    let ecu_id = col_net.pins[1].pin_id;
    let fac_id = col_net.pins[2].pin_id;
    let sto_id = col_net.pins[3].pin_id;

    // Duplicate CC on same planet -> ColonyAlreadyExists; MaxColoniesReached on another planet
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(200), PyRep::Integer(i64::from(cc_tid)), PyRep::Real(0.2), PyRep::Real(0.2)])])],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ColonyAlreadyExists"
    );
    let other_planet = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .unwrap()
        .planet_ids
        .iter()
        .copied()
        .find(|&p| p != planet_id && resolve_sde_planet_p0_extraction(&ctx, sys_id, p, None).is_ok())
        .unwrap();
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(other_planet), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(200), PyRep::Integer(i64::from(cc_tid)), PyRep::Real(0.2), PyRep::Real(0.2)])])],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MaxColoniesReached"
    );

    // Command 9: CCU skill 0 -> MissingSkillForCommandCenterUpgrade; InvalidCommandCenterLevel (0, 6); InsufficientFunds
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(6)])])],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidCommandCenterLevel"
    );
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(1)])])],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MissingSkillForCommandCenterUpgrade"
    );

    // Command 5 errors: <= current level (1 on level-2 link), missing link; Command 4 prunes routes when link removed
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(ecu_id), PyRep::Integer(fac_id), PyRep::Integer(1)])])],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidLinkLevel"
    );
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(ecu_id), PyRep::Integer(sto_id), PyRep::Integer(2)])])],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "LinkNotFound"
    );

    // Command 14/15: out-of-range head, update existing head 0, add heads 1..=9, 10th new (11th total) -> InvalidExtractorHeadCount
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(14), PyRep::Integer(ecu_id), PyRep::Integer(0), PyRep::Real(1.5), PyRep::Real(1.5)])])],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ExtractorHeadOutOfRange"
    );
    let mut head_cmds: Vec<PyRep> = (0..10)
        .map(|hid| {
            PyRep::Tuple(vec![
                PyRep::Integer(14),
                PyRep::Integer(ecu_id),
                PyRep::Integer(hid),
                PyRep::Real(0.05 + 0.005 * (hid as f64)),
                PyRep::Real(0.05),
            ])
        })
        .collect();
    handle_pi_colonies(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[PyRep::Integer(planet_id), PyRep::List(head_cmds.clone())],
        &kw,
        None,
    )
    .unwrap()
    .unwrap();
    head_cmds.push(PyRep::Tuple(vec![
        PyRep::Integer(15),
        PyRep::Integer(ecu_id),
        PyRep::Integer(10),
        PyRep::Real(0.05),
        PyRep::Real(0.05),
    ]));
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id), PyRep::List(head_cmds)],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidExtractorHeadCount"
    );

    // Command 4 deletes link ecu->fac and prunes route 500; Command 2 deletes storage pin & its remaining link
    let del_res = handle_pi_colonies(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(planet_id),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(4), PyRep::Integer(ecu_id), PyRep::Integer(fac_id)]),
                PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(sto_id)]),
            ]),
        ],
        &kw,
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(del_res.dict_get("linksCount"), Some(&PyRep::Integer(0)));
    let after_del = ctx.db.with_snapshot(|s| load_pi_network(s, cid, planet_id, 0));
    assert!(after_del.routes.is_empty());

    // Zero balance hits InsufficientFunds on pin placement & command 9 upgrade
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.get_mut(&cid).unwrap().get_mut(&2505).unwrap().level = 5;
        snap.characters.get_mut(&cid).unwrap().balance = IskCents(0);
    });
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(i64::from(storage_tid)), PyRep::Real(0.08), PyRep::Real(0.05)])])],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );
    assert_eq!(
        handle_pi_colonies(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(1)])])],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );
}

#[test]
fn handle_pi_colonies_unknown_returns_none_and_user_update_network_dispatches_kill_mutants() {
    let (ctx, _clock) = make_pi_ctx();
    let (sys_id, planet_id, cc_tid, _, _, _, _, _, _) = find_sde_pi_fixtures(&ctx);
    let cid = 90_711_001_i64;
    let ship_id = 1_711_001_i64;
    seed_char(&ctx, cid, sys_id, ship_id, 10_000_000, 3, 3);
    let mut sess = auth_session(cid as u64, cid, 98_000_001, sys_id);
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();

    // 1. Unrelated service returns None (kills `handle_pi_colonies -> Some(Ok(Default::default()))`)
    assert!(
        handle_pi_colonies(&ctx, &mut sess, "marketProxy", "GetMarketGroups", &[], &kw, None)
            .is_none()
    );

    // 2. Empty command stream fails explicitly with InvalidCommandStream
    let empty_err = handle_pi_colonies(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[PyRep::Integer(planet_id), PyRep::List(Vec::new())],
        &kw,
        None,
    )
    .expect("UserUpdateNetwork must return Some")
    .unwrap_err();
    assert_eq!(empty_err.code, "InvalidCommandStream");

    // 3. Valid Command Center placement persists colony and returns populated keyval
    //    (kills `handle_pi_colonies -> None` and `delete arm ("planetMgr", "UserUpdateNetwork")`).
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            950_001,
            InventoryItemRecord {
                item_id: 950_001,
                type_id: cc_tid,
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let res = handle_pi_colonies(
        &ctx,
        &mut sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(planet_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(101),
                PyRep::Integer(i64::from(cc_tid)),
                PyRep::Real(0.1),
                PyRep::Real(0.1),
            ])]),
        ],
        &kw,
        None,
    )
    .expect("UserUpdateNetwork must return Some")
    .expect("Command Center placement succeeds");
    assert_eq!(res.dict_get("pinsCount"), Some(&PyRep::Integer(1)));
    assert!(ctx
        .db
        .with_snapshot(|s| s.planetary_colonies.contains_key(&(cid, planet_id))));
}
