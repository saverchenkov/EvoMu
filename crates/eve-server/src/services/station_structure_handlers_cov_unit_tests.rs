//! Focused unit tests for uncovered branches in `station_structure_handlers`
//! (`handle_upgrade_gantry`, `handle_anchor_orbital`, `handle_get_structures`,
//! `handle_get_structure_info`, `check_station_service_standing`,
//! `validate_npc_station_docking_gate`, `evaluate_station_service_standing_gate`,
//! and `resolve_upwell_dock_target`).

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_with_clock;
use crate::services::station_structure::structure_upwell_ops::{save_ext_state, StructureAccessProfile};
use eve_db::types::UpwellStructureRecord;
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, WorldStateSnapshot};
use eve_proto::PyRep;
use std::collections::HashSet;

fn insert_char(snap: &mut WorldStateSnapshot, char_id: i64, user_id: u64, corp_id: i64, balance: IskCents) {
    let mut ch = snap.characters.values().next().cloned().expect("seed char");
    ch.character_id = char_id;
    ch.user_id = user_id;
    ch.name = format!("Pilot_{char_id}");
    ch.corporation_id = corp_id;
    ch.solar_system_id = 30_000_142;
    ch.station_id = Some(60_003_760);
    ch.structure_id = None;
    ch.balance = balance;
    snap.characters.insert(char_id, ch);
}

fn setup_orbital_and_structure_world() -> (ServerContext, i64, i64, i64, i64, i64) {
    let ctx = test_ctx_with_clock(1_700_000_000);
    let ceo_id = 96_710_001i64;
    let member_id = 96_710_002i64;
    let outsider_id = 96_710_003i64;
    let corp_id = 98_710_001i64;
    let gantry_id = 96_710_100i64;
    let customs_office_id = 96_710_101i64;
    let skyhook_id = 96_710_102i64;
    let structure_id = 1_040_000_000_777i64;

    ctx.db.with_mut_snapshot(|snap| {
        insert_char(snap, ceo_id, 9101, corp_id, IskCents::from_f64_isk(500_000_000.0));
        insert_char(snap, member_id, 9102, corp_id, IskCents::from_f64_isk(100_000_000.0));
        insert_char(snap, outsider_id, 9103, 98_710_999, IskCents::from_f64_isk(100_000_000.0));

        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Orbital Logistics".into();
            __corp.ticker = "ORBL".into();
            __corp.ceo_id = ceo_id;
            __corp.set_division_balance(1, IskCents::from_f64_isk(500_000_000.0));
            __corp };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);

        // Customs Office Gantry in space (type 3962)
        snap.items.insert(
            gantry_id,
            InventoryItemRecord {
                item_id: gantry_id,
                type_id: 3962,
                owner_id: corp_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Existing Customs Office (type 2233)
        snap.items.insert(
            customs_office_id,
            InventoryItemRecord {
                item_id: customs_office_id,
                type_id: 2233,
                owner_id: corp_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Unanchored Skyhook in active ship cargo
        snap.items.insert(
            skyhook_id,
            InventoryItemRecord {
                item_id: skyhook_id,
                type_id: 81080,
                owner_id: ceo_id,
                location_id: 96_710_500,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            96_710_500,
            InventoryItemRecord {
                item_id: 96_710_500,
                type_id: 587,
                owner_id: ceo_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );

        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                solar_system_id: 30_000_142,
                type_id: 35832, // Astrahus
                owner_corp_id: corp_id,
                name: "Test Astrahus".into(),
                state: "ShieldVulnerable".into(),
                position: [10_000.0, 20_000.0, 30_000.0],
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
    });

    (ctx, ceo_id, member_id, outsider_id, corp_id, structure_id)
}

#[tokio::test]
async fn upgrade_gantry_and_anchor_orbital_validation_and_success_paths() {
    let (ctx, ceo_id, member_id, outsider_id, corp_id, _) = setup_orbital_and_structure_world();

    let sys_h = ctx.systems.get_or_spawn(30_000_142);
    let _ = sys_h
        .spawn_ball_with_meta(
            eve_destiny::Ball {
                id: 96_710_500,
                type_id: 587,
                position: glam::DVec3::new(1_000.0, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
            eve_destiny::SlimItemMeta {
                owner_id: ceo_id,
                char_id: Some(ceo_id),
                corp_id,
                group_id: 25,
                category_id: 6,
                ..Default::default()
            },
        )
        .await;

    // Anchor gantry structure & place Customs Office upgrade item in active ship cargo
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            96_710_100,
            UpwellStructureRecord {
                structure_id: 96_710_100,
                solar_system_id: 30_000_142,
                type_id: 3962,
                owner_corp_id: corp_id,
                name: "Gantry".into(),
                state: "OnliningVulnerable".into(),
                position: [0.0, 0.0, 0.0],
                ..Default::default()
            },
        );
        snap.items.insert(
            96_710_109,
            InventoryItemRecord {
                item_id: 96_710_109,
                type_id: 2233,
                owner_id: ceo_id,
                location_id: 96_710_500,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let ceo_sess = SessionState {
        user_id: Some(9101),
        character_id: Some(ceo_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        ship_id: Some(96_710_500),
        ..Default::default()
    };
    let member_sess = SessionState {
        user_id: Some(9102),
        character_id: Some(member_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        ship_id: Some(96_710_500),
        ..Default::default()
    };
    let outsider_sess = SessionState {
        user_id: Some(9103),
        character_id: Some(outsider_id),
        corporation_id: Some(98_710_999),
        solar_system_id: Some(30_000_142),
        ship_id: Some(96_710_500),
        ..Default::default()
    };

    // Upgrade non-existent gantry fails
    let err = handle_upgrade_gantry(&ctx, &ceo_sess, &[PyRep::Integer(999_999_999)]).expect_err("missing gantry");
    assert_eq!(err.code, "OrbitalNotFound");

    // Upgrade wrong item type (existing customs office 96_710_101 is type 2233, not 3962) fails
    let err = handle_upgrade_gantry(&ctx, &ceo_sess, &[PyRep::Integer(96_710_101)]).expect_err("already upgraded");
    assert_eq!(err.code, "InvalidOrbitalGantry");

    // Upgrade by non-owning corp rejected
    let err = handle_upgrade_gantry(&ctx, &outsider_sess, &[PyRep::Integer(96_710_100)]).expect_err("wrong corp");
    assert_eq!(err.code, "CrpAccessDenied");

    // Upgrade by non-privileged corp member rejected
    let err = handle_upgrade_gantry(&ctx, &member_sess, &[PyRep::Integer(96_710_100)]).expect_err("no role");
    assert_eq!(err.code, "CrpAccessDenied");

    // CEO upgrades gantry 96_710_100 -> becomes Customs Office (type 2233) & updates structure entry
    let res = handle_upgrade_gantry(&ctx, &ceo_sess, &[PyRep::Integer(96_710_100)]).expect("CEO gantry upgrade succeeds");
    assert!(matches!(res, PyRep::KeyVal(_)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&96_710_100].type_id, 2233);
        assert_eq!(snap.structures[&96_710_100].type_id, 2233);
    });

    // AnchorOrbital on non-gantry item returns StarbaseRemoved; high-sec returns CannotAnchorOrbitalInHighSec;
    // low-sec gantry anchors cleanly
    let err = handle_anchor_orbital(&ctx, &ceo_sess, &[PyRep::Integer(96_710_500), PyRep::Integer(40_000_005)])
        .expect_err("non-gantry returns StarbaseRemoved");
    assert_eq!(err.code, "StarbaseRemoved");

    let lowsec_sys = (30_002_537u32, 40_161_469i64);
    let unanchored_gantry_id = 96_710_115i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            unanchored_gantry_id,
            InventoryItemRecord {
                item_id: unanchored_gantry_id,
                type_id: 3962,
                owner_id: ceo_id,
                location_id: 96_710_500,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // High-sec Jita blocked
    let err = handle_anchor_orbital(&ctx, &ceo_sess, &[PyRep::Integer(unanchored_gantry_id), PyRep::Integer(40_000_005)])
        .expect_err("highsec gantry anchoring blocked");
    assert_eq!(err.code, "CannotAnchorOrbitalInHighSec");

    // Non-director in low-sec blocked
    let mut member_lowsec = member_sess.clone();
    member_lowsec.solar_system_id = Some(lowsec_sys.0);
    let err = handle_anchor_orbital(&ctx, &member_lowsec, &[PyRep::Integer(unanchored_gantry_id), PyRep::Integer(lowsec_sys.1)])
        .expect_err("member lacks director role");
    assert_eq!(err.code, "CrpAccessDenied");

    // CEO in low-sec with active ship ball spawned near planet anchors gantry around planet
    let planet_pos = ctx
        .sde
        .celestials_in_system(lowsec_sys.0)
        .into_iter()
        .find(|c| c.celestial_id == lowsec_sys.1)
        .map(|c| c.position)
        .unwrap_or(glam::DVec3::ZERO);
    let lowsec_h = ctx.systems.get_or_spawn(lowsec_sys.0);
    let _ = lowsec_h
        .spawn_ball_with_meta(
            eve_destiny::Ball {
                id: 96_710_500,
                type_id: 587,
                position: planet_pos,
                velocity: glam::DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
            eve_destiny::SlimItemMeta {
                owner_id: ceo_id,
                char_id: Some(ceo_id),
                corp_id,
                group_id: 25,
                category_id: 6,
                ..Default::default()
            },
        )
        .await;
    let mut ceo_lowsec = ceo_sess.clone();
    ceo_lowsec.solar_system_id = Some(lowsec_sys.0);
    let anchored = handle_anchor_orbital(
        &ctx,
        &ceo_lowsec,
        &[PyRep::Integer(unanchored_gantry_id), PyRep::Integer(lowsec_sys.1)],
    )
    .expect("gantry anchors in low-sec");
    assert!(matches!(anchored, PyRep::KeyVal(_)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&unanchored_gantry_id].location_id, lowsec_sys.1);
        assert!(snap.structures.contains_key(&unanchored_gantry_id));
    });
}

#[test]
fn structure_queries_upwell_dock_targets_and_station_standing_gates() {
    let (ctx, ceo_id, _, outsider_id, corp_id, structure_id) = setup_orbital_and_structure_world();

    let mut ceo_sess = SessionState {
        user_id: Some(9101),
        character_id: Some(ceo_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        ..Default::default()
    };

    // GetStructures & GetStructureInfo
    let list_rep = handle_get_structures(&ctx, &mut ceo_sess, false).expect("structures listed");
    assert!(matches!(list_rep, PyRep::Dict(ref d) if !d.is_empty()));

    let info_rep = handle_get_structure_info(&ctx, &mut ceo_sess, &[PyRep::Integer(structure_id)])
        .expect("structure info");
    assert!(matches!(info_rep, PyRep::KeyVal(_)));

    let err_missing = handle_get_structure_info(&ctx, &mut ceo_sess, &[PyRep::Integer(1_040_999_999_999)])
        .expect_err("unknown structure");
    assert_eq!(err_missing.code, "StructureNotFound");

    // resolve_upwell_dock_target: hull class gate on Medium Astrahus (35832):
    // Dreadnought (group 485) is capital-sized -> blocked, while Freighter (513) is allowed
    let err = resolve_upwell_dock_target(&ctx, structure_id, 30_000_142, ceo_id, 19720, 485)
        .expect_err("dreadnought cannot dock at medium Astrahus");
    assert_eq!(err.code, "ShipTooLargeForStructure");

    // resolve_upwell_dock_target: destroyed structure blocked
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&structure_id) {
            st.state = "Destroyed".into();
        }
        let mut ext = structure_upwell_ops::load_ext_state(snap);
        ext.access_profiles.insert(
            structure_id,
            StructureAccessProfile {
                profile_id: 7001,
                owner_corp_id: corp_id,
                allowed_corporations: HashSet::from([corp_id]),
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
    });
    let err = resolve_upwell_dock_target(&ctx, structure_id, 30_000_142, ceo_id, 587, 25)
        .expect_err("destroyed structure denies docking");
    assert_eq!(err.code, "StructureDestroyed");

    // Wrong solar system denied
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&structure_id) {
            st.state = "ShieldVulnerable".into();
        }
    });
    let err = resolve_upwell_dock_target(&ctx, structure_id, 30_002_187, ceo_id, 587, 25)
        .expect_err("wrong solar system");
    assert_eq!(err.code, "DockTargetWrongSystem");

    // Outsider without ACL denied; CEO with corp ACL allowed
    let err = resolve_upwell_dock_target(&ctx, structure_id, 30_000_142, outsider_id, 587, 25)
        .expect_err("outsider denied by ACL");
    assert_eq!(err.code, "StructureDockingAccessDenied");

    let pos = resolve_upwell_dock_target(&ctx, structure_id, 30_000_142, ceo_id, 587, 25)
        .expect("CEO allowed to dock");
    assert_eq!(pos, glam::DVec3::new(10_000.0, 20_000.0, 30_000.0));

    // Fulcrum docking gates: militia blocked & weapon timer blocked
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.war_faction_id = Some(500_001);
        }
    });
    let err = validate_npc_station_docking_gate(&ctx, ceo_id, 60_015_187)
        .expect_err("empire militia banned from Fulcrum");
    assert!(err.message.contains("FulcrumRefusesEmpireMilitia") || err.code == "FulcrumRefusesEmpireMilitia");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.war_faction_id = None;
        }
    });
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.insert(ceo_id, ctx.now_unix() + 300);
    });
    let err = validate_npc_station_docking_gate(&ctx, ceo_id, 60_015_187)
        .expect_err("retribution timer active");
    assert!(err.message.contains("FulcrumRefusesTimerActive") || err.code == "UserError");

    // evaluate_station_service_standing_gate on unknown station -> StationNotFound
    let err = evaluate_station_service_standing_gate(&ctx, ceo_id, 60_999_999, 13)
        .expect_err("missing station");
    assert_eq!(err.code, "StationNotFound");

    // fulcrum_alignment_inertia_multiplier returns 0.80 at Zarzakh Fulcrum station (60_015_187) for pirate-enlisted
    // pilots (500010 Angel / 500011 Guristas) and 1.0 elsewhere
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.war_faction_id = Some(500_010);
        }
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(fulcrum_alignment_inertia_multiplier(snap, ceo_id, Some(60_015_187), 587), 0.80);
        assert_eq!(fulcrum_alignment_inertia_multiplier(snap, ceo_id, Some(60_003_760), 587), 1.0);
    });
}
