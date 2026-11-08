use super::*;
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_sde::{
    FighterAbilityCharges, FighterAbilityInfo, FighterAbilitySlot, InvType, SdeRegistry,
};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(test)]
#[path = "combat_fighters_control_tests.rs"]
mod combat_fighters_control_tests;

pub(in crate::services) fn make_fighter_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1 && !matches!(flag, InventoryFlag::FighterBay),
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

pub(in crate::services) fn make_fighter_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    attrs: Vec<(u16, f64)>,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: format!("FighterTest-{type_id}"),
        mass: 50_000.0,
        volume,
        capacity: 5_000.0,
        portion_size: 1,
        radius: 25.0,
        base_price: 500_000.0,
        published: true,
        market_group_id: None,
        race_id: Some(1),
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: Some(1),
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes: attrs.into_iter().collect(),
    }
}

pub(in crate::services) fn make_fighter_sde_registry() -> SdeRegistry {
    let mut sde = SdeRegistry::with_canonical_seed();
    // Carrier (Chimera 23915): 3 tubes (attr 2216=3), fighterBayCapacity (2055=50_000), max Light/Support slots = 2 (2217=2, 2218=1, 2219=0)
    sde.types.insert(
        23_915,
        make_fighter_type(
            23_915,
            547,
            6,
            1_000_000.0,
            vec![(2216, 3.0), (2055, 50_000.0), (2217, 2.0), (2218, 1.0), (2219, 0.0)],
        ),
    );
    // Light Fighter (Dragonfly 23057, group 1652, cat 87, vol 1000, maxSquadronSize 2215=9, light attr 2212=1, shield HP 263=1000, mwd 2151=400%)
    sde.types.insert(
        23_057,
        make_fighter_type(
            23_057,
            1652,
            87,
            1_000.0,
            vec![(2215, 9.0), (2212, 1.0), (263, 1_000.0), (2151, 400.0), (2426, 4_000.0)],
        ),
    );
    // Heavy Attack Fighter (Malleus 23061, group 1653, cat 87, vol 1000, heavy attr 2214=1 -> disallowed on carrier with 2219=0!)
    sde.types.insert(
        23_061,
        make_fighter_type(23_061, 1653, 87, 1_000.0, vec![(2215, 6.0), (2214, 1.0), (2154, 100_000.0)]),
    );
    // Non-fighter module item for category rejection check
    sde.types.insert(587, make_fighter_type(587, 25, 6, 10_000.0, vec![(2055, 0.0)]));

    sde.fighter_abilities.insert(
        19,
        FighterAbilityInfo {
            ability_id: 19,
            target_mode: "itemTargeted".into(),
            disallow_in_high_sec: false,
            disallow_in_low_sec: false,
            icon_id: Some(100),
            display_name_id: Some(200),
            ability_name_id: Some(201),
            tooltip_text_id: Some(202),
            turret_graphic_id: Some(203),
            raw_json: serde_json::Value::Null,
        },
    );
    sde.fighter_abilities.insert(
        4,
        FighterAbilityInfo {
            ability_id: 4,
            target_mode: "untargeted".into(),
            disallow_in_high_sec: false,
            disallow_in_low_sec: false,
            icon_id: None,
            display_name_id: None,
            ability_name_id: None,
            tooltip_text_id: None,
            turret_graphic_id: None,
            raw_json: serde_json::Value::Null,
        },
    );
    sde.fighter_abilities.insert(
        5,
        FighterAbilityInfo {
            ability_id: 5,
            target_mode: "untargeted".into(),
            disallow_in_high_sec: false,
            disallow_in_low_sec: false,
            icon_id: None,
            display_name_id: None,
            ability_name_id: None,
            tooltip_text_id: None,
            turret_graphic_id: None,
            raw_json: serde_json::Value::Null,
        },
    );
    sde.types.insert(
        11_370,
        make_fighter_type(11_370, 330, 7, 5.0, vec![]),
    );
    sde.fighter_abilities_by_type.insert(
        23_057,
        vec![
            FighterAbilitySlot {
                slot_index: 0,
                ability_id: 19,
                cooldown_seconds: Some(0.0),
                charges: None,
            },
            FighterAbilitySlot {
                slot_index: 1,
                ability_id: 4,
                cooldown_seconds: Some(20.0),
                charges: None,
            },
            FighterAbilitySlot {
                slot_index: 2,
                ability_id: 19,
                cooldown_seconds: Some(30.0),
                charges: Some(FighterAbilityCharges {
                    charge_count: 6,
                    rearm_time_seconds: 10.0,
                }),
            },
        ],
    );
    sde.types.insert(
        11_370,
        make_fighter_type(11_370, 330, 7, 5.0, vec![]),
    );
    sde.fighter_abilities_by_type.insert(
        23_061,
        vec![FighterAbilitySlot {
            slot_index: 1,
            ability_id: 5,
            cooldown_seconds: Some(45.0),
            charges: None,
        }],
    );
    sde
}

pub(in crate::services) fn make_fighter_sde() -> Arc<SdeRegistry> {
    Arc::new(make_fighter_sde_registry())
}

pub(in crate::services) fn make_fighter_ctx_and_session() -> (ServerContext, SessionState, i64, i64, u32) {
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(make_fighter_sde(), db.clone());
    let char_id = 90_000_001i64;
    let ship_id = 200_000_001i64;
    let sys_id = 30_000_142u32;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_fighter_item(ship_id, 23_915, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.ship_id = Some(ship_id);
    session.solar_system_id = Some(sys_id);
    (ctx, session, char_id, ship_id, sys_id)
}

#[test]
fn fighter_class_and_slot_attr_maps_all_light_support_heavy_and_structure_classes() {
    let sde = make_fighter_sde_registry();
    assert_eq!(fighter_class_and_slot_attr(&sde, 23_057), Some((2212, 2217))); // Light fighter
    assert_eq!(fighter_class_and_slot_attr(&sde, 23_061), Some((2214, 2219))); // Heavy attack fighter
    assert_eq!(fighter_class_and_slot_attr(&sde, 587), None);
}

#[tokio::test]
async fn fighter_tube_load_unload_launch_and_recall_with_reserve_refill() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kwargs = HashMap::new();

    // Seed carrier ship in space
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 120.0,
                inertia: 0.5,
                mass: 1_000_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    // Seed: oversized stack of 12 Dragonflies (item 3001), reserve stack of 4 Dragonflies (3002), heavy fighter (3003), non-fighter (3004)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            3001,
            make_fighter_item(3001, 23_057, char_id, ship_id, InventoryFlag::FighterBay, 12),
        );
        snap.items.insert(
            3002,
            make_fighter_item(3002, 23_057, char_id, ship_id, InventoryFlag::FighterBay, 4),
        );
        snap.items.insert(
            3003,
            make_fighter_item(3003, 23_061, char_id, ship_id, InventoryFlag::FighterBay, 3),
        );
        snap.items.insert(
            3004,
            make_fighter_item(3004, 587, char_id, ship_id, InventoryFlag::Cargo, 1),
        );
    });

    // 1. Rejection checks on LoadFightersToTube: invalid tube 9, heavy fighter class limit, non-fighter item
    let err_tube = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(9), PyRep::Integer(3001)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_tube.code, "InvalidFighterTube");

    let err_heavy = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(0), PyRep::Integer(3003)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_heavy.code, "CannotLoadFighterTubeNoTubesExistForClass");

    let err_non_f = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(0), PyRep::Integer(3004)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_non_f.code, "ItemNotInFighterBay");

    // 2. Load oversized stack 3001 (qty 12 -> max squadron 9 + splits 3 into new FighterBay stack)
    let load_res = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(3001), PyRep::Integer(0)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("load succeeds");
    assert!(matches!(load_res, PyRep::KeyVal(_)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&3001].quantity, 9);
    });

    // Duplicate load into tube 1 fails with FighterAlreadyLoaded
    let dup_err = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(1), PyRep::Integer(3001)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(dup_err.code, "FighterAlreadyLoaded");

    // 3. GetFightersForShip shows tube 0 loaded, in-space empty
    let state_res = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "GetFightersForShip",
        &[],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("GetFightersForShip succeeds");
    let PyRep::Tuple(parts) = state_res else {
        panic!("expected tuple");
    };
    let PyRep::List(tubes) = &parts[0] else {
        panic!("expected tubes list");
    };
    assert_eq!(tubes.len(), 1);

    // 4. Unload tube 0 then re-load it
    let unloaded = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "UnloadTubeToFighterBay",
        &[PyRep::Integer(0)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(unloaded, PyRep::Bool(true));

    let _ = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(0), PyRep::Integer(3001)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();

    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 120.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
        )
        .await;

    // 5. Block launch while cloaked (module active state) -> CannotOpenFighterTubesWhileCloaked, then uncloak and launch
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            990_001,
            make_fighter_item(990_001, 11_370, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.module_active_states.insert(990_001);
    });
    let cloak_err = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(159)])],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(cloak_err.code, "CannotOpenFighterTubesWhileCloaked");
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&990_001);
    });

    let launch_res = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(159)])],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("launch succeeds");
    assert!(matches!(launch_res, PyRep::Dict(_)));

    let fighter_ball_id = ctx
        .active_fighters
        .read()
        .unwrap()
        .keys()
        .next()
        .copied()
        .expect("fighter launched in space");

    // 6. Destroy 3 fighter units in squadron (9 -> 6 units) and spend 1 bomb charge on slot 2
    let (lost, remaining, wiped) = apply_damage_to_fighter_squadron(
        &ctx,
        sys_id,
        fighter_ball_id,
        DamageVector::new(3_000.0, 0.0, 0.0, 0.0),
    )
    .await;
    assert_eq!(lost, 3);
    assert_eq!(remaining, 6);
    assert!(!wiped);
    ctx.fighter_charge_state.write().unwrap().insert((fighter_ball_id, 2), 1);

    // 7. Recall fighter to tube 0 -> refills 3 lost units from reserve stack 3002 (6 + 3 = 9!)
    let recall_res = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "RecallFightersToTubes",
        &[PyRep::List(vec![PyRep::Integer(fighter_ball_id)])],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("recall succeeds");
    assert!(matches!(recall_res, PyRep::List(ref l) if l.len() == 1));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&3001].quantity, 9);
        let reserve_qty: i32 = snap
            .items
            .values()
            .filter(|i| i.location_id == ship_id && i.type_id == 23_057 && i.item_id != 3001)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(reserve_qty, 4); // (3 split + 4 in 3002) - 3 refilled = 4 remaining
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toctou_concurrent_scoop_abandoned_fighter_only_one_wins() {
    let sde = make_fighter_sde_registry();
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);

    let char1 = 91_000_011i64;
    let char2 = 91_000_012i64;
    let ship1 = 91_000_111i64;
    let ship2 = 91_000_112i64;
    let sys_id = 30_000_142u32;
    let fighter_item_id = 91_000_201i64;
    let fighter_ball_id = 91_000_202i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().unwrap().clone();
        for &(cid, sid) in &[(char1, ship1), (char2, ship2)] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.corporation_id = 98_000_001;
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);
            snap.items.insert(sid, make_fighter_item(sid, 23_915, cid, sys_id as i64, InventoryFlag::None, 1));
        }
        snap.items.insert(
            fighter_item_id,
            make_fighter_item(fighter_item_id, 23_057, char1, sys_id as i64, InventoryFlag::None, 5),
        );
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for sid in [ship1, ship2, fighter_ball_id] {
        let tid = if sid == fighter_ball_id { 23_057 } else { 23_915 };
        sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: sid,
                    type_id: tid,
                    position: DVec3::new(100.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 10_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState::default(),
            )
            .await
            .unwrap();
    }
    ctx.active_fighters.write().unwrap().insert(
        fighter_ball_id,
        crate::router::context::ActiveFighterSquadron {
            ball_id: fighter_ball_id,
            item_id: fighter_item_id,
            type_id: 23_057,
            owner_char_id: char1,
            parent_ship_id: ship1,
            tube_index: 0,
            squadron_size: 5,
            target_id: None,
            is_abandoned: true,
            mwd_active: false,
        },
    );

    let mut s1 = SessionState::new(1);
    s1.user_id = Some(1);
    s1.character_id = Some(char1);
    s1.corporation_id = Some(98_000_001);
    s1.solar_system_id = Some(sys_id);
    s1.ship_id = Some(ship1);
    let mut s2 = SessionState::new(2);
    s2.user_id = Some(2);
    s2.character_id = Some(char2);
    s2.corporation_id = Some(98_000_001);
    s2.solar_system_id = Some(sys_id);
    s2.ship_id = Some(ship2);

    let kw = HashMap::new();
    let f_args1 = [PyRep::Integer(fighter_ball_id)];
    let f_args2 = [PyRep::Integer(fighter_ball_id)];
    let (r1, r2) = tokio::join!(
        handle_fighters(&ctx, &mut s1, "fighterMgr", "CmdScoopAbandonedFighterFromSpace", &f_args1, &kw),
        handle_fighters(&ctx, &mut s2, "fighterMgr", "CmdScoopAbandonedFighterFromSpace", &f_args2, &kw),
    );
    let ok1 = r1.as_ref().is_some_and(|r| r.is_ok());
    let ok2 = r2.as_ref().is_some_and(|r| r.is_ok());
    assert!(ok1 ^ ok2, "expected exactly 1 ScoopAbandonedFighter winner, got r1={r1:?} r2={r2:?}");
    let final_loc = ctx.db.with_snapshot(|s| s.items[&fighter_item_id].location_id);
    let expected_winner_ship = if ok1 { ship1 } else { ship2 };
    assert_eq!(final_loc, expected_winner_ship);
}
