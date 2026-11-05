//! Unit tests for `fitting_dogma_ops` (`AddTargetOBO`, `CancelAddTarget`, `RemoveTargetOBO`, `RemoveTargets`, `QueryAttributeValue`, `QueryAllAttributesForItem`, `FullyDescribeAttribute`, `ShipOnlineModules`, `InitiateModuleRepairMany`, `ChangeDroneSettings`).

use super::*;
use eve_db::{AccountRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository};
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
async fn dogma_ops_targeting_attribute_queries_online_repair_and_drone_settings() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_200_001i64;
    let sys_id = 30_000_142u32;
    let ship_id = 96_200_100i64;
    let target_ship_id = 96_200_200i64;
    let mod_id = 96_200_300i64;
    let paste_id = 96_200_400i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "user_96201".into(),
            AccountRecord {
                user_id: 96_201,
                username: "user_96201".into(),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 96_201;
        ch.name = "DogmaOpsPilot".into();
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
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            target_ship_id,
            InventoryItemRecord {
                item_id: target_ship_id,
                type_id: 587,
                owner_id: 96_200_999,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
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
                type_id: 484, // 125mm Gatling AutoCannon I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(attr_ids::DAMAGE, 10.0), (attr_ids::HP_STRUCT, 40.0)]),
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mod_id, false);
        snap.items.insert(
            paste_id,
            InventoryItemRecord {
                item_id: paste_id,
                type_id: 28668, // Nanite Repair Paste
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        for &sk in &[3300u32, 3302, 3318, 3429, 3430, 28164, 28879, 28880, 3392, 3426] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut cs = CombatState::default();
    cs.capacitor = 250.0;
    cs.max_capacitor = 250.0;
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
                position: DVec3::new(5_000.0, 0.0, 0.0),
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
                owner_id: 96_200_999,
                char_id: Some(96_200_999),
                ..Default::default()
            },
        )
        .await;

    let mut session = SessionState::default();
    session.user_id = Some(96_201);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);

    // 1. AddTargetOBO, CancelAddTarget, RemoveTargetOBO, RemoveTargets
    let add_res = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "AddTargetOBO",
        &[PyRep::Integer(ship_id), PyRep::Integer(target_ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("AddTargetOBO ok");
    assert!(matches!(add_res, PyRep::Tuple(_)));

    let _ = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "CancelAddTarget",
        &[PyRep::Integer(target_ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("CancelAddTarget ok");

    let _ = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "AddTargetOBO",
        &[PyRep::Integer(ship_id), PyRep::Integer(target_ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("AddTargetOBO 2 ok");

    let _ = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "RemoveTargetOBO",
        &[PyRep::Integer(ship_id), PyRep::Integer(target_ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("RemoveTargetOBO ok");

    let _ = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "RemoveTargets",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("RemoveTargets ok");

    // 2. QueryAttributeValue, QueryAllAttributesForItem, FullyDescribeAttribute
    let dmg_val = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "QueryAttributeValue",
        &[PyRep::Integer(mod_id), PyRep::Integer(attr_ids::DAMAGE as i64)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("QueryAttributeValue ok");
    assert_eq!(dmg_val, PyRep::Real(10.0));

    let all_attrs = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "QueryAllAttributesForItem",
        &[PyRep::Integer(ship_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("QueryAllAttributesForItem ok");
    assert!(matches!(all_attrs, PyRep::Dict(_)));

    let desc = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "FullyDescribeAttribute",
        &[PyRep::Integer(mod_id), PyRep::Integer(attr_ids::DAMAGE as i64)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("FullyDescribeAttribute ok");
    assert!(matches!(desc, PyRep::Tuple(_)));

    // 3. ShipOnlineModules
    let _ = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "ShipOnlineModules",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("ShipOnlineModules ok");
    let is_online = ctx
        .db
        .with_snapshot(|s| s.module_online_states.get(&mod_id).copied().unwrap_or(false));
    assert!(is_online);

    // 4. InitiateModuleRepairMany
    let rep_res = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "InitiateModuleRepairMany",
        &[PyRep::List(vec![PyRep::Integer(mod_id)])],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("InitiateModuleRepairMany ok");
    if let PyRep::Tuple(parts) = rep_res {
        assert_eq!(parts.first(), Some(&PyRep::List(vec![PyRep::Integer(mod_id)])));
    } else {
        panic!("Expected Tuple");
    }

    // 5. ChangeDroneSettings
    let _ = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "ChangeDroneSettings",
        &[PyRep::Dict(vec![
            (PyRep::String("aggressive".into()), PyRep::Bool(false)),
            (PyRep::String("focusFire".into()), PyRep::Bool(true)),
        ])],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("ChangeDroneSettings ok");
    let saved_ds = ctx
        .db
        .with_snapshot(|s| s.combat_crimewatch_state.drone_settings.get(&char_id).copied());
    assert_eq!(saved_ds, Some((false, true)));
}
