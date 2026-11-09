#![forbid(unsafe_code)]

use super::*;
use crate::router::{context::BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::{self, SafetyLevel};
use crate::services::inventory_fitting::fitting_containers_ships::{
    read_container_ship_state, save_container_ship_state,
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
    {
        let mut st = read_container_ship_state(&ctx);
        st.alsc_locked_items.insert(locked_stack);
        save_container_ship_state(&ctx, &st);
    }
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
    {
        let mut st = read_container_ship_state(&ctx);
        st.alsc_locked_items.remove(&locked_stack);
        save_container_ship_state(&ctx, &st);
    }

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

    {
        let mut st = read_container_ship_state(&ctx);
        st.container_passwords.insert(alsc_id, "secret123".into());
        save_container_ship_state(&ctx, &st);
    }

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
    let st_after = read_container_ship_state(&ctx);
    assert!(st_after.unlocked_containers.contains(&(alsc_id, char_id)));
    assert_eq!(st_after.alsc_logs.get(&alsc_id).map(|v| v.len()), Some(1));

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
