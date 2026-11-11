//! Edge-case, ACL, structure-tower, and error-path tests for `fitting_ship_forcefield`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::inventory_fitting::fitting_containers_ships::{
    read_container_ship_state, save_container_ship_state,
};
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, StarbaseDetailSettings,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;

#[tokio::test]
async fn forcefield_protection_blocking_roles_structure_towers_and_ballpark_fallback() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            992_001,
            custom_inv_type(sde, 992_001, 365, 23, "Test Control Tower", &[(680, 20_000.0), (2, 100.0)]),
        );
        sde.types.insert(
            992_002,
            custom_inv_type(sde, 992_002, 340, 2, "Secure Cargo Container", &[]),
        );
        sde.types.insert(
            992_003,
            custom_inv_type(sde, 992_003, 1250, 22, "Mobile Tractor Unit", &[]),
        );
        sde.types.insert(
            992_004,
            custom_inv_type(sde, 992_004, 999, 18, "Personal Mobile Depot", &[]),
        );
    });

    assert!(is_deployable_item(&ctx.sde, 992_002));
    assert!(!is_infrastructure_item(&ctx.sde, 992_002));
    assert!(is_deployable_item(&ctx.sde, 992_003));
    assert!(is_infrastructure_item(&ctx.sde, 992_003));
    assert!(!is_deployable_item(&ctx.sde, 992_004));
    assert!(is_mobile_depot(&ctx.sde, 992_004));
    assert!(!is_deployable_item(&ctx.sde, 88_888_888));
    assert!(!is_infrastructure_item(&ctx.sde, 88_888_888));
    assert!(!is_mobile_depot(&ctx.sde, 88_888_888));

    let sys_id = 30_000_142u32;
    let ceo_id = 96_600_001i64;
    let member_id = 96_600_002i64;
    let outsider_id = 96_600_003i64;
    let corp_id = 98_600_001i64;
    let other_corp_id = 98_600_002i64;
    let member_ship = 96_600_101i64;
    let outsider_ship = 96_600_102i64;
    let tower_item_id = 96_600_200i64;
    let tower_fuel_id = 96_600_201i64;
    let struct_tower_id = 1_000_000_600_202i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.values().next().cloned().expect("seed char");
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "TowerCorp".into(); __corp.ceo_id = ceo_id; __corp.ensure_wallet_divisions(); __corp },
        );
        for (cid, co) in [(ceo_id, corp_id), (member_id, corp_id), (outsider_id, other_corp_id)] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.name = format!("Pilot{cid}");
            ch.corporation_id = co;
            ch.solar_system_id = sys_id;
            snap.characters.insert(cid, ch);
        }
        snap.items.insert(
            member_ship,
            InventoryItemRecord {
                item_id: member_ship,
                type_id: 587,
                owner_id: member_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            outsider_ship,
            InventoryItemRecord {
                item_id: outsider_ship,
                type_id: 587,
                owner_id: outsider_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            tower_item_id,
            InventoryItemRecord {
                item_id: tower_item_id,
                type_id: 992_001,
                owner_id: corp_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            tower_fuel_id,
            InventoryItemRecord {
                item_id: tower_fuel_id,
                type_id: 4051,
                owner_id: corp_id,
                location_id: tower_item_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 0,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_tower_id,
            UpwellStructureRecord {
                structure_id: struct_tower_id,
                type_id: 992_001,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "StructTower".into(),
                position: [100_000.0, 0.0, 0.0],
                state: "Online".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                hull_hp: 10_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.starbase_settings.insert(
            struct_tower_id,
            StarbaseDetailSettings {
                allow_corporation_members: false,
                password: Some("structpw".into()),
                ..Default::default()
            },
        );
    });

    // Place tower_item_id ONLY in ballpark (tests ballpark fallback when item_positions is absent!)
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: tower_item_id,
            type_id: 992_001,
            position: DVec3::new(0.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    assert!(!can_enter_forcefield(&ctx, ceo_id, member_ship, 77_777_777));
    assert!(can_enter_forcefield(&ctx, ceo_id, member_ship, tower_item_id));
    assert!(!can_enter_forcefield(&ctx, member_id, member_ship, tower_item_id));

    assert!(can_enter_forcefield(&ctx, ceo_id, member_ship, struct_tower_id));
    assert!(!can_enter_forcefield(&ctx, member_id, member_ship, struct_tower_id));

    {
        let mut st = read_container_ship_state(&ctx);
        st.ship_configs.entry(member_ship).or_default().password = Some("wrongpw".into());
        save_container_ship_state(&ctx, &st);
    }
    assert!(!can_enter_forcefield(&ctx, member_id, member_ship, struct_tower_id));

    {
        let mut st = read_container_ship_state(&ctx);
        st.ship_configs.entry(member_ship).or_default().password = Some("structpw".into());
        save_container_ship_state(&ctx, &st);
    }
    assert!(can_enter_forcefield(&ctx, member_id, member_ship, struct_tower_id));

    ctx.db.with_mut_snapshot(|snap| {
        snap.starbase_settings.insert(
            tower_item_id,
            StarbaseDetailSettings {
                allow_corporation_members: false,
                ..Default::default()
            },
        );
    });
    assert!(
        !is_pos_forcefield_protected(&ctx, sys_id, member_ship, DVec3::new(5_000.0, 0.0, 0.0)).await
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&tower_fuel_id).unwrap().quantity = 10;
    });
    assert!(
        !is_pos_forcefield_protected(&ctx, sys_id, tower_item_id, DVec3::new(0.0, 0.0, 0.0)).await
    );
    assert!(
        is_pos_forcefield_protected(&ctx, sys_id, member_ship, DVec3::new(5_000.0, 0.0, 0.0)).await
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(tower_item_id, false);
    });
    assert!(
        !is_pos_forcefield_protected(&ctx, sys_id, member_ship, DVec3::new(5_000.0, 0.0, 0.0)).await
    );
    assert!(
        check_pos_forcefield_coverage(&ctx, sys_id, outsider_id, outsider_ship, &[DVec3::new(5_000.0, 0.0, 0.0)])
            .await
            .is_ok()
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(tower_item_id, true);
        snap.items.get_mut(&tower_item_id).unwrap().dynamic_attributes.insert(2, 0.0);
    });
    assert!(
        !is_pos_forcefield_protected(&ctx, sys_id, member_ship, DVec3::new(5_000.0, 0.0, 0.0)).await
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&tower_item_id).unwrap().dynamic_attributes.insert(2, 100.0);
    });
    let empty_bp = eve_destiny::Ballpark::new(sys_id);
    ctx.db.with_snapshot(|snap| {
        assert!(is_pos_forcefield_blocked_in_snap(
            snap,
            &ctx.sde,
            &empty_bp,
            sys_id,
            outsider_id,
            outsider_ship,
            &[DVec3::new(105_000.0, 0.0, 0.0)],
        ));
        assert!(!is_pos_forcefield_blocked_in_snap(
            snap,
            &ctx.sde,
            &empty_bp,
            sys_id,
            member_id,
            member_ship,
            &[DVec3::new(105_000.0, 0.0, 0.0)],
        ));
    });
    assert_eq!(
        check_pos_forcefield_coverage(&ctx, sys_id, outsider_id, outsider_ship, &[DVec3::new(5_000.0, 0.0, 0.0)])
            .await
            .unwrap_err()
            .code,
        "ForcefieldAccessDenied"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_tower_id).unwrap().state = "Destroyed".into();
    });
    assert!(
        !is_pos_forcefield_protected(&ctx, sys_id, outsider_ship, DVec3::new(105_000.0, 0.0, 0.0)).await
    );
}

#[test]
fn ship_configuration_error_and_flag_paths() {
    let ctx = test_ctx_with_custom(|_| {});
    let owner_id = 96_610_001i64;
    let ship_id = 96_610_100i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let unauth = SessionState::default();
    assert!(ship_configuration(&ctx, &unauth, "GetShipConfiguration", &[]).is_err());

    let owner_sess = SessionState {
        user_id: Some(1),
        character_id: Some(owner_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let mut missing_sess = owner_sess.clone();
    missing_sess.ship_id = Some(88_888_888);
    assert_eq!(
        ship_configuration(&ctx, &missing_sess, "GetShipConfiguration", &[])
            .unwrap_err()
            .code,
        "ShipNotFound"
    );

    assert_eq!(
        ship_configuration(&ctx, &owner_sess, "SetShipPassword", &[PyRep::String("alpha".into())])
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        read_container_ship_state(&ctx).ship_passwords.get(&ship_id).map(String::as_str),
        Some("alpha")
    );
    assert_eq!(
        ship_configuration(&ctx, &owner_sess, "SetShipPassword", &[PyRep::Integer(ship_id), PyRep::String("".into())])
            .unwrap(),
        PyRep::None
    );
    assert!(!read_container_ship_state(&ctx).ship_passwords.contains_key(&ship_id));

    assert!(ship_configuration(
        &ctx,
        &owner_sess,
        "SetShipPassword",
        &[PyRep::String("beta".into()), PyRep::String("ignored".into())],
    )
    .is_ok());
    assert_eq!(
        read_container_ship_state(&ctx).ship_passwords.get(&ship_id).map(String::as_str),
        Some("beta")
    );

    let cfg_res = ship_configuration(
        &ctx,
        &owner_sess,
        "ConfigureShip",
        &[PyRep::Dict(vec![
            (PyRep::String(CFG_FLEET_HANGAR_CORP.into()), PyRep::Integer(0)),
            (PyRep::String(CFG_SMB_FLEET.into()), PyRep::Bool(false)),
        ])],
    )
    .unwrap();
    let st = read_container_ship_state(&ctx);
    assert!(!st.ship_configs[&ship_id].allow_corp_hangar);
    assert!(!st.ship_configs[&ship_id].allow_fleet_smb);
    assert!(matches!(cfg_res, PyRep::Dict(_)));

    assert_eq!(
        ship_configuration(
            &ctx,
            &owner_sess,
            "ConfigureShip",
            &[
                PyRep::Integer(ship_id),
                PyRep::Dict(vec![(PyRep::String("BadKey".into()), PyRep::Bool(true))]),
            ],
        )
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
}
