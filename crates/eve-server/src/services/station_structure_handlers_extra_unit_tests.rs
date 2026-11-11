//! Extra branch-coverage and mutation-killing unit tests for `station_structure_handlers`.

#![forbid(unsafe_code)]

use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use crate::services::station_structure_handlers::{
    check_station_service_standing, effective_standing_with_skills,
    evaluate_station_service_standing_gate, fulcrum_alignment_inertia_multiplier,
    handle_anchor_orbital, handle_get_structure_info, handle_get_structures, handle_upgrade_gantry,
    load_station_standings_restrictions, resolve_upwell_dock_target,
    validate_npc_station_docking_gate,
};
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;

#[tokio::test]
async fn anchor_orbital_and_upgrade_gantry_error_branches_and_nearest_planet() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let (low_sys, planet) = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| {
            s.security_status > 0.0 && s.security_status < 0.45 && s.solar_system_id < 31_000_000
        })
        .find_map(|s| {
            ctx.sde
                .celestials_in_system(s.solar_system_id)
                .into_iter()
                .find(|c| c.group_id == 7)
                .map(|p| (s.solar_system_id, p))
        })
        .expect("low-sec planet");

    let char_id = 915_101_i64;
    let other_char = 915_102_i64;
    let npc_corp = 1_000_125_i64;
    let player_corp = 98_715_101_i64;
    let other_corp = 98_715_102_i64;
    let ship_id = 915_201_i64;
    let gantry_id = 915_202_i64;
    let non_gantry_id = 915_203_i64;
    let upgrade_id = 915_204_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let ch = clone_char_with(snap, char_id, "Orbital Tester", npc_corp, low_sys, ship_id);
        snap.characters.insert(char_id, ch);
        snap.corporations.insert(
            player_corp,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = player_corp;
                __corp.ceo_id = other_char;
                __corp },
        );
        snap.items.insert(
            non_gantry_id,
            InventoryItemRecord {
                item_id: non_gantry_id,
                type_id: 34, // Tritanium, not a gantry
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            gantry_id,
            InventoryItemRecord {
                item_id: gantry_id,
                type_id: 3_962,
                owner_id: other_char, // initially wrong owner
                location_id: ship_id,
                flag: InventoryFlag::Hangar, // initially wrong flag
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState::new(1);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);

    // 1. AnchorOrbital pre-checks
    assert_eq!(
        handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    // Thera (31000005) disallowed
    sess.solar_system_id = Some(31_000_005);
    assert_eq!(
        handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "AnchoringDisallowedInTheraOrShattered"
    );
    sess.solar_system_id = Some(low_sys);
    assert_eq!(
        handle_anchor_orbital(&ctx, &sess, &[]).unwrap_err().code,
        "MissingArgument"
    );
    assert_eq!(
        handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(non_gantry_id)])
            .unwrap_err()
            .code,
        "StarbaseRemoved"
    );
    sess.station_id = Some(60_003_760);
    assert_eq!(
        handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "NotInSpace"
    );
    sess.station_id = None;
    // High-sec disallowed (Jita 30000142)
    sess.solar_system_id = Some(30_000_142);
    assert_eq!(
        handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "CannotAnchorOrbitalInHighSec"
    );
    sess.solar_system_id = Some(low_sys);
    // NPC corp rejected
    assert_eq!(
        handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    // Switch character to player_corp without roles -> CrpAccessDenied
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().corporation_id = player_corp;
    });
    sess.corporation_id = Some(player_corp);
    assert_eq!(
        handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    // Grant Station Manager role
    sess.corp_role = corp_roles::ROLE_STATION_MANAGER;
    assert_eq!(
        handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    sess.ship_id = Some(ship_id);
    assert_eq!(
        handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Spawn ship far away from planet -> OrbitalOutOfRange; also test InvalidOrbitalPlanet
    let sys_handle = ctx.systems.get_or_spawn(low_sys);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: planet.position + DVec3::new(500_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        handle_anchor_orbital(
            &ctx,
            &sess,
            &[PyRep::Integer(gantry_id), PyRep::Integer(999_999_999)]
        )
        .unwrap_err()
        .code,
        "InvalidOrbitalPlanet"
    );
    assert_eq!(
        handle_anchor_orbital(
            &ctx,
            &sess,
            &[PyRep::Integer(gantry_id), PyRep::Integer(planet.celestial_id)]
        )
        .unwrap_err()
        .code,
        "OrbitalOutOfRange"
    );

    // Move ship within range of planet
    let near_pos = planet.position + DVec3::new(5_000.0, 0.0, 0.0);
    let _ = sys_handle.remove_ball(ship_id).await;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: near_pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // NotItemOwner -> ItemNotInCargo -> happy path with implicit nearest planet (1 arg)
    assert_eq!(
        handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "NotItemOwner"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&gantry_id).unwrap().owner_id = char_id;
    });
    assert_eq!(
        handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "ItemNotInCargo"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&gantry_id).unwrap().flag = InventoryFlag::Cargo;
    });
    let anchored = handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(gantry_id)]).unwrap();
    assert!(matches!(anchored, PyRep::KeyVal(_)));
    // Second anchor attempt of same item -> AlreadyAnchored
    assert_eq!(
        handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "AlreadyAnchored"
    );

    // 2. UpgradeGantry pre-checks and error branches
    let mut docked_sess = sess.clone();
    docked_sess.structure_id = Some(1_000_000_001);
    assert_eq!(
        handle_upgrade_gantry(&ctx, &docked_sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "MustBeInSpace"
    );
    assert_eq!(
        handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(999_888_777)])
            .unwrap_err()
            .code,
        "OrbitalNotFound"
    );
    assert_eq!(
        handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(non_gantry_id)])
            .unwrap_err()
            .code,
        "InvalidOrbitalGantry"
    );

    // Test OrbitalWrongSystem, InvalidOrbitalState, OrbitalOwnershipDenied
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&gantry_id).unwrap().solar_system_id = 30_000_142;
    });
    assert_eq!(
        handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "OrbitalWrongSystem"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&gantry_id).unwrap();
        st.solar_system_id = low_sys;
        st.state = "Destroyed".into();
    });
    assert_eq!(
        handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "InvalidOrbitalState"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&gantry_id).unwrap();
        st.state = "ShieldVulnerable".into();
        st.owner_corp_id = other_corp;
    });
    assert_eq!(
        handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "OrbitalOwnershipDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&gantry_id).unwrap().owner_corp_id = player_corp;
    });

    // Missing upgrade item in cargo -> CustomsOfficeItemRequired; nonexistent explicit arg -> CustomsOfficeItemRequired
    assert_eq!(
        handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "CustomsOfficeItemRequired"
    );
    assert_eq!(
        handle_upgrade_gantry(
            &ctx,
            &sess,
            &[PyRep::Integer(gantry_id), PyRep::Integer(888_777_666)]
        )
        .unwrap_err()
        .code,
        "CustomsOfficeItemRequired"
    );

    // Insert upgrade item stack of quantity 2 and call handle_upgrade_gantry without explicit item arg -> consumes 1, leaves 1
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            upgrade_id,
            InventoryItemRecord {
                item_id: upgrade_id,
                type_id: 2_233,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                ..Default::default()
            },
        );
    });
    let upg = handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(gantry_id)]).unwrap();
    assert!(matches!(upg, PyRep::KeyVal(_)));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&upgrade_id].quantity),
        1
    );
}

#[test]
fn get_structures_directory_info_standing_gates_and_fulcrum_timers() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let char_id = 915_301_i64;
    let outsider_id = 915_302_i64;
    let corp_id = 98_715_301_i64;
    let other_corp = 98_715_302_i64;
    let sys_id = 30_000_144_u32;
    let st_online = 1_000_000_980_001_i64;
    let st_destroyed = 1_000_000_980_002_i64;
    let st_anchoring = 1_000_000_980_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let ch1 = clone_char_with(snap, char_id, "Structure CEO", corp_id, sys_id, 100_000_001);
        let ch2 = clone_char_with(
            snap,
            outsider_id,
            "Outsider Pilot",
            other_corp,
            30_000_142,
            100_000_001,
        );
        snap.characters.insert(char_id, ch1);
        snap.characters.insert(outsider_id, ch2);
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.ceo_id = char_id;
                __corp },
        );
        for (sid, state) in [
            (st_online, "ShieldVulnerable"),
            (st_destroyed, "Destroyed"),
            (st_anchoring, "Anchoring"),
        ] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_832,
                    owner_corp_id: corp_id,
                    solar_system_id: sys_id,
                    name: format!("Citadel {sid}"),
                    position: [0.0, 0.0, 0.0],
                    state: state.into(),
                    quantum_core_installed: true,
                    fuel_blocks: 120,
                    shield_hp: 1.0,
                    armor_hp: 1.0,
                    hull_hp: 1.0,
                    reinforced_until_unix: if state == "Anchoring" { 4_000_000_000 } else { 0 },
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
        }
        let mut ext = load_ext_state(snap);
        ext.structure_profile_id.insert(st_online, 77);
        ext.pending_reinforce_schedule
            .insert(st_online, (20, 3, 1_800_086_400));
        save_ext_state(snap, &ext);
    });

    let mut ceo_sess = SessionState::new(1);
    ceo_sess.user_id = Some(char_id as u64);
    ceo_sess.character_id = Some(char_id);
    ceo_sess.corporation_id = Some(corp_id);
    ceo_sess.corp_role = corp_roles::ROLE_DIRECTOR;
    ceo_sess.solar_system_id = Some(sys_id);

    // handle_get_structures skips Destroyed and includes manager fields (fuelBlocks, profileID, pending reinforce)
    let PyRep::Dict(dir_entries) = handle_get_structures(&ctx, &mut ceo_sess, true).unwrap() else {
        panic!("expected Dict");
    };
    assert!(dir_entries.iter().any(|(k, _)| k.as_i64() == Some(st_online)));
    assert!(!dir_entries.iter().any(|(k, _)| k.as_i64() == Some(st_destroyed)));

    let info = handle_get_structure_info(&ctx, &mut ceo_sess, &[PyRep::Integer(st_online)]).unwrap();
    let PyRep::KeyVal(kv) = info else {
        panic!("expected KeyVal");
    };
    assert_eq!(kv.get("profileID"), Some(&PyRep::Integer(77)));
    assert_eq!(kv.get("fuelBlocks"), Some(&PyRep::Integer(120)));
    assert_eq!(kv.get("nextReinforceHour"), Some(&PyRep::Integer(20)));

    // handle_get_structure_info error paths
    let mut outsider_sess = SessionState::new(2);
    outsider_sess.user_id = Some(outsider_id as u64);
    outsider_sess.character_id = Some(outsider_id);
    outsider_sess.corporation_id = Some(other_corp);
    outsider_sess.solar_system_id = Some(30_000_142);

    assert_eq!(
        handle_get_structure_info(&ctx, &mut outsider_sess, &[])
            .unwrap_err()
            .code,
        "MissingStructureId"
    );
    assert_eq!(
        handle_get_structure_info(&ctx, &mut outsider_sess, &[PyRep::Integer(st_destroyed)])
            .unwrap_err()
            .code,
        "StructureNotFound"
    );
    assert_eq!(
        handle_get_structure_info(&ctx, &mut outsider_sess, &[PyRep::Integer(st_online)])
            .unwrap_err()
            .code,
        "StructureAccessDenied"
    );

    // resolve_upwell_dock_target state checks (Destroyed & Anchoring)
    assert_eq!(
        resolve_upwell_dock_target(&ctx, st_destroyed, sys_id, char_id, 587, 25)
            .unwrap_err()
            .code,
        "StructureDestroyed"
    );
    assert_eq!(
        resolve_upwell_dock_target(&ctx, st_anchoring, sys_id, char_id, 587, 25)
            .unwrap_err()
            .code,
        "StructureNotDockable"
    );

    // Fulcrum retribution timer blocks docking; direct-enlisted Guristas (500011) passes service 24 & Zarzakh alignment
    assert!(!load_station_standings_restrictions(&ctx.sde).is_empty());
    assert!(check_station_service_standing(&ctx, char_id, 60_003_760, 1, 1, 999_999_999, None).is_ok());
    assert_eq!(effective_standing_with_skills(0.0, 3, 3), 0.0);

    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.insert(char_id, 1_800_000_500);
    });
    assert_eq!(
        validate_npc_station_docking_gate(&ctx, char_id, 60_015_187)
            .unwrap_err()
            .code,
        "FulcrumRefusesTimerActive"
    );
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.remove(&char_id);
    });

    // StationNotFound and StationServiceUnavailable on evaluate_station_service_standing_gate
    assert_eq!(
        evaluate_station_service_standing_gate(&ctx, char_id, 999_999_999, 1)
            .unwrap_err()
            .code,
        "StationNotFound"
    );
    assert_eq!(
        evaluate_station_service_standing_gate(&ctx, char_id, 60_003_760, 1 << 30)
            .unwrap_err()
            .code,
        "StationServiceUnavailable"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime
            .direct_enlisted_chars
            .insert(char_id, 500_011);
        assert!(
            (fulcrum_alignment_inertia_multiplier(snap, char_id, None, 30_100_000) - 0.80).abs()
                < 1e-9
        );
    });
}

#[tokio::test]
async fn mutant_kill_anchor_orbital_and_upgrade_gantry_role_bitmasks_and_outputs() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    let (low_sys, planet) = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| {
            s.security_status > 0.0 && s.security_status < 0.45 && s.solar_system_id < 31_000_000
        })
        .find_map(|s| {
            ctx.sde
                .celestials_in_system(s.solar_system_id)
                .into_iter()
                .find(|c| c.group_id == 7)
                .map(|p| (s.solar_system_id, p))
        })
        .expect("low-sec planet");

    let player_corp = 98_715_201_i64;
    let npc_corp = 1_000_125_i64;
    let ceo_cid = 915_501_i64;
    let dir_cid = 915_502_i64;
    let sm_cid = 915_503_i64;
    let aud_cid = 915_504_i64;
    let npc_sess_dir_cid = 915_505_i64;
    let ship_id = 915_601_i64;
    let gantry_id = 915_602_i64;
    let upgrade_id = 915_603_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = player_corp;
        corp.ceo_id = ceo_cid;
        corp.member_roles.insert(dir_cid, corp_roles::ROLE_DIRECTOR);
        corp.member_roles
            .insert(sm_cid, corp_roles::ROLE_STATION_MANAGER);
        corp.member_roles.insert(aud_cid, corp_roles::ROLE_AUDITOR);
        snap.corporations.insert(player_corp, corp);

        for (cid, co) in [
            (ceo_cid, player_corp),
            (dir_cid, player_corp),
            (sm_cid, player_corp),
            (aud_cid, player_corp),
            (npc_sess_dir_cid, npc_corp),
        ] {
            let ch = clone_char_with(snap, cid, "OrbRoleTester", co, low_sys, ship_id);
            snap.characters.insert(cid, ch);
        }
    });

    let sys_handle = ctx.systems.get_or_spawn(low_sys);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: planet.position + DVec3::new(5_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let mk_sess = |cid: i64, co: i64, crole: u64| {
        let mut s = SessionState::new(cid as u64);
        s.user_id = Some(cid as u64);
        s.character_id = Some(cid);
        s.corporation_id = Some(co);
        s.solar_system_id = Some(low_sys);
        s.ship_id = Some(ship_id);
        s.role = 0;
        s.corp_role = crole;
        s
    };

    let orbital_tag = format!("Orbital:{}", planet.celestial_id);
    let reset_items_and_clear_orbital = |snap: &mut eve_db::WorldStateSnapshot, owner_cid: i64, owner_corp: i64| {
        snap.structures.remove(&gantry_id);
        snap.structures.retain(|_, st| st.name != orbital_tag);
        snap.items.insert(
            gantry_id,
            InventoryItemRecord {
                item_id: gantry_id,
                type_id: 3_962,
                owner_id: owner_cid,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            upgrade_id,
            InventoryItemRecord {
                item_id: upgrade_id,
                type_id: 2_233,
                owner_id: owner_corp,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    };

    // 1. Auditor (both sess.corp_role and DB member_roles = ROLE_AUDITOR, not CEO) -> CrpAccessDenied on both
    ctx.db.with_mut_snapshot(|snap| reset_items_and_clear_orbital(snap, aud_cid, player_corp));
    let aud_sess = mk_sess(aud_cid, player_corp, corp_roles::ROLE_AUDITOR);
    assert_eq!(
        handle_anchor_orbital(&ctx, &aud_sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        handle_upgrade_gantry(&ctx, &aud_sess, &[PyRep::Integer(gantry_id)])
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );

    // 2. NPC corp character with session ROLE_DIRECTOR owns anchored gantry -> passes upgrade_gantry L69 role check
    ctx.db.with_mut_snapshot(|snap| reset_items_and_clear_orbital(snap, npc_sess_dir_cid, npc_corp));
    let npc_dir_sess = mk_sess(npc_sess_dir_cid, npc_corp, corp_roles::ROLE_DIRECTOR);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&gantry_id) {
            it.location_id = planet.celestial_id;
            it.flag = InventoryFlag::None;
        }
        snap.structures.insert(
            gantry_id,
            UpwellStructureRecord {
                structure_id: gantry_id,
                type_id: 3_962,
                owner_corp_id: npc_corp,
                solar_system_id: low_sys,
                name: orbital_tag.clone(),
                position: (planet.position + DVec3::new(5_000.0, 0.0, 0.0)).to_array(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });
    let npc_upg = handle_upgrade_gantry(&ctx, &npc_dir_sess, &[PyRep::Integer(gantry_id)]).unwrap();
    assert_eq!(npc_upg.dict_get("typeID"), Some(&PyRep::Integer(2_233)));

    // 3. CEO, Director (DB-only, sess.corp_role = 0), and StationManager (DB-only, sess.corp_role = 0)
    // all pass both handle_anchor_orbital and handle_upgrade_gantry with valid return payloads
    for cid in [ceo_cid, dir_cid, sm_cid] {
        ctx.db.with_mut_snapshot(|snap| reset_items_and_clear_orbital(snap, cid, player_corp));
        let sess = mk_sess(cid, player_corp, 0);
        let anchored = handle_anchor_orbital(&ctx, &sess, &[PyRep::Integer(gantry_id)]).unwrap();
        assert_eq!(anchored.dict_get("orbitalID"), Some(&PyRep::Integer(gantry_id)));
        assert_eq!(anchored.dict_get("typeID"), Some(&PyRep::Integer(3_962)));
        let upg = handle_upgrade_gantry(&ctx, &sess, &[PyRep::Integer(gantry_id)]).unwrap();
        assert_eq!(upg.dict_get("orbitalID"), Some(&PyRep::Integer(gantry_id)));
        assert_eq!(upg.dict_get("typeID"), Some(&PyRep::Integer(2_233)));
    }
}
