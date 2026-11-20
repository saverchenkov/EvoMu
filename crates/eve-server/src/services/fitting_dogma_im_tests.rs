//! Unit tests for `fitting_dogma_im` (`ItemGetInfo`, `GetAllInfo`, `AddTarget`, `RemoveTarget`, `ClearTargets`, `GetTargets`, `GetTargeters`, `LinkWeapons`, `PeelAndLink`, `MergeModuleGroups`, `UnlinkModule`, `DestroyWeaponBank`, `UnlinkAllModules`, `GetLayerDamageValuesByItems`).

use super::*;
use eve_db::{AccountRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[tokio::test]
async fn dogma_im_inspection_targeting_weapon_banks_and_damage_values() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_300_001i64;
    let sys_id = 30_000_142u32;
    let ship_id = 96_300_100i64;
    let target_ship_id = 96_300_200i64;
    let gun1_id = 96_300_301i64;
    let gun2_id = 96_300_302i64;
    let gun3_id = 96_300_303i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "user_96301".into(),
            AccountRecord {
                user_id: 96_301,
                username: "user_96301".into(),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 96_301;
        ch.name = "DogmaImPilot".into();
        ch.corporation_id = 1_000_009;
        ch.station_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([
                    (attr_ids::ARMOR_DAMAGE, 25.0),
                    (attr_ids::DAMAGE, 12.0),
                ]),
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            target_ship_id,
            InventoryItemRecord {
                item_id: target_ship_id,
                type_id: 587,
                owner_id: 96_300_999,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for (gid, flg) in [
            (gun1_id, InventoryFlag::HiSlot0),
            (gun2_id, InventoryFlag::HiSlot1),
            (gun3_id, InventoryFlag::HiSlot2),
        ] {
            snap.items.insert(
                gid,
                InventoryItemRecord {
                    item_id: gid,
                    type_id: 484, // 125mm Gatling AutoCannon I
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: flg,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let cs = CombatState::default();
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_350_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs.clone(),
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: char_id,
                char_id: Some(char_id),
                ..Default::default()
            },
        )
        .await;
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: target_ship_id,
                type_id: 587,
                position: DVec3::new(4_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_350_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: 96_300_999,
                char_id: Some(96_300_999),
                ..Default::default()
            },
        )
        .await;

    let mut session = SessionState::default();
    session.user_id = Some(96_301);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);

    // 1. handle_item_get_info & GetAllInfo
    let info_ship = handle_item_get_info(&ctx, &session, &[PyRep::Integer(ship_id)]).expect("ship info");
    assert_ne!(info_ship, PyRep::None);

    let info_mod = handle_item_get_info(&ctx, &session, &[PyRep::Integer(gun1_id)]).expect("mod info");
    assert_ne!(info_mod, PyRep::None);

    let info_type = handle_item_get_info(&ctx, &session, &[PyRep::Integer(587)]).expect("type info");
    assert_ne!(info_type, PyRep::None);

    let all_info = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "GetAllInfo",
        &[PyRep::Bool(true), PyRep::Bool(true)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetAllInfo ok");
    assert_ne!(all_info, PyRep::None);

    // 2. GetTargets, GetTargeters, RemoveTarget, ClearTargets
    let _ = sys
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: target_ship_id,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(25_000.0),
            },
        )
        .await;

    let targets = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "GetTargets",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetTargets ok");
    assert_eq!(targets, PyRep::List(vec![PyRep::Integer(target_ship_id)]));

    let targeters = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "GetTargeters",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetTargeters ok");
    assert_eq!(targeters, PyRep::List(vec![]));

    let rem_res = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "RemoveTarget",
        &[PyRep::Integer(target_ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("RemoveTarget ok");
    assert_eq!(rem_res, PyRep::Integer(target_ship_id));

    let _ = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "ClearTargets",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("ClearTargets ok");

    // 3. Weapon grouping: LinkWeapons, PeelAndLink, MergeModuleGroups, UnlinkModule, DestroyWeaponBank, UnlinkAllModules
    let _ = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "LinkWeapons",
        &[
            PyRep::Integer(ship_id),
            PyRep::Integer(gun1_id),
            PyRep::Integer(gun2_id),
        ],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("LinkWeapons ok");

    let _ = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "PeelAndLink",
        &[
            PyRep::Integer(ship_id),
            PyRep::Integer(gun3_id),
            PyRep::Integer(gun1_id),
        ],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("PeelAndLink ok");

    let _ = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "UnlinkModule",
        &[PyRep::Integer(ship_id), PyRep::Integer(gun3_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("UnlinkModule ok");

    let _ = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "MergeModuleGroups",
        &[
            PyRep::Integer(ship_id),
            PyRep::Integer(gun1_id),
            PyRep::Integer(gun3_id),
        ],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("MergeModuleGroups ok");

    let _ = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "DestroyWeaponBank",
        &[PyRep::Integer(ship_id), PyRep::Integer(gun1_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("DestroyWeaponBank ok");

    let _ = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "UnlinkAllModules",
        &[PyRep::Integer(ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("UnlinkAllModules ok");

    // 4. GetLayerDamageValuesByItems
    let dmg_dict = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "GetLayerDamageValuesByItems",
        &[PyRep::List(vec![PyRep::Integer(ship_id)])],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetLayerDamageValuesByItems ok");
    assert!(matches!(dmg_dict, PyRep::Dict(_)));
}

// parity: rpc:dogmaIM.SetSessionCheck
#[tokio::test]
async fn dogma_im_set_session_check_validates_updates_and_clears() {
    eve_testkit::attest(&["rpc:dogmaIM.SetSessionCheck"]);
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let mut unauth = SessionState::new(96_398);
    let unauth_err = handle_dogma_im(
        &ctx,
        &mut unauth,
        None,
        "dogmaIM",
        "SetSessionCheck",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect_err("unauthenticated rejected");
    assert_eq!(unauth_err.code, "Unauthorized");

    let char_id = 90_000_001i64;
    let ship_id = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|it| it.owner_id == char_id && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.category_id == 6))
            .map(|it| it.item_id)
    });
    let mut session = SessionState::new(96_399);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = ship_id;

    let check_spec = PyRep::Dict(vec![
        (PyRep::String("charid".into()), PyRep::Integer(char_id)),
        (PyRep::String("solarsystemid2".into()), PyRep::Integer(30_000_142)),
    ]);
    let first = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "SetSessionCheck",
        &[check_spec.clone()],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("first SetSessionCheck");
    assert_eq!(first.dict_get("valid"), Some(&PyRep::Bool(true)));
    assert_eq!(first.dict_get("characterID"), Some(&PyRep::Integer(char_id)));
    assert_eq!(first.dict_get("checkCount"), Some(&PyRep::Integer(1)));
    let inner = first.dict_get("sessionCheck").expect("sessionCheck");
    assert_eq!(inner.dict_get("charid"), Some(&PyRep::Integer(char_id)));

    let second = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "SetSessionCheck",
        &[check_spec],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("second SetSessionCheck");
    assert_eq!(second.dict_get("checkCount"), Some(&PyRep::Integer(2)));

    let wrong_sys = PyRep::Dict(vec![(
        PyRep::String("solarsystemid2".into()),
        PyRep::Integer(30_002_187),
    )]);
    let mismatch = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "SetSessionCheck",
        &[wrong_sys],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect_err("wrong system rejected");
    assert_eq!(mismatch.code, "SessionCheckMismatch");

    let cleared = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "SetSessionCheck",
        &[PyRep::None],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("clear SetSessionCheck");
    assert_eq!(cleared.dict_get("valid"), Some(&PyRep::Bool(true)));
    assert_eq!(cleared.dict_get("sessionCheck"), Some(&PyRep::None));
    assert_eq!(cleared.dict_get("checkCount"), Some(&PyRep::Integer(0)));
}

#[tokio::test]
async fn get_all_info_includes_offline_missing_skill_modules_and_zeros_base_loads() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_399_001_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 96_399_100_i64;
    let dcu_id = 96_399_201_i64;
    let gun_id = 96_399_202_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 96_399;
        ch.name = "GetAllInfoPilot".into();
        ch.corporation_id = 1_000_009;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        // Grant Hull Upgrades 4 (3394) for DCU II (2048), but NOT Small Projectile Turret (3301) for 125mm AutoCannon (484)
        snap.skills.entry(char_id).or_default().insert(
            3394,
            eve_db::TrainedSkillRecord {
                type_id: 3394,
                level: 4,
                skill_points: 45_255.0,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            dcu_id,
            InventoryItemRecord {
                item_id: dcu_id,
                type_id: 2048, // Damage Control II (onlineable)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            gun_id,
            InventoryItemRecord {
                item_id: gun_id,
                type_id: 484, // 125mm Gatling AutoCannon I (missing skill 3301)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(dcu_id, true);
        snap.module_online_states.insert(gun_id, false);
    });

    let mut session = SessionState::new(96_399);
    session.user_id = Some(96_399);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.station_id = Some(station_id);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = Some(ship_id);

    let all_info = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "GetAllInfo",
        &[PyRep::Bool(true), PyRep::Bool(true), PyRep::Bool(false)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetAllInfo ok");

    // #649: Both online DCU and offline missing-skill gun must appear in shipInfo
    let PyRep::Dict(ship_info) = all_info.dict_get("shipInfo").expect("shipInfo") else {
        panic!("expected shipInfo Dict");
    };
    assert!(
        ship_info.iter().any(|(k, _)| k.as_i64() == Some(dcu_id)),
        "online DCU missing from shipInfo"
    );
    let (_, gun_entry) = ship_info
        .iter()
        .find(|(k, _)| k.as_i64() == Some(gun_id))
        .expect("offline missing-skill gun omitted from shipInfo");
    let PyRep::Dict(gun_attrs) = gun_entry.dict_get("attributes").expect("gun attributes") else {
        panic!("expected gun attributes Dict");
    };
    assert_eq!(
        gun_attrs
            .iter()
            .find(|(k, _)| k.as_i64() == Some(2))
            .and_then(|(_, v)| v.as_f64()),
        Some(0.0),
        "missing-skill gun must report isOnline (attr 2) = 0.0 in GetAllInfo"
    );

    // #785: GetAllInfo shipInfo.attributes and shipAttributes carry evaluated CPU_LOAD (49) and POWER_LOAD (15)
    let (_, ship_entry) = ship_info
        .iter()
        .find(|(k, _)| k.as_i64() == Some(ship_id))
        .expect("ship entry");
    let PyRep::Dict(ship_attrs) = ship_entry.dict_get("attributes").expect("attributes") else {
        panic!("expected attributes Dict");
    };
    let cpu_load_info = ship_attrs
        .iter()
        .find(|(k, _)| k.as_i64() == Some(attr_ids::CPU_LOAD as i64))
        .and_then(|(_, v)| v.as_f64())
        .expect("CPU_LOAD in GetAllInfo");
    let pg_load_info = ship_attrs
        .iter()
        .find(|(k, _)| k.as_i64() == Some(attr_ids::POWER_LOAD as i64))
        .and_then(|(_, v)| v.as_f64())
        .expect("POWER_LOAD in GetAllInfo");

    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    let eval_cpu_load = fit.ship.get_attribute(attr_ids::CPU_LOAD, &ctx.sde);
    let eval_pg_load = fit.ship.get_attribute(attr_ids::POWER_LOAD, &ctx.sde);
    assert!(eval_cpu_load > 0.0, "Evaluated ship CPU_LOAD should be > 0");
    assert!(eval_pg_load > 0.0, "Evaluated ship POWER_LOAD should be > 0");
    assert!(
        (cpu_load_info - eval_cpu_load).abs() < 1e-6,
        "GetAllInfo CPU_LOAD ({cpu_load_info}) must match evaluated load ({eval_cpu_load})"
    );
    assert!(
        (pg_load_info - eval_pg_load).abs() < 1e-6,
        "GetAllInfo POWER_LOAD ({pg_load_info}) must match evaluated load ({eval_pg_load})"
    );
}

#[tokio::test]
async fn add_target_wire_flags_reconcile_relocks_prune_stale_slots_and_enforce_cap() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_310_001i64;
    let sys_id = 30_000_142u32;
    let ship_id = 96_310_100i64;
    let ast1_id = 96_310_201i64;
    let ast2_id = 96_310_202i64;
    let rifter_id = 96_310_203i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "user_96310".into(),
            AccountRecord {
                user_id: 96_310,
                username: "user_96310".into(),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 96_310;
        ch.name = "VentureLockPilot".into();
        ch.corporation_id = 1_000_009;
        ch.station_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        // Clear Target Management skills so pilot cap is `2 + 0 = 2` (`min(hull_max=5, char_max=2) == 2`)
        snap.skills.insert(char_id, HashMap::new());
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 32_880, // Venture (hull maxLockedTargets = 5, skill-capped to 2)
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (bid, tid, pos, owner) in [
        (ship_id, 32_880u32, DVec3::ZERO, char_id),
        (ast1_id, 1230u32, DVec3::new(2_000.0, 0.0, 0.0), 1),
        (ast2_id, 1230u32, DVec3::new(2_500.0, 0.0, 0.0), 1),
        (rifter_id, 587u32, DVec3::new(3_000.0, 0.0, 0.0), 1),
    ] {
        let _ = sys
            .spawn_ball_with_meta(
                Ball {
                    id: bid,
                    type_id: tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState::default(),
                SlimItemMeta {
                    owner_id: owner,
                    char_id: (owner == char_id).then_some(char_id),
                    ..Default::default()
                },
            )
            .await;
    }

    let mut session = SessionState::new(96_310);
    session.user_id = Some(96_310);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);
    let kw = HashMap::new();

    // 1. Live wire AddTarget starts async lock -> returns `(1, [])` (`flag = 1`), NOT `(0, [id])`
    let wire_lock1 = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "AddTarget",
            &[PyRep::Integer(ast1_id)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("AddTarget ast1");
    assert_eq!(
        wire_lock1,
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::List(Vec::new())]),
        "Async target lock start must return flag=1 and empty targetList"
    );

    // 2. CancelAddTarget releases the pending slot cleanly
    ctx.dispatch_rpc(
        &mut session,
        "dogmaIM",
        None,
        "CancelAddTarget",
        &[PyRep::Integer(ast1_id)],
        &kw,
        Some("machoNet:26000"),
    )
    .await
    .expect("CancelAddTarget ast1");
    assert!(sys.get_target_locks(ship_id).await.unwrap().is_empty());

    // 3. Lock ast1 and ast2 to fill Venture's 2 target slots, step until locked
    for tid in [ast1_id, ast2_id] {
        ctx.dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "AddTarget",
            &[PyRep::Integer(tid)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("AddTarget slot");
    }
    sys.step_manual(15.0).await.expect("step_manual locks");
    assert_eq!(sys.get_target_locks(ship_id).await.unwrap().len(), 2);

    // 4. Re-locking already-locked ast1 at full capacity returns `(0, [ast1, ast2])` without failing or leaking slots
    let relock_ast1 = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "AddTarget",
            &[PyRep::Integer(ast1_id)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("Re-locking already-locked target must succeed instead of Max locked targets exceeded");
    assert_eq!(
        relock_ast1,
        PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::List(vec![PyRep::Integer(ast1_id), PyRep::Integer(ast2_id)]),
        ])
    );
    assert_eq!(sys.get_target_locks(ship_id).await.unwrap().len(), 2);

    // 5. Negative check: locking a 3rd distinct valid target while 2 valid targets are locked fails cleanly
    let cap_err = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "AddTarget",
            &[PyRep::Integer(rifter_id)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect_err("3rd distinct target on 2-slot Venture must fail");
    assert_eq!(cap_err.code, "TargetingFailed");
    assert!(cap_err.message.contains("Max locked targets exceeded"));

    // 6. When ast2 moves beyond maxTargetRange before `step()` runs, stale slot is pruned so rifter_id locks cleanly
    let _ = sys
        .set_ball_position(ast2_id, DVec3::new(500_000.0, 0.0, 0.0))
        .await;
    let lock_rifter = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "AddTarget",
            &[PyRep::Integer(rifter_id)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("Stale out-of-range ast2 slot must be pruned so rifter_id locks");
    assert_eq!(
        lock_rifter,
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::List(Vec::new())])
    );
    assert_eq!(sys.get_target_locks(ship_id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn venture_lock_spawned_veldspar_preserves_progress_emits_single_add_and_mines() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_311_001i64;
    let sys_id = 30_000_142u32;
    let ship_id = 96_311_100i64;
    let miner_id = 96_311_101i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "user_96311".into(),
            AccountRecord {
                user_id: 96_311,
                username: "user_96311".into(),
                password_token: String::new(),
                role: eve_machonet::service_roles::ROLE_GMH,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 96_311;
        ch.name = "VentureMinerPilot".into();
        ch.corporation_id = 1_000_009;
        ch.station_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.skills.insert(char_id, HashMap::new());
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 32_880, // Venture (scanResolution = 750)
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            miner_id,
            InventoryItemRecord {
                item_id: miner_id,
                type_id: 483, // Miner I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 32_880,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 335.0,
                inertia: 1.0,
                mass: 1_200_000.0,
                radius: 41.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                owner_id: char_id,
                char_id: Some(char_id),
                ..Default::default()
            },
        )
        .await;

    let mut session = SessionState::new(96_311);
    session.user_id = Some(96_311);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);
    session.role = eve_machonet::service_roles::ROLE_GMH;
    let kw = HashMap::new();

    // Spawn Veldspar via `/spawn 1230` at ~2km distance
    let spawn_res = ctx
        .dispatch_rpc(
            &mut session,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/spawn 1230".into())],
            &kw,
            None,
        )
        .await
        .expect("/spawn 1230");
    let ast_id = spawn_res
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("spawned asteroid itemID");
    let _ = session.drain_notifications();
    let _ = ctx.drain_notifications_for_char(char_id);

    // First AddTarget returns `(1, [])` (`flag = 1`), no `OnTarget` `"locking"`, no `OnMultiEvent` `"add"`
    let r1 = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "AddTarget",
            &[PyRep::Integer(ast_id)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("AddTarget Veldspar");
    assert_eq!(r1, PyRep::Tuple(vec![PyRep::Integer(1), PyRep::List(Vec::new())]));
    let pre_notifs: Vec<_> = session
        .drain_notifications()
        .into_iter()
        .chain(
            ctx.drain_notifications_for_char(char_id)
                .into_iter()
                .map(|n| (n.event, n.args)),
        )
        .collect();
    assert!(
        !pre_notifs
            .iter()
            .any(|(ev, args)| (ev == "OnTarget" && format!("{args:?}").contains("locking"))
                || ev == "OnMultiEvent"),
        "Must not emit locking or OnMultiEvent OnTarget add before lock completes: {pre_notifs:?}"
    );

    let initial_locks = sys.get_target_locks(ship_id).await.unwrap();
    assert_eq!(initial_locks.len(), 1);
    let initial_ms = initial_locks[0].lock_time_ms;
    assert!(
        initial_ms > 500.0 && initial_ms < 3_000.0,
        "Venture (750mm scan res) on Veldspar (200m+ radius) must lock in ~1.5s, got {initial_ms}ms"
    );

    // Step 1.0s (< 1.5s): still locking, ~486ms remaining
    sys.step_manual(1.0).await.expect("tick 1");
    let mid_locks = sys.get_target_locks(ship_id).await.unwrap();
    assert!(!mid_locks[0].locked, "lock should still be pending after 1.0s");
    let mid_remaining = mid_locks[0].remaining_lock_ms;
    assert!(mid_remaining < initial_ms && mid_remaining > 0.0);

    // Re-issuing AddTarget mid-lock must NOT reset remaining_lock_ms back to lock_time_ms
    let r2 = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "AddTarget",
            &[PyRep::Integer(ast_id)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("re-AddTarget mid-lock");
    assert_eq!(r2, PyRep::Tuple(vec![PyRep::Integer(1), PyRep::List(Vec::new())]));
    let after_readd = sys.get_target_locks(ship_id).await.unwrap();
    assert!(
        (after_readd[0].remaining_lock_ms - mid_remaining).abs() < 1e-6,
        "Duplicate AddTarget while locking must preserve remaining_lock_ms ({mid_remaining}), got {}",
        after_readd[0].remaining_lock_ms
    );

    // Step 1.0s more (2.0s total > ~1.5s): lock completes and emits single OnMultiEvent OnTarget add
    sys.step_manual(1.0).await.expect("tick 2");
    let done_locks = sys.get_target_locks(ship_id).await.unwrap();
    assert!(done_locks[0].locked, "Venture must complete Veldspar lock within 2s");
    let done_notifs: Vec<_> = session
        .drain_notifications()
        .into_iter()
        .chain(
            ctx.drain_notifications_for_char(char_id)
                .into_iter()
                .map(|n| (n.event, n.args)),
        )
        .collect();
    assert!(
        done_notifs.iter().any(|(ev, args)| ev == "OnMultiEvent"
            && format!("{args:?}").contains("\"OnTarget\"")
            && format!("{args:?}").contains("\"add\"")),
        "Expected OnMultiEvent OnTarget add after lock completion, got {done_notifs:?}"
    );

    // Miner I (483) Activate against locked asteroid succeeds cleanly
    let act_res = ctx
        .dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(miner_id),
                PyRep::String("miningLaser".into()),
                PyRep::Integer(ast_id),
                PyRep::Integer(0),
            ],
            &kw,
            None,
        )
        .await;
    assert!(act_res.is_ok(), "Miner I activation on locked Veldspar failed: {act_res:?}");
}

#[tokio::test]
async fn destroy_fitting_removes_loaded_charges_and_notifies_947() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_309_470_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 96_309_471_i64;
    let gun_id = 96_309_472_i64;
    let charge_id = 96_309_473_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.station_id = Some(station_id);
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            gun_id,
            InventoryItemRecord {
                item_id: gun_id,
                type_id: 484, // 125mm Gatling AutoCannon I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge_id,
            InventoryItemRecord {
                item_id: charge_id,
                type_id: 179, // Depleted Uranium S
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 40,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.weapon_banks
            .entry(ship_id)
            .or_default()
            .insert(gun_id, vec![gun_id, 96_309_499]);
    });

    let mut session = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        station_id: Some(station_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    ctx.db
        .acquire_online_lock(1, 1, Some(char_id), true, ctx.now_unix_secs())
        .expect("online lock");

    // Destroying the active ship itself via DestroyFitting while docked must be rejected
    let active_ship_err = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "DestroyFitting",
        &[PyRep::Integer(ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .unwrap_err();
    assert_eq!(active_ship_err.code, "ItemNotFittedToActiveShip");

    let _ = ctx.drain_notifications_for_char(char_id);
    let res = handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "DestroyFitting",
        &[PyRep::Integer(gun_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("DestroyFitting ok");
    assert_eq!(res.dict_get("destroyed").and_then(|v| v.as_bool()), Some(true));
    assert!(
        ctx.db.with_snapshot(|s| s.weapon_banks.get(&ship_id).is_none_or(|b| b.is_empty())),
        "DestroyFitting must remove destroyed weapon from weapon_banks"
    );

    assert!(ctx.db.get_item(gun_id).is_none(), "destroyed module must be removed");
    assert!(
        ctx.db.get_item(charge_id).is_none(),
        "loaded charge in destroyed module slot must also be removed (#947)"
    );

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnItemChange" && format!("{:?}", n.args).contains(&charge_id.to_string())),
        "DestroyFitting must emit OnItemChange deletion for loaded charge (#947), got {notifs:?}"
    );
}

#[tokio::test]
async fn get_layer_damage_values_by_items_encodes_v24_shield_ratio_tau_and_wallclock_ft() {
    // Issue #990: `dogmaIM.GetLayerDamageValuesByItems` must return `shieldInfo = (shieldRatio, tauMs, simFt)`
    // where `shieldRatio` is normalized `0.0..=1.0`, `tauMs = rechargeMs / 5.0`, and `simFt` is anchored to
    // real OS wallclock FILETIME ticks so `eveInflight/damageStateValue.pyj` `exp((time - now) / 10000 / tau)`
    // never overflows even after `advance_secs_without_sim_clock_rebase`.
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_399_001i64;
    let other_char_id = 96_399_002i64;
    let sys_id = 30_000_142u32;
    let ship_id = 96_399_100i64;
    let drone_healthy_id = 96_399_201i64;
    let drone_damaged_id = 96_399_202i64;
    let foreign_drone_id = 96_399_203i64;
    let hobgoblin_tid = 2_454u32;

    let hob_type = ctx.sde.types.get(&hobgoblin_tid).expect("Hobgoblin I in SDE");
    let max_shield = hob_type
        .default_attributes
        .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
        .copied()
        .unwrap_or(26.0);
    let max_armor = hob_type
        .default_attributes
        .get(&eve_sde::attr_ids::ARMOR_HP)
        .copied()
        .unwrap_or(29.0);
    let max_hull = hob_type
        .default_attributes
        .get(&eve_sde::attr_ids::HP_STRUCT)
        .copied()
        .unwrap_or(35.0);
    let recharge_ms = hob_type
        .default_attributes
        .get(&479)
        .copied()
        .unwrap_or(400_000.0);
    let expected_tau_ms = recharge_ms / 5.0;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().unwrap();
        for cid in [char_id, other_char_id] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
            snap.characters.insert(cid, ch);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 645,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for (did, oid, dyn_attrs) in [
            (drone_healthy_id, char_id, HashMap::new()),
            (
                drone_damaged_id,
                char_id,
                HashMap::from([
                    (eve_sde::attr_ids::ARMOR_DAMAGE, 5.0),
                    (eve_sde::attr_ids::DAMAGE, 3.0),
                ]),
            ),
            (
                foreign_drone_id,
                other_char_id,
                HashMap::from([
                    (eve_sde::attr_ids::ARMOR_DAMAGE, 12.0),
                    (eve_sde::attr_ids::DAMAGE, 9.0),
                ]),
            ),
        ] {
            snap.items.insert(
                did,
                InventoryItemRecord {
                    item_id: did,
                    type_id: hobgoblin_tid,
                    owner_id: oid,
                    location_id: ship_id,
                    flag: InventoryFlag::DroneBay,
                    quantity: 1,
                    singleton: true,
                    dynamic_attributes: dyn_attrs,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.sp6_lifecycle.ship_combat_hp.insert(
            drone_damaged_id,
            eve_db::PersistedCombatHp {
                shield_hp: max_shield * 0.25,
                armor_hp: (max_armor - 5.0).max(0.0),
                hull_hp: (max_hull - 3.0).max(0.0),
            },
        );
        snap.sp6_lifecycle.ship_combat_hp.insert(
            foreign_drone_id,
            eve_db::PersistedCombatHp {
                shield_hp: max_shield * 0.1,
                armor_hp: (max_armor - 12.0).max(0.0),
                hull_hp: (max_hull - 9.0).max(0.0),
            },
        );
    });

    // Advance virtual clock 30 days ahead without rebasing live clients' GetSimTime()
    ctx.advance_secs_without_sim_clock_rebase(30 * 86_400);
    let (wall_s, wall_ns) = eve_db::clock::host_wall_unix_secs_and_nanos();
    let client_sim_ft = (wall_s as i64 + 11_644_473_600) * 10_000_000 + (wall_ns as i64 / 100);
    assert!(
        ctx.now_blueos_filetime_ticks() - client_sim_ft > 20 * 86_400 * 10_000_000,
        "virtual server clock must be >20 days ahead of wallclock sim time"
    );

    // Negative 1: unauthenticated session rejected
    let mut unauth_sess = SessionState::new(99_901);
    assert!(
        handle_dogma_im(
            &ctx,
            &mut unauth_sess,
            None,
            "dogmaIM",
            "GetLayerDamageValuesByItems",
            &[PyRep::PySet(vec![PyRep::Integer(drone_healthy_id)])],
            &HashMap::new(),
        )
        .await
        .expect("handled")
        .is_err()
    );

    let mut sess = SessionState::new(99_902);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER;

    let rep = handle_dogma_im(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "GetLayerDamageValuesByItems",
        &[PyRep::PySet(vec![
            PyRep::Integer(drone_healthy_id),
            PyRep::Integer(drone_damaged_id),
            PyRep::Integer(foreign_drone_id),
        ])],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetLayerDamageValuesByItems ok");

    let PyRep::Dict(entries) = rep else {
        panic!("expected Dict from GetLayerDamageValuesByItems");
    };
    let find_entry = |iid: i64| -> &PyRep {
        &entries
            .iter()
            .find(|(k, _)| *k == PyRep::Integer(iid))
            .expect("entry present")
            .1
    };

    // Healthy Hobgoblin I: shieldRatio == 1.0, tau == recharge_ms / 5.0, finite exp()
    let healthy_kv = find_entry(drone_healthy_id);
    let Some(PyRep::Tuple(h_sh)) = healthy_kv.dict_get("shieldInfo") else {
        panic!("expected shieldInfo tuple");
    };
    assert!((h_sh[0].as_f64().unwrap() - 1.0).abs() < 1e-9);
    assert!((h_sh[1].as_f64().unwrap() - expected_tau_ms).abs() < 1e-6);
    let h_ft = h_sh[2].as_i64().unwrap();
    assert!((h_ft - client_sim_ft).abs() < 50_000_000);
    let exp_arg = ((h_ft - client_sim_ft) as f64) / 10_000.0 / h_sh[1].as_f64().unwrap();
    assert!(exp_arg.exp().is_finite(), "CalculateCurrentDamageStateValues exp must not overflow");

    // Damaged Hobgoblin I: shieldRatio == 0.25, tau == recharge_ms / 5.0, armorDamage == 5.0, hullDamage == 3.0
    let dmg_kv = find_entry(drone_damaged_id);
    let Some(PyRep::Tuple(d_sh)) = dmg_kv.dict_get("shieldInfo") else {
        panic!("expected shieldInfo tuple");
    };
    assert!((d_sh[0].as_f64().unwrap() - 0.25).abs() < 1e-6);
    assert!((d_sh[1].as_f64().unwrap() - expected_tau_ms).abs() < 1e-6);
    assert_eq!(dmg_kv.dict_get("armorDamage").and_then(PyRep::as_f64), Some(5.0));
    assert_eq!(dmg_kv.dict_get("hullDamage").and_then(PyRep::as_f64), Some(3.0));

    // Negative 2: non-GM player querying another pilot's drone receives neutral fallback (no damage leak)
    let foreign_kv = find_entry(foreign_drone_id);
    let Some(PyRep::Tuple(f_sh)) = foreign_kv.dict_get("shieldInfo") else {
        panic!("expected shieldInfo tuple");
    };
    assert_eq!(f_sh[0].as_f64(), Some(1.0));
    assert_eq!(f_sh[1].as_f64(), Some(20_000.0));
    assert_eq!(foreign_kv.dict_get("armorDamage").and_then(PyRep::as_f64), Some(0.0));
    assert_eq!(foreign_kv.dict_get("hullDamage").and_then(PyRep::as_f64), Some(0.0));
}

#[tokio::test]
async fn item_get_info_active_module_uses_attr51_speed_and_repeat_1000_819() {
    let mut sde = (*shared_sde()).clone();
    let base_sb = sde
        .types
        .values()
        .find(|t| t.group_id == 40 && t.published && t.effect_ids.iter().any(|&eid| eid != 16))
        .cloned()
        .expect("canonical published shield booster in group 40");
    let sb_type = 999_819_u32;
    let expected_ms = 4_250.0_f64;
    let mut sb_mod = base_sb;
    sb_mod.type_id = sb_type;
    sb_mod.default_attributes.remove(&73);
    sb_mod.default_attributes.insert(51, expected_ms);
    sde.types.insert(sb_type, sb_mod);
    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_300_555_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 96_300_556_i64;
    let mod_id = 96_300_557_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: sb_type,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(mod_id);
    });

    let mut sess = SessionState::new(99_903);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER;

    let info = handle_dogma_im(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "ItemGetInfo",
        &[PyRep::Integer(mod_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("ItemGetInfo ok");

    let Some(PyRep::Dict(active_effs)) = info.dict_get("activeEffects") else {
        panic!("expected activeEffects Dict");
    };
    let non_online: Vec<&Vec<PyRep>> = active_effs
        .iter()
        .filter(|(eid, _)| eid.as_i64() != Some(16))
        .filter_map(|(_, row)| if let PyRep::List(v) = row { Some(v) } else { None })
        .collect();
    assert_eq!(non_online.len(), 1, "expected 1 active non-online effect entry");
    assert_eq!(non_online[0].get(8), Some(&PyRep::Real(expected_ms)));
    assert_eq!(non_online[0].get(9), Some(&PyRep::Integer(1000)));
}

#[tokio::test]
async fn issue_1352_item_get_info_enforces_auth_ownership_corp_roles_grid_and_gm() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let sys_id = 30_000_142u32;
    let other_sys_id = 30_000_144u32;
    let station_id = 60_003_760i64;

    let caller_id = 96_352_001i64;
    let victim_id = 96_352_002i64;
    let corp_member_ok_id = 96_352_003i64;
    let corp_member_roleless_id = 96_352_004i64;
    let corp_member_wrong_div_id = 96_352_005i64;
    let npc_member_id = 96_352_006i64;
    let gm_id = 96_352_007i64;
    let corp_equip_mgr_id = 96_352_008i64;
    let corp_station_mgr_id = 96_352_009i64;
    let corp_fuel_tech_id = 96_352_010i64;

    let player_corp_id = 98_352_001i64;
    let npc_corp_id = 1_000_009i64;
    let structure_id = 1_096_352_000_002i64;

    let caller_ship_id = 96_352_100i64;
    let nested_in_caller_ship_id = 96_352_101i64;
    let victim_docked_ship_id = 96_352_200i64;
    let victim_docked_item_id = 96_352_201i64;
    let victim_fitted_mod_id = 96_352_202i64;
    let victim_loaded_charge_id = 96_352_203i64;
    let victim_space_on_grid_id = 96_352_210i64;
    let victim_space_on_grid_mod_id = 96_352_211i64;
    let victim_space_off_grid_id = 96_352_212i64;
    let victim_space_cloaked_id = 96_352_213i64;
    let victim_other_sys_ship_id = 96_352_214i64;
    let victim_space_far_bubble_id = 96_352_215i64;
    let victim_space_gate_cloaked_id = 96_352_216i64;
    let corp_office_id = 96_352_300i64;
    let corp_sag2_item_id = 96_352_301i64;
    let npc_corp_item_id = 96_352_302i64;
    let struct_sag2_item_id = 96_352_310i64;
    let struct_deliveries_item_id = 96_352_311i64;
    let struct_hi_mod_id = 96_352_312i64;
    let struct_core_id = 96_352_313i64;
    let struct_fuel_id = 96_352_314i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.values().next().cloned().expect("seed char");
        let mut corp = eve_db::CorporationRecord::new(
            player_corp_id,
            "Authz Corp 1352",
            "A1352",
            victim_id,
        );
        corp.hq_station_id = Some(station_id);
        corp.members.insert(victim_id);
        corp.members.insert(corp_member_ok_id);
        corp.members.insert(corp_member_roleless_id);
        corp.members.insert(corp_member_wrong_div_id);
        corp.members.insert(corp_equip_mgr_id);
        corp.members.insert(corp_station_mgr_id);
        corp.members.insert(corp_fuel_tech_id);
        corp.member_roles.insert(victim_id, u64::MAX);
        corp.member_roles
            .insert(corp_member_ok_id, eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[1]);
        corp.member_roles.insert(corp_member_roleless_id, 0);
        corp.member_roles
            .insert(corp_member_wrong_div_id, eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[0]);
        corp.member_roles
            .insert(corp_equip_mgr_id, eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG);
        corp.member_roles
            .insert(corp_station_mgr_id, eve_db::corp_roles::ROLE_STATION_MANAGER);
        corp.member_roles
            .insert(corp_fuel_tech_id, eve_db::corp_roles::ROLE_STARBASE_CARETAKER);
        snap.corporations.insert(player_corp_id, corp);

        snap.structures.insert(
            structure_id,
            eve_db::UpwellStructureRecord {
                structure_id,
                type_id: 35832,
                owner_corp_id: player_corp_id,
                solar_system_id: sys_id,
                name: "Authz Test Astrahus".into(),
                position: [10_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );

        let mut gov = eve_db::CorpGovState::default();
        gov.offices.insert(station_id, corp_office_id);
        snap.corp_governance.insert(player_corp_id, gov);

        for (cid, cid_corp, st_opt, active_ship) in [
            (caller_id, npc_corp_id, None, caller_ship_id),
            (victim_id, player_corp_id, Some(station_id), victim_docked_ship_id),
            (corp_member_ok_id, player_corp_id, Some(station_id), 0),
            (corp_member_roleless_id, player_corp_id, Some(station_id), 0),
            (corp_member_wrong_div_id, player_corp_id, Some(station_id), 0),
            (corp_equip_mgr_id, player_corp_id, Some(station_id), 0),
            (corp_station_mgr_id, player_corp_id, Some(station_id), 0),
            (corp_fuel_tech_id, player_corp_id, Some(station_id), 0),
            (npc_member_id, npc_corp_id, Some(station_id), 0),
            (gm_id, npc_corp_id, Some(station_id), 0),
        ] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.corporation_id = cid_corp;
            ch.solar_system_id = sys_id;
            ch.station_id = st_opt;
            ch.structure_id = None;
            ch.active_ship_id = active_ship;
            snap.characters.insert(cid, ch);
        }

        let mk_item = |item_id, type_id, owner_id, location_id, flag, qty, singleton| {
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity: qty,
                singleton,
                version: 1,
                ..Default::default()
            }
        };
        snap.items.insert(
            caller_ship_id,
            mk_item(caller_ship_id, 587, caller_id, i64::from(sys_id), InventoryFlag::None, 1, true),
        );
        snap.items.insert(
            nested_in_caller_ship_id,
            mk_item(nested_in_caller_ship_id, 34, victim_id, caller_ship_id, InventoryFlag::Cargo, 10, false),
        );
        snap.items.insert(
            victim_docked_ship_id,
            mk_item(victim_docked_ship_id, 587, victim_id, station_id, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            victim_docked_item_id,
            mk_item(victim_docked_item_id, 34, victim_id, station_id, InventoryFlag::Hangar, 100, false),
        );
        snap.items.insert(
            victim_fitted_mod_id,
            mk_item(victim_fitted_mod_id, 2889, victim_id, victim_docked_ship_id, InventoryFlag::HiSlot0, 1, true),
        );
        snap.items.insert(
            victim_loaded_charge_id,
            mk_item(victim_loaded_charge_id, 183, victim_id, victim_docked_ship_id, InventoryFlag::HiSlot0, 40, false),
        );
        snap.items.insert(
            victim_space_on_grid_id,
            mk_item(victim_space_on_grid_id, 587, victim_id, i64::from(sys_id), InventoryFlag::None, 1, true),
        );
        snap.items.insert(
            victim_space_on_grid_mod_id,
            mk_item(victim_space_on_grid_mod_id, 2889, victim_id, victim_space_on_grid_id, InventoryFlag::HiSlot0, 1, true),
        );
        snap.items.insert(
            victim_space_off_grid_id,
            mk_item(victim_space_off_grid_id, 587, victim_id, i64::from(sys_id), InventoryFlag::None, 1, true),
        );
        snap.items.insert(
            victim_space_cloaked_id,
            mk_item(victim_space_cloaked_id, 587, victim_id, i64::from(sys_id), InventoryFlag::None, 1, true),
        );
        snap.items.insert(
            victim_other_sys_ship_id,
            mk_item(victim_other_sys_ship_id, 587, victim_id, i64::from(other_sys_id), InventoryFlag::None, 1, true),
        );
        snap.items.insert(
            corp_office_id,
            mk_item(corp_office_id, 27, player_corp_id, station_id, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            corp_sag2_item_id,
            mk_item(corp_sag2_item_id, 34, player_corp_id, corp_office_id, InventoryFlag::CorpSAG2, 500, false),
        );
        snap.items.insert(
            npc_corp_item_id,
            mk_item(npc_corp_item_id, 34, npc_corp_id, station_id, InventoryFlag::CorpSAG2, 500, false),
        );
        snap.items.insert(
            victim_space_far_bubble_id,
            mk_item(victim_space_far_bubble_id, 587, victim_id, i64::from(sys_id), InventoryFlag::None, 1, true),
        );
        snap.items.insert(
            victim_space_gate_cloaked_id,
            mk_item(victim_space_gate_cloaked_id, 587, victim_id, i64::from(sys_id), InventoryFlag::None, 1, true),
        );
        snap.items.insert(
            struct_sag2_item_id,
            mk_item(struct_sag2_item_id, 34, player_corp_id, structure_id, InventoryFlag::CorpSAG2, 100, false),
        );
        snap.items.insert(
            struct_deliveries_item_id,
            mk_item(struct_deliveries_item_id, 34, player_corp_id, structure_id, InventoryFlag::Deliveries, 100, false),
        );
        snap.items.insert(
            struct_hi_mod_id,
            mk_item(struct_hi_mod_id, 2889, player_corp_id, structure_id, InventoryFlag::HiSlot0, 1, true),
        );
        snap.items.insert(
            struct_core_id,
            mk_item(struct_core_id, 34, player_corp_id, structure_id, InventoryFlag::QuantumCoreRoom, 1, true),
        );
        snap.items.insert(
            struct_fuel_id,
            mk_item(struct_fuel_id, 34, player_corp_id, structure_id, InventoryFlag::StructureFuel, 200, false),
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let other_sys = ctx.systems.get_or_spawn(other_sys_id);
    let mk_ball = |id: i64, x: f64| Ball {
        id,
        type_id: 587,
        position: DVec3::new(x, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_350_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys
        .spawn_ball_with_meta(
            mk_ball(caller_ship_id, 0.0),
            CombatState::default(),
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: caller_id,
                char_id: Some(caller_id),
                ..Default::default()
            },
        )
        .await;
    let _ = sys
        .spawn_ball_with_meta(
            mk_ball(victim_space_on_grid_id, 10_000.0),
            CombatState::default(),
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: victim_id,
                char_id: Some(victim_id),
                ..Default::default()
            },
        )
        .await;
    let _ = sys
        .spawn_ball_with_meta(
            mk_ball(victim_space_off_grid_id, 20_000_000.0),
            CombatState::default(),
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: victim_id,
                char_id: Some(victim_id),
                ..Default::default()
            },
        )
        .await;
    let _ = sys
        .spawn_ball_with_meta(
            mk_ball(victim_space_far_bubble_id, 300_000.0),
            CombatState::default(),
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: victim_id,
                char_id: Some(victim_id),
                ..Default::default()
            },
        )
        .await;
    let _ = sys
        .spawn_ball_with_meta(
            mk_ball(victim_space_gate_cloaked_id, 5_000.0),
            CombatState {
                gate_cloak_remaining_ticks: 30,
                ..Default::default()
            },
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: victim_id,
                char_id: Some(victim_id),
                ..Default::default()
            },
        )
        .await;
    let _ = sys
        .spawn_ball_with_meta(
            mk_ball(victim_space_cloaked_id, 5_000.0),
            CombatState {
                is_cloaked: true,
                ..Default::default()
            },
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: victim_id,
                char_id: Some(victim_id),
                is_cloaked: true,
                ..Default::default()
            },
        )
        .await;
    let _ = other_sys
        .spawn_ball_with_meta(
            mk_ball(victim_other_sys_ship_id, 1_000.0),
            CombatState::default(),
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: victim_id,
                char_id: Some(victim_id),
                ..Default::default()
            },
        )
        .await;

    let endpoints = [
        ("dogmaIM", "ItemGetInfo"),
        ("dogmaIM", "GetDynamicItemInfo"),
        ("invbroker", "GetItemInfo"),
        ("invbroker", "GetDynamicItemInfo"),
    ];
    let kw = HashMap::new();

    // 1. Unauthenticated caller is rejected on all 4 endpoints (zero DB mutations).
    let mut unauth = SessionState::new(96_352_900);
    let (epoch0, commits0) = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    for (svc, meth) in endpoints {
        let err = handle_dogma_im(&ctx, &mut unauth, None, svc, meth, &[PyRep::Integer(587)], &kw)
            .await
            .expect("handled")
            .unwrap_err();
        assert_ne!(err.code, "", "unauthenticated {svc}.{meth} must fail");
    }
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), (epoch0, commits0));

    let mk_sess = |cid: i64, corp_id: i64, st_opt: Option<i64>, ship_opt: Option<i64>, role: u64| {
        let mut s = SessionState::new(cid as u64);
        s.user_id = Some(cid as u64);
        s.character_id = Some(cid);
        s.corporation_id = Some(corp_id);
        s.solar_system_id = Some(sys_id);
        s.station_id = st_opt;
        s.ship_id = ship_opt;
        s.role = role;
        s
    };

    // 2. Owner and nested item in owner's ship succeed; cross-owner private docked items,
    // fitted modules, and sublocation charge keys are rejected read-purely.
    let mut caller_sess = mk_sess(
        caller_id,
        npc_corp_id,
        None,
        Some(caller_ship_id),
        eve_machonet::service_roles::ROLE_PLAYER,
    );
    for (svc, meth) in endpoints {
        assert!(
            handle_dogma_im(&ctx, &mut caller_sess, None, svc, meth, &[PyRep::Integer(caller_ship_id)], &kw)
                .await
                .expect("handled")
                .is_ok(),
            "owner must be allowed on {svc}.{meth}"
        );
        assert!(
            handle_dogma_im(&ctx, &mut caller_sess, None, svc, meth, &[PyRep::Integer(nested_in_caller_ship_id)], &kw)
                .await
                .expect("handled")
                .is_ok(),
            "nested item inside caller ship must be allowed on {svc}.{meth}"
        );
        for denied_id in [
            victim_docked_ship_id,
            victim_docked_item_id,
            victim_fitted_mod_id,
            victim_space_on_grid_mod_id,
            victim_space_off_grid_id,
            victim_space_cloaked_id,
            victim_space_gate_cloaked_id,
            victim_other_sys_ship_id,
        ] {
            let err = handle_dogma_im(&ctx, &mut caller_sess, None, svc, meth, &[PyRep::Integer(denied_id)], &kw)
                .await
                .expect("handled")
                .unwrap_err();
            assert_eq!(err.code, "AccessDenied", "{svc}.{meth} must deny item {denied_id}");
        }
        let subloc_arg = PyRep::Tuple(vec![
            PyRep::Integer(victim_docked_ship_id),
            PyRep::Integer(InventoryFlag::HiSlot0 as i64),
            PyRep::Integer(183),
        ]);
        let sub_err = handle_dogma_im(&ctx, &mut caller_sess, None, svc, meth, &[subloc_arg], &kw)
            .await
            .expect("handled")
            .unwrap_err();
        assert_eq!(sub_err.code, "AccessDenied", "{svc}.{meth} must deny foreign sublocation tuple");
    }

    // 3. On-grid visible uncloaked space ships (including >250 km within 8,000 km grid bubble)
    // are allowed for undocked caller in same system.
    for (svc, meth) in endpoints {
        for visible_id in [victim_space_on_grid_id, victim_space_far_bubble_id] {
            let info = handle_dogma_im(
                &ctx,
                &mut caller_sess,
                None,
                svc,
                meth,
                &[PyRep::Integer(visible_id)],
                &kw,
            )
            .await
            .expect("handled")
            .expect("on-grid space ship must be inspectable");
            assert_eq!(info.dict_get("typeID"), Some(&PyRep::Integer(587)));
        }
    }

    // 4. Player-corp hangar item: DB role member allowed; roleless (even with spoofed session role),
    // wrong-division member, and NPC corp member all denied.
    let mut corp_ok_sess = mk_sess(
        corp_member_ok_id,
        player_corp_id,
        Some(station_id),
        None,
        eve_machonet::service_roles::ROLE_PLAYER,
    );
    let mut corp_roleless_sess = mk_sess(
        corp_member_roleless_id,
        player_corp_id,
        Some(station_id),
        None,
        eve_machonet::service_roles::ROLE_PLAYER,
    );
    corp_roleless_sess.corp_role = u64::MAX; // stale/spoofed session role must NOT grant access
    let mut corp_wrong_div_sess = mk_sess(
        corp_member_wrong_div_id,
        player_corp_id,
        Some(station_id),
        None,
        eve_machonet::service_roles::ROLE_PLAYER,
    );
    let mut npc_sess = mk_sess(
        npc_member_id,
        npc_corp_id,
        Some(station_id),
        None,
        eve_machonet::service_roles::ROLE_PLAYER,
    );
    npc_sess.corp_role = u64::MAX;

    for (svc, meth) in endpoints {
        assert!(
            handle_dogma_im(&ctx, &mut corp_ok_sess, None, svc, meth, &[PyRep::Integer(corp_sag2_item_id)], &kw)
                .await
                .expect("handled")
                .is_ok(),
            "corp member with CorpSAG2 query role must be allowed on {svc}.{meth}"
        );
        assert_eq!(
            handle_dogma_im(&ctx, &mut corp_roleless_sess, None, svc, meth, &[PyRep::Integer(corp_sag2_item_id)], &kw)
                .await
                .expect("handled")
                .unwrap_err()
                .code,
            "AccessDenied"
        );
        assert_eq!(
            handle_dogma_im(&ctx, &mut corp_wrong_div_sess, None, svc, meth, &[PyRep::Integer(corp_sag2_item_id)], &kw)
                .await
                .expect("handled")
                .unwrap_err()
                .code,
            "AccessDenied"
        );
        assert_eq!(
            handle_dogma_im(&ctx, &mut npc_sess, None, svc, meth, &[PyRep::Integer(npc_corp_item_id)], &kw)
                .await
                .expect("handled")
                .unwrap_err()
                .code,
            "AccessDenied"
        );
    }

    // 4b. Structure-hosted items: ROLE_EQUIPMENT_CONFIG / ROLE_STATION_MANAGER can inspect
    // equipment/core flags but CANNOT bypass CorpSAG / Deliveries; fuel technician can inspect
    // StructureFuel but NOT equipment slots.
    let mut equip_sess = mk_sess(
        corp_equip_mgr_id,
        player_corp_id,
        Some(station_id),
        None,
        eve_machonet::service_roles::ROLE_PLAYER,
    );
    let mut station_mgr_sess = mk_sess(
        corp_station_mgr_id,
        player_corp_id,
        Some(station_id),
        None,
        eve_machonet::service_roles::ROLE_PLAYER,
    );
    let mut fuel_tech_sess = mk_sess(
        corp_fuel_tech_id,
        player_corp_id,
        Some(station_id),
        None,
        eve_machonet::service_roles::ROLE_PLAYER,
    );
    for (svc, meth) in endpoints {
        for allowed_id in [struct_hi_mod_id, struct_core_id] {
            assert!(
                handle_dogma_im(&ctx, &mut equip_sess, None, svc, meth, &[PyRep::Integer(allowed_id)], &kw)
                    .await
                    .expect("handled")
                    .is_ok(),
                "ROLE_EQUIPMENT_CONFIG must inspect structure item {allowed_id} on {svc}.{meth}"
            );
            assert!(
                handle_dogma_im(&ctx, &mut station_mgr_sess, None, svc, meth, &[PyRep::Integer(allowed_id)], &kw)
                    .await
                    .expect("handled")
                    .is_ok(),
                "ROLE_STATION_MANAGER must inspect structure item {allowed_id} on {svc}.{meth}"
            );
        }
        assert!(
            handle_dogma_im(&ctx, &mut corp_ok_sess, None, svc, meth, &[PyRep::Integer(struct_sag2_item_id)], &kw)
                .await
                .expect("handled")
                .is_ok(),
            "CorpSAG2 query role must inspect structure CorpSAG2 item on {svc}.{meth}"
        );
        assert!(
            handle_dogma_im(&ctx, &mut fuel_tech_sess, None, svc, meth, &[PyRep::Integer(struct_fuel_id)], &kw)
                .await
                .expect("handled")
                .is_ok(),
            "fuel technician role must inspect StructureFuel on {svc}.{meth}"
        );
        assert!(
            handle_dogma_im(&ctx, &mut station_mgr_sess, None, svc, meth, &[PyRep::Integer(struct_fuel_id)], &kw)
                .await
                .expect("handled")
                .is_ok(),
            "ROLE_STATION_MANAGER must inspect StructureFuel on {svc}.{meth}"
        );
        for denied_id in [struct_sag2_item_id, struct_deliveries_item_id, struct_fuel_id] {
            assert_eq!(
                handle_dogma_im(&ctx, &mut equip_sess, None, svc, meth, &[PyRep::Integer(denied_id)], &kw)
                    .await
                    .expect("handled")
                    .unwrap_err()
                    .code,
                "AccessDenied",
                "ROLE_EQUIPMENT_CONFIG must be denied structure item {denied_id} on {svc}.{meth}"
            );
        }
        for denied_id in [struct_sag2_item_id, struct_deliveries_item_id] {
            assert_eq!(
                handle_dogma_im(&ctx, &mut station_mgr_sess, None, svc, meth, &[PyRep::Integer(denied_id)], &kw)
                    .await
                    .expect("handled")
                    .unwrap_err()
                    .code,
                "AccessDenied",
                "ROLE_STATION_MANAGER must be denied structure hangar/deliveries item {denied_id} on {svc}.{meth}"
            );
        }
        assert_eq!(
            handle_dogma_im(&ctx, &mut fuel_tech_sess, None, svc, meth, &[PyRep::Integer(struct_hi_mod_id)], &kw)
                .await
                .expect("handled")
                .unwrap_err()
                .code,
            "AccessDenied",
            "fuel technician must be denied structure HiSlot0 item on {svc}.{meth}"
        );
    }

    // 5. GM role can inspect private foreign docked items.
    let mut gm_sess = mk_sess(
        gm_id,
        npc_corp_id,
        Some(station_id),
        None,
        eve_machonet::service_roles::ROLE_GMH,
    );
    for (svc, meth) in endpoints {
        assert!(
            handle_dogma_im(&ctx, &mut gm_sess, None, svc, meth, &[PyRep::Integer(victim_docked_item_id)], &kw)
                .await
                .expect("handled")
                .is_ok(),
            "GM must be allowed on {svc}.{meth}"
        );
    }
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), (epoch0, commits0));
}

#[tokio::test]
async fn issue_1352_link_all_weapons_guards_auth_owner_docked_cloaked_and_read_purity() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let sys_id = 30_000_142u32;
    let station_id = 60_003_760i64;
    let structure_id = 1_096_352_000_001i64;
    let char_id = 96_352_501i64;
    let other_char_id = 96_352_502i64;

    let ship_id = 96_352_600i64;
    let inactive_owned_ship_id = 96_352_601i64;
    let foreign_ship_id = 96_352_602i64;
    let gun1_id = 96_352_610i64;
    let gun2_id = 96_352_611i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.values().next().cloned().expect("seed char");
        let mut ch = base_char.clone();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        let mk_item = |item_id, type_id, owner_id, location_id, flag| InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        };
        snap.items.insert(ship_id, mk_item(ship_id, 587, char_id, i64::from(sys_id), InventoryFlag::None));
        snap.items.insert(
            inactive_owned_ship_id,
            mk_item(inactive_owned_ship_id, 587, char_id, i64::from(sys_id), InventoryFlag::None),
        );
        snap.items.insert(
            foreign_ship_id,
            mk_item(foreign_ship_id, 587, other_char_id, i64::from(sys_id), InventoryFlag::None),
        );
        // Only 1 gun initially (insufficient to form a group).
        snap.items.insert(gun1_id, mk_item(gun1_id, 2889, char_id, ship_id, InventoryFlag::HiSlot0));
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_350_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: char_id,
                char_id: Some(char_id),
                ..Default::default()
            },
        )
        .await;

    let kw = HashMap::new();
    let (epoch0, commits0) = (ctx.db.state_epoch(), ctx.db.tx_commit_count());

    // 1. Unauthenticated -> rejected, zero DB mutation.
    let mut unauth = SessionState::new(96_352_990);
    assert!(
        handle_dogma_im(&ctx, &mut unauth, None, "dogmaIM", "LinkAllWeapons", &[PyRep::Integer(ship_id)], &kw)
            .await
            .expect("handled")
            .is_err()
    );
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), (epoch0, commits0));

    let mut sess = SessionState::new(char_id as u64);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER;

    // 2. Cross-owner ship -> ShipOwnershipViolation, zero DB mutation.
    assert_eq!(
        handle_dogma_im(&ctx, &mut sess, None, "dogmaIM", "LinkAllWeapons", &[PyRep::Integer(foreign_ship_id)], &kw)
            .await
            .expect("handled")
            .unwrap_err()
            .code,
        "ShipOwnershipViolation"
    );

    // 3. Undocked + uncloaked with only 1 weapon -> InvalidWeaponGroup, zero DB mutation!
    assert_eq!(
        handle_dogma_im(&ctx, &mut sess, None, "dogmaIM", "LinkAllWeapons", &[PyRep::Integer(ship_id)], &kw)
            .await
            .expect("handled")
            .unwrap_err()
            .code,
        "InvalidWeaponGroup"
    );
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), (epoch0, commits0));
    ctx.db.with_snapshot(|snap| assert!(!snap.weapon_banks.contains_key(&ship_id)));

    // Fit second identical gun so ship now has a valid linkable weapon pair.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            gun2_id,
            InventoryItemRecord {
                item_id: gun2_id,
                type_id: 2889,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    // 4. Docked in station -> CannotLinkWeaponsWhileDocked, zero DB mutation.
    sess.station_id = Some(station_id);
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    assert_eq!(
        handle_dogma_im(&ctx, &mut sess, None, "dogmaIM", "LinkAllWeapons", &[PyRep::Integer(ship_id)], &kw)
            .await
            .expect("handled")
            .unwrap_err()
            .code,
        "CannotLinkWeaponsWhileDocked"
    );
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before);
    sess.station_id = None;

    // 5. Docked in structure -> CannotLinkWeaponsWhileDocked, zero DB mutation.
    sess.structure_id = Some(structure_id);
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    assert_eq!(
        handle_dogma_im(&ctx, &mut sess, None, "dogmaIM", "LinkAllWeapons", &[PyRep::Integer(ship_id)], &kw)
            .await
            .expect("handled")
            .unwrap_err()
            .code,
        "CannotLinkWeaponsWhileDocked"
    );
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before);
    sess.structure_id = None;

    // 6. Inactive owned ship -> ShipNotActive, zero DB mutation.
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    assert_eq!(
        handle_dogma_im(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "LinkAllWeapons",
            &[PyRep::Integer(inactive_owned_ship_id)],
            &kw,
        )
        .await
        .expect("handled")
        .unwrap_err()
        .code,
        "ShipNotActive"
    );
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before);

    // 6b. Unboarded caller (active_ship_id == 0 && sess.ship_id == None) and
    // session/DB active-ship disagreement -> ShipNotActive, zero DB mutation.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = 0;
        }
    });
    sess.ship_id = None;
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    assert_eq!(
        handle_dogma_im(&ctx, &mut sess, None, "dogmaIM", "LinkAllWeapons", &[PyRep::Integer(ship_id)], &kw)
            .await
            .expect("handled")
            .unwrap_err()
            .code,
        "ShipNotActive"
    );
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before);

    sess.ship_id = Some(ship_id);
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    assert_eq!(
        handle_dogma_im(&ctx, &mut sess, None, "dogmaIM", "LinkAllWeapons", &[PyRep::Integer(ship_id)], &kw)
            .await
            .expect("handled")
            .unwrap_err()
            .code,
        "ShipNotActive"
    );
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
        }
    });
    sess.ship_id = None;
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    assert_eq!(
        handle_dogma_im(&ctx, &mut sess, None, "dogmaIM", "LinkAllWeapons", &[PyRep::Integer(ship_id)], &kw)
            .await
            .expect("handled")
            .unwrap_err()
            .code,
        "ShipNotActive"
    );
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before);

    sess.ship_id = Some(ship_id);

    // 7. Cloaked in space -> CannotLinkWeaponsWhileCloaked, zero DB mutation.
    let _ = sys
        .modify_combat_state(ship_id, |cs| {
            cs.is_cloaked = true;
        })
        .await;
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    assert_eq!(
        handle_dogma_im(&ctx, &mut sess, None, "dogmaIM", "LinkAllWeapons", &[PyRep::Integer(ship_id)], &kw)
            .await
            .expect("handled")
            .unwrap_err()
            .code,
        "CannotLinkWeaponsWhileCloaked"
    );
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before);

    // 8. Gate-cloaked in space -> CannotLinkWeaponsWhileCloaked, zero DB mutation.
    let _ = sys
        .modify_combat_state(ship_id, |cs| {
            cs.is_cloaked = false;
            cs.gate_cloak_remaining_ticks = 30;
        })
        .await;
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    assert_eq!(
        handle_dogma_im(&ctx, &mut sess, None, "dogmaIM", "LinkAllWeapons", &[PyRep::Integer(ship_id)], &kw)
            .await
            .expect("handled")
            .unwrap_err()
            .code,
        "CannotLinkWeaponsWhileCloaked"
    );
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before);
    ctx.db.with_snapshot(|snap| assert!(!snap.weapon_banks.contains_key(&ship_id)));

    // 9. Undocked + uncloaked active owner with 2+ identical weapons -> succeeds!
    let _ = sys
        .modify_combat_state(ship_id, |cs| {
            cs.is_cloaked = false;
            cs.gate_cloak_remaining_ticks = 0;
        })
        .await;
    let linked = handle_dogma_im(&ctx, &mut sess, None, "dogmaIM", "LinkAllWeapons", &[PyRep::Integer(ship_id)], &kw)
        .await
        .expect("handled")
        .expect("undocked uncloaked LinkAllWeapons must succeed");
    assert!(matches!(linked, PyRep::Dict(ref d) if d.len() == 1));
    ctx.db.with_snapshot(|snap| {
        let banks = snap.weapon_banks.get(&ship_id).expect("weapon banks created");
        assert_eq!(banks.len(), 1);
        assert_eq!(banks.values().next().map(Vec::len), Some(2));
    });
}
