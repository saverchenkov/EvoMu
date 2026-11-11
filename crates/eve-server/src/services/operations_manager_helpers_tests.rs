use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;

#[test]
fn test_operations_manager_helpers_defs_rewards_and_server_events() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    session.role = u64::MAX;

    let defs = get_operation_defs(&ctx.sde);
    assert!(!defs.sde_categories.is_empty());
    assert!(!defs.sde_operations.is_empty());
    assert!(!defs.sde_tasks.is_empty());

    // Find an operation with tasks
    let (&op_id, tasks) = defs
        .op_tasks
        .iter()
        .find(|(_, ts)| !ts.is_empty())
        .expect("expected SDE operation with tasks");
    let first_tid = tasks[0];

    // Activate operation in character's store
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store_pub(snap);
        let rec = store.by_character.entry(char_id).or_default();
        rec.active_operation_id = Some(op_id);
        rec.active_operation_system_id = Some(sys_id);
        let rw = grant_operation_activation_rewards(
            snap,
            rec,
            defs,
            1,
            char_id,
            Some(60_003_760),
            op_id,
        );
        emit_operation_reward_notifications(&ctx, char_id, &rw);
        save_store_pub(snap, &store, 1_700_000_000);
    });

    // Test GM complete_task via handle_operation_task_client_event
    let res = handle_operation_task_client_event(
        &ctx,
        &session,
        1,
        char_id,
        "complete_task".into(),
        None,
        true,
    )
    .unwrap();
    assert_eq!(res, PyRep::Bool(true));

    // Verify task was marked completed
    let is_done = ctx.db.with_snapshot(|snap| {
        load_store_pub(snap)
            .by_character
            .get(&char_id)
            .is_some_and(|r| r.completed_tasks.contains(&first_tid))
    });
    assert!(is_done);

    // Test record_invbroker_add_operation_event for fitting module and loading charges
    record_invbroker_add_operation_event(
        &ctx,
        char_id,
        60_003_760,
        ship_id,
        eve_db::InventoryFlag::HiSlot0,
        34,
        1,
    );

    // Test advance_and_notify_server_operation_tasks
    advance_and_notify_server_operation_tasks(
        &ctx.db,
        &ctx.sde,
        &ctx.quasar_gateway,
        char_id,
        &["npcDestroyed"],
        &[],
        1,
        ctx.now_unix(),
    );
}

#[test]
fn test_operations_manager_helpers_private_parsers_conditions_and_edge_branches() {
    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};

    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();

    // 1. parse_op_item_rewards edge cases (missing key, typeID=0 skipped, amount=0 clamped to 1, raceID)
    let json_val = serde_json::json!({
        "rewards": [
            { "typeID": 0, "amount": 5 },
            { "typeID": 34, "amount": 0, "raceID": 1 },
            { "typeID": 35 }
        ]
    });
    assert!(parse_op_item_rewards(&json_val, "missing").is_empty());
    let parsed = parse_op_item_rewards(&json_val, "rewards");
    assert_eq!(parsed.len(), 2);
    assert_eq!(
        parsed[0],
        SdeOperationItemReward {
            type_id: 34,
            amount: 1,
            race_id: Some(1),
        }
    );
    assert_eq!(
        parsed[1],
        SdeOperationItemReward {
            type_id: 35,
            amount: 1,
            race_id: None,
        }
    );

    // 2. event_matches_task, pyrep_matches_allowed, extract_condition_pyrep, check_client_extra_conditions
    assert!(event_matches_task("openWindow", "client_OpenWindow"));
    assert!(event_matches_task("client_openWindow", "CLIENT_OPENWINDOW"));
    assert!(!event_matches_task("other", "client_OpenWindow"));

    let allowed = vec!["123".to_string(), "Caldari".to_string()];
    assert!(pyrep_matches_allowed(&PyRep::String("caldari".into()), &allowed));
    assert!(pyrep_matches_allowed(&PyRep::Integer(123), &allowed));
    assert!(pyrep_matches_allowed(
        &PyRep::Tuple(vec![PyRep::Integer(999), PyRep::Integer(123)]),
        &allowed
    ));
    assert!(!pyrep_matches_allowed(&PyRep::Real(123.0), &allowed));
    assert!(!pyrep_matches_allowed(&PyRep::None, &allowed));

    let dict_extra = PyRep::Dict(vec![
        (PyRep::String("RaceID".into()), PyRep::Integer(1)),
        (PyRep::Integer(77), PyRep::String("ok".into())),
    ]);
    assert_eq!(
        extract_condition_pyrep(Some(&dict_extra), "raceid", None),
        Some(&PyRep::Integer(1))
    );
    assert_eq!(
        extract_condition_pyrep(Some(&dict_extra), "missingKey", Some(77)),
        Some(&PyRep::String("ok".into()))
    );
    let kv_extra = PyRep::keyval([("faction", PyRep::String("Caldari".into()))]);
    assert_eq!(
        extract_condition_pyrep(Some(&kv_extra), "faction", None),
        Some(&PyRep::String("Caldari".into()))
    );
    assert!(extract_condition_pyrep(Some(&PyRep::Integer(1)), "faction", None).is_none());

    let mut defs = OperationDefinitions::default();
    let op_id = 888_i64;
    let t1 = 8801_i64;
    let t2 = 8802_i64;
    defs.sde_operations.insert(op_id);
    defs.op_tasks.insert(op_id, vec![t1, t2]);
    defs.task_target_values.insert(t1, 3);
    defs.task_target_values.insert(t2, 1);
    defs.task_is_optional.insert(t2);
    defs.task_is_client_skippable.insert(t2);
    defs.task_event_conditions.insert(
        t1,
        HashMap::from([
            ("faction".into(), vec!["Caldari".into()]),
            ("emptyRule".into(), vec![]),
        ]),
    );
    assert!(check_client_extra_conditions(&defs, &ctx.sde, t2, None, true));
    assert!(!check_client_extra_conditions(&defs, &ctx.sde, t1, None, true));
    assert!(check_client_extra_conditions(&defs, &ctx.sde, t1, None, false));
    assert!(check_client_extra_conditions(
        &defs,
        &ctx.sde,
        t1,
        Some(&kv_extra),
        true
    ));
    let bad_kv = PyRep::keyval([("faction", PyRep::String("Gallente".into()))]);
    assert!(!check_client_extra_conditions(
        &defs,
        &ctx.sde,
        t1,
        Some(&bad_kv),
        true
    ));

    // 3. deliver_op_item & credit_op_isk edge cases
    ctx.db.with_mut_snapshot(|snap| {
        let mut out = OperationRewardOutcome::default();
        deliver_op_item(snap, &mut out, 1, char_id, Some(60_003_760), 0, 5, "zero type");
        deliver_op_item(snap, &mut out, 1, char_id, Some(60_003_760), 34, 0, "zero qty");
        assert!(out.created_hangar_items.is_empty());
        assert!(!out.enqueued_redeem_tokens);

        deliver_op_item(snap, &mut out, 1, char_id, Some(60_003_760), 34, 5, "stack");
        assert_eq!(out.created_hangar_items.len(), 1);
        let iid = out.created_hangar_items[0].0;
        assert!(!snap.items[&iid].singleton);
        assert_eq!(snap.items[&iid].quantity, 5);

        assert!(credit_op_isk(snap, char_id, 0, 100, "op", "zero".into()).is_none());
        assert!(credit_op_isk(snap, 999_999_999, 100, 100, "op", "nochar".into()).is_none());
        let before_bal = snap.characters[&char_id].balance;
        let new_bal = credit_op_isk(snap, char_id, 500, 100, "op", "ok".into()).unwrap();
        assert_eq!(new_bal, before_bal.0 + 50_000);
    });

    // 4. advance_active_task_in_record partial progress vs completion with client_skippable task
    let mut rec = CharacterOperationsRecord::default();
    ctx.db.with_mut_snapshot(|snap| {
        let (tr1, rw1) = advance_active_task_in_record(
            snap, &mut rec, &defs, 1, char_id, None, 100, op_id, t1, 2, true,
        );
        assert!(tr1.is_none());
        assert_eq!(rec.task_progress[&t1], 2);
        assert!(rec.server_verified_tasks.contains(&t1));
        assert!(!rec.completed_tasks.contains(&t1));
        assert!(rw1.created_hangar_items.is_empty());

        // Over-progress clamps to target (3) and completes op since t2 is client_skippable
        let (tr2, _) = advance_active_task_in_record(
            snap, &mut rec, &defs, 1, char_id, None, 100, op_id, t1, 99, false,
        );
        assert_eq!(tr2, Some((31, op_id, t1)));
        assert_eq!(rec.task_progress[&t1], 3);
        assert!(rec.completed_tasks.contains(&t1));
        assert!(rec.completed_operations.contains(&op_id));
    });

    // 5. handle_operation_task_client_event: wrong solar_system_id & non-GM complete_task on non-client task
    let real_defs = get_operation_defs(&ctx.sde);
    if let Some((&srv_tid, _)) = real_defs
        .task_event_names
        .iter()
        .find(|(&tid, ev)| !ev.starts_with("client_") && real_defs.task_to_op.contains_key(&tid))
    {
        let srv_op = real_defs.task_to_op[&srv_tid];
        let tasks = &real_defs.op_tasks[&srv_op];
        ctx.db.with_mut_snapshot(|snap| {
            let mut store = load_store_pub(snap);
            let r = store.by_character.entry(char_id).or_default();
            r.active_operation_id = Some(srv_op);
            r.active_operation_system_id = Some(sys_id + 1); // mismatched system
            r.completed_tasks.clear();
            for &t in tasks {
                if t == srv_tid {
                    break;
                }
                r.completed_tasks.insert(t);
            }
            save_store_pub(snap, &store, 100);
        });
        session.role = 0;
        assert_eq!(
            handle_operation_task_client_event(
                &ctx,
                &session,
                1,
                char_id,
                "complete_task".into(),
                None,
                true,
            )
            .unwrap(),
            PyRep::Bool(false)
        );
        // Fix system, still non-GM -> cannot force-complete a server task
        ctx.db.with_mut_snapshot(|snap| {
            let mut store = load_store_pub(snap);
            store
                .by_character
                .get_mut(&char_id)
                .unwrap()
                .active_operation_system_id = Some(sys_id);
            save_store_pub(snap, &store, 100);
        });
        assert_eq!(
            handle_operation_task_client_event(
                &ctx,
                &session,
                1,
                char_id,
                "complete_task".into(),
                None,
                true,
            )
            .unwrap(),
            PyRep::Bool(false)
        );
    }

    // 6. record_invbroker_add_operation_event non-ship dest & non-wreck source do not panic
    let station_loc = 60_003_760_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            777_001,
            InventoryItemRecord {
                item_id: 777_001,
                type_id: 34,
                owner_id: char_id,
                location_id: station_loc,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::ZERO;
        }
    });
    record_invbroker_add_operation_event(
        &ctx,
        char_id,
        777_001,
        station_loc, // not a ship
        InventoryFlag::HiSlot0,
        34,
        1,
    );
    record_invbroker_add_operation_event(
        &ctx,
        char_id,
        777_001, // not in space & not wreck/container
        ship_id,
        InventoryFlag::Cargo,
        34,
        1,
    );

    // 7. advance_and_notify_server_operation_tasks with macho_notify_sink == None
    *ctx.quasar_gateway.macho_notify_sink.write().unwrap() = None;
    advance_and_notify_server_operation_tasks(
        &ctx.db,
        &ctx.sde,
        &ctx.quasar_gateway,
        char_id,
        &["npcDestroyed"],
        &[],
        u64::MAX,
        ctx.now_unix(),
    );
}
