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

#[tokio::test]
async fn r2_p1_1_foreign_on_grid_item_get_info_returns_public_only_and_never_mutates_fit() {
    let sde = shared_sde();
    let ctx = ServerContext::with_sde_and_repository(
        Arc::clone(&sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let sys_id = 30_000_142u32;
    let caller_id = 980_101i64;
    let victim_id = 980_102i64;
    let caller_ship = 980_201i64;
    let victim_ship = 980_202i64;
    let ok_gun_id = 980_203i64;
    let over_gun_id = 980_204i64;
    let victim_drone_id = 980_205i64;

    let mut req_skills: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    for tid in [587u32, 491u32, 2889u32, 2454u32] {
        if let Some(ty) = sde.types.get(&tid) {
            for &aid in &[182u16, 183, 184, 1285, 1289, 1290] {
                if let Some(&sid) = ty.default_attributes.get(&aid) {
                    let s = sid as u32;
                    if s > 0 {
                        req_skills.insert(s);
                    }
                }
            }
        }
    }
    req_skills.insert(3436); // Drones
    req_skills.insert(3442); // Drone Interfacing

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, active_ship) in [(caller_id, caller_ship), (victim_id, victim_ship)] {
            snap.characters.insert(
                cid,
                eve_db::CharacterRecord {
                    character_id: cid,
                    user_id: cid as u64,
                    name: format!("Pilot {cid}"),
                    corporation_id: 1_000_009,
                    solar_system_id: sys_id,
                    active_ship_id: active_ship,
                    ..Default::default()
                },
            );
        }
        snap.skills.insert(
            victim_id,
            req_skills
                .into_iter()
                .map(|tid| {
                    (
                        tid,
                        eve_db::TrainedSkillRecord {
                            type_id: tid,
                            level: 5,
                            skill_points: 256_000.0,
                        },
                    )
                })
                .collect(),
        );
        snap.items.insert(
            caller_ship,
            InventoryItemRecord {
                item_id: caller_ship,
                type_id: 587,
                owner_id: caller_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            victim_ship,
            InventoryItemRecord {
                item_id: victim_ship,
                type_id: 587,
                owner_id: victim_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(265, 999.0)]),
                ..Default::default()
            },
        );
        snap.items.insert(
            ok_gun_id,
            InventoryItemRecord {
                item_id: ok_gun_id,
                type_id: 2889,
                owner_id: victim_id,
                location_id: victim_ship,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            over_gun_id,
            InventoryItemRecord {
                item_id: over_gun_id,
                type_id: 491, // 1400mm Howitzer Artillery (140 PG > 41 PG Rifter)
                owner_id: victim_id,
                location_id: victim_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(ok_gun_id, true);
        snap.module_online_states.insert(over_gun_id, true);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (bid, tid, owner, pos_x) in [
        (caller_ship, 587u32, caller_id, 1_000.0),
        (victim_ship, 587u32, victim_id, 2_000.0),
        (victim_drone_id, 2454u32, victim_id, 2_500.0),
    ] {
        let _ = sys
            .spawn_ball_with_meta(
                Ball {
                    id: bid,
                    type_id: tid,
                    position: DVec3::new(pos_x, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 3.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState::default(),
                SlimItemMeta {
                    owner_id: owner,
                    char_id: Some(owner),
                    ..Default::default()
                },
            )
            .await;
    }

    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            victim_drone_id,
            crate::services::combat_crimewatch::DroneSpaceState {
                ball_id: victim_drone_id,
                drone_item_id: victim_drone_id,
                type_id: 2454,
                group_id: 100,
                owner_char_id: victim_id,
                original_owner_char_id: victim_id,
                parent_ship_id: victim_ship,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 2_600.0,
                optimal_range: 2_000.0,
                damage_vector: eve_destiny::DamageVector {
                    em: 0.0,
                    thermal: 20.0,
                    kinetic: 0.0,
                    explosive: 0.0,
                },
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 0.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });

    let mut caller_sess = SessionState::new(980_001);
    caller_sess.user_id = Some(caller_id as u64);
    caller_sess.character_id = Some(caller_id);
    caller_sess.solar_system_id = Some(sys_id);
    caller_sess.ship_id = Some(caller_ship);

    let mut victim_sess = SessionState::new(980_002);
    victim_sess.user_id = Some(victim_id as u64);
    victim_sess.character_id = Some(victim_id);
    victim_sess.solar_system_id = Some(sys_id);
    victim_sess.ship_id = Some(victim_ship);

    let mut gm_sess = SessionState::new(980_003);
    gm_sess.user_id = Some(980_003);
    gm_sess.character_id = Some(caller_id);
    gm_sess.solar_system_id = Some(sys_id);
    gm_sess.ship_id = Some(caller_ship);
    gm_sess.role = eve_machonet::service_roles::ROLE_GM_MASK;

    let kw = HashMap::new();
    let attr_val = |info: &PyRep, aid: i64| -> Option<f64> {
        let PyRep::Dict(entries) = info.dict_get("attributes")? else {
            return None;
        };
        entries.iter().find_map(|(k, v)| match (k.as_i64(), v) {
            (Some(id), PyRep::Real(val)) if id == aid => Some(*val),
            (Some(id), PyRep::Integer(val)) if id == aid => Some(*val as f64),
            _ => None,
        })
    };

    // Cold cache before foreign on-grid inspection.
    ctx.ship_fittings.write().unwrap().remove(&victim_ship);
    let (epoch_before, tx_before) = (ctx.db.state_epoch(), ctx.db.tx_commit_count());

    let foreign_ship_info = handle_dogma_im(
        &ctx,
        &mut caller_sess,
        None,
        "dogmaIM",
        "ItemGetInfo",
        &[PyRep::Integer(victim_ship)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("foreign on-grid ship ItemGetInfo returns sanitized hull metadata");
    assert_eq!(foreign_ship_info.dict_get("typeID"), Some(&PyRep::Integer(587)));
    assert_eq!(foreign_ship_info.dict_get("invItem"), Some(&PyRep::None));
    assert_eq!(
        foreign_ship_info.dict_get("dynamicAttributes"),
        Some(&PyRep::Dict(Vec::new()))
    );
    assert_eq!(
        foreign_ship_info.dict_get("activeEffects"),
        Some(&PyRep::Dict(Vec::new()))
    );
    let hull_defaults = &sde.show_info_for_type(587).expect("587 show_info").default_attributes;
    assert_eq!(
        attr_val(&foreign_ship_info, 265),
        hull_defaults.get(&265).copied(),
        "Foreign on-grid inspection must return SDE hull default armorHP, not dynamicAttributes 999.0"
    );

    // Foreign on-grid launched drone inspection also skips ctx.fit & invItem.
    let foreign_drone_info = handle_dogma_im(
        &ctx,
        &mut caller_sess,
        None,
        "dogmaIM",
        "ItemGetInfo",
        &[PyRep::Integer(victim_drone_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("foreign on-grid drone ItemGetInfo returns sanitized metadata");
    assert_eq!(foreign_drone_info.dict_get("typeID"), Some(&PyRep::Integer(2454)));
    assert_eq!(foreign_drone_info.dict_get("invItem"), Some(&PyRep::None));
    assert_eq!(
        foreign_drone_info.dict_get("dynamicAttributes"),
        Some(&PyRep::Dict(Vec::new()))
    );

    assert!(
        !ctx.ship_fittings.read().unwrap().contains_key(&victim_ship),
        "Foreign on-grid ItemGetInfo must not populate cold victim ship_fittings cache"
    );
    assert_eq!(
        (ctx.db.state_epoch(), ctx.db.tx_commit_count()),
        (epoch_before, tx_before),
        "Foreign on-grid ItemGetInfo must not commit DB writes"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.module_online_states.get(&over_gun_id),
            Some(&true),
            "Foreign on-grid ItemGetInfo must not trigger ctx.fit to offline overloaded victim modules"
        );
    });

    // Owner inspection DOES invoke ctx.fit, returning invItem + dynamicAttributes and offlining PG-overloaded module.
    let owner_ship_info = handle_dogma_im(
        &ctx,
        &mut victim_sess,
        None,
        "dogmaIM",
        "ItemGetInfo",
        &[PyRep::Integer(victim_ship)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("owner ItemGetInfo must succeed");
    assert_ne!(owner_ship_info.dict_get("invItem"), Some(&PyRep::None));
    assert_eq!(attr_val(&owner_ship_info, 265), Some(999.0));
    assert!(ctx.ship_fittings.read().unwrap().contains_key(&victim_ship));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.module_online_states.get(&over_gun_id),
            Some(&false),
            "Owner ctx.fit must still reconcile overloaded PG module"
        );
    });

    // GM retains full inspection capability even on cold cache.
    ctx.ship_fittings.write().unwrap().remove(&victim_ship);
    let gm_ship_info = handle_dogma_im(
        &ctx,
        &mut gm_sess,
        None,
        "dogmaIM",
        "ItemGetInfo",
        &[PyRep::Integer(victim_ship)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("GM ItemGetInfo must succeed");
    assert_ne!(gm_ship_info.dict_get("invItem"), Some(&PyRep::None));
    assert_eq!(attr_val(&gm_ship_info, 265), Some(999.0));
}

#[tokio::test]
async fn r2_p1_2_link_weapons_peel_merge_and_link_all_enforce_guards_atomically() {
    let sde = shared_sde();
    let ctx = ServerContext::with_sde_and_repository(
        Arc::clone(&sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 981_101i64;
    let sys_id = 30_000_142u32;
    let station_id = 60_003_760i64;
    let structure_id = 1_000_000_981_101i64;
    let ship_id = 981_201i64;
    let other_ship_id = 981_202i64;
    let gun1 = 981_301i64;
    let gun2 = 981_302i64;
    let gun3 = 981_303i64;
    let gun4 = 981_304i64;
    let gun5 = 981_305i64;
    let diff_gun = 981_306i64;
    let salvager = 981_307i64;
    let cargo_gun = 981_308i64;
    let other_gun1 = 981_309i64;
    let other_gun2 = 981_310i64;

    let mk = |id: i64, tid: u32, loc: i64, flag: InventoryFlag| InventoryItemRecord {
        item_id: id,
        type_id: tid,
        owner_id: char_id,
        location_id: loc,
        flag,
        quantity: 1,
        singleton: true,
        version: 1,
        ..Default::default()
    };

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            char_id,
            eve_db::CharacterRecord {
                character_id: char_id,
                user_id: char_id as u64,
                name: "Gunner".into(),
                corporation_id: 1_000_009,
                solar_system_id: sys_id,
                active_ship_id: ship_id,
                ..Default::default()
            },
        );
        snap.items.insert(ship_id, mk(ship_id, 587, i64::from(sys_id), InventoryFlag::None));
        snap.items.insert(other_ship_id, mk(other_ship_id, 587, i64::from(sys_id), InventoryFlag::None));
        snap.items.insert(gun1, mk(gun1, 2889, ship_id, InventoryFlag::HiSlot0));
        snap.items.insert(gun2, mk(gun2, 2889, ship_id, InventoryFlag::HiSlot1));
        snap.items.insert(gun3, mk(gun3, 2889, ship_id, InventoryFlag::HiSlot2));
        snap.items.insert(gun4, mk(gun4, 2889, ship_id, InventoryFlag::HiSlot3));
        snap.items.insert(gun5, mk(gun5, 2889, ship_id, InventoryFlag::HiSlot4));
        snap.items.insert(diff_gun, mk(diff_gun, 2873, ship_id, InventoryFlag::HiSlot5));
        snap.items.insert(salvager, mk(salvager, 25_861, ship_id, InventoryFlag::HiSlot6));
        snap.items.insert(cargo_gun, mk(cargo_gun, 2889, ship_id, InventoryFlag::Cargo));
        snap.items.insert(other_gun1, mk(other_gun1, 2889, other_ship_id, InventoryFlag::HiSlot0));
        snap.items.insert(other_gun2, mk(other_gun2, 2889, other_ship_id, InventoryFlag::HiSlot1));
        snap.weapon_banks
            .insert(ship_id, std::collections::HashMap::from([(gun1, vec![gun1, gun2])]));
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    for sid in [ship_id, other_ship_id] {
        sys.spawn_ball_with_meta(
            Ball {
                id: sid,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 40.0,
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
        .await
        .unwrap();
    }

    let mut sess = SessionState::new(981_001);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();
    let expected_banks = std::collections::HashMap::from([(gun1, vec![gun1, gun2])]);

    let assert_all_rejected = |label: &str, code: &str, actual: Result<PyRep, BridgeRpcError>, before: (u64, u64)| {
        assert_eq!(
            actual.unwrap_err().code,
            code,
            "{label} must fail with {code}"
        );
        assert_eq!(
            (ctx.db.state_epoch(), ctx.db.tx_commit_count()),
            before,
            "{label} must not mutate DB epoch or commit count"
        );
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                snap.weapon_banks.get(&ship_id),
                Some(&expected_banks),
                "{label} must preserve existing weapon_banks"
            );
        });
    };

    // 1. Docked in station (session & DB) rejects all 4 RPCs without mutating weapon_banks.
    sess.station_id = Some(station_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(station_id);
    });
    for (svc, meth, args) in [
        ("dogmaIM", "LinkWeapons", vec![PyRep::Integer(ship_id), PyRep::Integer(gun3), PyRep::Integer(gun4)]),
        ("charFittingMgr", "PeelAndLink", vec![PyRep::Integer(ship_id), PyRep::Integer(gun3), PyRep::Integer(gun2)]),
        ("dogmaIM", "MergeModuleGroups", vec![PyRep::Integer(ship_id), PyRep::Integer(gun1), PyRep::Integer(gun3)]),
        ("charFittingMgr", "LinkAllWeapons", vec![PyRep::Integer(ship_id)]),
    ] {
        let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
        let res = handle_dogma_im(&ctx, &mut sess, None, svc, meth, &args, &kw)
            .await
            .expect("handled");
        assert_all_rejected(&format!("docked_station {svc}.{meth}"), "CannotLinkWeaponsWhileDocked", res, before);
    }

    // 2. TOCTOU: session claims undocked in space while DB character stays docked in structure.
    sess.station_id = None;
    sess.structure_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = Some(structure_id);
    });
    for (svc, meth, args) in [
        ("charFittingMgr", "LinkWeapons", vec![PyRep::Integer(ship_id), PyRep::Integer(gun3), PyRep::Integer(gun4)]),
        ("dogmaIM", "PeelAndLink", vec![PyRep::Integer(ship_id), PyRep::Integer(gun3), PyRep::Integer(gun2)]),
        ("charFittingMgr", "MergeModuleGroups", vec![PyRep::Integer(ship_id), PyRep::Integer(gun1), PyRep::Integer(gun3)]),
        ("dogmaIM", "LinkAllWeapons", vec![PyRep::Integer(ship_id)]),
    ] {
        let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
        let res = handle_dogma_im(&ctx, &mut sess, None, svc, meth, &args, &kw)
            .await
            .expect("handled");
        assert_all_rejected(&format!("toctou_db_structure {svc}.{meth}"), "CannotLinkWeaponsWhileDocked", res, before);
    }

    // Clear docked state; test cloaked & gate-cloaked rejection.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().structure_id = None;
    });
    sys.modify_combat_state(ship_id, |cs| cs.is_cloaked = true).await.unwrap();
    for (svc, meth, args) in [
        ("dogmaIM", "LinkWeapons", vec![PyRep::Integer(ship_id), PyRep::Integer(gun3), PyRep::Integer(gun4)]),
        ("dogmaIM", "PeelAndLink", vec![PyRep::Integer(ship_id), PyRep::Integer(gun3), PyRep::Integer(gun2)]),
        ("dogmaIM", "MergeModuleGroups", vec![PyRep::Integer(ship_id), PyRep::Integer(gun1), PyRep::Integer(gun3)]),
        ("dogmaIM", "LinkAllWeapons", vec![PyRep::Integer(ship_id)]),
    ] {
        let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
        let res = handle_dogma_im(&ctx, &mut sess, None, svc, meth, &args, &kw)
            .await
            .expect("handled");
        assert_all_rejected(&format!("cloaked {svc}.{meth}"), "CannotLinkWeaponsWhileCloaked", res, before);
    }
    sys.modify_combat_state(ship_id, |cs| {
        cs.is_cloaked = false;
        cs.gate_cloak_remaining_ticks = 15;
    })
    .await
    .unwrap();
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    let gate_res = handle_dogma_im(
        &ctx,
        &mut sess,
        None,
        "charFittingMgr",
        "MergeModuleGroups",
        &[PyRep::Integer(ship_id), PyRep::Integer(gun1), PyRep::Integer(gun3)],
        &kw,
    )
    .await
    .expect("handled");
    assert_all_rejected("gate_cloaked MergeModuleGroups", "CannotLinkWeaponsWhileCloaked", gate_res, before);

    // Uncloak; test non-active owned ship rejection.
    sys.modify_combat_state(ship_id, |cs| cs.gate_cloak_remaining_ticks = 0).await.unwrap();
    for (svc, meth, args) in [
        ("dogmaIM", "LinkWeapons", vec![PyRep::Integer(other_ship_id), PyRep::Integer(other_gun1), PyRep::Integer(other_gun2)]),
        ("charFittingMgr", "PeelAndLink", vec![PyRep::Integer(other_ship_id), PyRep::Integer(other_gun1), PyRep::Integer(other_gun2)]),
        ("dogmaIM", "MergeModuleGroups", vec![PyRep::Integer(other_ship_id), PyRep::Integer(other_gun1), PyRep::Integer(other_gun2)]),
        ("dogmaIM", "LinkAllWeapons", vec![PyRep::Integer(other_ship_id)]),
    ] {
        let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
        let res = handle_dogma_im(&ctx, &mut sess, None, svc, meth, &args, &kw)
            .await
            .expect("handled");
        assert_all_rejected(&format!("non_active_ship {svc}.{meth}"), "ShipNotActive", res, before);
    }

    // Non-weapon, cargo-flag, mixed weapon types, and self-link all fail InvalidWeaponGroup atomically.
    for (label, svc, meth, pair) in [
        ("non_weapon_salvager", "dogmaIM", "LinkWeapons", [gun1, salvager]),
        ("cargo_gun", "charFittingMgr", "PeelAndLink", [cargo_gun, gun1]),
        ("mixed_weapon_types", "dogmaIM", "MergeModuleGroups", [gun1, diff_gun]),
        ("self_link", "charFittingMgr", "LinkWeapons", [gun1, gun1]),
    ] {
        let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
        let res = handle_dogma_im(
            &ctx,
            &mut sess,
            None,
            svc,
            meth,
            &[PyRep::Integer(ship_id), PyRep::Integer(pair[0]), PyRep::Integer(pair[1])],
            &kw,
        )
        .await
        .expect("handled");
        assert_all_rejected(label, "InvalidWeaponGroup", res, before);
    }

    // Valid undocked uncloaked active ship operations mutate banks accurately.
    ctx.db.with_mut_snapshot(|snap| {
        snap.weapon_banks.remove(&ship_id);
    });
    let linked = handle_dogma_im(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "LinkWeapons",
        &[PyRep::Integer(ship_id), PyRep::Integer(gun1), PyRep::Integer(gun2)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("valid LinkWeapons");
    assert!(matches!(linked, PyRep::Dict(ref d) if d.len() == 1));
    handle_dogma_im(
        &ctx,
        &mut sess,
        None,
        "charFittingMgr",
        "PeelAndLink",
        &[PyRep::Integer(ship_id), PyRep::Integer(gun3), PyRep::Integer(gun2)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("valid PeelAndLink");
    handle_dogma_im(
        &ctx,
        &mut sess,
        None,
        "charFittingMgr",
        "LinkWeapons",
        &[PyRep::Integer(ship_id), PyRep::Integer(gun4), PyRep::Integer(gun5)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("valid second LinkWeapons");
    handle_dogma_im(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "MergeModuleGroups",
        &[PyRep::Integer(ship_id), PyRep::Integer(gun1), PyRep::Integer(gun4)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("valid MergeModuleGroups");

    ctx.db.with_snapshot(|snap| {
        let banks = snap.weapon_banks.get(&ship_id).expect("merged weapon banks");
        assert_eq!(banks.len(), 1);
        assert_eq!(banks.get(&gun1), Some(&vec![gun1, gun2, gun3, gun4, gun5]));
    });
}

#[tokio::test]
async fn r2_p1_2_disjoint_weapon_banks_and_offline_weapon_rejection() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_399_701i64;
    let sys_id = 30_000_142u32;
    let ship_id = 96_399_710i64;
    let g1 = 96_399_721i64;
    let g2 = 96_399_722i64;
    let g3 = 96_399_723i64;
    let g4 = 96_399_724i64;
    let g5 = 96_399_725i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "user_96399701".into(),
            AccountRecord {
                user_id: 96_399_701,
                username: "user_96399701".into(),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 96_399_701;
        ch.name = "DisjointBankPilot".into();
        ch.corporation_id = 1_000_009;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for (gid, flag) in [
            (g1, InventoryFlag::HiSlot0),
            (g2, InventoryFlag::HiSlot1),
            (g3, InventoryFlag::HiSlot2),
            (g4, InventoryFlag::HiSlot3),
            (g5, InventoryFlag::HiSlot4),
        ] {
            snap.items.insert(
                gid,
                InventoryItemRecord {
                    item_id: gid,
                    type_id: 484,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(gid, true);
        }
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
                corp_id: 1_000_009,
                char_id: Some(char_id),
                ..Default::default()
            },
        )
        .await;

    let mut sess = SessionState::new(96_399_701);
    sess.user_id = Some(96_399_701);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    ctx.db
        .acquire_online_lock(96_399_701, 96_399_701, Some(char_id), true, ctx.now_unix_secs())
        .expect("online lock");
    let kw = HashMap::new();
    let svc = "dogmaIM";

    // 1. LinkWeapons(g1, g2) then LinkWeapons(g2, g1) must preserve a single disjoint bank {g1: [g1, g2]}.
    handle_dogma_im(&ctx, &mut sess, None, svc, "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(g1), PyRep::Integer(g2)], &kw)
        .await
        .expect("handled")
        .expect("LinkWeapons(g1, g2)");
    handle_dogma_im(&ctx, &mut sess, None, svc, "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(g2), PyRep::Integer(g1)], &kw)
        .await
        .expect("handled")
        .expect("LinkWeapons(g2, g1)");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.weapon_banks.get(&ship_id),
            Some(&HashMap::from([(g1, vec![g1, g2])]))
        );
    });

    // 2. PeelAndLink(g2, g3) peels g2 out of g1's bank, dissolves g1's 1-member remnant, and emits OnWeaponGroupDestroyed(g1).
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_dogma_im(&ctx, &mut sess, None, svc, "PeelAndLink", &[PyRep::Integer(ship_id), PyRep::Integer(g2), PyRep::Integer(g3)], &kw)
        .await
        .expect("handled")
        .expect("PeelAndLink(g2, g3)");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.weapon_banks.get(&ship_id),
            Some(&HashMap::from([(g3, vec![g3, g2])]))
        );
    });
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnWeaponGroupDestroyed" && n.args == vec![PyRep::Integer(ship_id), PyRep::Integer(g1)]),
        "dissolved bank master g1 must emit OnWeaponGroupDestroyed, got {notifs:?}"
    );

    // 3. Build 3-gun bank {g1: [g1, g4, g5]}, peel master g1 into g3's bank -> remnant re-keys to g4 & emits OnWeaponGroupDestroyed(g1).
    handle_dogma_im(&ctx, &mut sess, None, svc, "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(g1), PyRep::Integer(g4)], &kw)
        .await
        .expect("handled")
        .expect("LinkWeapons(g1, g4)");
    handle_dogma_im(&ctx, &mut sess, None, svc, "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(g1), PyRep::Integer(g5)], &kw)
        .await
        .expect("handled")
        .expect("LinkWeapons(g1, g5)");
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_dogma_im(&ctx, &mut sess, None, svc, "PeelAndLink", &[PyRep::Integer(ship_id), PyRep::Integer(g1), PyRep::Integer(g3)], &kw)
        .await
        .expect("handled")
        .expect("PeelAndLink(g1, g3)");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.weapon_banks.get(&ship_id),
            Some(&HashMap::from([(g3, vec![g3, g2, g1]), (g4, vec![g4, g5])]))
        );
    });
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnWeaponGroupDestroyed" && n.args == vec![PyRep::Integer(ship_id), PyRep::Integer(g1)]),
        "re-keyed master g1 must emit OnWeaponGroupDestroyed, got {notifs:?}"
    );

    // 4. MergeModuleGroups(g5, g2) resolves slave args to masters g4 and g3, merges disjointly, and emits OnWeaponGroupDestroyed(g3).
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_dogma_im(&ctx, &mut sess, None, svc, "MergeModuleGroups", &[PyRep::Integer(ship_id), PyRep::Integer(g5), PyRep::Integer(g2)], &kw)
        .await
        .expect("handled")
        .expect("MergeModuleGroups(g5, g2)");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.weapon_banks.get(&ship_id),
            Some(&HashMap::from([(g4, vec![g4, g5, g3, g2, g1])]))
        );
    });
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnWeaponGroupDestroyed" && n.args == vec![PyRep::Integer(ship_id), PyRep::Integer(g3)]),
        "merged bank master g3 must emit OnWeaponGroupDestroyed, got {notifs:?}"
    );

    // 5. Offline high-slot weapon is rejected across LinkWeapons, PeelAndLink, MergeModuleGroups, and LinkAllWeapons.
    ctx.db.with_mut_snapshot(|snap| {
        snap.weapon_banks.remove(&ship_id);
        snap.module_online_states.insert(g2, false);
    });
    let expected_err = "InvalidWeaponGroup";
    let before = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    for (meth, args) in [
        ("LinkWeapons", vec![PyRep::Integer(ship_id), PyRep::Integer(g1), PyRep::Integer(g2)]),
        ("PeelAndLink", vec![PyRep::Integer(ship_id), PyRep::Integer(g2), PyRep::Integer(g1)]),
        ("MergeModuleGroups", vec![PyRep::Integer(ship_id), PyRep::Integer(g1), PyRep::Integer(g2)]),
        ("LinkAllWeapons", vec![PyRep::Integer(ship_id)]),
    ] {
        // Need only 1 online gun if testing LinkAllWeapons rejection with 1 online + 1 offline,
        // so test LinkAllWeapons specifically below while verifying pair calls fail here.
        if meth == "LinkAllWeapons" {
            continue;
        }
        let err = handle_dogma_im(&ctx, &mut sess, None, svc, meth, &args, &kw)
            .await
            .expect("handled")
            .unwrap_err();
        assert_eq!(err.code, expected_err, "{meth} must reject offline module");
        assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before);
    }

    ctx.db.with_mut_snapshot(|snap| {
        for off_id in [g3, g4, g5] {
            snap.module_online_states.insert(off_id, false);
        }
    });
    let before_all = (ctx.db.state_epoch(), ctx.db.tx_commit_count());
    let link_all_err = handle_dogma_im(&ctx, &mut sess, None, svc, "LinkAllWeapons", &[PyRep::Integer(ship_id)], &kw)
        .await
        .expect("handled")
        .unwrap_err();
    assert_eq!(link_all_err.code, expected_err);
    assert_eq!((ctx.db.state_epoch(), ctx.db.tx_commit_count()), before_all);

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(g3, true);
    });
    handle_dogma_im(&ctx, &mut sess, None, svc, "LinkAllWeapons", &[PyRep::Integer(ship_id)], &kw)
        .await
        .expect("handled")
        .expect("LinkAllWeapons links two online weapons while excluding offline ones");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.weapon_banks.get(&ship_id),
            Some(&HashMap::from([(g1, vec![g1, g3])]))
        );
    });
}

#[tokio::test]
async fn r2_p1_3_structure_gunner_defense_acl_and_starbase_operator_can_inspect_combat_slots() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let corp_id = 98_201_001i64;
    let allied_corp_id = 98_201_002i64;
    let sys_id = 30_000_142u32;
    let structure_id = 10_000_982_101i64;

    let ceo_id = 982_201i64;
    let starbase_op_id = 982_202i64;
    let allied_gunner_id = 982_203i64;
    let outsider_id = 982_204i64;

    let hi_launcher_id = 982_220_001i64;
    let hi_charge_id = 982_220_002i64;
    let ammo_hold_item_id = 982_220_003i64;
    let fighter_tube_item_id = 982_220_004i64;
    let service_mod_id = 982_220_005i64;
    let rig_mod_id = 982_220_006i64;
    let core_item_id = 982_220_007i64;
    let sag1_item_id = 982_220_008i64;
    let deliveries_item_id = 982_220_009i64;
    let fuel_item_id = 982_220_010i64;
    let charge_tid = 24_533u32;
    let launcher_tid = 35_894u32;

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.values().next().cloned().expect("seed char");
        let mut corp = eve_db::CorporationRecord::new(corp_id, "Citadel Defense", "CDEF", ceo_id);
        corp.members.insert(ceo_id);
        corp.members.insert(starbase_op_id);
        corp.member_roles
            .insert(starbase_op_id, eve_db::corp_roles::ROLE_STARBASE_OPERATOR);
        snap.corporations.insert(corp_id, corp);

        let mut allied_corp =
            eve_db::CorporationRecord::new(allied_corp_id, "Allied Gunners", "ALLY", allied_gunner_id);
        allied_corp.members.insert(allied_gunner_id);
        allied_corp.members.insert(outsider_id);
        snap.corporations.insert(allied_corp_id, allied_corp);

        for (cid, cid_corp) in [
            (ceo_id, corp_id),
            (starbase_op_id, corp_id),
            (allied_gunner_id, allied_corp_id),
            (outsider_id, allied_corp_id),
        ] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.corporation_id = cid_corp;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = Some(structure_id);
            ch.active_ship_id = 0;
            snap.characters.insert(cid, ch);
        }

        snap.structures.insert(
            structure_id,
            eve_db::UpwellStructureRecord {
                structure_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Gunner Authz Fortizar".into(),
                position: [15_000.0, 0.0, 0.0],
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

        let mut prof = crate::services::station_structure::structure_upwell_ops::StructureAccessProfile::default();
        prof.profile_id = 982_101;
        prof.owner_corp_id = corp_id;
        prof.defense_allowed_characters.insert(allied_gunner_id);
        let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.profiles_by_id.insert(prof.profile_id, prof.clone());
        ext.structure_profile_id.insert(structure_id, prof.profile_id);
        ext.access_profiles.insert(structure_id, prof);
        crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);

        for (iid, tid, flag, qty) in [
            (hi_launcher_id, launcher_tid, InventoryFlag::HiSlot0, 1),
            (hi_charge_id, charge_tid, InventoryFlag::HiSlot0, 20),
            (ammo_hold_item_id, charge_tid, InventoryFlag::SpecializedAmmoHold, 100),
            (fighter_tube_item_id, 23_057, InventoryFlag::FighterBay, 6),
            (service_mod_id, 35_879, InventoryFlag::ServiceSlot0, 1),
            (rig_mod_id, 37_148, InventoryFlag::RigSlot0, 1),
            (core_item_id, 56_201, InventoryFlag::QuantumCoreRoom, 1),
            (sag1_item_id, 34, InventoryFlag::CorpSAG1, 50),
            (deliveries_item_id, 34, InventoryFlag::Deliveries, 50),
            (fuel_item_id, 4247, InventoryFlag::StructureFuel, 200),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: corp_id,
                    location_id: structure_id,
                    flag,
                    quantity: qty,
                    singleton: qty == 1,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let mk_sess = |cid: i64, cid_corp: i64| {
        let mut s = SessionState::new(cid as u64);
        s.user_id = Some(cid as u64);
        s.character_id = Some(cid);
        s.corporation_id = Some(cid_corp);
        s.solar_system_id = Some(sys_id);
        s.structure_id = Some(structure_id);
        s
    };

    let mut allied_sess = mk_sess(allied_gunner_id, allied_corp_id);
    let mut op_sess = mk_sess(starbase_op_id, corp_id);
    let mut outsider_sess = mk_sess(outsider_id, allied_corp_id);
    let kw = HashMap::new();
    let svc = "dogmaIM";
    let expected_denied = "AccessDenied";

    let hi_subloc = PyRep::Tuple(vec![
        PyRep::Integer(structure_id),
        PyRep::Integer(i64::from(InventoryFlag::HiSlot0 as u16)),
        PyRep::Integer(i64::from(charge_tid)),
    ]);
    let svc_subloc = PyRep::Tuple(vec![
        PyRep::Integer(structure_id),
        PyRep::Integer(i64::from(InventoryFlag::ServiceSlot0 as u16)),
        PyRep::Integer(35_879),
    ]);
    let rig_subloc = PyRep::Tuple(vec![
        PyRep::Integer(structure_id),
        PyRep::Integer(i64::from(InventoryFlag::RigSlot0 as u16)),
        PyRep::Integer(37_148),
    ]);
    let core_subloc = PyRep::Tuple(vec![
        PyRep::Integer(structure_id),
        PyRep::Integer(i64::from(InventoryFlag::QuantumCoreRoom as u16)),
        PyRep::Integer(56_201),
    ]);

    // Allied Defense-ACL gunner takes control and inspects HiSlot0 sublocation, fitted weapons, ammo hold, and tubes.
    crate::services::structure_control_ops::handle_take_control(
        &ctx,
        &mut allied_sess,
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect("allied Defense-ACL gunner TakeControl");

    for arg in [
        hi_subloc.clone(),
        PyRep::Integer(hi_launcher_id),
        PyRep::Integer(ammo_hold_item_id),
        PyRep::Integer(fighter_tube_item_id),
    ] {
        handle_dogma_im(&ctx, &mut allied_sess, None, svc, "ItemGetInfo", &[arg], &kw)
            .await
            .expect("handled")
            .expect("active Defense-ACL gunner inspects combat slots");
    }
    for forbidden in [
        svc_subloc.clone(),
        rig_subloc.clone(),
        core_subloc.clone(),
        PyRep::Integer(service_mod_id),
        PyRep::Integer(rig_mod_id),
        PyRep::Integer(core_item_id),
        PyRep::Integer(sag1_item_id),
        PyRep::Integer(deliveries_item_id),
        PyRep::Integer(fuel_item_id),
    ] {
        let err = handle_dogma_im(&ctx, &mut allied_sess, None, svc, "ItemGetInfo", &[forbidden], &kw)
            .await
            .expect("handled")
            .unwrap_err();
        assert_eq!(err.code, expected_denied);
    }

    // Release control, take control as ROLE_STARBASE_OPERATOR owning-corp gunner, verify combat slots OK
    // while non-gunnery corp flags (ServiceSlot0, RigSlot0, QuantumCoreRoom, CorpSAG1, Deliveries) stay denied.
    crate::services::structure_control_ops::handle_release_control(
        &ctx,
        &mut allied_sess,
        &[PyRep::Integer(structure_id)],
    )
    .expect("allied ReleaseControl");
    crate::services::structure_control_ops::handle_take_control(
        &ctx,
        &mut op_sess,
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect("ROLE_STARBASE_OPERATOR TakeControl");

    for arg in [
        hi_subloc.clone(),
        PyRep::Integer(hi_launcher_id),
        PyRep::Integer(ammo_hold_item_id),
        PyRep::Integer(fighter_tube_item_id),
    ] {
        handle_dogma_im(&ctx, &mut op_sess, None, svc, "ItemGetInfo", &[arg], &kw)
            .await
            .expect("handled")
            .expect("ROLE_STARBASE_OPERATOR gunner inspects combat slots");
    }
    for forbidden in [
        svc_subloc,
        rig_subloc,
        core_subloc,
        PyRep::Integer(service_mod_id),
        PyRep::Integer(rig_mod_id),
        PyRep::Integer(core_item_id),
        PyRep::Integer(sag1_item_id),
        PyRep::Integer(deliveries_item_id),
    ] {
        let err = handle_dogma_im(&ctx, &mut op_sess, None, svc, "ItemGetInfo", &[forbidden], &kw)
            .await
            .expect("handled")
            .unwrap_err();
        assert_eq!(err.code, expected_denied);
    }

    // Allied outsider without Defense ACL remains denied on HiSlot0 sublocation and fitted weapon.
    for arg in [hi_subloc, PyRep::Integer(hi_launcher_id)] {
        let err = handle_dogma_im(&ctx, &mut outsider_sess, None, svc, "ItemGetInfo", &[arg], &kw)
            .await
            .expect("handled")
            .unwrap_err();
        assert_eq!(err.code, expected_denied);
    }
}

#[tokio::test]
async fn r2_p1_3_structure_sublocation_authz_allows_gunners_and_denies_unauthorized() {
    let sde = shared_sde();
    let ctx = ServerContext::with_sde_and_repository(
        Arc::clone(&sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let sys_id = 30_000_142u32;
    let corp_id = 98_200_001i64;
    let other_corp_id = 98_200_002i64;
    let ceo_id = 982_101i64;
    let station_mgr_id = 982_102i64;
    let equip_mgr_id = 982_103i64;
    let director_id = 982_104i64;
    let fuel_tech_id = 982_105i64;
    let roleless_id = 982_106i64;
    let outsider_id = 982_107i64;

    let structure_only_id = 1_000_000_982_001i64;
    let structure_with_item_id = 1_000_000_982_002i64;
    let corp_db_charge_id = 982_301i64;
    let owner_ship_id = 982_401i64;
    let owner_container_id = 982_402i64;
    let station_id = 60_003_760i64;
    let charge_tid = 183u32;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord::new(corp_id, "Citadel Ops", "COPS", ceo_id);
        for cid in [ceo_id, station_mgr_id, equip_mgr_id, director_id, fuel_tech_id, roleless_id] {
            corp.members.insert(cid);
        }
        corp.member_roles
            .insert(station_mgr_id, eve_db::corp_roles::ROLE_STATION_MANAGER);
        corp.member_roles
            .insert(equip_mgr_id, eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG);
        corp.member_roles
            .insert(director_id, eve_db::corp_roles::ROLE_DIRECTOR);
        corp.member_roles
            .insert(fuel_tech_id, eve_db::corp_roles::ROLE_STARBASE_CARETAKER);
        snap.corporations.insert(corp_id, corp);

        let mut other_corp =
            eve_db::CorporationRecord::new(other_corp_id, "Outsiders", "OUTS", outsider_id);
        other_corp.members.insert(outsider_id);
        snap.corporations.insert(other_corp_id, other_corp);

        for (cid, cid_corp) in [
            (ceo_id, corp_id),
            (station_mgr_id, corp_id),
            (equip_mgr_id, corp_id),
            (director_id, corp_id),
            (fuel_tech_id, corp_id),
            (roleless_id, corp_id),
            (outsider_id, other_corp_id),
        ] {
            snap.characters.insert(
                cid,
                eve_db::CharacterRecord {
                    character_id: cid,
                    user_id: cid as u64,
                    name: format!("StructMember {cid}"),
                    corporation_id: cid_corp,
                    solar_system_id: sys_id,
                    structure_id: Some(structure_only_id),
                    active_ship_id: owner_ship_id,
                    ..Default::default()
                },
            );
        }

        for sid in [structure_only_id, structure_with_item_id] {
            snap.structures.insert(
                sid,
                eve_db::UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_832,
                    owner_corp_id: corp_id,
                    solar_system_id: sys_id,
                    name: format!("Astrahus {sid}"),
                    position: [0.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    ..Default::default()
                },
            );
        }
        // Second structure is ALSO present in `snap.items` to verify root-loc stops at structure_id.
        snap.items.insert(
            structure_with_item_id,
            InventoryItemRecord {
                item_id: structure_with_item_id,
                type_id: 35_832,
                owner_id: corp_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // DB-backed corp charge inside structure's HiSlot0 on structure_with_item_id.
        snap.items.insert(
            corp_db_charge_id,
            InventoryItemRecord {
                item_id: corp_db_charge_id,
                type_id: charge_tid,
                owner_id: corp_id,
                location_id: structure_with_item_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 20,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Ship & container items to verify ship/station sublocations continue working.
        snap.items.insert(
            owner_ship_id,
            InventoryItemRecord {
                item_id: owner_ship_id,
                type_id: 587,
                owner_id: equip_mgr_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            owner_container_id,
            InventoryItemRecord {
                item_id: owner_container_id,
                type_id: 17_366,
                owner_id: equip_mgr_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mk_sess = |cid: i64, role: u64| {
        let mut s = SessionState::new(cid as u64);
        s.user_id = Some(cid as u64);
        s.character_id = Some(cid);
        s.corporation_id = Some(if cid == outsider_id { other_corp_id } else { corp_id });
        s.solar_system_id = Some(sys_id);
        s.structure_id = Some(structure_only_id);
        s.role = role;
        s
    };
    let mut ceo_sess = mk_sess(ceo_id, 0);
    let mut station_mgr_sess = mk_sess(station_mgr_id, 0);
    let mut equip_mgr_sess = mk_sess(equip_mgr_id, 0);
    let mut director_sess = mk_sess(director_id, 0);
    let mut fuel_tech_sess = mk_sess(fuel_tech_id, 0);
    let mut roleless_sess = mk_sess(roleless_id, 0);
    let mut outsider_sess = mk_sess(outsider_id, 0);
    let mut gm_sess = mk_sess(outsider_id, eve_machonet::service_roles::ROLE_GM_MASK);

    let kw = HashMap::new();
    let subloc_arg = |loc_id: i64, flag: InventoryFlag| {
        PyRep::Tuple(vec![
            PyRep::Integer(loc_id),
            PyRep::Integer(i64::from(flag as u16)),
            PyRep::Integer(i64::from(charge_tid)),
        ])
    };

    for sid in [structure_only_id, structure_with_item_id] {
        for flag in [
            InventoryFlag::HiSlot0,
            InventoryFlag::ServiceSlot0,
            InventoryFlag::SpecializedAmmoHold,
            InventoryFlag::QuantumCoreRoom,
        ] {
            let arg = subloc_arg(sid, flag);
            for (label, s) in [
                ("ceo", &mut ceo_sess),
                ("station_mgr", &mut station_mgr_sess),
                ("equip_mgr", &mut equip_mgr_sess),
                ("director", &mut director_sess),
                ("gm", &mut gm_sess),
            ] {
                let info = handle_dogma_im(&ctx, s, None, "dogmaIM", "ItemGetInfo", std::slice::from_ref(&arg), &kw)
                    .await
                    .expect("handled")
                    .unwrap_or_else(|e| panic!("{label} must authorize ({sid}, {flag:?}): {e:?}"));
                assert_eq!(info.dict_get("typeID"), Some(&PyRep::Integer(i64::from(charge_tid))));
                assert_eq!(info.dict_get("invItem"), Some(&PyRep::None));
                assert_eq!(info.dict_get("ownerID"), Some(&PyRep::Integer(corp_id)));
                assert_eq!(info.dict_get("locationID"), Some(&PyRep::Integer(sid)));
            }
            for (label, s) in [
                ("fuel_tech", &mut fuel_tech_sess),
                ("roleless", &mut roleless_sess),
                ("outsider", &mut outsider_sess),
            ] {
                let err = handle_dogma_im(&ctx, s, None, "dogmaIM", "ItemGetInfo", std::slice::from_ref(&arg), &kw)
                    .await
                    .expect("handled")
                    .unwrap_err();
                assert_eq!(err.code, "AccessDenied", "{label} must be denied ({sid}, {flag:?})");
            }
        }

        // Fuel bay authorizes fuel_tech & station_mgr/director/ceo, rejects equip_mgr.
        let fuel_arg = subloc_arg(sid, InventoryFlag::StructureFuel);
        assert!(
            handle_dogma_im(&ctx, &mut fuel_tech_sess, None, "dogmaIM", "ItemGetInfo", std::slice::from_ref(&fuel_arg), &kw)
                .await
                .expect("handled")
                .is_ok()
        );
        assert_eq!(
            handle_dogma_im(&ctx, &mut equip_mgr_sess, None, "dogmaIM", "ItemGetInfo", &[fuel_arg], &kw)
                .await
                .expect("handled")
                .unwrap_err()
                .code,
            "AccessDenied"
        );

        // CorpSAG2 authorizes director, rejects equip_mgr.
        let sag_arg = subloc_arg(sid, InventoryFlag::CorpSAG2);
        assert!(
            handle_dogma_im(&ctx, &mut director_sess, None, "dogmaIM", "ItemGetInfo", std::slice::from_ref(&sag_arg), &kw)
                .await
                .expect("handled")
                .is_ok()
        );
        assert_eq!(
            handle_dogma_im(&ctx, &mut equip_mgr_sess, None, "dogmaIM", "ItemGetInfo", &[sag_arg], &kw)
                .await
                .expect("handled")
                .unwrap_err()
                .code,
            "AccessDenied"
        );
    }

    // Ship and station container sublocations remain intact.
    for (loc, flag) in [
        (owner_ship_id, InventoryFlag::HiSlot0),
        (owner_container_id, InventoryFlag::Cargo),
    ] {
        let arg = subloc_arg(loc, flag);
        assert!(
            handle_dogma_im(&ctx, &mut equip_mgr_sess, None, "dogmaIM", "ItemGetInfo", std::slice::from_ref(&arg), &kw)
                .await
                .expect("handled")
                .is_ok()
        );
        assert_eq!(
            handle_dogma_im(&ctx, &mut outsider_sess, None, "dogmaIM", "ItemGetInfo", &[arg], &kw)
                .await
                .expect("handled")
                .unwrap_err()
                .code,
            "AccessDenied"
        );
    }
}

#[tokio::test]
async fn hf1476_structure_combat_access_denies_stale_active_pilot_and_invalid_owner_corps() {
    use crate::services::station_structure::structure_upwell_ops::{
        load_ext_state, save_ext_state, AccessGroupRecord, StructureAccessProfile,
    };
    use eve_db::corp_roles::{ROLE_ACCOUNTANT, ROLE_STARBASE_OPERATOR};
    use eve_db::{CharacterRecord, CorporationRecord, UpwellStructureRecord};
    use std::collections::HashSet;

    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let sys_id = 30_000_142u32;
    let corp_id = 98_476_101i64;
    let empty_role_corp_id = 98_476_102i64;
    let other_corp_id = 98_476_103i64;
    let unregistered_corp_id = 98_476_199i64;
    let npc_corp_id = 1_000_009i64;

    let char_gunner = 96_476_001i64;
    let char_role_op = 96_476_002i64;
    let char_empty_mem = 96_476_003i64;
    let char_outsider = 96_476_004i64;
    let char_ceo = 96_476_005i64;

    let sid_valid = 1_096_476_000_001i64;
    let sid_uncored_fort = 1_096_476_000_002i64;
    let sid_anchoring = 1_096_476_000_003i64;
    let sid_anchor_vuln = 1_096_476_000_004i64;
    let sid_unanchored = 1_096_476_000_005i64;
    let sid_zero_corp = 1_096_476_000_006i64;
    let sid_npc_corp = 1_096_476_000_007i64;
    let sid_unreg_corp = 1_096_476_000_008i64;
    let sid_empty_corp = 1_096_476_000_009i64;

    let mod_valid = 96_476_101i64;
    let mod_uncored_fort = 96_476_102i64;
    let mod_anchoring = 96_476_103i64;
    let mod_anchor_vuln = 96_476_104i64;
    let mod_unanchored = 96_476_105i64;
    let mod_zero_corp = 96_476_106i64;
    let mod_npc_corp = 96_476_107i64;
    let mod_unreg_corp = 96_476_108i64;
    let mod_empty_corp = 96_476_109i64;

    let ship_id = 96_476_500i64;
    let cmd_a = 96_476_501i64;
    let cmd_b = 96_476_502i64;
    let strip_a = 96_476_503i64;
    let strip_b = 96_476_504i64;

    let now = ctx.now_unix();
    let group_gunner = 76_001i64;
    let group_outsider = 76_002i64;
    let prof_initial_id = 76_101i64;
    let prof_revoked_id = 76_102i64;
    let prof_outsider_id = 76_103i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.values().next().cloned().expect("seed char");
        for (cid, cid_corp) in [
            (char_gunner, corp_id),
            (char_role_op, corp_id),
            (char_ceo, corp_id),
            (char_empty_mem, empty_role_corp_id),
            (char_outsider, npc_corp_id),
        ] {
            snap.characters.insert(
                cid,
                CharacterRecord {
                    character_id: cid,
                    corporation_id: cid_corp,
                    solar_system_id: sys_id,
                    station_id: None,
                    structure_id: None,
                    active_ship_id: if cid == char_role_op { ship_id } else { 0 },
                    ..base_char.clone()
                },
            );
        }

        let mut corp = CorporationRecord::new(corp_id, "Combat Corp 1476", "C76", char_ceo);
        corp.members.insert(char_gunner);
        corp.members.insert(char_role_op);
        corp.members.insert(char_ceo);
        corp.member_roles.insert(char_role_op, ROLE_STARBASE_OPERATOR);
        corp.member_roles.insert(char_gunner, ROLE_ACCOUNTANT);
        snap.corporations.insert(corp_id, corp);

        let mut empty_corp = CorporationRecord::new(empty_role_corp_id, "Empty Role Corp", "ERC", 0);
        empty_corp.members.insert(char_empty_mem);
        snap.corporations.insert(empty_role_corp_id, empty_corp);

        let specs = [
            (sid_valid, mod_valid, 35_832u32, corp_id, "ShieldVulnerable", true),
            (sid_uncored_fort, mod_uncored_fort, 35_833, corp_id, "ShieldVulnerable", false),
            (sid_anchoring, mod_anchoring, 35_832, corp_id, "Anchoring", true),
            (sid_anchor_vuln, mod_anchor_vuln, 35_832, corp_id, "AnchorVulnerable", true),
            (sid_unanchored, mod_unanchored, 35_832, corp_id, "Unanchored", true),
            (sid_zero_corp, mod_zero_corp, 35_832, 0, "ShieldVulnerable", true),
            (sid_npc_corp, mod_npc_corp, 35_832, npc_corp_id, "ShieldVulnerable", true),
            (sid_unreg_corp, mod_unreg_corp, 35_832, unregistered_corp_id, "ShieldVulnerable", true),
            (sid_empty_corp, mod_empty_corp, 35_832, empty_role_corp_id, "ShieldVulnerable", true),
        ];
        for (sid, mod_id, tid, owner_corp_id, state, cored) in specs {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: tid,
                    owner_corp_id,
                    solar_system_id: sys_id,
                    state: state.into(),
                    quantum_core_installed: cored,
                    reinforced_until_unix: if state == "ShieldVulnerable" { 0 } else { now + 86_400 },
                    ..Default::default()
                },
            );
            snap.items.insert(
                mod_id,
                InventoryItemRecord {
                    item_id: mod_id,
                    type_id: 35_921,
                    owner_id: owner_corp_id.max(0),
                    location_id: sid,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }

        let mut ext = load_ext_state(snap);
        ext.owner_groups.insert(
            group_gunner,
            AccessGroupRecord {
                group_id: group_gunner,
                creator_id: char_role_op,
                owner_corp_id: corp_id,
                name: "Gunners".into(),
                description: String::new(),
                members: HashMap::from([(char_gunner, 1)]),
            },
        );
        ext.owner_groups.insert(
            group_outsider,
            AccessGroupRecord {
                group_id: group_outsider,
                creator_id: char_outsider,
                owner_corp_id: other_corp_id,
                name: "Fake".into(),
                description: String::new(),
                members: HashMap::from([(char_outsider, 1)]),
            },
        );

        let prof_initial = StructureAccessProfile {
            profile_id: prof_initial_id,
            owner_corp_id: corp_id,
            defense_group_ids: HashSet::from([group_gunner]),
            ..Default::default()
        };
        let prof_revoked = StructureAccessProfile {
            profile_id: prof_revoked_id,
            owner_corp_id: corp_id,
            ..Default::default()
        };
        let prof_outsider = StructureAccessProfile {
            profile_id: prof_outsider_id,
            owner_corp_id: corp_id,
            defense_group_ids: HashSet::from([group_outsider]),
            ..Default::default()
        };
        ext.access_profiles.insert(sid_valid, prof_initial);
        ext.profiles_by_id.insert(prof_revoked_id, prof_revoked);
        ext.pending_profile_applications
            .insert(sid_valid, (prof_revoked_id, now + 60));
        ext.active_pilot_by_structure.insert(sid_valid, char_gunner);

        for invalid_sid in [
            sid_uncored_fort,
            sid_anchoring,
            sid_anchor_vuln,
            sid_unanchored,
            sid_zero_corp,
            sid_npc_corp,
            sid_unreg_corp,
        ] {
            ext.access_profiles.insert(invalid_sid, prof_outsider.clone());
            ext.active_pilot_by_structure.insert(invalid_sid, char_outsider);
        }
        ext.active_pilot_by_structure
            .insert(sid_empty_corp, char_empty_mem);
        save_ext_state(snap, &ext);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_role_op,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (mod_id, tid, flag) in [
            (cmd_a, 42_526u32, InventoryFlag::HiSlot0),
            (cmd_b, 42_526, InventoryFlag::HiSlot1),
            (strip_a, 17_912, InventoryFlag::HiSlot2),
            (strip_b, 17_912, InventoryFlag::HiSlot3),
        ] {
            snap.items.insert(
                mod_id,
                InventoryItemRecord {
                    item_id: mod_id,
                    type_id: tid,
                    owner_id: char_role_op,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
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
                owner_id: char_role_op,
                char_id: Some(char_role_op),
                ..Default::default()
            },
        )
        .await;

    let mk_sess = |cid: i64, cid_corp: i64, sid: i64| {
        let mut s = SessionState::new(cid as u64);
        s.user_id = Some(cid as u64);
        s.character_id = Some(cid);
        s.corporation_id = Some(cid_corp);
        s.solar_system_id = Some(sys_id);
        s.structure_id = Some(sid);
        s
    };
    let kw = HashMap::new();

    // Gunner starts allowed under active prof_initial.
    let mut gunner_sess = mk_sess(char_gunner, corp_id, sid_valid);
    assert!(
        handle_dogma_im(&ctx, &mut gunner_sess, None, "dogmaIM", "ItemGetInfo", &[PyRep::Integer(mod_valid)], &kw)
            .await
            .expect("handled")
            .is_ok()
    );

    // Delayed profile transition revokes ACL; stale active_pilot_by_structure must NOT keep access alive.
    ctx.advance_virtual_clock(120);
    let err = handle_dogma_im(&ctx, &mut gunner_sess, None, "dogmaIM", "ItemGetInfo", &[PyRep::Integer(mod_valid)], &kw)
        .await
        .expect("handled")
        .unwrap_err();
    assert_eq!(err.code, "AccessDenied");

    // Operational corp operator is allowed, then denied when role or corp membership is removed.
    let mut op_sess = mk_sess(char_role_op, corp_id, sid_valid);
    assert!(
        handle_dogma_im(&ctx, &mut op_sess, None, "dogmaIM", "ItemGetInfo", &[PyRep::Integer(mod_valid)], &kw)
            .await
            .expect("handled")
            .is_ok()
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .expect("corp")
            .member_roles
            .remove(&char_role_op);
    });
    let err = handle_dogma_im(&ctx, &mut op_sess, None, "dogmaIM", "ItemGetInfo", &[PyRep::Integer(mod_valid)], &kw)
        .await
        .expect("handled")
        .unwrap_err();
    assert_eq!(err.code, "AccessDenied");

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .expect("corp")
            .member_roles
            .insert(char_role_op, ROLE_STARBASE_OPERATOR);
        snap.characters
            .get_mut(&char_role_op)
            .expect("op")
            .corporation_id = other_corp_id;
    });
    let err = handle_dogma_im(&ctx, &mut op_sess, None, "dogmaIM", "ItemGetInfo", &[PyRep::Integer(mod_valid)], &kw)
        .await
        .expect("handled")
        .unwrap_err();
    assert_eq!(err.code, "AccessDenied");

    // Non-CEO omitted from populated corp.members fails ItemGetInfo even with matching corp & stale role.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get_mut(&char_role_op)
            .expect("op")
            .corporation_id = corp_id;
        snap.corporations
            .get_mut(&corp_id)
            .expect("corp")
            .members
            .remove(&char_role_op);
    });
    let err = handle_dogma_im(&ctx, &mut op_sess, None, "dogmaIM", "ItemGetInfo", &[PyRep::Integer(mod_valid)], &kw)
        .await
        .expect("handled")
        .unwrap_err();
    assert_eq!(err.code, "AccessDenied");

    // Empty role-registry corp fails closed on combat path even for members, active pilot,
    // or when an unrelated character/group sits on the structure's defense ACL.
    let mut empty_mem_sess = mk_sess(char_empty_mem, empty_role_corp_id, sid_empty_corp);
    let err = handle_dogma_im(&ctx, &mut empty_mem_sess, None, "dogmaIM", "ItemGetInfo", &[PyRep::Integer(mod_empty_corp)], &kw)
        .await
        .expect("handled")
        .unwrap_err();
    assert_eq!(err.code, "AccessDenied");

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.access_profiles.insert(
            sid_empty_corp,
            StructureAccessProfile {
                profile_id: prof_outsider_id,
                owner_corp_id: empty_role_corp_id,
                defense_group_ids: HashSet::from([group_outsider]),
                defense_allowed_characters: HashSet::from([char_outsider]),
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
    });
    let err = handle_dogma_im(&ctx, &mut empty_mem_sess, None, "dogmaIM", "ItemGetInfo", &[PyRep::Integer(mod_empty_corp)], &kw)
        .await
        .expect("handled")
        .unwrap_err();
    assert_eq!(err.code, "AccessDenied");

    // Uncored Fortizar, non-operational states, and owner_corp_id == 0 / NPC / unregistered all fail closed.
    for (sid, mod_id) in [
        (sid_uncored_fort, mod_uncored_fort),
        (sid_anchoring, mod_anchoring),
        (sid_anchor_vuln, mod_anchor_vuln),
        (sid_unanchored, mod_unanchored),
        (sid_zero_corp, mod_zero_corp),
        (sid_npc_corp, mod_npc_corp),
        (sid_unreg_corp, mod_unreg_corp),
    ] {
        let mut s = mk_sess(char_outsider, npc_corp_id, sid);
        let err = handle_dogma_im(&ctx, &mut s, None, "dogmaIM", "ItemGetInfo", &[PyRep::Integer(mod_id)], &kw)
            .await
            .expect("handled")
            .unwrap_err();
        assert_eq!(err.code, "AccessDenied", "structure {sid} must fail closed");
    }

    // LinkWeapons rejects chargeGroup1 Command Bursts and Strip Miners.
    let mut ship_sess = SessionState::new(char_role_op as u64);
    ship_sess.user_id = Some(char_role_op as u64);
    ship_sess.character_id = Some(char_role_op);
    ship_sess.corporation_id = Some(other_corp_id);
    ship_sess.solar_system_id = Some(sys_id);
    ship_sess.ship_id = Some(ship_id);
    for (a, b) in [(cmd_a, cmd_b), (strip_a, strip_b)] {
        let err = handle_dogma_im(
            &ctx,
            &mut ship_sess,
            None,
            "dogmaIM",
            "LinkWeapons",
            &[PyRep::Integer(ship_id), PyRep::Integer(a), PyRep::Integer(b)],
            &kw,
        )
        .await
        .expect("handled")
        .unwrap_err();
        assert_eq!(err.code, "InvalidWeaponGroup");
    }
}

#[tokio::test]
async fn hf1476_sublocation_lookup_prefers_caller_owner_on_shared_locations() {
    use crate::services::station_structure::structure_upwell_ops::{
        load_ext_state, save_ext_state, StructureAccessProfile,
    };
    use eve_db::corp_roles::ROLE_DIRECTOR;
    use eve_db::{CharacterRecord, CorporationRecord, UpwellStructureRecord};
    use std::collections::HashSet;

    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let sys_id = 30_000_142u32;
    let station_id = 60_003_760i64;
    let corp_id = 98_476_201i64;
    let ally_corp_id = 98_476_202i64;
    let sid = 1_096_476_000_201i64;

    let caller_id = 96_476_201i64;
    let stranger_a = 96_476_202i64;
    let stranger_b = 96_476_203i64;
    let gunner_ally = 96_476_204i64;
    let outsider_id = 96_476_205i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.values().next().cloned().expect("seed char");
        for (cid, cid_corp) in [
            (caller_id, corp_id),
            (stranger_a, ally_corp_id),
            (stranger_b, ally_corp_id),
            (gunner_ally, ally_corp_id),
            (outsider_id, ally_corp_id),
        ] {
            snap.characters.insert(
                cid,
                CharacterRecord {
                    character_id: cid,
                    corporation_id: cid_corp,
                    ..base_char.clone()
                },
            );
        }

        let mut corp = CorporationRecord::new(corp_id, "Shared Loc Corp", "SLC", caller_id);
        corp.members.insert(caller_id);
        corp.member_roles.insert(caller_id, ROLE_DIRECTOR);
        snap.corporations.insert(corp_id, corp);

        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        let mut ext = load_ext_state(snap);
        ext.access_profiles.insert(
            sid,
            StructureAccessProfile {
                profile_id: 76_201,
                owner_corp_id: corp_id,
                defense_allowed_characters: HashSet::from([gunner_ally]),
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);

        // Shared corp station container with smaller corp/foreign stacks before caller's stack.
        let corp_box_id = 96_476_250i64;
        snap.items.insert(
            corp_box_id,
            InventoryItemRecord {
                item_id: corp_box_id,
                type_id: 17_366,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (item_id, owner_id, type_id) in [
            (96_476_300, corp_id, 183u32),
            (96_476_301, stranger_a, 183),
            (96_476_302, caller_id, 183),
            (96_476_303, stranger_b, 183),
            (96_476_304, stranger_a, 185),
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id,
                    location_id: corp_box_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 50,
                    singleton: false,
                    ..Default::default()
                },
            );
        }

        // Shared structure CorpSAG1 with smaller foreign stack before owner_corp_id stack.
        for (item_id, owner_id, type_id) in [
            (96_476_311, stranger_a, 183u32),
            (96_476_312, corp_id, 183),
            (96_476_313, stranger_a, 185),
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id,
                    location_id: sid,
                    flag: InventoryFlag::CorpSAG1,
                    quantity: 40,
                    singleton: false,
                    ..Default::default()
                },
            );
        }

        // Shared structure HiSlot0 with smaller allied-corp & unrelated stacks before structure-corp loaded charge.
        for (item_id, owner_id) in [
            (96_476_320, ally_corp_id),
            (96_476_321, stranger_a),
            (96_476_322, corp_id),
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id: 183,
                    owner_id,
                    location_id: sid,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 20,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
    });

    let mk_station_sess = |cid: i64, cid_corp: i64| {
        let mut s = SessionState::new(cid as u64);
        s.user_id = Some(cid as u64);
        s.character_id = Some(cid);
        s.corporation_id = Some(cid_corp);
        s.solar_system_id = Some(sys_id);
        s.station_id = Some(station_id);
        s
    };
    let mk_struct_sess = |cid: i64, cid_corp: i64| {
        let mut s = SessionState::new(cid as u64);
        s.user_id = Some(cid as u64);
        s.character_id = Some(cid);
        s.corporation_id = Some(cid_corp);
        s.solar_system_id = Some(sys_id);
        s.structure_id = Some(sid);
        s
    };
    let subloc = |loc: i64, flag: InventoryFlag, tid: u32| {
        PyRep::Tuple(vec![
            PyRep::Integer(loc),
            PyRep::Integer(i64::from(flag as u16)),
            PyRep::Integer(i64::from(tid)),
        ])
    };
    let kw = HashMap::new();

    let corp_box_id = 96_476_250i64;
    let mut caller_station_sess = mk_station_sess(caller_id, corp_id);
    let box_info = handle_dogma_im(
        &ctx,
        &mut caller_station_sess,
        None,
        "dogmaIM",
        "ItemGetInfo",
        &[subloc(corp_box_id, InventoryFlag::Hangar, 183)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("caller stack preferred over smaller corp and foreign stacks");
    assert!(
        format!("{box_info:?}").contains("\"ownerID\": Integer(96476201)"),
        "expected caller-owned stack (owner_rank=0), got {box_info:?}"
    );
    let err = handle_dogma_im(
        &ctx,
        &mut caller_station_sess,
        None,
        "dogmaIM",
        "ItemGetInfo",
        &[subloc(corp_box_id, InventoryFlag::Hangar, 185)],
        &kw,
    )
    .await
    .expect("handled")
    .unwrap_err();
    assert_eq!(err.code, "AccessDenied");

    let mut outsider_station_sess = mk_station_sess(outsider_id, ally_corp_id);
    let err = handle_dogma_im(
        &ctx,
        &mut outsider_station_sess,
        None,
        "dogmaIM",
        "ItemGetInfo",
        &[subloc(corp_box_id, InventoryFlag::Hangar, 183)],
        &kw,
    )
    .await
    .expect("handled")
    .unwrap_err();
    assert_eq!(err.code, "AccessDenied");

    let mut caller_struct_sess = mk_struct_sess(caller_id, corp_id);
    assert!(
        handle_dogma_im(
            &ctx,
            &mut caller_struct_sess,
            None,
            "dogmaIM",
            "ItemGetInfo",
            &[subloc(sid, InventoryFlag::CorpSAG1, 183)],
            &kw,
        )
        .await
        .expect("handled")
        .is_ok()
    );
    let err = handle_dogma_im(
        &ctx,
        &mut caller_struct_sess,
        None,
        "dogmaIM",
        "ItemGetInfo",
        &[subloc(sid, InventoryFlag::CorpSAG1, 185)],
        &kw,
    )
    .await
    .expect("handled")
    .unwrap_err();
    assert_eq!(err.code, "AccessDenied");

    let mut ally_struct_sess = mk_struct_sess(gunner_ally, ally_corp_id);
    let err = handle_dogma_im(
        &ctx,
        &mut ally_struct_sess,
        None,
        "dogmaIM",
        "ItemGetInfo",
        &[subloc(sid, InventoryFlag::CorpSAG1, 183)],
        &kw,
    )
    .await
    .expect("handled")
    .unwrap_err();
    assert_eq!(err.code, "AccessDenied");
    let ally_hi_info = handle_dogma_im(
        &ctx,
        &mut ally_struct_sess,
        None,
        "dogmaIM",
        "ItemGetInfo",
        &[subloc(sid, InventoryFlag::HiSlot0, 183)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("allied gunner inspects structure combat charge");
    assert!(
        format!("{ally_hi_info:?}").contains("\"ownerID\": Integer(98476201)"),
        "structure owner_corp_id charge must win over allied gunner corp shadow stack, got {ally_hi_info:?}"
    );

    let mut outsider_struct_sess = mk_struct_sess(outsider_id, ally_corp_id);
    let err = handle_dogma_im(
        &ctx,
        &mut outsider_struct_sess,
        None,
        "dogmaIM",
        "ItemGetInfo",
        &[subloc(sid, InventoryFlag::HiSlot0, 183)],
        &kw,
    )
    .await
    .expect("handled")
    .unwrap_err();
    assert_eq!(err.code, "AccessDenied");
}

#[tokio::test]
async fn destroy_fitting_on_corp_upwell_structure_emits_on_item_change_for_loaded_charge() {
    use crate::router::delta_item_switch::ItemWireFingerprint;
    use crate::router::ProjectedItemSwitchConfig;
    use eve_db::corp_roles::ROLE_DIRECTOR;
    use eve_db::{CharacterRecord, CorporationRecord, UpwellStructureRecord};

    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    ctx.set_projected_item_switches(ProjectedItemSwitchConfig::enabled());
    let (sys_id, corp_id, sid) = (30_000_142_u32, 98_476_901_i64, 1_096_476_000_901_i64);
    let (caller_id, mod_id, charge_id) = (96_476_901_i64, 96_476_910_i64, 96_476_911_i64);
    let uid = caller_id as u64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            format!("user_{uid}"),
            AccountRecord {
                user_id: uid,
                username: format!("user_{uid}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let base_char = snap.characters.values().next().cloned().expect("seed char");
        snap.characters.insert(
            caller_id,
            CharacterRecord {
                character_id: caller_id,
                user_id: uid,
                corporation_id: corp_id,
                station_id: None,
                structure_id: Some(sid),
                solar_system_id: sys_id,
                ..base_char
            },
        );
        let mut corp = CorporationRecord::new(corp_id, "Structure Owners", "STOW", caller_id);
        corp.members.insert(caller_id);
        corp.member_roles.insert(caller_id, ROLE_DIRECTOR);
        snap.corporations.insert(corp_id, corp);
        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 484,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge_id,
            InventoryItemRecord {
                item_id: charge_id,
                type_id: 183,
                owner_id: corp_id,
                location_id: sid,
                flag: InventoryFlag::HiSlot0,
                quantity: 20,
                singleton: false,
                ..Default::default()
            },
        );
    });
    ctx.db
        .acquire_online_lock(uid, uid, Some(caller_id), true, 1_700_000_500)
        .expect("online lock");

    let mut sess = SessionState::new(uid);
    sess.user_id = Some(uid);
    sess.character_id = Some(caller_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.structure_id = Some(sid);
    let _ = ctx.drain_notifications_for_char(caller_id);

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "DestroyFitting",
        &[PyRep::Integer(mod_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("DestroyFitting on corp structure succeeds");

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&mod_id));
        assert!(!snap.items.contains_key(&charge_id));
    });

    let notifs = ctx.drain_notifications_for_char(caller_id);
    let charge_changes: Vec<_> = notifs
        .iter()
        .filter(|n| n.event == "OnItemChange")
        .filter_map(|n| n.args.first().and_then(ItemWireFingerprint::from_pyrep_row))
        .filter(|(iid, _)| *iid == charge_id)
        .collect();
    assert_eq!(
        charge_changes.len(),
        1,
        "DestroyFitting on corp Upwell structure must emit OnItemChange for destroyed corp-owned loaded charge: {notifs:?}"
    );
    assert_eq!(charge_changes[0].1.owner_id, corp_id);
    assert_eq!(charge_changes[0].1.location_id, 6);
}

#[tokio::test]
async fn gh1195_t1_add_target_and_probe_invalid_target_emit_typed_user_errors_without_custom_info() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_399_001i64;
    let unspawned_char_id = 96_399_002i64;
    let sys_id = 30_000_142u32;
    let ship_id = 96_399_100i64;
    let unspawned_ship_id = 96_399_101i64;
    let near_target_id = 96_399_200i64;
    let far_target_id = 96_399_201i64;
    let probe_id = 96_399_202i64;
    let missing_target_id = 96_399_999i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.values().next().cloned().expect("seed char");
        for (cid, uid, sid, name) in [
            (char_id, 96_399_1u64, ship_id, "TypedTargetPilot"),
            (unspawned_char_id, 96_399_2u64, unspawned_ship_id, "UnspawnedPilot"),
        ] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.name = name.into();
            ch.corporation_id = 1_000_009;
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);
        }
        for (iid, owner, tid) in [
            (ship_id, char_id, 587u32),
            (unspawned_ship_id, unspawned_char_id, 587u32),
            (near_target_id, 96_399_888i64, 587u32),
            (far_target_id, 96_399_889i64, 587u32),
            (probe_id, 96_399_890i64, 22_778u32),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    for (bid, tid, pos, owner, grp, cat) in [
        (ship_id, 587u32, DVec3::ZERO, char_id, 25u32, 6u32),
        (near_target_id, 587u32, DVec3::new(4_000.0, 0.0, 0.0), 96_399_888i64, 25u32, 6u32),
        (far_target_id, 587u32, DVec3::new(500_000.0, 0.0, 0.0), 96_399_889i64, 25u32, 6u32),
        (probe_id, 22_778u32, DVec3::new(3_000.0, 0.0, 0.0), 96_399_890i64, 548u32, 8u32),
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
                    mass: 1_350_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState::default(),
                SlimItemMeta {
                    group_id: grp,
                    category_id: cat,
                    owner_id: owner,
                    char_id: Some(owner),
                    ..Default::default()
                },
            )
            .await;
    }

    let mut sess = SessionState::default();
    sess.user_id = Some(96_399_1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // 1. Distance > maxTargetRange -> TargetTooFar + TargetingFailed RejectionFacts
    let far_err = handle_dogma_im(&ctx, &mut sess, None, "dogmaIM", "AddTarget", &[PyRep::Integer(far_target_id)], &HashMap::new())
        .await
        .expect("handled")
        .expect_err("out-of-range target must fail");
    assert_eq!(far_err.code, "TargetingFailed");
    let far_ce = far_err.to_client_error("dogmaIM", "AddTarget");
    let (far_key, far_args) = far_ce.as_user_error().expect("typed UserError, not CustomInfo");
    assert_eq!(far_key, eve_proto::UserErrorKey::TargetTooFar);
    assert!(far_args.iter().any(|(k, _)| k == "targetname"));
    assert!(far_args.iter().any(|(k, v)| k == "module" && *v == PyRep::Integer(587)));
    assert!(matches!(
        far_err.rejection_facts(),
        Some(eve_proto::RejectionFacts::TargetingFailed {
            ship_id: Some(s),
            target_id: Some(t),
            reason,
            distance_m: Some(d),
            max_range_m: Some(max_r),
        }) if *s == ship_id
            && *t == far_target_id
            && reason == "exceeds_max_target_range"
            && *d > *max_r
            && far_args.iter().any(|(k, v)| {
                k == "distance" && matches!(v, PyRep::Real(arg_d) if (*arg_d - *d).abs() < 1e-3)
            })
    ));

    // 2. Active cloak -> CantTargetWhileCloaked; decloak recalibration delay -> DeniedTargetAfterCloak
    let _ = sys.modify_combat_state(ship_id, |cs| {
        cs.is_cloaked = true;
        cs.gate_cloak_remaining_ticks = 0;
    }).await;
    let cloak_err = handle_dogma_im(&ctx, &mut sess, None, "dogmaIM", "AddTarget", &[PyRep::Integer(near_target_id)], &HashMap::new())
        .await
        .expect("handled")
        .expect_err("cloaked ship cannot lock");
    assert_eq!(
        cloak_err.to_client_error("dogmaIM", "AddTarget").as_user_error().map(|(k, _)| k),
        Some(eve_proto::UserErrorKey::CantTargetWhileCloaked)
    );

    let _ = sys.modify_combat_state(ship_id, |cs| {
        cs.is_cloaked = false;
        cs.decloak_targeting_delay_remaining_s = 5.0;
    }).await;
    let delay_err = handle_dogma_im(&ctx, &mut sess, None, "dogmaIM", "AddTarget", &[PyRep::Integer(near_target_id)], &HashMap::new())
        .await
        .expect("handled")
        .expect_err("decloak delay cannot lock");
    assert_eq!(
        delay_err.to_client_error("dogmaIM", "AddTarget").as_user_error().map(|(k, _)| k),
        Some(eve_proto::UserErrorKey::DeniedTargetAfterCloak)
    );
    let _ = sys.modify_combat_state(ship_id, |cs| cs.decloak_targeting_delay_remaining_s = 0.0).await;

    // 3. Missing target ball -> TargetCantValidate
    let missing_err = handle_dogma_im(&ctx, &mut sess, None, "dogmaIM", "AddTarget", &[PyRep::Integer(missing_target_id)], &HashMap::new())
        .await
        .expect("handled")
        .expect_err("missing target ball must fail");
    assert_eq!(
        missing_err.to_client_error("dogmaIM", "AddTarget").as_user_error().map(|(k, _)| k),
        Some(eve_proto::UserErrorKey::TargetCantValidate)
    );

    // 4. Untargetable interdiction probe -> InvalidTarget -> InvalidTargetForModule + InvalidTarget facts
    let probe_err = handle_dogma_im(&ctx, &mut sess, None, "dogmaIM", "AddTarget", &[PyRep::Integer(probe_id)], &HashMap::new())
        .await
        .expect("handled")
        .expect_err("probe cannot be targeted");
    assert_eq!(probe_err.code, "InvalidTarget");
    assert_eq!(
        probe_err.to_client_error("dogmaIM", "AddTarget").as_user_error().map(|(k, _)| k),
        Some(eve_proto::UserErrorKey::InvalidTargetForModule)
    );
    assert!(matches!(
        probe_err.rejection_facts(),
        Some(eve_proto::RejectionFacts::InvalidTarget { target_id: Some(t), .. }) if *t == probe_id
    ));

    // 5. Missing source ship ball -> CantShipNotPresent
    let mut unspawned_sess = sess;
    unspawned_sess.user_id = Some(96_399_2);
    unspawned_sess.character_id = Some(unspawned_char_id);
    unspawned_sess.ship_id = Some(unspawned_ship_id);
    let no_ship_err = handle_dogma_im(&ctx, &mut unspawned_sess, None, "dogmaIM", "AddTarget", &[PyRep::Integer(near_target_id)], &HashMap::new())
        .await
        .expect("handled")
        .expect_err("unspawned source ship must fail");
    assert_eq!(
        no_ship_err.to_client_error("dogmaIM", "AddTarget").as_user_error().map(|(k, _)| k),
        Some(eve_proto::UserErrorKey::CantShipNotPresent)
    );
}

#[test]
fn dogma_session_check_store_is_isolated_per_server_context() {
    let ctx_a = ServerContext::with_repository(Arc::new(WorldRepository::with_demo_seed()));
    let ctx_b = ServerContext::with_repository(Arc::new(WorldRepository::with_demo_seed()));

    let mut sess = SessionState::new(99_101);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(1_000_009);
    sess.solar_system_id = Some(30_000_142);

    let spec = PyRep::Dict(vec![(
        PyRep::String("charid".into()),
        PyRep::Integer(90_000_001),
    )]);
    let kw = HashMap::new();

    let first_a = handle_dogma_set_session_check(&ctx_a, &sess, None, &[spec.clone()], &kw)
        .expect("first SetSessionCheck on ctx_a");
    assert_eq!(first_a.dict_get("checkCount"), Some(&PyRep::Integer(1)));

    let second_a = handle_dogma_set_session_check(&ctx_a, &sess, None, &[spec.clone()], &kw)
        .expect("second SetSessionCheck on ctx_a");
    assert_eq!(second_a.dict_get("checkCount"), Some(&PyRep::Integer(2)));

    // Identical session ID on a separate ServerContext starts with an isolated checkCount.
    let first_b = handle_dogma_set_session_check(&ctx_b, &sess, None, &[spec], &kw)
        .expect("first SetSessionCheck on ctx_b");
    assert_eq!(first_b.dict_get("checkCount"), Some(&PyRep::Integer(1)));

    // Clearing ctx_a leaves ctx_b's stored session-check count intact.
    let cleared_a = handle_dogma_set_session_check(&ctx_a, &sess, None, &[PyRep::None], &kw)
        .expect("clear ctx_a");
    assert_eq!(cleared_a.dict_get("checkCount"), Some(&PyRep::Integer(0)));

    let second_b = handle_dogma_set_session_check(&ctx_b, &sess, None, &[], &kw)
        .expect("ctx_b still tracks session check");
    assert_eq!(second_b.dict_get("checkCount"), Some(&PyRep::Integer(2)));
}
