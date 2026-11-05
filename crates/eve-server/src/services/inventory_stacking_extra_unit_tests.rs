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
            CorporationRecord {
                corporation_id: corp_id,
                name: "Test Corp".into(),
                ticker: "TSTC".into(),
                ceo_id: char_id + 99,
                members: HashSet::from([char_id]),
                member_roles: HashMap::from([(char_id, 0)]),
                master_wallet: IskCents::ZERO,
                ..Default::default()
            },
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
            CorporationRecord {
                corporation_id: corp_id,
                name: "List Corp".into(),
                ticker: "LSTC".into(),
                ceo_id: corp_mate_id,
                members: HashSet::from([char_id, corp_mate_id]),
                member_roles: HashMap::from([(char_id, 0)]),
                master_wallet: IskCents::ZERO,
                ..Default::default()
            },
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
