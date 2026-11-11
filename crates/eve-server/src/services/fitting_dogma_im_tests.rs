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
    let info_ship = handle_item_get_info(&ctx, &[PyRep::Integer(ship_id)]).expect("ship info");
    assert_ne!(info_ship, PyRep::None);

    let info_mod = handle_item_get_info(&ctx, &[PyRep::Integer(gun1_id)]).expect("mod info");
    assert_ne!(info_mod, PyRep::None);

    let info_type = handle_item_get_info(&ctx, &[PyRep::Integer(587)]).expect("type info");
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
