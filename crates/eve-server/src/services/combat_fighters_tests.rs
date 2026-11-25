use super::*;
use crate::router::ActiveFighterSquadron;
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

    let fighter_ball_id = fighter_test_helpers::active_fighter_ids(&ctx)
        .into_iter()
        .next()
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
    fighter_test_helpers::set_fighter_ability_charges_used(&ctx, fighter_ball_id, 2, 1);

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
    fighter_test_helpers::insert_active_fighter(
        &ctx,
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

    // Non-abandoned fighter cannot be scooped via CmdScoopAbandonedFighterFromSpace
    let non_aband_ball = 91_000_299i64;
    fighter_test_helpers::insert_active_fighter(
        &ctx,
        non_aband_ball,
        crate::router::context::ActiveFighterSquadron {
            ball_id: non_aband_ball,
            item_id: fighter_item_id,
            type_id: 23_057,
            owner_char_id: char1,
            parent_ship_id: ship1,
            tube_index: 0,
            squadron_size: 5,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );
    assert!(handle_fighters(
        &ctx,
        &mut s1,
        "fighterMgr",
        "CmdScoopAbandonedFighterFromSpace",
        &[PyRep::Integer(non_aband_ball)],
        &kw
    )
    .await
    .unwrap()
    .is_err());
}

#[tokio::test]
async fn fighter_recall_abandon_and_launch_guards_preserve_docked_tube_management() {
    const CODE_SHIP_DOCKED: &str = "ShipDocked";
    const CODE_SHIP_NOT_IN_SPACE: &str = "ShipNotInSpace";
    const CODE_CANNOT_ACTIVATE_CLOAKED: &str = "CannotActivateCloaked";
    const CODE_FIGHTER_NOT_FOUND: &str = "FighterNotFound";
    const CODE_NO_ACTIVE_FIGHTERS: &str = "NoActiveFighters";
    const CODE_INVALID_ARGUMENT: &str = "InvalidArgument";

    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let fid = 930_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fid,
            make_fighter_item(fid, 23_057, char_id, ship_id, InventoryFlag::FighterBay, 9),
        );
    });

    // 1. Docked carrier CAN load & unload tubes, while Launch/Recall/Abandon MUST reject with ShipDocked
    session.station_id = Some(60_003_760);
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(0), PyRep::Integer(fid)],
        &kw,
    )
    .await
    .unwrap()
    .expect("docked LoadFightersToTube allowed");
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "UnloadTubeToFighterBay",
        &[PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .expect("docked UnloadTubeToFighterBay allowed");
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(0), PyRep::Integer(fid)],
        &kw,
    )
    .await
    .unwrap()
    .expect("docked reload tube 0 allowed");

    for (method, args) in [
        ("LaunchFightersFromTubes", vec![PyRep::List(vec![PyRep::Integer(0)])]),
        ("RecallFightersToTubes", vec![]),
        ("CmdAbandonFighter", vec![]),
    ] {
        let err = handle_fighters(&ctx, &mut session, "fighterMgr", method, &args, &kw)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, CODE_SHIP_DOCKED, "{method} must reject station-docked");
    }

    session.station_id = None;
    session.structure_id = Some(1_000_000_000_001);
    for (method, args) in [
        ("LaunchFightersFromTubes", vec![PyRep::List(vec![PyRep::Integer(0)])]),
        ("RecallFightersToTubes", vec![]),
        ("CmdAbandonFighter", vec![]),
    ] {
        let err = handle_fighters(&ctx, &mut session, "fighterMgr", method, &args, &kw)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, CODE_SHIP_DOCKED, "{method} must reject structure-docked");
    }
    session.structure_id = None;

    // 2. Undocked without ship ball in space -> ShipNotInSpace
    for method in ["RecallFightersToTubes", "CmdAbandonFighter"] {
        let err = handle_fighters(&ctx, &mut session, "fighterMgr", method, &[], &kw)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, CODE_SHIP_NOT_IN_SPACE, "{method} must reject missing ship ball");
    }

    // 3. Undock character DB record, spawn carrier ball, and launch fighter squadron into space
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 120.0,
                inertia: 1.0,
                mass: 1_000_000_000.0,
                radius: 1_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    // Empty space before launch: Recall/Abandon reject with NoActiveFighters / FighterNotFound
    assert_eq!(
        handle_fighters(&ctx, &mut session, "fighterMgr", "RecallFightersToTubes", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        CODE_NO_ACTIVE_FIGHTERS
    );
    assert_eq!(
        handle_fighters(&ctx, &mut session, "fighterMgr", "CmdAbandonFighter", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        CODE_NO_ACTIVE_FIGHTERS
    );

    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(0)])],
        &kw,
    )
    .await
    .unwrap()
    .expect("launch tube 0 succeeds");
    let fball = fighter_test_helpers::find_active_fighter_by_item(&ctx, fid)
        .unwrap()
        .ball_id;

    // 4. Actively cloaked (CombatState and active group-330 module) blocks Recall/Abandon with zero mutation
    let _ = sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.is_cloaked = true;
        cs.gate_cloak_remaining_ticks = 0;
    });
    for (method, args) in [
        ("RecallFightersToTubes", vec![PyRep::List(vec![PyRep::Integer(fball)])]),
        ("CmdAbandonFighter", vec![PyRep::List(vec![PyRep::Integer(fball)])]),
    ] {
        let err = handle_fighters(&ctx, &mut session, "fighterMgr", method, &args, &kw)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, CODE_CANNOT_ACTIVATE_CLOAKED, "{method} must reject active cloak");
    }
    let _ = sys_handle.try_update_combat_state(ship_id, |cs| cs.is_cloaked = false);

    let cloak_mod = 930_099i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_mod,
            make_fighter_item(cloak_mod, 11_370, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.module_active_states.insert(cloak_mod);
    });
    for method in ["RecallFightersToTubes", "CmdAbandonFighter"] {
        let err = handle_fighters(&ctx, &mut session, "fighterMgr", method, &[], &kw)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, CODE_CANNOT_ACTIVATE_CLOAKED, "{method} must reject active cloak module");
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_mod);
        snap.items.remove(&cloak_mod);
    });

    // 5. Invalid/non-matching fighter IDs and empty lists reject without mutating launched fighter
    for method in ["RecallFightersToTubes", "CmdAbandonFighter"] {
        let err_missing = handle_fighters(
            &ctx,
            &mut session,
            "fighterMgr",
            method,
            &[PyRep::Integer(999_888_777)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err_missing.code, CODE_FIGHTER_NOT_FOUND);

        let err_empty = handle_fighters(
            &ctx,
            &mut session,
            "fighterMgr",
            method,
            &[PyRep::List(vec![])],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err_empty.code, CODE_INVALID_ARGUMENT);
    }
    assert!(!fighter_test_helpers::active_fighter(&ctx, fball).unwrap().is_abandoned);

    // Non-owner in space calling CmdAbandonFighter with another pilot's active fighter is a safe no-op returning []
    let other_char = 930_009i64;
    let other_ship = 930_090i64;
    let mut other_session = session.clone();
    other_session.character_id = Some(other_char);
    other_session.ship_id = Some(other_ship);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: other_ship,
                type_id: 23757,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 120.0,
                inertia: 1.0,
                mass: 1_000_000_000.0,
                radius: 1_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    let other_ab_res = handle_fighters(
        &ctx,
        &mut other_session,
        "fighterMgr",
        "CmdAbandonFighter",
        &[PyRep::Integer(fball)],
        &kw,
    )
    .await
    .unwrap()
    .expect("non-owner CmdAbandonFighter returns empty list");
    assert_eq!(other_ab_res, PyRep::List(Vec::new()));
    assert!(!fighter_test_helpers::active_fighter(&ctx, fball).unwrap().is_abandoned);

    // 6. Valid List-arg CmdAbandonFighter succeeds once, then rejects duplicate abandon as FighterNotFound
    let ab_res = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "CmdAbandonFighter",
        &[PyRep::List(vec![PyRep::Integer(fball)])],
        &kw,
    )
    .await
    .unwrap()
    .expect("CmdAbandonFighter succeeds");
    assert_eq!(ab_res, PyRep::List(vec![PyRep::Integer(fball)]));
    assert!(fighter_test_helpers::active_fighter(&ctx, fball).unwrap().is_abandoned);

    let err_dup = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "CmdAbandonFighter",
        &[PyRep::Integer(fball)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_dup.code, CODE_FIGHTER_NOT_FOUND);
}

#[tokio::test]
async fn fighter_r2_db_dock_structure_control_cloak_launch_and_movement_scoop_guards() {
    const CODE_SHIP_DOCKED: &str = "ShipDocked";
    const CODE_CANNOT_LAUNCH_CLOAKED: &str = "CannotOpenFighterTubesWhileCloaked";
    const CODE_CANNOT_ACTIVATE_CLOAKED: &str = "CannotActivateCloaked";

    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let station_id = ctx.sde.stations.keys().next().copied().unwrap_or(60_003_760);
    let other_struct_id = 1_000_931_999i64;
    let f_tube_id = 931_001i64;
    let ab_f_id = 931_002i64;
    let no_cs_carrier = 200_931_003i64;
    let no_cs_fighter = 931_003i64;
    let cloak_mod_id = 931_004i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            f_tube_id,
            make_fighter_item(f_tube_id, 23_057, char_id, ship_id, InventoryFlag::FighterBay, 9),
        );
        snap.items.insert(
            ab_f_id,
            make_fighter_item(ab_f_id, 23_057, char_id, sys_id as i64, InventoryFlag::None, 9),
        );
        snap.items.insert(
            no_cs_carrier,
            make_fighter_item(no_cs_carrier, 23_915, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            no_cs_fighter,
            make_fighter_item(no_cs_fighter, 23_057, char_id, no_cs_carrier, InventoryFlag::FighterBay, 9),
        );
    });
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(0), PyRep::Integer(f_tube_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("LoadFightersToTube succeeds");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 120.0,
                inertia: 1.0,
                mass: 1_000_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    // Seed an active fighter and an abandoned fighter for movement / scoop guard verification
    let active_f = 931_010i64;
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: active_f,
                type_id: 23_057,
                position: DVec3::new(300.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 1_000.0,
                inertia: 0.5,
                mass: 50_000.0,
                radius: 25.0,
                speed_fraction: 0.0,
                mode: MovementMode::Follow {
                    target_id: ship_id,
                    range: 500.0,
                },
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ab_f_id,
                type_id: 23_057,
                position: DVec3::new(200.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 1_000.0,
                inertia: 0.5,
                mass: 50_000.0,
                radius: 25.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    fighter_test_helpers::insert_active_fighter(
        &ctx,
        active_f,
        ActiveFighterSquadron {
            ball_id: active_f,
            item_id: active_f,
            type_id: 23_057,
            owner_char_id: char_id,
            parent_ship_id: ship_id,
            tube_index: 1,
            squadron_size: 9,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );
    fighter_test_helpers::insert_active_fighter(
        &ctx,
        ab_f_id,
        ActiveFighterSquadron {
            ball_id: ab_f_id,
            item_id: ab_f_id,
            type_id: 23_057,
            owner_char_id: char_id,
            parent_ship_id: 0,
            tube_index: 0,
            squadron_size: 9,
            target_id: None,
            is_abandoned: true,
            mwd_active: false,
        },
    );

    let guarded_cmds: &[(&str, Vec<PyRep>)] = &[
        ("LaunchFightersFromTubes", vec![PyRep::List(vec![PyRep::Integer(0)])]),
        ("RecallFightersToTubes", vec![PyRep::List(vec![PyRep::Integer(active_f)])]),
        ("CmdAbandonFighter", vec![PyRep::List(vec![PyRep::Integer(active_f)])]),
        (
            "ExecuteMovementCommandOnFighters",
            vec![PyRep::List(vec![PyRep::Integer(active_f)]), PyRep::String("STOP".into())],
        ),
        ("CmdScoopAbandonedFighterFromSpace", vec![PyRep::Integer(ab_f_id)]),
    ];

    // 1. DB station-docked rejects even with stale undocked session & lingering ship ball
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(station_id);
    });
    for (method, args) in guarded_cmds {
        let err = handle_fighters(&ctx, &mut session, "fighterMgr", method, args, &kw)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, CODE_SHIP_DOCKED, "{method} must reject DB station-docked");
    }

    // Even if session claims structure control over ship_id, DB station_id STILL rejects ShipDocked
    session.structure_id = Some(ship_id);
    let err_db_station_vs_sess_struct = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(0)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_db_station_vs_sess_struct.code, CODE_SHIP_DOCKED);
    session.structure_id = None;

    // 2. DB structure-docked in another structure rejects ShipDocked even with undocked session
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = Some(other_struct_id);
    });
    for (method, args) in guarded_cmds {
        let err = handle_fighters(&ctx, &mut session, "fighterMgr", method, args, &kw)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, CODE_SHIP_DOCKED, "{method} must reject DB structure-docked");
    }

    // 3. Controlling own structure (structure_id == Some(ship_id)) is ALLOWED, whereas docked in other_struct_id rejects
    session.structure_id = Some(other_struct_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().structure_id = Some(ship_id);
    });
    let err_sess_other_struct = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(0)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_sess_other_struct.code, CODE_SHIP_DOCKED);

    session.structure_id = Some(ship_id);
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(0)])],
        &kw,
    )
    .await
    .unwrap()
    .expect("structure controller (structure_id == Some(ship_id)) may launch fighters");
    session.structure_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().structure_id = None;
    });

    // 4. ExecuteMovementCommandOnFighters and CmdScoopAbandonedFighterFromSpace reject cloaked without mutating state
    let _ = sys_handle.try_update_combat_state(ship_id, |cs| cs.is_cloaked = true);
    let err_move_cloaked = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[PyRep::List(vec![PyRep::Integer(active_f)]), PyRep::String("STOP".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_move_cloaked.code, CODE_CANNOT_ACTIVATE_CLOAKED);
    assert!(matches!(
        sys_handle.get_ball_snapshot(active_f).await.unwrap().unwrap().1.mode,
        MovementMode::Follow { .. }
    ));

    let err_scoop_cloaked = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "CmdScoopAbandonedFighterFromSpace",
        &[PyRep::Integer(ab_f_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_scoop_cloaked.code, CODE_CANNOT_ACTIVATE_CLOAKED);
    assert!(fighter_test_helpers::has_active_fighter(&ctx, ab_f_id));

    // 5. LaunchFightersFromTubes rejects cloak_velocity_multiplier.is_some() (even if is_cloaked == false) without tether mutation
    let _ = sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.is_cloaked = false;
        cs.cloak_velocity_multiplier = Some(0.25);
        cs.is_tethered = false;
    });
    let err_mult_cloak = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(1)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_mult_cloak.code, CODE_CANNOT_LAUNCH_CLOAKED);
    assert!(!sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);
    let _ = sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.cloak_velocity_multiplier = None;
    });

    // Uncloaked movement and scoop now succeed cleanly
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[PyRep::List(vec![PyRep::Integer(active_f)]), PyRep::String("STOP".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("uncloaked ExecuteMovementCommandOnFighters succeeds");
    assert!(matches!(
        sys_handle.get_ball_snapshot(active_f).await.unwrap().unwrap().1.mode,
        MovementMode::Stop
    ));
    handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "CmdScoopAbandonedFighterFromSpace",
        &[PyRep::Integer(ab_f_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("uncloaked CmdScoopAbandonedFighterFromSpace succeeds");
    assert!(!fighter_test_helpers::has_active_fighter(&ctx, ab_f_id));

    // 6. LaunchFightersFromTubes rejects active DB cloak module even when ship ball has NO CombatState (Ok(None))
    let mut no_cs_sess = session.clone();
    no_cs_sess.ship_id = Some(no_cs_carrier);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = no_cs_carrier;
        snap.items.insert(
            cloak_mod_id,
            make_fighter_item(cloak_mod_id, 11_370, char_id, no_cs_carrier, InventoryFlag::HiSlot0, 1),
        );
        snap.module_active_states.insert(cloak_mod_id);
    });
    handle_fighters(
        &ctx,
        &mut no_cs_sess,
        "fighterMgr",
        "LoadFightersToTube",
        &[PyRep::Integer(0), PyRep::Integer(no_cs_fighter)],
        &kw,
    )
    .await
    .unwrap()
    .expect("LoadFightersToTube succeeds on second carrier");
    sys_handle
        .spawn_ball(Ball {
            id: no_cs_carrier,
            type_id: 23_915,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 120.0,
            inertia: 1.0,
            mass: 1_000_000_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    let pre_cs = sys_handle.get_combat_state(no_cs_carrier).await.unwrap().unwrap();
    assert!(!pre_cs.is_cloaked && pre_cs.cloak_velocity_multiplier.is_none());
    let err_no_cs_db_cloak = handle_fighters(
        &ctx,
        &mut no_cs_sess,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(0)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_no_cs_db_cloak.code, CODE_CANNOT_LAUNCH_CLOAKED);
    assert!(!sys_handle.get_combat_state(no_cs_carrier).await.unwrap().unwrap().is_tethered);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_fighter_ability_activations_and_death_never_double_spend_or_resurrect() {
    let (raw_ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let ctx = Arc::new(raw_ctx);
    let kw = HashMap::new();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
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
            CombatState::default(),
        )
        .await
        .unwrap();
    let fid = 720_001i64;
    let target_id = 720_999i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(fid, make_fighter_item(fid, 23_057, char_id, ship_id, InventoryFlag::FighterBay, 9));
    });
    handle_fighters(&ctx, &mut session, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fid)], &kw)
        .await
        .unwrap()
        .unwrap();
    handle_fighters(&ctx, &mut session, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw)
        .await
        .unwrap()
        .unwrap();

    let fball = fighter_test_helpers::find_active_fighter_by_item(&ctx, fid).unwrap().ball_id;
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: target_id,
                type_id: 587,
                position: DVec3::new(1_200.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 10_000.0,
                max_shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                max_armor_hp: 10_000.0,
                structure_hp: 10_000.0,
                max_structure_hp: 10_000.0,
                signature_radius: 40.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
    sys_handle
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap();

    // 1. Seed 5/6 used charges on slot 2 -> concurrent dual CmdActivateAbilitySlots allows strictly one activation
    fighter_test_helpers::set_fighter_ability_charges_used(&ctx, fball, 2, 5);
    let mut s1 = session.clone();
    let mut s2 = session.clone();
    let act_args1 = [PyRep::List(vec![PyRep::Integer(fball)]), PyRep::Integer(2), PyRep::Integer(target_id)];
    let act_args2 = [PyRep::List(vec![PyRep::Integer(fball)]), PyRep::Integer(2), PyRep::Integer(target_id)];
    let (r1, r2) = tokio::join!(
        handle_fighters(
            &ctx,
            &mut s1,
            "fighterMgr",
            "CmdActivateAbilitySlots",
            &act_args1,
            &kw,
        ),
        handle_fighters(
            &ctx,
            &mut s2,
            "fighterMgr",
            "CmdActivateAbilitySlots",
            &act_args2,
            &kw,
        ),
    );
    let ok_count = [r1.unwrap(), r2.unwrap()].iter().filter(|r| r.is_ok()).count();
    assert_eq!(ok_count, 1, "strictly one concurrent activation on 1-charge slot may succeed");
    assert_eq!(fighter_test_helpers::fighter_ability_charges_used(&ctx, fball, 2), Some(6));

    // 2. Concurrent multi-task damage hits via tokio::spawn split safely without double-counting
    let dmg_barrier = Arc::new(tokio::sync::Barrier::new(2));
    let ctx_a = Arc::clone(&ctx);
    let b_a = Arc::clone(&dmg_barrier);
    let t_a = tokio::spawn(async move {
        b_a.wait().await;
        apply_damage_to_fighter_squadron(&ctx_a, sys_id, fball, DamageVector::new(2_000.0, 0.0, 0.0, 0.0)).await
    });
    let ctx_b = Arc::clone(&ctx);
    let b_b = Arc::clone(&dmg_barrier);
    let t_b = tokio::spawn(async move {
        b_b.wait().await;
        apply_damage_to_fighter_squadron(&ctx_b, sys_id, fball, DamageVector::new(3_000.0, 0.0, 0.0, 0.0)).await
    });
    let (r1, r2) = tokio::join!(t_a, t_b);
    let (l1, l2) = (r1.unwrap().0, r2.unwrap().0);
    assert_eq!(l1 + l2, 5);
    assert_eq!(fighter_test_helpers::active_fighter(&ctx, fball).unwrap().squadron_size, 4);
    assert_eq!(ctx.db.get_item(fid).unwrap().quantity, 4);

    let (_, _, wiped) = apply_damage_to_fighter_squadron(
        &ctx,
        sys_id,
        fball,
        DamageVector::new(10_000.0, 0.0, 0.0, 0.0),
    )
    .await;
    assert!(wiped);
    assert!(sys_handle.get_ball_snapshot(fball).await.unwrap().is_none());

    let err_mwd = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "CmdActivateAbilitySlots",
        &[PyRep::List(vec![PyRep::Integer(fball)]), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_mwd.code, "FighterNotFound");
    assert!(sys_handle.get_ball_snapshot(fball).await.unwrap().is_none());

    let err_move_mwd = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[PyRep::List(vec![PyRep::Integer(fball)]), PyRep::String("MWD".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_move_mwd.code, "FighterNotFound");
    assert!(sys_handle.get_ball_snapshot(fball).await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fighter_mjd_batch_prevalidates_atomically_rolls_back_errors_and_multitask_damage_exact() {
    let (raw_ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let ctx = Arc::new(raw_ctx);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let kw = HashMap::new();

    let heavy_ok = 620_101i64;
    let heavy_ok_item = 520_101i64;
    let heavy_cd = 620_102i64;
    let heavy_cd_item = 520_102i64;
    let light_no_mjd = 620_103i64;
    let light_no_mjd_item = 520_103i64;
    let dmg_ball = 620_104i64;
    let dmg_item = 520_104i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(heavy_ok_item, make_fighter_item(heavy_ok_item, 23_061, char_id, sys_id as i64, InventoryFlag::None, 6));
        snap.items.insert(heavy_cd_item, make_fighter_item(heavy_cd_item, 23_061, char_id, sys_id as i64, InventoryFlag::None, 6));
        snap.items.insert(light_no_mjd_item, make_fighter_item(light_no_mjd_item, 23_057, char_id, sys_id as i64, InventoryFlag::None, 9));
        snap.items.insert(dmg_item, make_fighter_item(dmg_item, 23_057, char_id, sys_id as i64, InventoryFlag::None, 9));
    });

    for (bid, tid, pos, radius) in [
        (ship_id, 23_915u32, DVec3::ZERO, 500.0),
        (heavy_ok, 23_061u32, DVec3::new(1_000.0, 0.0, 0.0), 25.0),
        (heavy_cd, 23_061u32, DVec3::new(2_000.0, 0.0, 0.0), 25.0),
        (light_no_mjd, 23_057u32, DVec3::new(3_000.0, 0.0, 0.0), 25.0),
        (dmg_ball, 23_057u32, DVec3::new(4_000.0, 0.0, 0.0), 25.0),
    ] {
        sys_handle
            .spawn_ball(
                Ball {
                    id: bid,
                    type_id: tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 1_500.0,
                    inertia: 1.0,
                    mass: 25_000.0,
                    radius,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
            )
            .await
            .unwrap();
    }

    for (bid, iid, tid, sz, tube) in [
        (heavy_ok, heavy_ok_item, 23_061u32, 6u32, 0usize),
        (heavy_cd, heavy_cd_item, 23_061u32, 6u32, 1usize),
        (light_no_mjd, light_no_mjd_item, 23_057u32, 9u32, 2usize),
        (dmg_ball, dmg_item, 23_057u32, 9u32, 3usize),
    ] {
        fighter_test_helpers::insert_active_fighter(
            &ctx,
            bid,
            ActiveFighterSquadron {
                ball_id: bid,
                item_id: iid,
                type_id: tid,
                owner_char_id: char_id,
                parent_ship_id: ship_id,
                tube_index: tube,
                squadron_size: sz,
                target_id: None,
                is_abandoned: false,
                mwd_active: false,
            },
        );
    }

    // 1. Multi-task concurrent damage: 10 tokio::spawn hits x 500 dmg (5000 total / 1000 unit HP) kills strictly 5 units
    let barrier = Arc::new(tokio::sync::Barrier::new(10));
    let mut handles = Vec::with_capacity(10);
    for _ in 0..10 {
        let c = Arc::clone(&ctx);
        let b = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            b.wait().await;
            apply_damage_to_fighter_squadron(&c, sys_id, dmg_ball, DamageVector::new(500.0, 0.0, 0.0, 0.0)).await
        }));
    }
    let mut total_lost = 0u32;
    for h in handles {
        total_lost += h.await.unwrap().0;
    }
    assert_eq!(total_lost, 5);
    assert_eq!(fighter_test_helpers::active_fighter(&ctx, dmg_ball).unwrap().squadron_size, 4);
    assert_eq!(ctx.db.get_item(dmg_item).unwrap().quantity, 4);
    let (lost_wipe, _, wiped) = apply_damage_to_fighter_squadron(
        &ctx,
        sys_id,
        dmg_ball,
        DamageVector::new(10_000.0, 0.0, 0.0, 0.0),
    )
    .await;
    assert!(wiped && lost_wipe == 4);
    ctx.db.with_snapshot(|snap| {
        assert!(snap
            .combat_crimewatch_state
            .is_recently_destroyed_item_id(dmg_ball));
        assert!(snap
            .combat_crimewatch_state
            .is_recently_destroyed_item_id(dmg_item));
    });

    // 2. Batch "MJD" pre-validates FighterLacksMjdAbility without jumping heavy_ok or setting cooldown
    let err_lack = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[
            PyRep::List(vec![PyRep::Integer(heavy_ok), PyRep::Integer(light_no_mjd)]),
            PyRep::String("MJD".into()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_lack.code, "FighterLacksMjdAbility");
    assert_eq!(sys_handle.get_ball_snapshot(heavy_ok).await.unwrap().unwrap().1.position, DVec3::new(1_000.0, 0.0, 0.0));
    assert!(fighter_test_helpers::fighter_ability_cooldown(&ctx, heavy_ok, 1).is_none());

    // 3. Batch "MJD" pre-validates AbilityOnCooldown without jumping heavy_ok or setting cooldown
    fighter_test_helpers::set_fighter_ability_cooldown(&ctx, heavy_cd, 1, ctx.now_unix() + 120);
    let err_cd = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[
            PyRep::List(vec![PyRep::Integer(heavy_ok), PyRep::Integer(heavy_cd)]),
            PyRep::String("MJD".into()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_cd.code, "AbilityOnCooldown");
    assert_eq!(sys_handle.get_ball_snapshot(heavy_ok).await.unwrap().unwrap().1.position, DVec3::new(1_000.0, 0.0, 0.0));
    assert!(fighter_test_helpers::fighter_ability_cooldown(&ctx, heavy_ok, 1).is_none());

    // 4. Failed micro_jump (warp-scrambled fighter) in CmdActivateAbilitySlots and "MJD" propagates MicroJumpFailed and rolls back cooldown
    sys_handle
        .set_combat_state(heavy_ok, CombatState::default())
        .await
        .unwrap();
    sys_handle
        .add_module_cycle(crate::system_actor::ActiveModuleCycle {
            ship_id,
            module_id: 99_901,
            effect_name: "warp_scramble".to_string(),
            target_id: Some(heavy_ok),
            repeat: 1_000,
            duration_s: 10.0,
            warp_scramble_points: 2,
            disables_target_mwd_mjd: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let err_slot_mjd = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "CmdActivateAbilitySlots",
        &[PyRep::List(vec![PyRep::Integer(heavy_ok)]), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_slot_mjd.code, "MicroJumpFailed");
    assert!(fighter_test_helpers::fighter_ability_cooldown(&ctx, heavy_ok, 1).is_none());

    let err_move_mjd = handle_fighters(
        &ctx,
        &mut session,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[PyRep::List(vec![PyRep::Integer(heavy_ok)]), PyRep::String("MJD".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_move_mjd.code, "MicroJumpFailed");
    assert!(fighter_test_helpers::fighter_ability_cooldown(&ctx, heavy_ok, 1).is_none());
}
