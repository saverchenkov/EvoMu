//! Extra branch-coverage and mutation-killing unit tests for `operations_manager_helpers.rs`.

#![forbid(unsafe_code)]

use crate::router::NotificationTarget;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use crate::services::operations_manager::{
    load_store_pub, save_store_pub, CharacterOperationsRecord,
};
use crate::services::operations_manager_helpers::{
    advance_and_notify_server_operation_tasks, emit_operation_reward_notifications_with_sink,
    get_operation_defs, grant_category_completion_rewards, grant_operation_activation_rewards,
    grant_task_and_operation_rewards, handle_operation_task_client_event,
    record_invbroker_add_operation_event, record_server_operation_event_in_db,
    OperationDefinitions, SdeOperationItemReward,
};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_proto::PyRep;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

#[test]
fn operation_definitions_dungeon_resolution_and_prerequisite_trees() {
    let (ctx, _sess, _cid, _sid, _sys) = make_fighter_ctx_and_session();
    let (&any_dungeon_id, _) = ctx
        .sde
        .dungeons
        .iter()
        .next()
        .expect("SDE has at least one dungeon");
    let (&second_dungeon_id, _) = ctx
        .sde
        .dungeons
        .iter()
        .nth(1)
        .unwrap_or((&any_dungeon_id, &ctx.sde.dungeons[&any_dungeon_id]));

    let mut defs = OperationDefinitions::default();
    // [amarr, caldari, gallente, minmatar]
    defs.site_race_dungeons.insert(
        500,
        [any_dungeon_id, second_dungeon_id, any_dungeon_id, second_dungeon_id],
    );
    defs.site_race_dungeons
        .insert(501, [0, 0, any_dungeon_id, 0]);
    defs.site_race_dungeons
        .insert(99_999_502, [999_999_999, 0, 0, 0]);

    // Race-specific lookup: 1=caldari, 2=minmatar, 4=amarr, 8=gallente
    assert_eq!(
        defs.resolve_site_dungeon_id(&ctx.sde, 500, 1),
        Some(second_dungeon_id)
    );
    assert_eq!(
        defs.resolve_site_dungeon_id(&ctx.sde, 500, 2),
        Some(second_dungeon_id)
    );
    assert_eq!(
        defs.resolve_site_dungeon_id(&ctx.sde, 500, 4),
        Some(any_dungeon_id)
    );
    assert_eq!(
        defs.resolve_site_dungeon_id(&ctx.sde, 500, 8),
        Some(any_dungeon_id)
    );
    // Unknown race falls back to first non-zero entry
    assert_eq!(
        defs.resolve_site_dungeon_id(&ctx.sde, 501, 99),
        Some(any_dungeon_id)
    );
    // Invalid dungeon in site_race_dungeons falls back to site_id if site_id is in sde.dungeons
    assert_eq!(defs.resolve_site_dungeon_id(&ctx.sde, 99_999_502, 4), None);
    assert_eq!(
        defs.resolve_site_dungeon_id(&ctx.sde, i64::from(any_dungeon_id), 1),
        Some(any_dungeon_id)
    );
    assert_eq!(defs.resolve_site_dungeon_id(&ctx.sde, -5, 1), None);

    // Prerequisites (ALL required) + Tree parents (ANY required)
    defs.op_prerequisites.insert(10, vec![1, 2]);
    defs.op_tree_parents.insert(10, vec![3, 4]);
    defs.op_tree_parents.insert(11, Vec::new());

    let mut completed = HashSet::new();
    assert!(defs.are_prerequisites_met(99, &completed));
    assert!(defs.are_prerequisites_met(11, &completed));
    assert!(!defs.are_prerequisites_met(10, &completed));

    completed.insert(1);
    assert!(!defs.are_prerequisites_met(10, &completed));
    completed.insert(2);
    // Prereqs 1 & 2 met, but neither tree parent 3 nor 4 is completed
    assert!(!defs.are_prerequisites_met(10, &completed));
    completed.insert(4);
    assert!(defs.are_prerequisites_met(10, &completed));
}

#[test]
fn activation_task_operation_and_category_reward_payout_and_notifications() {
    let (ctx, _sess, char_id, _sid, _sys) = make_fighter_ctx_and_session();
    let mut defs = OperationDefinitions::default();
    let op_id = 701_i64;
    let t_req = 7001_i64;
    let t_opt = 7002_i64;
    let cat_id = 31_i64;

    defs.sde_operations.insert(op_id);
    defs.op_tasks.insert(op_id, vec![t_req, t_opt]);
    defs.task_is_optional.insert(t_opt);
    defs.op_activation_rewards.insert(
        op_id,
        vec![
            SdeOperationItemReward {
                type_id: 34,
                amount: 10,
                race_id: None,
            },
            SdeOperationItemReward {
                type_id: 35,
                amount: 5,
                race_id: Some(999), // mismatched race -> skipped
            },
        ],
    );
    defs.task_completion_rewards.insert(t_req, vec![(36, 1)]);
    defs.op_isk_rewards.insert(op_id, 250_000);
    defs.op_completion_rewards.insert(
        op_id,
        vec![SdeOperationItemReward {
            type_id: 37,
            amount: 2,
            race_id: Some(0),
        }],
    );
    defs.op_skill_rewards.insert(op_id, vec![3300]);
    defs.cat_isk_rewards.insert(cat_id, 500_000);

    let mut rec = CharacterOperationsRecord::default();

    // 1. Activation rewards while undocked (sess_docked = None, char in space) -> enqueues redeem tokens
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
        }
        let out_undocked =
            grant_operation_activation_rewards(snap, &mut rec, &defs, 1, char_id, None, op_id);
        assert!(out_undocked.enqueued_redeem_tokens);
        assert!(out_undocked.created_hangar_items.is_empty());

        // Second call is idempotent
        let out_dup =
            grant_operation_activation_rewards(snap, &mut rec, &defs, 1, char_id, None, op_id);
        assert!(!out_dup.enqueued_redeem_tokens);
    });

    // 2. Task & operation rewards without server_verified_tasks -> blocked from payout!
    rec.completed_tasks.insert(t_req);
    ctx.db.with_mut_snapshot(|snap| {
        let out_unverified = grant_task_and_operation_rewards(
            snap,
            &mut rec,
            &defs,
            1,
            char_id,
            Some(60_003_760),
            1_700_000_000,
            op_id,
            Some(t_req),
            true,
        );
        assert!(out_unverified.new_balance_cents.is_none());
        assert!(out_unverified.created_hangar_items.is_empty());
    });

    // 3. Mark t_req as server-verified -> grants task reward, operation ISK, completion item, and skill book in hangar
    rec.server_verified_tasks.insert(t_req);
    let (out_verified, out_cat) = ctx.db.with_mut_snapshot(|snap| {
        let ov = grant_task_and_operation_rewards(
            snap,
            &mut rec,
            &defs,
            1,
            char_id,
            Some(60_003_760),
            1_700_000_000,
            op_id,
            Some(t_req),
            true,
        );
        // Duplicate call is idempotent
        let ov_dup = grant_task_and_operation_rewards(
            snap,
            &mut rec,
            &defs,
            1,
            char_id,
            Some(60_003_760),
            1_700_000_000,
            op_id,
            Some(t_req),
            true,
        );
        assert!(ov_dup.new_balance_cents.is_none());
        assert!(ov_dup.created_hangar_items.is_empty());

        let oc = grant_category_completion_rewards(
            snap,
            &mut rec,
            &defs,
            char_id,
            1_700_000_000,
            cat_id,
        );
        let oc_dup = grant_category_completion_rewards(
            snap,
            &mut rec,
            &defs,
            char_id,
            1_700_000_000,
            cat_id,
        );
        assert!(oc_dup.new_balance_cents.is_none());
        (ov, oc)
    });
    assert!(out_verified.new_balance_cents.is_some());
    assert_eq!(out_verified.created_hangar_items.len(), 3); // task item + op item + skill
    assert!(out_cat.new_balance_cents.is_some());

    // 4. Verify notification sink emits OnAccountChange, OnItemChange, and OnRedeemingTokensUpdated
    let mut combined = out_verified;
    combined.enqueued_redeem_tokens = true;
    let emitted = Arc::new(Mutex::new(Vec::<String>::new()));
    let emitted_clone = Arc::clone(&emitted);
    emit_operation_reward_notifications_with_sink(
        &ctx.sde,
        move |_target: NotificationTarget, event: &str, _id_type: &str, _args: Vec<PyRep>| {
            emitted_clone.lock().unwrap().push(event.to_string());
        },
        char_id,
        &combined,
    );
    let events = emitted.lock().unwrap().clone();
    assert!(events.contains(&"OnAccountChange".to_string()));
    assert!(events.contains(&"OnItemChange".to_string()));
    assert!(events.contains(&"OnRedeemingTokensUpdated".to_string()));
}

#[test]
fn client_and_server_task_events_invbroker_fitting_charges_and_space_wreck_looting() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    session.role = 0; // non-GM session
    let defs = get_operation_defs(&ctx.sde);

    // Find a client_* completable task in SDE
    let client_tid_opt = defs
        .client_completable_tasks
        .iter()
        .copied()
        .find(|tid| defs.task_to_op.contains_key(tid));
    if let Some(client_tid) = client_tid_opt {
        let op_id = defs.task_to_op[&client_tid];
        let all_tasks = defs.op_tasks[&op_id].clone();
        let ev_name = defs.task_event_names[&client_tid].clone();

        // Build extra_data satisfying any task_event_conditions on client_tid
        let mut dict_entries = Vec::new();
        if let Some(conds) = defs.task_event_conditions.get(&client_tid) {
            for (k, allowed) in conds {
                if let Some(first_val) = allowed.first() {
                    dict_entries.push((
                        PyRep::String(k.clone()),
                        PyRep::List(vec![PyRep::String(first_val.clone())]),
                    ));
                }
            }
        }
        let extra = PyRep::Dict(dict_entries);

        ctx.db.with_mut_snapshot(|snap| {
            let mut store = load_store_pub(snap);
            let rec = store.by_character.entry(char_id).or_default();
            rec.active_operation_id = Some(op_id);
            rec.active_operation_system_id = Some(sys_id);
            rec.completed_tasks.clear();
            for &t in &all_tasks {
                if t == client_tid {
                    break;
                }
                rec.completed_tasks.insert(t);
            }
            // Block the task first -> should return Bool(false) when has_call_args=true, None when false
            rec.blocked_tasks.insert(client_tid);
            save_store_pub(snap, &store, 1_700_000_000);
        });

        assert_eq!(
            handle_operation_task_client_event(
                &ctx,
                &session,
                1,
                char_id,
                ev_name.clone(),
                Some(&extra),
                true
            )
            .unwrap(),
            PyRep::Bool(false)
        );
        assert_eq!(
            handle_operation_task_client_event(
                &ctx,
                &session,
                1,
                char_id,
                ev_name.clone(),
                Some(&extra),
                false
            )
            .unwrap(),
            PyRep::None
        );

        // Unblock and trigger using stripped suffix name (without "client_" prefix)
        ctx.db.with_mut_snapshot(|snap| {
            let mut store = load_store_pub(snap);
            store
                .by_character
                .get_mut(&char_id)
                .unwrap()
                .blocked_tasks
                .remove(&client_tid);
            save_store_pub(snap, &store, 1_700_000_000);
        });
        let stripped = ev_name.strip_prefix("client_").unwrap_or(&ev_name).to_string();
        let res = handle_operation_task_client_event(
            &ctx,
            &session,
            1,
            char_id,
            stripped,
            Some(&extra),
            true,
        )
        .unwrap();
        assert_eq!(res, PyRep::Bool(true));
    }

    // Test record_server_operation_event_in_db guard rails (char_id <= 0, increment == 0, unknown char)
    assert_eq!(
        record_server_operation_event_in_db(&ctx.db, &ctx.sde, 0, "ev", &[], 1, 100).0,
        None
    );
    assert_eq!(
        record_server_operation_event_in_db(&ctx.db, &ctx.sde, char_id, "ev", &[], 0, 100).0,
        None
    );
    assert_eq!(
        record_server_operation_event_in_db(&ctx.db, &ctx.sde, 999_999_999, "ev", &[], 1, 100).0,
        None
    );

    // Find a server task with event name and test advance_and_notify_server_operation_tasks with sink
    let server_tid_opt = defs
        .task_event_names
        .iter()
        .find(|(tid, ev)| !ev.starts_with("client_") && defs.task_to_op.contains_key(tid))
        .map(|(&tid, ev)| (tid, ev.clone()));
    if let Some((srv_tid, srv_ev)) = server_tid_opt {
        let op_id = defs.task_to_op[&srv_tid];
        let all_tasks = defs.op_tasks[&op_id].clone();
        let mut owned_params: Vec<(String, String)> = Vec::new();
        if let Some(conds) = defs.task_event_conditions.get(&srv_tid) {
            for (ck, allowed) in conds {
                if let Some(first_val) = allowed.first() {
                    owned_params.push((ck.clone(), first_val.clone()));
                }
            }
        }
        let param_refs: Vec<(&str, String)> = owned_params
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();

        ctx.db.with_mut_snapshot(|snap| {
            let mut store = load_store_pub(snap);
            let rec = store.by_character.entry(char_id).or_default();
            rec.active_operation_id = Some(op_id);
            rec.active_operation_system_id = Some(sys_id);
            rec.completed_tasks.clear();
            rec.task_progress.clear();
            for &t in &all_tasks {
                if t == srv_tid {
                    break;
                }
                rec.completed_tasks.insert(t);
            }
            save_store_pub(snap, &store, 1_700_000_000);
        });

        let notified = Arc::new(Mutex::new(Vec::<String>::new()));
        let notified_clone = Arc::clone(&notified);
        *ctx.quasar_gateway.macho_notify_sink.write().unwrap() = Some(Arc::new(
            move |_target: NotificationTarget, event: &str, _id_type: &str, _args: Vec<PyRep>| {
                notified_clone.lock().unwrap().push(event.to_string());
            },
        ));

        advance_and_notify_server_operation_tasks(
            &ctx.db,
            &ctx.sde,
            &ctx.quasar_gateway,
            char_id,
            &[srv_ev.as_str()],
            &param_refs,
            100,
            1_700_000_000,
        );
        assert!(notified
            .lock()
            .unwrap()
            .contains(&"OnOperationTaskCompleted".to_string()));
    }

    // Test record_invbroker_add_operation_event:
    // 1. Charge (category 8) fitted to ship -> loadChargesCount
    // 2. Item looted from a wreck/cargo container (group 186) in space into ship Cargo -> lootItemCount
    let charge_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 8)
        .map(|t| t.type_id)
        .unwrap_or(182);
    let wreck_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 186)
        .map(|t| t.type_id)
        .unwrap_or(26_561);
    let wreck_item_id = 1_888_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_item_id,
            InventoryItemRecord {
                item_id: wreck_item_id,
                type_id: wreck_tid,
                owner_id: 1,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
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
        60_003_760,
        ship_id,
        InventoryFlag::HiSlot0,
        charge_tid,
        40,
    );
    record_invbroker_add_operation_event(
        &ctx,
        char_id,
        wreck_item_id,
        ship_id,
        InventoryFlag::Cargo,
        34,
        10,
    );
}
