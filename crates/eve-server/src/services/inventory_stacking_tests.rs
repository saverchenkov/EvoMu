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
