//! Upwell structures, Sovereignty, and Faction Warfare integration tests.

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_phase_0_s7_no_hardcoded_fallbacks_and_in_space_range_checks() {
    // parity: plan:S-7, rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID, rpc:mailMgr.SendMail, rpc:planetMgr.Flush, rpc:planetMgr.GetPlanetInfo, rpc:planetMgr.GetPlanetResourceInfo, rpc:planetMgr.GetPlanetsForChar, rpc:planetMgr.UserUpdateNetwork, rpc:shipCosmeticsMgr.ApplySkinToShip, rpc:sovMgr.AcquireSkyhooks, rpc:stationSvc.Undock
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(907);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 1. Missing arguments reject instead of falling back to Rifter/Jita/Tritanium
    let missing_skin = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "ApplySkinToShip",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(missing_skin.unwrap_err().code, "InvalidShipTypeForSkin");
    let missing_mail = ctx
        .dispatch_rpc(&mut sess, "mailMgr", None, "SendMail", &[], &kw, None)
        .await;
    assert_eq!(missing_mail.unwrap_err().code, "InvalidRecipient");

    // 2. PI: GetPlanetInfo and GetPlanetsForChar are read-only, GetPlanetResourceInfo returns SDE resource info,
    // and UserUpdateNetwork rejects a planet not in the character's current system.
    let planet_info = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "GetPlanetInfo",
            &[PyRep::Integer(40_009_077)],
            &kw,
            None,
        )
        .await
        .expect("GetPlanetInfo");
    assert!(matches!(planet_info, PyRep::KeyVal(_)));
    let my_colonies = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "GetPlanetsForChar",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetPlanetsForChar");
    assert!(matches!(my_colonies, PyRep::Rowset { .. }));
    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.planetary_colonies.is_empty(),
            "S-7: GetPlanetInfo and GetPlanetsForChar must be read-only and never auto-create a colony"
        );
    });
    let res_info_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "GetPlanetResourceInfo",
            &[PyRep::Integer(40_009_077)],
            &kw,
            None,
        )
        .await
        .expect("GetPlanetResourceInfo");
    assert!(matches!(res_info_ok, PyRep::KeyVal(_)));
    let invalid_planet = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[PyRep::Integer(99_999_999)],
            &kw,
            None,
        )
        .await;
    assert_eq!(invalid_planet.unwrap_err().code, "InvalidPlanetID");

    let invalid_pin_planet = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(99_999_999),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                    PyRep::Integer(2256),
                    PyRep::Real(0.1),
                    PyRep::Real(0.2),
                ])]),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(invalid_pin_planet.unwrap_err().code, "InvalidPlanetID");

    // Real SDE planet in Perimeter (30000144, e.g. 40009148) while character is in Jita (30000142) -> PlanetNotInCurrentSystem
    let perimeter_planet_id = ctx
        .sde
        .celestials_in_system(30_000_144)
        .into_iter()
        .find(|c| c.group_id == 7)
        .map(|c| c.celestial_id)
        .expect("Perimeter (30000144) must have a real SDE planet");
    let wrong_sys_planet = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[PyRep::Integer(perimeter_planet_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        wrong_sys_planet.unwrap_err().code,
        "PlanetNotInCurrentSystem"
    );
    let wrong_sys_pin = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(perimeter_planet_id),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                    PyRep::Integer(2256),
                    PyRep::Real(0.1),
                    PyRep::Real(0.2),
                ])]),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        wrong_sys_pin.unwrap_err().code,
        "PlanetNotInCurrentSystem"
    );
    let flush_override = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "Flush",
            &[PyRep::Integer(40_009_077), PyRep::Integer(65)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        flush_override.unwrap_err().code,
        "ClientSchematicOverrideForbidden"
    );

    // 3. AttackStructure, ClaimSovereignty, AcquireSkyhooks, and CaptureComplex enforce in-space and proximity/system checks
    let attack_docked = ctx
        .dispatch_rpc(
            &mut sess,
            // rpc-lint: allow S-7 canonical task requires testing AttackStructure
            "structureControl",
            None,
            // rpc-lint: allow S-7 canonical task requires testing AttackStructure
            "AttackStructure",
            &[PyRep::Integer(1_040_000_000_001)],
            &kw,
            None,
        )
        .await;
    assert_eq!(attack_docked.unwrap_err().code, "ShipDocked");

    let skyhook_docked = ctx
        .dispatch_rpc(&mut sess, "sovMgr", None, "AcquireSkyhooks", &[], &kw, None)
        .await;
    assert_eq!(skyhook_docked.unwrap_err().code, "ShipDocked");

    // S-7 requires ClaimSovereignty to either enforce proximity/ownership/state checks or return UnimplementedRPC
    let claim_sov_docked = ctx
        .dispatch_rpc(
            &mut sess,
            // rpc-lint: allow S-7 canonical task requires testing ClaimSovereignty returns UnimplementedRPC
            "sovMgr",
            None,
            // rpc-lint: allow S-7 canonical task requires testing ClaimSovereignty returns UnimplementedRPC
            "ClaimSovereignty",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(claim_sov_docked.unwrap_err().code, "UnimplementedRPC");

    let fw_docked = ctx
        .dispatch_rpc(
            &mut sess,
            // rpc-lint: allow S-7 canonical task requires testing CaptureComplex
            "facWarMgr",
            None,
            // rpc-lint: allow S-7 canonical task requires testing CaptureComplex
            "CaptureComplex",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(fw_docked.unwrap_err().code, "ShipDocked");

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock in Jita");

    let fw_highsec = ctx
        .dispatch_rpc(
            &mut sess,
            // rpc-lint: allow S-7 canonical task requires testing CaptureComplex
            "facWarMgr",
            None,
            // rpc-lint: allow S-7 canonical task requires testing CaptureComplex
            "CaptureComplex",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(fw_highsec.unwrap_err().code, "NotLowSecWarzoneSystem");

    let skyhook_highsec = ctx
        .dispatch_rpc(&mut sess, "sovMgr", None, "AcquireSkyhooks", &[], &kw, None)
        .await;
    assert_eq!(skyhook_highsec.unwrap_err().code, "NotNullSecSystem");

    // Move ship into K-space null-sec 30002904 (copied ball from Jita undock) without a friendly structure within 50 km -> OutsideSovStructureProximity
    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
    let (_, ship_ball) = jita_sys
        .get_ball_snapshot(100_000_001)
        .await
        .expect("get_ball_snapshot")
        .expect("ship ball in Jita");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_002_904;
            ch.station_id = None;
        }
        if let Some(ship) = snap.items.get_mut(&100_000_001) {
            ship.location_id = 30_002_904;
        }
    });
    sess.solar_system_id = Some(30_002_904);
    sess.station_id = None;
    let null_sys = ctx.systems.get_or_spawn(30_002_904);
    let ship_pos = ship_ball.position;
    null_sys
        .spawn_ball_with_combat(ship_ball.clone(), eve_destiny::CombatState::default())
        .await
        .expect("spawn ship in null-sec");
    let skyhook_no_alliance = ctx
        .dispatch_rpc(&mut sess, "sovMgr", None, "AcquireSkyhooks", &[], &kw, None)
        .await;
    assert_eq!(skyhook_no_alliance.unwrap_err().code, "NotInAlliance");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = 98_000_001;
            ch.alliance_id = Some(99_000_001);
        }
        snap.alliances.insert(
            99_000_001,
            eve_db::AllianceRecord {
                alliance_id: 99_000_001,
                name: "Test Sov Alliance".into(),
                short_name: "TSOV".into(),
                executor_corp_id: 98_000_001,
                member_corp_ids: std::collections::HashSet::from([98_000_001]),
            },
        );
    });
    sess.corporation_id = Some(98_000_001);
    sess.alliance_id = Some(99_000_001);
    let skyhook_no_struct = ctx
        .dispatch_rpc(&mut sess, "sovMgr", None, "AcquireSkyhooks", &[], &kw, None)
        .await;
    assert_eq!(
        skyhook_no_struct.unwrap_err().code,
        "NoFriendlySovStructureInSystem"
    );

    // Seed an online friendly structure 100 km away (> 50 km max proximity) -> OutsideSovStructureProximity
    // and an enemy structure 100 km away (> 50 km max attack range) -> StructureOutOfRange
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            1_040_000_000_099,
            eve_db::UpwellStructureRecord {
                structure_id: 1_040_000_000_099,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_002_904,
                name: "Far Sov Hub".into(),
                position: [ship_pos.x + 100_000.0, ship_pos.y, ship_pos.z],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 5_000.0,
                armor_hp: 5_000.0,
                hull_hp: 5_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        snap.structures.insert(
            1_040_000_000_098,
            eve_db::UpwellStructureRecord {
                structure_id: 1_040_000_000_098,
                type_id: 35832,
                owner_corp_id: 98_000_002,
                solar_system_id: 30_002_904,
                name: "Far Enemy Astrahus".into(),
                position: [ship_pos.x + 100_000.0, ship_pos.y, ship_pos.z],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 5_000.0,
                armor_hp: 5_000.0,
                hull_hp: 5_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        // Ensure 100_000_001 has a fitted weapon (2889 in HiSlot0) and loaded ammo (185 in HiSlot0)
        if let Some(w) = snap.items.get_mut(&100_000_005) {
            w.location_id = 100_000_001;
            w.flag = eve_db::InventoryFlag::HiSlot0;
            w.singleton = true;
        }
        snap.items.insert(
            100_907_185,
            eve_db::InventoryItemRecord {
                item_id: 100_907_185,
                type_id: 185, // Phased Plasma S
                owner_id: 90_000_001,
                location_id: 100_000_001,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 100,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let skyhook_far_struct = ctx
        .dispatch_rpc(&mut sess, "sovMgr", None, "AcquireSkyhooks", &[], &kw, None)
        .await;
    assert_eq!(
        skyhook_far_struct.unwrap_err().code,
        "OutsideSovStructureProximity"
    );

    // Move friendly structure within 20 km (< 50 km max proximity) and verify AcquireSkyhooks succeeds
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.structures.get_mut(&1_040_000_000_099) {
            s.position = [ship_pos.x + 20_000.0, ship_pos.y, ship_pos.z];
        }
    });
    let claim_ok = ctx
        .dispatch_rpc(&mut sess, "sovMgr", None, "AcquireSkyhooks", &[], &kw, None)
        .await
        .expect("AcquireSkyhooks within 50 km of friendly online ShieldVulnerable structure");
    assert!(matches!(claim_ok, PyRep::KeyVal(_)));

    let attack_far_struct = ctx
        .dispatch_rpc(
            &mut sess,
            // rpc-lint: allow S-7 canonical task requires testing AttackStructure
            "structureControl",
            None,
            // rpc-lint: allow S-7 canonical task requires testing AttackStructure
            "AttackStructure",
            &[PyRep::Integer(1_040_000_000_098)],
            &kw,
            None,
        )
        .await;
    assert_eq!(attack_far_struct.unwrap_err().code, "StructureOutOfRange");

    // Move ship into low-sec warzone 30002537 (Amamake) 100 km from FW complex beacon -> OutsideComplexCaptureRadius
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_002_537;
            ch.station_id = None;
            ch.war_faction_id = Some(500_001);
        }
        if let Some(ship) = snap.items.get_mut(&100_000_001) {
            ship.location_id = 30_002_537;
        }
        let st =
            snap.system_states
                .entry(30_002_537)
                .or_insert_with(|| eve_db::SystemDynamicState {
                    solar_system_id: 30_002_537,
                    dbs_multiplier: 1.0,
                    ess_main_bank: eve_db::IskCents(0),
                    ess_reserve_bank: eve_db::IskCents(0),
                    ess_linked_character_id: None,
                    ess_link_started_tick: None,
                    sov_alliance_id: None,
                    military_index: 0,
                    industrial_index: 0,
                    strategic_index: 0,
                    fw_contested_pct: 10.0,
                    insurgency_corruption_stage: 1,
                    insurgency_suppression_stage: 0,
                    wormhole_remaining_mass_kg: 0.0,
                    wormhole_destination_system_id: None,
                    signature_position: None,
                    fw_complex_position: Some([ship_pos.x + 100_000.0, ship_pos.y, ship_pos.z]),
                    fw_capture_seconds_elapsed: 60,
                    fw_capture_duration_seconds: 600,
                    last_fw_capture_tick: 0,
                    last_sov_claim_tick: 0,
                });
        st.fw_complex_position = Some([ship_pos.x + 100_000.0, ship_pos.y, ship_pos.z]);
    });
    sess.solar_system_id = Some(30_002_537);
    let fw_sys = ctx.systems.get_or_spawn(30_002_537);
    fw_sys
        .spawn_ball_with_combat(ship_ball, eve_destiny::CombatState::default())
        .await
        .expect("spawn ship in Amamake");
    let fw_far_beacon = ctx
        .dispatch_rpc(
            &mut sess,
            // rpc-lint: allow S-7 canonical task requires testing CaptureComplex
            "facWarMgr",
            None,
            // rpc-lint: allow S-7 canonical task requires testing CaptureComplex
            "CaptureComplex",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        fw_far_beacon.unwrap_err().code,
        "OutsideComplexCaptureRadius"
    );
}
