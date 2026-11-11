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

    // Float 0.0/1.0 accepted via attribute-ID dict, tuple, and kwargs; non-0/1 floats rejected
    let _ = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "ChangeDroneSettings",
        &[PyRep::Dict(vec![
            (PyRep::Integer(1297), PyRep::Real(0.0)),
            (PyRep::Integer(1275), PyRep::Real(1.0)),
        ])],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("ChangeDroneSettings float attr dict ok");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.combat_crimewatch_state.drone_settings.get(&char_id).copied()),
        Some((true, false))
    );

    let _ = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "ChangeDroneSettings",
        &[PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(1.0)])],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("ChangeDroneSettings float tuple ok");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.combat_crimewatch_state.drone_settings.get(&char_id).copied()),
        Some((false, true))
    );

    let mut kw_float = HashMap::new();
    kw_float.insert("aggressive".into(), PyRep::Real(1.0));
    kw_float.insert("focusFire".into(), PyRep::Real(0.0));
    let _ = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "ChangeDroneSettings",
        &[],
        &kw_float,
    )
    .await
    .expect("handled")
    .expect("ChangeDroneSettings float kwargs ok");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.combat_crimewatch_state.drone_settings.get(&char_id).copied()),
        Some((true, false))
    );

    let bad_float = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "ChangeDroneSettings",
        &[PyRep::Tuple(vec![PyRep::Real(0.5), PyRep::Real(1.0)])],
        &HashMap::new(),
    )
    .await
    .expect("handled");
    assert_eq!(bad_float.unwrap_err().code, "InvalidArgument");
}

fn make_test_inv_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    default_attributes: HashMap<u16, f64>,
    effect_ids: Vec<u16>,
) -> eve_sde::InvType {
    eve_sde::InvType {
        type_id,
        group_id,
        category_id,
        name: name.into(),
        mass: 1_000.0,
        volume: 10.0,
        capacity: 10.0,
        portion_size: 1,
        radius: 10.0,
        base_price: 0.0,
        published: true,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids,
        default_attributes,
    }
}

#[tokio::test]
async fn query_item_attributes_routes_modules_charges_drones_and_fleet_bursts_without_double_apply() {
    let mut sde = shared_sde().as_ref().clone();
    sde.attributes.insert(
        212,
        eve_sde::DogmaAttributeType {
            attribute_id: 212,
            name: "missileDamageMultiplier".to_string(),
            default_value: 1.0,
            high_is_good: true,
            stackable: false,
            published: true,
        },
    );
    // Surgical Strike-style skill (3312) boosting turret damageMultiplier (64) by +5%/lvl on modules requiring 3300
    sde.effects.insert(
        9101,
        eve_sde::DogmaEffectType {
            effect_id: 9101,
            name: "turretDmgSkill".into(),
            effect_category: 0,
            modifiers: vec![
                eve_sde::EffectModifierSpec {
                    domain: "itemID".into(),
                    func: "ItemModifier".into(),
                    modified_attribute_id: 292,
                    modifying_attribute_id: 280,
                    operator: 0,
                    group_id: None,
                    skill_type_id: None,
                    effect_id: None,
                },
                eve_sde::EffectModifierSpec {
                    domain: "shipID".into(),
                    func: "LocationRequiredSkillModifier".into(),
                    modified_attribute_id: attr_ids::DAMAGE_MULTIPLIER,
                    modifying_attribute_id: 292,
                    operator: 6,
                    group_id: None,
                    skill_type_id: Some(3300),
                    effect_id: None,
                },
            ],
            ..Default::default()
        },
    );
    sde.types.insert(
        3312,
        make_test_inv_type(
            3312,
            255,
            16,
            "Surgical Strike",
            HashMap::from([(292, 5.0)]),
            vec![9101],
        ),
    );
    // Drone Interfacing skill (3442, effects 146 + 1730) +10%/lvl drone damageMultiplier (64)
    sde.effects.insert(
        146,
        eve_sde::DogmaEffectType {
            effect_id: 146,
            name: "damageMultiplierSkillBonus".into(),
            effect_category: 0,
            modifiers: vec![eve_sde::EffectModifierSpec {
                domain: "itemID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: 292,
                modifying_attribute_id: 280,
                operator: 0,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    sde.effects.insert(
        1730,
        eve_sde::DogmaEffectType {
            effect_id: 1730,
            name: "droneDmgBonus".into(),
            effect_category: 0,
            ..Default::default()
        },
    );
    sde.types.insert(
        3442,
        make_test_inv_type(
            3442,
            273,
            16,
            "Drone Interfacing",
            HashMap::from([(292, 10.0)]),
            vec![146, 1730],
        ),
    );
    // Ballistic Control System (type 22291, effect 9102 PostMul charID attr 212)
    sde.effects.insert(
        9102,
        eve_sde::DogmaEffectType {
            effect_id: 9102,
            name: "bcsCharMult".into(),
            effect_category: 0,
            modifiers: vec![eve_sde::EffectModifierSpec {
                domain: "charID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: 212,
                modifying_attribute_id: 213,
                operator: 4,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    sde.types.insert(
        22291,
        make_test_inv_type(
            22291,
            367,
            7,
            "Ballistic Control System II",
            HashMap::from([(attr_ids::CPU, 10.0), (attr_ids::POWER, 1.0), (213, 1.10)]),
            vec![11, 16, 9102],
        ),
    );
    // Turret requiring 3300, Launcher (group 509), Missile Charge (group 384), Drone requiring 3442
    sde.types.insert(
        90_484,
        make_test_inv_type(
            90_484,
            55,
            7,
            "Test AutoCannon",
            HashMap::from([
                (attr_ids::CPU, 5.0),
                (attr_ids::POWER, 5.0),
                (attr_ids::DAMAGE_MULTIPLIER, 2.0),
                (attr_ids::REQUIRED_SKILL_1, 3300.0),
            ]),
            vec![12, 16],
        ),
    );
    sde.types.insert(
        90_509,
        make_test_inv_type(
            90_509,
            509,
            7,
            "Test Light Missile Launcher",
            HashMap::from([(attr_ids::CPU, 5.0), (attr_ids::POWER, 5.0)]),
            vec![12, 16],
        ),
    );
    sde.types.insert(
        90_384,
        make_test_inv_type(
            90_384,
            384,
            8,
            "Scourge Test Missile",
            HashMap::from([(attr_ids::KINETIC_DAMAGE, 100.0)]),
            Vec::new(),
        ),
    );
    sde.types.insert(
        90_245,
        make_test_inv_type(
            90_245,
            100,
            18,
            "Test Combat Drone",
            HashMap::from([
                (attr_ids::DAMAGE_MULTIPLIER, 1.0),
                (attr_ids::REQUIRED_SKILL_1, 3442.0),
            ]),
            Vec::new(),
        ),
    );
    // Dbuff 26 (Information Burst: Sensor Optimization +25% maxTargetRange attr 76)
    sde.dbuff_collections.insert(
        26,
        eve_sde::DbuffCollectionInfo {
            dbuff_id: 26,
            name: "sensorOpt".into(),
            operation: "PostPercent".into(),
            aggregate_mode: "Maximum".into(),
            display_name_id: None,
            show_output_value_in_ui: None,
            item_modifiers: vec![attr_ids::MAX_TARGET_RANGE],
            location_modifiers: Vec::new(),
            location_group_modifiers: Vec::new(),
            location_required_skill_modifiers: Vec::new(),
            target_attribute_ids: Vec::new(),
        },
    );

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_250_001i64;
    let stranger_id = 96_250_002i64;
    let sys_id = 30_000_142u32;
    let ship_id = 96_250_100i64;
    let turret_id = 96_250_201i64;
    let offline_turret_id = 96_250_202i64;
    let launcher_id = 96_250_203i64;
    let charge_id = 96_250_204i64;
    let bcs_id = 96_250_205i64;
    let drone_id = 96_250_206i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.clone_grade = 2; // Omega so Drone Interfacing V (3442) is not capped by Alpha clone grade
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch.clone());
        ch.character_id = stranger_id;
        snap.characters.insert(stranger_id, ch);

        snap.skills.insert(
            char_id,
            HashMap::from([
                (3312, TrainedSkillRecord { type_id: 3312, level: 4, skill_points: 45_255.0 }),
                (3442, TrainedSkillRecord { type_id: 3442, level: 5, skill_points: 256_000.0 }),
            ]),
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Online Abyssal-rolled turret: rolled base DAMAGE_MULTIPLIER = 2.5, scaled by +20% skill -> 3.0!
        snap.items.insert(
            turret_id,
            InventoryItemRecord {
                item_id: turret_id,
                type_id: 90_484,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(attr_ids::DAMAGE_MULTIPLIER, 2.5)]),
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(turret_id, true);
        // Offline turret: must NOT receive skill bonus and must report isOnline == 0.0
        snap.items.insert(
            offline_turret_id,
            InventoryItemRecord {
                item_id: offline_turret_id,
                type_id: 90_484,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(offline_turret_id, false);
        // Online Launcher + loaded Missile Charge + Online BCS
        snap.items.insert(
            launcher_id,
            InventoryItemRecord {
                item_id: launcher_id,
                type_id: 90_509,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot2,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(launcher_id, true);
        snap.items.insert(
            charge_id,
            InventoryItemRecord {
                item_id: charge_id,
                type_id: 90_384,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot2,
                quantity: 20,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            bcs_id,
            InventoryItemRecord {
                item_id: bcs_id,
                type_id: 22291,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(bcs_id, true);
        // Owned drone in DroneBay
        snap.items.insert(
            drone_id,
            InventoryItemRecord {
                item_id: drone_id,
                type_id: 90_245,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut session = SessionState::default();
    session.user_id = Some(96_250);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);

    async fn query_attr(
        ctx: &ServerContext,
        sess: &mut SessionState,
        item_id: i64,
        aid: u16,
    ) -> Result<PyRep, BridgeRpcError> {
        let args = vec![PyRep::Integer(item_id), PyRep::Integer(aid as i64)];
        handle_dogma_ops(ctx, sess, None, "dogmaIM", "QueryAttributeValue", &args, &HashMap::new())
            .await
            .expect("handled")
    }

    // 1. Fitted online Abyssal turret: 2.5 * 1.20 = 3.0 in both QueryAttributeValue and QueryAllAttributesForItem
    let mod_dmg = query_attr(&ctx, &mut session, turret_id, attr_ids::DAMAGE_MULTIPLIER).await.unwrap();
    assert!((mod_dmg.as_f64().unwrap() - 3.0).abs() < 1e-9);
    let mod_all = handle_dogma_ops(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "QueryAllAttributesForItem",
        &[PyRep::Integer(turret_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    if let PyRep::Dict(entries) = mod_all {
        let map: HashMap<i64, f64> = entries
            .into_iter()
            .filter_map(|(k, v)| Some((k.as_i64()?, v.as_f64()?)))
            .collect();
        assert!((map[&(attr_ids::DAMAGE_MULTIPLIER as i64)] - 3.0).abs() < 1e-9);
        assert_eq!(map.get(&2).copied(), Some(1.0));
    } else {
        panic!("Expected Dict");
    }

    // Negative check: Offline turret gets raw 2.0 (not skill-scaled 2.4) and isOnline == 0.0
    assert!((query_attr(&ctx, &mut session, offline_turret_id, attr_ids::DAMAGE_MULTIPLIER).await.unwrap().as_f64().unwrap() - 2.0).abs() < 1e-9);
    assert_eq!(query_attr(&ctx, &mut session, offline_turret_id, 2).await.unwrap().as_f64(), Some(0.0));

    // Negative check: Unauthorized pilot querying turret_id is rejected with AccessDenied
    let mut stranger_sess = SessionState::default();
    stranger_sess.user_id = Some(96_251);
    stranger_sess.character_id = Some(stranger_id);
    stranger_sess.solar_system_id = Some(sys_id);
    assert_eq!(
        query_attr(&ctx, &mut stranger_sess, turret_id, attr_ids::DAMAGE_MULTIPLIER).await.unwrap_err().code,
        "AccessDenied"
    );

    // 2. Loaded missile charge includes BCS +10% (100.0 * 1.10 = 110.0)
    let chg_kin = query_attr(&ctx, &mut session, charge_id, attr_ids::KINETIC_DAMAGE).await.unwrap();
    assert!((chg_kin.as_f64().unwrap() - 110.0).abs() < 1e-9);

    // 3. Owned drone includes Drone Interfacing V +50% (1.0 * 1.50 = 1.50)
    let drn_dmg = query_attr(&ctx, &mut session, drone_id, attr_ids::DAMAGE_MULTIPLIER).await.unwrap();
    assert!((drn_dmg.as_f64().unwrap() - 1.5).abs() < 1e-9);

    // 4. Fleet Command Burst (`ship_dbuffs` -> `fit.gang_bursts`): +25% maxTargetRange without double-counting,
    //    followed by timed expiry pruning on cached `get_or_rebuild_ship_fitting`!
    let base_target_range = query_attr(&ctx, &mut session, ship_id, attr_ids::MAX_TARGET_RANGE)
        .await
        .unwrap()
        .as_f64()
        .unwrap();
    let now_u = ctx.now_unix_secs();
    {
        let mut st = crate::services::inventory_fitting::fitting_scanners_superweapons::scanners_superweapons_state()
            .write()
            .unwrap();
        st.ship_dbuffs
            .entry((ctx.instance_id, ship_id))
            .or_default()
            .push(crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry {
                dbuff_id: 26,
                value: 25.0,
                expires_at_unix: now_u + 30,
                source_module_id: 777_001,
            });
    }
    let fit_boosted = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    assert!(fit_boosted.gang_bursts.iter().any(|b| b.dbuff_id == Some(26)));
    let boosted_range = query_attr(&ctx, &mut session, ship_id, attr_ids::MAX_TARGET_RANGE)
        .await
        .unwrap()
        .as_f64()
        .unwrap();
    assert!(
        (boosted_range - base_target_range * 1.25).abs() < 1e-6,
        "Fleet burst must boost range once (1.25x), never squared (1.5625x): got {boosted_range} vs base {base_target_range}"
    );
    let ship_info = crate::services::inventory_fitting::fitting_dogma_im::handle_dogma_im(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "ItemGetInfo",
        &[PyRep::Integer(ship_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    if let Some(PyRep::Dict(attrs)) = ship_info.dict_get("attributes") {
        let info_range = attrs
            .iter()
            .find(|(k, _)| k.as_i64() == Some(attr_ids::MAX_TARGET_RANGE as i64))
            .and_then(|(_, v)| v.as_f64())
            .unwrap();
        assert!((info_range - base_target_range * 1.25).abs() < 1e-6);
    } else {
        panic!("Expected attributes Dict in ItemGetInfo");
    }

    // Advance past 30s expiry WITHOUT manual invalidate_ship_fitting -> cached fitting drops expired burst!
    ctx.advance_secs(35);
    let fit_expired = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    assert!(!fit_expired.gang_bursts.iter().any(|b| b.dbuff_id == Some(26)));
    let expired_range = query_attr(&ctx, &mut session, ship_id, attr_ids::MAX_TARGET_RANGE)
        .await
        .unwrap()
        .as_f64()
        .unwrap();
    assert!((expired_range - base_target_range).abs() < 1e-6);
}

#[tokio::test]
async fn triage_target_cap_does_not_exceed_character_target_management_limit() {
    let mut sde = shared_sde().as_ref().clone();
    sde.types.insert(
        4294,
        make_test_inv_type(
            4294,
            515,
            7,
            "Triage Module II",
            HashMap::from([(attr_ids::CPU, 1.0), (attr_ids::POWER, 1.0)]),
            vec![12, 16],
        ),
    );
    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_260_001i64;
    let sys_id = 30_000_142u32;
    let ship_id = 96_260_100i64;
    let triage_id = 96_260_101i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.clone_grade = 2; // Omega so Drone Interfacing V (3442) is not capped by Alpha clone grade
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        // Pilot has Target Management I (max 2 + 1 = 3 locked targets on character!)
        snap.skills.insert(
            char_id,
            HashMap::from([(
                3429,
                TrainedSkillRecord {
                    type_id: 3429,
                    level: 1,
                    skill_points: 250.0,
                },
            )]),
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
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
            triage_id,
            InventoryItemRecord {
                item_id: triage_id,
                type_id: 4294,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(triage_id, true);
        snap.module_active_states.insert(triage_id);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut cs = CombatState::default();
    cs.shield_hp = 500.0;
    cs.max_shield_hp = 500.0;
    cs.structure_hp = 500.0;
    cs.max_structure_hp = 500.0;
    cs.signature_radius = 40.0;
    let _ = sys
        .spawn_ball_with_meta(
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
    for idx in 1..=4i64 {
        let tid = 96_260_200 + idx;
        let _ = sys
            .spawn_ball_with_meta(
                Ball {
                    id: tid,
                    type_id: 587,
                    position: DVec3::new(1_000.0 * (idx as f64), 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs.clone(),
                SlimItemMeta {
                    group_id: 25,
                    category_id: 6,
                    owner_id: 96_260_999,
                    ..Default::default()
                },
            )
            .await;
    }

    let mut sess = SessionState::default();
    sess.user_id = Some(96_260);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    for idx in 1..=3i64 {
        let res = crate::services::inventory_fitting::fitting_dogma_im::handle_dogma_im(
            &ctx,
            &mut sess,
            None,
            "dogmaIM",
            "AddTarget",
            &[PyRep::Integer(96_260_200 + idx)],
            &HashMap::new(),
        )
        .await
        .unwrap();
        assert!(res.is_ok(), "Target {idx} within TM I limit (3) must succeed");
    }
    // 4th target must fail with TargetingFailed (must NOT add +5 triage_bonus above char_max=3!)
    let fourth = crate::services::inventory_fitting::fitting_dogma_im::handle_dogma_im(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(96_260_204)],
        &HashMap::new(),
    )
    .await
    .unwrap();
    assert_eq!(fourth.unwrap_err().code, "TargetingFailed");
}

#[cfg(test)]
#[path = "fitting_dogma_ops_extra_unit_tests.rs"]
mod extra_unit_tests;
