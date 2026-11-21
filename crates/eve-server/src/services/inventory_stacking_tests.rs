#![forbid(unsafe_code)]

use super::*;
use crate::router::{context::BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::{self, SafetyLevel};
use crate::services::inventory_fitting::fitting_containers_ships::{
    clear_container_unlocks_snap, container_alsc_logs_snap, is_container_unlocked_snap,
    set_alsc_item_locked_snap, set_container_config_snap, set_container_password_snap,
    set_container_unlocked_snap, set_ship_access_config_snap, ship_access_config_snap,
};
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, VirtualServerClock, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn test_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

fn ensure_char(ctx: &ServerContext, char_id: i64, corp_id: i64, station_id: Option<i64>, sys_id: u32, ship_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo seed character");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.station_id = station_id;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.security_status = 5.0;
        ch.clone_grade = 2;
        ch.fleet_id = None;
        snap.characters.entry(char_id).or_insert(ch);
        snap.accounts.insert(
            format!("user_{char_id}"),
            eve_db::AccountRecord {
                user_id: char_id as u64,
                username: format!("user_{char_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    let _ = ctx
        .db
        .acquire_online_lock(char_id as u64, char_id as u64, Some(char_id), true, ctx.now_unix_secs());
}

fn make_session(char_id: i64, corp_id: i64, station_id: Option<i64>, sys_id: Option<u32>, ship_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id,
        structure_id: None,
        solar_system_id: sys_id,
        ship_id,
        ..Default::default()
    }
}

fn make_bound(object_id: i64, char_id: i64) -> BoundObjectEntry {
    BoundObjectEntry {
        bound_id: 10_001,
        session_id: 1,
        user_id: char_id as u64,
        character_id: char_id,
        service: "invbroker".into(),
        object_id,
        oid_str: format!("N=10001:{object_id}"),
        ref_id: 1,
    }
}

fn insert_item(
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
fn split_stack_multi_merge_and_stack_all_happy_and_error_paths() {
    let ctx = test_ctx();
    let char_id = 92_000_001;
    let corp_id = 98_000_002;
    let station_id = 60_003_760;
    let ship_id = 1_000_200_001;
    let tritanium_stack_a = 1_000_200_002;
    let tritanium_stack_b = 1_000_200_003;
    let locked_stack = 1_000_200_004;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), 30_000_142, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, tritanium_stack_a, 34, char_id, station_id, InventoryFlag::Hangar, 1_000, false);
    insert_item(&ctx, tritanium_stack_b, 34, char_id, station_id, InventoryFlag::Hangar, 500, false);
    insert_item(&ctx, locked_stack, 34, char_id, station_id, InventoryFlag::Hangar, 200, false);

    let kw = HashMap::new();
    let mut sess = make_session(char_id, corp_id, Some(station_id), None, Some(ship_id));
    let bound = make_bound(station_id, char_id);

    // 1. SplitStack via contractProxy: splits 250 units out of 1000
    let split_rep = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&bound),
        "contractProxy",
        "SplitStack",
        &[PyRep::Integer(station_id), PyRep::Integer(tritanium_stack_a), PyRep::Integer(250)],
        &kw,
    )
    .expect("matched")
    .expect("split succeeded");
    assert_eq!(split_rep.dict_get("originalQuantity").and_then(|v| v.as_i64()), Some(750));
    assert_eq!(split_rep.dict_get("newQuantity").and_then(|v| v.as_i64()), Some(250));
    let new_split_id = split_rep.dict_get("newItemID").and_then(|v| v.as_i64()).unwrap();

    // Two OnItemChange notifications emitted
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().filter(|n| n.event == "OnItemChange").count() >= 2);

    // Invalid split quantity fails with SplitFailed
    let bad_split = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&bound),
        "invbroker",
        "SplitStack",
        &[PyRep::Integer(tritanium_stack_a), PyRep::Integer(99_999)],
        &kw,
    )
    .expect("matched")
    .unwrap_err();
    assert_eq!(bad_split.code, "SplitFailed");

    // 2. MultiMerge: partial merge 100 from new_split_id into tritanium_stack_b
    let merge_op = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(new_split_id),
        PyRep::Integer(tritanium_stack_b),
        PyRep::Integer(100),
    ])]);
    let merge_rep = handle_stack_ops(&ctx, &mut sess, Some(&bound), "invbroker", "MultiMerge", &[merge_op], &kw)
        .expect("matched")
        .expect("partial merge");
    assert_eq!(merge_rep.dict_get("quantity").and_then(|v| v.as_i64()), Some(600));

    // ALSC locked stack rejected in MultiMerge
    ctx.db.with_mut_snapshot(|snap| {
        set_alsc_item_locked_snap(snap, locked_stack, true, 0).unwrap();
    });
    let alsc_err = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&bound),
        "invbroker",
        "MultiMerge",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(locked_stack),
            PyRep::Integer(tritanium_stack_b),
        ])])],
        &kw,
    )
    .expect("matched")
    .unwrap_err();
    assert_eq!(alsc_err.code, "ALSCItemLocked");
    ctx.db.with_mut_snapshot(|snap| {
        set_alsc_item_locked_snap(snap, locked_stack, false, 0).unwrap();
    });

    // 3. StackAll merges remaining Tritanium stacks in station hangar
    let stack_rep = handle_stack_ops(&ctx, &mut sess, Some(&bound), "invbroker", "StackAll", &[], &kw)
        .expect("matched")
        .expect("stack all");
    assert!(stack_rep.dict_get("mergedCount").is_some() || stack_rep.dict_get("stacked").is_some());

    // Unhandled method returns None
    assert!(handle_stack_ops(&ctx, &mut sess, Some(&bound), "invbroker", "GetItemDescriptor", &[], &kw).is_none());
}

#[tokio::test]
async fn multi_add_cargo_transfer_and_fitting_rejection() {
    let ctx = test_ctx();
    let char_id = 92_000_010;
    let corp_id = 98_000_002;
    let station_id = 60_003_760;
    let ship_id = 1_000_200_101;
    let tritium_id = 1_000_200_102;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), 30_000_142, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, tritium_id, 17888, char_id, station_id, InventoryFlag::Hangar, 100, false);

    let kw = HashMap::new();
    let mut sess = make_session(char_id, corp_id, Some(station_id), None, Some(ship_id));
    let ship_bound = make_bound(ship_id, char_id);

    // Reject fitting module flag (HiSlot0 = 27)
    let fit_err = handle_multi_add(
        &ctx,
        &mut sess,
        Some(&ship_bound),
        &[
            PyRep::List(vec![PyRep::Integer(tritium_id)]),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::HiSlot0 as i64),
        ],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(fit_err.code, "InvalidLocation");

    // Move Tritanium from station hangar into active ship Cargo
    let move_ok = handle_multi_add(
        &ctx,
        &mut sess,
        Some(&ship_bound),
        &[
            PyRep::List(vec![PyRep::Integer(tritium_id)]),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
    )
    .await
    .expect("multi_add to cargo");
    assert_eq!(move_ok.dict_get("movedCount").and_then(|v| v.as_i64()), Some(1));
    ctx.db.with_snapshot(|snap| {
        let item = snap.items.get(&tritium_id).unwrap();
        assert_eq!(item.location_id, ship_id);
        assert_eq!(item.flag, InventoryFlag::Cargo);
    });
}

#[test]
fn evaluate_space_wreck_or_jetcan_loot_and_suspect_flag() {
    let ctx = test_ctx();
    let owner_id = 92_000_020;
    let looter_id = 92_000_021;
    let fleet_mate_id = 92_000_022;
    let sys_id = 30_000_142;
    let jetcan_id = 1_000_200_201;
    let loot_item_id = 1_000_200_202;

    ensure_char(&ctx, owner_id, 98_000_010, None, sys_id, 1_000_200_210);
    ensure_char(&ctx, looter_id, 98_000_011, None, sys_id, 1_000_200_211);
    ensure_char(&ctx, fleet_mate_id, 98_000_012, None, sys_id, 1_000_200_212);
    insert_item(&ctx, jetcan_id, 23, owner_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, loot_item_id, 34, owner_id, jetcan_id, InventoryFlag::Cargo, 50, false);

    let loot_rec = ctx.db.with_snapshot(|snap| snap.items.get(&loot_item_id).cloned().unwrap());

    // Owner taking own loot is allowed without suspect
    assert_eq!(
        evaluate_space_wreck_or_jetcan_loot(&ctx, owner_id, &loot_rec).unwrap(),
        (true, false)
    );

    // Stranger with Green safety is blocked
    combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(looter_id, SafetyLevel::Green);
    });
    assert!(evaluate_space_wreck_or_jetcan_loot(&ctx, looter_id, &loot_rec).is_err());

    // Stranger with Yellow safety triggers suspect flag
    combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(looter_id, SafetyLevel::Yellow);
    });
    assert_eq!(
        evaluate_space_wreck_or_jetcan_loot(&ctx, looter_id, &loot_rec).unwrap(),
        (true, true)
    );

    let sess = make_session(looter_id, 98_000_011, None, Some(sys_id), Some(1_000_200_211));
    apply_space_loot_suspect_flag(&ctx, &sess, looter_id);
    assert!(ctx
        .crimewatch
        .read()
        .unwrap()
        .states
        .get(&looter_id)
        .is_some_and(|st| st.suspect_flag_until_unix > ctx.now_unix_secs()));

    // Fleet mate can loot without suspect flag
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            700_001,
            FleetRecord {
                fleet_id: 700_001,
                boss_id: owner_id,
                members: vec![owner_id, fleet_mate_id],
                ..Default::default()
            },
        );
    });
    assert_eq!(
        evaluate_space_wreck_or_jetcan_loot(&ctx, fleet_mate_id, &loot_rec).unwrap(),
        (true, false)
    );
}

#[tokio::test]
async fn get_container_contents_alsc_password_list_and_empty_jetcan_despawn() {
    let ctx = test_ctx();
    let char_id = 92_000_030;
    let corp_id = 98_000_002;
    let station_id = 60_003_760;
    let sys_id = 30_000_142;
    let alsc_id = 1_000_200_301;
    let inner_item_id = 1_000_200_302;
    let empty_jetcan_id = 1_000_200_303;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), sys_id, 1_000_200_300);
    // 17366 = Audit Log Secure Container (group 448)
    insert_item(&ctx, alsc_id, 17366, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, inner_item_id, 34, char_id, alsc_id, InventoryFlag::Cargo, 42, false);
    insert_item(&ctx, empty_jetcan_id, 23, char_id, sys_id as i64, InventoryFlag::None, 1, true);

    ctx.db.with_mut_snapshot(|snap| {
        set_container_password_snap(snap, alsc_id, 1, "secret123", 0).unwrap();
    });

    let mut sess = make_session(char_id, corp_id, Some(station_id), None, None);
    let mut kw = HashMap::new();

    // Missing password -> ALSCPasswordRequired
    let pw_err = handle_get_container_contents(&ctx, &sess, None, &[PyRep::Integer(alsc_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(pw_err.code, "ALSCPasswordRequired");

    // Supplying password unlocks and records ALSCActionOpen log
    kw.insert("password".into(), PyRep::String("secret123".into()));
    assert!(
        handle_get_container_contents(&ctx, &mut sess, None, &[PyRep::Integer(alsc_id)], &kw)
            .await
            .is_ok()
    );
    ctx.db.with_snapshot(|snap| {
        assert!(is_container_unlocked_snap(snap, alsc_id, char_id));
        assert_eq!(container_alsc_logs_snap(snap, alsc_id).len(), 1);
    });

    // List & ListByFlags on station hangar
    let empty_kw = HashMap::new();
    let station_bound = make_bound(station_id, char_id);
    let list_res = handle_list(
        &ctx,
        &sess,
        Some(&station_bound),
        &[PyRep::Integer(InventoryFlag::Hangar as i64)],
        &empty_kw,
    )
    .expect("handle_list");
    match list_res {
        PyRep::ObjectEx { list, .. } => assert!(!list.is_empty()),
        other => panic!("expected rowset from handle_list, got {other:?}"),
    }
    let flags_res = handle_list_by_flags(
        &ctx,
        &sess,
        Some(&station_bound),
        &[PyRep::List(vec![PyRep::Integer(InventoryFlag::Hangar as i64)])],
        &empty_kw,
    )
    .expect("handle_list_by_flags");
    match flags_res {
        PyRep::ObjectEx { list, .. } => assert!(!list.is_empty()),
        other => panic!("expected rowset from handle_list_by_flags, got {other:?}"),
    }

    // Empty jetcan despawn removes item and emits RemoveBall DoDestinyUpdate
    assert!(!despawn_empty_jetcan_if_needed(&ctx, Some(sys_id), -1).await);
    assert!(despawn_empty_jetcan_if_needed(&ctx, Some(sys_id), empty_jetcan_id).await);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&empty_jetcan_id));
    });
}

fn dbrow_col_i64(row: &PyRep, col: &str) -> Option<i64> {
    let PyRep::DBRow { columns, .. } = row else {
        return None;
    };
    columns
        .iter()
        .find(|(name, _)| name == col)
        .and_then(|(_, val)| val.as_i64())
}

#[tokio::test]
async fn multi_merge_and_stack_all_emit_full_dbrow_item_change_notifications() {
    let ctx = test_ctx();
    let char_id = 92_000_050;
    let corp_id = 98_000_002;
    let station_id = 60_003_760;
    let ship_id = 1_000_250_001;
    let stack_a = 1_000_250_002;
    let stack_b = 1_000_250_003;
    let stack_c = 1_000_250_004;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), 30_000_142, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, stack_a, 34, char_id, station_id, InventoryFlag::Hangar, 100, false);
    insert_item(&ctx, stack_b, 34, char_id, station_id, InventoryFlag::Hangar, 50, false);
    insert_item(&ctx, stack_c, 34, char_id, station_id, InventoryFlag::Hangar, 40, false);

    let kw = HashMap::new();
    let mut sess = make_session(char_id, corp_id, Some(station_id), None, Some(ship_id));
    let station_bound = make_bound(station_id, char_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Full MultiMerge: stack_b (50) -> stack_a (100) => stack_a = 150, stack_b removed
    handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&station_bound),
        "invbroker",
        "MultiMerge",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(stack_b),
            PyRep::Integer(stack_a),
        ])])],
        &kw,
    )
    .expect("matched")
    .expect("full MultiMerge ok");

    let notifs = ctx.drain_notifications_for_char(char_id);
    let item_changes: Vec<_> = notifs.iter().filter(|n| n.event == "OnItemChange").collect();
    assert_eq!(item_changes.len(), 2, "expected 2 OnItemChange for full merge");
    let del = item_changes
        .iter()
        .find(|n| dbrow_col_i64(&n.args[0], "itemID") == Some(stack_b))
        .expect("deleted source notification");
    assert_eq!(dbrow_col_i64(&del.args[0], "locationID"), Some(6));
    assert_eq!(dbrow_col_i64(&del.args[0], "quantity"), Some(0));
    assert_ne!(dbrow_col_i64(&del.args[0], "groupID").unwrap_or(0), 0);
    assert_ne!(dbrow_col_i64(&del.args[0], "categoryID").unwrap_or(0), 0);
    assert_eq!(dbrow_col_i64(&del.args[0], "ownerID"), Some(char_id));
    let surv = item_changes
        .iter()
        .find(|n| dbrow_col_i64(&n.args[0], "itemID") == Some(stack_a))
        .expect("survivor notification");
    assert_eq!(dbrow_col_i64(&surv.args[0], "quantity"), Some(150));

    // 2. Partial MultiMerge: 15 from stack_c (40) -> stack_a (150) => stack_c=25, stack_a=165
    handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&station_bound),
        "invbroker",
        "MultiMerge",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(stack_c),
            PyRep::Integer(stack_a),
            PyRep::Integer(15),
        ])])],
        &kw,
    )
    .expect("matched")
    .expect("partial MultiMerge ok");
    let p_notifs = ctx.drain_notifications_for_char(char_id);
    let p_changes: Vec<_> = p_notifs.iter().filter(|n| n.event == "OnItemChange").collect();
    assert_eq!(p_changes.len(), 2, "expected 2 OnItemChange for partial merge");
    let rem = p_changes
        .iter()
        .find(|n| dbrow_col_i64(&n.args[0], "itemID") == Some(stack_c))
        .expect("remainder notification");
    assert_eq!(dbrow_col_i64(&rem.args[0], "quantity"), Some(25));
    let p_surv = p_changes
        .iter()
        .find(|n| dbrow_col_i64(&n.args[0], "itemID") == Some(stack_a))
        .expect("survivor notification");
    assert_eq!(dbrow_col_i64(&p_surv.args[0], "quantity"), Some(165));

    // 3. StackAll merges stack_c (25) into stack_a (165 -> 190) and emits full DBRows
    handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&station_bound),
        "invbroker",
        "StackAll",
        &[PyRep::Integer(InventoryFlag::Hangar as i64)],
        &kw,
    )
    .expect("matched")
    .expect("StackAll ok");
    let sa_notifs = ctx.drain_notifications_for_char(char_id);
    let sa_changes: Vec<_> = sa_notifs.iter().filter(|n| n.event == "OnItemChange").collect();
    assert_eq!(sa_changes.len(), 2, "expected 2 OnItemChange for StackAll");
    assert!(sa_changes.iter().any(|n| {
        dbrow_col_i64(&n.args[0], "itemID") == Some(stack_c)
            && dbrow_col_i64(&n.args[0], "locationID") == Some(6)
    }));
    assert!(sa_changes.iter().any(|n| {
        dbrow_col_i64(&n.args[0], "itemID") == Some(stack_a)
            && dbrow_col_i64(&n.args[0], "quantity") == Some(190)
    }));

    // 4. Subsequent MultiAdd of surviving stack_a to ship cargo succeeds and emits OnItemChange
    let ship_bound = make_bound(ship_id, char_id);
    handle_multi_add(
        &ctx,
        &mut sess,
        Some(&ship_bound),
        &[
            PyRep::List(vec![PyRep::Integer(stack_a)]),
            PyRep::Integer(station_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
    )
    .await
    .expect("subsequent move of merged survivor succeeds");
    let m_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(m_notifs.iter().any(|n| {
        n.event == "OnItemChange"
            && dbrow_col_i64(&n.args[0], "itemID") == Some(stack_a)
            && dbrow_col_i64(&n.args[0], "locationID") == Some(ship_id)
            && dbrow_col_i64(&n.args[0], "flagID") == Some(InventoryFlag::Cargo as i64)
    }));
}

#[tokio::test]
async fn multi_add_auto_fit_splits_stacked_module_and_emits_notifications() {
    let ctx = test_ctx();
    let char_id = 92_000_060;
    let corp_id = 98_000_002;
    let station_id = 60_003_760;
    let ship_id = 1_000_260_001;
    let gun_stack_id = 1_000_260_002;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), 30_000_142, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, gun_stack_id, 484, char_id, station_id, InventoryFlag::Hangar, 5, false);
    ctx.db.with_mut_snapshot(|snap| {
        for &sk in &[3300u32, 3302, 3318, 3392, 3426] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                eve_db::TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    let kw = HashMap::new();
    let mut sess = make_session(char_id, corp_id, Some(station_id), None, Some(ship_id));
    let ship_bound = make_bound(ship_id, char_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Fit first unit from 5x stack with flag = 0 (flagAutoFit)
    let fit1 = handle_multi_add(
        &ctx,
        &mut sess,
        Some(&ship_bound),
        &[
            PyRep::List(vec![PyRep::Integer(gun_stack_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .await
    .expect("auto-fit 1st module from stack");
    let PyRep::List(items1) = fit1.dict_get("items").unwrap() else {
        panic!("expected items list");
    };
    let fitted1_id = items1[0].as_i64().unwrap();
    assert_ne!(fitted1_id, gun_stack_id);

    ctx.db.with_snapshot(|snap| {
        let orig = &snap.items[&gun_stack_id];
        assert_eq!(orig.location_id, station_id);
        assert_eq!(orig.flag, InventoryFlag::Hangar);
        assert_eq!(orig.quantity, 4);
        let fitted = &snap.items[&fitted1_id];
        assert_eq!(fitted.location_id, ship_id);
        assert_eq!(fitted.flag, InventoryFlag::HiSlot0);
        assert_eq!(fitted.quantity, 1);
        assert!(fitted.singleton);
        assert_eq!(snap.module_online_states.get(&fitted1_id).copied(), Some(true));
    });

    let n1 = ctx.drain_notifications_for_char(char_id);
    let ic1: Vec<_> = n1.iter().filter(|n| n.event == "OnItemChange").collect();
    assert!(ic1.iter().any(|n| {
        dbrow_col_i64(&n.args[0], "itemID") == Some(gun_stack_id)
            && dbrow_col_i64(&n.args[0], "quantity") == Some(4)
    }));
    assert!(ic1.iter().any(|n| {
        dbrow_col_i64(&n.args[0], "itemID") == Some(fitted1_id)
            && dbrow_col_i64(&n.args[0], "locationID") == Some(ship_id)
            && dbrow_col_i64(&n.args[0], "flagID") == Some(InventoryFlag::HiSlot0 as i64)
            && dbrow_col_i64(&n.args[0], "quantity") == Some(-1)
    }));
    assert!(n1.iter().any(|n| n.event == "OnModuleFitted"));

    // 2. Fit second unit from remaining 4x stack (with flag kwarg = 0) -> lands in HiSlot1 (28)
    let mut kw2 = HashMap::new();
    kw2.insert("flag".into(), PyRep::Integer(0));
    let fit2 = handle_multi_add(
        &ctx,
        &mut sess,
        Some(&ship_bound),
        &[
            PyRep::List(vec![PyRep::Integer(gun_stack_id)]),
            PyRep::Integer(station_id),
        ],
        &kw2,
    )
    .await
    .expect("auto-fit 2nd module into HiSlot1");
    let PyRep::List(items2) = fit2.dict_get("items").unwrap() else {
        panic!("expected items list");
    };
    let fitted2_id = items2[0].as_i64().unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&gun_stack_id].quantity, 3);
        assert_eq!(snap.items[&fitted2_id].flag, InventoryFlag::HiSlot1);
    });
}

fn change_dict_has_key(rep: &PyRep, target_key: i64) -> bool {
    let PyRep::Dict(entries) = rep else {
        return false;
    };
    entries.iter().any(|(k, _)| k.as_i64() == Some(target_key))
}

fn change_dict_get_i64(rep: &PyRep, target_key: i64) -> Option<i64> {
    let PyRep::Dict(entries) = rep else {
        return None;
    };
    entries
        .iter()
        .find(|(k, _)| k.as_i64() == Some(target_key))
        .and_then(|(_, v)| v.as_i64())
}

#[tokio::test]
async fn item_change_notifications_omit_ix_quantity_and_autofit_syncs_singleton_and_load() {
    let ctx = test_ctx();
    let char_id = 90_000_261;
    let corp_id = 98_000_002;
    let station_id = 60_003_760;
    let ship_id = 1_000_261_001;
    let stack_a = 1_000_261_002;
    let stack_b = 1_000_261_003;
    let single_unpackaged_gun = 1_000_261_004;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), 30_000_142, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, stack_a, 484, char_id, station_id, InventoryFlag::Hangar, 5, false);
    insert_item(&ctx, stack_b, 484, char_id, station_id, InventoryFlag::Hangar, 2, false);
    insert_item(
        &ctx,
        single_unpackaged_gun,
        484,
        char_id,
        station_id,
        InventoryFlag::Hangar,
        1,
        false,
    );
    ctx.db.with_mut_snapshot(|snap| {
        for &sk in &[3300u32, 3302, 3318, 3392, 3426] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                eve_db::TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    let kw = HashMap::new();
    let mut sess = make_session(char_id, corp_id, Some(station_id), None, Some(ship_id));
    let hangar_bound = make_bound(station_id, char_id);
    let ship_bound = make_bound(ship_id, char_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. MultiAdd flagAutoFit on an unpackaged qty=1 module notifies ixSingleton (10), online effect, & load change
    handle_multi_add(
        &ctx,
        &mut sess,
        Some(&ship_bound),
        &[
            PyRep::List(vec![PyRep::Integer(single_unpackaged_gun)]),
            PyRep::Integer(station_id),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .await
    .expect("autofit unpackaged singleton gun");
    let n_auto = ctx.drain_notifications_for_char(char_id);
    let ic_auto: Vec<_> = n_auto.iter().filter(|n| n.event == "OnItemChange").collect();
    assert!(ic_auto.iter().all(|n| !change_dict_has_key(&n.args[1], 5)));
    assert!(
        ic_auto.iter().any(|n| {
            dbrow_col_i64(&n.args[0], "itemID") == Some(single_unpackaged_gun)
                && change_dict_get_i64(&n.args[1], 10) == Some(1)
        }),
        "unpackaged autofit must emit ixSingleton=10 change"
    );
    assert!(
        n_auto.iter().any(|n| {
            n.event == "OnModuleAttributeChanges"
                && matches!(n.args.first(), Some(PyRep::List(items)) if items.iter().any(|it| {
                    matches!(
                        it,
                        PyRep::Tuple(t)
                            if t.get(2).and_then(|v| v.as_i64()) == Some(single_unpackaged_gun)
                            && t.get(3).and_then(|v| v.as_i64()) == Some(2)
                            && t.get(5).and_then(|v| v.as_f64()) == Some(1.0)
                    )
                }))
        }),
        "autofit online module must emit OnModuleAttributeChanges (online=1.0)"
    );
    assert!(
        n_auto.iter().any(|n| {
            n.event == "OnGodmaShipEffect"
                && n.args.first().and_then(|v| v.as_i64()) == Some(single_unpackaged_gun)
                && n.args.get(1).and_then(|v| v.as_i64()) == Some(16)
        }),
        "autofit online module must emit OnGodmaShipEffect(online=16)"
    );
    assert!(
        n_auto.iter().any(|n| {
            n.event == "OnModuleAttributeChanges"
                && matches!(n.args.first(), Some(PyRep::List(items)) if items.iter().any(|it| {
                    matches!(it, PyRep::Tuple(t) if t.get(2).and_then(|v| v.as_i64()) == Some(ship_id))
                }))
        }),
        "autofit online module must emit ship CPU/PG load change"
    );
    assert!(
        n_auto.iter().filter(|n| n.event == "OnModuleAttributeChanges").all(|n| {
            matches!(n.args.first(), Some(PyRep::List(items)) if items.iter().all(|it| {
                matches!(
                    it,
                    PyRep::Tuple(t)
                        if t.len() == 8
                        && t.get(1).and_then(|v| v.as_i64()).is_some_and(|oid| oid > 0)
                        && t.get(3).and_then(|v| v.as_i64()).is_some_and(|aid| aid > 0)
                        && !matches!(t.get(7), Some(PyRep::Bool(_)))
                )
            }))
        }),
        "all OnModuleAttributeChanges tuples must have valid ownerID, non-zero attributeID, and non-bool wallclockTime (#799)"
    );

    // 2. Fill remaining turret hardpoints on Rifter, then verify extra turret fails cleanly
    let max_hi = ctx
        .sde
        .types
        .get(&587)
        .map(|t| {
            let hi = t.default_attributes.get(&eve_sde::attr_ids::HI_SLOTS).copied().unwrap_or(0.0) as usize;
            let tur = t.default_attributes.get(&eve_sde::attr_ids::TURRET_SLOTS_LEFT).copied().unwrap_or(0.0) as usize;
            hi.min(tur)
        })
        .unwrap_or(3);
    for _ in 1..max_hi {
        handle_multi_add(
            &ctx,
            &mut sess,
            Some(&ship_bound),
            &[
                PyRep::List(vec![PyRep::Integer(stack_a)]),
                PyRep::Integer(station_id),
                PyRep::Integer(0),
            ],
            &kw,
        )
        .await
        .expect("autofit within hull capacity");
    }
    let over_err = handle_multi_add(
        &ctx,
        &mut sess,
        Some(&ship_bound),
        &[
            PyRep::List(vec![PyRep::Integer(stack_a)]),
            PyRep::Integer(station_id),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .await
    .expect_err("over-capacity turret autofit must fail");
    assert_eq!(over_err.code, "FittingValidationFailed");

    let _ = ctx.drain_notifications_for_char(char_id);

    // 3. SplitStack, MultiMerge, and StackAll must emit ixStackSize (9) and NEVER ixQuantity (5)
    super::inventory_stacking_ops::handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&hangar_bound),
        "invbroker",
        "SplitStack",
        &[PyRep::Integer(stack_a), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .expect("SplitStack ok");
    super::inventory_stacking_ops::handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&hangar_bound),
        "invbroker",
        "MultiMerge",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(stack_b),
            PyRep::Integer(stack_a),
            PyRep::Integer(1),
        ])])],
        &kw,
    )
    .unwrap()
    .expect("MultiMerge ok");
    super::inventory_stacking_ops::handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&hangar_bound),
        "invbroker",
        "StackAll",
        &[PyRep::Integer(InventoryFlag::Hangar as i64)],
        &kw,
    )
    .unwrap()
    .expect("StackAll ok");

    let n_ops = ctx.drain_notifications_for_char(char_id);
    let ic_ops: Vec<_> = n_ops.iter().filter(|n| n.event == "OnItemChange").collect();
    assert!(!ic_ops.is_empty());
    for notif in &ic_ops {
        assert!(
            !change_dict_has_key(&notif.args[1], 5),
            "OnItemChange must NEVER emit legacy ixQuantity (5): {:?}",
            notif.args[1]
        );
    }
    assert!(
        ic_ops.iter().any(|n| change_dict_has_key(&n.args[1], 9)),
        "quantity mutations must emit ixStackSize (9)"
    );
}

#[tokio::test]
async fn multi_add_and_add_redirect_station_source_arg_to_active_ship_for_ship_flags() {
    let ctx = test_ctx();
    let char_id = 90_000_001;
    let corp_id = 1_000_009;
    let station_id = 60_003_760;
    let ship_id = 1_000_691_001;
    let mod_stack_id = 1_000_691_002;
    let cargo_ore_id = 1_000_691_003;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.active_ship_id = ship_id;
        }
        snap.skills.entry(char_id).or_default().insert(
            3386,
            eve_db::TrainedSkillRecord {
                type_id: 3386, // Mining
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 32880, // Venture (2 hiSlots)
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_stack_id,
            InventoryItemRecord {
                item_id: mod_stack_id,
                type_id: 482, // Miner I
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 3,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            cargo_ore_id,
            InventoryItemRecord {
                item_id: cargo_ore_id,
                type_id: 1230, // Veldspar
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let kw = HashMap::new();
    let mut sess = make_session(char_id, corp_id, Some(station_id), None, Some(ship_id));
    let hangar_bound = make_bound(station_id, char_id);

    // 1. Station-bound MultiAdd with flagAutoFit (0) & args[1]=station_id redirects to active ship
    let fit_res = handle_multi_add(
        &ctx,
        &mut sess,
        Some(&hangar_bound),
        &[
            PyRep::List(vec![PyRep::Integer(mod_stack_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .await
    .expect("station-bound MultiAdd flagAutoFit must auto-fit onto active ship");
    let PyRep::List(fitted_items) = fit_res.dict_get("items").unwrap() else {
        panic!("expected items list");
    };
    let fitted1_id = fitted_items[0].as_i64().unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&mod_stack_id].quantity, 2);
        assert_eq!(snap.items[&fitted1_id].location_id, ship_id);
        assert_eq!(snap.items[&fitted1_id].flag, InventoryFlag::HiSlot0);
        assert!(snap.items[&fitted1_id].singleton);
    });

    // 2. Unbound Add with HiSlot1 (28) & args[1]=station_id redirects to active ship
    let add_fit = crate::services::inventory_fitting::handle(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(mod_stack_id),
            PyRep::Integer(station_id),
            PyRep::Integer(InventoryFlag::HiSlot1 as i64),
            PyRep::Integer(1),
        ],
        &kw,
    )
    .await
    .expect("handled")
    .expect("unbound Add HiSlot1 with args[1]=station_id fits onto active ship");
    let fitted2_id = add_fit.dict_get("itemID").and_then(PyRep::as_i64).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&mod_stack_id].quantity, 1);
        assert_eq!(snap.items[&fitted2_id].location_id, ship_id);
        assert_eq!(snap.items[&fitted2_id].flag, InventoryFlag::HiSlot1);
    });

    // 3. Station-bound MultiAdd with Cargo flag (5) & args[1]=station_id moves into active ship cargo
    handle_multi_add(
        &ctx,
        &mut sess,
        Some(&hangar_bound),
        &[
            PyRep::List(vec![PyRep::Integer(cargo_ore_id)]),
            PyRep::Integer(station_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
    )
    .await
    .expect("station-bound MultiAdd Cargo moves item to active ship");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&cargo_ore_id].location_id, ship_id);
        assert_eq!(snap.items[&cargo_ore_id].flag, InventoryFlag::Cargo);
    });
}

#[tokio::test]
async fn group_2b_multi_add_validation_autofit_locks_and_capacity_guards() {
    let mut sde = (*shared_sde()).clone();
    let mut base_ship = sde.types.get(&587).expect("Rifter").clone();
    base_ship.type_id = 999_101;
    base_ship.name = "Tiny Hold Carrier".into();
    base_ship.capacity = 20.0;
    base_ship.default_attributes.insert(908, 100.0); // SMA cap
    base_ship.default_attributes.insert(912, 20.0);  // Fleet Hangar cap
    sde.types.insert(999_101, base_ship);

    let mut base_cont = sde.types.get(&1230).expect("Veldspar").clone();
    base_cont.type_id = 999_102;
    base_cont.group_id = 12;
    base_cont.category_id = 2;
    base_cont.volume = 5.0;
    base_cont.capacity = 5.0;
    base_cont.name = "Small Freight Can".into();
    sde.types.insert(999_102, base_cont);

    let mut base_ore = sde.types.get(&1230).expect("Veldspar").clone();
    base_ore.type_id = 999_103;
    base_ore.volume = 10.0;
    base_ore.name = "Dense Rock".into();
    sde.types.insert(999_103, base_ore);

    let mut base_ehe = sde.types.get(&484).expect("Light Ion Blaster / module").clone();
    base_ehe.type_id = 40714;
    base_ehe.category_id = 7;
    base_ehe.name = "Capital EHE Test".into();
    sde.types.insert(40714, base_ehe);

    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));

    let char_id = 92_010_001;
    let stranger_id = 92_010_002;
    let corp_id = 98_010_001;
    let station_id = 60_003_760;
    let sys_id = 30_000_142;

    let ship_id = 1_000_700_001;
    let carrier_id = 1_000_700_002;
    let small_cont_id = 1_000_700_003;
    let foreign_ship_id = 1_000_700_004;
    let ore_item = 1_000_700_010;
    let dense_ore_item = 1_000_700_011;
    let courier_item = 1_000_700_012;
    let rig_item = 1_000_700_013;
    let fitted_gun_item = 1_000_700_014;
    let ehe_item = 1_000_700_015;
    let spare_frigate = 1_000_700_016;
    let extra_gun = 1_000_700_017;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, carrier_id, 999_101, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, small_cont_id, 999_102, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, foreign_ship_id, 587, stranger_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, ore_item, 1230, char_id, station_id, InventoryFlag::Hangar, 10, false);
    insert_item(&ctx, dense_ore_item, 999_103, char_id, station_id, InventoryFlag::Hangar, 5, false); // 50 m3 > 20/5 m3
    insert_item(&ctx, courier_item, 1230, char_id, station_id, InventoryFlag::Hangar, 1, false);
    insert_item(&ctx, rig_item, 482, char_id, ship_id, InventoryFlag::RigSlot0, 1, true);
    insert_item(&ctx, fitted_gun_item, 482, char_id, ship_id, InventoryFlag::HiSlot0, 1, true);
    insert_item(&ctx, ehe_item, 40714, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, spare_frigate, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, extra_gun, 482, char_id, station_id, InventoryFlag::Hangar, 1, true);

    ctx.db.with_mut_snapshot(|snap| {
        snap.item_custom_names.insert(courier_item, "courier_cargo:77".into());
        snap.items
            .get_mut(&ehe_item)
            .unwrap()
            .dynamic_attributes
            .insert(eve_sde::attr_ids::DAMAGE, 50.0);
    });

    let kw = HashMap::new();
    let mut sess = make_session(char_id, corp_id, Some(station_id), None, Some(ship_id));
    let hangar_bound = make_bound(station_id, char_id);
    let ship_bound = make_bound(ship_id, char_id);

    // Missing destination and no bound on session without station/ship
    let mut no_loc_sess = make_session(char_id, corp_id, None, None, None);
    assert_eq!(
        handle_multi_add(&ctx, &mut no_loc_sess, None, &[PyRep::List(vec![PyRep::Integer(ore_item)])], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidLocation"
    );
    // Direct module/rig flags rejected by handle_multi_add
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            Some(&hangar_bound),
            &[PyRep::List(vec![PyRep::Integer(extra_gun)]), PyRep::Integer(ship_id), PyRep::Integer(27)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "InvalidLocation"
    );
    // Restricted destination flags: 36 AssetSafety, 63 Locked, 72 Impounded
    for (flag_i, expected_code) in [(36, "AssetSafetyLocked"), (63, "ItemLocked"), (72, "ImpoundedItem")] {
        assert_eq!(
            handle_multi_add(
                &ctx,
                &mut sess,
                Some(&hangar_bound),
                &[PyRep::List(vec![PyRep::Integer(ore_item)]), PyRep::Integer(station_id), PyRep::Integer(flag_i)],
                &kw,
            )
            .await
            .unwrap_err()
            .code,
            expected_code
        );
    }
    // CannotMoveActiveShip into another item or corp hangar
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            Some(&hangar_bound),
            &[PyRep::List(vec![PyRep::Integer(ship_id)]), PyRep::Integer(small_cont_id), PyRep::Integer(4)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CannotMoveActiveShip"
    );
    // CannotUnfitRig
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            Some(&hangar_bound),
            &[PyRep::List(vec![PyRep::Integer(rig_item)]), PyRep::Integer(station_id), PyRep::Integer(4)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CannotUnfitRig"
    );
    // CannotDeliverCourierCargo to corp division (116)
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            Some(&hangar_bound),
            &[PyRep::List(vec![PyRep::Integer(courier_item)]), PyRep::Integer(station_id), PyRep::Integer(116)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CannotDeliverCourierCargo"
    );
    // Container CapacityExceeded (50 m3 > 5 m3)
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            Some(&hangar_bound),
            &[PyRep::List(vec![PyRep::Integer(dense_ore_item)]), PyRep::Integer(small_cont_id), PyRep::Integer(5)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CapacityExceeded"
    );
    // Fleet Hangar (155) NotEnoughSpecialBaySpace (50 m3 > 20 m3) and SMA (90) NotEnoughSpecialBaySpace (ship > 100 m3)
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            Some(&hangar_bound),
            &[PyRep::List(vec![PyRep::Integer(dense_ore_item)]), PyRep::Integer(carrier_id), PyRep::Integer(155)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughSpecialBaySpace"
    );
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            Some(&hangar_bound),
            &[PyRep::List(vec![PyRep::Integer(spare_frigate)]), PyRep::Integer(carrier_id), PyRep::Integer(90)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughSpecialBaySpace"
    );

    // Auto-fit guards: CannotMoveActiveShip (self onto self), InvalidFittingTarget (foreign ship), ItemNotFittableModule (ore), CannotRefitDamagedModule (damaged EHE), InvalidLocation (already fitted)
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            Some(&hangar_bound),
            &[PyRep::List(vec![PyRep::Integer(ship_id)]), PyRep::Integer(ship_id), PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CannotMoveActiveShip"
    );
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            Some(&hangar_bound),
            &[PyRep::List(vec![PyRep::Integer(extra_gun)]), PyRep::Integer(foreign_ship_id), PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "DestinationAccessDenied"
    );
    assert_eq!(
        handle_multi_add_auto_fit(&ctx, &mut sess, char_id, small_cont_id, &[extra_gun], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidFittingTarget"
    );
    assert_eq!(
        handle_multi_add_auto_fit(&ctx, &mut sess, char_id, ship_id, &[ore_item], &kw)
            .await
            .unwrap_err()
            .code,
        "ItemNotFittableModule"
    );
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            Some(&hangar_bound),
            &[PyRep::List(vec![PyRep::Integer(ehe_item)]), PyRep::Integer(ship_id), PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CannotRefitDamagedModule"
    );
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut sess,
            Some(&ship_bound),
            &[PyRep::List(vec![PyRep::Integer(fitted_gun_item)]), PyRep::Integer(ship_id), PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "InvalidLocation"
    );

    // In-space refitting guards: CannotUnfitInSpace + CannotRefitWithWeaponsTimer + CannotFitInSpace
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().location_id = sys_id as i64;
        snap.items.get_mut(&extra_gun).unwrap().location_id = ship_id;
        snap.items.get_mut(&extra_gun).unwrap().flag = InventoryFlag::Cargo;
        snap.characters.get_mut(&char_id).unwrap().station_id = None;
    });
    let mut space_sess = make_session(char_id, corp_id, None, Some(sys_id), Some(ship_id));
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut space_sess,
            Some(&ship_bound),
            &[PyRep::List(vec![PyRep::Integer(fitted_gun_item)]), PyRep::Integer(ship_id), PyRep::Integer(5)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CannotUnfitInSpace"
    );
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut space_sess,
            Some(&ship_bound),
            &[PyRep::List(vec![PyRep::Integer(extra_gun)]), PyRep::Integer(ship_id), PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CannotFitInSpace"
    );
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.states.entry(char_id).or_default().weapon_timer_until_unix = u64::MAX / 2;
    }
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut space_sess,
            Some(&ship_bound),
            &[PyRep::List(vec![PyRep::Integer(extra_gun)]), PyRep::Integer(ship_id), PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CannotRefitWithWeaponsTimer"
    );
}

#[tokio::test]
async fn group_2b_container_contents_list_alsc_password_corp_role_and_space_range() {
    let mut sde = (*shared_sde()).clone();
    let mut alsc_type = sde.types.get(&1230).expect("Veldspar").clone();
    alsc_type.type_id = 999_110;
    alsc_type.group_id = 448;
    alsc_type.category_id = 2;
    alsc_type.capacity = 10_000.0;
    alsc_type.radius = 50.0;
    alsc_type.name = "Audit Log Secure Container Test".into();
    sde.types.insert(999_110, alsc_type);

    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));

    let char_id = 92_020_001;
    let ceo_id = 92_020_002;
    let corp_id = 98_020_001;
    let station_id = 60_003_760;
    let sys_id = 30_000_142;
    let ship_id = 1_000_710_001;
    let docked_alsc_id = 1_000_710_002;
    let space_cont_id = 1_000_710_003;
    let alsc_loot_id = 1_000_710_004;
    let corp_loot_id = 1_000_710_005;

    ensure_char(&ctx, ceo_id, corp_id, Some(station_id), sys_id, ship_id);
    ensure_char(&ctx, char_id, corp_id, Some(station_id), sys_id, ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = snap.corporations.values().next().cloned().expect("demo seed corp");
        corp.corporation_id = corp_id;
        corp.ceo_id = ceo_id;
        corp.members.insert(ceo_id);
        corp.members.insert(char_id);
        snap.corporations.insert(corp_id, corp);
    });
    insert_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, docked_alsc_id, 999_110, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, alsc_loot_id, 34, char_id, docked_alsc_id, InventoryFlag::Cargo, 25, false);
    insert_item(&ctx, corp_loot_id, 34, corp_id, station_id, InventoryFlag::CorpSAG1, 100, false);
    insert_item(&ctx, space_cont_id, 999_110, char_id, sys_id as i64, InventoryFlag::None, 1, true);

    ctx.db.with_mut_snapshot(|snap| {
        set_container_password_snap(snap, docked_alsc_id, 1, "pw448", 0).unwrap();
    });

    let kw = HashMap::new();
    let sess = make_session(char_id, corp_id, Some(station_id), None, Some(ship_id));
    let unloc_sess = make_session(char_id, corp_id, None, None, None);

    // GetContainerContents: missing ID -> InvalidArgument
    assert_eq!(
        handle_get_container_contents(&ctx, &sess, None, &[], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    // Wrong / missing password -> ALSCPasswordRequired
    assert_eq!(
        handle_get_container_contents(&ctx, &sess, None, &[PyRep::Integer(docked_alsc_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "ALSCPasswordRequired"
    );
    // Unlock with password in kwargs -> succeeds, unlocks container, logs ALSCActionOpen
    let mut kw_pw = HashMap::new();
    kw_pw.insert("password".into(), PyRep::String("pw448".into()));
    assert!(
        handle_get_container_contents(&ctx, &sess, None, &[PyRep::Integer(docked_alsc_id)], &kw_pw)
            .await
            .is_ok()
    );
    // Subsequent call without password succeeds from unlocked cache
    assert!(
        handle_get_container_contents(&ctx, &sess, None, &[PyRep::Integer(docked_alsc_id)], &kw)
            .await
            .is_ok()
    );
    ctx.db.with_snapshot(|snap| {
        assert!(is_container_unlocked_snap(snap, docked_alsc_id, char_id));
        assert!(container_alsc_logs_snap(snap, docked_alsc_id)
            .iter()
            .any(|l| l.action == "ALSCActionOpen"));
    });

    // handle_list: no location -> InvalidLocation
    assert_eq!(handle_list(&ctx, &unloc_sess, None, &[], &kw).unwrap_err().code, "InvalidLocation");
    // Station bound while not docked there -> BoundObjectOwnershipViolation
    let station_bound = make_bound(station_id, char_id);
    assert_eq!(
        handle_list(&ctx, &unloc_sess, Some(&station_bound), &[], &kw)
            .unwrap_err()
            .code,
        "BoundObjectOwnershipViolation"
    );

    // CorpSAG1 (116) query role check via handle_list_by_flags (fails non-CEO without role, passes once CEO)
    assert_eq!(
        handle_list_by_flags(&ctx, &sess, Some(&station_bound), &[PyRep::List(vec![PyRep::Integer(116)])], &kw)
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().ceo_id = char_id;
    });
    assert!(
        handle_list_by_flags(&ctx, &sess, Some(&station_bound), &[PyRep::List(vec![PyRep::Integer(116)])], &kw)
            .is_ok()
    );

    // Space container range checks in handle_list
    let cont_bound = make_bound(space_cont_id, char_id);
    assert_eq!(handle_list(&ctx, &sess, Some(&cont_bound), &[], &kw).unwrap_err().code, "NotInSameSolarSystem");
    let space_sess = make_session(char_id, corp_id, None, Some(sys_id), Some(ship_id));
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
        snap.item_positions.insert(space_cont_id, [10_000.0, 0.0, 0.0]);
    });
    assert_eq!(handle_list(&ctx, &space_sess, Some(&cont_bound), &[], &kw).unwrap_err().code, "OutOfRange");
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(space_cont_id, [1_000.0, 0.0, 0.0]);
    });
    ctx.systems
        .get_or_spawn(sys_id)
        .try_set_ball_position(space_cont_id, glam::DVec3::new(1_000.0, 0.0, 0.0));
    handle_list(&ctx, &space_sess, Some(&cont_bound), &[], &kw)
        .expect("space container handle_list within range");
}

#[test]
fn issue_888_partial_multimerge_is_atomic_on_destination_failure() {
    let ctx = test_ctx();
    let char_id = 92_888_001_i64;
    let corp_id = 98_888_001_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 1_000_888_001_i64;
    let src_stack = 1_000_888_002_i64;
    let wrong_type_dst = 1_000_888_003_i64;
    let overflow_dst = 1_000_888_004_i64;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), 30_000_142, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, src_stack, 34, char_id, station_id, InventoryFlag::Hangar, 50, false);
    insert_item(&ctx, wrong_type_dst, 35, char_id, station_id, InventoryFlag::Hangar, 20, false);
    insert_item(
        &ctx,
        overflow_dst,
        34,
        char_id,
        station_id,
        InventoryFlag::Hangar,
        i32::MAX - 5,
        false,
    );

    let kw = HashMap::new();
    let mut sess = make_session(char_id, corp_id, Some(station_id), None, Some(ship_id));
    let station_bound = make_bound(station_id, char_id);
    let item_count_before = ctx.db.with_snapshot(|snap| snap.items.len());
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Partial MultiMerge into incompatible type fails atomically without splitting src_stack
    let err1 = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&station_bound),
        "invbroker",
        "MultiMerge",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(src_stack),
            PyRep::Integer(wrong_type_dst),
            PyRep::Integer(10),
        ])])],
        &kw,
    )
    .expect("matched")
    .expect_err("partial MultiMerge into mismatched type must fail");
    assert_eq!(err1.code, "StackFailed");

    // 2. Partial MultiMerge that would overflow destination i32::MAX fails atomically without splitting src_stack
    let err2 = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&station_bound),
        "invbroker",
        "MultiMerge",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(src_stack),
            PyRep::Integer(overflow_dst),
            PyRep::Integer(10),
        ])])],
        &kw,
    )
    .expect("matched")
    .expect_err("partial MultiMerge overflowing destination must fail");
    assert_eq!(err2.code, "StackFailed");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.items.len(),
            item_count_before,
            "failed partial MultiMerge must not leave behind an intermediate split item"
        );
        assert_eq!(
            snap.items[&src_stack].quantity, 50,
            "source stack quantity must remain untouched on failed partial MultiMerge"
        );
        assert_eq!(snap.items[&wrong_type_dst].quantity, 20);
        assert_eq!(snap.items[&overflow_dst].quantity, i32::MAX - 5);
    });
    assert!(
        ctx.drain_notifications_for_char(char_id).is_empty(),
        "failed partial MultiMerge must not emit OnItemChange notifications"
    );

    // 3. Partial MultiMerge inside a password-protected / ALSC-locked container enforces locks inside write transaction
    let cont_id = 1_000_888_010;
    let cont_src = 1_000_888_011;
    let cont_dst = 1_000_888_012;
    insert_item(&ctx, cont_id, 17366, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, cont_src, 34, char_id, cont_id, InventoryFlag::Cargo, 40, false);
    insert_item(&ctx, cont_dst, 34, char_id, cont_id, InventoryFlag::Cargo, 25, false);
    ctx.db.with_mut_snapshot(|snap| {
        set_container_password_snap(snap, cont_id, 1, "vaultpw", 0).unwrap();
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    let pw_err = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&station_bound),
        "invbroker",
        "MultiMerge",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(cont_src),
            PyRep::Integer(cont_dst),
            PyRep::Integer(10),
        ])])],
        &kw,
    )
    .expect("matched")
    .expect_err("partial MultiMerge inside password-locked container without password must fail");
    assert_eq!(pw_err.code, "ALSCPasswordRequired");
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // ALSC-locked ancestor container rejects with ALSCItemLocked even when password is provided
    ctx.db.with_mut_snapshot(|snap| {
        set_alsc_item_locked_snap(snap, cont_id, true, 0).unwrap();
    });
    let mut pw_kw = HashMap::new();
    pw_kw.insert("password".into(), PyRep::String("vaultpw".into()));
    let alsc_err = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&station_bound),
        "invbroker",
        "MultiMerge",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(cont_src),
            PyRep::Integer(cont_dst),
            PyRep::Integer(10),
        ])])],
        &pw_kw,
    )
    .expect("matched")
    .expect_err("partial MultiMerge inside ALSC-locked container must return ALSCItemLocked");
    assert_eq!(alsc_err.code, "ALSCItemLocked");
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // Unlock ALSC lock and supply password -> partial MultiMerge succeeds cleanly
    ctx.db.with_mut_snapshot(|snap| {
        set_alsc_item_locked_snap(snap, cont_id, false, 0).unwrap();
    });
    let ok_merge = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&station_bound),
        "invbroker",
        "MultiMerge",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(cont_src),
            PyRep::Integer(cont_dst),
            PyRep::Integer(10),
        ])])],
        &pw_kw,
    )
    .expect("matched")
    .expect("partial MultiMerge with container password should succeed");
    assert_eq!(ok_merge.dict_get("quantity"), Some(&PyRep::Integer(35)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&cont_src].quantity, 30);
        assert_eq!(snap.items[&cont_dst].quantity, 35);
    });
}

#[test]
fn stack_all_preserves_all_eligibility_rules_tiebreak_notifs_and_reply() {
    let ctx = test_ctx();
    let char_id = 92_000_070;
    let corp_id = 98_000_070;
    let station_id = 60_003_760;
    let ship_id = 1_000_270_001;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), 30_000_142, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);

    let id_high = 1_000_270_020_i64;
    let id_low = 1_000_270_010_i64;
    let id_singleton = 1_000_270_021_i64;
    let id_bpc = 1_000_270_022_i64;
    let id_runs = 1_000_270_023_i64;
    let id_dyn = 1_000_270_024_i64;
    let id_muta = 1_000_270_025_i64;
    let id_alsc = 1_000_270_026_i64;
    let id_tube = 1_000_270_027_i64;
    let id_flag_locked = 1_000_270_028_i64;
    let id_flag_impounded = 1_000_270_029_i64;
    let id_flag_asset_safety = 1_000_270_030_i64;
    let id_contract = 1_000_270_031_i64;
    let id_job = 1_000_270_032_i64;
    let id_trade = 1_000_270_033_i64;
    let id_raffle = 1_000_270_034_i64;
    let id_courier = 1_000_270_035_i64;
    let id_non_empty = 1_000_270_036_i64;
    let id_non_empty_child = 1_000_270_037_i64;
    let cont_locked = 1_000_270_100_i64;
    let cont_child_a = 1_000_270_101_i64;
    let cont_child_b = 1_000_270_102_i64;

    for (id, qty, singleton, flag) in [
        (id_high, 25, false, InventoryFlag::Hangar),
        (id_low, 15, false, InventoryFlag::Hangar),
        (id_singleton, 1, true, InventoryFlag::Hangar),
        (id_bpc, 10, false, InventoryFlag::Hangar),
        (id_runs, 10, false, InventoryFlag::Hangar),
        (id_dyn, 10, false, InventoryFlag::Hangar),
        (id_muta, 10, false, InventoryFlag::Hangar),
        (id_alsc, 10, false, InventoryFlag::Hangar),
        (id_tube, 10, false, InventoryFlag::Hangar),
        (id_flag_locked, 10, false, InventoryFlag::Locked),
        (id_flag_impounded, 10, false, InventoryFlag::Impounded),
        (id_flag_asset_safety, 10, false, InventoryFlag::from_u16(36)),
        (id_contract, 10, false, InventoryFlag::Hangar),
        (id_job, 10, false, InventoryFlag::Hangar),
        (id_trade, 10, false, InventoryFlag::Hangar),
        (id_raffle, 10, false, InventoryFlag::Hangar),
        (id_courier, 10, false, InventoryFlag::Hangar),
        (id_non_empty, 10, false, InventoryFlag::Hangar),
    ] {
        insert_item(&ctx, id, 34, char_id, station_id, flag, qty, singleton);
    }
    insert_item(
        &ctx,
        id_non_empty_child,
        34,
        char_id,
        id_non_empty,
        InventoryFlag::Cargo,
        5,
        false,
    );
    insert_item(&ctx, cont_locked, 3293, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, cont_child_a, 34, char_id, cont_locked, InventoryFlag::Cargo, 20, false);
    insert_item(&ctx, cont_child_b, 34, char_id, cont_locked, InventoryFlag::Cargo, 30, false);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&id_bpc).unwrap().is_blueprint_copy = true;
        snap.items.get_mut(&id_runs).unwrap().blueprint_runs_remaining = Some(2);
        snap.items.get_mut(&id_dyn).unwrap().dynamic_attributes.insert(1, 3.0);
        snap.items.get_mut(&id_muta).unwrap().source_mutaplasmid_id = Some(47_740);
        snap.item_custom_names
            .insert(id_courier, "courier_cargo:77".into());
        snap.contracts.insert(
            930_001,
            eve_db::ContractRecord {
                contract_id: 930_001,
                issuer_id: char_id,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                start_station_id: station_id,
                end_station_id: station_id,
                item_ids: vec![id_contract],
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            930_002,
            eve_db::IndustryJobRecord {
                job_id: 930_002,
                installer_id: char_id,
                facility_id: station_id,
                blueprint_item_id: id_job,
                status: "Active".into(),
                ..Default::default()
            },
        );
        snap.owner_notes.insert(
            -73_901,
            eve_db::OwnerNoteRecord {
                note_id: -73_901,
                character_id: -73,
                item_id: 1,
                label: "trade".into(),
                note: serde_json::json!({
                    "trade_id": 930_003_i64,
                    "station_id": station_id,
                    "items_a": [[id_trade, 10]],
                    "items_b": [],
                    "completed": false,
                    "cancelled": false
                })
                .to_string(),
                timestamp_unix: 1,
            },
        );
        snap.owner_notes.insert(
            -74_901,
            eve_db::OwnerNoteRecord {
                note_id: -74_901,
                character_id: -74,
                item_id: 1,
                label: "raffle".into(),
                note: serde_json::json!({
                    "item_id": id_raffle,
                    "winner_id": null,
                    "expired": false
                })
                .to_string(),
                timestamp_unix: 1,
            },
        );
    });
    ctx.db.with_mut_snapshot(|snap| {
        set_alsc_item_locked_snap(snap, id_alsc, true, 0).unwrap();
        set_alsc_item_locked_snap(snap, cont_locked, true, 0).unwrap();
    });
    ctx.fighter_tube_loads
        .write()
        .unwrap()
        .insert((ship_id, 0), id_tube);

    let kw = HashMap::new();
    let mut sess = make_session(char_id, corp_id, Some(station_id), Some(30_000_142), Some(ship_id));
    let station_bound = make_bound(station_id, char_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    let commits_before = ctx.db.tx_commit_count();
    let reply = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&station_bound),
        "invbroker",
        "StackAll",
        &[PyRep::Integer(InventoryFlag::Hangar as i64)],
        &kw,
    )
    .expect("matched")
    .expect("StackAll ok");
    assert_eq!(ctx.db.tx_commit_count(), commits_before + 1);

    assert_eq!(reply.dict_get("locationID"), Some(&PyRep::Integer(station_id)));
    assert_eq!(
        reply.dict_get("flagID"),
        Some(&PyRep::Integer(InventoryFlag::Hangar as i64))
    );
    assert_eq!(reply.dict_get("mergedCount"), Some(&PyRep::Integer(1)));
    let Some(PyRep::List(stacks)) = reply.dict_get("stacks") else {
        panic!("expected stacks list in StackAll reply: {reply:?}");
    };
    assert_eq!(stacks.len(), 1);
    assert_eq!(stacks[0].dict_get("itemID"), Some(&PyRep::Integer(id_low)));
    assert_eq!(stacks[0].dict_get("typeID"), Some(&PyRep::Integer(34)));
    assert_eq!(stacks[0].dict_get("quantity"), Some(&PyRep::Integer(40)));
    assert_eq!(stacks[0].dict_get("version"), Some(&PyRep::Integer(2)));

    let notifs = ctx.drain_notifications_for_char(char_id);
    let changes: Vec<_> = notifs.iter().filter(|n| n.event == "OnItemChange").collect();
    assert_eq!(changes.len(), 2);
    let del_notif = changes
        .iter()
        .find(|n| dbrow_col_i64(&n.args[0], "itemID") == Some(id_high))
        .expect("deleted source OnItemChange");
    assert_eq!(dbrow_col_i64(&del_notif.args[0], "locationID"), Some(6));
    let surv_notif = changes
        .iter()
        .find(|n| dbrow_col_i64(&n.args[0], "itemID") == Some(id_low))
        .expect("survivor OnItemChange");
    assert_eq!(dbrow_col_i64(&surv_notif.args[0], "quantity"), Some(40));
    let PyRep::Dict(change_map) = &surv_notif.args[1] else {
        panic!("expected change dict in OnItemChange: {:?}", surv_notif.args);
    };
    assert!(change_map
        .iter()
        .any(|(k, v)| k.as_i64() == Some(9) && v.as_i64() == Some(15)));
    assert!(!change_map.iter().any(|(k, _)| k.as_i64() == Some(5)));

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&id_high));
        assert_eq!(snap.items[&id_low].quantity, 40);
        for kept in [
            id_singleton,
            id_bpc,
            id_runs,
            id_dyn,
            id_muta,
            id_alsc,
            id_tube,
            id_flag_locked,
            id_flag_impounded,
            id_flag_asset_safety,
            id_contract,
            id_job,
            id_trade,
            id_raffle,
            id_courier,
            id_non_empty,
            cont_child_a,
            cont_child_b,
        ] {
            assert!(snap.items.contains_key(&kept), "excluded item {kept} must not be deleted");
        }
    });

    // StackAll inside ALSC-locked ancestor container merges nothing and commits 0 transactions
    let cont_bound = make_bound(cont_locked, char_id);
    let cont_reply = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&cont_bound),
        "invbroker",
        "StackAll",
        &[PyRep::Integer(InventoryFlag::Cargo as i64)],
        &kw,
    )
    .expect("matched")
    .expect("StackAll on locked container ok");
    assert_eq!(cont_reply.dict_get("mergedCount"), Some(&PyRep::Integer(0)));
    assert_eq!(ctx.db.tx_commit_count(), commits_before + 1);
}

#[test]
fn stack_all_single_commit_and_linear_scaling_over_thousand_items() {
    let ctx = test_ctx();
    let char_id = 92_000_071;
    let corp_id = 98_000_071;
    let station_id = 60_003_760;
    let ship_id = 1_000_271_001;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), 30_000_142, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);

    let per_type = 600_i64;
    let types = [34_u32, 35_u32];
    ctx.db.with_mut_snapshot(|snap| {
        let mut next_id = 1_271_000_000_i64;
        for &tid in &types {
            for idx in 0..per_type {
                let id = next_id;
                next_id += 1;
                snap.items.insert(
                    id,
                    InventoryItemRecord {
                        item_id: id,
                        type_id: tid,
                        owner_id: char_id,
                        location_id: station_id,
                        flag: InventoryFlag::Hangar,
                        quantity: (idx as i32) + 1,
                        singleton: false,
                        ..Default::default()
                    },
                );
            }
        }
        for i in 0..150_i64 {
            snap.contracts.insert(
                940_000 + i,
                eve_db::ContractRecord {
                    contract_id: 940_000 + i,
                    issuer_id: char_id,
                    contract_type: "ItemExchange".into(),
                    status: "Outstanding".into(),
                    start_station_id: station_id,
                    end_station_id: station_id,
                    item_ids: vec![1_290_000_000 + i],
                    ..Default::default()
                },
            );
            snap.industry_jobs.insert(
                941_000 + i,
                eve_db::IndustryJobRecord {
                    job_id: 941_000 + i,
                    installer_id: char_id,
                    facility_id: station_id,
                    blueprint_item_id: 1_291_000_000 + i,
                    status: "Active".into(),
                    ..Default::default()
                },
            );
            snap.owner_notes.insert(
                -73_500 - i,
                eve_db::OwnerNoteRecord {
                    note_id: -73_500 - i,
                    character_id: -73,
                    item_id: 1,
                    label: "t".into(),
                    note: serde_json::json!({
                        "trade_id": 95_000 + i,
                        "items_a": [[1_292_000_000 + i, 1]],
                        "items_b": [],
                        "completed": false,
                        "cancelled": false
                    })
                    .to_string(),
                    timestamp_unix: 1,
                },
            );
            snap.owner_notes.insert(
                -74_500 - i,
                eve_db::OwnerNoteRecord {
                    note_id: -74_500 - i,
                    character_id: -74,
                    item_id: 1,
                    label: "r".into(),
                    note: serde_json::json!({
                        "item_id": 1_293_000_000 + i,
                        "winner_id": null,
                        "expired": false
                    })
                    .to_string(),
                    timestamp_unix: 1,
                },
            );
        }
    });

    let kw = HashMap::new();
    let mut sess = make_session(char_id, corp_id, Some(station_id), Some(30_000_142), Some(ship_id));
    let station_bound = make_bound(station_id, char_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    let commits_before = ctx.db.tx_commit_count();
    let started = std::time::Instant::now();
    let reply = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&station_bound),
        "invbroker",
        "StackAll",
        &[],
        &kw,
    )
    .expect("matched")
    .expect("StackAll ok");
    let elapsed = started.elapsed();

    assert_eq!(ctx.db.tx_commit_count(), commits_before + 1);
    assert_eq!(reply.dict_get("mergedCount"), Some(&PyRep::Integer(1_198)));
    let Some(PyRep::List(stacks)) = reply.dict_get("stacks") else {
        panic!("expected stacks list");
    };
    assert_eq!(stacks.len(), 2);
    let expected_sum: i64 = (1..=per_type).sum();
    for (idx, &tid) in types.iter().enumerate() {
        assert_eq!(
            stacks[idx].dict_get("itemID"),
            Some(&PyRep::Integer(1_271_000_000 + (idx as i64) * per_type))
        );
        assert_eq!(stacks[idx].dict_get("typeID"), Some(&PyRep::Integer(tid as i64)));
        assert_eq!(
            stacks[idx].dict_get("quantity"),
            Some(&PyRep::Integer(expected_sum))
        );
    }

    ctx.db.with_snapshot(|snap| {
        let hangar_items: Vec<_> = snap
            .items
            .values()
            .filter(|it| {
                it.location_id == station_id
                    && it.flag == InventoryFlag::Hangar
                    && it.owner_id == char_id
            })
            .collect();
        assert_eq!(hangar_items.len(), 3);
        assert!(hangar_items.iter().any(|it| it.item_id == ship_id && it.singleton));
        for (idx, &tid) in types.iter().enumerate() {
            let expected_id = 1_271_000_000 + (idx as i64) * per_type;
            let survivor = hangar_items
                .iter()
                .find(|it| it.item_id == expected_id)
                .expect("expected merged survivor stack");
            assert_eq!(survivor.type_id, tid);
            assert_eq!(survivor.quantity, expected_sum as i32);
        }
    });

    // emit_inv_item_{deleted,change} emit OnItemChange + OnMultipleItemChange pairs,
    // so direct handler calls without dispatch_rpc retain MAX_PENDING_NOTIFICATIONS_PER_CHAR events.
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        notifs.len(),
        crate::router::cache_notifications::MAX_PENDING_NOTIFICATIONS_PER_CHAR
    );
    let changes: Vec<_> = notifs.iter().filter(|n| n.event == "OnItemChange").collect();
    assert_eq!(
        changes.len(),
        crate::router::cache_notifications::MAX_PENDING_NOTIFICATIONS_PER_CHAR / 2
    );
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "expected linear O(N+M) single-tx StackAll over 1,200 items, took {elapsed:?}"
    );
}

#[test]
fn stack_all_and_multimerge_enforce_colocation_range_bound_flag_and_structure_acl() {
    let ctx = test_ctx();
    let char_id = 92_000_072;
    let corp_id = 98_000_072;
    let station_id = 60_003_760;
    let remote_station = 60_003_761;
    let sys_id = 30_000_142;
    let ship_id = 1_000_272_001;
    let cont_a = 1_000_272_002;
    let cont_b = 1_000_272_003;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, cont_a, 17_366, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, cont_b, 17_366, char_id, station_id, InventoryFlag::Hangar, 1, true);

    let remote_a = 1_000_272_010;
    let remote_b = 1_000_272_011;
    let local_a = 1_000_272_012;
    let local_b = 1_000_272_013;
    let ca_1 = 1_000_272_014;
    let ca_2 = 1_000_272_015;
    insert_item(&ctx, remote_a, 34, char_id, remote_station, InventoryFlag::Hangar, 20, false);
    insert_item(&ctx, remote_b, 34, char_id, remote_station, InventoryFlag::Hangar, 30, false);
    insert_item(&ctx, local_a, 34, char_id, station_id, InventoryFlag::Hangar, 20, false);
    insert_item(&ctx, local_b, 34, char_id, station_id, InventoryFlag::Hangar, 30, false);
    insert_item(&ctx, ca_1, 34, char_id, cont_a, InventoryFlag::Cargo, 10, false);
    insert_item(&ctx, ca_2, 34, char_id, cont_a, InventoryFlag::Cargo, 15, false);

    let mut sess = make_session(char_id, corp_id, Some(station_id), Some(sys_id), Some(ship_id));
    let kw = HashMap::new();

    // 1. Remote station rejected on both StackAll and MultiMerge
    let mut remote_kw = HashMap::new();
    remote_kw.insert("locationID".into(), PyRep::Integer(remote_station));
    assert_eq!(
        handle_stack_ops(&ctx, &mut sess, None, "invbroker", "StackAll", &[], &remote_kw)
            .unwrap()
            .unwrap_err()
            .code,
        "DestinationAccessDenied"
    );
    let remote_merge = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(remote_a),
        PyRep::Integer(remote_b),
        PyRep::None,
    ])]);
    assert_eq!(
        handle_stack_ops(&ctx, &mut sess, None, "invbroker", "MultiMerge", &[remote_merge], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "DestinationAccessDenied"
    );

    // 2. Bound-moniker vs locationID / containerID consistency checks
    let bound_cont_a = make_bound(cont_a, char_id);
    let mut kw_cont_b = HashMap::new();
    kw_cont_b.insert("locationID".into(), PyRep::Integer(cont_b));
    assert_eq!(
        handle_stack_ops(&ctx, &mut sess, Some(&bound_cont_a), "invbroker", "StackAll", &[], &kw_cont_b)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidLocation"
    );
    let mut kw_station = HashMap::new();
    kw_station.insert("locationID".into(), PyRep::Integer(station_id));
    assert_eq!(
        handle_stack_ops(&ctx, &mut sess, Some(&bound_cont_a), "invbroker", "StackAll", &[], &kw_station)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidLocation"
    );
    let merge_ca = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(ca_1),
        PyRep::Integer(ca_2),
        PyRep::None,
    ])]);
    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut sess,
            Some(&bound_cont_a),
            "invbroker",
            "MultiMerge",
            &[merge_ca.clone(), PyRep::Integer(station_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "StackFailed"
    );
    let bound_cont_b = make_bound(cont_b, char_id);
    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut sess,
            Some(&bound_cont_b),
            "invbroker",
            "MultiMerge",
            &[merge_ca.clone()],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "StackFailed"
    );

    // 3. Flag bounds & solar-system target rejection on StackAll
    for bad_flag in [
        PyRep::Integer(65_540),
        PyRep::Integer(-1),
        PyRep::String("hangar".into()),
    ] {
        assert_eq!(
            handle_stack_ops(
                &ctx,
                &mut sess,
                Some(&make_bound(station_id, char_id)),
                "invbroker",
                "StackAll",
                &[bad_flag],
                &kw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "InvalidInventoryFlag"
        );
    }
    let mut kw_sys = HashMap::new();
    kw_sys.insert("locationID".into(), PyRep::Integer(i64::from(sys_id)));
    assert_eq!(
        handle_stack_ops(&ctx, &mut sess, None, "invbroker", "StackAll", &[], &kw_sys)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidLocation"
    );

    // 4. Undocked in space: docked hangar rejected, container >2,500m rejected OutOfRange, <=2,500m succeeds
    let space_ship = 1_000_272_020;
    let space_cont = 1_000_272_021;
    let sp_1 = 1_000_272_022;
    let sp_2 = 1_000_272_023;
    let sp_3 = 1_000_272_024;
    insert_item(&ctx, space_ship, 587, char_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, space_cont, 23, char_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, sp_1, 34, char_id, space_cont, InventoryFlag::Cargo, 10, false);
    insert_item(&ctx, sp_2, 34, char_id, space_cont, InventoryFlag::Cargo, 20, false);
    insert_item(&ctx, sp_3, 34, char_id, space_cont, InventoryFlag::Cargo, 30, false);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.active_ship_id = space_ship;
        snap.item_positions.insert(space_ship, [0.0, 0.0, 0.0]);
        snap.item_positions.insert(space_cont, [3_200.0, 0.0, 0.0]);
    });
    sess.station_id = None;
    sess.ship_id = Some(space_ship);

    assert_eq!(
        handle_stack_ops(&ctx, &mut sess, None, "invbroker", "StackAll", &[], &kw_station)
            .unwrap()
            .unwrap_err()
            .code,
        "DestinationAccessDenied"
    );
    let merge_local = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(local_a),
        PyRep::Integer(local_b),
        PyRep::None,
    ])]);
    assert_eq!(
        handle_stack_ops(&ctx, &mut sess, None, "invbroker", "MultiMerge", &[merge_local], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "DestinationAccessDenied"
    );

    let bound_space_cont = make_bound(space_cont, char_id);
    assert_eq!(
        handle_stack_ops(&ctx, &mut sess, Some(&bound_space_cont), "invbroker", "StackAll", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "OutOfRange"
    );
    let merge_space_far = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(sp_1),
        PyRep::Integer(sp_2),
        PyRep::None,
    ])]);
    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut sess,
            Some(&bound_space_cont),
            "invbroker",
            "MultiMerge",
            &[merge_space_far.clone()],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OutOfRange"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(space_cont, [1_200.0, 0.0, 0.0]);
    });
    handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&bound_space_cont),
        "invbroker",
        "MultiMerge",
        &[merge_space_far],
        &kw,
    )
    .unwrap()
    .expect("in-range MultiMerge must succeed");
    let space_stack = handle_stack_ops(&ctx, &mut sess, Some(&bound_space_cont), "invbroker", "StackAll", &[], &kw)
        .unwrap()
        .expect("in-range StackAll must succeed");
    assert_eq!(space_stack.dict_get("mergedCount"), Some(&PyRep::Integer(1)));

    // 5. Upwell structure ACL denial, destroyed structure rejection, and allowed ACL success
    let struct_id = 1_000_272_030;
    let st_1 = 1_000_272_031;
    let st_2 = 1_000_272_032;
    let st_3 = 1_000_272_033;
    insert_item(&ctx, st_1, 34, char_id, struct_id, InventoryFlag::Hangar, 10, false);
    insert_item(&ctx, st_2, 34, char_id, struct_id, InventoryFlag::Hangar, 20, false);
    insert_item(&ctx, st_3, 34, char_id, struct_id, InventoryFlag::Hangar, 30, false);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: 98_000_999,
                solar_system_id: sys_id,
                name: "Test Astrahus".into(),
                position: [10_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.structure_id = Some(struct_id);
        snap.structure_docking_acl_denied
            .entry(struct_id)
            .or_default()
            .insert(char_id);
    });
    sess.structure_id = Some(struct_id);
    let bound_struct = make_bound(struct_id, char_id);
    assert_eq!(
        handle_stack_ops(&ctx, &mut sess, Some(&bound_struct), "invbroker", "StackAll", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "DestinationAccessDenied"
    );
    let merge_st = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(st_1),
        PyRep::Integer(st_2),
        PyRep::None,
    ])]);
    assert_eq!(
        handle_stack_ops(&ctx, &mut sess, Some(&bound_struct), "invbroker", "MultiMerge", &[merge_st.clone()], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "DestinationAccessDenied"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_docking_acl_denied.remove(&struct_id);
        snap.structures.get_mut(&struct_id).unwrap().state = "Destroyed".into();
    });
    assert_eq!(
        handle_stack_ops(&ctx, &mut sess, Some(&bound_struct), "invbroker", "StackAll", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "StructureDestroyed"
    );
    assert_eq!(
        handle_stack_ops(&ctx, &mut sess, Some(&bound_struct), "invbroker", "MultiMerge", &[merge_st.clone()], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "StructureDestroyed"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&struct_id).unwrap();
        st.state = "ShieldVulnerable".into();
        st.owner_corp_id = corp_id;
    });
    handle_stack_ops(&ctx, &mut sess, Some(&bound_struct), "invbroker", "MultiMerge", &[merge_st], &kw)
        .unwrap()
        .expect("MultiMerge with structure ACL must succeed");
    let st_stack = handle_stack_ops(&ctx, &mut sess, Some(&bound_struct), "invbroker", "StackAll", &[], &kw)
        .unwrap()
        .expect("StackAll with structure ACL must succeed");
    assert_eq!(st_stack.dict_get("mergedCount"), Some(&PyRep::Integer(1)));
}

#[test]
fn stack_all_enforces_secure_container_passwords_direct_and_ancestor() {
    let ctx = test_ctx();
    let char_id = 92_000_073;
    let corp_id = 98_000_073;
    let station_id = 60_003_760;
    let sys_id = 30_000_142;
    let ship_id = 1_000_273_001;
    let outer_cont = 1_000_273_002;
    let inner_cont = 1_000_273_003;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, outer_cont, 17_366, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, inner_cont, 17_366, char_id, outer_cont, InventoryFlag::Cargo, 1, true);

    let o1 = 1_000_273_010;
    let o2 = 1_000_273_011;
    let i1 = 1_000_273_012;
    let i2 = 1_000_273_013;
    let i3 = 1_000_273_014;
    insert_item(&ctx, o1, 34, char_id, outer_cont, InventoryFlag::Cargo, 10, false);
    insert_item(&ctx, o2, 34, char_id, outer_cont, InventoryFlag::Cargo, 20, false);
    insert_item(&ctx, i1, 35, char_id, inner_cont, InventoryFlag::Cargo, 5, false);
    insert_item(&ctx, i2, 35, char_id, inner_cont, InventoryFlag::Cargo, 7, false);
    insert_item(&ctx, i3, 35, char_id, inner_cont, InventoryFlag::Cargo, 9, false);

    ctx.db.with_mut_snapshot(|snap| {
        set_container_password_snap(snap, outer_cont, 1, "outerpw", 0).unwrap();
        set_container_password_snap(snap, inner_cont, 1, "innerpw", 0).unwrap();
    });

    let mut sess = make_session(char_id, corp_id, Some(station_id), Some(sys_id), Some(ship_id));
    let kw = HashMap::new();
    let commits_before = ctx.db.tx_commit_count();

    // Direct locked container rejected with ALSCPasswordRequired and 0 DB commits
    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut sess,
            Some(&make_bound(outer_cont, char_id)),
            "invbroker",
            "StackAll",
            &[PyRep::Integer(InventoryFlag::Cargo as i64)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ALSCPasswordRequired"
    );
    assert_eq!(ctx.db.tx_commit_count(), commits_before);

    // Wrong password on outer container rejected
    let mut wrong_pw = HashMap::new();
    wrong_pw.insert("password".into(), PyRep::String("wrong".into()));
    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut sess,
            Some(&make_bound(outer_cont, char_id)),
            "invbroker",
            "StackAll",
            &[PyRep::Integer(InventoryFlag::Cargo as i64)],
            &wrong_pw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ALSCPasswordRequired"
    );
    assert_eq!(ctx.db.tx_commit_count(), commits_before);

    // Correct password unlocks outer container and merges outer stacks
    let mut outer_pw = HashMap::new();
    outer_pw.insert("password".into(), PyRep::String("outerpw".into()));
    let outer_res = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&make_bound(outer_cont, char_id)),
        "invbroker",
        "StackAll",
        &[PyRep::Integer(InventoryFlag::Cargo as i64)],
        &outer_pw,
    )
    .unwrap()
    .expect("outer StackAll with valid password must succeed");
    assert_eq!(outer_res.dict_get("mergedCount"), Some(&PyRep::Integer(1)));

    // Ancestor outer is now unlocked, but inner still rejects without innerpw
    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut sess,
            Some(&make_bound(inner_cont, char_id)),
            "invbroker",
            "StackAll",
            &[PyRep::Integer(InventoryFlag::Cargo as i64)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ALSCPasswordRequired"
    );

    // Lock outer again; supplying innerpw when outer is still locked must reject on ancestor container
    ctx.db.with_mut_snapshot(|snap| {
        set_container_unlocked_snap(snap, outer_cont, char_id, false, 0).unwrap();
    });
    let mut inner_pw = HashMap::new();
    inner_pw.insert("password".into(), PyRep::String("innerpw".into()));
    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut sess,
            Some(&make_bound(inner_cont, char_id)),
            "invbroker",
            "StackAll",
            &[PyRep::Integer(InventoryFlag::Cargo as i64)],
            &inner_pw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ALSCPasswordRequired"
    );

    // Unlock outer via ContainerUnlockedChar and supply innerpw -> succeeds and records inner_cont unlock
    ctx.db.with_mut_snapshot(|snap| {
        set_container_unlocked_snap(snap, outer_cont, char_id, true, 0).unwrap();
    });
    let inner_res = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&make_bound(inner_cont, char_id)),
        "invbroker",
        "StackAll",
        &[PyRep::Integer(InventoryFlag::Cargo as i64)],
        &inner_pw,
    )
    .unwrap()
    .expect("inner StackAll with valid password and unlocked ancestor must succeed");
    assert_eq!(inner_res.dict_get("mergedCount"), Some(&PyRep::Integer(2)));

    // Subsequent StackAll on inner_cont succeeds without re-sending password
    let inner_repeat = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&make_bound(inner_cont, char_id)),
        "invbroker",
        "StackAll",
        &[PyRep::Integer(InventoryFlag::Cargo as i64)],
        &kw,
    )
    .unwrap()
    .expect("subsequent StackAll on unlocked inner_cont must succeed");
    assert_eq!(inner_repeat.dict_get("mergedCount"), Some(&PyRep::Integer(0)));
}

#[test]
fn stack_ops_reject_auth_colocation_acl_before_password_oracle_and_run_atomically() {
    let ctx = test_ctx();
    let owner_id = 92_000_074;
    let corp_owner = 98_000_074;
    let attacker_id = 92_000_075;
    let corp_attacker = 98_000_075;
    let corp_mate_id = 92_000_076;
    let station_a = 60_003_760;
    let station_b = 60_003_761;
    let sys_id = 30_000_142;
    let ship_owner = 1_000_274_001;
    let ship_attacker = 1_000_274_002;
    let ship_mate = 1_000_274_003;
    let cont_owner = 1_000_274_010;
    let cont_remote = 1_000_274_011;
    let corp_cont = 1_000_274_012;
    let it_1 = 1_000_274_020;
    let it_2 = 1_000_274_021;
    let r_1 = 1_000_274_022;
    let r_2 = 1_000_274_023;
    let c_1 = 1_000_274_024;
    let c_2 = 1_000_274_025;

    ensure_char(&ctx, owner_id, corp_owner, Some(station_a), sys_id, ship_owner);
    ensure_char(&ctx, attacker_id, corp_attacker, Some(station_a), sys_id, ship_attacker);
    ensure_char(&ctx, corp_mate_id, corp_owner, Some(station_a), sys_id, ship_mate);
    insert_item(&ctx, ship_owner, 587, owner_id, station_a, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, ship_attacker, 587, attacker_id, station_a, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, ship_mate, 587, corp_mate_id, station_a, InventoryFlag::Hangar, 1, true);

    insert_item(&ctx, cont_owner, 17_366, owner_id, station_a, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, it_1, 34, owner_id, cont_owner, InventoryFlag::Cargo, 20, false);
    insert_item(&ctx, it_2, 34, owner_id, cont_owner, InventoryFlag::Cargo, 30, false);

    insert_item(&ctx, cont_remote, 17_366, attacker_id, station_b, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, r_1, 34, attacker_id, cont_remote, InventoryFlag::Cargo, 15, false);
    insert_item(&ctx, r_2, 34, attacker_id, cont_remote, InventoryFlag::Cargo, 25, false);

    insert_item(&ctx, corp_cont, 17_366, corp_owner, station_a, InventoryFlag::CorpSAG1, 1, true);
    insert_item(&ctx, c_1, 34, corp_mate_id, corp_cont, InventoryFlag::Cargo, 10, false);
    insert_item(&ctx, c_2, 34, corp_mate_id, corp_cont, InventoryFlag::Cargo, 12, false);

    ctx.db.with_mut_snapshot(|snap| {
        set_container_password_snap(snap, cont_owner, 1, "topsecret", 0).unwrap();
        set_container_password_snap(snap, cont_remote, 1, "remotepw", 0).unwrap();
        set_container_password_snap(snap, corp_cont, 1, "corppw", 0).unwrap();
    });

    let mut owner_sess = make_session(owner_id, corp_owner, Some(station_a), Some(sys_id), Some(ship_owner));
    let mut attacker_sess =
        make_session(attacker_id, corp_attacker, Some(station_a), Some(sys_id), Some(ship_attacker));
    let mut mate_sess = make_session(corp_mate_id, corp_owner, Some(station_a), Some(sys_id), Some(ship_mate));
    let kw_empty = HashMap::new();

    // 1. Unbound StackAll password oracle against foreign container (wrong vs exact password)
    for pw in ["wrong", "topsecret"] {
        let mut kw = HashMap::new();
        kw.insert("locationID".into(), PyRep::Integer(cont_owner));
        kw.insert("password".into(), PyRep::String(pw.into()));
        let err = handle_stack_ops(&ctx, &mut attacker_sess, None, "invbroker", "StackAll", &[], &kw)
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, "DestinationAccessDenied");
    }

    // 2. Remote owned container (co-location check precedes password check on StackAll/SplitStack/MultiMerge)
    for pw in ["wrong", "remotepw"] {
        let mut kw_stack = HashMap::new();
        kw_stack.insert("locationID".into(), PyRep::Integer(cont_remote));
        kw_stack.insert("password".into(), PyRep::String(pw.into()));
        assert_eq!(
            handle_stack_ops(&ctx, &mut attacker_sess, None, "invbroker", "StackAll", &[], &kw_stack)
                .unwrap()
                .unwrap_err()
                .code,
            "DestinationAccessDenied"
        );

        let mut kw_pw = HashMap::new();
        kw_pw.insert("password".into(), PyRep::String(pw.into()));
        assert_eq!(
            handle_stack_ops(
                &ctx,
                &mut attacker_sess,
                None,
                "invbroker",
                "SplitStack",
                &[
                    PyRep::Integer(station_b),
                    PyRep::Integer(r_1),
                    PyRep::Integer(5),
                ],
                &kw_pw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "SplitFailed"
        );

        let merge_remote = PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(r_1),
            PyRep::Integer(r_2),
            PyRep::None,
        ])]);
        assert_eq!(
            handle_stack_ops(
                &ctx,
                &mut attacker_sess,
                None,
                "invbroker",
                "MultiMerge",
                &[merge_remote],
                &kw_pw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "DestinationAccessDenied"
        );
    }

    // 3. Foreign SplitStack rejects ownership before container password check
    for pw in ["wrong", "topsecret"] {
        let mut kw = HashMap::new();
        kw.insert("password".into(), PyRep::String(pw.into()));
        assert_eq!(
            handle_stack_ops(
                &ctx,
                &mut attacker_sess,
                None,
                "invbroker",
                "SplitStack",
                &[
                    PyRep::Integer(station_a),
                    PyRep::Integer(it_1),
                    PyRep::Integer(5),
                ],
                &kw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "SplitFailed"
        );
    }

    // 4. Corp container take role is validated before container password across StackAll, MultiMerge, and SplitStack
    for pw in ["wrong", "corppw"] {
        let mut kw = HashMap::new();
        kw.insert("locationID".into(), PyRep::Integer(corp_cont));
        kw.insert("password".into(), PyRep::String(pw.into()));
        assert_eq!(
            handle_stack_ops(&ctx, &mut mate_sess, None, "invbroker", "StackAll", &[], &kw)
                .unwrap()
                .unwrap_err()
                .code,
            "CorpRoleDenied"
        );

        let mut kw_pw = HashMap::new();
        kw_pw.insert("password".into(), PyRep::String(pw.into()));
        let merge_corp = PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(c_1),
            PyRep::Integer(c_2),
            PyRep::None,
        ])]);
        assert_eq!(
            handle_stack_ops(
                &ctx,
                &mut mate_sess,
                Some(&make_bound(station_a, corp_mate_id)),
                "invbroker",
                "MultiMerge",
                &[merge_corp],
                &kw_pw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "CorpRoleDenied"
        );
        assert_eq!(
            handle_stack_ops(
                &ctx,
                &mut mate_sess,
                Some(&make_bound(station_a, corp_mate_id)),
                "invbroker",
                "SplitStack",
                &[
                    PyRep::Integer(station_a),
                    PyRep::Integer(c_1),
                    PyRep::Integer(5),
                ],
                &kw_pw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "CorpRoleDenied"
        );
    }
    ctx.db.with_snapshot(|snap| {
        assert!(!is_container_unlocked_snap(snap, corp_cont, corp_mate_id));
        assert!(!is_container_unlocked_snap(snap, corp_mate_id, corp_cont));
        assert_eq!(snap.items.get(&c_1).map(|i| i.quantity), Some(10));
        assert_eq!(snap.items.get(&c_2).map(|i| i.quantity), Some(12));
    });

    // 5. Failed SplitStack with matching password MUST NOT persist ContainerUnlockedChar
    let commits_before_fail_split = ctx.db.tx_commit_count();
    let mut owner_pw = HashMap::new();
    owner_pw.insert("password".into(), PyRep::String("topsecret".into()));
    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut owner_sess,
            Some(&make_bound(station_a, owner_id)),
            "invbroker",
            "SplitStack",
            &[
                PyRep::Integer(station_a),
                PyRep::Integer(it_1),
                PyRep::Integer(999),
            ],
            &owner_pw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "SplitFailed"
    );
    assert_eq!(ctx.db.tx_commit_count(), commits_before_fail_split);
    assert!(!ctx.db.with_snapshot(|snap| is_container_unlocked_snap(snap, cont_owner, owner_id)));

    // 6. StackAll unlocking container + merging executes in exactly 1 commit, and no-op repeat does 0
    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut owner_sess,
            Some(&make_bound(cont_owner, owner_id)),
            "invbroker",
            "StackAll",
            &[PyRep::Integer(InventoryFlag::Cargo as i64)],
            &kw_empty,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ALSCPasswordRequired"
    );
    let commits_before_stack = ctx.db.tx_commit_count();
    let stack_res = handle_stack_ops(
        &ctx,
        &mut owner_sess,
        Some(&make_bound(cont_owner, owner_id)),
        "invbroker",
        "StackAll",
        &[PyRep::Integer(InventoryFlag::Cargo as i64)],
        &owner_pw,
    )
    .unwrap()
    .expect("owner StackAll with valid password must succeed");
    assert_eq!(stack_res.dict_get("mergedCount"), Some(&PyRep::Integer(1)));
    assert_eq!(ctx.db.tx_commit_count(), commits_before_stack + 1);

    assert!(ctx.db.with_snapshot(|snap| is_container_unlocked_snap(snap, cont_owner, owner_id)));

    let commits_before_noop = ctx.db.tx_commit_count();
    let noop_res = handle_stack_ops(
        &ctx,
        &mut owner_sess,
        Some(&make_bound(cont_owner, owner_id)),
        "invbroker",
        "StackAll",
        &[PyRep::Integer(InventoryFlag::Cargo as i64)],
        &kw_empty,
    )
    .unwrap()
    .expect("subsequent no-op StackAll must succeed without password");
    assert_eq!(noop_res.dict_get("mergedCount"), Some(&PyRep::Integer(0)));
    assert_eq!(ctx.db.tx_commit_count(), commits_before_noop);
}

#[test]
fn get_inventory_mgr_bound_split_stack_works_when_docked_in_upwell_structure() {
    let ctx = test_ctx();
    let char_id = 92_000_077;
    let corp_id = 98_000_077;
    let sys_id = 30_000_142;
    let struct_id = 1_000_275_900;
    let ship_id = 1_000_275_001;
    let cont_id = 1_000_275_002;
    let hangar_stack = 1_000_275_010;
    let cont_stack = 1_000_275_011;

    ensure_char(&ctx, char_id, corp_id, None, sys_id, ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Test Fortizar".into(),
                position: [25_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = Some(struct_id);
    });
    insert_item(&ctx, ship_id, 587, char_id, struct_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, cont_id, 17_366, char_id, struct_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, hangar_stack, 34, char_id, struct_id, InventoryFlag::Hangar, 40, false);
    insert_item(&ctx, cont_stack, 34, char_id, cont_id, InventoryFlag::Cargo, 60, false);

    ctx.db.with_mut_snapshot(|snap| {
        set_container_password_snap(snap, cont_id, 1, "structpw", 0).unwrap();
    });

    let mut sess = make_session(char_id, corp_id, None, Some(sys_id), Some(ship_id));
    sess.structure_id = Some(struct_id);
    let inv_mgr_bound = make_bound(i64::from(sys_id), char_id);
    let kw_empty = HashMap::new();

    // 1. V24 GetInventoryMgr() binds to solar_system_id; splitting in structure hangar succeeds
    let split_hangar = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&inv_mgr_bound),
        "invbroker",
        "SplitStack",
        &[
            PyRep::Integer(struct_id),
            PyRep::Integer(hangar_stack),
            PyRep::Integer(15),
            PyRep::Integer(char_id),
        ],
        &kw_empty,
    )
    .unwrap()
    .expect("GetInventoryMgr-bound SplitStack in docked Upwell hangar must succeed");
    assert_eq!(split_hangar.dict_get("originalQuantity"), Some(&PyRep::Integer(25)));
    assert_eq!(split_hangar.dict_get("newQuantity"), Some(&PyRep::Integer(15)));

    // 2. GetInventoryMgr()-bound SplitStack inside a passworded container respects ACL before password
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().owner_corp_id = 98_000_999;
        snap.structure_docking_acl_denied
            .entry(struct_id)
            .or_default()
            .insert(char_id);
    });
    for pw in ["wrong", "structpw"] {
        let mut kw = HashMap::new();
        kw.insert("password".into(), PyRep::String(pw.into()));
        assert_eq!(
            handle_stack_ops(
                &ctx,
                &mut sess,
                Some(&inv_mgr_bound),
                "invbroker",
                "SplitStack",
                &[
                    PyRep::Integer(struct_id),
                    PyRep::Integer(cont_stack),
                    PyRep::Integer(20),
                    PyRep::Integer(char_id),
                ],
                &kw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "SplitFailed"
        );
    }

    // 3. Destroyed Upwell structure still yields StructureDestroyed even via GetInventoryMgr()
    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_docking_acl_denied.remove(&struct_id);
        snap.structures.get_mut(&struct_id).unwrap().state = "Destroyed".into();
    });
    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut sess,
            Some(&inv_mgr_bound),
            "invbroker",
            "SplitStack",
            &[
                PyRep::Integer(struct_id),
                PyRep::Integer(cont_stack),
                PyRep::Integer(20),
                PyRep::Integer(char_id),
            ],
            &kw_empty,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "StructureDestroyed"
    );

    // 4. Restored ACL + valid password inside container succeeds via GetInventoryMgr()
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&struct_id).unwrap();
        st.state = "ShieldVulnerable".into();
        st.owner_corp_id = corp_id;
    });
    let mut kw_ok = HashMap::new();
    kw_ok.insert("password".into(), PyRep::String("structpw".into()));
    let split_cont = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&inv_mgr_bound),
        "invbroker",
        "SplitStack",
        &[
            PyRep::Integer(struct_id),
            PyRep::Integer(cont_stack),
            PyRep::Integer(20),
            PyRep::Integer(char_id),
        ],
        &kw_ok,
    )
    .unwrap()
    .expect("GetInventoryMgr-bound SplitStack inside Upwell container with ACL+pw must succeed");
    assert_eq!(split_cont.dict_get("originalQuantity"), Some(&PyRep::Integer(40)));
    assert_eq!(split_cont.dict_get("newQuantity"), Some(&PyRep::Integer(20)));
}

#[test]
fn corp_container_stack_ops_evaluate_due_pending_role_grants_without_reentrant_lock() {
    let ctx = test_ctx();
    let grantee_id = 92_000_078;
    let unmatured_id = 92_000_079;
    let corp_id = 98_000_078;
    let station_id = 60_003_760;
    let sys_id = 30_000_142;
    let ship_a = 1_000_278_001;
    let ship_b = 1_000_278_002;
    let corp_cont = 1_000_278_010;
    let g_1 = 1_000_278_020;
    let g_2 = 1_000_278_021;
    let g_3 = 1_000_278_022;
    let u_1 = 1_000_278_030;
    let u_2 = 1_000_278_031;

    ensure_char(&ctx, grantee_id, corp_id, Some(station_id), sys_id, ship_a);
    ensure_char(&ctx, unmatured_id, corp_id, Some(station_id), sys_id, ship_b);
    insert_item(&ctx, ship_a, 587, grantee_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, ship_b, 587, unmatured_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, corp_cont, 17_366, corp_id, station_id, InventoryFlag::CorpSAG1, 1, true);

    insert_item(&ctx, g_1, 34, grantee_id, corp_cont, InventoryFlag::Cargo, 20, false);
    insert_item(&ctx, g_2, 34, grantee_id, corp_cont, InventoryFlag::Cargo, 30, false);
    insert_item(&ctx, g_3, 34, grantee_id, corp_cont, InventoryFlag::Cargo, 50, false);
    insert_item(&ctx, u_1, 34, unmatured_id, corp_cont, InventoryFlag::Cargo, 12, false);
    insert_item(&ctx, u_2, 34, unmatured_id, corp_cont, InventoryFlag::Cargo, 18, false);

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Grant Test Corp".into();
        corp.ticker = "GTC".into();
        corp.ceo_id = 92_000_999;
        corp.members = HashSet::from([92_000_999, grantee_id, unmatured_id]);
        snap.corporations.insert(corp_id, corp);
        let gov = snap.corp_governance.entry(corp_id).or_default();
        // Matured (due) pending grant exercises the `has_due == true` path without 40->40 deadlock.
        gov.pending_role_grants
            .insert(grantee_id, (eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[0], 1));
        gov.pending_role_grants.insert(
            unmatured_id,
            (eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[0], now + 86_400),
        );
        // Also seed metadata on g_2/g_3 to confirm remove_system_cleanup_item cleanup on StackAll.
        snap.item_custom_names.insert(g_2, "Old Stack Label".into());
        snap.item_positions.insert(g_2, [1.0, 2.0, 3.0]);
        snap.item_custom_names.insert(g_3, "Old Stack Label 3".into());
        snap.item_positions.insert(g_3, [4.0, 5.0, 6.0]);
    });
    ctx.db.with_mut_snapshot(|snap| {
        set_container_password_snap(snap, corp_cont, 1, "grantpw", 0).unwrap();
    });

    let mut grantee_sess =
        make_session(grantee_id, corp_id, Some(station_id), Some(sys_id), Some(ship_a));
    let mut unmatured_sess =
        make_session(unmatured_id, corp_id, Some(station_id), Some(sys_id), Some(ship_b));
    let mut kw_pw = HashMap::new();
    kw_pw.insert("password".into(), PyRep::String("grantpw".into()));

    // 1. Unmatured grant still denies SplitStack/MultiMerge/StackAll with CorpRoleDenied and no deadlock
    let commits_unmatured = ctx.db.tx_commit_count();
    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut unmatured_sess,
            Some(&make_bound(station_id, unmatured_id)),
            "invbroker",
            "SplitStack",
            &[
                PyRep::Integer(station_id),
                PyRep::Integer(u_1),
                PyRep::Integer(4),
            ],
            &kw_pw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    let merge_u = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(u_1),
        PyRep::Integer(u_2),
        PyRep::None,
    ])]);
    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut unmatured_sess,
            Some(&make_bound(station_id, unmatured_id)),
            "invbroker",
            "MultiMerge",
            &[merge_u],
            &kw_pw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut unmatured_sess,
            Some(&make_bound(corp_cont, unmatured_id)),
            "invbroker",
            "StackAll",
            &[PyRep::Integer(InventoryFlag::Cargo as i64)],
            &kw_pw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(ctx.db.tx_commit_count(), commits_unmatured);

    // 2. Matured (due) grant succeeds across SplitStack, MultiMerge, and StackAll in single txs
    let split_res = handle_stack_ops(
        &ctx,
        &mut grantee_sess,
        Some(&make_bound(station_id, grantee_id)),
        "invbroker",
        "SplitStack",
        &[
            PyRep::Integer(station_id),
            PyRep::Integer(g_1),
            PyRep::Integer(5),
        ],
        &kw_pw,
    )
    .unwrap()
    .expect("due pending role grant must allow SplitStack without re-entrant lock panic");
    assert_eq!(split_res.dict_get("originalQuantity"), Some(&PyRep::Integer(15)));

    ctx.db.with_mut_snapshot(|snap| {
        clear_container_unlocks_snap(snap, corp_cont);
    });

    let merge_g = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(g_2),
        PyRep::Integer(g_1),
        PyRep::None,
    ])]);
    handle_stack_ops(
        &ctx,
        &mut grantee_sess,
        Some(&make_bound(station_id, grantee_id)),
        "invbroker",
        "MultiMerge",
        &[merge_g],
        &kw_pw,
    )
    .unwrap()
    .expect("due pending role grant must allow MultiMerge without re-entrant lock panic");

    ctx.db.with_mut_snapshot(|snap| {
        clear_container_unlocks_snap(snap, corp_cont);
    });

    let stack_res = handle_stack_ops(
        &ctx,
        &mut grantee_sess,
        Some(&make_bound(corp_cont, grantee_id)),
        "invbroker",
        "StackAll",
        &[PyRep::Integer(InventoryFlag::Cargo as i64)],
        &kw_pw,
    )
    .unwrap()
    .expect("due pending role grant must allow StackAll without re-entrant lock panic");
    assert_eq!(stack_res.dict_get("mergedCount"), Some(&PyRep::Integer(2)));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&g_3));
        assert!(!snap.item_custom_names.contains_key(&g_3));
        assert!(!snap.item_positions.contains_key(&g_3));
    });
}

#[test]
fn in_space_foreign_ship_nested_container_rejects_before_password_oracle() {
    let ctx = test_ctx();
    let attacker_id = 92_000_080;
    let victim_id = 92_000_081;
    let corp_a = 98_000_080;
    let corp_v = 98_000_081;
    let sys_id = 30_000_142;
    let attacker_ship = 1_000_280_001;
    let victim_ship = 1_000_280_002;
    let nested_cont = 1_000_280_010;
    let s_1 = 1_000_280_020;
    let s_2 = 1_000_280_021;

    ensure_char(&ctx, attacker_id, corp_a, None, sys_id, attacker_ship);
    ensure_char(&ctx, victim_id, corp_v, None, sys_id, victim_ship);
    insert_item(&ctx, attacker_ship, 587, attacker_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, victim_ship, 587, victim_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, nested_cont, 17_366, victim_id, victim_ship, InventoryFlag::Cargo, 1, true);
    insert_item(&ctx, s_1, 34, attacker_id, nested_cont, InventoryFlag::Cargo, 20, false);
    insert_item(&ctx, s_2, 34, attacker_id, nested_cont, InventoryFlag::Cargo, 30, false);

    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(attacker_ship, [0.0, 0.0, 0.0]);
        snap.item_positions.insert(victim_ship, [500.0, 0.0, 0.0]);
    });
    ctx.db.with_mut_snapshot(|snap| {
        set_container_password_snap(snap, nested_cont, 1, "shippw", 0).unwrap();
    });

    let mut attacker_sess =
        make_session(attacker_id, corp_a, None, Some(sys_id), Some(attacker_ship));

    // 1. Unshared foreign ship hull within 2_500m must reject before container password oracle
    for pw in ["wrong", "shippw"] {
        let mut kw = HashMap::new();
        kw.insert("password".into(), PyRep::String(pw.into()));
        assert_eq!(
            handle_stack_ops(
                &ctx,
                &mut attacker_sess,
                Some(&make_bound(nested_cont, attacker_id)),
                "invbroker",
                "SplitStack",
                &[
                    PyRep::Integer(nested_cont),
                    PyRep::Integer(s_1),
                    PyRep::Integer(5),
                ],
                &kw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "SplitFailed"
        );
        let merge_nested = PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(s_1),
            PyRep::Integer(s_2),
            PyRep::None,
        ])]);
        assert_eq!(
            handle_stack_ops(
                &ctx,
                &mut attacker_sess,
                Some(&make_bound(nested_cont, attacker_id)),
                "invbroker",
                "MultiMerge",
                &[merge_nested],
                &kw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "DestinationAccessDenied"
        );
    }

    // 2. Move nested_cont into shared FleetHangar and add fleet membership -> reaches container pw check
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&nested_cont).unwrap().flag = InventoryFlag::FleetHangar;
        snap.fleets.insert(
            777_001,
            eve_db::FleetRecord {
                fleet_id: 777_001,
                boss_id: victim_id,
                members: vec![victim_id, attacker_id],
                ..Default::default()
            },
        );
    });
    let mut kw_wrong = HashMap::new();
    kw_wrong.insert("password".into(), PyRep::String("wrong".into()));
    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut attacker_sess,
            Some(&make_bound(nested_cont, attacker_id)),
            "invbroker",
            "SplitStack",
            &[
                PyRep::Integer(nested_cont),
                PyRep::Integer(s_1),
                PyRep::Integer(5),
            ],
            &kw_wrong,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ALSCPasswordRequired"
    );
    let mut kw_right = HashMap::new();
    kw_right.insert("password".into(), PyRep::String("shippw".into()));
    let ok_split = handle_stack_ops(
        &ctx,
        &mut attacker_sess,
        Some(&make_bound(nested_cont, attacker_id)),
        "invbroker",
        "SplitStack",
        &[
            PyRep::Integer(nested_cont),
            PyRep::Integer(s_1),
            PyRep::Integer(5),
        ],
        &kw_right,
    )
    .unwrap()
    .expect("shared fleet hangar nested container with valid password must allow SplitStack");
    assert_eq!(ok_split.dict_get("originalQuantity"), Some(&PyRep::Integer(15)));
}

#[test]
fn undocked_structure_and_missing_pos_fail_closed_before_password_oracle() {
    let ctx = test_ctx();
    let char_id = 92_000_082;
    let corp_id = 98_000_082;
    let sys_id = 30_000_142;
    let struct_id = 1_000_282_900;
    let ship_id = 1_000_282_001;
    let struct_cont = 1_000_282_010;
    let space_cont = 1_000_282_011;
    let st_1 = 1_000_282_020;
    let st_2 = 1_000_282_021;
    let sc_1 = 1_000_282_022;
    let sc_2 = 1_000_282_023;
    let sp_1 = 1_000_282_030;
    let sp_2 = 1_000_282_031;

    ensure_char(&ctx, char_id, corp_id, None, sys_id, ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Citadel Undocked Guard".into(),
                position: [10_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
    });
    insert_item(&ctx, ship_id, 587, char_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, struct_cont, 17_366, char_id, struct_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, st_1, 34, char_id, struct_id, InventoryFlag::Hangar, 20, false);
    insert_item(&ctx, st_2, 34, char_id, struct_id, InventoryFlag::Hangar, 30, false);
    insert_item(&ctx, sc_1, 35, char_id, struct_cont, InventoryFlag::Cargo, 10, false);
    insert_item(&ctx, sc_2, 35, char_id, struct_cont, InventoryFlag::Cargo, 15, false);

    insert_item(&ctx, space_cont, 17_366, char_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, sp_1, 34, char_id, space_cont, InventoryFlag::Cargo, 25, false);
    insert_item(&ctx, sp_2, 34, char_id, space_cont, InventoryFlag::Cargo, 35, false);

    ctx.db.with_mut_snapshot(|snap| {
        set_container_password_snap(snap, struct_cont, 1, "structcontpw", 0).unwrap();
        set_container_password_snap(snap, space_cont, 1, "spacecontpw", 0).unwrap();
    });

    // 1. Undocked pilot tethered within 5,000m of ACL-owned structure MUST be rejected for internal hangars/containers
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, [10_500.0, 0.0, 0.0]);
    });
    let mut space_sess = make_session(char_id, corp_id, None, Some(sys_id), Some(ship_id));
    for (ship_pos_opt, pw) in [
        (Some([10_500.0, 0.0, 0.0]), "wrong"),
        (Some([10_500.0, 0.0, 0.0]), "structcontpw"),
        (None, "structcontpw"),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(pos) = ship_pos_opt {
                snap.item_positions.insert(ship_id, pos);
            } else {
                snap.item_positions.remove(&ship_id);
            }
        });
        let commits_before = ctx.db.tx_commit_count();
        let mut kw_hangar = HashMap::new();
        kw_hangar.insert("locationID".into(), PyRep::Integer(struct_id));
        kw_hangar.insert("flag".into(), PyRep::Integer(InventoryFlag::Hangar as i64));
        assert_eq!(
            handle_stack_ops(&ctx, &mut space_sess, None, "invbroker", "StackAll", &[], &kw_hangar)
                .unwrap()
                .unwrap_err()
                .code,
            "DestinationAccessDenied"
        );
        let mut kw_cont = HashMap::new();
        kw_cont.insert("locationID".into(), PyRep::Integer(struct_cont));
        kw_cont.insert("flag".into(), PyRep::Integer(InventoryFlag::Cargo as i64));
        kw_cont.insert("password".into(), PyRep::String(pw.into()));
        assert_eq!(
            handle_stack_ops(&ctx, &mut space_sess, None, "invbroker", "StackAll", &[], &kw_cont)
                .unwrap()
                .unwrap_err()
                .code,
            "DestinationAccessDenied"
        );
        let merge_cont = PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(sc_1),
            PyRep::Integer(sc_2),
            PyRep::None,
        ])]);
        let mut kw_pw = HashMap::new();
        kw_pw.insert("password".into(), PyRep::String(pw.into()));
        assert_eq!(
            handle_stack_ops(&ctx, &mut space_sess, None, "invbroker", "MultiMerge", &[merge_cont], &kw_pw)
                .unwrap()
                .unwrap_err()
                .code,
            "DestinationAccessDenied"
        );
        assert_eq!(
            handle_stack_ops(
                &ctx,
                &mut space_sess,
                None,
                "invbroker",
                "SplitStack",
                &[
                    PyRep::Integer(struct_cont),
                    PyRep::Integer(sc_1),
                    PyRep::Integer(5),
                ],
                &kw_pw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "SplitFailed"
        );
        assert_eq!(ctx.db.tx_commit_count(), commits_before);
    }
    assert!(!ctx.db.with_snapshot(|snap| is_container_unlocked_snap(snap, struct_cont, char_id)));

    // 2. In-space container with missing ship or container position MUST fail closed before password oracle
    for (ship_pos_opt, cont_pos_opt, pw) in [
        (Some([0.0, 0.0, 0.0]), None, "wrong"),
        (Some([0.0, 0.0, 0.0]), None, "spacecontpw"),
        (None, Some([1_000.0, 0.0, 0.0]), "spacecontpw"),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            match ship_pos_opt {
                Some(pos) => {
                    snap.item_positions.insert(ship_id, pos);
                }
                None => {
                    snap.item_positions.remove(&ship_id);
                }
            }
            match cont_pos_opt {
                Some(pos) => {
                    snap.item_positions.insert(space_cont, pos);
                }
                None => {
                    snap.item_positions.remove(&space_cont);
                }
            }
        });
        let commits_before = ctx.db.tx_commit_count();
        let bound_space = make_bound(space_cont, char_id);
        let mut kw_pw = HashMap::new();
        kw_pw.insert("password".into(), PyRep::String(pw.into()));
        assert_eq!(
            handle_stack_ops(&ctx, &mut space_sess, Some(&bound_space), "invbroker", "StackAll", &[], &kw_pw)
                .unwrap()
                .unwrap_err()
                .code,
            "OutOfRange"
        );
        let merge_sp = PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(sp_1),
            PyRep::Integer(sp_2),
            PyRep::None,
        ])]);
        assert_eq!(
            handle_stack_ops(&ctx, &mut space_sess, Some(&bound_space), "invbroker", "MultiMerge", &[merge_sp], &kw_pw)
                .unwrap()
                .unwrap_err()
                .code,
            "OutOfRange"
        );
        assert_eq!(
            handle_stack_ops(
                &ctx,
                &mut space_sess,
                Some(&bound_space),
                "invbroker",
                "SplitStack",
                &[
                    PyRep::Integer(space_cont),
                    PyRep::Integer(sp_1),
                    PyRep::Integer(5),
                ],
                &kw_pw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "SplitFailed"
        );
        assert_eq!(ctx.db.tx_commit_count(), commits_before);
    }

    // Restoring both in-range positions enables normal container operations
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
        snap.item_positions.insert(space_cont, [1_200.0, 0.0, 0.0]);
    });
    let mut kw_space_ok = HashMap::new();
    kw_space_ok.insert("password".into(), PyRep::String("spacecontpw".into()));
    let space_ok = handle_stack_ops(
        &ctx,
        &mut space_sess,
        Some(&make_bound(space_cont, char_id)),
        "invbroker",
        "StackAll",
        &[],
        &kw_space_ok,
    )
    .unwrap()
    .expect("in-range space container StackAll with positions and password must succeed");
    assert_eq!(space_ok.dict_get("mergedCount"), Some(&PyRep::Integer(1)));

    // Docking in structure allows internal hangar/container operations
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().structure_id = Some(struct_id);
    });
    space_sess.structure_id = Some(struct_id);
    let mut kw_st = HashMap::new();
    kw_st.insert("locationID".into(), PyRep::Integer(struct_id));
    kw_st.insert("flag".into(), PyRep::Integer(InventoryFlag::Hangar as i64));
    let docked_ok = handle_stack_ops(&ctx, &mut space_sess, None, "invbroker", "StackAll", &[], &kw_st)
        .unwrap()
        .expect("docked structure StackAll must succeed");
    assert_eq!(docked_ok.dict_get("mergedCount"), Some(&PyRep::Integer(1)));
}

#[test]
fn paid_asset_safety_wrap_items_allow_stacking_while_unpaid_remain_locked() {
    use crate::services::station_structure::structure_upwell_ops::{
        load_ext_state, save_ext_state, AssetSafetyWrapRecord,
    };

    let ctx = test_ctx();
    let char_id = 92_000_083;
    let corp_id = 98_000_083;
    let sys_id = 30_000_142;
    let struct_id = 1_000_283_900;
    let ship_id = 1_000_283_001;
    let unpaid_wrap_id = 1_000_283_010;
    let paid_wrap_id = 1_000_283_011;
    let u_1 = 1_000_283_020;
    let u_2 = 1_000_283_021;
    let p_1 = 1_000_283_030;
    let p_2 = 1_000_283_031;
    let p_3 = 1_000_283_032;

    ensure_char(&ctx, char_id, corp_id, None, sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, struct_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, u_1, 34, char_id, struct_id, InventoryFlag::Hangar, 40, false);
    insert_item(&ctx, u_2, 34, char_id, struct_id, InventoryFlag::Hangar, 60, false);
    insert_item(&ctx, p_1, 34, char_id, struct_id, InventoryFlag::Deliveries, 40, false);
    insert_item(&ctx, p_2, 34, char_id, struct_id, InventoryFlag::Deliveries, 60, false);
    insert_item(&ctx, p_3, 34, char_id, struct_id, InventoryFlag::Deliveries, 50, false);

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Asset Safety Test Structure".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = Some(struct_id);

        let mut ext = load_ext_state(snap);
        ext.asset_safety_wraps.insert(
            unpaid_wrap_id,
            AssetSafetyWrapRecord {
                wrap_id: unpaid_wrap_id,
                owner_id: char_id,
                is_corp: false,
                source_structure_id: struct_id,
                source_structure_name: "Source".into(),
                solar_system_id: sys_id,
                created_unix: 1,
                deliverable_after_unix: 1,
                delivered_to_id: None,
                item_ids: vec![u_1, u_2],
                estimated_value_isk: 1_000.0,
                nearest_npc_station_id: None,
                auto_delivered: false,
                retrieval_fee_paid: false,
            },
        );
        // Simulate post-MoveSafetyWrapToStructure state: wrap record kept with retrieval_fee_paid=true
        ext.asset_safety_wraps.insert(
            paid_wrap_id,
            AssetSafetyWrapRecord {
                wrap_id: paid_wrap_id,
                owner_id: char_id,
                is_corp: false,
                source_structure_id: struct_id,
                source_structure_name: "Source".into(),
                solar_system_id: sys_id,
                created_unix: 1,
                deliverable_after_unix: 1,
                delivered_to_id: Some(struct_id),
                item_ids: vec![p_1, p_2, p_3],
                estimated_value_isk: 1_000.0,
                nearest_npc_station_id: None,
                auto_delivered: false,
                retrieval_fee_paid: true,
            },
        );
        save_ext_state(snap, &ext);
    });

    let mut sess = make_session(char_id, corp_id, None, Some(sys_id), Some(ship_id));
    sess.structure_id = Some(struct_id);
    let bound_struct = make_bound(struct_id, char_id);
    let kw = HashMap::new();

    // 1. Unpaid Asset Safety wrap items remain blocked across SplitStack, MultiMerge, and StackAll
    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut sess,
            Some(&bound_struct),
            "invbroker",
            "SplitStack",
            &[PyRep::Integer(u_1), PyRep::Integer(10)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "SplitFailed"
    );
    let merge_unpaid = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(u_1),
        PyRep::Integer(u_2),
        PyRep::None,
    ])]);
    assert_eq!(
        handle_stack_ops(&ctx, &mut sess, Some(&bound_struct), "invbroker", "MultiMerge", &[merge_unpaid], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "StackFailed"
    );
    let unpaid_stack = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&bound_struct),
        "invbroker",
        "StackAll",
        &[PyRep::Integer(InventoryFlag::Hangar as i64)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(unpaid_stack.dict_get("mergedCount"), Some(&PyRep::Integer(0)));

    // 2. Paid/delivered Asset Safety wrap items split, MultiMerge, and StackAll cleanly
    let split_paid = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&bound_struct),
        "invbroker",
        "SplitStack",
        &[PyRep::Integer(p_1), PyRep::Integer(15)],
        &kw,
    )
    .unwrap()
    .expect("paid Asset Safety item SplitStack must succeed");
    assert_eq!(split_paid.dict_get("originalQuantity"), Some(&PyRep::Integer(25)));
    let split_new_id = split_paid
        .dict_get("newItemID")
        .and_then(|v| v.as_i64())
        .expect("newItemID");

    let merge_paid = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(split_new_id),
        PyRep::Integer(p_2),
        PyRep::None,
    ])]);
    handle_stack_ops(&ctx, &mut sess, Some(&bound_struct), "invbroker", "MultiMerge", &[merge_paid], &kw)
        .unwrap()
        .expect("paid Asset Safety item MultiMerge must succeed");

    let stack_paid = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&bound_struct),
        "invbroker",
        "StackAll",
        &[PyRep::Integer(InventoryFlag::Deliveries as i64)],
        &kw,
    )
    .unwrap()
    .expect("paid Asset Safety item StackAll must succeed");
    assert_eq!(stack_paid.dict_get("mergedCount"), Some(&PyRep::Integer(2)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&p_1).map(|i| i.quantity), Some(150));
        assert_eq!(snap.items.get(&u_1).map(|i| i.quantity), Some(40));
        assert_eq!(snap.items.get(&u_2).map(|i| i.quantity), Some(60));
    });
}

#[test]
fn foreign_ship_bay_password_and_alsc_password_per_action_enforced() {
    use crate::services::inventory_fitting::fitting_containers_ships::{
        ContainerConfig, ShipAccessConfig,
    };

    let ctx = test_ctx();
    let owner_id = 92_000_084;
    let mate_id = 92_000_085;
    let corp_id = 98_000_084;
    let sys_id = 30_000_142;
    let station_id = 60_003_760;
    let owner_ship = 1_000_284_001;
    let mate_ship = 1_000_284_002;
    let sma_nest_cont = 1_000_284_010;
    let alsc_cont = 1_000_284_011;
    let f_1 = 1_000_284_020;
    let f_2 = 1_000_284_021;
    let m_1 = 1_000_284_022;
    let m_2 = 1_000_284_023;
    let a_1 = 1_000_284_030;
    let a_2 = 1_000_284_031;
    let a_3 = 1_000_284_032;

    ensure_char(&ctx, owner_id, corp_id, None, sys_id, owner_ship);
    ensure_char(&ctx, mate_id, corp_id, None, sys_id, mate_ship);
    insert_item(&ctx, owner_ship, 587, owner_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, mate_ship, 587, mate_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, f_1, 34, mate_id, owner_ship, InventoryFlag::FleetHangar, 30, false);
    insert_item(&ctx, f_2, 34, mate_id, owner_ship, InventoryFlag::FleetHangar, 20, false);
    insert_item(&ctx, sma_nest_cont, 17_366, mate_id, owner_ship, InventoryFlag::ShipHangar, 1, true);
    insert_item(&ctx, m_1, 34, mate_id, sma_nest_cont, InventoryFlag::Cargo, 12, false);
    insert_item(&ctx, m_2, 34, mate_id, sma_nest_cont, InventoryFlag::Cargo, 8, false);

    // Docked ALSC with password_per_action = true for part 2
    insert_item(&ctx, alsc_cont, 17_366, owner_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, a_1, 34, owner_id, alsc_cont, InventoryFlag::Cargo, 40, false);
    insert_item(&ctx, a_2, 34, owner_id, alsc_cont, InventoryFlag::Cargo, 15, false);
    insert_item(&ctx, a_3, 34, owner_id, alsc_cont, InventoryFlag::Cargo, 25, false);

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            777_002,
            FleetRecord {
                fleet_id: 777_002,
                boss_id: owner_id,
                members: vec![owner_id, mate_id],
                ..Default::default()
            },
        );
        snap.item_positions.insert(mate_ship, [0.0, 0.0, 0.0]);
        snap.item_positions.insert(owner_ship, [10_000.0, 0.0, 0.0]);
    });
    ctx.db.with_mut_snapshot(|snap| {
        set_ship_access_config_snap(
            snap,
            owner_ship,
            &ShipAccessConfig {
                allow_fleet_smb: true,
                allow_corp_smb: false,
                allow_fleet_hangar: true,
                allow_corp_hangar: false,
                password: Some("bay-secret".into()),
            },
            0,
        )
        .unwrap();
        set_container_password_snap(snap, alsc_cont, 1, "peract", 0).unwrap();
        set_container_config_snap(
            snap,
            alsc_cont,
            &ContainerConfig {
                lock_by_default: false,
                password_per_action: true,
                config_bitmask: 0,
            },
            0,
        )
        .unwrap();
        // Pre-seed ContainerUnlockedChar to ensure password_per_action ignores cached unlocks
        set_container_unlocked_snap(snap, alsc_cont, owner_id, true, 0).unwrap();
    });

    let mut mate_sess = make_session(mate_id, corp_id, None, Some(sys_id), Some(mate_ship));
    let bound_owner_ship = make_bound(owner_ship, mate_id);
    let bound_sma_cont = make_bound(sma_nest_cont, mate_id);
    let kw_empty = HashMap::new();

    // 1. Out-of-range foreign ship (> 2,500m) rejects with OutOfRange/SplitFailed BEFORE ShipAccessConfig.password
    for pw in ["wrong", "bay-secret"] {
        let mut kw_pw = HashMap::new();
        kw_pw.insert("password".into(), PyRep::String(pw.into()));
        let merge_fh = PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(f_1),
            PyRep::Integer(f_2),
            PyRep::None,
        ])]);
        assert_eq!(
            handle_stack_ops(&ctx, &mut mate_sess, Some(&bound_owner_ship), "invbroker", "MultiMerge", &[merge_fh], &kw_pw)
                .unwrap()
                .unwrap_err()
                .code,
            "OutOfRange"
        );
        assert_eq!(
            handle_stack_ops(
                &ctx,
                &mut mate_sess,
                Some(&bound_owner_ship),
                "invbroker",
                "SplitStack",
                &[PyRep::Integer(owner_ship), PyRep::Integer(f_1), PyRep::Integer(5)],
                &kw_pw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "SplitFailed"
        );
    }
    assert!(!ctx.db.with_snapshot(|snap| is_container_unlocked_snap(snap, owner_ship, mate_id)));

    // 2. Move within 2,500m: missing/wrong bay password denies FleetHangarAccessDenied & SMAAccessDenied
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(owner_ship, [500.0, 0.0, 0.0]);
    });
    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut mate_sess,
            Some(&bound_owner_ship),
            "invbroker",
            "SplitStack",
            &[PyRep::Integer(owner_ship), PyRep::Integer(f_1), PyRep::Integer(5)],
            &kw_empty,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FleetHangarAccessDenied"
    );
    let merge_sma = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(m_1),
        PyRep::Integer(m_2),
        PyRep::None,
    ])]);
    assert_eq!(
        handle_stack_ops(&ctx, &mut mate_sess, Some(&bound_sma_cont), "invbroker", "MultiMerge", &[merge_sma.clone()], &kw_empty)
            .unwrap()
            .unwrap_err()
            .code,
        "SMAAccessDenied"
    );

    // Failed SplitStack with exact bay password MUST NOT persist ship unlock
    let mut kw_bay = HashMap::new();
    kw_bay.insert("password".into(), PyRep::String("bay-secret".into()));
    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut mate_sess,
            Some(&bound_owner_ship),
            "invbroker",
            "SplitStack",
            &[PyRep::Integer(owner_ship), PyRep::Integer(f_1), PyRep::Integer(999)],
            &kw_bay,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "SplitFailed"
    );
    assert!(!ctx.db.with_snapshot(|snap| is_container_unlocked_snap(snap, owner_ship, mate_id)));

    // Valid SplitStack with bay password succeeds AND records ContainerUnlockedChar(owner_ship, mate_id)
    let ok_fh_split = handle_stack_ops(
        &ctx,
        &mut mate_sess,
        Some(&bound_owner_ship),
        "invbroker",
        "SplitStack",
        &[PyRep::Integer(owner_ship), PyRep::Integer(f_1), PyRep::Integer(10)],
        &kw_bay,
    )
    .unwrap()
    .expect("in-range FleetHangar SplitStack with bay password must succeed");
    assert_eq!(ok_fh_split.dict_get("originalQuantity"), Some(&PyRep::Integer(20)));
    assert!(ctx.db.with_snapshot(|snap| is_container_unlocked_snap(snap, owner_ship, mate_id)));

    // Subsequent MultiMerge without re-sending bay password succeeds via persisted unlock
    let split_fh_id = ok_fh_split.dict_get("newItemID").and_then(|v| v.as_i64()).unwrap();
    let merge_fh_ok = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(split_fh_id),
        PyRep::Integer(f_2),
        PyRep::None,
    ])]);
    handle_stack_ops(&ctx, &mut mate_sess, Some(&bound_owner_ship), "invbroker", "MultiMerge", &[merge_fh_ok], &kw_empty)
        .unwrap()
        .expect("subsequent MultiMerge on unlocked ship bay must succeed");

    // Clear ship unlock and confirm SMA container requires bay password then unlocks on commit
    ctx.db.with_mut_snapshot(|snap| {
        set_container_unlocked_snap(snap, owner_ship, mate_id, false, 0).unwrap();
    });
    handle_stack_ops(&ctx, &mut mate_sess, Some(&bound_sma_cont), "invbroker", "MultiMerge", &[merge_sma], &kw_bay)
        .unwrap()
        .expect("ShipHangar MultiMerge with bay password must succeed");
    assert!(ctx.db.with_snapshot(|snap| is_container_unlocked_snap(snap, owner_ship, mate_id)));

    // 3. ALSC password_per_action requires password on every SplitStack / MultiMerge / StackAll invocation
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&owner_id).unwrap().station_id = Some(station_id);
    });
    let mut owner_sess = make_session(owner_id, corp_id, Some(station_id), Some(sys_id), Some(owner_ship));
    let bound_alsc = make_bound(alsc_cont, owner_id);
    let mut kw_peract = HashMap::new();
    kw_peract.insert("password".into(), PyRep::String("peract".into()));

    assert_eq!(
        handle_stack_ops(
            &ctx,
            &mut owner_sess,
            Some(&bound_alsc),
            "invbroker",
            "SplitStack",
            &[PyRep::Integer(alsc_cont), PyRep::Integer(a_1), PyRep::Integer(10)],
            &kw_empty,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ALSCPasswordRequired"
    );
    let ok_alsc_split = handle_stack_ops(
        &ctx,
        &mut owner_sess,
        Some(&bound_alsc),
        "invbroker",
        "SplitStack",
        &[PyRep::Integer(alsc_cont), PyRep::Integer(a_1), PyRep::Integer(10)],
        &kw_peract,
    )
    .unwrap()
    .expect("SplitStack with per-action password must succeed");
    let a_split_id = ok_alsc_split.dict_get("newItemID").and_then(|v| v.as_i64()).unwrap();

    // Even immediately after a valid per-action RPC, next MultiMerge without password MUST reject
    let merge_alsc = PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(a_split_id),
        PyRep::Integer(a_2),
        PyRep::None,
    ])]);
    assert_eq!(
        handle_stack_ops(&ctx, &mut owner_sess, Some(&bound_alsc), "invbroker", "MultiMerge", &[merge_alsc.clone()], &kw_empty)
            .unwrap()
            .unwrap_err()
            .code,
        "ALSCPasswordRequired"
    );
    handle_stack_ops(&ctx, &mut owner_sess, Some(&bound_alsc), "invbroker", "MultiMerge", &[merge_alsc], &kw_peract)
        .unwrap()
        .expect("MultiMerge with per-action password must succeed");

    // Next StackAll without password MUST still reject, and with password succeeds
    assert_eq!(
        handle_stack_ops(&ctx, &mut owner_sess, Some(&bound_alsc), "invbroker", "StackAll", &[PyRep::Integer(InventoryFlag::Cargo as i64)], &kw_empty)
            .unwrap()
            .unwrap_err()
            .code,
        "ALSCPasswordRequired"
    );
    let ok_alsc_stack = handle_stack_ops(
        &ctx,
        &mut owner_sess,
        Some(&bound_alsc),
        "invbroker",
        "StackAll",
        &[PyRep::Integer(InventoryFlag::Cargo as i64)],
        &kw_peract,
    )
    .unwrap()
    .expect("StackAll with per-action password must succeed");
    assert_eq!(ok_alsc_stack.dict_get("mergedCount"), Some(&PyRep::Integer(2)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_ship_bay_list_visibility_and_multiadd_owner_transfer() {
    use crate::services::inventory_fitting::fitting_containers_ships::ShipAccessConfig;

    let mut sde_mut = (*shared_sde()).clone();
    let carrier_tid = 999_801u32;
    let mut carrier = sde_mut.types[&587].clone();
    carrier.type_id = carrier_tid;
    carrier.group_id = 30;
    carrier.category_id = 6;
    carrier.name = "Synthetic List Carrier".into();
    carrier.capacity = 1_000.0;
    carrier.default_attributes.insert(908, 1_000_000.0);
    carrier.default_attributes.insert(912, 10_000.0);
    sde_mut.types.insert(carrier_tid, carrier);
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde_mut), Arc::new(WorldRepository::with_demo_seed()));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));

    let owner_id = 92_801_001i64;
    let mate_id = 92_801_002i64;
    let owner_corp = 98_801_001i64;
    let mate_corp = 98_801_002i64;
    let sys_id = 30_000_142u32;
    let station_id = 60_003_760i64;
    let wrong_station = 60_008_494i64;
    let owner_ship = 1_000_801_001i64;
    let mate_ship = 1_000_801_002i64;
    let owner_cargo_mod = 1_000_801_010i64;
    let owner_fh_trit = 1_000_801_011i64;
    let owner_sma_rifter = 1_000_801_012i64;
    let mate_hangar_trit = 1_000_801_013i64;
    let mate_hangar_pyerite = 1_000_801_014i64;

    ensure_char(&ctx, owner_id, owner_corp, Some(station_id), sys_id, owner_ship);
    ensure_char(&ctx, mate_id, mate_corp, Some(station_id), sys_id, mate_ship);
    insert_item(&ctx, owner_ship, carrier_tid, owner_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, mate_ship, 587, mate_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, owner_cargo_mod, 34, owner_id, owner_ship, InventoryFlag::Cargo, 5, false);
    insert_item(&ctx, owner_fh_trit, 34, owner_id, owner_ship, InventoryFlag::FleetHangar, 20, false);
    insert_item(&ctx, owner_sma_rifter, 587, owner_id, owner_ship, InventoryFlag::ShipHangar, 1, true);
    insert_item(&ctx, mate_hangar_trit, 34, mate_id, station_id, InventoryFlag::Hangar, 15, false);
    insert_item(&ctx, mate_hangar_pyerite, 35, mate_id, station_id, InventoryFlag::Hangar, 8, false);

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            777_801,
            FleetRecord {
                fleet_id: 777_801,
                boss_id: owner_id,
                members: vec![owner_id, mate_id],
                ..Default::default()
            },
        );
    });
    ctx.db.with_mut_snapshot(|snap| {
        set_ship_access_config_snap(
            snap,
            owner_ship,
            &ShipAccessConfig {
                allow_fleet_smb: true,
                allow_corp_smb: false,
                allow_fleet_hangar: true,
                allow_corp_hangar: false,
                password: None,
            },
            0,
        )
        .unwrap();
    });

    let owner_sess = make_session(owner_id, owner_corp, Some(station_id), Some(sys_id), Some(owner_ship));
    let mut mate_sess = make_session(mate_id, mate_corp, Some(station_id), Some(sys_id), Some(mate_ship));
    let bound_owner_for_owner = make_bound(owner_ship, owner_id);
    let bound_owner_for_mate = make_bound(owner_ship, mate_id);
    let kw = HashMap::new();

    let extract_ids = |rep: PyRep| -> Vec<i64> {
        let PyRep::ObjectEx { list, .. } = rep else {
            panic!("expected ObjectEx rowset, got {rep:?}");
        };
        list.iter().filter_map(|r| dbrow_col_i64(r, "itemID")).collect()
    };

    // 1. Authorized fleetmate List returns 90 | 155 items only (never owner's Cargo flag 5).
    let mate_list_ids = extract_ids(
        handle_list(&ctx, &mate_sess, Some(&bound_owner_for_mate), &[], &kw).expect("foreign ship list"),
    );
    assert!(mate_list_ids.contains(&owner_fh_trit));
    assert!(mate_list_ids.contains(&owner_sma_rifter));
    assert!(!mate_list_ids.contains(&owner_cargo_mod));

    // 2. MultiAdd by fleetmate into owner_ship FleetHangar transfers owner_id = owner_id, notifies owner_id, and Pilot A sees both items in List.
    let _ = ctx.drain_notifications_for_char(owner_id);
    let moved_ids = handle_multi_add(
        &ctx,
        &mut mate_sess,
        Some(&bound_owner_for_mate),
        &[
            PyRep::List(vec![PyRep::Integer(mate_hangar_trit), PyRep::Integer(mate_hangar_pyerite)]),
            PyRep::Integer(owner_ship),
            PyRep::Integer(155),
        ],
        &kw,
    )
    .await
    .expect("MultiAdd into foreign FleetHangar");
    assert!(!matches!(moved_ids, PyRep::None));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&mate_hangar_trit].quantity, 15);
        assert_eq!(snap.items[&mate_hangar_trit].owner_id, owner_id);
        assert_eq!(snap.items[&mate_hangar_trit].location_id, owner_ship);
        assert_eq!(snap.items[&mate_hangar_trit].flag, InventoryFlag::FleetHangar);
        assert_eq!(snap.items[&mate_hangar_pyerite].owner_id, owner_id);
        assert_eq!(snap.items[&mate_hangar_pyerite].location_id, owner_ship);
        assert_eq!(snap.items[&mate_hangar_pyerite].flag, InventoryFlag::FleetHangar);
    });
    let owner_list_ids = extract_ids(
        handle_list(&ctx, &owner_sess, Some(&bound_owner_for_owner), &[], &kw).expect("owner list"),
    );
    assert!(owner_list_ids.contains(&mate_hangar_trit));
    assert!(owner_list_ids.contains(&mate_hangar_pyerite));
    let owner_notifs = ctx.drain_notifications_for_char(owner_id);
    assert!(owner_notifs.iter().any(|n| n.event == "OnItemChange"));
    assert!(owner_notifs.iter().any(|n| n.event == "ProcessInventoryChange"));

    // 3. Disabling allow_fleet_hangar hides flag 155 and rejects ListByFlags([155]), while SMA flag 90 remains readable.
    ctx.db.with_mut_snapshot(|snap| {
        let mut cfg = ship_access_config_snap(snap, owner_ship);
        cfg.allow_fleet_hangar = false;
        set_ship_access_config_snap(snap, owner_ship, &cfg, 0).unwrap();
    });
    assert_eq!(
        handle_list_by_flags(
            &ctx,
            &mate_sess,
            Some(&bound_owner_for_mate),
            &[PyRep::List(vec![PyRep::Integer(155)])],
            &kw,
        )
        .unwrap_err()
        .code,
        "FleetHangarAccessDenied"
    );
    let sma_only_ids = extract_ids(
        handle_list(&ctx, &mate_sess, Some(&bound_owner_for_mate), &[], &kw).expect("list with only SMA allowed"),
    );
    assert_eq!(sma_only_ids, vec![owner_sma_rifter]);

    // 4. Wrong dock returns BoundObjectOwnershipViolation; undocked wrong system -> NotInSameSolarSystem; > 2,500m -> OutOfRange.
    let wrong_dock_sess = make_session(mate_id, mate_corp, Some(wrong_station), Some(sys_id), Some(mate_ship));
    assert_eq!(
        handle_list(&ctx, &wrong_dock_sess, Some(&bound_owner_for_mate), &[], &kw)
            .unwrap_err()
            .code,
        "BoundObjectOwnershipViolation"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&owner_ship).unwrap().location_id = i64::from(sys_id);
        snap.item_positions.insert(owner_ship, [10_000.0, 0.0, 0.0]);
        snap.item_positions.insert(mate_ship, [0.0, 0.0, 0.0]);
    });
    let wrong_sys_sess = make_session(mate_id, mate_corp, None, Some(30_000_144), Some(mate_ship));
    assert_eq!(
        handle_list(&ctx, &wrong_sys_sess, Some(&bound_owner_for_mate), &[], &kw)
            .unwrap_err()
            .code,
        "NotInSameSolarSystem"
    );
    let far_space_sess = make_session(mate_id, mate_corp, None, Some(sys_id), Some(mate_ship));
    assert_eq!(
        handle_list(&ctx, &far_space_sess, Some(&bound_owner_for_mate), &[], &kw)
            .unwrap_err()
            .code,
        "OutOfRange"
    );

    // 5. Within 2,500m after fleet kick: List is empty and explicit ListByFlags returns SMAAccessDenied.
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(owner_ship, [500.0, 0.0, 0.0]);
        snap.fleets.get_mut(&777_801).unwrap().members.retain(|&m| m != mate_id);
    });
    assert!(extract_ids(handle_list(&ctx, &far_space_sess, Some(&bound_owner_for_mate), &[], &kw).unwrap()).is_empty());
    assert_eq!(
        handle_list_by_flags(
            &ctx,
            &far_space_sess,
            Some(&bound_owner_for_mate),
            &[PyRep::List(vec![PyRep::Integer(90)])],
            &kw,
        )
        .unwrap_err()
        .code,
        "SMAAccessDenied"
    );
}

#[tokio::test]
async fn gh_1311_get_container_contents_remote_docked_assets_and_3arg_password() {
    let ctx = test_ctx();
    let char_id = 95_920_001_i64;
    let other_id = 95_920_002_i64;
    let corp_id = 98_920_001_i64;
    let other_corp_id = 98_920_002_i64;

    let jita_station = 60_003_760_i64; // Jita 4-4 (30000142)
    let jita_sys = 30_000_142_u32;
    let amarr_station = 60_008_494_i64; // Amarr VIII (30002187)
    let amarr_sys = 30_002_187_u32;
    let keepstar_id = 1_001_896_944_936_i64;
    let destroyed_keepstar_id = 1_001_896_944_937_i64;

    let active_jita_ship = 1_897_147_461_i64;
    let remote_avatar = 1_897_147_462_i64; // Avatar (11567) in Keepstar
    let avatar_cargo_trit = 1_897_147_463_i64;
    let nested_can = 1_897_147_464_i64; // Small Standard Container (3293) inside Avatar
    let nested_can_trit = 1_897_147_465_i64;
    let remote_npc_ship = 1_897_147_466_i64; // Rifter (587) at Amarr
    let remote_npc_trit = 1_897_147_467_i64;
    let remote_alsc = 1_897_147_468_i64; // Small Audit Log Secure Container (17363) in Keepstar
    let remote_alsc_trit = 1_897_147_469_i64;
    let jita_alsc = 1_897_147_470_i64; // Local ALSC at Jita
    let jita_alsc_trit = 1_897_147_471_i64;
    let foreign_avatar = 1_897_147_472_i64;
    let owned_can_in_foreign_titan = 1_897_147_473_i64;
    let space_owned_can = 1_897_147_474_i64;
    let space_remote_sys_can = 1_897_147_475_i64;
    let destroyed_keepstar_ship = 1_897_147_476_i64;

    ensure_char(&ctx, char_id, corp_id, Some(jita_station), jita_sys, active_jita_ship);
    ensure_char(&ctx, other_id, other_corp_id, Some(amarr_station), amarr_sys, foreign_avatar);

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            keepstar_id,
            eve_db::UpwellStructureRecord {
                structure_id: keepstar_id,
                type_id: 35834,
                owner_corp_id: corp_id,
                solar_system_id: amarr_sys,
                name: "1DQ1-A Keepstar".into(),
                state: "ShieldVulnerable".into(),
                position: [1_000.0, 0.0, 0.0],
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.structures.insert(
            destroyed_keepstar_id,
            eve_db::UpwellStructureRecord {
                structure_id: destroyed_keepstar_id,
                type_id: 35834,
                owner_corp_id: corp_id,
                solar_system_id: amarr_sys,
                name: "Destroyed Keepstar".into(),
                state: "Destroyed".into(),
                position: [5_000.0, 0.0, 0.0],
                quantum_core_installed: false,
                fuel_blocks: 0,
                ..Default::default()
            },
        );
    });

    insert_item(&ctx, active_jita_ship, 587, char_id, jita_station, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, remote_avatar, 11567, char_id, keepstar_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, avatar_cargo_trit, 34, char_id, remote_avatar, InventoryFlag::Cargo, 500, false);
    insert_item(&ctx, nested_can, 3293, char_id, remote_avatar, InventoryFlag::Cargo, 1, true);
    insert_item(&ctx, nested_can_trit, 34, char_id, nested_can, InventoryFlag::Cargo, 42, false);
    insert_item(&ctx, remote_npc_ship, 587, char_id, amarr_station, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, remote_npc_trit, 34, char_id, remote_npc_ship, InventoryFlag::Cargo, 77, false);
    insert_item(&ctx, remote_alsc, 17363, char_id, keepstar_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, remote_alsc_trit, 34, char_id, remote_alsc, InventoryFlag::Cargo, 99, false);
    insert_item(&ctx, jita_alsc, 17363, char_id, jita_station, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, jita_alsc_trit, 34, char_id, jita_alsc, InventoryFlag::Cargo, 88, false);
    insert_item(&ctx, foreign_avatar, 11567, other_id, keepstar_id, InventoryFlag::Hangar, 1, true);
    insert_item(
        &ctx,
        owned_can_in_foreign_titan,
        3293,
        char_id,
        foreign_avatar,
        InventoryFlag::FleetHangar,
        1,
        true,
    );
    insert_item(&ctx, space_owned_can, 23, char_id, i64::from(jita_sys), InventoryFlag::None, 1, true);
    insert_item(&ctx, space_remote_sys_can, 23, char_id, i64::from(amarr_sys), InventoryFlag::None, 1, true);
    insert_item(
        &ctx,
        destroyed_keepstar_ship,
        587,
        char_id,
        destroyed_keepstar_id,
        InventoryFlag::Hangar,
        1,
        true,
    );

    ctx.db.with_mut_snapshot(|snap| {
        set_container_password_snap(snap, remote_alsc, 1, "pw-remote", 0).unwrap();
        set_container_password_snap(snap, jita_alsc, 1, "pw-jita", 0).unwrap();
    });

    let jita_sess = make_session(char_id, corp_id, Some(jita_station), Some(jita_sys), Some(active_jita_ship));
    let jita_bound = make_bound(jita_station, char_id);
    let space_jita_sess = make_session(char_id, corp_id, None, Some(jita_sys), Some(active_jita_ship));
    let kw = HashMap::new();

    let extract_ids = |rep: PyRep| -> Vec<i64> {
        let PyRep::ObjectEx { list, .. } = rep else {
            panic!("expected ObjectEx rowset, got {rep:?}");
        };
        list.iter().filter_map(|r| dbrow_col_i64(r, "itemID")).collect()
    };

    // 1. GH #1311 exact repro: docked at Jita 4-4, GetContainerContents(1897147462, 1001896944936, None)
    //    on own remote Avatar in Keepstar succeeds both unbound and bound to active Jita invbroker.
    let avatar_unbound_ids = extract_ids(
        handle_get_container_contents(
            &ctx,
            &jita_sess,
            None,
            &[PyRep::Integer(remote_avatar), PyRep::Integer(keepstar_id), PyRep::None],
            &kw,
        )
        .await
        .expect("remote Avatar GetContainerContents unbound succeeds"),
    );
    assert!(avatar_unbound_ids.contains(&avatar_cargo_trit));
    assert!(avatar_unbound_ids.contains(&nested_can));

    let avatar_bound_ids = extract_ids(
        handle_get_container_contents(
            &ctx,
            &jita_sess,
            Some(&jita_bound),
            &[PyRep::Integer(remote_avatar), PyRep::Integer(keepstar_id), PyRep::None],
            &kw,
        )
        .await
        .expect("remote Avatar GetContainerContents bound to Jita station invbroker succeeds"),
    );
    assert_eq!(avatar_bound_ids, avatar_unbound_ids);

    // 2. Nested owned container inside remote Avatar and owned ship at remote NPC station (Amarr) also succeed.
    let nested_ids = extract_ids(
        handle_get_container_contents(
            &ctx,
            &jita_sess,
            Some(&jita_bound),
            &[PyRep::Integer(nested_can), PyRep::Integer(remote_avatar), PyRep::None],
            &kw,
        )
        .await
        .expect("nested owned container in remote Avatar succeeds"),
    );
    assert_eq!(nested_ids, vec![nested_can_trit]);

    let npc_remote_ids = extract_ids(
        handle_get_container_contents(
            &ctx,
            &jita_sess,
            None,
            &[PyRep::Integer(remote_npc_ship), PyRep::Integer(amarr_station), PyRep::None],
            &kw,
        )
        .await
        .expect("remote NPC station ship GetContainerContents succeeds"),
    );
    assert_eq!(npc_remote_ids, vec![remote_npc_trit]);

    // 3. V24.02 3-arg `(containerID, locationID, password)` parsing on local & remote passworded ALSCs.
    let wrong_pw_err = handle_get_container_contents(
        &ctx,
        &jita_sess,
        Some(&jita_bound),
        &[PyRep::Integer(jita_alsc), PyRep::Integer(jita_station), PyRep::String("wrong".into())],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(wrong_pw_err.code, "ALSCPasswordRequired");

    let local_alsc_ids = extract_ids(
        handle_get_container_contents(
            &ctx,
            &jita_sess,
            Some(&jita_bound),
            &[PyRep::Integer(jita_alsc), PyRep::Integer(jita_station), PyRep::String("pw-jita".into())],
            &kw,
        )
        .await
        .expect("3-arg password unlocks Jita ALSC"),
    );
    assert_eq!(local_alsc_ids, vec![jita_alsc_trit]);

    let remote_alsc_none = handle_get_container_contents(
        &ctx,
        &jita_sess,
        None,
        &[PyRep::Integer(remote_alsc), PyRep::Integer(keepstar_id), PyRep::None],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(remote_alsc_none.code, "ALSCPasswordRequired");

    let remote_alsc_ids = extract_ids(
        handle_get_container_contents(
            &ctx,
            &jita_sess,
            None,
            &[PyRep::Integer(remote_alsc), PyRep::Integer(keepstar_id), PyRep::String("pw-remote".into())],
            &kw,
        )
        .await
        .expect("3-arg password unlocks remote Keepstar ALSC"),
    );
    assert_eq!(remote_alsc_ids, vec![remote_alsc_trit]);
    ctx.db.with_snapshot(|snap| {
        assert!(is_container_unlocked_snap(snap, remote_alsc, char_id));
        assert!(is_container_unlocked_snap(snap, jita_alsc, char_id));
        assert!(container_alsc_logs_snap(snap, jita_alsc)
            .iter()
            .any(|e| e.action == "ALSCActionOpen"));
    });

    // 4. Negative security checks:
    //    - Foreign remote Avatar -> CantDoThatWithSomeoneElsesStuff
    assert_eq!(
        handle_get_container_contents(
            &ctx,
            &jita_sess,
            Some(&jita_bound),
            &[PyRep::Integer(foreign_avatar), PyRep::Integer(keepstar_id), PyRep::None],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CantDoThatWithSomeoneElsesStuff"
    );
    //    - Owned container inside a foreign Titan -> blocked (#1290 foreign traversal barrier preserved)
    assert_eq!(
        handle_get_container_contents(
            &ctx,
            &jita_sess,
            None,
            &[PyRep::Integer(owned_can_in_foreign_titan), PyRep::Integer(foreign_avatar), PyRep::None],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CantDoThatWithSomeoneElsesStuff"
    );
    //    - Owned ship in Destroyed Upwell structure -> blocked
    assert_eq!(
        handle_get_container_contents(
            &ctx,
            &jita_sess,
            None,
            &[PyRep::Integer(destroyed_keepstar_ship), PyRep::Integer(destroyed_keepstar_id), PyRep::None],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CantDoThatWithSomeoneElsesStuff"
    );
    //    - Owned space container while docked or in a remote solar system -> must NOT bypass space range limit
    assert_eq!(
        handle_get_container_contents(
            &ctx,
            &jita_sess,
            None,
            &[PyRep::Integer(space_owned_can), PyRep::Integer(i64::from(jita_sys)), PyRep::None],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CantDoThatWithSomeoneElsesStuff"
    );
    assert_eq!(
        handle_get_container_contents(
            &ctx,
            &space_jita_sess,
            None,
            &[PyRep::Integer(space_remote_sys_can), PyRep::Integer(i64::from(amarr_sys)), PyRep::None],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CantDoThatWithSomeoneElsesStuff"
    );
    //    - Mismatched bound moniker (bound to Amarr while docked in Jita) -> rejected
    let stale_amarr_bound = make_bound(amarr_station, char_id);
    assert_eq!(
        handle_get_container_contents(
            &ctx,
            &jita_sess,
            Some(&stale_amarr_bound),
            &[PyRep::Integer(remote_avatar), PyRep::Integer(keepstar_id), PyRep::None],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CantDoThatWithSomeoneElsesStuff"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gh_1317_suspect_jetcan_wreck_looting_without_ball_preserves_range_and_acl() {
    use eve_destiny::{Ball, MovementMode};

    let ctx = test_ctx();
    let jita_sys: u32 = 30_000_142;
    let looter_id = 90_000_002_i64;
    let owner_id = 90_000_001_i64;
    let looter_ship = 96_317_001_i64;
    let unpos_jetcan = 96_317_010_i64;
    let jetcan_loot_add = 96_317_011_i64;
    let jetcan_loot_multi = 96_317_012_i64;
    let unpos_wreck = 96_317_020_i64;
    let wreck_loot = 96_317_021_i64;
    let far_jetcan = 96_317_030_i64;
    let far_jetcan_loot = 96_317_031_i64;
    let unpos_secure_can = 96_317_040_i64;
    let secure_can_loot = 96_317_041_i64;
    let unspawned_looter_id = 96_317_090_i64;
    let unspawned_ship = 96_317_091_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&looter_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = jita_sys;
            ch.active_ship_id = looter_ship;
        }
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.corporation_id = 1_000_009;
        }
        let mut phantom = snap.characters[&looter_id].clone();
        phantom.character_id = unspawned_looter_id;
        phantom.user_id = 917_090;
        phantom.name = "PhantomLooter".into();
        phantom.active_ship_id = unspawned_ship;
        phantom.corporation_id = 1_000_044;
        snap.characters.insert(unspawned_looter_id, phantom);
    });

    insert_item(&ctx, looter_ship, 587, looter_id, i64::from(jita_sys), InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, unspawned_ship, 587, unspawned_looter_id, i64::from(jita_sys), InventoryFlag::Hangar, 1, true);

    // Suspect jetcan (type_id 23) and wreck (type_id 26557, group_id 186) in space without Ball/item_positions.
    insert_item(&ctx, unpos_jetcan, 23, owner_id, i64::from(jita_sys), InventoryFlag::None, 1, true);
    insert_item(&ctx, jetcan_loot_add, 34, owner_id, unpos_jetcan, InventoryFlag::Cargo, 10, false);
    insert_item(&ctx, jetcan_loot_multi, 35, owner_id, unpos_jetcan, InventoryFlag::Cargo, 5, false);

    insert_item(&ctx, unpos_wreck, 26_557, owner_id, i64::from(jita_sys), InventoryFlag::None, 1, true);
    insert_item(&ctx, wreck_loot, 36, owner_id, unpos_wreck, InventoryFlag::Cargo, 8, false);

    // Positioned far jetcan (> 2,500m) must STILL be rejected.
    insert_item(&ctx, far_jetcan, 23, owner_id, i64::from(jita_sys), InventoryFlag::None, 1, true);
    insert_item(&ctx, far_jetcan_loot, 34, owner_id, far_jetcan, InventoryFlag::Cargo, 12, false);

    // Non-loot secure container without position must STILL fail closed.
    insert_item(&ctx, unpos_secure_can, 3293, owner_id, i64::from(jita_sys), InventoryFlag::None, 1, true);
    insert_item(&ctx, secure_can_loot, 34, owner_id, unpos_secure_can, InventoryFlag::Cargo, 4, false);

    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(far_jetcan, [3_500.0, 0.0, 0.0]);
    });
    let _ = ctx
        .systems
        .get_or_spawn(jita_sys)
        .spawn_ball(Ball {
            id: looter_ship,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let mut looter_sess = make_session(looter_id, 1_000_045, None, Some(jita_sys), Some(looter_ship));
    let mut phantom_sess = make_session(unspawned_looter_id, 1_000_044, None, Some(jita_sys), Some(unspawned_ship));
    let kw = HashMap::new();

    // 1. Phantom looter without ship ball in space still cannot loot unpositioned jetcan.
    assert_eq!(
        crate::services::inventory_fitting::handle(
            &ctx,
            &mut phantom_sess,
            None,
            "invbroker",
            "Add",
            &[PyRep::Integer(jetcan_loot_add), PyRep::Integer(unpos_jetcan)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ItemOwnershipViolation"
    );

    // 2. Non-loot secure container without ball still fails closed.
    assert_eq!(
        crate::services::inventory_fitting::handle(
            &ctx,
            &mut looter_sess,
            None,
            "invbroker",
            "Add",
            &[PyRep::Integer(secure_can_loot), PyRep::Integer(unpos_secure_can)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ItemOwnershipViolation"
    );

    // 3. Out-of-range positioned jetcan (> 2,500m) still fails on Add and MultiAdd.
    assert_eq!(
        crate::services::inventory_fitting::handle(
            &ctx,
            &mut looter_sess,
            None,
            "invbroker",
            "Add",
            &[PyRep::Integer(far_jetcan_loot), PyRep::Integer(far_jetcan)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ItemOwnershipViolation"
    );
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut looter_sess,
            None,
            &[
                PyRep::List(vec![PyRep::Integer(far_jetcan_loot)]),
                PyRep::Integer(looter_ship),
                PyRep::Integer(InventoryFlag::Cargo as i64),
            ],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "ItemOwnershipViolation"
    );

    // 4. Undocked pilot with active ball loots foreign unpositioned jetcan via Add (triggers Suspect!) and MultiAdd.
    crate::services::inventory_fitting::handle(
        &ctx,
        &mut looter_sess,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(jetcan_loot_add), PyRep::Integer(unpos_jetcan)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Add from foreign unpositioned jetcan must succeed");
    let moved_jetcan_item = ctx.db.get_item(jetcan_loot_add).expect("looted jetcan item exists");
    assert_eq!(
        (moved_jetcan_item.location_id, moved_jetcan_item.owner_id),
        (looter_ship, looter_id),
        "looted jetcan item must land inside looter_ship owned by looter"
    );
    assert!(
        ctx.crimewatch
            .read()
            .unwrap()
            .is_suspect(looter_id, ctx.now_unix()),
        "looting foreign jetcan must trigger Crimewatch Suspect flag"
    );
    handle_multi_add(
        &ctx,
        &mut looter_sess,
        None,
        &[
            PyRep::List(vec![PyRep::Integer(jetcan_loot_multi)]),
            PyRep::Integer(looter_ship),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
    )
    .await
    .expect("MultiAdd from foreign unpositioned jetcan must succeed");
    assert_eq!(
        ctx.db.get_item(jetcan_loot_multi).unwrap().location_id,
        looter_ship,
        "MultiAdd looted jetcan item must land inside looter_ship"
    );

    // 5. Undocked pilot with active ball loots foreign unpositioned wreck (group_id == 186) via Add.
    crate::services::inventory_fitting::handle(
        &ctx,
        &mut looter_sess,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(wreck_loot), PyRep::Integer(unpos_wreck)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Add from foreign unpositioned wreck must succeed");
    let moved_wreck_item = ctx.db.get_item(wreck_loot).expect("looted wreck item exists");
    assert_eq!(
        (moved_wreck_item.location_id, moved_wreck_item.owner_id),
        (looter_ship, looter_id),
        "looted wreck item must land inside looter_ship owned by looter"
    );

    // 6. Putting an item INTO a foreign unpositioned wreck still fails closed with DestinationAccessDenied.
    assert_eq!(
        crate::services::inventory_fitting::handle(
            &ctx,
            &mut looter_sess,
            None,
            "invbroker",
            "Add",
            &[PyRep::Integer(wreck_loot), PyRep::Integer(unpos_wreck)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "DestinationAccessDenied"
    );
}

#[tokio::test]
async fn get_container_contents_rejected_open_does_not_forge_alsc_log_or_unlock() {
    let ctx = test_ctx();
    let owner_id = 90_000_001_i64;
    let intruder_id = 90_000_002_i64;
    let station_id = 60_003_760_i64;
    let owner_ship = 9_150_001_i64;
    let intruder_ship = 9_150_002_i64;
    let alsc_id = 9_250_001_i64;
    let alsc_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 448)
        .map(|t| t.type_id)
        .expect("SDE must contain group 448 ALSC type");

    ensure_char(&ctx, owner_id, 98_000_001, Some(station_id), 30_000_142, owner_ship);
    ensure_char(&ctx, intruder_id, 98_000_002, Some(station_id), 30_000_142, intruder_ship);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            owner_ship,
            InventoryItemRecord {
                item_id: owner_ship,
                type_id: 587,
                owner_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            intruder_ship,
            InventoryItemRecord {
                item_id: intruder_ship,
                type_id: 587,
                owner_id: intruder_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            alsc_id,
            InventoryItemRecord {
                item_id: alsc_id,
                type_id: alsc_type_id,
                owner_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    ctx.db.with_mut_snapshot(|snap| {
        set_container_password_snap(snap, alsc_id, 1, "vault-pw", 0).unwrap();
    });

    let mut owner_sess = make_session(owner_id, 98_000_001, Some(station_id), Some(30_000_142), Some(owner_ship));
    let mut intruder_sess = make_session(intruder_id, 98_000_002, Some(station_id), Some(30_000_142), Some(intruder_ship));
    let kw = HashMap::new();

    // Intruder knowing the container password still fails container ownership check and MUST NOT
    // record unlock state or ALSCActionOpen audit log entry.
    let err = crate::services::inventory_fitting::handle(
        &ctx,
        &mut intruder_sess,
        None,
        "invbroker",
        "GetContainerContents",
        &[PyRep::Integer(alsc_id), PyRep::None, PyRep::String("vault-pw".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(!err.code.is_empty());

    ctx.db.with_snapshot(|snap| {
        assert!(
            !is_container_unlocked_snap(snap, alsc_id, intruder_id),
            "rejected GetContainerContents must not mark container unlocked"
        );
        assert!(
            container_alsc_logs_snap(snap, alsc_id).is_empty(),
            "rejected GetContainerContents must not append ALSCActionOpen log"
        );
    });

    // Owner opening with valid password succeeds and appends ALSCActionOpen.
    crate::services::inventory_fitting::handle(
        &ctx,
        &mut owner_sess,
        None,
        "invbroker",
        "GetContainerContents",
        &[PyRep::Integer(alsc_id), PyRep::None, PyRep::String("vault-pw".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("owner GetContainerContents with valid password succeeds");
    ctx.db.with_snapshot(|snap| {
        assert!(is_container_unlocked_snap(snap, alsc_id, owner_id));
        assert_eq!(
            container_alsc_logs_snap(snap, alsc_id)
                .last()
                .map(|e| e.action.as_str()),
            Some("ALSCActionOpen")
        );
    });
}

#[tokio::test]
async fn multi_add_and_auto_fit_enforce_alsc_lock_and_commit_logs_atomically() {
    let ctx = test_ctx();
    let char_id = 92_000_260_i64;
    let corp_id = 98_000_002_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 1_000_265_001_i64;
    let alsc_id = 1_000_265_002_i64;
    let locked_ore = 1_000_265_003_i64;
    let locked_gun = 1_000_265_004_i64;
    let unlocked_guns = 1_000_265_005_i64;
    let hangar_ice = 1_000_265_006_i64;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), 30_000_142, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, alsc_id, 17366, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, locked_ore, 34, char_id, alsc_id, InventoryFlag::Cargo, 10, false);
    insert_item(&ctx, locked_gun, 484, char_id, alsc_id, InventoryFlag::Cargo, 1, false);
    insert_item(&ctx, unlocked_guns, 484, char_id, alsc_id, InventoryFlag::Cargo, 2, false);
    insert_item(&ctx, hangar_ice, 17888, char_id, station_id, InventoryFlag::Hangar, 5, false);

    ctx.db.with_mut_snapshot(|snap| {
        for &sk in &[3300u32, 3302, 3318, 3392, 3426] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                eve_db::TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        set_container_config_snap(
            snap,
            alsc_id,
            &crate::services::inventory_fitting::fitting_containers_ships::ContainerConfig {
                lock_by_default: true,
                password_per_action: false,
                config_bitmask: 0,
            },
            0,
        )
        .unwrap();
        set_alsc_item_locked_snap(snap, locked_ore, true, 0).unwrap();
        set_alsc_item_locked_snap(snap, locked_gun, true, 0).unwrap();
    });

    let kw = HashMap::new();
    let mut sess = make_session(char_id, corp_id, Some(station_id), None, Some(ship_id));
    let ship_bound = make_bound(ship_id, char_id);
    let alsc_bound = make_bound(alsc_id, char_id);

    let commits_before = ctx.db.tx_commit_count();
    let ore_err = handle_multi_add(
        &ctx,
        &mut sess,
        Some(&ship_bound),
        &[
            PyRep::List(vec![PyRep::Integer(locked_ore)]),
            PyRep::Integer(alsc_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(ore_err.code, "ALSCItemLocked");

    let gun_err = handle_multi_add(
        &ctx,
        &mut sess,
        Some(&ship_bound),
        &[
            PyRep::List(vec![PyRep::Integer(locked_gun)]),
            PyRep::Integer(alsc_id),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(gun_err.code, "ALSCItemLocked");
    assert_eq!(ctx.db.tx_commit_count(), commits_before);
    ctx.db.with_snapshot(|snap| {
        assert!(container_alsc_logs_snap(snap, alsc_id).is_empty());
        assert_eq!(snap.items[&locked_ore].location_id, alsc_id);
        assert_eq!(snap.items[&locked_gun].location_id, alsc_id);
    });

    // Moving item into ALSC with lock_by_default commits move + lock + ALSC logs in 1 atomic tx.
    handle_multi_add(
        &ctx,
        &mut sess,
        Some(&alsc_bound),
        &[
            PyRep::List(vec![PyRep::Integer(hangar_ice)]),
            PyRep::Integer(station_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw,
    )
    .await
    .expect("MultiAdd into lock_by_default ALSC succeeds");
    assert_eq!(ctx.db.tx_commit_count(), commits_before + 1);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&hangar_ice].location_id, alsc_id);
        assert!(snap.is_item_in_alsc_lock(hangar_ice));
        let actions: Vec<String> = container_alsc_logs_snap(snap, alsc_id)
            .into_iter()
            .map(|e| e.action)
            .collect();
        assert_eq!(actions, vec!["ALSCActionLock", "ALSCActionAdd"]);
    });

    // Auto-fit from ALSC splits stack, fits into HiSlot0, and logs ALSCActionTake in 1 tx.
    let fit_rep = handle_multi_add(
        &ctx,
        &mut sess,
        Some(&ship_bound),
        &[
            PyRep::List(vec![PyRep::Integer(unlocked_guns)]),
            PyRep::Integer(alsc_id),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .await
    .expect("auto-fit from ALSC succeeds");
    let PyRep::List(fitted_items) = fit_rep.dict_get("items").unwrap() else {
        panic!("expected fitted items list");
    };
    let fitted_id = fitted_items[0].as_i64().unwrap();
    assert_eq!(ctx.db.tx_commit_count(), commits_before + 2);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&unlocked_guns].location_id, alsc_id);
        assert_eq!(snap.items[&unlocked_guns].quantity, 1);
        assert_eq!(snap.items[&fitted_id].location_id, ship_id);
        assert_eq!(snap.items[&fitted_id].flag, InventoryFlag::HiSlot0);
        let logs = container_alsc_logs_snap(snap, alsc_id);
        let actions: Vec<String> = logs.iter().map(|e| e.action.clone()).collect();
        assert_eq!(actions, vec!["ALSCActionLock", "ALSCActionAdd", "ALSCActionTake"]);
        assert_eq!(
            logs.last().unwrap().item_id,
            unlocked_guns,
            "auto-fit split-take ALSCActionTake must record original source stack item_id"
        );
    });
}


#[test]
fn bf_1180_list_nested_ship_bay_unlock_and_concurrent_acl_revocation_fail_closed() {
    use crate::services::inventory_fitting::fitting_containers_ships::{
        verify_nested_unit_list_in_snap, ContainerConfig, ShipAccessConfig,
    };

    let ctx = test_ctx();
    let owner_id = 92_880_001_i64;
    let mate_id = 92_880_002_i64;
    let corp_id = 98_880_001_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let owner_ship = 1_880_100_001_i64;
    let mate_ship = 1_880_100_002_i64;
    let nest_box = 1_880_100_010_i64;
    let nest_item = 1_880_100_020_i64;
    let sma_item = 1_880_100_030_i64;

    ensure_char(&ctx, owner_id, corp_id, Some(station_id), sys_id, owner_ship);
    ensure_char(&ctx, mate_id, corp_id, Some(station_id), sys_id, mate_ship);
    insert_item(&ctx, owner_ship, 587, owner_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, mate_ship, 587, mate_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, nest_box, 17_366, owner_id, owner_ship, InventoryFlag::ShipHangar, 1, true);
    insert_item(&ctx, nest_item, 34, owner_id, nest_box, InventoryFlag::Cargo, 42, false);
    insert_item(&ctx, sma_item, 587, owner_id, owner_ship, InventoryFlag::ShipHangar, 1, true);

    ctx.db.with_mut_snapshot(|snap| {
        set_ship_access_config_snap(
            snap,
            owner_ship,
            &ShipAccessConfig {
                allow_fleet_smb: false,
                allow_corp_smb: true,
                allow_fleet_hangar: false,
                allow_corp_hangar: false,
                password: Some("host-pw".into()),
            },
            0,
        )
        .unwrap();
        set_container_config_snap(
            snap,
            nest_box,
            &ContainerConfig {
                lock_by_default: false,
                password_per_action: false,
                config_bitmask: 0,
            },
            0,
        )
        .unwrap();
    });

    let mate_sess = make_session(mate_id, corp_id, Some(station_id), Some(sys_id), Some(mate_ship));
    let bound_nest = make_bound(nest_box, mate_id);
    let mut kw_pw = HashMap::new();
    kw_pw.insert("password".into(), PyRep::String("host-pw".into()));
    let extract_ids = |rep: PyRep| -> Vec<i64> {
        let PyRep::ObjectEx { list, .. } = rep else {
            panic!("expected ObjectEx rowset, got {rep:?}");
        };
        list.iter().filter_map(|r| dbrow_col_i64(r, "itemID")).collect()
    };

    assert!(!ctx.db.with_snapshot(|snap| is_container_unlocked_snap(snap, owner_ship, mate_id)));
    let ids = extract_ids(
        handle_list(&ctx, &mate_sess, Some(&bound_nest), &[], &kw_pw)
            .expect("nested container List with valid host ship password must succeed"),
    );
    assert_eq!(ids, vec![nest_item]);
    assert!(
        ctx.db.with_snapshot(|snap| is_container_unlocked_snap(snap, owner_ship, mate_id)),
        "nested container List must persist unlock on foreign host ship"
    );

    // Concurrent ACL revocation / missing ship in snapshot must fail closed in both helpers
    ctx.db.with_mut_snapshot(|snap| {
        set_ship_access_config_snap(
            snap,
            owner_ship,
            &ShipAccessConfig {
                allow_fleet_smb: false,
                allow_corp_smb: false,
                allow_fleet_hangar: false,
                allow_corp_hangar: false,
                password: None,
            },
            0,
        )
        .unwrap();
    });
    assert_eq!(
        handle_list(&ctx, &mate_sess, Some(&bound_nest), &[], &kw_pw).unwrap_err().code,
        "SMAAccessDenied"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            verify_nested_unit_list_in_snap(&ctx, snap, &mate_sess, mate_id, nest_box, Some("host-pw"))
                .unwrap_err()
                .code,
            "SMAAccessDenied"
        );
        assert_eq!(
            super::verify_and_collect_foreign_ship_rows(
                &ctx, snap, &mate_sess, mate_id, owner_ship, true, false, None, Some("host-pw"),
            )
            .unwrap_err()
            .code,
            "SMAAccessDenied"
        );
        assert_eq!(
            super::verify_and_collect_foreign_ship_rows(
                &ctx, snap, &mate_sess, mate_id, 1_880_999_999, true, false, Some(&[90]), None,
            )
            .unwrap_err()
            .code,
            "SMAAccessDenied"
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1180_multi_add_and_move_enforce_foreign_corp_and_password_per_action() {
    use crate::services::inventory_fitting::fitting_containers_ships::{
        validate_container_and_bay_move, ContainerConfig,
    };

    let ctx = test_ctx();
    let owner_id = 92_881_001_i64;
    let stranger_id = 92_881_002_i64;
    let corp_a = 98_881_001_i64;
    let corp_b = 98_881_002_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let owner_ship = 1_881_100_001_i64;
    let stranger_ship = 1_881_100_002_i64;
    let owner_alsc = 1_881_100_010_i64;
    let alsc_item = 1_881_100_020_i64;
    let foreign_box = 1_881_100_030_i64;
    let foreign_item = 1_881_100_040_i64;
    let foreign_jetcan = 1_881_100_050_i64;
    let jetcan_loot = 1_881_100_060_i64;

    ensure_char(&ctx, owner_id, corp_a, Some(station_id), sys_id, owner_ship);
    ensure_char(&ctx, stranger_id, corp_b, Some(station_id), sys_id, stranger_ship);
    insert_item(&ctx, owner_ship, 587, owner_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, stranger_ship, 587, stranger_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, owner_alsc, 17_366, owner_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, alsc_item, 34, owner_id, owner_alsc, InventoryFlag::Cargo, 20, false);
    insert_item(&ctx, foreign_box, 17_366, corp_a, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, foreign_item, 34, stranger_id, foreign_box, InventoryFlag::Cargo, 15, false);
    insert_item(&ctx, foreign_jetcan, 23, owner_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, jetcan_loot, 34, owner_id, foreign_jetcan, InventoryFlag::Cargo, 10, false);

    ctx.db.with_mut_snapshot(|snap| {
        let mut c_rec = eve_db::CorporationRecord::default();
        c_rec.corporation_id = corp_a;
        c_rec.name = "Foreign Container Corp".into();
        c_rec.ticker = "FCC".into();
        c_rec.ceo_id = owner_id;
        c_rec.members.insert(owner_id);
        snap.corporations.insert(corp_a, c_rec);
        snap.item_positions.insert(stranger_ship, [0.0, 0.0, 0.0]);
        snap.item_positions.insert(foreign_jetcan, [200.0, 0.0, 0.0]);
        set_container_password_snap(snap, owner_alsc, 1, "peract", 0).unwrap();
        set_container_config_snap(
            snap,
            owner_alsc,
            &ContainerConfig {
                lock_by_default: false,
                password_per_action: true,
                config_bitmask: 0,
            },
            0,
        )
        .unwrap();
        // Pre-seed cached unlock; password_per_action must still demand password on takes
        set_container_unlocked_snap(snap, owner_alsc, owner_id, true, 0).unwrap();
    });

    let mut owner_sess = make_session(owner_id, corp_a, Some(station_id), Some(sys_id), Some(owner_ship));
    let mut stranger_docked = make_session(stranger_id, corp_b, Some(station_id), Some(sys_id), Some(stranger_ship));
    let mut stranger_space = make_session(stranger_id, corp_b, None, Some(sys_id), Some(stranger_ship));

    let kw_empty = HashMap::new();
    let mut kw_pw = HashMap::new();
    kw_pw.insert("password".into(), PyRep::String("peract".into()));

    // password_per_action rejects MultiAdd and validate_container_and_bay_move without password even when unlocked
    let station_bound_owner = make_bound(station_id, owner_id);
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut owner_sess,
            Some(&station_bound_owner),
            &[
                PyRep::List(vec![PyRep::Integer(alsc_item)]),
                PyRep::Integer(owner_alsc),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ],
            &kw_empty,
        )
        .await
        .unwrap_err()
        .code,
        "ALSCPasswordNeededToOpen"
    );
    assert_eq!(
        validate_container_and_bay_move(
            &ctx, owner_id, alsc_item, station_id, InventoryFlag::Hangar as u16, &kw_empty, None,
        )
        .unwrap_err()
        .code,
        "ALSCPasswordNeededToOpen"
    );
    handle_multi_add(
        &ctx,
        &mut owner_sess,
        Some(&station_bound_owner),
        &[
            PyRep::List(vec![PyRep::Integer(alsc_item)]),
            PyRep::Integer(owner_alsc),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        &kw_pw,
    )
    .await
    .expect("MultiAdd with password_per_action secret succeeds");

    // Foreign-corp container take rejects with CorpRoleDenied, while jetcan (type_id=23) remains lootable
    let station_bound_stranger = make_bound(station_id, stranger_id);
    assert_eq!(
        handle_multi_add(
            &ctx,
            &mut stranger_docked,
            Some(&station_bound_stranger),
            &[
                PyRep::List(vec![PyRep::Integer(foreign_item)]),
                PyRep::Integer(foreign_box),
                PyRep::Integer(InventoryFlag::Hangar as i64),
            ],
            &kw_empty,
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    let ship_bound_stranger = make_bound(stranger_ship, stranger_id);
    handle_multi_add(
        &ctx,
        &mut stranger_space,
        Some(&ship_bound_stranger),
        &[
            PyRep::List(vec![PyRep::Integer(jetcan_loot)]),
            PyRep::Integer(foreign_jetcan),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kw_empty,
    )
    .await
    .expect("jetcan take by foreign corp remains allowed");
}

#[test]
fn bf_1180_undocked_caller_cannot_list_docked_foreign_ship_or_nested_bay() {
    use crate::services::inventory_fitting::fitting_containers_ships::{
        verify_nested_unit_list_in_snap, ShipAccessConfig,
    };

    let ctx = test_ctx();
    let owner_id = 92_880_011_i64;
    let mate_id = 92_880_012_i64;
    let corp_id = 98_880_011_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let owner_ship = 1_880_200_001_i64;
    let mate_ship = 1_880_200_002_i64;
    let sma_ship = 1_880_200_010_i64;
    let fh_ore = 1_880_200_011_i64;
    let nest_box = 1_880_200_012_i64;
    let nest_ore = 1_880_200_013_i64;

    ensure_char(&ctx, owner_id, corp_id, Some(station_id), sys_id, owner_ship);
    ensure_char(&ctx, mate_id, corp_id, None, sys_id, mate_ship);
    insert_item(&ctx, owner_ship, 587, owner_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, mate_ship, 587, mate_id, i64::from(sys_id), InventoryFlag::None, 1, true);
    insert_item(&ctx, sma_ship, 587, owner_id, owner_ship, InventoryFlag::ShipHangar, 1, true);
    insert_item(&ctx, fh_ore, 34, owner_id, owner_ship, InventoryFlag::FleetHangar, 50, false);
    // Nested container owned by mate_id inside docked owner_ship FleetHangar.
    insert_item(&ctx, nest_box, 17_366, mate_id, owner_ship, InventoryFlag::FleetHangar, 1, true);
    insert_item(&ctx, nest_ore, 34, mate_id, nest_box, InventoryFlag::Cargo, 25, false);

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            777_880,
            FleetRecord {
                fleet_id: 777_880,
                boss_id: owner_id,
                members: vec![owner_id, mate_id],
                ..Default::default()
            },
        );
        set_ship_access_config_snap(
            snap,
            owner_ship,
            &ShipAccessConfig {
                allow_fleet_smb: true,
                allow_corp_smb: true,
                allow_fleet_hangar: true,
                allow_corp_hangar: true,
                password: None,
            },
            0,
        )
        .unwrap();
        snap.item_positions.insert(mate_ship, [0.0, 0.0, 0.0]);
    });

    let mate_sess = make_session(mate_id, corp_id, None, Some(sys_id), Some(mate_ship));
    let bound_owner_ship = make_bound(owner_ship, mate_id);
    let bound_nest = make_bound(nest_box, mate_id);
    let kw = HashMap::new();
    let row_count = |rep: PyRep| -> usize {
        let PyRep::ObjectEx { list, .. } = rep else {
            panic!("expected ObjectEx rowset, got {rep:?}");
        };
        list.len()
    };

    // Undocked caller holding bound moniker for docked corp/fleet ship must get 0 rows.
    assert_eq!(
        row_count(handle_list(&ctx, &mate_sess, Some(&bound_owner_ship), &[], &kw).unwrap()),
        0,
        "undocked List on docked foreign ship must return 0 rows"
    );
    assert_eq!(
        row_count(
            super::handle_list_by_flags(
                &ctx,
                &mate_sess,
                Some(&bound_owner_ship),
                &[PyRep::List(vec![PyRep::Integer(90)])],
                &kw,
            )
            .unwrap()
        ),
        0,
        "undocked ListByFlags([90]) on docked foreign ship must return 0 rows"
    );
    assert_eq!(
        row_count(
            super::handle_list_by_flags(
                &ctx,
                &mate_sess,
                Some(&bound_owner_ship),
                &[PyRep::List(vec![PyRep::Integer(155)])],
                &kw,
            )
            .unwrap()
        ),
        0,
        "undocked ListByFlags([155]) on docked foreign ship must return 0 rows"
    );
    assert_eq!(
        row_count(handle_list(&ctx, &mate_sess, Some(&bound_nest), &[], &kw).unwrap()),
        0,
        "undocked List on nested container inside docked foreign ship must return 0 rows"
    );

    ctx.db.with_snapshot(|snap| {
        let (unlock_none, rows_none) = super::verify_and_collect_foreign_ship_rows(
            &ctx, snap, &mate_sess, mate_id, owner_ship, false, false, None, None,
        )
        .unwrap();
        assert!(unlock_none.is_none() && rows_none.is_empty());
        let (unlock_docked, rows_docked) = super::verify_and_collect_foreign_ship_rows(
            &ctx, snap, &mate_sess, mate_id, owner_ship, true, true, Some(&[90]), None,
        )
        .unwrap();
        assert!(unlock_docked.is_none() && rows_docked.is_empty());
        assert_eq!(
            verify_nested_unit_list_in_snap(&ctx, snap, &mate_sess, mate_id, nest_box, None)
                .unwrap_err()
                .code,
            "BoundObjectOwnershipViolation"
        );
    });

    // Move foreign ship into space >2,500m -> second-snapshot helpers must reject OutOfRange.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&owner_ship).unwrap().location_id = i64::from(sys_id);
        snap.items.get_mut(&owner_ship).unwrap().flag = InventoryFlag::None;
        snap.item_positions.insert(owner_ship, [10_000.0, 0.0, 0.0]);
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            super::verify_and_collect_foreign_ship_rows(
                &ctx, snap, &mate_sess, mate_id, owner_ship, true, true, Some(&[90]), None,
            )
            .unwrap_err()
            .code,
            "OutOfRange"
        );
        assert_eq!(
            verify_nested_unit_list_in_snap(&ctx, snap, &mate_sess, mate_id, nest_box, None)
                .unwrap_err()
                .code,
            "OutOfRange"
        );
    });
}
