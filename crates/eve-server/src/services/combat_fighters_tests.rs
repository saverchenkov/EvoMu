use super::*;
use crate::router::ActiveFighterSquadron;
use crate::system_actor::fighter_class_and_slot_attr;
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
    // Support Fighter (Dromi 23063, group 1537, cat 87, support attr 2213=1 -> Chimera 2218=1 allows strictly 1)
    sde.types.insert(
        23_063,
        make_fighter_type(23_063, 1537, 87, 1_000.0, vec![(2215, 3.0), (2213, 1.0)]),
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

async fn spawn_fighter_test_ball(
    sys_handle: &crate::system_actor::SystemHandle,
    id: i64,
    type_id: u32,
    position: DVec3,
    combat: Option<CombatState>,
) {
    let ball = Ball {
        id,
        type_id,
        position,
        velocity: DVec3::ZERO,
        max_velocity: 500.0,
        inertia: 1.0,
        mass: 25_000.0,
        radius: if type_id == 23_915 { 500.0 } else { 25.0 },
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    if let Some(cs) = combat {
        sys_handle.spawn_ball_with_combat(ball, cs).await.unwrap();
    } else {
        sys_handle.spawn_ball(ball).await.unwrap();
    }
}

fn make_test_fighter_sq(
    ball_id: i64,
    item_id: i64,
    type_id: u32,
    owner_char_id: i64,
    parent_ship_id: i64,
    tube_index: usize,
    is_abandoned: bool,
) -> ActiveFighterSquadron {
    ActiveFighterSquadron {
        ball_id,
        item_id,
        type_id,
        owner_char_id,
        parent_ship_id,
        tube_index,
        squadron_size: 9,
        target_id: None,
        is_abandoned,
        mwd_active: false,
    }
}

#[tokio::test]
async fn r2_f01_destroyed_launched_fighter_ball_ne_item_removed_from_db_and_no_resurrection() {
    let dir = std::env::temp_dir().join(format!("evomu_r2_f01_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create r2_f01 temp dir");
    let (db_path, sde, sys_id, char_id, ship_id, attacker_id) = (dir.join("f01_fighters.sqlite"), make_fighter_sde(), 30_000_142u32, 90_000_001i64, 200_000_001i64, 200_000_099i64);
    let ((f_ball_turret, f_item_turret), (f_ball_missile, f_item_missile)) = ((630_101i64, 530_101i64), (630_102i64, 530_102i64));
    {
        let db = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("open f01 sqlite"));
        db.with_mut_snapshot(|snap| {
            snap.system_states.insert(sys_id, eve_db::SystemDynamicState { solar_system_id: sys_id, ..Default::default() });
            for (iid, tid, owner, qty) in [(ship_id, 23_915u32, char_id, 1), (attacker_id, 587, 90_000_002, 1), (f_item_turret, 23_057, char_id, 9), (f_item_missile, 23_057, char_id, 9)] {
                snap.items.insert(iid, make_fighter_item(iid, tid, owner, sys_id as i64, InventoryFlag::None, qty));
            }
        });
        let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let low_hp = CombatState { shield_hp: 1.0, max_shield_hp: 1.0, armor_hp: 1.0, max_armor_hp: 1.0, structure_hp: 10.0, max_structure_hp: 10.0, shield_resonance: [1.0; 4], armor_resonance: [1.0; 4], structure_resonance: [1.0; 4], capacitor: 50_000.0, max_capacitor: 50_000.0, signature_radius: 250.0, ..CombatState::default() };
        for (bid, tid, pos) in [(ship_id, 23_915u32, DVec3::new(-5_000.0, 0.0, 0.0)), (attacker_id, 587u32, DVec3::ZERO), (f_ball_turret, 23_057u32, DVec3::new(1_500.0, 0.0, 0.0)), (f_ball_missile, 23_057u32, DVec3::new(150.0, 0.0, 0.0))] {
            spawn_fighter_test_ball(&sys_handle, bid, tid, pos, Some(low_hp.clone())).await;
        }
        for (bid, iid, tube) in [(f_ball_turret, f_item_turret, 0usize), (f_ball_missile, f_item_missile, 1usize)] {
            fighter_test_helpers::insert_active_fighter_in_system(&ctx, sys_id, bid, make_test_fighter_sq(bid, iid, 23_057, char_id, ship_id, tube, false));
            fighter_test_helpers::set_fighter_tube_load_in_system(&ctx, sys_id, ship_id, tube, iid);
        }
        let _ = sys_handle.flush_fighter_checkpoint().await;

        let turret_ball = sys_handle.get_ball_snapshot(f_ball_turret).await.unwrap().unwrap().1;
        let _ = crate::services::combat_weapons_spool::handle_ship_or_npc_destruction(&ctx, &sys_handle, 90_000_002, attacker_id, 587, 0, f_ball_turret, &turret_ball, None, sys_id, 5_000.0, ctx.now_unix()).await;
        assert!(ctx.db.get_item(f_item_turret).is_none() && fighter_test_helpers::active_fighter(&ctx, f_ball_turret).is_none());
        ctx.db.with_snapshot(|snap| assert!(snap.combat_crimewatch_state.is_recently_destroyed_item_id(f_item_turret) && snap.combat_crimewatch_state.is_recently_destroyed_item_id(f_ball_turret)));

        sys_handle.add_module_cycle(crate::system_actor::ActiveModuleCycle {
            ship_id: attacker_id, module_id: 99_905, owner_char_id: Some(90_000_002), effect_name: "missile_launching".into(),
            target_id: Some(f_ball_missile), repeat: 1, duration_s: 1.0, elapsed_s: 0.99, weapon_is_guided_missile: true,
            missile_charge_type_id: Some(215), missile_max_velocity_ms: 5_000.0, missile_flight_time_s: 5.0, missile_ball_radius: 100.0,
            missile_explosion_radius_m: 1_000.0, missile_explosion_velocity_ms: 1_000.0, missile_drf: 1.0, damage_vector: DamageVector::new(10_000.0, 0.0, 0.0, 0.0), ..Default::default()
        }).await.unwrap();
        let mut saw_missile_destroy = false;
        for _ in 0..8 {
            if sys_handle.step_with_output(1.0).await.unwrap().destroyed_balls.iter().any(|&(bid, _)| bid == f_ball_missile) { saw_missile_destroy = true; break; }
        }
        assert!(saw_missile_destroy && ctx.db.get_item(f_item_missile).is_none() && fighter_test_helpers::active_fighter(&ctx, f_ball_missile).is_none());
        ctx.db.with_snapshot(|snap| assert!(snap.combat_crimewatch_state.is_recently_destroyed_item_id(f_item_missile) && snap.combat_crimewatch_state.is_recently_destroyed_item_id(f_ball_missile)));
    }
    let ctx2 = ServerContext::with_sde_and_repository(sde, Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("reopen f01 sqlite")));
    let _ = ctx2.systems.get_or_spawn(sys_id);
    assert!(ctx2.db.get_item(f_item_turret).is_none() && ctx2.db.get_item(f_item_missile).is_none());
    assert!(fighter_test_helpers::active_fighter(&ctx2, f_ball_turret).is_none() && fighter_test_helpers::active_fighter(&ctx2, f_ball_missile).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn r2_f03_failed_scoop_preserves_cooldowns_charges_and_damage_accum() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let (sys_handle, kw, now, (f_ball, f_item)) = (ctx.systems.get_or_spawn(sys_id), HashMap::new(), ctx.now_unix(), (630_201i64, 530_201i64));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&ship_id) { s.type_id = 587; }
        snap.items.insert(f_item, make_fighter_item(f_item, 23_057, char_id, sys_id as i64, InventoryFlag::None, 6));
    });
    spawn_fighter_test_ball(&sys_handle, ship_id, 587, DVec3::ZERO, None).await;
    spawn_fighter_test_ball(&sys_handle, f_ball, 23_057, DVec3::new(50_000.0, 0.0, 0.0), None).await;
    fighter_test_helpers::insert_active_fighter(&ctx, f_ball, make_test_fighter_sq(f_ball, f_item, 23_057, char_id, 0, 0, true));
    fighter_test_helpers::set_fighter_ability_cooldown(&ctx, f_ball, 0, now + 120);
    fighter_test_helpers::set_fighter_ability_charges_used(&ctx, f_ball, 2, 3);
    fighter_test_helpers::set_fighter_damage_accum(&ctx, f_ball, 275.5);
    let assert_preserved = || assert!(fighter_test_helpers::active_fighter(&ctx, f_ball).is_some()
        && fighter_test_helpers::fighter_ability_cooldown(&ctx, f_ball, 0) == Some(now + 120)
        && fighter_test_helpers::fighter_ability_charges_used(&ctx, f_ball, 2) == Some(3)
        && fighter_test_helpers::fighter_damage_accum(&ctx, f_ball) == Some(275.5));
    assert_eq!(handle_fighters(&ctx, &mut session, "fighterMgr", "CmdScoopAbandonedFighterFromSpace", &[PyRep::Integer(f_ball)], &kw).await.unwrap().unwrap_err().code, "FighterOutOfRange");
    assert_preserved();
    let _ = sys_handle.remove_ball(ship_id).await;
    spawn_fighter_test_ball(&sys_handle, ship_id, 587, DVec3::new(49_000.0, 0.0, 0.0), None).await;
    assert_eq!(handle_fighters(&ctx, &mut session, "fighterMgr", "CmdScoopAbandonedFighterFromSpace", &[PyRep::Integer(f_ball)], &kw).await.unwrap().unwrap_err().code, "NoFighterBay");
    assert_preserved();
    ctx.db.with_mut_snapshot(|snap| { if let Some(s) = snap.items.get_mut(&ship_id) { s.type_id = 23_915; } });
    assert_eq!(handle_fighters(&ctx, &mut session, "fighterMgr", "CmdScoopAbandonedFighterFromSpace", &[PyRep::Integer(f_ball)], &kw).await.unwrap().unwrap(), PyRep::Bool(true));
    assert!(fighter_test_helpers::active_fighter(&ctx, f_ball).is_none()
        && fighter_test_helpers::fighter_ability_cooldown(&ctx, f_ball, 0).is_none()
        && fighter_test_helpers::fighter_ability_charges_used(&ctx, f_ball, 2).is_none()
        && fighter_test_helpers::fighter_damage_accum(&ctx, f_ball).is_none());
    assert_eq!((ctx.db.get_item(f_item).unwrap().location_id, ctx.db.get_item(f_item).unwrap().flag), (ship_id, InventoryFlag::FighterBay));
}

#[tokio::test]
async fn r2_f06_mjd_rejects_abandoned_or_missing_without_mutating_cooldowns() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let (sys_handle, kw, (f_aband, f_missing)) = (ctx.systems.get_or_spawn(sys_id), HashMap::new(), (630_301i64, 630_399i64));
    spawn_fighter_test_ball(&sys_handle, ship_id, 23_915, DVec3::ZERO, Some(CombatState::default())).await;
    spawn_fighter_test_ball(&sys_handle, f_aband, 23_061, DVec3::new(1_000.0, 0.0, 0.0), Some(CombatState::default())).await;
    fighter_test_helpers::insert_active_fighter(&ctx, f_aband, make_test_fighter_sq(f_aband, 530_301, 23_061, char_id, 0, 0, true));
    assert_eq!(handle_fighters(&ctx, &mut session, "fighterMgr", "ExecuteMovementCommandOnFighters", &[PyRep::List(vec![PyRep::Integer(f_aband)]), PyRep::String("MJD".into())], &kw).await.unwrap().unwrap_err().code, "FighterNotOwned");
    assert_eq!(sys_handle.mutate_fighter_ability_state(FighterAbilityActorOp::ReserveMjdBatch { char_id, targets: vec![(f_aband, 1, 100_000.0, 60)], now_unix: ctx.now_unix() }).await.unwrap_err().code, "FighterNotOwned");
    assert_eq!(sys_handle.mutate_fighter_ability_state(FighterAbilityActorOp::ReserveMjdBatch { char_id, targets: vec![(f_missing, 1, 100_000.0, 60)], now_unix: ctx.now_unix() }).await.unwrap_err().code, "FighterNotFound");
    assert!(fighter_test_helpers::fighter_ability_cooldown(&ctx, f_aband, 1).is_none() && fighter_test_helpers::fighter_ability_cooldown(&ctx, f_missing, 1).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r3_f2_03_concurrent_load_fighters_to_tube_enforces_class_slot_limit() {
    let (raw_ctx, session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let (ctx, f1, f2) = (Arc::new(raw_ctx), 630_401i64, 630_402i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(f1, make_fighter_item(f1, 23_063, char_id, ship_id, InventoryFlag::FighterBay, 3));
        snap.items.insert(f2, make_fighter_item(f2, 23_063, char_id, ship_id, InventoryFlag::FighterBay, 3));
    });
    let race_two_loads = |c: Arc<ServerContext>, sess: SessionState| async move {
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let run = |fid: i64, tube: i64| {
            let (c, b, mut s) = (Arc::clone(&c), Arc::clone(&barrier), sess.clone());
            tokio::spawn(async move { b.wait().await; handle_fighters(&c, &mut s, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(fid), PyRep::Integer(tube)], &HashMap::new()).await.unwrap() })
        };
        let (r1, r2) = tokio::join!(run(f1, 0), run(f2, 1));
        let results = [r1.unwrap(), r2.unwrap()];
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1, "at most 1 Support squadron fits Chimera's 1 Support slot");
        assert_eq!(results.into_iter().find_map(Result::err).expect("second concurrent load must reject").code, "CannotLoadFighterTubeNoTubesAvailableForClass");
    };
    race_two_loads(Arc::clone(&ctx), session.clone()).await;
    assert_eq!(fighter_test_helpers::fighter_tube_load_keys(&ctx).len(), 1);
    for tidx in 0..2 { let _ = ctx.systems.load_or_unload_fighter_tube(sys_id, crate::system_actor::FighterTubeOp::Unload { ship_id, tube_index: tidx }).await; }
    let _sys_handle = ctx.systems.get_or_spawn(sys_id);
    race_two_loads(Arc::clone(&ctx), session).await;
    assert_eq!(fighter_test_helpers::fighter_tube_load_keys(&ctx).len(), 1);
}

#[tokio::test]
async fn r5_f2_02_claim_ops_do_not_dirty_and_preserve_precommit_state_on_flush() {
    use crate::system_actor::{load_cold_fighters_for_system, FighterLaunchActorOp, FighterLaunchSpawnInput, FighterRecallActorOp, FighterScoopActorOp};
    let (ctx, _, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let (sys_h, f_item, sq_ball, ab_ball) = (ctx.systems.get_or_spawn(sys_id), 530_501i64, 770_002i64, 770_003i64);
    ctx.db.with_mut_snapshot(|s| { s.items.insert(f_item, make_fighter_item(f_item, 23_057, char_id, ship_id, InventoryFlag::FighterBay, 9)); });
    f2_test_commit_load(&ctx, sys_id, ship_id, 0, f_item, 23_057).await;
    sys_h.flush_fighter_checkpoint().await.unwrap();
    assert!(!sys_h.test_with_actor_state_async(|st| st.is_fighters_dirty()).await);

    // 1. Launch ClaimAndReserve must not mark dirty and concurrent flush keeps tube 0 without res_id.
    let mk_claim = || FighterLaunchActorOp::ClaimAndReserve { ship_id, char_id, carrier_type_id: 23_915, tube_count: 3, requested_tubes: Some(vec![0]), now_unix: ctx.now_unix(), next_res_ids: vec![770_001], sde: Arc::clone(&ctx.sde) };
    let claim = sys_h.fighter_launch_op(mk_claim()).await.unwrap();
    assert!(!sys_h.test_with_actor_state_async(|st| st.is_fighters_dirty()).await);
    fighter_test_helpers::set_fighter_unit_damage_accum_in_system(&ctx, sys_id, 999_001, 5.0);
    ctx.systems.flush_system_fighter_checkpoint_sync(sys_id);
    let cold = load_cold_fighters_for_system(&ctx.db, Some(&ctx.sde), sys_id).unwrap();
    assert!(cold.tube_loads.get(&(ship_id, 0)) == Some(&(f_item, 23_057)) && !cold.active_fighters.contains_key(&770_001));
    sys_h.fighter_launch_op(FighterLaunchActorOp::Rollback { ship_id, res_ids: claim.res_ids.iter().map(|&(_, r)| r).collect(), claimed_tubes: claim.claimed_tubes }).await.unwrap();

    let claim2 = sys_h.fighter_launch_op(mk_claim()).await.unwrap();
    sys_h.fighter_launch_op(FighterLaunchActorOp::CommitSpawned {
        res_ids: claim2.res_ids.into_iter().map(|(_, r)| r).collect(),
        spawns: vec![FighterLaunchSpawnInput {
            squadron: make_test_fighter_sq(sq_ball, f_item, 23_057, char_id, ship_id, 0, false),
            ball: eve_destiny::Ball { id: sq_ball, type_id: 23_057, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 2_500.0, inertia: 0.5, mass: 25_000.0, radius: 20.0, speed_fraction: 1.0, mode: eve_destiny::MovementMode::Stop },
            combat_state: CombatState::default(), slim_meta: None,
        }],
    }).await.unwrap();
    fighter_test_helpers::set_fighter_ability_cooldown_in_system(&ctx, sys_id, sq_ball, 0, 5_000);
    sys_h.flush_fighter_checkpoint().await.unwrap();
    assert!(!sys_h.test_with_actor_state_async(|st| st.is_fighters_dirty()).await);

    // 2. Recall ClaimActive must not mark dirty and concurrent flush retains launched sq + cooldown.
    let rc = sys_h.fighter_recall_op(FighterRecallActorOp::ClaimActive { ball_id: sq_ball, ship_id, char_id, sde: Arc::clone(&ctx.sde) }).await.unwrap().expect("claim active");
    assert!(!sys_h.test_with_actor_state_async(|st| st.is_fighters_dirty()).await);
    fighter_test_helpers::set_fighter_unit_damage_accum_in_system(&ctx, sys_id, 999_002, 5.0);
    ctx.systems.flush_system_fighter_checkpoint_sync(sys_id);
    let cold2 = load_cold_fighters_for_system(&ctx.db, Some(&ctx.sde), sys_id).unwrap();
    assert!(cold2.active_fighters.contains_key(&sq_ball) && cold2.cooldown_state.get(&(sq_ball, 0)) == Some(&5_000));
    sys_h.fighter_recall_op(FighterRecallActorOp::RestoreClaimed { squadron: rc.squadron, saved_charges: rc.saved_charges, saved_cooldowns: rc.saved_cooldowns, accum_dmg: rc.accum_dmg, abandoned_fallback: false }).await.unwrap();
    assert!(!sys_h.test_with_actor_state_async(|st| st.is_fighters_dirty()).await);

    // 3. Scoop Claim must not mark dirty and concurrent flush retains abandoned sq + damage until Commit.
    fighter_test_helpers::insert_active_fighter_in_system(&ctx, sys_id, ab_ball, make_test_fighter_sq(ab_ball, 530_503, 23_057, char_id, 0, 0, true));
    fighter_test_helpers::set_fighter_unit_damage_accum_in_system(&ctx, sys_id, ab_ball, 12.5);
    sys_h.flush_fighter_checkpoint().await.unwrap();
    assert!(!sys_h.test_with_actor_state_async(|st| st.is_fighters_dirty()).await);
    let (k, sq) = sys_h.scoop_abandoned_fighter_op(FighterScoopActorOp::Claim { target_fighter_id: ab_ball }).await.unwrap().unwrap();
    assert!(!sys_h.test_with_actor_state_async(|st| st.is_fighters_dirty()).await);
    fighter_test_helpers::set_fighter_unit_damage_accum_in_system(&ctx, sys_id, 999_003, 5.0);
    ctx.systems.flush_system_fighter_checkpoint_sync(sys_id);
    let cold3 = load_cold_fighters_for_system(&ctx.db, Some(&ctx.sde), sys_id).unwrap();
    assert!(cold3.active_fighters.contains_key(&ab_ball) && cold3.unit_damage_accum.get(&ab_ball) == Some(&12.5));
    sys_h.scoop_abandoned_fighter_op(FighterScoopActorOp::Restore { key: k, squadron: sq }).await.unwrap();
    assert!(!sys_h.test_with_actor_state_async(|st| st.is_fighters_dirty()).await);
    let (k2, _) = sys_h.scoop_abandoned_fighter_op(FighterScoopActorOp::Claim { target_fighter_id: ab_ball }).await.unwrap().unwrap();
    sys_h.scoop_abandoned_fighter_op(FighterScoopActorOp::Commit { key: k2 }).await.unwrap();
    assert!(sys_h.test_with_actor_state_async(|st| st.is_fighters_dirty()).await);
    sys_h.flush_fighter_checkpoint().await.unwrap();
    let cold4 = load_cold_fighters_for_system(&ctx.db, Some(&ctx.sde), sys_id).unwrap();
    assert!(!cold4.active_fighters.contains_key(&ab_ball) && cold4.unit_damage_accum.get(&ab_ball).is_none());
}

fn f2_hold_db(db: &Arc<eve_db::WorldRepository>, mut_fn: impl FnOnce(&mut eve_db::WorldStateSnapshot) + Send + 'static) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let ((htx, hrx), (rtx, rrx), d) = (std::sync::mpsc::channel(), std::sync::mpsc::channel(), Arc::clone(db));
    let h = std::thread::spawn(move || d.with_mut_snapshot(move |s| { htx.send(()).unwrap(); let _ = rrx.recv(); mut_fn(s); }));
    hrx.recv().unwrap();
    (rtx, h)
}

async fn f2_test_commit_load(ctx: &ServerContext, sys_id: u32, ship_id: i64, tube_index: usize, fighter_item_id: i64, fighter_type_id: u32) {
    ctx.systems.load_or_unload_fighter_tube(sys_id, crate::system_actor::FighterTubeOp::CommitLoad {
        ship_id, tube_index, fighter_item_id, carrier_type_id: 23_915, fighter_type_id, sde: Arc::clone(&ctx.sde),
    }).await.unwrap();
}

async fn f2_test_spawn_unhydrated(ctx: &ServerContext, sys_id: u32) -> crate::system_actor::SystemHandle {
    let held = f2_hold_db(&ctx.db, |_| ());
    let h = tokio::task::spawn_blocking({ let r = Arc::clone(&ctx.systems); move || r.get_or_spawn(sys_id) }).await.unwrap();
    let _ = held.0.send(());
    held.1.join().unwrap();
    h
}

fn f2_test_break_cp(ctx: &ServerContext, broken: bool) {
    let sql = if broken { "ALTER TABLE system_checkpoint RENAME TO system_checkpoint_bak; CREATE TABLE system_checkpoint(broken INT);" }
        else { "DROP TABLE system_checkpoint; ALTER TABLE system_checkpoint_bak RENAME TO system_checkpoint;" };
    ctx.db.with_sqlite_conn(|c| c.execute_batch(sql).map_err(|e| e.to_string())).unwrap();
}

fn f2_test_seed_sqlite_fighters(ctx: &ServerContext, sys_id: u32, sqs: &[(i64, ActiveFighterSquadron)]) {
    ctx.db.with_mut_snapshot(|s| { s.system_states.entry(sys_id).or_insert_with(|| eve_db::SystemDynamicState { solar_system_id: sys_id, ..Default::default() }); });
    let rows: Vec<_> = sqs.iter().map(|(id, sq)| (format!("sq:{id}"), serde_json::json!({ "Squadron": sq }))).collect();
    ctx.db.with_sqlite_conn(|c| eve_db::sqlite_store::system_checkpoint::replace_system_checkpoints_for_kind(c, sys_id, eve_db::sqlite_store::system_checkpoint::KIND_FIGHTERS, &rows, 100)).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r6_f2_02_destroyed_recall_db_fail_discards_claim_and_cleanup_clears_claims() {
    use crate::system_actor::{load_cold_fighters_for_system, FighterLaunchActorOp, FighterRecallActorOp};
    let (raw_ctx, session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let (ctx, (f_ball, f_item), (c_ball, c_item), (res_tube_item, res_id)) = (Arc::new(raw_ctx), (630_601i64, 530_601i64), (630_602i64, 530_602i64), (530_603i64, -399_001i64));
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(f_item, make_fighter_item(f_item, 23_057, char_id, sys_id as i64, InventoryFlag::None, 9));
        s.items.insert(res_tube_item, make_fighter_item(res_tube_item, 23_063, char_id, ship_id, InventoryFlag::FighterBay, 3));
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    spawn_fighter_test_ball(&sys_h, ship_id, 23_915, DVec3::ZERO, Some(CombatState::default())).await;
    spawn_fighter_test_ball(&sys_h, f_ball, 23_057, DVec3::new(100.0, 0.0, 0.0), None).await;
    spawn_fighter_test_ball(&sys_h, c_ball, 23_057, DVec3::new(150.0, 0.0, 0.0), None).await;
    fighter_test_helpers::insert_active_fighter_in_system(&ctx, sys_id, f_ball, make_test_fighter_sq(f_ball, f_item, 23_057, char_id, ship_id, 0, false));
    fighter_test_helpers::insert_active_fighter_in_system(&ctx, sys_id, c_ball, make_test_fighter_sq(c_ball, c_item, 23_057, char_id, ship_id, 2, false));
    f2_test_commit_load(&ctx, sys_id, ship_id, 1, res_tube_item, 23_063).await;
    sys_h.flush_fighter_checkpoint().await.unwrap();

    // Pre-existing P2: ClaimAndReserve + ClaimActive entries cleared on destruction without snapshot resurrection.
    sys_h.fighter_launch_op(FighterLaunchActorOp::ClaimAndReserve {
        ship_id, char_id, carrier_type_id: 23_915, tube_count: 3, requested_tubes: Some(vec![1]), now_unix: ctx.now_unix(), next_res_ids: vec![res_id], sde: Arc::clone(&ctx.sde),
    }).await.unwrap();
    assert!(sys_h.fighter_recall_op(FighterRecallActorOp::ClaimActive { ball_id: c_ball, ship_id, char_id, sde: Arc::clone(&ctx.sde) }).await.unwrap().is_some());
    assert!(sys_h.cleanup_destroyed_fighter_sync(res_id).is_some() && sys_h.cleanup_destroyed_fighter_sync(c_ball).is_some());
    {
        let (no_res, no_claims, snap_ok) = sys_h
            .test_with_actor_state_async(move |st| {
                (
                    st.pending_launch_reservations.is_empty(),
                    st.pending_recall_or_scoop_claims.is_empty(),
                    st.take_fighter_checkpoint_snapshot().is_some_and(|s| {
                        !s.state.tube_loads.contains_key(&(ship_id, 1))
                            && !s.state.active_fighters.contains_key(&c_ball)
                    }),
                )
            })
            .await;
        assert!(no_res && no_claims);
        assert!(snap_ok);
    }

    // F2-R6-02: Destroyed squadron whose recall fails DB check after ClaimActive must discard claim instead of resurrecting.
    ctx.db.with_mut_snapshot(|s| { s.combat_crimewatch_state.record_destroyed_item_id(f_ball); s.combat_crimewatch_state.record_destroyed_item_id(f_item); });
    let held = f2_hold_db(&ctx.db, move |s| { s.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_760); });
    let rpc = tokio::spawn({ let (c, mut s) = (Arc::clone(&ctx), session); async move { handle_fighters(&c, &mut s, "fighterMgr", "RecallFightersToTubes", &[PyRep::List(vec![PyRep::Integer(f_ball)])], &HashMap::new()).await } });
    for _ in 0..200 {
        if sys_h
            .test_with_actor_state_async(move |st| st.pending_recall_or_scoop_claims.contains_key(&f_ball))
            .await
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        sys_h
            .test_with_actor_state_async(move |st| st.pending_recall_or_scoop_claims.contains_key(&f_ball))
            .await
    );
    let _ = held.0.send(());
    held.1.join().unwrap();
    assert!(rpc.await.unwrap().unwrap().is_err());
    assert!(
        sys_h
            .test_with_actor_state_async(|st| st.pending_recall_or_scoop_claims.is_empty())
            .await
            && fighter_test_helpers::active_fighter(&ctx, f_ball).is_none()
    );
    let _ = sys_h.flush_fighter_checkpoint().await;
    assert!(!load_cold_fighters_for_system(&ctx.db, Some(&ctx.sde), sys_id).unwrap().active_fighters.contains_key(&f_ball));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r6_f2_04_load_tube_split_invalid_restores_prev_and_preserves_concurrent_load() {
    use crate::services::combat_crimewatch::combat_fighters_abilities::{
        handle_activate_fighter_ability_slots, handle_activate_fighter_ability_slots_on_point, handle_deactivate_fighter_ability_slots,
    };
    use crate::system_actor::FighterTubeOp;
    let (ctx, _, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let ((f_prev, f_next, f_win, sq_ball), kw) = ((530_701i64, 530_702i64, 530_703i64, 630_701i64), HashMap::new());
    ctx.db.with_mut_snapshot(|s| { for iid in [f_prev, f_next, f_win] { s.items.insert(iid, make_fighter_item(iid, 23_057, char_id, ship_id, InventoryFlag::FighterBay, 9)); } });

    // F2-R6-04: RollbackLoad restores prev if unchanged and preserves concurrent overwrite (Cold + Live).
    for live in [false, true] {
        if live { let _ = ctx.systems.get_or_spawn(sys_id); }
        f2_test_commit_load(&ctx, sys_id, ship_id, 0, f_prev, 23_057).await;
        f2_test_commit_load(&ctx, sys_id, ship_id, 0, f_next, 23_057).await;
        let rb = || FighterTubeOp::RollbackLoad { ship_id, tube_index: 0, expected_fighter_item_id: f_next, restore_prev: Some((f_prev, 23_057)) };
        assert_eq!(ctx.systems.load_or_unload_fighter_tube(sys_id, rb()).await.unwrap(), Some(f_prev));
        assert_eq!(ctx.systems.snapshot_fighters_for_system(sys_id).tube_loads.get(&(ship_id, 0)), Some(&(f_prev, 23_057)));
        f2_test_commit_load(&ctx, sys_id, ship_id, 0, f_next, 23_057).await;
        f2_test_commit_load(&ctx, sys_id, ship_id, 0, f_win, 23_057).await;
        assert_eq!(ctx.systems.load_or_unload_fighter_tube(sys_id, rb()).await.unwrap(), None);
        assert_eq!(ctx.systems.snapshot_fighters_for_system(sys_id).tube_loads.get(&(ship_id, 0)), Some(&(f_win, 23_057)));
    }
    assert!(ctx.systems.stop_system(sys_id).await);

    // F2-R6-03: Mutating fighter RPCs, ability endpoints, and damage path refuse/hydrate cleanly when unhydrated.
    let (ctx2, mut sess2, char2, ship2, sys2) = make_fighter_ctx_and_session();
    let h2 = f2_test_spawn_unhydrated(&ctx2, sys2).await;
    fighter_test_helpers::insert_active_fighter_in_system(&ctx2, sys2, sq_ball, make_test_fighter_sq(sq_ball, 530_799, 23_057, char2, ship2, 0, false));
    spawn_fighter_test_ball(&h2, ship2, 23_915, DVec3::ZERO, Some(CombatState::default())).await;
    spawn_fighter_test_ball(&h2, sq_ball, 23_057, DVec3::new(100.0, 0.0, 0.0), Some(CombatState::default())).await;
    f2_test_break_cp(&ctx2, true);
    assert_eq!(handle_fighters(&ctx2, &mut sess2, "fighterMgr", "CmdAbandonFighter", &[PyRep::Integer(sq_ball)], &kw).await.unwrap().unwrap_err().code, "StorageError");
    assert_eq!(handle_activate_fighter_ability_slots(&ctx2, &mut sess2, &[PyRep::Integer(sq_ball), PyRep::Integer(1)]).await.unwrap().unwrap_err().code, "StorageError");
    assert_eq!(handle_activate_fighter_ability_slots_on_point(&ctx2, &mut sess2, &[PyRep::List(vec![PyRep::Integer(sq_ball)]), PyRep::Integer(1), PyRep::Tuple(vec![PyRep::Real(0.0); 3])]).await.unwrap().unwrap_err().code, "StorageError");
    assert_eq!(handle_deactivate_fighter_ability_slots(&ctx2, &mut sess2, &[PyRep::Integer(sq_ball), PyRep::Integer(1)]).await.unwrap().unwrap_err().code, "StorageError");
    assert_eq!(apply_damage_to_fighter_squadron(&ctx2, sys2, sq_ball, DamageVector::new(100.0, 0.0, 0.0, 0.0)).await, (0, 0, false));
    f2_test_break_cp(&ctx2, false);
    assert!(apply_damage_to_fighter_squadron(&ctx2, sys2, sq_ball, DamageVector::new(1_200.0, 0.0, 0.0, 0.0)).await.0 >= 1 && !h2.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await);

    // BF-02: Unhydrated cross-system fighter lookup hydrates and returns FighterNotInSystem.
    let (sys_other, other_sq) = (30_002_187u32, 630_799i64);
    f2_test_seed_sqlite_fighters(&ctx2, sys_other, &[(other_sq, make_test_fighter_sq(other_sq, 530_798, 23_057, char2, ship2, 0, false))]);
    let h_other = f2_test_spawn_unhydrated(&ctx2, sys_other).await;
    assert!(h_other.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await);
    assert_eq!(handle_activate_fighter_ability_slots(&ctx2, &mut sess2, &[PyRep::Integer(other_sq), PyRep::Integer(1)]).await.unwrap().unwrap_err().code, "FighterNotInSystem");
    assert!(!h_other.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r7_f2_02_unhydrated_tether_damage_standup_spool_npc_and_notify() {
    use crate::combat::npc_ai::{step_system_npc_spawns_ai_sync, NpcSpawnManager};
    use crate::services::combat_crimewatch::{emit_combat_log_and_damage_state, set_char_safety_level, SafetyLevel};
    use crate::services::combat_crimewatch::combat_weapons_spool::handle_ship_or_npc_destruction;
    use crate::services::space_destiny::space_nav_travel::{attr_id_by_name, sync_tether};
    use crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball;
    use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state, InFlightGuidedBombRecord};
    use crate::services::structure_standup_weapons::{step_guided_bombs_in_system, GROUP_STANDUP_GUIDED_BOMB};
    let (mut ctx, _, char_id, ship_id, sys_t) = make_fighter_ctx_and_session();
    let (corp_owner, gunner_id, gunner_ship) = (98_500_001_i64, 90_000_099_i64, 200_000_099_i64);
    let (sys_n1, sys_n2, sys_d, sys_sp, sys_b, sys_npc) = (30_002_512u32, 30_002_518u32, 30_002_514u32, 30_002_515u32, 30_002_511u32, 30_002_516u32);
    let (bomb_id, bomb_tid, st_id, sg_id) = (880_001_i64, 99_901_u32, 1_049_901_001_i64, 999_142_u32);
    let tether_attr = attr_id_by_name(&ctx, "tetheringRange").unwrap_or_else(|| {
        let aid = 2_095_u16;
        Arc::make_mut(&mut ctx.sde).attributes.insert(aid, eve_sde::DogmaAttributeType { attribute_id: aid, name: "tetheringRange".into(), default_value: 0.0, high_is_good: true, stackable: true, published: true });
        aid
    });
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        sde.types.insert(35_832, make_fighter_type(35_832, 1657, 65, 800_000.0, vec![(tether_attr, 15_000.0)]));
        sde.types.insert(bomb_tid, make_fighter_type(bomb_tid, GROUP_STANDUP_GUIDED_BOMB, 8, 50.0, vec![(114, 200.0), (99, 20_000.0), (654, 100.0)]));
        sde.solar_systems.insert(sys_b, eve_sde::SolarSystem { solar_system_id: sys_b, constellation_id: 20_000_001, region_id: 10_000_001, name: "R7Lowsec".into(), security_status: 0.2, radius: 1.0e12, galactic_pos_ly: [0.0; 3], planet_ids: vec![], stations: vec![], wormhole_class_id: None });
        sde.npc_spawn_groups.insert(sg_id, eve_sde::NpcSpawnGroupInfo { spawn_group_id: sg_id, faction_id: 500_012, ship_class: 1, group_id: 550, group_name: "Corpii Test".into(), npc_type_ids: vec![16_936] });
    }
    let st_rec = |sid: i64, sol: u32, corp: i64| eve_db::UpwellStructureRecord { structure_id: sid, type_id: 35_832, owner_corp_id: corp, solar_system_id: sol, name: "Citadel".into(), state: "ShieldVulnerable".into(), quantum_core_installed: true, fuel_blocks: 100, position: [0.0; 3], shield_hp: 100_000.0, armor_hp: 100_000.0, hull_hp: 100_000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None };
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&char_id).unwrap().corporation_id = 1_000_009;
        let mut g = s.characters.get(&char_id).unwrap().clone();
        g.character_id = gunner_id; g.corporation_id = corp_owner; g.alliance_id = None; g.solar_system_id = sys_b; g.active_ship_id = gunner_ship;
        s.characters.insert(gunner_id, g);
        s.items.insert(gunner_ship, make_fighter_item(gunner_ship, 23_915, gunner_id, sys_b as i64, InventoryFlag::None, 1));
        s.structures.insert(st_id, st_rec(st_id, sys_t, 1_000_009));
        s.structures.insert(st_id + 1, st_rec(st_id + 1, sys_b, corp_owner));
        let mut ext = load_ext_state(s);
        ext.in_flight_guided_bombs.insert(bomb_id, InFlightGuidedBombRecord { bomb_id, structure_id: st_id + 1, structure_corp_id: corp_owner, gunner_char_id: gunner_id, solar_system_id: sys_b, target_id: gunner_ship, bomb_type_id: bomb_tid, remaining_flight_s: 5.0 });
        save_ext_state(s, &ext);
    });
    set_char_safety_level(&ctx, gunner_id, SafetyLevel::Red);
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx.db.acquire_online_lock(uid, 1, Some(char_id), true, ctx.now_unix());
    let sponge_cs = || CombatState { shield_hp: 500.0, max_shield_hp: 500.0, armor_hp: 500.0, max_armor_hp: 500.0, structure_hp: 500.0, max_structure_hp: 500.0, signature_radius: 1_000.0, ..CombatState::default() };

    // 1. sync_tether hydrates and denies tether while deployed SQLite fighter is active.
    f2_test_seed_sqlite_fighters(&ctx, sys_t, &[(630_801, make_test_fighter_sq(630_801, 530_801, 23_057, char_id, ship_id, 0, false))]);
    let h_t = f2_test_spawn_unhydrated(&ctx, sys_t).await;
    spawn_fighter_test_ball(&h_t, ship_id, 23_915, DVec3::new(2_000.0, 0.0, 0.0), Some(CombatState::default())).await;
    sync_tether(&ctx, char_id, sys_t, ship_id).await;
    assert!(!h_t.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await && !h_t.get_combat_state(ship_id).await.unwrap().unwrap().is_tethered);

    // 2. crimewatch_scene_notify hydrates on both targeted combat_sys_id (Some) and scan fallback (None).
    for (sys_n, sq_n, explicit) in [(sys_n1, 630_802_i64, Some(sys_n1)), (sys_n2, 630_803_i64, None)] {
        ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&char_id).unwrap().solar_system_id = sys_n; s.characters.get_mut(&gunner_id).unwrap().solar_system_id = sys_n; s.items.get_mut(&gunner_ship).unwrap().location_id = sys_n as i64; });
        let _ = ctx.drain_notifications_for_char(char_id);
        f2_test_seed_sqlite_fighters(&ctx, sys_n, &[(sq_n, make_test_fighter_sq(sq_n, sq_n - 100_000, 23_057, char_id, ship_id, 0, false))]);
        let h_n = f2_test_spawn_unhydrated(&ctx, sys_n).await;
        emit_combat_log_and_damage_state(&ctx, gunner_id, None, gunner_ship, sq_n, 23_057, 25.0, false, 0.8, 1.0, 1.0, explicit);
        assert!(!h_n.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await && ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event.starts_with("OnDamageMessage")));
    }

    // 3. apply_damage_structure_or_ball hydrates and applies squadron casualty damage.
    let sq_d = 630_804_i64;
    f2_test_seed_sqlite_fighters(&ctx, sys_d, &[(sq_d, make_test_fighter_sq(sq_d, 530_804, 23_057, char_id, ship_id, 0, false))]);
    let h_d = f2_test_spawn_unhydrated(&ctx, sys_d).await;
    spawn_fighter_test_ball(&h_d, sq_d, 23_057, DVec3::ZERO, Some(sponge_cs())).await;
    let dmg_hit = apply_damage_structure_or_ball(&ctx, &h_d, sys_d, gunner_id, sq_d, DamageVector::new(1_200.0, 0.0, 0.0, 0.0), 1.0, ctx.now_unix()).await.unwrap();
    assert!(dmg_hit.is_some_and(|(a, _, _)| a > 0.0) && !h_d.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await && h_d.view().fighters.active_fighters.get(&sq_d).is_none_or(|sq| sq.squadron_size < 9));

    // 4. combat_weapons_spool::handle_ship_or_npc_destruction hydrates and removes destroyed fighter while keeping siblings.
    let (sq_sp, sq_sp_keep) = (630_805_i64, 630_806_i64);
    f2_test_seed_sqlite_fighters(&ctx, sys_sp, &[(sq_sp, make_test_fighter_sq(sq_sp, 530_805, 23_057, char_id, ship_id, 0, false)), (sq_sp_keep, make_test_fighter_sq(sq_sp_keep, 530_806, 23_057, char_id, ship_id, 1, false))]);
    let h_sp = f2_test_spawn_unhydrated(&ctx, sys_sp).await;
    spawn_fighter_test_ball(&h_sp, sq_sp, 23_057, DVec3::ZERO, Some(sponge_cs())).await;
    let ball_sp = h_sp.get_ball_snapshot(sq_sp).await.unwrap().unwrap().1;
    let _ = handle_ship_or_npc_destruction(&ctx, &h_sp, gunner_id, gunner_ship, 23_915, 0, sq_sp, &ball_sp, None, sys_sp, 100.0, ctx.now_unix()).await;
    assert!(!h_sp.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await && !h_sp.view().fighters.active_fighters.contains_key(&sq_sp) && h_sp.view().fighters.active_fighters.contains_key(&sq_sp_keep));

    // 5. step_guided_bombs_in_system hydrates before hostility check: spares corp-friendly fighter, damages hostile fighter.
    let (sq_fr, sq_en) = (630_807_i64, 630_808_i64);
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&char_id).unwrap().solar_system_id = sys_b; s.characters.get_mut(&gunner_id).unwrap().solar_system_id = sys_b; });
    f2_test_seed_sqlite_fighters(&ctx, sys_b, &[(sq_fr, make_test_fighter_sq(sq_fr, 530_807, 23_057, gunner_id, gunner_ship, 0, false)), (sq_en, make_test_fighter_sq(sq_en, 530_808, 23_057, char_id, ship_id, 1, false))]);
    let h_b = f2_test_spawn_unhydrated(&ctx, sys_b).await;
    for (bid, tid, pos, cs) in [(bomb_id, bomb_tid, DVec3::new(100.0, 0.0, 0.0), None), (gunner_ship, 23_915, DVec3::ZERO, Some(sponge_cs())), (sq_fr, 23_057, DVec3::new(200.0, 0.0, 0.0), Some(sponge_cs())), (sq_en, 23_057, DVec3::new(200.0, 0.0, 0.0), Some(sponge_cs()))] {
        spawn_fighter_test_ball(&h_b, bid, tid, pos, cs).await;
    }
    step_guided_bombs_in_system(&ctx, sys_b, 0.1).await;
    assert!(!h_b.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await && h_b.get_combat_state(sq_fr).await.unwrap().unwrap().shield_hp == 500.0 && h_b.get_combat_state(sq_en).await.unwrap().unwrap().shield_hp < 500.0);

    // 6. npc_ai::step_system_npc_spawns_ai_sync hydrates and resolves fighter owner_char_id -> delivers OnDamageMessage.
    let sq_npc = 630_809_i64;
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&char_id).unwrap().solar_system_id = sys_npc; });
    let _ = ctx.drain_notifications_for_char(char_id);
    f2_test_seed_sqlite_fighters(&ctx, sys_npc, &[(sq_npc, make_test_fighter_sq(sq_npc, 530_809, 23_057, char_id, ship_id, 0, false))]);
    let h_npc = f2_test_spawn_unhydrated(&ctx, sys_npc).await;
    let mut mgr = NpcSpawnManager::new();
    let npc_ids = mgr.spawn_group(&ctx, sys_npc, sg_id, DVec3::ZERO).await.unwrap();
    let npc_pos = h_npc.view().ball_position(npc_ids[0]).unwrap();
    crate::combat::npc_ai::npc_test_helpers::insert_npc_spawn_manager(&ctx, sys_npc, mgr);
    let _ = h_npc.spawn_ball_with_meta(
        eve_destiny::Ball { id: sq_npc, type_id: 23_057, position: npc_pos + DVec3::new(800.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 40.0, inertia: 1.0, mass: 1_000.0, radius: 25.0, speed_fraction: 1.0, mode: eve_destiny::MovementMode::Stop },
        sponge_cs(),
        eve_destiny::SlimItemMeta { group_id: 100, category_id: 18, owner_id: char_id, ..Default::default() },
    ).await.unwrap();
    assert!(h_npc.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await);
    for tick in 1..=5 { step_system_npc_spawns_ai_sync(&ctx, sys_npc, tick, 1.0); }
    assert!(!h_npc.test_with_actor_state_async(|st| st.fighters_needs_sqlite_hydration).await && ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event.starts_with("OnDamageMessage")));
}
