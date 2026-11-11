//! Extra unit and mutation-killing tests for `services::inventory_stacking`
//! covering `MultiMerge` validation rejections, `StackAll` ship/station flag defaults,
//! `handle_multi_add` hold capacity & role/rig/courier/weapons-timer guards,
//! `evaluate_space_wreck_or_jetcan_loot` corp/criminal/abandoned rules, and `handle_list` range/role checks.

#![forbid(unsafe_code)]

use crate::router::{context::BoundObjectEntry, ServerContext};
use crate::services::inventory_fitting::fitting_containers_ships::{
    read_container_ship_state, save_container_ship_state,
};
use crate::services::inventory_stacking::{
    despawn_empty_jetcan_if_needed, evaluate_space_wreck_or_jetcan_loot, handle_list,
    handle_list_by_flags, handle_multi_add, handle_stack_ops,
};
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, VirtualServerClock,
    WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

fn seed_char(
    ctx: &ServerContext,
    char_id: i64,
    corp_id: i64,
    station_id: Option<i64>,
    sys_id: u32,
    ship_id: i64,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("StackPilot {char_id}");
        ch.corporation_id = corp_id;
        ch.station_id = station_id;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
    });
    let _ = ctx.db.acquire_online_lock(
        char_id as u64,
        char_id as u64,
        Some(char_id),
        true,
        ctx.now_unix_secs(),
    );
    SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id,
        structure_id: None,
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    }
}

fn bound_inv(object_id: i64, char_id: i64) -> BoundObjectEntry {
    BoundObjectEntry {
        bound_id: 20_001,
        session_id: 1,
        user_id: char_id as u64,
        character_id: char_id,
        service: "invbroker".into(),
        object_id,
        oid_str: format!("N=20001:{object_id}"),
        ref_id: 1,
    }
}

fn put_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton,
                ..Default::default()
            },
        );
    });
}

#[test]
fn multi_merge_all_error_guards_and_in_space_stack_all() {
    let ctx = make_ctx();
    let char_id = 93_000_001;
    let corp_id = 98_000_010;
    let station_id = 60_003_760;
    let ship_id = 1_100_000_001;
    let mut sess = seed_char(&ctx, char_id, corp_id, Some(station_id), 30_000_142, ship_id);
    let kw = HashMap::new();

    let s1 = 1_100_000_010;
    let s2 = 1_100_000_011;
    let s_other_owner = 1_100_000_012;
    let s_diff_type = 1_100_000_013;
    let s_singleton = 1_100_000_014;
    let s_mutated = 1_100_000_015;
    let s_locked = 1_100_000_016;
    let s_courier = 1_100_000_017;

    put_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, s1, 34, char_id, station_id, InventoryFlag::Hangar, 100, false);
    put_item(&ctx, s2, 34, char_id, station_id, InventoryFlag::Hangar, 50, false);
    put_item(&ctx, s_other_owner, 34, char_id + 1, station_id, InventoryFlag::Hangar, 50, false);
    put_item(&ctx, s_diff_type, 35, char_id, station_id, InventoryFlag::Hangar, 50, false);
    put_item(&ctx, s_singleton, 34, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, s_mutated, 34, char_id, station_id, InventoryFlag::Hangar, 10, false);
    put_item(&ctx, s_locked, 34, char_id, station_id, InventoryFlag::Locked, 10, false);
    put_item(&ctx, s_courier, 34, char_id, station_id, InventoryFlag::Hangar, 10, false);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&s_mutated).unwrap().dynamic_attributes.insert(1, 2.0);
        snap.item_custom_names.insert(s_courier, "courier_cargo:99".into());
    });

    // Empty MultiMerge args -> StackFailed
    assert_eq!(
        handle_stack_ops(&ctx, &mut sess, None, "invbroker", "MultiMerge", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "StackFailed"
    );

    // Self-merge, missing item, ownership mismatch, type mismatch, singleton, mutated, locked, courier, bad qty
    for (src, dst, qty_opt) in [
        (s1, s1, None),
        (9_999_999, s1, None),
        (s1, 9_999_999, None),
        (s_other_owner, s1, None),
        (s_diff_type, s1, None),
        (s_singleton, s1, None),
        (s_mutated, s1, None),
        (s_locked, s1, None),
        (s_courier, s1, None),
        (s2, s1, Some(0)),
        (s2, s1, Some(999)),
    ] {
        let args = if let Some(q) = qty_opt {
            vec![PyRep::Integer(dst), PyRep::Integer(src), PyRep::Integer(q)]
        } else {
            vec![PyRep::Integer(dst), PyRep::Integer(src)]
        };
        let err = handle_stack_ops(&ctx, &mut sess, None, "invbroker", "MultiMerge", &args, &kw)
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, "StackFailed");
    }

    // Integer form `[dst_id, src_id, qty]` valid partial merge
    let ok_merge = handle_stack_ops(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "MultiMerge",
        &[PyRep::Integer(s1), PyRep::Integer(s2), PyRep::Integer(20)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(ok_merge.dict_get("quantity"), Some(&PyRep::Integer(120)));

    // StackAll without any location -> InvalidLocation
    let mut no_loc_sess = sess.clone();
    no_loc_sess.station_id = None;
    no_loc_sess.ship_id = None;
    assert_eq!(
        handle_stack_ops(&ctx, &mut no_loc_sess, None, "invbroker", "StackAll", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidLocation"
    );

    // StackAll in space defaults to Cargo flag on active ship
    let c1 = 1_100_000_020;
    let c2 = 1_100_000_021;
    put_item(&ctx, c1, 34, char_id, ship_id, InventoryFlag::Cargo, 30, false);
    put_item(&ctx, c2, 34, char_id, ship_id, InventoryFlag::Cargo, 45, false);
    sess.station_id = None;
    let ship_stack = handle_stack_ops(&ctx, &mut sess, None, "invbroker", "StackAll", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(ship_stack.dict_get("flagID"), Some(&PyRep::Integer(InventoryFlag::Cargo as i64)));
    assert_eq!(ship_stack.dict_get("mergedCount"), Some(&PyRep::Integer(1)));
}

#[tokio::test]
async fn multi_add_capacity_rig_locked_courier_corp_role_and_in_space_unfit_guards() {
    let ctx = make_ctx();
    let char_id = 93_000_002;
    let corp_id = 98_000_020;
    let station_id = 60_003_760;
    let ship_id = 1_100_000_100;
    let mut sess = seed_char(&ctx, char_id, corp_id, Some(station_id), 30_000_142, ship_id);
    let kw = HashMap::new();

    put_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);

    // Empty item list returns movedCount 0
    let empty_res = handle_multi_add(&ctx, &mut sess, None, &[PyRep::List(vec![])], &kw)
        .await
        .unwrap();
    assert_eq!(empty_res.dict_get("movedCount"), Some(&PyRep::Integer(0)));

    // Moving active ship -> CannotMoveActiveShip
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            None,
            &[PyRep::Integer(ship_id), PyRep::Integer(station_id), PyRep::Integer(4)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CannotMoveActiveShip"
    );

    // Fitted rig (flag 92) -> CannotUnfitRig
    let rig_id = 1_100_000_101;
    put_item(&ctx, rig_id, 34, char_id, ship_id, InventoryFlag::RigSlot0, 1, true);
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            None,
            &[PyRep::Integer(rig_id), PyRep::Integer(station_id), PyRep::Integer(4)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CannotUnfitRig"
    );

    // Locked item -> ItemLocked
    let locked_id = 1_100_000_102;
    put_item(&ctx, locked_id, 34, char_id, station_id, InventoryFlag::Locked, 10, false);
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            None,
            &[PyRep::Integer(locked_id), PyRep::Integer(ship_id), PyRep::Integer(5)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "ItemLocked"
    );

    // Courier cargo into corp hangar division (flag 116) -> CannotDeliverCourierCargo
    let courier_id = 1_100_000_103;
    put_item(&ctx, courier_id, 34, char_id, station_id, InventoryFlag::Hangar, 1, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_custom_names.insert(courier_id, "courier_cargo:1".into());
    });
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            None,
            &[PyRep::Integer(courier_id), PyRep::Integer(station_id), PyRep::Integer(116)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CannotDeliverCourierCargo"
    );

    // Taking from corp division 116 without take role -> CorpRoleDenied
    let corp_item_id = 1_100_000_104;
    put_item(&ctx, corp_item_id, 34, corp_id, station_id, InventoryFlag::CorpSAG1, 10, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Test Corp".into();
                __corp.ticker = "TSTC".into();
                __corp.ceo_id = char_id + 99;
                __corp.members = HashSet::from([char_id]);
                __corp.member_roles = HashMap::from([(char_id, 0)]);
                __corp.set_division_balance(1, IskCents::ZERO);
                __corp },
        );
    });
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            None,
            &[PyRep::Integer(corp_item_id), PyRep::Integer(station_id), PyRep::Integer(4)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Cargo capacity exceeded on Rifter (587) -> NotEnoughCargoSpace
    let huge_ore_id = 1_100_000_105;
    put_item(&ctx, huge_ore_id, 34, char_id, station_id, InventoryFlag::Hangar, 20_000_000, false);
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            None,
            &[PyRep::Integer(huge_ore_id), PyRep::Integer(ship_id), PyRep::Integer(5)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughCargoSpace"
    );

    // Unfitting a fitted module in space with active weapons timer -> CannotRefitWithWeaponsTimer;
    // without weapons timer and not near Mobile Depot -> CannotUnfitInSpace
    let mod_id = 1_100_000_106;
    put_item(&ctx, mod_id, 439, char_id, ship_id, InventoryFlag::MedSlot0, 1, true);
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
        snap.items.get_mut(&ship_id).unwrap().location_id = 30_000_142;
        snap.items.get_mut(&ship_id).unwrap().flag = InventoryFlag::None;
    });
    ctx.crimewatch
        .write()
        .unwrap()
        .trigger_weapon_timer(char_id, ctx.now_unix_secs());
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            None,
            &[PyRep::Integer(mod_id), PyRep::Integer(ship_id), PyRep::Integer(5)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CannotRefitWithWeaponsTimer"
    );

    ctx.crimewatch.write().unwrap().states.clear();
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            None,
            &[PyRep::Integer(mod_id), PyRep::Integer(ship_id), PyRep::Integer(5)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CannotUnfitInSpace"
    );
}

#[tokio::test]
async fn list_ownership_corp_roles_range_and_loot_abandonment_rules() {
    let ctx = make_ctx();
    let char_id = 93_000_003;
    let corp_mate_id = 93_000_004;
    let corp_id = 98_000_030;
    let station_id = 60_003_760;
    let sys_id = 30_000_142;
    let ship_id = 1_100_000_200;
    let mut sess = seed_char(&ctx, char_id, corp_id, Some(station_id), sys_id, ship_id);
    let _ = seed_char(&ctx, corp_mate_id, corp_id, None, sys_id, 1_100_000_201);
    let kw = HashMap::new();

    // 1. Bound station when undocked -> BoundObjectOwnershipViolation
    sess.station_id = None;
    let wrong_bound = bound_inv(station_id, char_id);
    assert_eq!(
        handle_list(&ctx, &sess, Some(&wrong_bound), &[], &kw)
            .unwrap_err()
            .code,
        "BoundObjectOwnershipViolation"
    );
    sess.station_id = Some(station_id);

    // 2. Listing corp division 116 without query role -> CorpRoleDenied; as CEO -> succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "List Corp".into();
                __corp.ticker = "LSTC".into();
                __corp.ceo_id = corp_mate_id;
                __corp.members = HashSet::from([char_id, corp_mate_id]);
                __corp.member_roles = HashMap::from([(char_id, 0)]);
                __corp.set_division_balance(1, IskCents::ZERO);
                __corp },
        );
    });
    assert_eq!(
        handle_list_by_flags(
            &ctx,
            &sess,
            Some(&wrong_bound),
            &[PyRep::Integer(116)],
            &kw,
        )
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().ceo_id = char_id;
    });
    assert!(
        handle_list_by_flags(
            &ctx,
            &sess,
            Some(&wrong_bound),
            &[PyRep::Integer(116)],
            &kw,
        )
        .is_ok()
    );

    // 3. Space jetcan range check in handle_list + non-empty jetcan does not despawn
    let jetcan_id = 1_100_000_210;
    let loot_id = 1_100_000_211;
    put_item(&ctx, ship_id, 587, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, jetcan_id, 23, corp_mate_id, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, loot_id, 34, corp_mate_id, jetcan_id, InventoryFlag::Cargo, 25, false);

    // Same player corp (> 2_000_000) can loot without suspect flag
    let loot_rec = ctx.db.with_snapshot(|s| s.items[&loot_id].clone());
    assert_eq!(
        evaluate_space_wreck_or_jetcan_loot(&ctx, char_id, &loot_rec).unwrap(),
        (true, false)
    );

    // Non-empty jetcan is not despawned
    assert!(!despawn_empty_jetcan_if_needed(&ctx, Some(sys_id), jetcan_id).await);

    // Container password check in handle_list
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&jetcan_id).unwrap().owner_id = char_id;
    });
    {
        let mut st = read_container_ship_state(&ctx);
        st.container_passwords.insert(jetcan_id, "pw42".into());
        save_container_ship_state(&ctx, &st);
    }
    let jet_bound = bound_inv(jetcan_id, char_id);
    assert_eq!(
        handle_list(&ctx, &sess, Some(&jet_bound), &[], &kw)
            .unwrap_err()
            .code,
        "ALSCPasswordRequired"
    );

    // Supply password via args, but caller is still docked -> NotInSameSolarSystem
    assert_eq!(
        handle_list(
            &ctx,
            &sess,
            Some(&jet_bound),
            &[PyRep::String("pw42".into())],
            &kw,
        )
        .unwrap_err()
        .code,
        "NotInSameSolarSystem"
    );

    // Undock and place jetcan at 10,000m (> 2,500m) -> OutOfRange
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
        snap.item_positions.insert(jetcan_id, [10_000.0, 0.0, 0.0]);
    });
    assert_eq!(
        handle_list(&ctx, &sess, Some(&jet_bound), &[], &kw)
            .unwrap_err()
            .code,
        "OutOfRange"
    );
}

#[tokio::test]
async fn ship_hold_capacity_errors_match_hold_specific_codes_for_add_and_multi_add() {
    use crate::services::inventory_fitting::{ship_hold_capacity_error, ship_hold_capacity_error_code};

    assert_eq!(ship_hold_capacity_error_code(5), "NotEnoughCargoSpace");
    assert_eq!(ship_hold_capacity_error_code(87), "NotEnoughDroneBaySpace");
    assert_eq!(ship_hold_capacity_error_code(158), "NotEnoughFighterBaySpace");
    assert_eq!(ship_hold_capacity_error_code(134), "NotEnoughSpecialBaySpace");

    let e = ship_hold_capacity_error(158, 1_000.0, 400.0, 800.0);
    assert_eq!(e.code, "NotEnoughFighterBaySpace");
    assert!(e.message.contains("free=600"), "msg={}", e.message);
    assert!(e.message.contains("required=800"), "msg={}", e.message);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let station_id = 60_003_760;
    let venture_id = 1_100_000_501;
    let rifter_id = 1_100_000_502;
    let thanatos_id = 1_100_000_503;
    let drone_id = 1_100_000_510;
    let ore_id = 1_100_000_511;
    let trit_id = 1_100_000_512;
    let fighter_id = 1_100_000_513;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            venture_id,
            InventoryItemRecord {
                item_id: venture_id,
                type_id: 32_880,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            rifter_id,
            InventoryItemRecord {
                item_id: rifter_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            thanatos_id,
            InventoryItemRecord {
                item_id: thanatos_id,
                type_id: 23_911,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            drone_id,
            InventoryItemRecord {
                item_id: drone_id,
                type_id: 2454,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 4, // 20 m3 > Venture 10 m3 DroneBay
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            ore_id,
            InventoryItemRecord {
                item_id: ore_id,
                type_id: 1230,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 60_000, // 6,000 m3 > Venture 5,000 m3 OreHold
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            trit_id,
            InventoryItemRecord {
                item_id: trit_id,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 20_000, // 200 m3 > Venture 50 m3 Cargo
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            fighter_id,
            InventoryItemRecord {
                item_id: fighter_id,
                type_id: 23_055, // Templar I (1000 m3 each)
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100, // 100,000 m3 > Thanatos FighterBay
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState::new(1);
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .unwrap();

    // MultiAdd Cargo -> NotEnoughCargoSpace
    let err = handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(trit_id)]),
            PyRep::Integer(venture_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NotEnoughCargoSpace");
    assert!(err.message.contains("free="));

    // MultiAdd DroneBay on Venture -> NotEnoughDroneBaySpace
    let err = handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(drone_id)]),
            PyRep::Integer(venture_id),
            PyRep::Integer(InventoryFlag::DroneBay as i64),
        ],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NotEnoughDroneBaySpace");

    // MultiAdd DroneBay on Rifter (0 m3 drone bay) -> NotEnoughDroneBaySpace (not CapacityExceeded/CannotMoveIntoHold)
    let err = handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(drone_id)]),
            PyRep::Integer(rifter_id),
            PyRep::Integer(InventoryFlag::DroneBay as i64),
        ],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NotEnoughDroneBaySpace");

    // MultiAdd OreHold -> NotEnoughSpecialBaySpace
    let err = handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(ore_id)]),
            PyRep::Integer(venture_id),
            PyRep::Integer(InventoryFlag::SpecializedOreHold as i64),
        ],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NotEnoughSpecialBaySpace");

    // Add & MultiAdd FighterBay -> NotEnoughFighterBaySpace
    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(fighter_id),
                PyRep::Integer(thanatos_id),
                PyRep::Integer(InventoryFlag::FighterBay as i64),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "NotEnoughFighterBaySpace");
    let err = handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(fighter_id)]),
            PyRep::Integer(thanatos_id),
            PyRep::Integer(InventoryFlag::FighterBay as i64),
        ],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "NotEnoughFighterBaySpace");
}

#[tokio::test]
async fn multi_add_autofit_subsystems_structure_guards_ownership_courier_and_session_fallbacks() {
    use eve_db::UpwellStructureRecord;

    let ctx = make_ctx();
    let char_id = 93_000_010;
    let foreign_char = 93_000_011;
    let zero_corp_char = 93_000_012;
    let corp_id = 98_000_040;
    let struct_npc_corp = 1_000_009;
    let station_id = 60_003_760;
    let other_station_id = 60_003_761;
    let sys_id = 30_000_142;
    let loki_id = 1_100_001_001;
    let rifter_id = 1_100_001_002;
    let struct_id = 1_100_001_050;
    let owned_struct_id = 1_100_001_051;

    let mut sess = seed_char(&ctx, char_id, corp_id, Some(station_id), sys_id, loki_id);
    let _ = seed_char(&ctx, foreign_char, corp_id + 1, Some(station_id), sys_id, 1_100_001_003);
    let mut sess_zero = seed_char(&ctx, zero_corp_char, 0, Some(station_id), sys_id, 1_100_001_004);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "MultiAdd Corp".into();
        corp.ticker = "MAD".into();
        corp.ceo_id = char_id;
        corp.members = HashSet::from([char_id]);
        corp.member_roles = HashMap::from([(char_id, u64::MAX)]);
        snap.corporations.insert(corp_id, corp);

        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: struct_npc_corp,
                solar_system_id: sys_id,
                name: "NPC Astrahus".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            owned_struct_id,
            UpwellStructureRecord {
                structure_id: owned_struct_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Owned Astrahus (No Fitting)".into(),
                position: [1_000_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });

    // Items setup
    let subsys_id = 1_100_001_010; // 45607 (category 32 Subsystem)
    let mod_id = 1_100_001_011;    // 492 (category 7 Module)
    let spare_mod = 1_100_001_012; // 492 (category 7 Module for Rifter unfit test)
    let trit_id = 1_100_001_013;   // 34 (category 4 non-fittable)
    let fuel_to_struct = 1_100_001_014; // 16273 Liquid Ozone
    let personal_ore = 1_100_001_015;
    let struct_hi_mod = 1_100_001_016;
    let struct_svc_mod = 1_100_001_017;
    let courier_id = 1_100_001_018;
    let foreign_cont = 1_100_001_019;
    let unowned_item = 1_100_001_020;
    let remote_item = 1_100_001_021;
    let jetcan_id = 1_100_001_022;
    let jetcan_loot = 1_100_001_023;
    let same_loc_trit = 1_100_001_024;
    let space_ship_id = 1_100_001_025;
    let corp_fuel_src = 1_100_001_026;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2; // Omega so T3C subsystem skill (30551) is not Alpha-capped to 0
        }
        let skills = snap.skills.entry(char_id).or_default();
        for tid in [45607_u32, 492_u32] {
            for (skill_tid, lvl) in ctx.sde.required_skills_for_type(tid) {
                skills.insert(
                    skill_tid,
                    eve_db::TrainedSkillRecord {
                        type_id: skill_tid,
                        level: lvl,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
    });
    put_item(&ctx, loki_id, 29990, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, rifter_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, subsys_id, 45607, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, mod_id, 492, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, spare_mod, 492, char_id, rifter_id, InventoryFlag::HiSlot0, 1, true);
    put_item(&ctx, trit_id, 34, char_id, station_id, InventoryFlag::Hangar, 5, false);
    put_item(&ctx, fuel_to_struct, 16273, char_id, loki_id, InventoryFlag::Cargo, 10, false);
    put_item(&ctx, personal_ore, 34, char_id, station_id, InventoryFlag::Hangar, 5, false);
    put_item(&ctx, struct_hi_mod, 492, corp_id, owned_struct_id, InventoryFlag::HiSlot0, 1, true);
    put_item(&ctx, struct_svc_mod, 492, corp_id, owned_struct_id, InventoryFlag::ServiceSlot0, 1, true);
    put_item(&ctx, courier_id, 34, char_id, station_id, InventoryFlag::Hangar, 2, false);
    put_item(&ctx, foreign_cont, 23, foreign_char, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, unowned_item, 34, 0, station_id, InventoryFlag::Hangar, 1, false);
    put_item(&ctx, remote_item, 34, char_id, other_station_id, InventoryFlag::Hangar, 1, false);
    put_item(&ctx, same_loc_trit, 34, char_id, station_id, InventoryFlag::Hangar, 3, false);
    put_item(&ctx, corp_fuel_src, 16273, corp_id, owned_struct_id, InventoryFlag::StructureFuel, 5, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_custom_names.insert(courier_id, "courier_cargo:55".into());
    });

    // 1. Auto-fit without explicit flag: Subsystem (category 32) & Module (category 7) auto-fit; non-fittable (Tritanium) goes to Cargo
    let sub_fit = handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[PyRep::List(vec![PyRep::Integer(subsys_id)]), PyRep::Integer(loki_id)],
        &kw,
    )
    .await
    .expect("auto-fit subsystem");
    assert_eq!(sub_fit.dict_get("movedCount"), Some(&PyRep::Integer(1)));
    let mod_fit = handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[PyRep::List(vec![PyRep::Integer(mod_id)]), PyRep::Integer(loki_id)],
        &kw,
    )
    .await
    .expect("auto-fit module onto loki");
    assert_eq!(mod_fit.dict_get("movedCount"), Some(&PyRep::Integer(1)));
    let cargo_trit = handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[PyRep::List(vec![PyRep::Integer(trit_id)]), PyRep::Integer(loki_id)],
        &kw,
    )
    .await
    .expect("non-fittable moves to Cargo with no flag");
    assert_eq!(cargo_trit.dict_get("movedCount"), Some(&PyRep::Integer(1)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&trit_id].flag), InventoryFlag::Cargo);

    // 2. Unfitting subsystem while Loki has high-slot module -> SubsystemHasFittedModules (kills all 19 validate_multi_add_in_snap FnValues)
    let sub_err = handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(subsys_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(sub_err.code, "SubsystemHasFittedModules");

    // Unfitting spare_mod from Rifter to Hangar while docked succeeds
    let mod_unfit = handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(spare_mod)]),
            PyRep::Integer(station_id),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        &kw,
    )
    .await
    .expect("docked unfit succeeds");
    assert_eq!(mod_unfit.dict_get("movedCount"), Some(&PyRep::Integer(1)));

    // 3. New owner rules:
    // Moving personal_ore into CorpSAG1 (flag 116) -> owner becomes caller corp_id
    handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(personal_ore)]),
            PyRep::Integer(station_id),
            PyRep::Integer(InventoryFlag::CorpSAG1 as i64),
        ],
        &kw,
    )
    .await
    .expect("move into CorpSAG1");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&personal_ore].owner_id), corp_id);

    // Moving personal item (same_loc_trit) to Hangar (4) when caller_corp_id > 0 keeps char_id (kills &&->|| on L351)
    handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(same_loc_trit)]),
            PyRep::Integer(loki_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
    )
    .await
    .expect("move to personal cargo");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&same_loc_trit].owner_id), char_id);

    // 4. Courier cargo to foreign container (flag 5, NOT 116..=122) -> CannotDeliverCourierCargo;
    // to own loki Cargo (flag 5, !foreign_dest) -> succeeds
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            None,
            &[
                PyRep::List(vec![PyRep::Integer(courier_id)]),
                PyRep::Integer(foreign_cont),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CannotDeliverCourierCargo"
    );
    handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(courier_id)]),
            PyRep::Integer(loki_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
    )
    .await
    .expect("courier cargo allowed in own ship cargo");

    // 5. Unowned item (owner 0) moved by zero-corp char (corp_id == 0) -> ItemOwnershipViolation (not CorpRoleDenied)
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess_zero,
            None,
            &[
                PyRep::List(vec![PyRep::Integer(unowned_item)]),
                PyRep::Integer(station_id),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "ItemOwnershipViolation"
    );

    // Non-colocated remote item -> ItemOwnershipViolation
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            None,
            &[
                PyRep::List(vec![PyRep::Integer(remote_item)]),
                PyRep::Integer(station_id),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "ItemOwnershipViolation"
    );

    // Structure fitted modules / services -> InvalidLocation ("Structure modules must be unfitted individually")
    sess.station_id = None;
    sess.structure_id = Some(owned_struct_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
        snap.characters.get_mut(&char_id).unwrap().structure_id = Some(owned_struct_id);
    });
    for mod_in_struct in [struct_hi_mod, struct_svc_mod] {
        assert_eq!(
            handle_multi_add(
                &ctx,
                &mut sess,
                None,
                &[
                    PyRep::List(vec![PyRep::Integer(mod_in_struct)]),
                    PyRep::Integer(owned_struct_id),
                    PyRep::Integer(InventoryFlag::Hangar as i64),
                ],
                &kw,
            )
            .await
            .unwrap_err()
            .code,
            "InvalidLocation"
        );
    }

    // Docked at Upwell structure WITHOUT Fitting Service: unfitting a ship module (`mod_id` on Loki) is rejected
    assert!(
        handle_multi_add(
            &ctx,
            &mut sess,
            None,
            &[
                PyRep::List(vec![PyRep::Integer(mod_id)]),
                PyRep::Integer(owned_struct_id),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ],
            &kw,
        )
        .await
        .is_err()
    );

    // Undocked structure fuel deposit & withdrawal + session ch_dock / ch_sys / caller_ship_id fallbacks + jetcan despawn on old_loc != new_loc
    sess.structure_id = None;
    sess.station_id = None;
    sess.ship_id = None; // tests caller_ship_id fallback to ch.active_ship_id!
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        // Stale ch.station_id with session.station_id == None tests ch_dock == 0 override;
        // ch.solar_system_id == 0 tests ch_sys fallback to session.solar_system_id!
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        ch.solar_system_id = 0;
        ch.active_ship_id = space_ship_id;
        snap.items.insert(
            space_ship_id,
            InventoryItemRecord {
                item_id: space_ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Move loki_id item into space_ship_id cargo fuel source & put ship within 5km of NPC struct_id at [0,0,0]
        snap.items.get_mut(&fuel_to_struct).unwrap().location_id = space_ship_id;
        snap.item_positions.insert(space_ship_id, [100.0, 0.0, 0.0]);
    });
    // Deposit fuel_to_struct -> NPC struct_id owner becomes struct_npc_corp
    handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(fuel_to_struct)]),
            PyRep::Integer(struct_id),
            PyRep::Integer(InventoryFlag::StructureFuel as i64),
        ],
        &kw,
    )
    .await
    .expect("deposit fuel to nearby NPC structure");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&fuel_to_struct].owner_id), struct_npc_corp);

    // Now move space_ship_id near owned_struct_id at [1_000_000, 0, 0] and withdraw corp fuel (src_is_struct_fuel == true)
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(space_ship_id, [1_000_100.0, 0.0, 0.0]);
        // Revoke HangarCanTake1 while keeping StarbaseOperator/Director via corp role? No, CEO bypasses both;
        // set non-CEO member_roles = StarbaseOperator only (not HangarCanTake1)!
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.ceo_id = foreign_char;
        corp.member_roles.insert(char_id, eve_db::corp_roles::ROLE_STARBASE_OPERATOR);
    });
    handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(corp_fuel_src)]),
            PyRep::Integer(space_ship_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
    )
    .await
    .expect("withdraw structure fuel with fuel-only role (src_is_struct_fuel bypasses hangar take role)");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&corp_fuel_src].owner_id), char_id);

    // Looting last item from space jetcan (`old_loc != new_loc`) despawns empty jetcan
    sess.ship_id = Some(space_ship_id);
    put_item(&ctx, jetcan_id, 23, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    put_item(&ctx, jetcan_loot, 34, char_id, jetcan_id, InventoryFlag::Cargo, 4, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(jetcan_id, [1_000_200.0, 0.0, 0.0]);
    });
    handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(jetcan_loot)]),
            PyRep::Integer(space_ship_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
    )
    .await
    .expect("loot last item from jetcan");
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&jetcan_id)));
}

#[tokio::test]
async fn multi_add_multi_item_volume_exact_bounds_across_holds_containers_sma_and_fleet_hangar() {
    let mut sde = (*shared_sde()).clone();
    let ship_tid = 999_201_u32;
    let cont_tid = 999_202_u32;
    let ore_tid = 999_203_u32;
    let cargo_tid = 999_204_u32;
    let hull_tid = 999_205_u32;

    let mut ship_t = sde.types[&587].clone();
    ship_t.type_id = ship_tid;
    ship_t.capacity = 50.0; // Cargo cap = 50.0
    ship_t.default_attributes.insert(1556, 50.0); // Ore Hold cap = 50.0
    ship_t.default_attributes.insert(908, 100.0); // SMA cap = 100.0
    ship_t.default_attributes.insert(912, 50.0);  // Fleet Hangar cap = 50.0
    sde.types.insert(ship_tid, ship_t);

    let mut cont_t = sde.types[&17366].clone();
    cont_t.type_id = cont_tid;
    cont_t.group_id = 12;
    cont_t.category_id = 2;
    cont_t.capacity = 50.0;
    sde.types.insert(cont_tid, cont_t);

    let mut ore_t = sde.types[&1230].clone();
    ore_t.type_id = ore_tid;
    ore_t.category_id = 25;
    ore_t.volume = 10.0;
    sde.types.insert(ore_tid, ore_t);

    let mut cargo_t = sde.types[&34].clone();
    cargo_t.type_id = cargo_tid;
    cargo_t.category_id = 4;
    cargo_t.volume = 10.0;
    sde.types.insert(cargo_tid, cargo_t);

    let mut hull_t = sde.types[&587].clone();
    hull_t.type_id = hull_tid;
    hull_t.category_id = 6;
    hull_t.volume = 40.0;
    sde.types.insert(hull_tid, hull_t);

    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));

    let char_id = 93_000_020;
    let station_id = 60_003_760;
    let sys_id = 30_000_142;
    let carrier_id = 1_100_002_001;
    let rifter_id = 1_100_002_002;
    let cont_id = 1_100_002_003;
    let mut sess = seed_char(&ctx, char_id, 98_000_050, Some(station_id), sys_id, carrier_id);
    let kw = HashMap::new();

    put_item(&ctx, carrier_id, ship_tid, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, rifter_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, cont_id, cont_tid, char_id, station_id, InventoryFlag::Hangar, 1, true);

    // 1. Invalid item type for SpecializedOreHold (Tritanium/cargo_tid cat 4) -> InvalidItemTypeForHold
    let bad_ore_cargo = 1_100_002_010;
    put_item(&ctx, bad_ore_cargo, cargo_tid, char_id, station_id, InventoryFlag::Hangar, 1, false);
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            None,
            &[
                PyRep::List(vec![PyRep::Integer(bad_ore_cargo)]),
                PyRep::Integer(carrier_id),
                PyRep::Integer(InventoryFlag::SpecializedOreHold as i64),
            ],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "InvalidItemTypeForHold"
    );

    // Rifter OreHold capacity == 0.0 -> NotEnoughSpecialBaySpace
    let test_ore = 1_100_002_011;
    put_item(&ctx, test_ore, ore_tid, char_id, station_id, InventoryFlag::Hangar, 1, false);
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            None,
            &[
                PyRep::List(vec![PyRep::Integer(test_ore)]),
                PyRep::Integer(rifter_id),
                PyRep::Integer(InventoryFlag::SpecializedOreHold as i64),
            ],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughSpecialBaySpace"
    );

    // 2. Ship Hold (Cargo flag 5, cap 50.0 m3): pre-existing qty 2 (20 m3) + unrelated flag item (qty 5 in DroneBay);
    // batch [qty 2 (20 m3), qty 2 (20 m3)] individually fit (20+20=40 <= 50), combined overflow (20+40=60 > 50);
    // exact batch [qty 2 (20 m3), qty 1 (10 m3)] -> 20+30 = 50.0 == 50.0 succeeds; re-moving both items tests !item_ids.contains!
    let c_existing = 1_100_002_020;
    let c_b1 = 1_100_002_021;
    let c_b2 = 1_100_002_022;
    let c_b_ok = 1_100_002_023;
    let c_other_flag = 1_100_002_024;
    put_item(&ctx, c_existing, cargo_tid, char_id, carrier_id, InventoryFlag::Cargo, 2, false);
    put_item(&ctx, c_other_flag, 2454, char_id, carrier_id, InventoryFlag::DroneBay, 1, false);
    put_item(&ctx, c_b1, cargo_tid, char_id, station_id, InventoryFlag::Hangar, 2, false);
    put_item(&ctx, c_b2, cargo_tid, char_id, station_id, InventoryFlag::Hangar, 2, false);
    put_item(&ctx, c_b_ok, cargo_tid, char_id, station_id, InventoryFlag::Hangar, 1, false);

    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            None,
            &[
                PyRep::List(vec![PyRep::Integer(c_b1), PyRep::Integer(c_b2)]),
                PyRep::Integer(carrier_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughCargoSpace"
    );
    handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(c_b1), PyRep::Integer(c_b_ok)]),
            PyRep::Integer(carrier_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
    )
    .await
    .expect("exact 50.0 == 50.0 ship cargo batch");
    // Re-add c_existing + c_b1 (40 m3 total out of 50) already inside Cargo -> succeeds only if !item_ids.contains excludes them from used_vol
    handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(c_existing), PyRep::Integer(c_b1)]),
            PyRep::Integer(carrier_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
    )
    .await
    .expect("!item_ids.contains excludes items being moved within same cargo");

    // 3. Container (`cont_tid`, cap 50.0 m3): pre-existing qty 2 (20 m3), batch [qty 2, qty 2] fails; [qty 2, qty 1] succeeds; self re-add excludes item_ids
    let k_existing = 1_100_002_030;
    let k_b1 = 1_100_002_031;
    let k_b2 = 1_100_002_032;
    let k_b_ok = 1_100_002_033;
    put_item(&ctx, k_existing, cargo_tid, char_id, cont_id, InventoryFlag::Cargo, 2, false);
    put_item(&ctx, k_b1, cargo_tid, char_id, station_id, InventoryFlag::Hangar, 2, false);
    put_item(&ctx, k_b2, cargo_tid, char_id, station_id, InventoryFlag::Hangar, 2, false);
    put_item(&ctx, k_b_ok, cargo_tid, char_id, station_id, InventoryFlag::Hangar, 1, false);
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            None,
            &[
                PyRep::List(vec![PyRep::Integer(k_b1), PyRep::Integer(k_b2)]),
                PyRep::Integer(cont_id),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CapacityExceeded"
    );
    handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(k_b1), PyRep::Integer(k_b_ok)]),
            PyRep::Integer(cont_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
    )
    .await
    .expect("exact 50.0 == 50.0 container batch");
    handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(k_existing), PyRep::Integer(k_b1)]),
            PyRep::Integer(cont_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
    )
    .await
    .expect("!item_ids.contains in container");

    // 4. Ship Maintenance Bay (flag 90, cap 100.0 m3): pre-existing hull (40 m3) in flag 138 (`SpecializedShipHold`),
    // batch [hull_1 (40 m3), hull_2 (40 m3)] fails (40 + 80 = 120 > 100) while [hull_1 (40), hull_3 (20? use 40+40=80)] fits, and exact [hull_1, existing_in_90] self-move passes!
    let s_138 = 1_100_002_040;
    let s_h1 = 1_100_002_041;
    let s_h2 = 1_100_002_042;
    put_item(&ctx, s_138, hull_tid, char_id, carrier_id, InventoryFlag::SpecializedShipHold, 1, true);
    put_item(&ctx, s_h1, hull_tid, char_id, station_id, InventoryFlag::Hangar, 1, true);
    put_item(&ctx, s_h2, hull_tid, char_id, station_id, InventoryFlag::Hangar, 1, true);
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            None,
            &[
                PyRep::List(vec![PyRep::Integer(s_h1), PyRep::Integer(s_h2)]),
                PyRep::Integer(carrier_id),
                PyRep::Integer(InventoryFlag::ShipHangar as i64),
            ],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CapacityExceeded"
    );
    handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(s_h1)]),
            PyRep::Integer(carrier_id),
            PyRep::Integer(InventoryFlag::ShipHangar as i64),
        ],
        &kw,
    )
    .await
    .expect("SMA fits 40 (flag 138) + 40 (flag 90) = 80 <= 100");
    handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(s_138), PyRep::Integer(s_h1)]),
            PyRep::Integer(carrier_id),
            PyRep::Integer(InventoryFlag::ShipHangar as i64),
        ],
        &kw,
    )
    .await
    .expect("!item_ids.contains in SMA (80 + 80 would exceed 100)");

    // 5. Fleet Hangar (flag 155, cap 50.0 m3): pre-existing qty 2 (20 m3) in flag 155, batch [qty 2, qty 2] fails,
    // [qty 2, qty 1] succeeds (20+30 = 50.0), self re-add passes!
    let f_existing = 1_100_002_050;
    let f_b1 = 1_100_002_051;
    let f_b2 = 1_100_002_052;
    let f_b_ok = 1_100_002_053;
    put_item(&ctx, f_existing, cargo_tid, char_id, carrier_id, InventoryFlag::FleetHangar, 2, false);
    put_item(&ctx, f_b1, cargo_tid, char_id, station_id, InventoryFlag::Hangar, 2, false);
    put_item(&ctx, f_b2, cargo_tid, char_id, station_id, InventoryFlag::Hangar, 2, false);
    put_item(&ctx, f_b_ok, cargo_tid, char_id, station_id, InventoryFlag::Hangar, 1, false);
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            None,
            &[
                PyRep::List(vec![PyRep::Integer(f_b1), PyRep::Integer(f_b2)]),
                PyRep::Integer(carrier_id),
                PyRep::Integer(InventoryFlag::FleetHangar as i64),
            ],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CapacityExceeded"
    );
    handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(f_b1), PyRep::Integer(f_b_ok)]),
            PyRep::Integer(carrier_id),
            PyRep::Integer(InventoryFlag::FleetHangar as i64),
        ],
        &kw,
    )
    .await
    .expect("exact 50.0 == 50.0 Fleet Hangar batch");
    handle_multi_add(
        &ctx,
        &mut sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(f_existing), PyRep::Integer(f_b1)]),
            PyRep::Integer(carrier_id),
            PyRep::Integer(InventoryFlag::FleetHangar as i64),
        ],
        &kw,
    )
    .await
    .expect("!item_ids.contains in Fleet Hangar");
}
