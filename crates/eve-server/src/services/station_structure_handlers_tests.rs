//! Unit tests for `station_structure_handlers` (Fulcrum gate & alignment buff, Upwell dock hull matrix, orbital gantry anchor & upgrade).

#![forbid(unsafe_code)]

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_db::{corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use glam::DVec3;

#[test]
fn standing_skills_fulcrum_multiplier_and_upwell_dock_hull_matrix() {
    assert!((effective_standing_with_skills(0.0, 5, 5) - 0.0).abs() < 1e-9);
    assert!((effective_standing_with_skills(5.0, 0, 5) - 6.0).abs() < 1e-9);
    assert!((effective_standing_with_skills(-5.0, 5, 0) - (-2.0)).abs() < 1e-9);

    let ctx = test_ctx_with_clock(1_800_000_000);
    let char_id = 910_701_i64;
    let corp_id = 98_700_701_i64;
    let sys_id = 30_000_144_u32;
    let astra_id = 1_000_000_970_001_i64;
    let tatara_id = 1_000_000_970_002_i64;
    let fort_id = 1_000_000_970_003_i64;
    let keep_id = 1_000_000_970_004_i64;
    let flex_id = 1_000_000_970_005_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = clone_char_with(snap, char_id, "Dock Matrix Pilot", corp_id, sys_id, 100_000_001);
        if let Some(st) = ctx.sde.stations.get(&60_015_187) {
            ch.standings.insert(st.owner_corporation_id, 10.0);
            if let Some(fid) = st.owner_faction_id {
                ch.standings.insert(fid, 10.0);
            }
        }
        snap.characters.insert(char_id, ch);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                ceo_id: char_id,
                ..Default::default()
            },
        );
        assert!((fulcrum_alignment_inertia_multiplier(snap, char_id, Some(60_015_187), 30_100_000) - 1.0).abs() < 1e-9);
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_010);
        assert!((fulcrum_alignment_inertia_multiplier(snap, char_id, Some(60_015_187), 30_000_142) - 0.80).abs() < 1e-9);
        assert!((fulcrum_alignment_inertia_multiplier(snap, char_id, None, 30_000_142) - 1.0).abs() < 1e-9);

        for (sid, tid) in [
            (astra_id, 35_832),
            (tatara_id, 35_836),
            (fort_id, 35_833),
            (keep_id, 35_834),
            (flex_id, 35_840),
        ] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: tid,
                    owner_corp_id: corp_id,
                    solar_system_id: sys_id,
                    name: format!("Struct {sid}"),
                    position: [10_000.0, 20_000.0, 30_000.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 500,
                    shield_hp: 1.0,
                    armor_hp: 1.0,
                    hull_hp: 1.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
        }
    });

    // Fulcrum docking gate refuses Empire Militia (500001..=500004), allows Pirate (500010)
    assert!(validate_npc_station_docking_gate(&ctx, char_id, 60_015_187).is_ok());
    assert!(evaluate_station_service_standing_gate(&ctx, char_id, 60_015_187, 23).is_ok());
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_001);
    });
    assert_eq!(
        validate_npc_station_docking_gate(&ctx, char_id, 60_015_187).unwrap_err().code,
        "FulcrumRefusesEmpireMilitia"
    );
    assert_eq!(
        evaluate_station_service_standing_gate(&ctx, char_id, 60_015_187, 23).unwrap_err().code,
        "StandingTooLowForStationService"
    );

    // Upwell dock target errors & hull size matrix
    assert_eq!(
        resolve_upwell_dock_target(&ctx, 999_999, sys_id, char_id, 587, 25).unwrap_err().code,
        "StructureNotFound"
    );
    assert_eq!(
        resolve_upwell_dock_target(&ctx, astra_id, 30_000_142, char_id, 587, 25).unwrap_err().code,
        "DockTargetWrongSystem"
    );
    assert_eq!(
        resolve_upwell_dock_target(&ctx, flex_id, sys_id, char_id, 587, 25).unwrap_err().code,
        "StructureDoesNotAllowDocking"
    );

    // Astrahus: allows subcap (25) & Freighter (513), rejects Rorqual (883) & Dreadnought (485)
    assert!(resolve_upwell_dock_target(&ctx, astra_id, sys_id, char_id, 587, 25).is_ok());
    assert!(resolve_upwell_dock_target(&ctx, astra_id, sys_id, char_id, 20_185, 513).is_ok());
    assert_eq!(
        resolve_upwell_dock_target(&ctx, astra_id, sys_id, char_id, 28_352, 883).unwrap_err().code,
        "ShipTooLargeForStructure"
    );

    // Tatara: allows Rorqual (28352, group 883), rejects Dreadnought (group 485)
    assert!(resolve_upwell_dock_target(&ctx, tatara_id, sys_id, char_id, 28_352, 883).is_ok());
    assert_eq!(
        resolve_upwell_dock_target(&ctx, tatara_id, sys_id, char_id, 19_720, 485).unwrap_err().code,
        "ShipTooLargeForStructure"
    );

    // Fortizar: allows Dreadnought (485), rejects Titan (30) & Supercarrier (659)
    assert!(resolve_upwell_dock_target(&ctx, fort_id, sys_id, char_id, 19_720, 485).is_ok());
    assert_eq!(
        resolve_upwell_dock_target(&ctx, fort_id, sys_id, char_id, 11_567, 30).unwrap_err().code,
        "ShipTooLargeForStructure"
    );

    // Keepstar: allows Titan (30) & Supercarrier (659)
    assert!(resolve_upwell_dock_target(&ctx, keep_id, sys_id, char_id, 11_567, 30).is_ok());
    assert!(resolve_upwell_dock_target(&ctx, keep_id, sys_id, char_id, 23_919, 659).is_ok());
}

#[tokio::test]
async fn anchor_orbital_gantry_and_upgrade_to_customs_office() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    // Find a low-sec system with at least one planet (group 7)
    let (low_sys, planet) = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status > 0.0 && s.security_status < 0.45 && s.solar_system_id < 31_000_000)
        .find_map(|s| {
            ctx.sde
                .celestials_in_system(s.solar_system_id)
                .into_iter()
                .find(|c| c.group_id == 7)
                .map(|p| (s.solar_system_id, p))
        })
        .expect("low-sec planet");

    let char_id = 910_710_i64;
    let corp_id = 98_700_710_i64;
    let ship_id = 910_711_i64;
    let gantry_id = 910_712_i64;
    let upgrade_id = 910_713_i64;

    let sys_handle = ctx.systems.get_or_spawn(low_sys);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: planet.position + DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    ctx.db.with_mut_snapshot(|snap| {
        let ch = clone_char_with(snap, char_id, "PI Director", corp_id, low_sys, ship_id);
        snap.characters.insert(char_id, ch);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                ceo_id: char_id,
                ..Default::default()
            },
        );
        snap.items.insert(
            gantry_id,
            InventoryItemRecord {
                item_id: gantry_id,
                type_id: 3_962, // Customs Office Gantry
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            upgrade_id,
            InventoryItemRecord {
                item_id: upgrade_id,
                type_id: 2_233, // Customs Office upgrade item
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let mut session = SessionState::new(1);
    session.user_id = Some(char_id as u64);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.corp_role = corp_roles::ROLE_DIRECTOR;
    session.solar_system_id = Some(low_sys);
    session.ship_id = Some(ship_id);

    // Anchor orbital gantry at planet
    let PyRep::KeyVal(anc_kv) = handle_anchor_orbital(
        &ctx,
        &session,
        &[PyRep::Integer(gantry_id), PyRep::Integer(planet.celestial_id)],
    )
    .expect("anchor orbital gantry") else {
        panic!("expected keyval");
    };
    assert_eq!(anc_kv.get("orbitalID").and_then(|v| v.as_i64()), Some(gantry_id));
    assert_eq!(anc_kv.get("planetID").and_then(|v| v.as_i64()), Some(planet.celestial_id));

    // Upgrade gantry to full Customs Office (2233)
    let PyRep::KeyVal(upg_kv) = handle_upgrade_gantry(
        &ctx,
        &session,
        &[PyRep::Integer(gantry_id), PyRep::Integer(upgrade_id)],
    )
    .expect("upgrade gantry") else {
        panic!("expected keyval");
    };
    assert_eq!(upg_kv.get("orbitalID").and_then(|v| v.as_i64()), Some(gantry_id));
    assert_eq!(upg_kv.get("typeID").and_then(|v| v.as_i64()), Some(2_233));
}

#[tokio::test]
async fn resolve_upwell_dock_target_denies_npc_fobs_and_non_dockable_groups_allows_upwell_classes() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let sys_id = 30_000_142_u32;
    let char_id = 960_101_i64;
    let corp_id = 98_000_001_i64;

    let denied_types = [
        46_363_u32, // Blood Raider Sotiyo (group 1924 / FOB)
        46_364_u32, // Guristas Sotiyo (group 1924 / FOB)
        32_226_u32, // FW Infrastructure Hub
        81_826_u32, // Metenox Moon Drill (group 4744)
        35_841_u32, // Ansiblex Jump Gate
    ];
    let allowed_types = [
        35_832_u32, // Astrahus (Citadel group 1657)
        35_825_u32, // Raitaru (Engineering Complex group 1404)
        35_835_u32, // Athanor (Refinery group 1406)
    ];

    ctx.db.with_mut_snapshot(|snap| {
        let ch = clone_char_with(snap, char_id, "Dock Check Pilot", corp_id, sys_id, 960_102);
        snap.characters.insert(char_id, ch);
        for (idx, tid) in denied_types.iter().chain(allowed_types.iter()).enumerate() {
            let sid = 1_000_000_960_100 + idx as i64;
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: *tid,
                    owner_corp_id: corp_id,
                    solar_system_id: sys_id,
                    name: format!("Struct {tid}"),
                    position: [50_000.0 + (idx as f64) * 5_000.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    shield_hp: 1.0,
                    armor_hp: 1.0,
                    hull_hp: 1.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
        }
    });

    for (idx, tid) in denied_types.iter().enumerate() {
        let sid = 1_000_000_960_100 + idx as i64;
        let err = resolve_upwell_dock_target(&ctx, sid, sys_id, char_id, 587, 25)
            .expect_err(&format!("type {tid} should not allow docking"));
        assert_eq!(err.code, "StructureDoesNotAllowDocking", "type {tid}");
    }
    for (offset, tid) in allowed_types.iter().enumerate() {
        let sid = 1_000_000_960_100 + (denied_types.len() + offset) as i64;
        assert!(
            resolve_upwell_dock_target(&ctx, sid, sys_id, char_id, 587, 25).is_ok(),
            "dockable Upwell type {tid} should allow docking"
        );
    }
}
