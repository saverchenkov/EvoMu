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
