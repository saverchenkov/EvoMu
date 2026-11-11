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

    // Find a deterministic server task with event name and test advance_and_notify_server_operation_tasks with sink
    let server_tid_opt = defs
        .task_event_names
        .iter()
        .filter(|(&tid, ev)| {
            !ev.starts_with("client_")
                && defs
                    .task_to_op
                    .get(&tid)
                    .is_some_and(|op_id| defs.sde_operations.contains(op_id))
                && defs.task_target_values.get(&tid).copied().unwrap_or(1) <= 100
        })
        .min_by_key(|(&tid, _)| tid)
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

mod w4_ops_pve_mut_extra_tests {
    use crate::router::{NotificationTarget, ServerContext};
    use crate::services::operations_manager::{
        load_store_pub, save_store_pub, CharacterOperationsRecord,
    };
    use crate::services::operations_manager_helpers::*;
    use eve_db::clock::VirtualServerClock;
    use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_sde::SdeRegistry;
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex, OnceLock};

    fn shared_sde() -> Arc<SdeRegistry> {
        static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
        Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
    }

    fn make_ctx() -> ServerContext {
        let db = Arc::new(WorldRepository::new_empty());
        db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
        ServerContext::with_sde_and_repository(shared_sde(), db)
    }

    fn provision_char(
        ctx: &ServerContext,
        char_id: i64,
        race_id: u32,
        sys_id: u32,
        station_id: Option<i64>,
        structure_id: Option<i64>,
    ) -> u64 {
        let ship_id = char_id * 10 + 1;
        ctx.db.provision_gm_account(&format!("op_user_{char_id}"), "pw", char_id, "OpPilot");
        let uid = ctx
            .db
            .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id).unwrap_or(1));
        let now = ctx.now_unix();
        let _ = ctx.db.acquire_online_lock_with_computer_hash(
            uid,
            char_id as u64,
            Some(char_id),
            None,
            true,
            now,
        );
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.race_id = race_id;
                ch.solar_system_id = sys_id;
                ch.station_id = station_id;
                ch.structure_id = structure_id;
                ch.active_ship_id = ship_id;
                ch.balance = eve_db::IskCents(100_000);
            }
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 587,
                    owner_id: char_id,
                    location_id: station_id.or(structure_id).unwrap_or(i64::from(sys_id)),
                    flag: if station_id.is_some() || structure_id.is_some() {
                        InventoryFlag::Hangar
                    } else {
                        InventoryFlag::None
                    },
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        uid
    }

    #[test]
    fn definitions_dungeon_resolution_prereq_cycles_and_load_edges() {
        let sde = shared_sde();
        let mut defs = OperationDefinitions::default();
        let mut d_ids: Vec<u32> = sde.dungeons.keys().copied().collect();
        d_ids.sort_unstable();
        let (d1, d2, d3, d4) = if d_ids.len() >= 4 {
            (d_ids[0], d_ids[1], d_ids[2], d_ids[3])
        } else {
            (1420, 1420, 1420, 1420)
        };
        defs.site_race_dungeons.insert(100, [d1, d2, d3, d4]);
        let any_real_dungeon = d1;
        defs.site_race_dungeons.insert(101, [0, d2, 0, 0]);
        defs.site_race_dungeons
            .insert(i64::from(any_real_dungeon), [0, 0, 0, 0]);

        // Race mapping: 4(Amarr)->idx0, 1(Caldari)->idx1, 8(Gallente)->idx2, 2(Minmatar)->idx3, other->idx0
        assert_eq!(defs.resolve_site_dungeon_id(&sde, 100, 4), Some(d1));
        assert_eq!(defs.resolve_site_dungeon_id(&sde, 100, 1), Some(d2));
        assert_eq!(defs.resolve_site_dungeon_id(&sde, 100, 8), Some(d3));
        assert_eq!(defs.resolve_site_dungeon_id(&sde, 100, 2), Some(d4));
        assert_eq!(defs.resolve_site_dungeon_id(&sde, 100, 16), Some(d2));
        assert_eq!(defs.resolve_site_dungeon_id(&sde, 100, 0), Some(d2));

        // Slot 0 with non-SDE site_id returns d2 for race 1 (slot 1) and falls back to d2 for slot 3 = 0
        assert_eq!(defs.resolve_site_dungeon_id(&sde, 101, 1), Some(d2));
        assert_eq!(defs.resolve_site_dungeon_id(&sde, 101, 2), Some(d2));
        // Slot 0 whose site_id IS in sde.dungeons falls back to site_id as u32
        assert_eq!(
            defs.resolve_site_dungeon_id(&sde, i64::from(any_real_dungeon), 1),
            Some(any_real_dungeon)
        );
        assert_eq!(defs.resolve_site_dungeon_id(&sde, 0, 1), None);
        assert_eq!(defs.resolve_site_dungeon_id(&sde, -5, 1), None);
        assert_eq!(
            defs.resolve_site_dungeon_id(&sde, i64::from(any_real_dungeon) + 90_000_000, 1),
            None
        );

        // Prerequisite chains and cycles
        defs.op_prerequisites.insert(2, vec![1]);
        defs.op_tree_parents.insert(3, vec![3]); // self cycle
        defs.op_tree_parents.insert(4, vec![5]);
        defs.op_tree_parents.insert(5, vec![6]);
        defs.op_tree_parents.insert(7, vec![8]);
        defs.op_tree_parents.insert(8, vec![7]); // mutual cycle

        let mut done = HashSet::new();
        assert!(defs.are_prerequisites_met(1, &done));
        assert!(!defs.are_prerequisites_met(2, &done));
        assert!(!defs.are_prerequisites_met(3, &done));
        assert!(!defs.are_prerequisites_met(4, &done));
        assert!(!defs.are_prerequisites_met(7, &done));

        done.insert(1);
        assert!(defs.are_prerequisites_met(2, &done));
        done.insert(6);
        assert!(defs.are_prerequisites_met(5, &done));
        assert!(!defs.are_prerequisites_met(4, &done));
        done.insert(5);
        assert!(defs.are_prerequisites_met(4, &done));
        done.insert(7);
        done.insert(8);
        assert!(defs.are_prerequisites_met(7, &done));

        // get_operation_defs / load_operation_definitions sanity
        let loaded = load_operation_definitions(&sde);
        let cached = get_operation_defs(&sde);
        assert_eq!(loaded.sde_operations.len(), cached.sde_operations.len());
    }

    #[test]
    fn reward_payouts_race_filters_locations_verification_and_notifications() {
        let ctx = make_ctx();
        let now = ctx.now_unix();
        let char_docked_station = 94_100_001;
        let char_docked_struct = 94_100_002;
        let char_in_space = 94_100_003;
        let uid1 = provision_char(&ctx, char_docked_station, 4, 30_000_142, Some(60_003_760), None);
        let uid2 = provision_char(&ctx, char_docked_struct, 1, 30_000_142, None, Some(1_040_800_001));
        let _uid3 = provision_char(&ctx, char_in_space, 2, 30_000_142, None, None);

        let mut defs = OperationDefinitions::default();
        let op_id = 701;
        let cat_id = 31;
        let t_req = 7001;
        let t_opt = 7002;
        let t_skip = 7003;
        let t_cli_skip = 7004;
        defs.sde_categories.insert(cat_id);
        defs.category_isk_rewards.insert(cat_id, 25_000);
        defs.cat_isk_rewards.insert(cat_id, 25_000);
        defs.sde_operations.insert(op_id);
        defs.op_isk_rewards.insert(op_id, 50_000);
        defs.op_tasks
            .insert(op_id, vec![t_req, t_opt, t_skip, t_cli_skip]);
        defs.task_is_optional.insert(t_opt);
        defs.task_is_client_skippable.insert(t_skip);
        defs.task_is_client_skippable.insert(t_cli_skip);
        defs.task_completion_rewards.insert(t_req, vec![(35, 3), (0, 2), (36, 0)]);
        defs.op_activation_rewards.insert(
            op_id,
            vec![
                SdeOperationItemReward { type_id: 0, amount: 5, race_id: None },
                SdeOperationItemReward { type_id: 34, amount: 0, race_id: None },
                SdeOperationItemReward { type_id: 34, amount: 1, race_id: None },
                SdeOperationItemReward { type_id: 35, amount: 10, race_id: Some(4) },
                SdeOperationItemReward { type_id: 36, amount: 7, race_id: Some(0) },
                SdeOperationItemReward { type_id: 37, amount: 9, race_id: Some(8) },
            ],
        );
        defs.op_completion_rewards.insert(
            op_id,
            vec![
                SdeOperationItemReward { type_id: 38, amount: 1, race_id: Some(4) },
                SdeOperationItemReward { type_id: 39, amount: 5, race_id: Some(2) },
                SdeOperationItemReward { type_id: 0, amount: 2, race_id: None },
            ],
        );

        let captured: Arc<Mutex<Vec<(NotificationTarget, String, Vec<PyRep>)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let cap = Arc::clone(&captured);
        *ctx.quasar_gateway.macho_notify_sink.write().unwrap() = Some(Arc::new(
            move |target, method, _id_type, args| {
                cap.lock().unwrap().push((target, method.to_string(), args));
            },
        ));

        // 1. Unknown char uses sess_docked fallback and race=0 wildcard
        ctx.db.with_mut_snapshot(|snap| {
            let mut rec = CharacterOperationsRecord::default();
            let out_unknown = grant_operation_activation_rewards(
                snap,
                &mut rec,
                &defs,
                999_999,
                94_199_999,
                Some(60_003_760),
                op_id,
            );
            assert_eq!(out_unknown.created_hangar_items.len(), 2); // (34, 1, None) & (36, 7, Some(0))
            assert!(!out_unknown.enqueued_redeem_tokens);
        });

        // 2. Character docked at structure receives items into structure_id hangar; repeat call is idempotent
        let out_struct = ctx.db.with_mut_snapshot(|snap| {
            let mut store = load_store_pub(snap);
            let rec = store.by_character.entry(char_docked_struct).or_default();
            let first = grant_operation_activation_rewards(
                snap,
                rec,
                &defs,
                uid2,
                char_docked_struct,
                None,
                op_id,
            );
            let second = grant_operation_activation_rewards(
                snap,
                rec,
                &defs,
                uid2,
                char_docked_struct,
                None,
                op_id,
            );
            assert!(second.created_hangar_items.is_empty());
            for &(item_id, _, loc, _) in &first.created_hangar_items {
                assert_eq!(loc, 1_040_800_001);
                assert!(snap.items.contains_key(&item_id));
            }
            save_store_pub(snap, &store, now);
            first
        });
        assert_eq!(out_struct.created_hangar_items.len(), 2);

        // 3. In-space char without sess_docked gets no hangar items and enqueues nothing for activation
        ctx.db.with_mut_snapshot(|snap| {
            let mut store = load_store_pub(snap);
            let rec = store.by_character.entry(char_in_space).or_default();
            let out_space = grant_operation_activation_rewards(
                snap,
                rec,
                &defs,
                _uid3,
                char_in_space,
                None,
                op_id,
            );
            assert!(out_space.created_hangar_items.is_empty());
        });

        // 4. Activation + task/op + category payouts for race 4 character docked at station
        let (out_act, out_blocked, out_full, out_cat) = ctx.db.with_mut_snapshot(|snap| {
            let mut store = load_store_pub(snap);
            let rec = store.by_character.entry(char_docked_station).or_default();
            let act = grant_operation_activation_rewards(
                snap,
                rec,
                &defs,
                uid1,
                char_docked_station,
                None,
                op_id,
            );
            // Amount=1 item is singleton=true, amount=10 is singleton=false
            assert_eq!(act.created_hangar_items.len(), 3);
            let one_item = act.created_hangar_items.iter().find(|x| x.1 == 34).unwrap().0;
            let stack_item = act.created_hangar_items.iter().find(|x| x.1 == 35).unwrap().0;
            assert!(snap.items.get(&one_item).unwrap().singleton);
            assert!(!snap.items.get(&stack_item).unwrap().singleton);

            // op_just_completed = true while t_req not in server_verified_tasks -> op & unverified task rewards blocked
            rec.skipped_tasks.insert(t_skip);
            let blocked = grant_task_and_operation_rewards(
                snap,
                rec,
                &defs,
                uid1,
                char_docked_station,
                None,
                now,
                op_id,
                Some(t_req),
                true,
            );
            assert!(blocked.created_hangar_items.is_empty());
            assert!(blocked.new_balance_cents.is_none());
            assert!(!rec.rewarded_operations.contains(&op_id));

            // Mark t_req as server_verified & completed -> first verify task-only payout with op_just_completed=false
            rec.server_verified_tasks.insert(t_req);
            rec.completed_tasks.insert(t_req);
            let task_only = grant_task_and_operation_rewards(
                snap,
                rec,
                &defs,
                uid1,
                char_docked_station,
                None,
                now,
                op_id,
                Some(t_req),
                false,
            );
            assert_eq!(task_only.created_hangar_items.len(), 1);
            assert_eq!(task_only.created_hangar_items[0].1, 35);
            assert!(task_only.new_balance_cents.is_none());

            // Now mark t_cli_skip skipped and op_just_completed=true -> unlocks op rewards
            rec.skipped_tasks.insert(t_cli_skip);
            let full = grant_task_and_operation_rewards(
                snap,
                rec,
                &defs,
                uid1,
                char_docked_station,
                None,
                now,
                op_id,
                Some(t_req), // already rewarded task -> skipped!
                true,
            );
            assert_eq!(
                full.new_balance_cents,
                Some(5_100_000),
                "full={full:?} rec.rewarded_ops={:?} rec.verified={:?} rec.skipped={:?}",
                rec.rewarded_operations,
                rec.server_verified_tasks,
                rec.skipped_tasks
            );
            assert_eq!(full.created_hangar_items.len(), 1);
            assert_eq!(full.created_hangar_items[0].1, 38); // only race 4 matches!
            assert!(rec.rewarded_operations.contains(&op_id));

            // Category reward grant + idempotent repeat
            let cat_first =
                grant_category_completion_rewards(snap, rec, &defs, char_docked_station, now, cat_id);
            let cat_second =
                grant_category_completion_rewards(snap, rec, &defs, char_docked_station, now, cat_id);
            assert_eq!(cat_first.new_balance_cents, Some(7_600_000));
            assert!(cat_second.new_balance_cents.is_none());

            // Category with 0 ISK emits nothing
            let cat_zero =
                grant_category_completion_rewards(snap, rec, &defs, char_docked_station, now, 999);
            assert!(cat_zero.new_balance_cents.is_none());

            save_store_pub(snap, &store, now);
            (act, blocked, full, cat_first)
        });

        let _ = (out_act, out_blocked, out_cat);
        // 5. Notifications emitted for full outcome (including enqueued_redeem_tokens flag)
        let _ = ctx.drain_notifications_for_char(char_docked_station);
        let mut notify_out = out_full.clone();
        notify_out.enqueued_redeem_tokens = true;
        emit_operation_reward_notifications(&ctx, char_docked_station, &notify_out);
        let notifs = ctx.drain_notifications_for_char(char_docked_station);
        assert!(notifs.iter().any(|n| n.event == "OnAccountChange"));
        assert!(notifs.iter().any(|n| n.event == "OnItemChange"));
        assert!(
            notifs.len() >= 2,
            "unexpected notifs: {:?}",
            notifs.iter().map(|n| n.event.as_str()).collect::<Vec<_>>()
        );

        // Empty outcome emits 0 notifications
        emit_operation_reward_notifications(
            &ctx,
            char_docked_station,
            &OperationRewardOutcome::default(),
        );
        assert!(ctx.drain_notifications_for_char(char_docked_station).is_empty());
    }

    #[test]
    fn client_events_extra_data_variants_system_mismatch_and_server_clamping() {
        let ctx = make_ctx();
        let now = ctx.now_unix();
        let char_id = 94_100_010;
        let uid = provision_char(&ctx, char_id, 1, 30_000_142, Some(60_003_760), None);
        let mut sess = SessionState::new(710);
        sess.user_id = Some(uid);
        sess.character_id = Some(char_id);
        sess.solar_system_id = Some(30_000_142);
        sess.station_id = Some(60_003_760);

        let defs = get_operation_defs(&ctx.sde);
        // Find a real op with >= 1 task and find a conditional task + a multi-step task if present
        let cond_task = defs
            .task_event_conditions
            .iter()
            .find(|(tid, conds)| {
                !conds.is_empty()
                    && defs.task_event_names.contains_key(tid)
                    && defs.task_to_op.contains_key(tid)
            })
            .map(|(&tid, c)| (tid, c.clone()));

        if let Some((tid, conds)) = cond_task {
            let op_id = defs.task_to_op[&tid];
            let ev_name = defs.task_event_names[&tid].clone();
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store_pub(snap);
                let rec = store.by_character.entry(char_id).or_default();
                rec.active_operation_id = Some(op_id);
                rec.active_operation_system_id = Some(30_002_187); // mismatch with 30_000_142
                rec.active_category_id = defs.op_default_category.get(&op_id).copied();
                save_store_pub(snap, &store, now);
            });

            // 1. System mismatch rejects client event
            let wrong_sys = handle_operation_task_client_event(
                &ctx,
                &sess,
                uid,
                char_id,
                ev_name.clone(),
                None,
                false,
            );
            assert!(wrong_sys.is_ok());
            assert!(ctx.db.with_snapshot(|snap| !load_store_pub(snap)
                .by_character
                .get(&char_id)
                .is_some_and(|r| r.completed_tasks.contains(&tid))));

            // Align system ID
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store_pub(snap);
                if let Some(rec) = store.by_character.get_mut(&char_id) {
                    rec.active_operation_system_id = Some(30_000_142);
                }
                save_store_pub(snap, &store, now);
            });

            // 2. Non-client event with missing condition key fails, and KeyVal with wrong value fails
            let empty_dict = PyRep::Dict(vec![]);
            let missing_cond = handle_operation_task_client_event(
                &ctx,
                &sess,
                uid,
                char_id,
                format!("client_{ev_name}"),
                Some(&empty_dict),
                true,
            );
            assert!(matches!(missing_cond.unwrap(), PyRep::Bool(false)));

            let bad_kv = PyRep::Object {
                type_name: "util.KeyVal".to_string(),
                args: Box::new(PyRep::Dict(
                    conds
                        .keys()
                        .map(|k| (PyRep::String(k.to_uppercase()), PyRep::String("__wrong__".into())))
                        .collect(),
                )),
            };
            let kv_mismatch = handle_operation_task_client_event(
                &ctx,
                &sess,
                uid,
                char_id,
                ev_name.clone(),
                Some(&bad_kv),
                true,
            );
            assert!(matches!(kv_mismatch.unwrap(), PyRep::Bool(false)));

            // 3. Matching KeyVal with List/Tuple allowed value succeeds (if client_ or client-completable)
            let good_kv = PyRep::Object {
                type_name: "util.KeyVal".to_string(),
                args: Box::new(PyRep::Dict(
                    conds
                        .iter()
                        .map(|(k, vals)| {
                            let val_str = vals.first().cloned().unwrap_or_default();
                            let pyv = if let Ok(iv) = val_str.parse::<i64>() {
                                PyRep::List(vec![PyRep::Integer(iv)])
                            } else {
                                PyRep::Tuple(vec![PyRep::String(val_str)])
                            };
                            (PyRep::String(k.to_uppercase()), pyv)
                        })
                        .collect(),
                )),
            };
            let _ = handle_operation_task_client_event(
                &ctx,
                &sess,
                uid,
                char_id,
                format!("client_{ev_name}"),
                Some(&good_kv),
                true,
            );
        }

        // 4. Non-GM "complete_task" fails on non-client task (if any exists)
        if let Some((&srv_tid, _)) = defs
            .task_event_names
            .iter()
            .filter(|(&tid, ev)| {
                !defs.client_completable_tasks.contains(&tid)
                    && !ev.starts_with("client_")
                    && defs
                        .task_to_op
                        .get(&tid)
                        .and_then(|op| defs.op_tasks.get(op))
                        .and_then(|ts| ts.first())
                        == Some(&tid)
            })
            .min_by_key(|(&tid, _)| tid)
        {
            let srv_op = defs.task_to_op[&srv_tid];
            let char2 = 94_100_011;
            let uid2 = provision_char(&ctx, char2, 1, 30_000_142, Some(60_003_760), None);
            let mut non_gm_sess = SessionState::new(711);
            non_gm_sess.user_id = Some(uid2);
            non_gm_sess.character_id = Some(char2);
            non_gm_sess.solar_system_id = Some(30_000_142);
            non_gm_sess.role = 0; // non-GM
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store_pub(snap);
                let rec = store.by_character.entry(char2).or_default();
                rec.active_operation_id = Some(srv_op);
                rec.active_operation_system_id = Some(30_000_142);
                save_store_pub(snap, &store, now);
            });
            let arg_tid = PyRep::Integer(srv_tid);
            let denied = handle_operation_task_client_event(
                &ctx,
                &non_gm_sess,
                uid2,
                char2,
                "complete_task".to_string(),
                Some(&arg_tid),
                true,
            );
            assert!(matches!(denied.unwrap(), PyRep::Bool(false)));
        }

        // 5. Multi-step server task + u64::MAX delta clamping + sink=None safety
        if let Some((&multi_tid, &target)) = defs
            .task_target_values
            .iter()
            .filter(|(&tid, &tgt)| {
                tgt > 1
                    && defs
                        .task_event_names
                        .get(&tid)
                        .is_some_and(|ev| !ev.starts_with("client_"))
                    && defs.task_to_op.contains_key(&tid)
            })
            .min_by_key(|(&tid, _)| {
                let cond_len = defs.task_event_conditions.get(&tid).map_or(0, |m| m.len());
                (cond_len, tid)
            })
        {
            let m_op = defs.task_to_op[&multi_tid];
            let m_ev = defs.task_event_names[&multi_tid].clone();
            let char3 = 94_100_012;
            let _ = provision_char(&ctx, char3, 1, 30_000_142, Some(60_003_760), None);
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store_pub(snap);
                let rec = store.by_character.entry(char3).or_default();
                rec.active_operation_id = Some(m_op);
                rec.active_category_id = defs.op_default_category.get(&m_op).copied();
                rec.active_operation_system_id = Some(30_000_142);
                if let Some(prereqs) = defs.op_prerequisites.get(&m_op) {
                    rec.completed_operations.extend(prereqs.iter().copied());
                }
                if let Some(parents) = defs.op_tree_parents.get(&m_op) {
                    rec.completed_operations.extend(parents.iter().copied());
                }
                if let Some(tasks) = defs.op_tasks.get(&m_op) {
                    for &prev_tid in tasks {
                        if prev_tid == multi_tid {
                            break;
                        }
                        rec.completed_tasks.insert(prev_tid);
                        rec.server_verified_tasks.insert(prev_tid);
                    }
                }
                save_store_pub(snap, &store, now);
            });
            let cond_owned: Vec<(String, String)> = defs
                .task_event_conditions
                .get(&multi_tid)
                .map(|m| {
                    m.iter()
                        .map(|(k, vals)| (k.clone(), vals.first().cloned().unwrap_or_default()))
                        .collect()
                })
                .unwrap_or_default();
            let cond_refs: Vec<(&str, String)> = cond_owned
                .iter()
                .map(|(k, v)| (k.as_str(), v.clone()))
                .collect();
            *ctx.quasar_gateway.macho_notify_sink.write().unwrap() = None;
            // Partial step with delta=1
            advance_and_notify_server_operation_tasks(
                &ctx.db,
                &ctx.sde,
                &ctx.quasar_gateway,
                char3,
                &["__ignored__", m_ev.as_str()],
                &cond_refs,
                1,
                now,
            );
            let prog_after_1 = ctx.db.with_snapshot(|snap| {
                load_store_pub(snap)
                    .by_character
                    .get(&char3)
                    .and_then(|r| r.task_progress.get(&multi_tid).copied())
                    .unwrap_or(0)
            });
            assert_eq!(
                prog_after_1,
                1,
                "m_op={m_op} multi_tid={multi_tid} m_ev={m_ev} target={target} conds={cond_owned:?} tasks={:?}",
                defs.op_tasks.get(&m_op)
            );

            // Huge delta clamps cleanly to u32::MAX without overflow and completes task
            advance_and_notify_server_operation_tasks(
                &ctx.db,
                &ctx.sde,
                &ctx.quasar_gateway,
                char3,
                &[m_ev.as_str()],
                &cond_refs,
                u64::MAX,
                now,
            );
            let (is_done, prog_after_max) = ctx.db.with_snapshot(|snap| {
                load_store_pub(snap)
                    .by_character
                    .get(&char3)
                    .map(|r| (r.completed_tasks.contains(&multi_tid), r.task_progress.get(&multi_tid).copied()))
                    .unwrap_or((false, None))
            });
            assert!(is_done, "expected multi_tid={multi_tid} target={target} completed, prog={prog_after_max:?}");
            assert!(
                prog_after_max.is_none_or(|p| p >= target),
                "unexpected prog_after_max={prog_after_max:?} target={target}"
            );
        }
    }

    #[test]
    fn record_invbroker_add_negative_guards() {
        let ctx = make_ctx();
        let now = ctx.now_unix();
        let char_id = 94_100_020;
        let _ = provision_char(&ctx, char_id, 1, 30_000_142, None, None);
        let ship_id = char_id * 10 + 1;
        let non_wreck_id = 88_000_001;
        ctx.db.with_mut_snapshot(|snap| {
            let mut store = load_store_pub(snap);
            if let Some(&op_id) = get_operation_defs(&ctx.sde).sde_operations.iter().next() {
                store.by_character.entry(char_id).or_default().active_operation_id = Some(op_id);
            }
            save_store_pub(snap, &store, now);
            snap.items.insert(
                non_wreck_id,
                InventoryItemRecord {
                    item_id: non_wreck_id,
                    type_id: 587, // ship/frigate, NOT wreck/cargo container
                    owner_id: char_id,
                    location_id: 30_000_142,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        // Negative early returns should never panic or mutate task progress
        record_invbroker_add_operation_event(&ctx, char_id, ship_id, ship_id, InventoryFlag::Cargo, 34, 5);
        record_invbroker_add_operation_event(&ctx, char_id, 0, ship_id, InventoryFlag::Cargo, 34, 5);
        record_invbroker_add_operation_event(&ctx, char_id, -1, ship_id, InventoryFlag::Cargo, 34, 5);
        record_invbroker_add_operation_event(&ctx, char_id, non_wreck_id, ship_id + 99, InventoryFlag::Cargo, 34, 5);
        record_invbroker_add_operation_event(&ctx, char_id, non_wreck_id, ship_id, InventoryFlag::Cargo, 34, 5);
    }

    #[test]
    fn batch3_gm_complete_dict_conditions_redeem_queue_server_completion_and_wreck_looting() {
        let ctx = make_ctx();
        let now = ctx.now_unix();
        let defs = get_operation_defs(&ctx.sde);

        // 1. Empty SDE resolve_site_dungeon_id returns None
        let empty_sde = SdeRegistry::default();
        assert_eq!(defs.resolve_site_dungeon_id(&empty_sde, 100, 1), None);

        // 2. GM complete_task succeeds on server task and rejects invalid/unknown task args
        let mut gm_candidates: Vec<(i64, i64)> = defs
            .op_tasks
            .iter()
            .filter_map(|(&op_id, tasks)| {
                let first_tid = *tasks.first()?;
                let ev = defs.task_event_names.get(&first_tid)?;
                if !defs.client_completable_tasks.contains(&first_tid) && !ev.starts_with("client_") {
                    Some((op_id, first_tid))
                } else {
                    None
                }
            })
            .collect();
        gm_candidates.sort_unstable();
        if let Some((srv_op, srv_tid)) = gm_candidates.first().copied() {
            let char_gm = 94_100_031;
            let uid_gm = provision_char(&ctx, char_gm, 1, 30_000_142, Some(60_003_760), None);
            let mut gm_sess = SessionState::new(712);
            gm_sess.user_id = Some(uid_gm);
            gm_sess.character_id = Some(char_gm);
            gm_sess.solar_system_id = Some(30_000_142);
            gm_sess.station_id = Some(60_003_760);
            gm_sess.role = eve_machonet::service_roles::ROLE_GMH;
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store_pub(snap);
                let rec = store.by_character.entry(char_gm).or_default();
                rec.active_operation_id = Some(srv_op);
                rec.active_category_id = defs.op_default_category.get(&srv_op).copied();
                rec.active_operation_system_id = Some(30_000_142);
                save_store_pub(snap, &store, now);
            });
            // Non-GM complete_task on server task returns false before GM completes it
            let mut non_gm_sess = gm_sess.clone();
            non_gm_sess.role = 0;
            let non_gm_bad = handle_operation_task_client_event(
                &ctx,
                &non_gm_sess,
                uid_gm,
                char_gm,
                "complete_task".to_string(),
                Some(&PyRep::Integer(srv_tid)),
                true,
            )
            .expect("non-gm complete_task");
            assert!(matches!(non_gm_bad, PyRep::Bool(false)));

            let gm_ok = handle_operation_task_client_event(
                &ctx,
                &gm_sess,
                uid_gm,
                char_gm,
                "complete_task".to_string(),
                Some(&PyRep::Integer(srv_tid)),
                true,
            )
            .expect("gm complete_task");
            assert!(matches!(gm_ok, PyRep::Bool(true)));
            assert!(ctx.db.with_snapshot(|snap| load_store_pub(snap)
                .by_character
                .get(&char_gm)
                .is_some_and(|r| r.completed_tasks.contains(&srv_tid))));
        }

        // 3. Dict extra_data / None / invalid extra_data variants on conditional task
        let mut cond_candidates: Vec<i64> = defs
            .task_event_conditions
            .iter()
            .filter(|(tid, c)| {
                !c.is_empty() && defs.task_event_names.contains_key(tid) && defs.task_to_op.contains_key(tid)
            })
            .map(|(tid, _)| *tid)
            .collect();
        cond_candidates.sort_unstable();
        if let Some(tid) = cond_candidates.first().copied() {
            let conds = &defs.task_event_conditions[&tid];
            let op_id = defs.task_to_op[&tid];
            let ev = defs.task_event_names[&tid].clone();
            let char_dict = 94_100_032;
            let uid_d = provision_char(&ctx, char_dict, 1, 30_000_142, Some(60_003_760), None);
            let mut s_dict = SessionState::new(713);
            s_dict.user_id = Some(uid_d);
            s_dict.character_id = Some(char_dict);
            s_dict.solar_system_id = Some(30_000_142);
            s_dict.station_id = Some(60_003_760);
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store_pub(snap);
                let rec = store.by_character.entry(char_dict).or_default();
                rec.active_operation_id = Some(op_id);
                rec.active_category_id = defs.op_default_category.get(&op_id).copied();
                rec.active_operation_system_id = Some(30_000_142);
                save_store_pub(snap, &store, now);
            });
            // None & primitive extra_data fail condition checks cleanly
            assert!(matches!(
                handle_operation_task_client_event(&ctx, &s_dict, uid_d, char_dict, format!("client_{ev}"), None, true).unwrap(),
                PyRep::Bool(false)
            ));
            assert!(matches!(
                handle_operation_task_client_event(&ctx, &s_dict, uid_d, char_dict, format!("client_{ev}"), Some(&PyRep::Integer(42)), true).unwrap(),
                PyRep::Bool(false)
            ));
            // Raw PyRep::Dict with mixed String / Integer / Real values
            let dict_rep = PyRep::Dict(
                conds
                    .iter()
                    .enumerate()
                    .map(|(idx, (k, vals))| {
                        let v = vals.first().cloned().unwrap_or_default();
                        let pyv = if let Ok(iv) = v.parse::<i64>() {
                            if idx % 2 == 0 {
                                PyRep::Integer(iv)
                            } else {
                                PyRep::Real(iv as f64)
                            }
                        } else {
                            PyRep::String(v)
                        };
                        (PyRep::String(k.clone()), pyv)
                    })
                    .collect(),
            );
            let _ = handle_operation_task_client_event(
                &ctx,
                &s_dict,
                uid_d,
                char_dict,
                format!("client_{ev}"),
                Some(&dict_rep),
                true,
            );
        }

        // 4. In-space grant_task_and_operation_rewards enqueues redeem tokens + task_is_optional coverage
        let mut syn = OperationDefinitions::default();
        let s_op = 801;
        let s_cat = 41;
        let s_tid = 8001;
        syn.sde_categories.insert(s_cat);
        syn.category_isk_rewards.insert(s_cat, 10_000);
        syn.sde_operations.insert(s_op);
        syn.op_default_category.insert(s_op, s_cat);
        syn.op_isk_rewards.insert(s_op, 20_000);
        syn.op_tasks.insert(s_op, vec![s_tid]);
        syn.op_completion_rewards.insert(
            s_op,
            vec![SdeOperationItemReward {
                type_id: 34,
                amount: 5,
                race_id: None,
            }],
        );
        let char_space = 94_100_033;
        let uid_space = provision_char(&ctx, char_space, 1, 30_000_142, None, None);
        ctx.db.with_mut_snapshot(|snap| {
            let mut rec = CharacterOperationsRecord::default();
            rec.server_verified_tasks.insert(s_tid);
            rec.completed_tasks.insert(s_tid);
            let out = grant_task_and_operation_rewards(
                snap,
                &mut rec,
                &syn,
                uid_space,
                char_space,
                None,
                now,
                s_op,
                None,
                true,
            );
            assert!(out.enqueued_redeem_tokens);
            assert!(out.created_hangar_items.is_empty());
            assert_eq!(out.new_balance_cents, Some(2_100_000));
            assert!(rec.rewarded_operations.contains(&s_op));
        });

        // 5. Server event completing final task of a real SDE op emits completion notifications & payouts
        let mut final_candidates: Vec<(i64, i64)> = defs
            .op_tasks
            .iter()
            .filter_map(|(&op_id, tasks)| {
                let &tid = tasks.last()?;
                let ev = defs.task_event_names.get(&tid)?;
                if !ev.starts_with("client_")
                    && defs.task_target_values.get(&tid).copied().unwrap_or(1) <= 1
                    && defs
                        .task_event_conditions
                        .get(&tid)
                        .is_none_or(|c| c.is_empty())
                {
                    Some((op_id, tid))
                } else {
                    None
                }
            })
            .collect();
        final_candidates.sort_unstable();
        if let Some((f_op, final_tid)) = final_candidates.first().copied() {
            let f_ev = defs.task_event_names[&final_tid].clone();
            let char_srv = 94_100_034;
            let _ = provision_char(&ctx, char_srv, 1, 30_000_142, Some(60_003_760), None);
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store_pub(snap);
                let rec = store.by_character.entry(char_srv).or_default();
                rec.active_operation_id = Some(f_op);
                rec.active_category_id = defs.op_default_category.get(&f_op).copied();
                rec.active_operation_system_id = Some(30_000_142);
                if let Some(prereqs) = defs.op_prerequisites.get(&f_op) {
                    rec.completed_operations.extend(prereqs.iter().copied());
                }
                if let Some(parents) = defs.op_tree_parents.get(&f_op) {
                    rec.completed_operations.extend(parents.iter().copied());
                }
                if let Some(tasks) = defs.op_tasks.get(&f_op) {
                    for &t in tasks {
                        if t != final_tid {
                            rec.completed_tasks.insert(t);
                            rec.server_verified_tasks.insert(t);
                        }
                    }
                }
                save_store_pub(snap, &store, now);
            });
            let sink_hits: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let sink_c = Arc::clone(&sink_hits);
            *ctx.quasar_gateway.macho_notify_sink.write().unwrap() =
                Some(Arc::new(move |_t, method, _id, _args| {
                    sink_c.lock().unwrap().push(method.to_string());
                }));
            advance_and_notify_server_operation_tasks(
                &ctx.db,
                &ctx.sde,
                &ctx.quasar_gateway,
                char_srv,
                &[f_ev.as_str()],
                &[],
                1,
                now,
            );
            // Repeat zero-progress event + record_server_operation_event_in_db call
            let empty_res = record_server_operation_event_in_db(
                &ctx.db,
                &ctx.sde,
                char_srv,
                f_ev.as_str(),
                &[],
                0,
                now,
            );
            assert!(empty_res.0.is_none());
            assert!(ctx.db.with_snapshot(|snap| load_store_pub(snap)
                .by_character
                .get(&char_srv)
                .is_some_and(|r| r.completed_tasks.contains(&final_tid))));
        }

        // 6. record_invbroker_add_operation_event with real wreck item (group_id = 186) & container
        if let Some(&wreck_tid) = ctx.sde.types.values().find(|t| t.group_id == 186).map(|t| &t.type_id) {
            let char_w = 94_100_035;
            let _ = provision_char(&ctx, char_w, 1, 30_000_142, None, None);
            let ship_w = char_w * 10 + 1;
            let wreck_id = 88_100_001;
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.insert(
                    wreck_id,
                    InventoryItemRecord {
                        item_id: wreck_id,
                        type_id: wreck_tid,
                        owner_id: 0,
                        location_id: 30_000_142,
                        flag: InventoryFlag::None,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        ..Default::default()
                    },
                );
            });
            record_invbroker_add_operation_event(&ctx, char_w, wreck_id, ship_w, InventoryFlag::Cargo, 34, 5);
            record_invbroker_add_operation_event(&ctx, char_w, wreck_id, ship_w, InventoryFlag::Cargo, 34, 0);
        }
    }

    #[test]
    fn batch4_server_conds_notify_client_progress_non_gm_and_empty_defs() {
        let empty_defs = load_operation_definitions(&SdeRegistry::default());
        assert!(empty_defs.sde_operations.is_empty());

        let ctx = make_ctx();
        let now = ctx.now_unix();
        let defs = get_operation_defs(&ctx.sde);

        // 1. Zero-reward operation hits zero-ISK early-None path cleanly
        let char_zero = 94_100_041;
        let uid_zero = provision_char(&ctx, char_zero, 1, 30_000_142, Some(60_003_760), None);
        let zero_defs = OperationDefinitions::default();
        ctx.db.with_mut_snapshot(|snap| {
            let mut rec = CharacterOperationsRecord::default();
            let out = grant_task_and_operation_rewards(
                snap,
                &mut rec,
                &zero_defs,
                uid_zero,
                char_zero,
                Some(60_003_760),
                now,
                999,
                Some(9999),
                false,
            );
            assert!(out.new_balance_cents.is_none());
        });

        // 2. record_server_operation_event with task_event_conditions (mismatch then match + completion notify)
        let mut srv_cond_list: Vec<(i64, i64, String)> = defs
            .task_event_conditions
            .iter()
            .filter_map(|(&tid, conds)| {
                if conds.is_empty() {
                    return None;
                }
                let ev = defs.task_event_names.get(&tid)?;
                let op_id = *defs.task_to_op.get(&tid)?;
                Some((op_id, tid, ev.clone()))
            })
            .collect();
        srv_cond_list.sort_unstable();

        if let Some((c_op, c_tid, c_ev)) = srv_cond_list
            .iter()
            .find(|(_, _, ev)| !ev.starts_with("client_"))
            .cloned()
        {
            let char_sc = 94_100_042;
            let _ = provision_char(&ctx, char_sc, 1, 30_000_142, Some(60_003_760), None);
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store_pub(snap);
                let rec = store.by_character.entry(char_sc).or_default();
                rec.active_operation_id = Some(c_op);
                rec.active_category_id = defs.op_default_category.get(&c_op).copied();
                rec.active_operation_system_id = Some(30_000_142);
                if let Some(tasks) = defs.op_tasks.get(&c_op) {
                    for &t in tasks {
                        if t != c_tid {
                            rec.completed_tasks.insert(t);
                            rec.server_verified_tasks.insert(t);
                        }
                    }
                }
                save_store_pub(snap, &store, now);
            });
            // Mismatched condition param does not advance
            let bad_params = vec![("nonexistent_cond_key", "bad".to_string())];
            record_server_operation_event(&ctx, char_sc, &c_ev, &bad_params, 1);
            assert!(ctx.db.with_snapshot(|snap| load_store_pub(snap)
                .by_character
                .get(&char_sc)
                .is_some_and(|r| !r.completed_tasks.contains(&c_tid))));

            // Valid condition params pass and trigger OnOperationTaskCompleted notify
            let conds = &defs.task_event_conditions[&c_tid];
            let mut valid_params: Vec<(&str, String)> = Vec::new();
            for (k, vals) in conds {
                if let Some(v) = vals.first() {
                    valid_params.push((k.as_str(), v.clone()));
                }
            }
            let _ = ctx.drain_notifications_for_char(char_sc);
            let target_v = defs.task_target_values.get(&c_tid).copied().unwrap_or(1).max(1);
            record_server_operation_event(&ctx, char_sc, &c_ev, &valid_params, target_v as u32);
            assert!(ctx.db.with_snapshot(|snap| load_store_pub(snap)
                .by_character
                .get(&char_sc)
                .is_some_and(|r| r.completed_tasks.contains(&c_tid))));
            let notifs = ctx.drain_notifications_for_char(char_sc);
            assert!(notifs.iter().any(|n| n.event == "OnOperationTaskCompleted"));
        }

        // 3. Test any client_ task with task_event_conditions (Tuple / List / KeyVal / wrong types)
        if let Some((cl_op, cl_tid, cl_ev)) = srv_cond_list
            .iter()
            .find(|(_, _, ev)| ev.starts_with("client_"))
            .cloned()
        {
            let char_cl = 94_100_043;
            let uid_cl = provision_char(&ctx, char_cl, 1, 30_000_142, Some(60_003_760), None);
            let mut sess_cl = SessionState::new(719);
            sess_cl.user_id = Some(uid_cl);
            sess_cl.character_id = Some(char_cl);
            sess_cl.solar_system_id = Some(30_000_142);
            sess_cl.station_id = Some(60_003_760);
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store_pub(snap);
                let rec = store.by_character.entry(char_cl).or_default();
                rec.active_operation_id = Some(cl_op);
                rec.active_category_id = defs.op_default_category.get(&cl_op).copied();
                rec.active_operation_system_id = Some(30_000_142);
                if let Some(tasks) = defs.op_tasks.get(&cl_op) {
                    for &t in tasks {
                        if t != cl_tid {
                            rec.completed_tasks.insert(t);
                        }
                    }
                }
                save_store_pub(snap, &store, now);
            });
            let conds = &defs.task_event_conditions[&cl_tid];
            let kv_entries: std::collections::HashMap<String, PyRep> = conds
                .iter()
                .map(|(k, vals)| {
                    let v = vals.first().cloned().unwrap_or_default();
                    (k.clone(), PyRep::Tuple(vec![PyRep::String(v)]))
                })
                .collect();
            let _ = handle_operation_task_client_event(
                &ctx,
                &sess_cl,
                uid_cl,
                char_cl,
                cl_ev.clone(),
                Some(&PyRep::KeyVal(kv_entries)),
                true,
            );
        }

        // 4. Multi-step client task partial progress -> progressed == true, completed_task == None
        let mut multi_client: Vec<(i64, i64, String)> = defs
            .op_tasks
            .iter()
            .filter_map(|(&op_id, tasks)| {
                let &tid = tasks.first()?;
                let ev = defs.task_event_names.get(&tid)?;
                let target = defs.task_target_values.get(&tid).copied().unwrap_or(1);
                if ev.starts_with("client_") && target > 1 && defs.task_event_conditions.get(&tid).is_none_or(|c| c.is_empty()) {
                    Some((op_id, tid, ev.clone()))
                } else {
                    None
                }
            })
            .collect();
        multi_client.sort_unstable();
        if let Some((m_op, m_tid, m_ev)) = multi_client.first().cloned() {
            let char_mc = 94_100_044;
            let uid_mc = provision_char(&ctx, char_mc, 1, 30_000_142, Some(60_003_760), None);
            let mut sess_mc = SessionState::new(720);
            sess_mc.user_id = Some(uid_mc);
            sess_mc.character_id = Some(char_mc);
            sess_mc.solar_system_id = Some(30_000_142);
            sess_mc.station_id = Some(60_003_760);
            ctx.db.with_mut_snapshot(|snap| {
                let mut store = load_store_pub(snap);
                let rec = store.by_character.entry(char_mc).or_default();
                rec.active_operation_id = Some(m_op);
                rec.active_category_id = defs.op_default_category.get(&m_op).copied();
                rec.active_operation_system_id = Some(30_000_142);
                save_store_pub(snap, &store, now);
            });
            let partial = handle_operation_task_client_event(
                &ctx,
                &sess_mc,
                uid_mc,
                char_mc,
                m_ev,
                None,
                true,
            )
            .expect("partial client progress");
            assert!(matches!(partial, PyRep::Bool(true)));
            assert!(ctx.db.with_snapshot(|snap| load_store_pub(snap)
                .by_character
                .get(&char_mc)
                .is_some_and(|r| !r.completed_tasks.contains(&m_tid)
                    && r.task_progress.get(&m_tid).copied().unwrap_or(0) >= 1)));
        }
    }
}
