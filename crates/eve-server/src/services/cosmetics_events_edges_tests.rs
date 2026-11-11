//! Edge-case behavioral unit tests for `cosmetics_events` (`shipCosmeticsMgr`, `materialize_system_lod2`, `interdict_convoy`).

#![forbid(unsafe_code)]

use super::*;
use crate::living_world::{LivingWorldConvoy, NpcArchetype};
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::{AccountRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::InvType as SdeInvType;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn gm_session(char_id: i64, sys_id: Option<u32>, ship_id: Option<i64>) -> SessionState {
    let mut sess = auth_session(1, char_id, 98_000_001, sys_id.unwrap_or(30_000_142));
    sess.role = service_roles::ROLE_GMH | service_roles::ROLE_GML | service_roles::ROLE_ADMIN;
    sess.solar_system_id = sys_id;
    sess.ship_id = ship_id;
    sess
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn materialize_system_lod2_all_error_and_omitted_arg_paths() {
    let ctx = test_ctx();
    let unauth = SessionState::default();
    assert!(materialize_system_lod2(&ctx, &unauth, &[]).await.is_err());

    let normal = auth_session(1, 90_300_001, 98_000_001, 30_000_142);
    assert_eq!(
        materialize_system_lod2(&ctx, &normal, &[]).await.unwrap_err().code,
        "InsufficientRole"
    );

    let gm_no_sys = gm_session(90_300_001, None, None);
    assert_eq!(
        materialize_system_lod2(&ctx, &gm_no_sys, &[]).await.unwrap_err().code,
        "UnknownSolarSystem"
    );

    let gm = gm_session(90_300_001, Some(30_000_142), None);
    // Negative arg fails u32::try_from
    assert_eq!(
        materialize_system_lod2(&ctx, &gm, &[PyRep::Integer(-1)])
            .await
            .unwrap_err()
            .code,
        "UnknownSolarSystem"
    );
    // Non-existent SDE system arg
    assert_eq!(
        materialize_system_lod2(&ctx, &gm, &[PyRep::Integer(999_999_999)])
            .await
            .unwrap_err()
            .code,
        "UnknownSolarSystem"
    );
    // Remote system arg != session system
    assert_eq!(
        materialize_system_lod2(&ctx, &gm, &[PyRep::Integer(30_000_144)])
            .await
            .unwrap_err()
            .code,
        "CannotMaterializeRemoteSystem"
    );
    // Omitted arg succeeds for valid session system
    let count = materialize_system_lod2(&ctx, &gm, &[]).await.unwrap();
    assert!(matches!(count, PyRep::Integer(n) if n >= 0));

    // Omitted arg with unknown session system
    let gm_bad_sys = gm_session(90_300_001, Some(999_999_999), None);
    assert_eq!(
        materialize_system_lod2(&ctx, &gm_bad_sys, &[])
            .await
            .unwrap_err()
            .code,
        "UnknownSolarSystem"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interdict_convoy_all_guards_success_and_duplicate_failure() {
    let ctx = test_ctx();
    let char_id = 90_300_002_i64;
    let ship_id = 1_000_300_002_i64;
    let convoy_id = 800_900_001_i64;
    let sys_id = 30_000_142_u32;

    // Unauthenticated
    let unauth = SessionState::default();
    assert!(interdict_convoy(&ctx, &unauth, &[]).await.is_err());

    // Docked via structure_id
    let mut gm_struct = gm_session(char_id, Some(sys_id), Some(ship_id));
    gm_struct.structure_id = Some(1_040_000_000_001);
    assert_eq!(
        interdict_convoy(&ctx, &gm_struct, &[PyRep::Integer(convoy_id)])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );

    ctx.living_world.register_convoy(LivingWorldConvoy {
        convoy_id,
        archetype: NpcArchetype::PirateRoamer,
        ship_type_id: 16997,
        origin_system_id: sys_id,
        current_system_id: sys_id,
        destination_system_id: 30_000_144,
        destination_station_id: 60_003_760,
        cargo_type_id: 34,
        cargo_quantity: 50,
        lod_level: 2,
        destroyed: false,
    });

    // Wrong system
    let gm_other_sys = gm_session(char_id, Some(30_000_144), Some(ship_id));
    assert_eq!(
        interdict_convoy(&ctx, &gm_other_sys, &[PyRep::Integer(convoy_id)])
            .await
            .unwrap_err()
            .code,
        "ConvoyWrongSystem"
    );

    // Same system, ship not in space
    let gm = gm_session(char_id, Some(sys_id), Some(ship_id));
    assert_eq!(
        interdict_convoy(&ctx, &gm, &[PyRep::Integer(convoy_id)])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Spawn ship ball, convoy ball not yet on grid -> ConvoyNotMaterializedOnGrid
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    assert_eq!(
        interdict_convoy(&ctx, &gm, &[PyRep::Integer(convoy_id)])
            .await
            .unwrap_err()
            .code,
        "ConvoyNotMaterializedOnGrid"
    );

    // Spawn convoy ball > 30 km away -> OutsideConvoyInterdictionRange
    let far_convoy_id = 800_900_002_i64;
    ctx.living_world.register_convoy(LivingWorldConvoy {
        convoy_id: far_convoy_id,
        archetype: NpcArchetype::PirateRoamer,
        ship_type_id: 16997,
        origin_system_id: sys_id,
        current_system_id: sys_id,
        destination_system_id: 30_000_144,
        destination_station_id: 60_003_760,
        cargo_type_id: 34,
        cargo_quantity: 50,
        lod_level: 2,
        destroyed: false,
    });
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: far_convoy_id,
                type_id: 16997,
                position: DVec3::new(45_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 80.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        interdict_convoy(&ctx, &gm, &[PyRep::Integer(far_convoy_id)])
            .await
            .unwrap_err()
            .code,
        "OutsideConvoyInterdictionRange"
    );

    // Spawn target convoy ball within 30 km -> full success
    let convoy_pos = DVec3::new(12_000.0, 3_000.0, -4_000.0);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: convoy_id,
                type_id: 16997,
                position: convoy_pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 80.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    let ok_res = interdict_convoy(&ctx, &gm, &[PyRep::Integer(convoy_id)])
        .await
        .unwrap();
    assert_eq!(ok_res.dict_get("convoyID"), Some(&PyRep::Integer(convoy_id)));
    let wreck_id = ok_res
        .dict_get("wreckItemID")
        .and_then(|v| v.as_i64())
        .expect("wreckItemID");
    assert!(ok_res.dict_get("lootItemID").and_then(|v| v.as_i64()).is_some());
    assert!(ok_res.dict_get("newDestinationAskISK").is_some());

    // Convoy ball removed, wreck ball spawned, position persisted
    assert!(sys_handle.get_ball_snapshot(convoy_id).await.unwrap().is_none());
    let (_, wreck_ball) = sys_handle.get_ball_snapshot(wreck_id).await.unwrap().expect("wreck ball");
    assert_eq!(wreck_ball.type_id, 26557);
    assert_eq!(wreck_ball.position, convoy_pos);
    assert!(ctx
        .db
        .with_snapshot(|s| s.item_positions.contains_key(&wreck_id)));

    // Respawn destroyed convoy's ball so Ballpark passes -> hits InterdictionFailed from living_world
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: convoy_id,
                type_id: 16997,
                position: convoy_pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 80.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        interdict_convoy(&ctx, &gm, &[PyRep::Integer(convoy_id)])
            .await
            .unwrap_err()
            .code,
        "InterdictionFailed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ship_cosmetics_mgr_give_and_activate_skin_edges() {
    let ctx = ServerContext::with_sde_and_repository(
        test_ctx().sde.clone(),
        Arc::new(WorldRepository::new_empty()),
    );
    let char_id = 90_300_003_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 1_000_300_003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "cosm_gm".into(),
            AccountRecord {
                user_id: 1,
                username: "cosm_gm".into(),
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
                user_id: 1,
                name: "CosmGM".into(),
                corporation_id: 98_000_001,
                alliance_id: None,
                war_faction_id: None,
                station_id: Some(station_id),
                structure_id: None,
                solar_system_id: 30_000_142,
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: ship_id,
                balance: eve_db::IskCents(0),
                plex_balance: 0,
                loyalty_points: Default::default(),
                standings: Default::default(),
                security_status: 0.0,
                clone_grade: 2,
                home_station_id: station_id,
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
    });
    let kw = HashMap::new();

    // Unknown service / method -> None
    let mut unauth = SessionState::default();
    assert!(
        handle(&ctx, &mut unauth, "shipCosmeticsMgr", "unknown_method_fallback", &[], &kw)
            .await
            .is_none()
    );
    assert!(
        handle(&ctx, &mut unauth, "other_service", "GiveSkin", &[], &kw)
            .await
            .is_none()
    );
    // Unauth errors for GiveSkin and ActivateSkinLicense
    assert!(handle(&ctx, &mut unauth, "shipCosmeticsMgr", "GiveSkin", &[], &kw)
        .await
        .unwrap()
        .is_err());
    assert!(
        handle(&ctx, &mut unauth, "shipCosmeticsMgr", "ActivateSkinLicense", &[], &kw)
            .await
            .unwrap()
            .is_err()
    );

    let mut gm = gm_session(char_id, Some(30_000_142), Some(ship_id));
    gm.station_id = Some(station_id);

    // GiveSkin missing / negative / non-existent args
    assert_eq!(
        handle(&ctx, &mut gm, "shipCosmeticsMgr", "GiveSkin", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        handle(&ctx, &mut gm, "shipCosmeticsMgr", "GiveSkin", &[PyRep::Integer(-3)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        handle(&ctx, &mut gm, "shipCosmeticsMgr", "GiveSkin", &[PyRep::Integer(999_999_999)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkinLicenseType"
    );

    // Find a valid license type whose mapped skin has >= 1 compatible ship type
    let (lic_tid, compat_ship_tid, mapped_skin_id) = ctx
        .sde
        .skin_licenses
        .iter()
        .filter_map(|(&tid, lic)| {
            let t = ctx.sde.types.get(&tid)?;
            if t.category_id != 91 {
                return None;
            }
            let skin = ctx.sde.skins.get(&lic.skin_id)?;
            let ship_tid = *skin.types.first()?;
            Some((tid, ship_tid, lic.skin_id))
        })
        .min_by_key(|&(tid, _, _)| tid)
        .expect("SDE must contain a valid skin license");

    // GiveSkin passing license typeID without ship_tid -> resolves via sde.skin_licenses & idempotent on repeat
    let give1 = handle(
        &ctx,
        &mut gm,
        "shipCosmeticsMgr",
        "GiveSkin",
        &[PyRep::Integer(i64::from(lic_tid))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(give1.dict_get("licensed"), Some(&PyRep::Bool(true)));
    let give2 = handle(
        &ctx,
        &mut gm,
        "shipCosmeticsMgr",
        "GiveSkin",
        &[PyRep::Integer(i64::from(lic_tid)), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(give2.dict_get("skinID"), Some(&PyRep::Integer(i64::from(lic_tid))));
    let skin_count_after_repeat = ctx.db.with_snapshot(|s| {
        s.licensed_skins
            .get(&char_id)
            .map(|v| v.iter().filter(|&&id| id == lic_tid).count())
            .unwrap_or(0)
    });
    assert_eq!(skin_count_after_repeat, 1);

    // ActivateSkinLicense error guards: missing, negative
    assert_eq!(
        handle(&ctx, &mut gm, "shipCosmeticsMgr", "ActivateSkinLicense", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[PyRep::Integer(-1)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkinLicenseType"
    );
    // Incompatible ship type
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[PyRep::Integer(i64::from(lic_tid)), PyRep::Integer(999_999)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidShipTypeForSkin"
    );

    // Activate qty=1 in ship cargo -> removes item cleanly and keeps existing licensed_skins deduplicated
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: compat_ship_tid,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            880_001,
            InventoryItemRecord {
                item_id: 880_001,
                type_id: lic_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });
    gm.station_id = None;
    let act_ok = handle(
        &ctx,
        &mut gm,
        "shipCosmeticsMgr",
        "ActivateSkinLicense",
        &[
            PyRep::Integer(i64::from(lic_tid)),
            PyRep::Integer(i64::from(compat_ship_tid)),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(act_ok.dict_get("licensed"), Some(&PyRep::Bool(true)));
    assert_eq!(act_ok.dict_get("skinHashID"), Some(&PyRep::Integer(i64::from(lic_tid))));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&880_001)));
    assert!(ctx.db.with_snapshot(|s| {
        let skins = s.licensed_skins.get(&char_id).unwrap();
        skins.contains(&mapped_skin_id)
    }));

    // Custom mutated SDE: category 91 unmapped in skin_licenses, and license mapped to missing skin_id
    let mut custom_sde = (*ctx.sde).clone();
    let base_type: SdeInvType = custom_sde.types.values().next().unwrap().clone();
    let base_license = custom_sde.skin_licenses.values().next().unwrap().clone();
    let unmapped_cat91 = 9_999_001_u32;
    let dangling_lic = 9_999_002_u32;
    custom_sde.types.insert(
        unmapped_cat91,
        SdeInvType {
            type_id: unmapped_cat91,
            group_id: 1,
            category_id: 91,
            name: "Unmapped Test SKIN".into(),
            published: true,
            ..base_type.clone()
        },
    );
    custom_sde.types.insert(
        dangling_lic,
        SdeInvType {
            type_id: dangling_lic,
            group_id: 1,
            category_id: 91,
            name: "Dangling Test SKIN".into(),
            published: true,
            ..base_type
        },
    );
    let mut dangling_info = base_license;
    dangling_info.license_type_id = dangling_lic;
    dangling_info.skin_id = 9_999_999;
    dangling_info.duration = -1;
    custom_sde.skin_licenses.insert(dangling_lic, dangling_info);
    let custom_ctx = ServerContext::with_sde_and_repository(
        Arc::new(custom_sde),
        Arc::new(WorldRepository::new_empty()),
    );
    assert_eq!(
        handle(
            &custom_ctx,
            &mut gm,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[PyRep::Integer(i64::from(unmapped_cat91))],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        handle(
            &custom_ctx,
            &mut gm,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[PyRep::Integer(i64::from(dangling_lic))],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSkinLicenseType"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mutant_kill_cosmetics_events_dispatch_and_ship_cargo_activate() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 880_800_001_i64;
    let lic_item_id = 880_800_002_i64;
    let kw = HashMap::new();

    let mut gm = gm_session(char_id, Some(30_000_142), Some(ship_id));
    gm.station_id = None;
    gm.structure_id = None;

    // Unknown service / method must return None
    assert!(handle(&ctx, &mut gm, "nonmatching_service", "GiveSkin", &[], &kw).await.is_none());
    assert!(handle(&ctx, &mut gm, "shipCosmeticsMgr", "unmatched_method", &[], &kw).await.is_none());

    let (lic_tid, compat_ship_tid, mapped_skin_id, material_id, internal_name) = ctx
        .sde
        .skin_licenses
        .iter()
        .filter_map(|(&tid, lic)| {
            let t = ctx.sde.types.get(&tid)?;
            if t.category_id != 91 {
                return None;
            }
            let skin = ctx.sde.skins.get(&lic.skin_id)?;
            let ship_tid = *skin.types.first()?;
            Some((
                tid,
                ship_tid,
                lic.skin_id,
                skin.skin_material_id,
                skin.internal_name.clone(),
            ))
        })
        .min_by_key(|&(tid, _, _, _, _)| tid)
        .expect("valid skin license");

    // GiveSkin returns full KeyVal & persists skin
    let give = handle(
        &ctx,
        &mut gm,
        "shipCosmeticsMgr",
        "GiveSkin",
        &[PyRep::Integer(i64::from(lic_tid)), PyRep::Integer(i64::from(compat_ship_tid))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(give.dict_get("characterID"), Some(&PyRep::Integer(char_id)));
    assert_eq!(give.dict_get("skinID"), Some(&PyRep::Integer(i64::from(lic_tid))));
    assert_eq!(give.dict_get("skinMaterialID"), Some(&PyRep::Integer(i64::from(material_id))));
    assert_eq!(give.dict_get("internalName"), Some(&PyRep::String(internal_name)));
    assert_eq!(give.dict_get("licensed"), Some(&PyRep::Bool(true)));

    // Clear skins & place license stack (qty=2) inside active ship cargo in space -> requires active_ship_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.licensed_skins.remove(&char_id);
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: compat_ship_tid,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            lic_item_id,
            InventoryItemRecord {
                item_id: lic_item_id,
                type_id: lic_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let act = handle(
        &ctx,
        &mut gm,
        "shipCosmeticsMgr",
        "ActivateSkinLicense",
        &[PyRep::Integer(i64::from(lic_tid)), PyRep::Integer(i64::from(compat_ship_tid))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(act.dict_get("characterID"), Some(&PyRep::Integer(char_id)));
    assert_eq!(act.dict_get("skinID"), Some(&PyRep::Integer(i64::from(lic_tid))));
    assert_eq!(act.dict_get("licensed"), Some(&PyRep::Bool(true)));
    assert_eq!(ctx.db.get_item(lic_item_id).unwrap().quantity, 1);
    assert!(ctx.db.with_snapshot(|s| {
        let sk = &s.licensed_skins[&char_id];
        sk.contains(&lic_tid) && sk.contains(&mapped_skin_id)
    }));
}
