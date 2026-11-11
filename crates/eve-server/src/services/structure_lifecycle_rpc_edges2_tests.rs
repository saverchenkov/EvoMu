//! Additional edge-case and branch coverage tests for `services::structure_lifecycle_rpc`.

#![forbid(unsafe_code)]

use super::structure_lifecycle_rpc_tests::{
    clone_type_with, make_session, seed_astrahus_type, seed_char_corp_ship, seed_structure,
    test_ctx,
};
use super::*;
use crate::services::station_structure::structure_upwell_ops::load_ext_state;
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anchor_errors_pochven_flex_ansiblex_and_coordinate_forms() {
    let mut ctx = test_ctx();
    let char_id = 960_101_i64;
    let corp_id = 98_000_601_i64;
    let ship_id = 100_601_i64;
    let sys_id = 30_000_142_u32; // Jita (highsec)
    seed_astrahus_type(&mut ctx, 35_832, false);

    // Seed an Ansiblex Jump Gate (type 35841, group 1408, category 65) + Pochven system
    clone_type_with(
        &mut ctx.sde,
        35_832,
        35_841,
        1408, // Jump Gate -> Flex hull class
        life::CATEGORY_STRUCTURE,
        50_000.0,
        0.0,
        &[(life::ATTR_SHIELD_CAPACITY, 500_000.0)],
    );
    let pochven_sys = 30_002_001_u32;
    let null_sys_1 = 30_002_002_u32;
    let null_sys_2 = 30_002_003_u32;
    {
        let sde = std::sync::Arc::make_mut(&mut ctx.sde);
        let template = sde.solar_systems.values().next().cloned().unwrap();
        let mut poch = template.clone();
        poch.solar_system_id = pochven_sys;
        poch.region_id = 10_000_070;
        poch.security_status = -0.5;
        sde.solar_systems.insert(pochven_sys, poch);

        let mut n1 = template.clone();
        n1.solar_system_id = null_sys_1;
        n1.region_id = 10_000_050;
        n1.security_status = -0.5;
        n1.galactic_pos_ly = [0.0, 0.0, 0.0];
        sde.solar_systems.insert(null_sys_1, n1);

        let mut n2 = template.clone();
        n2.solar_system_id = null_sys_2;
        n2.region_id = 10_000_050;
        n2.security_status = -0.5;
        n2.galactic_pos_ly = [10.0, 0.0, 0.0];
        sde.solar_systems.insert(null_sys_2, n2);
    }

    seed_char_corp_ship(&ctx, char_id, corp_id, sys_id, ship_id, true);
    let item_id = 880_101_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: 35_832,
                owner_id: 999_888, // wrong owner
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    // 1. ShipDocked when station_id is Some
    let mut docked_sess = make_session(char_id, corp_id, sys_id, Some(ship_id), corp_roles::ROLE_DIRECTOR);
    docked_sess.station_id = Some(60_003_760);
    assert_eq!(
        handle_anchor(&ctx, &mut docked_sess, &[PyRep::Integer(item_id)])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );

    // Spawn ship ball in space
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 670,
                position: DVec3::new(100_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    let mut sess = make_session(char_id, corp_id, sys_id, Some(ship_id), corp_roles::ROLE_DIRECTOR);

    // 1b. NpcCorpCannotAnchorStructure when pilot is in an NPC corp (< 98_000_000)
    let mut npc_sess = make_session(char_id, 1_000_009, sys_id, Some(ship_id), corp_roles::ROLE_DIRECTOR);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = 1_000_009;
        }
        if let Some(it) = snap.items.get_mut(&item_id) {
            it.owner_id = char_id;
        }
    });
    assert_eq!(
        handle_anchor(&ctx, &mut npc_sess, &[PyRep::Integer(item_id)])
            .await
            .unwrap_err()
            .code,
        "NpcCorpCannotAnchorStructure"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        if let Some(it) = snap.items.get_mut(&item_id) {
            it.owner_id = 999_888;
        }
    });

    // 2. ItemOwnershipViolation
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(item_id)])
            .await
            .unwrap_err()
            .code,
        "ItemOwnershipViolation"
    );

    // 3. ItemNotInCargo
    ctx.db.with_mut_snapshot(|snap| {
        let mut it = snap.items.get(&item_id).unwrap().clone();
        it.owner_id = char_id;
        it.flag = InventoryFlag::Hangar;
        snap.items.insert(item_id, it);
    });
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(item_id)])
            .await
            .unwrap_err()
            .code,
        "ItemNotInCargo"
    );

    // 4. AnchoringDisallowedInPochven
    ctx.db.with_mut_snapshot(|snap| {
        let mut it = snap.items.get(&item_id).unwrap().clone();
        it.flag = InventoryFlag::Cargo;
        snap.items.insert(item_id, it);
    });
    sess.solar_system_id = Some(pochven_sys);
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(item_id)])
            .await
            .unwrap_err()
            .code,
        "AnchoringDisallowedInPochven"
    );

    // 5. CannotAnchorFlexOutsideSovNull when anchoring Jump Gate 35841 in highsec
    sess.solar_system_id = Some(sys_id);
    assert_eq!(
        handle_anchor(&ctx, &mut sess, &[PyRep::Integer(35_841)])
            .await
            .unwrap_err()
            .code,
        "CannotAnchorFlexOutsideSovNull"
    );

    // 6. InvalidAnchorCoordinates with non-numeric List coords
    assert_eq!(
        handle_anchor(
            &ctx,
            &mut sess,
            &[
                PyRep::Integer(item_id),
                PyRep::List(vec![
                    PyRep::String("bad".into()),
                    PyRep::Real(0.0),
                    PyRep::Real(0.0),
                ]),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidAnchorCoordinates"
    );

    // 7. InsufficientCorpRoles when pilot lacks role in non-CEO corp
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = snap.corporations.get(&corp_id).unwrap().clone();
        corp.ceo_id = char_id + 12345;
        corp.member_roles.remove(&char_id);
        snap.corporations.insert(corp_id, corp);
    });
    let mut no_role_sess = make_session(char_id, corp_id, sys_id, Some(ship_id), 0);
    assert_eq!(
        handle_anchor(
            &ctx,
            &mut no_role_sess,
            &[
                PyRep::Integer(item_id),
                PyRep::List(vec![PyRep::Real(200_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientCorpRoles"
    );

    // Restore CEO role, seed default profile for corp_id, verify trade-hub block in Jita, then move to non-hub highsec (30000144)
    // and anchor using (day, hour) reinforce window tuple + default profile attachment
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = snap.corporations.get(&corp_id).unwrap().clone();
        corp.ceo_id = char_id;
        snap.corporations.insert(corp_id, corp);
        let mut ext = load_ext_state(snap);
        ext.profiles_by_id.insert(
            901,
            crate::services::station_structure::structure_upwell_ops::StructureAccessProfile {
                profile_id: 901,
                owner_corp_id: corp_id,
                name: "Corp Default".into(),
                is_default: true,
                ..Default::default()
            },
        );
        crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });
    assert_eq!(
        handle_anchor(
            &ctx,
            &mut sess,
            &[
                PyRep::Integer(item_id),
                PyRep::List(vec![
                    PyRep::Real(160_000.0),
                    PyRep::Real(100.0),
                    PyRep::Real(-50.0),
                ]),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "AnchoringDisallowedInTradeHub"
    );
    let anchor_sys_id = 30_000_144_u32;
    let _ = sys_h.remove_ball(ship_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = anchor_sys_id;
        }
    });
    sess.solar_system_id = Some(anchor_sys_id);
    let anchor_sys_h = ctx.systems.get_or_spawn(anchor_sys_id);
    anchor_sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 670,
                position: DVec3::new(100_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    let anchor_res = handle_anchor(
        &ctx,
        &mut sess,
        &[
            PyRep::Integer(item_id),
            PyRep::List(vec![
                PyRep::Real(160_000.0),
                PyRep::Real(100.0),
                PyRep::Real(-50.0),
            ]),
            PyRep::Integer(0),
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(19)]), // (day=3, hour=19)
            PyRep::String("Edge Citadel".into()),
        ],
    )
    .await
    .unwrap();
    let PyRep::KeyVal(anchor_kv) = anchor_res else {
        panic!("expected KeyVal from handle_anchor, got {anchor_res:?}");
    };
    let anchored_id = anchor_kv
        .get("structureID")
        .and_then(|v| v.as_i64())
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        let st = snap.structures.get(&anchored_id).unwrap();
        assert_eq!(st.name, "Edge Citadel");
        let ext = load_ext_state(snap);
        assert!(ext.structure_profile_id.contains_key(&anchored_id));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attack_precheck_special_types_damage_and_ball_sync_edges() {
    let mut ctx = test_ctx();
    let attacker_id = 960_201_i64;
    let owner_id = 960_202_i64;
    let att_corp = 98_000_602_i64;
    let def_corp = 98_000_603_i64;
    let sys_id = 30_000_142_u32; // highsec
    let lowsec_sys = 30_002_004_u32;
    let now = ctx.now_unix();

    seed_astrahus_type(&mut ctx, 35_832, false);
    {
        let sde = std::sync::Arc::make_mut(&mut ctx.sde);
        let mut low = sde.solar_systems.get(&sys_id).unwrap().clone();
        low.solar_system_id = lowsec_sys;
        low.security_status = 0.2;
        sde.solar_systems.insert(lowsec_sys, low);
    }
    seed_char_corp_ship(&ctx, attacker_id, att_corp, sys_id, 100_602, true);
    seed_char_corp_ship(&ctx, owner_id, def_corp, sys_id, 100_603, true);

    // Non-structure ball returns Ok(false)
    assert!(!validate_upwell_structure_attack_precheck(&ctx, sys_id, attacker_id, 777_777, now).unwrap());

    // Wrong system -> StructureWrongSystem
    let st_id = 1_000_000_960_001_i64;
    seed_structure(&ctx, st_id, 35_832, def_corp, lowsec_sys, "ShieldVulnerable", 10);
    assert_eq!(
        validate_upwell_structure_attack_precheck(&ctx, sys_id, attacker_id, st_id, now)
            .unwrap_err()
            .code,
        "StructureWrongSystem"
    );

    // Destroyed -> StructureAlreadyDestroyed
    seed_structure(&ctx, st_id, 35_832, def_corp, sys_id, "Destroyed", 10);
    assert_eq!(
        validate_upwell_structure_attack_precheck(&ctx, sys_id, attacker_id, st_id, now)
            .unwrap_err()
            .code,
        "StructureAlreadyDestroyed"
    );

    // Friendly corp structure -> CannotAttackFriendlyStructure
    seed_structure(&ctx, st_id, 35_832, att_corp, sys_id, "ShieldVulnerable", 10);
    assert_eq!(
        validate_upwell_structure_attack_precheck(&ctx, sys_id, attacker_id, st_id, now)
            .unwrap_err()
            .code,
        "CannotAttackFriendlyStructure"
    );

    // Highsec structure without active war -> IllegalHighSecStructureAttack
    seed_structure(&ctx, st_id, 35_832, def_corp, sys_id, "ShieldVulnerable", 10);
    assert_eq!(
        validate_upwell_structure_attack_precheck(&ctx, sys_id, attacker_id, st_id, now)
            .unwrap_err()
            .code,
        "IllegalHighSecStructureAttack"
    );

    // Reinforced structure -> StructureReinforcedInvulnerable
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = snap.structures.get(&st_id).unwrap().clone();
        st.state = "ArmorReinforced".into();
        st.reinforced_until_unix = now + 3600;
        snap.structures.insert(st_id, st);
    });
    assert_eq!(
        validate_upwell_structure_attack_precheck(&ctx, sys_id, attacker_id, st_id, now)
            .unwrap_err()
            .code,
        "StructureReinforcedInvulnerable"
    );

    // Skyhook ArmorReinforced -> StructureInvulnerable
    let sky_id = 1_000_000_960_002_i64;
    seed_structure(
        &ctx,
        sky_id,
        crate::services::sov_services::TYPE_ORBITAL_SKYHOOK,
        def_corp,
        lowsec_sys,
        "ArmorReinforced",
        0,
    );
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = snap.structures.get(&sky_id).unwrap().clone();
        st.reinforced_until_unix = now + 3600;
        snap.structures.insert(sky_id, st);
    });
    assert_eq!(
        validate_upwell_structure_attack_precheck(&ctx, lowsec_sys, attacker_id, sky_id, now)
            .unwrap_err()
            .code,
        "StructureInvulnerable"
    );

    // Pirate Stronghold FOB ArmorReinforced -> FobArmorReinforced
    let fob_id = 1_000_000_960_003_i64;
    seed_structure(
        &ctx,
        fob_id,
        79172,
        def_corp,
        lowsec_sys,
        "ArmorReinforced",
        0,
    );
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = snap.structures.get(&fob_id).unwrap().clone();
        st.reinforced_until_unix = now + 3600;
        snap.structures.insert(fob_id, st);
    });
    assert_eq!(
        validate_upwell_structure_attack_precheck(&ctx, lowsec_sys, attacker_id, fob_id, now)
            .unwrap_err()
            .code,
        "FobArmorReinforced"
    );

    // FW Supply Site requires militia membership (war_faction_id) & opposing faction
    let fw_id = 1_000_000_960_004_i64;
    seed_structure(&ctx, fw_id, 35_832, def_corp, lowsec_sys, "ShieldVulnerable", 0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.supply_sites.insert(fw_id, (lowsec_sys, "Supply".into(), 500_001));
    });
    assert_eq!(
        validate_upwell_structure_attack_precheck(&ctx, lowsec_sys, attacker_id, fw_id, now)
            .unwrap_err()
            .code,
        "NotEnlistedInMilitia"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&attacker_id).unwrap().clone();
        ch.war_faction_id = Some(500_001);
        snap.characters.insert(attacker_id, ch);
    });
    assert_eq!(
        validate_upwell_structure_attack_precheck(&ctx, lowsec_sys, attacker_id, fw_id, now)
            .unwrap_err()
            .code,
        "CannotAttackFriendlyStructure"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&attacker_id).unwrap().clone();
        ch.war_faction_id = Some(500_002);
        snap.characters.insert(attacker_id, ch);
    });
    assert!(validate_upwell_structure_attack_precheck(&ctx, lowsec_sys, attacker_id, fw_id, now).unwrap());

    // ensure_structure_ball_in_space spawns ball & apply_live_combat_damage_to_upwell_structure reduces shield
    let low_st = 1_000_000_960_005_i64;
    seed_structure(&ctx, low_st, 35_832, def_corp, lowsec_sys, "ShieldVulnerable", 10);
    let sys_h = ctx.systems.get_or_spawn(lowsec_sys);
    ensure_structure_ball_in_space(&ctx, lowsec_sys, low_st).await;
    let dmg_res = apply_live_combat_damage_to_upwell_structure(
        &ctx,
        &sys_h,
        lowsec_sys,
        attacker_id,
        low_st,
        DamageVector {
            em: 2_000.0,
            thermal: 0.0,
            kinetic: 0.0,
            explosive: 0.0,
        },
        1.0,
        now,
    )
    .await
    .unwrap()
    .expect("applied combat damage");
    assert!(dmg_res.applied_damage > 0.0);
    assert!(!dmg_res.destroyed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fit_move_rigs_charges_services_and_online_offline_edges() {
    let mut ctx = test_ctx();
    let char_id = 960_301_i64;
    let corp_id = 98_000_604_i64;
    let ship_id = 100_604_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 1_000_000_960_010_i64;

    seed_astrahus_type(&mut ctx, 35_832, false);
    std::sync::Arc::make_mut(&mut ctx.sde)
        .types
        .get_mut(&35_832)
        .unwrap()
        .default_attributes
        .insert(3101, 56_201.0); // ATTR_REQUIRES_DEED_TYPE -> quantum core type 56201
    // Rig with wrong rig size (size 1 instead of 2) & matching rig (size 2)
    clone_type_with(
        &mut ctx.sde,
        35_832,
        35_901,
        1570,
        7,
        10.0,
        0.0,
        &[(life::ATTR_RIG_SIZE, 1.0)],
    );
    clone_type_with(
        &mut ctx.sde,
        35_832,
        35_902,
        1570,
        7,
        10.0,
        0.0,
        &[(life::ATTR_RIG_SIZE, 2.0)],
    );
    // High-slot weapon + charge
    clone_type_with(
        &mut ctx.sde,
        35_832,
        35_903,
        1986,
        7,
        10.0,
        100.0,
        &[(11, 10.0)],
    );
    clone_type_with(&mut ctx.sde, 35_832, 35_904, 83, 8, 1.0, 0.0, &[]); // category Charge
    // Service module
    clone_type_with(
        &mut ctx.sde,
        35_832,
        35_905,
        1321,
        7,
        50.0,
        0.0,
        &[(life::ATTR_SERVICE_FUEL_ONLINE, 10.0)],
    );

    seed_char_corp_ship(&ctx, char_id, corp_id, sys_id, ship_id, true);
    seed_structure(&ctx, st_id, 35_832, corp_id, sys_id, "ShieldVulnerable", 0);

    let (wrong_rig, good_rig, weapon_id, charge_id, svc_id, foreign_item) =
        (880_201_i64, 880_202, 880_203, 880_204, 880_205, 880_206);
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, qty, loc) in [
            (wrong_rig, 35_901, 1, st_id),
            (good_rig, 35_902, 1, st_id),
            (weapon_id, 35_903, 1, st_id),
            (charge_id, 35_904, 5, st_id),
            (svc_id, 35_905, 1, st_id),
            (foreign_item, 35_903, 1, 60_003_760),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: corp_id,
                    location_id: loc,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: qty == 1,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let mut not_at_sess = make_session(char_id, corp_id, sys_id, Some(ship_id), corp_roles::ROLE_DIRECTOR);
    assert_eq!(
        handle_structure_fit_move(&ctx, &mut not_at_sess, weapon_id, st_id, InventoryFlag::HiSlot0, None)
            .unwrap_err()
            .code,
        "NotAtStructure"
    );

    let mut sess = make_session(char_id, corp_id, sys_id, Some(ship_id), corp_roles::ROLE_DIRECTOR);
    sess.structure_id = Some(st_id);

    // InvalidItemLocation when item isn't at structure
    assert_eq!(
        handle_structure_fit_move(&ctx, &mut sess, foreign_item, st_id, InventoryFlag::HiSlot0, None)
            .unwrap_err()
            .code,
        "InvalidItemLocation"
    );
    // RigSizeMismatch
    assert_eq!(
        handle_structure_fit_move(&ctx, &mut sess, wrong_rig, st_id, InventoryFlag::RigSlot0, None)
            .unwrap_err()
            .code,
        "RigSizeMismatch"
    );
    // NoWeaponFitted when loading charge into empty HiSlot0
    assert_eq!(
        handle_structure_fit_move(&ctx, &mut sess, charge_id, st_id, InventoryFlag::HiSlot0, None)
            .unwrap_err()
            .code,
        "NoWeaponFitted"
    );
    // Fit weapon into HiSlot0, then charge into HiSlot0
    let _ = handle_structure_fit_move(&ctx, &mut sess, weapon_id, st_id, InventoryFlag::HiSlot0, None).unwrap();
    let _ = handle_structure_fit_move(&ctx, &mut sess, charge_id, st_id, InventoryFlag::HiSlot0, None).unwrap();

    // Fit good rig into RigSlot0: cannot move fitted rig to another slot, unfit destroys it
    let _ = handle_structure_fit_move(&ctx, &mut sess, good_rig, st_id, InventoryFlag::RigSlot0, None).unwrap();
    assert_eq!(
        handle_structure_fit_move(&ctx, &mut sess, good_rig, st_id, InventoryFlag::RigSlot1, None)
            .unwrap_err()
            .code,
        "CannotMoveFittedRig"
    );
    let _ = handle_structure_fit_move(&ctx, &mut sess, good_rig, st_id, InventoryFlag::Hangar, None).unwrap();
    ctx.db.with_snapshot(|snap| assert!(!snap.items.contains_key(&good_rig)));

    // Fit service module offline, then QuantumCoreRequired when trying to online
    let _ = handle_structure_fit_move(&ctx, &mut sess, svc_id, st_id, InventoryFlag::ServiceSlot0, Some(false))
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&st_id) {
            st.quantum_core_installed = false;
        }
    });
    assert_eq!(
        handle_structure_module_online(&ctx, &mut sess, st_id, svc_id, true)
            .unwrap_err()
            .code,
        "QuantumCoreRequired"
    );

    // Install quantum core with 0 fuel -> InsufficientStructureFuelToOnline
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = snap.structures.get(&st_id).unwrap().clone();
        st.quantum_core_installed = true;
        st.fuel_blocks = 0;
        snap.structures.insert(st_id, st);
    });
    assert_eq!(
        handle_structure_module_online(&ctx, &mut sess, st_id, svc_id, true)
            .unwrap_err()
            .code,
        "InsufficientStructureFuelToOnline"
    );

    // Give fuel -> online succeeds, idempotent online, offline succeeds, re-online & unfit strips online_modules
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = snap.structures.get(&st_id).unwrap().clone();
        st.fuel_blocks = 50;
        snap.structures.insert(st_id, st);
    });
    let on1 = handle_structure_module_online(&ctx, &mut sess, st_id, svc_id, true).unwrap();
    assert_eq!(on1.dict_get("online"), Some(&PyRep::Bool(true)));
    let on2 = handle_structure_module_online(&ctx, &mut sess, st_id, svc_id, true).unwrap();
    assert_eq!(on2.dict_get("online"), Some(&PyRep::Bool(true)));
    let _ = handle_structure_fit_move(&ctx, &mut sess, svc_id, st_id, InventoryFlag::Hangar, None).unwrap();
    ctx.db.with_snapshot(|snap| {
        let ext = load_ext_state(snap);
        let online_list = ext.lifecycle.get(&st_id).map(|l| &l.online_modules);
        assert!(!online_list.is_some_and(|m| m.contains(&svc_id)));
    });
}
