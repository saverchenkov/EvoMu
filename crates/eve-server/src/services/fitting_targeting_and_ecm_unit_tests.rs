//! Semantic unit tests split from `fitting_dogma_im_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::fitting_dogma_im_extra_unit_tests::*;

use crate::router::ServerContext;
use eve_db::{
    InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldRepository,
};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta, TargetLock};
use eve_proto::PyRep;
use eve_sde::attr_ids;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn destroy_fitting_guards_subsystems_structures_and_weapon_groups() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_400_002i64;
    let corp_id = 98_400_002i64;
    let sys_id = 30_000_142u32;
    let ship_id = 96_400_200i64;
    let subsys_id = 96_400_201i64;
    let gun1_id = 96_400_202i64;
    let gun2_id = 96_400_203i64;
    let diff_mod_id = 96_400_204i64;
    let struct_id = 1_000_000_400_001i64;
    let struct_mod_id = 96_400_205i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        for (iid, tid, oid, loc, flg) in [
            (ship_id, 29984, char_id, sys_id as i64, InventoryFlag::None),
            (subsys_id, 45598, char_id, ship_id, InventoryFlag::SubSystemSlot0),
            (gun1_id, 484, char_id, ship_id, InventoryFlag::HiSlot0),
            (gun2_id, 484, char_id, ship_id, InventoryFlag::HiSlot1),
            (diff_mod_id, 440, char_id, ship_id, InventoryFlag::MedSlot0),
            (struct_mod_id, 35894, 98_999_999, struct_id, InventoryFlag::HiSlot0),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: oid,
                    location_id: loc,
                    flag: flg,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: 98_999_999,
                solar_system_id: sys_id,
                name: "TestAstra".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });

    let mut session = make_session(char_id, corp_id, Some(sys_id), None, Some(ship_id));

    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "DestroyFitting", &[PyRep::Integer(gun1_id)])
            .await
            .unwrap_err()
            .code,
        "CannotUnfitInSpace"
    );
    ctx.crimewatch
        .write()
        .unwrap()
        .states
        .entry(char_id)
        .or_default()
        .weapon_timer_until_unix = ctx.now_unix_secs() + 60;
    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "DestroyFitting", &[PyRep::Integer(gun1_id)])
            .await
            .unwrap_err()
            .code,
        "CannotRefitWithWeaponsTimer"
    );
    ctx.crimewatch.write().unwrap().states.clear();

    session.station_id = Some(60_003_760);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(gun1_id);
    });
    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "DestroyFitting", &[PyRep::Integer(gun1_id)])
            .await
            .unwrap_err()
            .code,
        "ModuleIsActive"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&gun1_id);
    });

    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "DestroyFitting", &[PyRep::Integer(subsys_id)])
            .await
            .unwrap_err()
            .code,
        "SubsystemHasFittedModules"
    );
    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "DestroyFitting", &[PyRep::Integer(struct_mod_id)])
            .await
            .unwrap_err()
            .code,
        "StructureOwnershipViolation"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().owner_corp_id = corp_id;
        snap.items.get_mut(&struct_mod_id).unwrap().owner_id = corp_id;
    });
    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "DestroyFitting", &[PyRep::Integer(struct_mod_id)])
            .await
            .unwrap_err()
            .code,
        "InsufficientCorpRoles"
    );

    for method in [
        "LinkWeapons",
        "PeelAndLink",
        "MergeModuleGroups",
        "UnlinkModule",
        "DestroyWeaponBank",
        "UnlinkAllModules",
    ] {
        assert_eq!(
            call_char_fitting_mgr(
                &ctx,
                &mut session,
                method,
                &[PyRep::Integer(999_999_999), PyRep::Integer(gun1_id)]
            )
            .await
            .unwrap_err()
            .code,
            "ShipOwnershipViolation"
        );
    }
    for method in ["LinkWeapons", "PeelAndLink", "MergeModuleGroups"] {
        assert_eq!(
            call_char_fitting_mgr(
                &ctx,
                &mut session,
                method,
                &[
                    PyRep::Integer(ship_id),
                    PyRep::Integer(gun1_id),
                    PyRep::Integer(diff_mod_id),
                ],
            )
            .await
            .unwrap_err()
            .code,
            "CannotLinkWeaponsWhileDocked"
        );
    }

    session.station_id = None;
    for method in ["LinkWeapons", "PeelAndLink", "MergeModuleGroups"] {
        assert_eq!(
            call_char_fitting_mgr(
                &ctx,
                &mut session,
                method,
                &[
                    PyRep::Integer(ship_id),
                    PyRep::Integer(gun1_id),
                    PyRep::Integer(diff_mod_id),
                ],
            )
            .await
            .unwrap_err()
            .code,
            "InvalidWeaponGroup"
        );
    }

    let linked_all =
        call_char_fitting_mgr(&ctx, &mut session, "LinkAllWeapons", &[PyRep::Integer(ship_id)])
            .await
            .unwrap();
    assert!(matches!(linked_all, PyRep::Dict(ref d) if !d.is_empty()));
    session.station_id = Some(60_003_760);

    for mid in [gun1_id, gun2_id, diff_mod_id, subsys_id] {
        let d_res =
            call_dogma_im(&ctx, &mut session, None, "DestroyFitting", &[PyRep::Integer(mid)])
                .await
                .unwrap();
        assert_eq!(d_res.dict_get("destroyed"), Some(&PyRep::Bool(true)));
    }
}

#[tokio::test]
async fn add_target_ecm_cloak_tether_range_passive_targeter_and_targeters_list() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_400_003i64;
    let enemy_char_id = 96_400_004i64;
    let sys_id = 30_000_142u32;
    let ship_id = 96_400_300i64;
    let enemy_ship_id = 96_400_301i64;
    let passive_mod_id = 96_400_302i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.values().next().cloned().unwrap();
        for (cid, sid) in [(char_id, ship_id), (enemy_char_id, enemy_ship_id)] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    dynamic_attributes: HashMap::from([
                        (attr_ids::ARMOR_DAMAGE, 40.0),
                        (attr_ids::DAMAGE, 15.0),
                    ]),
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            passive_mod_id,
            InventoryItemRecord {
                item_id: passive_mod_id,
                type_id: 581,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(passive_mod_id, true);
        snap.module_active_states.insert(passive_mod_id);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (sid, oid, pos) in [
        (ship_id, char_id, DVec3::ZERO),
        (enemy_ship_id, enemy_char_id, DVec3::new(3_000.0, 0.0, 0.0)),
    ] {
        let _ = sys
            .spawn_ball_with_meta(
                Ball {
                    id: sid,
                    type_id: 587,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState::default(),
                SlimItemMeta {
                    group_id: 25,
                    category_id: 6,
                    owner_id: oid,
                    char_id: Some(oid),
                    ..Default::default()
                },
            )
            .await;
    }

    let mut session = make_session(char_id, 1_000_009, Some(sys_id), None, Some(ship_id));

    let mut t_cs = CombatState::default();
    t_cs.is_tethered = true;
    let _ = sys.set_combat_state(enemy_ship_id, t_cs).await;
    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "AddTarget", &[PyRep::Integer(enemy_ship_id)])
            .await
            .unwrap_err()
            .code,
        "TargetInvulnerable"
    );
    let _ = sys.set_combat_state(enemy_ship_id, CombatState::default()).await;

    let mut s_cs = CombatState::default();
    s_cs.ecm_jammed_remaining_s = 5.0;
    let _ = sys.set_combat_state(ship_id, s_cs).await;
    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "AddTarget", &[PyRep::Integer(enemy_ship_id)])
            .await
            .unwrap_err()
            .code,
        "ShipJammed"
    );

    let mut cloaked_cs = CombatState::default();
    cloaked_cs.is_cloaked = true;
    let _ = sys.set_combat_state(ship_id, cloaked_cs).await;
    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "AddTarget", &[PyRep::Integer(enemy_ship_id)])
            .await
            .unwrap_err()
            .code,
        "TargetingFailed"
    );
    let _ = sys.set_combat_state(ship_id, CombatState::default()).await;
    if let Some(fit) = ctx.ship_fittings.write().unwrap().get_mut(&ship_id) {
        if let Some(m) = fit.modules.get_mut(&passive_mod_id) {
            m.state = eve_dogma::ModuleState::Active;
        }
    }

    let lock_res =
        call_dogma_im(&ctx, &mut session, None, "AddTarget", &[PyRep::Integer(enemy_ship_id)])
            .await
            .unwrap();
    assert_eq!(lock_res.dict_get("passiveLock"), Some(&PyRep::Bool(true)));

    let _ = sys
        .add_target_lock(
            enemy_ship_id,
            TargetLock {
                target_id: ship_id,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(20_000.0),
            },
        )
        .await;
    let targeters = call_dogma_im(&ctx, &mut session, None, "GetTargeters", &[]).await.unwrap();
    assert_eq!(targeters, PyRep::List(vec![PyRep::Integer(enemy_ship_id)]));

    let dmg_self =
        call_dogma_im(&ctx, &mut session, None, "GetLayerDamageValuesByItems", &[]).await.unwrap();
    assert!(format!("{dmg_self:?}").contains("40.0"));

    let dmg_enemy = call_dogma_im(
        &ctx,
        &mut session,
        None,
        "GetLayerDamageValuesByItems",
        &[PyRep::Integer(enemy_ship_id)],
    )
    .await
    .unwrap();
    assert!(!format!("{dmg_enemy:?}").contains("40.0"));
}

#[tokio::test]
async fn cold_system_docked_fitting_targeting_repair_trash_and_inspace_add_target_do_not_wake() {
    use crate::services::inventory_fitting::fitting_dogma_ops::handle_dogma_ops;
    use crate::services::inventory_fitting::fitting_ships_mastery::handle_ships_mastery_and_fitting_ext;
    use crate::system_actor::SystemLifecycleState;

    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let cold_sys_id = 30_000_144_u32;
    let station_id = 60_003_760_i64;
    let char_id = 96_400_901_i64;
    let ship_id = 96_400_902_i64;
    let mod_id = 96_400_903_i64;
    let hangar_trash_id = 96_400_904_i64;
    let jetcan_id = 96_400_905_i64;
    let jetcan_item_id = 96_400_906_i64;
    let enemy_ship_id = 96_400_907_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.corporation_id = 1_000_009;
        ch.solar_system_id = cold_sys_id;
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        for (iid, tid, loc, flg, qty, singleton, dyn_attrs) in [
            (ship_id, 587_u32, station_id, InventoryFlag::Hangar, 1_i32, true, HashMap::new()),
            (
                mod_id,
                484_u32,
                ship_id,
                InventoryFlag::HiSlot0,
                1_i32,
                true,
                HashMap::from([(attr_ids::DAMAGE, 10.0), (attr_ids::HP_STRUCT, 40.0)]),
            ),
            (hangar_trash_id, 34_u32, station_id, InventoryFlag::Hangar, 25_i32, false, HashMap::new()),
            (jetcan_id, 23_u32, i64::from(cold_sys_id), InventoryFlag::None, 1_i32, true, HashMap::new()),
            (jetcan_item_id, 34_u32, jetcan_id, InventoryFlag::Cargo, 10_i32, false, HashMap::new()),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag: flg,
                    quantity: qty,
                    singleton,
                    dynamic_attributes: dyn_attrs,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
        snap.item_positions.insert(jetcan_id, [100.0, 0.0, 0.0]);
    });

    assert_eq!(ctx.systems.system_lifecycle_state(cold_sys_id), SystemLifecycleState::Cold);
    assert!(!ctx.systems.is_system_loaded(cold_sys_id));

    let mut session = make_session(char_id, 1_000_009, Some(cold_sys_id), Some(station_id), Some(ship_id));

    // Docked GetTargets / GetTargeters / InitiateModuleRepairMany / DestroyFitting / TrashItems stay Cold
    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "GetTargets", &[]).await.unwrap(),
        PyRep::List(Vec::new())
    );
    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "GetTargeters", &[]).await.unwrap(),
        PyRep::List(Vec::new())
    );
    let repair_docked = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "InitiateModuleRepairMany",
        &[PyRep::List(vec![PyRep::Integer(mod_id)])],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("docked nanite repair must reject");
    assert_eq!(repair_docked.code, "CannotRepairNaniteWhileDocked");

    let destroyed = call_dogma_im(&ctx, &mut session, None, "DestroyFitting", &[PyRep::Integer(mod_id)])
        .await
        .expect("docked DestroyFitting succeeds without waking cold system");
    assert_eq!(destroyed.dict_get("destroyed"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&mod_id)));

    let trashed_hangar = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "TrashItems",
        &[PyRep::List(vec![PyRep::Integer(hangar_trash_id)])],
        &kw,
    )
    .await
    .expect("handled")
    .expect("docked TrashItems succeeds without waking cold system");
    assert_eq!(trashed_hangar, PyRep::List(Vec::new()));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&hangar_trash_id)));
    assert_eq!(ctx.systems.system_lifecycle_state(cold_sys_id), SystemLifecycleState::Cold);
    assert!(!ctx.systems.is_system_loaded(cold_sys_id));

    // Undocked cold system: GetTargets/GetTargeters return empty, TrashItems cleans empty jetcan,
    // AddTarget / AddTargetOBO / InitiateModuleRepairMany fail closed without waking.
    session.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = i64::from(cold_sys_id);
            sh.flag = InventoryFlag::None;
        }
    });

    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "GetTargets", &[]).await.unwrap(),
        PyRep::List(Vec::new())
    );
    assert_eq!(
        call_dogma_im(&ctx, &mut session, None, "GetTargeters", &[]).await.unwrap(),
        PyRep::List(Vec::new())
    );
    let trashed_jetcan = handle_ships_mastery_and_fitting_ext(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "TrashItems",
        &[PyRep::List(vec![PyRep::Integer(jetcan_item_id)])],
        &kw,
    )
    .await
    .expect("handled")
    .expect("cold-system TrashItems removes empty jetcan via item_positions without waking");
    assert_eq!(trashed_jetcan, PyRep::List(Vec::new()));
    assert!(ctx
        .db
        .with_snapshot(|s| !s.items.contains_key(&jetcan_item_id) && !s.items.contains_key(&jetcan_id)));

    let add_err = call_dogma_im(&ctx, &mut session, None, "AddTarget", &[PyRep::Integer(enemy_ship_id)])
        .await
        .expect_err("cold-system AddTarget must fail closed");
    assert_eq!(add_err.code, "TargetingFailed");
    assert_eq!(
        add_err
            .to_client_error("dogmaIM", "AddTarget")
            .as_user_error()
            .map(|(k, _)| k),
        Some(eve_proto::UserErrorKey::CantShipNotPresent)
    );

    let add_obo_err = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "AddTargetOBO",
        &[PyRep::Integer(ship_id), PyRep::Integer(enemy_ship_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("cold-system AddTargetOBO must fail closed");
    assert_eq!(add_obo_err.code, "TargetingFailed");
    assert_eq!(
        add_obo_err
            .to_client_error("dogmaIM", "AddTargetOBO")
            .as_user_error()
            .map(|(k, _)| k),
        Some(eve_proto::UserErrorKey::CantShipNotPresent)
    );

    let repair_cold = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "InitiateModuleRepairMany",
        &[PyRep::List(vec![PyRep::Integer(mod_id)])],
        &kw,
    )
    .await
    .expect("handled")
    .expect_err("cold-system InitiateModuleRepairMany must fail closed");
    assert_eq!(repair_cold.code, "ShipNotInSpace");

    assert_eq!(ctx.systems.system_lifecycle_state(cold_sys_id), SystemLifecycleState::Cold);
    assert!(!ctx.systems.is_system_loaded(cold_sys_id));
}
