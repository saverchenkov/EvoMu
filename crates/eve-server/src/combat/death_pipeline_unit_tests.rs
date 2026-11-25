//! Unit and mutation-killing tests for `combat::death_pipeline`.

use super::*;
use crate::formulas::compute_insurance_quote;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::AssistLogEntry;
use eve_db::{
    ContractRecord, DisconnectedShipEntry, InventoryFlag, InventoryItemRecord, IskCents,
    PersistedInsurgencyCampaign,
};
use eve_destiny::{Ball, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    insured_payout_isk: f64,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1 && flag != InventoryFlag::Cargo,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

fn spawn_ship(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    owner_id: i64,
    type_id: u32,
    pos: DVec3,
    insured_payout_isk: f64,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(
                ship_id,
                type_id,
                owner_id,
                sys_id as i64,
                InventoryFlag::None,
                1,
                insured_payout_isk,
            ),
        );
        snap.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let cs = ctx.build_ship_combat_state(owner_id, ship_id, type_id);
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        },
        cs,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn death_pipeline_rejects_non_ship_and_covers_all_insurance_branches() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_142;
    let victim = 90_000_001;
    let attacker = 90_000_002;
    let now = ctx.now_unix();

    // 1. Non-ship item (mineral type 34, category 4 != 6) returns Err
    let mineral_id = 100_200_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mineral_id,
            make_item(mineral_id, 34, victim, sys_id as i64, InventoryFlag::None, 1, 0.0),
        );
    });
    let non_ship_err =
        execute_death_pipeline_sync(&ctx, sys_id, victim, mineral_id, attacker, DVec3::ZERO, 1)
            .unwrap_err();
    assert!(non_ship_err.contains("is not a ship"));

    // 2. Criminal victim gets 0.0 insurance payout even with insured_payout_isk > 0
    let crim_ship = 100_200_002;
    spawn_ship(&ctx, sys_id, crim_ship, victim, 587, DVec3::ZERO, 500_000.0);
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(victim).criminal_flag_until_unix = now + 900;
    }
    let crim_res =
        execute_death_pipeline_sync(&ctx, sys_id, victim, crim_ship, attacker, DVec3::ZERO, 1)
            .unwrap();
    assert_eq!(crim_res.insurance_payout_isk, 0.0);
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(victim).criminal_flag_until_unix = 0;
    }

    // 3. Expired rt_contract (end_unix == now) falls back to 40% base quote
    let exp_ship = 100_200_003;
    spawn_ship(&ctx, sys_id, exp_ship, victim, 587, DVec3::ZERO, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurance_contracts.insert(
            exp_ship,
            eve_db::PersistedInsuranceContractRecord {
                ship_id: exp_ship,
                ship_type_id: 587,
                owner_id: victim,
                level_name: "Platinum".into(),
                level_fraction: 1.0,
                cost_isk: 100_000.0,
                payout_isk: 999_999.0,
                start_unix: now.saturating_sub(1_000),
                end_unix: now,
            },
        );
    });
    let base_587 = ctx.sde.types.get(&587).map(|t| t.base_price).unwrap_or(230_000.0);
    let expected_default = compute_insurance_quote(base_587, 0.40).1;
    let exp_res =
        execute_death_pipeline_sync(&ctx, sys_id, victim, exp_ship, attacker, DVec3::ZERO, 1)
            .unwrap();
    assert!((exp_res.insurance_payout_isk - expected_default).abs() < 1e-6);

    // 4. Active corporation-owned rt_contract credits corp master_wallet & division 0
    let corp_id = ctx.db.with_snapshot(|s| *s.corporations.keys().next().unwrap());
    let corp_ship = 100_200_004;
    spawn_ship(&ctx, sys_id, corp_ship, victim, 587, DVec3::ZERO, 0.0);
    let corp_bal_before = ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet_balance());
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurance_contracts.insert(
            corp_ship,
            eve_db::PersistedInsuranceContractRecord {
                ship_id: corp_ship,
                ship_type_id: 587,
                owner_id: corp_id,
                level_name: "Platinum".into(),
                level_fraction: 1.0,
                cost_isk: 100_000.0,
                payout_isk: 750_000.0,
                start_unix: now.saturating_sub(100),
                end_unix: now + 100,
            },
        );
    });
    let corp_res =
        execute_death_pipeline_sync(&ctx, sys_id, victim, corp_ship, attacker, DVec3::ZERO, 1)
            .unwrap();
    assert_eq!(corp_res.insurance_payout_isk, 750_000.0);
    let corp_bal_after = ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet_balance());
    assert_eq!(
        corp_bal_after,
        corp_bal_before.checked_add(IskCents::from_f64_isk(750_000.0)).unwrap()
    );

    // 5. Active snap.insurance_contracts with owner_id <= 0 falls back to victim_char_id
    let cs_ship = 100_200_005;
    spawn_ship(&ctx, sys_id, cs_ship, victim, 587, DVec3::ZERO, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurance_contracts.insert(
            cs_ship,
            eve_db::PersistedInsuranceContractRecord {
                ship_id: cs_ship,
                ship_type_id: 587,
                owner_id: 0, // tests owner_id <= 0 fallback to victim_char_id
                level_name: "Gold".into(),
                level_fraction: 0.8,
                cost_isk: 80_000.0,
                payout_isk: 612_345.0,
                start_unix: now.saturating_sub(10),
                end_unix: now + 100,
            },
        );
    });
    let cs_res =
        execute_death_pipeline_sync(&ctx, sys_id, victim, cs_ship, attacker, DVec3::ZERO, 1)
            .unwrap();
    assert_eq!(cs_res.insurance_payout_isk, 612_345.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn death_pipeline_rigs_orphans_nested_courier_disconnected_and_lowsec_pod_stage5() {
    let ctx = test_ctx();
    let sys_id = 30_002_813; // Low-sec system
    let victim = 90_000_001;
    let attacker = 90_000_002;
    let now = ctx.now_unix();

    let ship_id = 100_210_001;
    let rig_id = 100_210_010;
    let sub_id = 100_210_011;
    let can_in_ship = 100_210_012;
    let orphan_in_can = 100_210_013;
    spawn_ship(&ctx, sys_id, ship_id, victim, 587, DVec3::ZERO, 0.0);

    // Track ship in sp6 disconnected_ships to verify capsule replacement on ship loss
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st| {
        st.disconnected_ships.insert(
            victim,
            DisconnectedShipEntry {
                char_id: victim,
                ship_id,
                ship_type_id: 587,
                solar_system_id: sys_id,
                disconnected_at_unix: now,
                remove_at_unix: now + 60,
                emergency_warp_dest: None,
                last_position: [0.0, 0.0, 0.0],
                last_velocity: [0.0, 0.0, 0.0],
                removed_from_space: false,
                pre_warp_position: None,
            },
        );
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            rig_id,
            make_item(rig_id, 31_790, victim, ship_id, InventoryFlag::RigSlot0, 1, 0.0),
        );
        snap.items.insert(
            sub_id,
            make_item(sub_id, 29_964, victim, ship_id, InventoryFlag::from_u16(125), 1, 0.0),
        );
        snap.items.insert(
            can_in_ship,
            make_item(can_in_ship, 23, victim, ship_id, InventoryFlag::Cargo, 1, 0.0),
        );
        snap.items.insert(
            orphan_in_can,
            make_item(orphan_in_can, 34, victim, can_in_ship, InventoryFlag::Cargo, 50, 0.0),
        );
        // Courier contract referencing item nested inside can_in_ship
        snap.contracts.insert(
            88_001,
            ContractRecord {
                contract_id: 88_001,
                issuer_id: attacker,
                issuer_corp_id: 98_000_001,
                assignee_id: victim,
                acceptor_id: Some(victim),
                contract_type: "Courier".into(),
                status: "InProgress".into(),
                title: "Nested Courier".into(),
                availability: "Personal".into(),
                item_ids: vec![orphan_in_can],
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: IskCents(0),
                reward: IskCents::from_isk(10_000),
                collateral: IskCents::from_isk(50_000),
                expiration_unix: now + 10_000,
                date_issued_unix: now,
                ..Default::default()
            },
        );
    });

    // Self-destruct guarantees drop_threshold == 0.0 so container & orphan are destroyed
    let res = execute_death_pipeline_sync_inner(
        &ctx,
        sys_id,
        victim,
        ship_id,
        attacker,
        DVec3::ZERO,
        42,
        true,
    )
    .unwrap();
    assert!(res.loot_dropped_item_ids.is_empty());
    assert!(res.loot_destroyed_item_ids.contains(&rig_id));
    assert!(res.loot_destroyed_item_ids.contains(&sub_id));
    assert!(res.loot_destroyed_item_ids.contains(&can_in_ship));
    assert!(res.loot_destroyed_item_ids.contains(&orphan_in_can));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.contracts[&88_001].status.clone()),
        "Failed"
    );

    let ejected_pod = res.ejected_capsule_id.expect("capsule ejected");
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st| {
        assert_eq!(st.disconnected_ships[&victim].ship_id, ejected_pod);
        assert_eq!(st.disconnected_ships[&victim].ship_type_id, 670);
    });

    // Low-sec pod kill at Corruption Stage 5 does NOT grant kill right, then pod death removes disconnected_ships
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.insert(
            99_001,
            PersistedInsurgencyCampaign {
                campaign_id: 99_001,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: now.saturating_sub(100),
                forecasting_end_unix: now.saturating_sub(50),
                active_end_unix: now + 1_000,
                winddown_end_unix: now + 2_000,
                corruption_points: HashMap::from([(sys_id, 100_000)]),
                suppression_points: HashMap::new(),
            },
        );
    });
    let _pod_res =
        execute_death_pipeline_sync(&ctx, sys_id, victim, ejected_pod, attacker, DVec3::ZERO, 42)
            .unwrap();
    assert!(ctx
        .db
        .with_snapshot(|s| s.kill_rights.values().all(|kr| !(kr.owner_id == victim && kr.target_id == attacker))));
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st| {
        assert!(!st.disconnected_ships.contains_key(&victim));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn self_destruct_assist_attribution_and_timer_boundaries() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let victim = 90_000_001;
    let attacker_old = 90_000_002;
    let attacker_recent = 90_000_003;
    let now = ctx.now_unix();

    let ship_id = 100_220_001;
    spawn_ship(&ctx, sys_id, ship_id, victim, 587, DVec3::ZERO, 0.0);

    // Record assist entries: expired entry, older entry, most recent valid entry, and self entry
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.aggression_assist_log.insert(
            ship_id,
            vec![
                AssistLogEntry {
                    attacker_char_id: 90_000_099,
                    corporation_id: 98_000_001,
                    alliance_id: None,
                    ship_type_id: 587,
                    weapon_type_id: 587,
                    damage_done: 500.0,
                    is_ewar: false,
                    last_action_unix: now.saturating_sub(2_000), // expired (> 900s)
                },
                AssistLogEntry {
                    attacker_char_id: attacker_old,
                    corporation_id: 98_000_001,
                    alliance_id: None,
                    ship_type_id: 587,
                    weapon_type_id: 587,
                    damage_done: 100.0,
                    is_ewar: false,
                    last_action_unix: now.saturating_sub(50),
                },
                AssistLogEntry {
                    attacker_char_id: attacker_recent,
                    corporation_id: 98_000_001,
                    alliance_id: None,
                    ship_type_id: 587,
                    weapon_type_id: 587,
                    damage_done: 200.0,
                    is_ewar: false,
                    last_action_unix: now.saturating_sub(10),
                },
                AssistLogEntry {
                    attacker_char_id: victim, // self is ignored for final_blow override
                    corporation_id: 98_000_001,
                    alliance_id: None,
                    ship_type_id: 587,
                    weapon_type_id: 587,
                    damage_done: 300.0,
                    is_ewar: false,
                    last_action_unix: now,
                },
            ],
        );
    });

    let sd_res = execute_self_destruct(&ctx, sys_id, victim, ship_id, DVec3::ZERO, 99)
        .await
        .unwrap();
    assert_eq!(sd_res.insurance_payout_isk, 0.0);
    let km = ctx.db.with_snapshot(|s| s.killmails[&sd_res.killmail_id].clone());
    assert_eq!(km.attacker_character_id, attacker_recent);

    // SelfDestructManager exact timer boundaries
    let mut mgr = SelfDestructManager::new();
    assert!(!mgr.abort_self_destruct(999_999));
    assert_eq!(mgr.remaining_seconds(999_999, now), None);

    let ship2 = 100_220_002;
    spawn_ship(&ctx, sys_id, ship2, victim, 587, DVec3::new(1_000.0, 0.0, 0.0), 0.0);
    let det = mgr.start_self_destruct(ship2, victim, sys_id, now);
    assert_eq!(det, now + 120);
    assert_eq!(mgr.remaining_seconds(ship2, det - 1), Some(1));
    assert_eq!(mgr.remaining_seconds(ship2, det + 5), Some(0));

    // At det - 1, timer does not detonate
    let not_yet = mgr.step_timers(&ctx, det - 1, 1).await;
    assert!(not_yet.is_empty());
    assert!(mgr.is_self_destructing(ship2));

    // At exact det, timer detonates
    let detonated = mgr.step_timers(&ctx, det, 1).await;
    assert_eq!(detonated.len(), 1);
    assert_eq!(detonated[0].0, ship2);
    assert!(detonated[0].1.is_ok());
    assert!(!mgr.is_self_destructing(ship2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_self_destruct_sync_denies_uncontested_insurance_loot_validates_ownership_and_credits_aggressor() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let now = ctx.now_unix();
    let victim = 90_000_001_i64;
    let attacker = 90_000_002_i64;

    // 1. Uncontested live self-destruct: 0 insurance payout & 0% loot drop (#539).
    let uncontested_ship = 100_230_001_i64;
    let cargo_item = 100_230_002_i64;
    spawn_ship(&ctx, sys_id, uncontested_ship, victim, 587, DVec3::new(1_000.0, 0.0, 0.0), 500_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = uncontested_ship;
        }
        snap.items.insert(
            cargo_item,
            make_item(cargo_item, 34, victim, uncontested_ship, InventoryFlag::Cargo, 500, 0.0),
        );
    });
    let res_uncontested = execute_live_self_destruct_sync(
        &ctx,
        sys_id,
        victim,
        uncontested_ship,
        DVec3::new(1_000.0, 0.0, 0.0),
        now,
    )
    .expect("uncontested self-destruct succeeds")
    .expect("ship executed");
    assert_eq!(res_uncontested.insurance_payout_isk, 0.0);
    assert_eq!(res_uncontested.loot_dropped_item_ids.len(), 0);
    assert_eq!(res_uncontested.loot_destroyed_item_ids.len(), 1);

    // 2. Ownership / docked / inactive-ship mismatch is refused cleanly without destroying ship.
    let mismatch_ship = 100_230_003_i64;
    spawn_ship(&ctx, sys_id, mismatch_ship, victim, 587, DVec3::new(2_000.0, 0.0, 0.0), 500_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim) {
            ch.solar_system_id = sys_id;
            ch.station_id = Some(60_003_760);
            ch.structure_id = None;
            ch.active_ship_id = mismatch_ship;
        }
    });
    assert!(
        execute_live_self_destruct_sync(&ctx, sys_id, victim, mismatch_ship, DVec3::ZERO, now)
            .expect("ok")
            .is_none(),
        "docked ship must NOT self-destruct"
    );
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&mismatch_ship)));

    // Changing owner also prevents self-destruct.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim) {
            ch.station_id = None;
        }
        snap.items.get_mut(&mismatch_ship).unwrap().owner_id = attacker;
    });
    assert!(
        execute_live_self_destruct_sync(&ctx, sys_id, victim, mismatch_ship, DVec3::ZERO, now)
            .expect("ok")
            .is_none(),
        "transferred ship must NOT self-destruct for previous owner"
    );
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&mismatch_ship)));

    // 3. Tackled / aggressed self-destruct credits recent attacker on killmail + allows loot roll.
    let tackled_ship = 100_230_004_i64;
    let tackled_cargo = 100_230_005_i64;
    spawn_ship(&ctx, sys_id, tackled_ship, victim, 587, DVec3::new(3_000.0, 0.0, 0.0), 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = tackled_ship;
        }
        snap.items.insert(
            tackled_cargo,
            make_item(tackled_cargo, 34, victim, tackled_ship, InventoryFlag::Cargo, 500, 0.0),
        );
    });
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.aggression_assist_log.insert(
            tackled_ship,
            vec![AssistLogEntry {
                attacker_char_id: attacker,
                corporation_id: 98_000_001,
                alliance_id: None,
                ship_type_id: 587,
                weapon_type_id: 2_905,
                damage_done: 150.0,
                is_ewar: false,
                last_action_unix: now.saturating_sub(20),
            }],
        );
    });
    let res_tackled = execute_live_self_destruct_sync(
        &ctx,
        sys_id,
        victim,
        tackled_ship,
        DVec3::new(3_000.0, 0.0, 0.0),
        now,
    )
    .expect("tackled self-destruct succeeds")
    .expect("tackled ship executed");
    assert_ne!(res_tackled.wreck_item_id, 0);
    let km = ctx.db.with_snapshot(|s| s.killmails[&res_tackled.killmail_id].clone());
    assert_eq!(km.attacker_character_id, attacker);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn death_pipeline_sets_wreck_and_capsule_names_queues_set_state_and_emits_insurance_journal_and_mail() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let victim = 90_000_001_i64;
    let attacker = 90_000_002_i64;
    let ship_id = 100_240_001_i64;

    spawn_ship(&ctx, sys_id, ship_id, victim, 587, DVec3::new(5_000.0, 0.0, 0.0), 150_000.0);
    ctx.db
        .acquire_online_lock(1, 9_729, Some(victim), true, ctx.now_unix())
        .unwrap();
    let _ = ctx.drain_notifications_for_char(victim);

    let res = execute_death_pipeline_sync(
        &ctx,
        sys_id,
        victim,
        ship_id,
        attacker,
        DVec3::new(5_000.0, 0.0, 0.0),
        1,
    )
    .expect("death pipeline succeeds");

    assert_ne!(res.wreck_item_id, 0);
    let cap_id = res.ejected_capsule_id.expect("ejected capsule id");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let wreck_slim = sys_handle
        .get_ball_slim_item(res.wreck_item_id)
        .await
        .unwrap()
        .expect("wreck slim item");
    assert!(
        !wreck_slim
            .dict_get("name")
            .and_then(eve_proto::PyRep::as_str)
            .unwrap_or("")
            .is_empty(),
        "wreck slim item must have non-empty name (#728)"
    );

    let cap_slim = sys_handle
        .get_ball_slim_item(cap_id)
        .await
        .unwrap()
        .expect("capsule slim item");
    assert!(
        !cap_slim
            .dict_get("name")
            .and_then(eve_proto::PyRep::as_str)
            .unwrap_or("")
            .is_empty(),
        "ejected capsule slim item must have non-empty name (#728/#729)"
    );

    let (_tick, events, _rep) = sys_handle.drain_observer_updates(cap_id).await.unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, eve_destiny::DestinyEvent::SetState { .. })),
        "in-system pod ejection must not queue SetState (DoBallClear wipes overview celestials #970)"
    );
    assert!(
        !events.iter().any(|e| match e {
            eve_destiny::DestinyEvent::RemoveBall { ball_id } => *ball_id == ship_id,
            eve_destiny::DestinyEvent::RemoveBalls { ball_ids } => ball_ids.contains(&ship_id),
            _ => false,
        }),
        "death pipeline must defer RemoveBalls(old ship) out of immediate AddBalls2 batch (#1018)"
    );
    assert!(
        events.iter().any(|e| match e {
            eve_destiny::DestinyEvent::AddBalls2 { balls } => {
                balls.iter().any(|b| b.id == res.wreck_item_id) && balls.iter().any(|b| b.id == cap_id)
            }
            _ => false,
        }),
        "death pipeline must queue AddBalls2 with wreck and ejected capsule (#729/#970)"
    );

    // #1018, #1531: Stepping 2 ticks delivers deferred RemoveBalls([ship_id]) after ego switch.
    let mut saw_deferred_remove = false;
    for step_idx in 0..2 {
        let broadcast = sys_handle.tick().await.unwrap();
        let has_remove = broadcast.observer_events.get(&cap_id).is_some_and(|evs| {
            evs.iter().any(|e| {
                matches!(e, eve_destiny::DestinyEvent::RemoveBalls { ball_ids } if ball_ids == &[ship_id])
            })
        });
        if step_idx == 0 {
            assert!(!has_remove, "RemoveBalls must stay deferred on tick +1 (#1018)");
        } else {
            saw_deferred_remove = has_remove;
        }
    }
    assert!(
        saw_deferred_remove,
        "death pipeline must deliver deferred RemoveBalls([ship_id]) for destroyed ship on tick +2 (#970/#1018/#1531)"
    );

    // #731 / #770: Insurance payout records ref_types::INSURANCE with ship typeID as context_id
    // (V24 walletEntryFormat passes referenceID for entryTypeID=19 to evetypes.GetName)
    let has_insurance_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal.iter().any(|entry| {
            entry.character_id == victim
                && entry.amount == eve_db::IskCents::from_f64_isk(res.insurance_payout_isk)
                && snap
                    .wallet_journal_meta
                    .get(&entry.entry_id)
                    .map(|m| {
                        m.ref_type_id == eve_db::ledger::ref_types::INSURANCE
                            && m.context == eve_db::ledger::JournalContext::Type
                            && m.context_id == 587
                    })
                    .unwrap_or(false)
        })
    });
    assert!(
        has_insurance_journal,
        "insurance payout must persist wallet_journal_meta with ref_types::INSURANCE and ship type_id context (#731, #770)"
    );

    let mut victim_sess = eve_machonet::SessionState::new(9_770);
    victim_sess.character_id = Some(victim);
    victim_sess.user_id = Some(victim as u64);
    let tx_res = crate::services::wallet_ledger::handle(
        &ctx,
        &mut victim_sess,
        "account",
        "GetTransactions",
        &[],
        &std::collections::HashMap::new(),
    )
    .expect("account.GetTransactions handled")
    .expect("account.GetTransactions ok");
    let tx_rows = tx_res.crowset_rows().expect("expected CRowset from account.GetTransactions");
    assert!(
        tx_rows.iter().any(|r| {
            r.dbrow_get("entryTypeID").and_then(|v| v.as_i64())
                == Some(i64::from(eve_db::ledger::ref_types::INSURANCE))
                && r.dbrow_get("referenceID").and_then(|v| v.as_i64()) == Some(587)
        }),
        "account.GetTransactions must emit destroyed ship typeID (587) as referenceID for insurance entryTypeID=19 (#770)"
    );

    let notifs = ctx.drain_notifications_for_char(victim);
    assert!(
        notifs.iter().any(|n| n.event == "OnAccountChange"),
        "insurance payout must emit OnAccountChange (#731)"
    );
    assert!(
        notifs.iter().any(|n| n.event == "OnMailSent"),
        "insurance payout must emit OnMailSent mail notification (#731)"
    );
}

#[test]
fn descendant_check_drain_nested_and_resolve_ship_contents_cover_all_edges() {
    let ctx = test_ctx();
    let ship_id = 200_100_001;
    let esc_frig_id = 200_100_002;
    let cargo_frig_id = 200_100_003;
    let wrap_id = 200_100_004;
    let wrapped_item_id = 200_100_005;
    let container_id = 200_100_006;
    let nested_item_id = 200_100_007;
    let unknown_type_item_id = 200_100_008;
    let rig_item_id = 200_100_009;
    let self_loop_id = 200_100_010;

    // Register fittings on both nested frigates
    {
        let mut guard = ctx.ship_fittings.write().unwrap();
        let mut f_esc = eve_dogma::ShipFittingEngine::from_sde(esc_frig_id, 587, &ctx.sde);
        f_esc.modules.insert(
            200_100_101,
            eve_dogma::FittedModule {
                item_id: 200_100_101,
                type_id: 2048,
                flag_id: 11,
                state: eve_dogma::ModuleState::Online,
                damage_hp: 0.0,
                max_hp: 40.0,
                charge_type_id: None,
                charge_quantity: 0,
                dynamic_attribute_overrides: HashMap::new(),
            },
        );
        guard.insert(esc_frig_id, f_esc);

        let mut f_cargo = eve_dogma::ShipFittingEngine::from_sde(cargo_frig_id, 587, &ctx.sde);
        f_cargo.modules.insert(
            200_100_102,
            eve_dogma::FittedModule {
                item_id: 200_100_102,
                type_id: 2048,
                flag_id: 12,
                state: eve_dogma::ModuleState::Online,
                damage_hp: 0.0,
                max_hp: 40.0,
                charge_type_id: None,
                charge_quantity: 0,
                dynamic_attribute_overrides: HashMap::new(),
            },
        );
        guard.insert(cargo_frig_id, f_cargo);
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, 638, 90_000_001, 30_000_142, InventoryFlag::None, 1, 0.0),
        );
        snap.items.insert(
            esc_frig_id,
            make_item(
                esc_frig_id,
                587,
                90_000_001,
                ship_id,
                InventoryFlag::from_u16(177),
                1,
                0.0,
            ),
        );
        snap.items.insert(
            cargo_frig_id,
            make_item(cargo_frig_id, 587, 90_000_001, ship_id, InventoryFlag::Cargo, 1, 0.0),
        );
        snap.items.insert(
            self_loop_id,
            make_item(self_loop_id, 34, 90_000_001, self_loop_id, InventoryFlag::Cargo, 1, 0.0),
        );
        snap.items.insert(
            wrap_id,
            make_item(wrap_id, 3468, 90_000_001, ship_id, InventoryFlag::Cargo, 1, 0.0),
        );
        snap.items.insert(
            wrapped_item_id,
            make_item(wrapped_item_id, 34, 90_000_001, wrap_id, InventoryFlag::None, 10, 0.0),
        );
        snap.items.insert(
            container_id,
            make_item(container_id, 23, 90_000_001, ship_id, InventoryFlag::Cargo, 1, 0.0),
        );
        snap.items.insert(
            nested_item_id,
            make_item(nested_item_id, 34, 90_000_001, container_id, InventoryFlag::Cargo, 25, 0.0),
        );
        snap.items.insert(
            unknown_type_item_id,
            make_item(unknown_type_item_id, 999_999_888, 90_000_001, ship_id, InventoryFlag::Cargo, 3, 0.0),
        );
        snap.items.insert(
            rig_item_id,
            make_item(rig_item_id, 31_790, 90_000_001, ship_id, InventoryFlag::RigSlot0, 1, 0.0),
        );

        // Deep chain > 8 hops
        let mut prev = ship_id;
        for i in 0..10 {
            let nid = 200_100_200 + i;
            snap.items.insert(
                nid,
                make_item(nid, 23, 90_000_001, prev, InventoryFlag::Cargo, 1, 0.0),
            );
            prev = nid;
        }
        assert!(is_item_descendant_of_in_snap(snap, wrapped_item_id, ship_id));
        assert!(!is_item_descendant_of_in_snap(snap, self_loop_id, ship_id));
        assert!(!is_item_descendant_of_in_snap(snap, 999_999_999, ship_id));
        assert!(!is_item_descendant_of_in_snap(snap, 200_100_209, ship_id));
    });

    // drain_nested_ship_fittings with exclude_escape_bay = true drains only cargo_frig_id
    let drained_no_esc = drain_nested_ship_fittings(&ctx, ship_id, true);
    assert_eq!(drained_no_esc.len(), 1);
    assert_eq!(drained_no_esc[0].0, cargo_frig_id);
    // drain_nested_ship_fittings with exclude_escape_bay = false drains esc_frig_id too
    let drained_esc = drain_nested_ship_fittings(&ctx, ship_id, false);
    assert_eq!(drained_esc.len(), 1);
    assert_eq!(drained_esc[0].0, esc_frig_id);

    // resolve_destroyed_ship_contents_in_snap:
    // - wrap 3468 removed and wrapped_item_id forced to InventoryFlag::Cargo
    // - override_owner_id = Some(90_000_002)
    // - unknown_type_item_id contributes 3 * 100.0 = 300.0 ISK fallback
    // - rig_item_id destroyed even when roll_fn returns 0.0
    ctx.db.with_mut_snapshot(|snap| {
        let out = resolve_destroyed_ship_contents_in_snap(
            snap,
            &ctx.sde,
            ship_id,
            888_001,
            Some(90_000_002),
            Some(esc_frig_id),
            false,
            0.5,
            |i| matches!(i.flag as u16, 4 | 5 | 92..=94),
            |iid| if iid == container_id || iid == wrapped_item_id || iid == nested_item_id || iid == unknown_type_item_id {
                0.1
            } else {
                0.9
            },
        );
        assert!(!snap.items.contains_key(&wrap_id));
        assert_eq!(snap.items[&wrapped_item_id].location_id, 888_001);
        assert_eq!(snap.items[&wrapped_item_id].flag, InventoryFlag::Cargo);
        assert_eq!(snap.items[&wrapped_item_id].owner_id, 90_000_002);
        assert_eq!(snap.items[&nested_item_id].location_id, container_id);
        assert!(out.dropped_ids.contains(&wrapped_item_id));
        assert!(out.dropped_ids.contains(&container_id));
        assert!(out.dropped_ids.contains(&nested_item_id));
        assert!(out.destroyed_ids.contains(&rig_item_id));
        assert!(out.items_isk >= 300.0);

        // wreck_id == 0 forces everything left to be destroyed
        let out_no_wreck = resolve_destroyed_ship_contents_in_snap(
            snap,
            &ctx.sde,
            ship_id,
            0,
            None,
            None,
            false,
            1.0,
            |_| true,
            |_| 0.0,
        );
        assert!(out_no_wreck.dropped_ids.is_empty());
        assert!(!out_no_wreck.destroyed_ids.is_empty());
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn death_pipeline_escape_bay_pod_implants_insurance_kill_rights_and_structure_control() {
    let ctx = test_ctx();
    let sys_hi = 30_000_142_u32; // High-sec (1.0)
    let sys_null = 30_003_729_u32; // Null-sec (or fallback check)
    let victim = 90_000_001_i64;
    let attacker = 90_000_002_i64;
    let now = ctx.now_unix();

    // 1. Missing ship returns Err
    let missing_err =
        execute_death_pipeline(&ctx, sys_hi, victim, 999_888_777, attacker, DVec3::ZERO, 1)
            .await
            .unwrap_err();
    assert!(missing_err.contains("not found"));

    // 2. Battleship (638 Raven, group 27 -> has Frigate Escape Bay) with escape frigate (587, flag 177),
    //    active character-owned rt_contract, ship_fittings module, and plastic wrap (3468)
    let bs_id = 200_200_001_i64;
    let esc_frig = 200_200_002_i64;
    let wrap_id = 200_200_003_i64;
    let wrapped_trit = 200_200_004_i64;
    let fitted_mod_id = 200_200_005_i64;
    spawn_ship(&ctx, sys_hi, bs_id, victim, 638, DVec3::new(100.0, 0.0, 0.0), 0.0);
    {
        let mut guard = ctx.ship_fittings.write().unwrap();
        let mut fit = eve_dogma::ShipFittingEngine::from_sde(bs_id, 638, &ctx.sde);
        fit.modules.insert(
            fitted_mod_id,
            eve_dogma::FittedModule {
                item_id: fitted_mod_id,
                type_id: 2048,
                flag_id: 11,
                state: eve_dogma::ModuleState::Online,
                damage_hp: 0.0,
                max_hp: 40.0,
                charge_type_id: None,
                charge_quantity: 0,
                dynamic_attribute_overrides: HashMap::new(),
            },
        );
        guard.insert(bs_id, fit);
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim) {
            ch.security_status = 5.0;
        }
        if let Some(att) = snap.characters.get_mut(&attacker) {
            att.security_status = 5.0;
        }
        snap.items.insert(
            esc_frig,
            make_item(esc_frig, 587, victim, bs_id, InventoryFlag::from_u16(177), 1, 0.0),
        );
        snap.items.insert(
            wrap_id,
            make_item(wrap_id, 3468, victim, bs_id, InventoryFlag::Cargo, 1, 0.0),
        );
        snap.items.insert(
            wrapped_trit,
            make_item(wrapped_trit, 34, victim, wrap_id, InventoryFlag::None, 200, 0.0),
        );
    });
    let vic_bal_before = ctx.db.with_snapshot(|s| s.characters[&victim].balance);
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurance_contracts.insert(
            bs_id,
            eve_db::PersistedInsuranceContractRecord {
                ship_id: bs_id,
                ship_type_id: 638,
                owner_id: victim,
                level_name: "Platinum".into(),
                level_fraction: 1.0,
                cost_isk: 500_000.0,
                payout_isk: 1_234_567.0,
                start_unix: now.saturating_sub(100),
                end_unix: now + 1_000,
            },
        );
    });

    let bs_res = execute_death_pipeline(
        &ctx,
        sys_hi,
        victim,
        bs_id,
        attacker,
        DVec3::new(100.0, 0.0, 0.0),
        0, // exercises getrandom entropy path
    )
    .await
    .unwrap();
    assert_eq!(bs_res.ejected_capsule_id, Some(esc_frig));
    assert_eq!(bs_res.insurance_payout_isk, 1_234_567.0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&esc_frig].location_id, sys_hi as i64);
        assert_eq!(snap.items[&esc_frig].flag, InventoryFlag::None);
        assert_eq!(snap.characters[&victim].active_ship_id, esc_frig);
        assert!(!snap.items.contains_key(&wrap_id));
        assert_eq!(
            snap.characters[&victim].balance,
            vic_bal_before.checked_add(IskCents::from_f64_isk(1_234_567.0)).unwrap()
        );
        assert!(snap
            .wallet_journal
            .iter()
            .any(|j| j.character_id == victim && j.entry_type == "insurance_payout"));
        assert!(snap
            .mails
            .iter()
            .any(|m| m.recipient_id == victim && m.subject.contains(&bs_id.to_string())));
        // Illegal Highsec kill granted Kill Right and reduced attacker sec status
        assert!(snap
            .kill_rights
            .values()
            .any(|kr| kr.owner_id == victim && kr.target_id == attacker));
        assert!(snap.characters[&attacker].security_status < 5.0);
    });
    // Detailed killmail includes escape frigate entry with 0 dropped & 0 destroyed
    ctx.db.with_snapshot(|snap| {
        let dkm = &snap.combat_crimewatch_state.detailed_killmails[&bs_res.killmail_id];
        assert!(dkm
            .dropped_items
            .iter()
            .any(|i| i.type_id == 587 && i.qty_dropped == 0 && i.qty_destroyed == 0));
    });

    // 3. Pod death (type 670): wreck type 25, boosters & implants destroyed, zarzakh_gate_locks cleared, home station respawn
    let pod_id = 200_200_020_i64;
    let booster_id = 200_200_021_i64;
    let implant_id = 200_200_022_i64;
    spawn_ship(&ctx, sys_hi, pod_id, victim, 670, DVec3::new(200.0, 0.0, 0.0), 500_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            booster_id,
            make_item(booster_id, 28_672, victim, victim, InventoryFlag::Booster, 1, 0.0),
        );
        snap.items.insert(
            implant_id,
            make_item(implant_id, 9_899, victim, victim, InventoryFlag::Implant, 1, 0.0),
        );
        snap.active_implants.insert(victim, vec![9_899, 10_208]);
        snap.zarzakh_gate_locks.insert(victim, (40_000_001, now + 3600));
    });
    let pod_res = execute_death_pipeline_sync(&ctx, sys_hi, victim, pod_id, attacker, DVec3::ZERO, 77)
        .unwrap();
    assert_eq!(pod_res.wreck_type_id, 25);
    assert_eq!(pod_res.ejected_capsule_id, None);
    assert_eq!(pod_res.insurance_payout_isk, 0.0);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&booster_id));
        assert!(!snap.items.contains_key(&implant_id));
        assert!(!snap.active_implants.contains_key(&victim));
        assert!(!snap.zarzakh_gate_locks.contains_key(&victim));
    });
    ctx.db.with_snapshot(|snap| {
        let dkm = &snap.combat_crimewatch_state.detailed_killmails[&pod_res.killmail_id];
        assert!(dkm.destroyed_items.iter().any(|i| i.type_id == 9_899));
        assert!(dkm.destroyed_items.iter().any(|i| i.type_id == 10_208));
    });

    // 4. Expired contract falls back to 40% base quote; insured_payout > 0 pays directly when no contract;
    //    victim controlling structure suppresses capsule ejection; null-sec skips sec loss & kill rights
    let expired_cs_ship = 200_200_030_i64;
    spawn_ship(&ctx, sys_null, expired_cs_ship, victim, 587, DVec3::ZERO, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurance_contracts.insert(
            expired_cs_ship,
            eve_db::PersistedInsuranceContractRecord {
                ship_id: expired_cs_ship,
                ship_type_id: 587,
                owner_id: victim,
                level_name: "Gold".into(),
                level_fraction: 0.8,
                cost_isk: 80_000.0,
                payout_isk: 999_999.0,
                start_unix: now.saturating_sub(100),
                end_unix: now,
            },
        );
    });
    let base_587 = ctx.sde.types.get(&587).map(|t| t.base_price).unwrap_or(230_000.0);
    let expected_40 = compute_insurance_quote(base_587, 0.40).1;
    let exp_cs_res =
        execute_death_pipeline_sync(&ctx, sys_null, victim, expired_cs_ship, attacker, DVec3::ZERO, 1)
            .unwrap();
    assert!((exp_cs_res.insurance_payout_isk - expected_40).abs() < 1e-6);

    // Direct insured_payout_isk & structure control
    let direct_ship = 200_200_031_i64;
    let fake_struct_id = 200_200_032_i64;
    spawn_ship(&ctx, sys_hi, direct_ship, victim, 587, DVec3::ZERO, 333_333.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(fake_struct_id, Default::default());
        if let Some(ch) = snap.characters.get_mut(&victim) {
            ch.active_ship_id = fake_struct_id;
        }
    });
    // Mark victim suspect so kill is legal and sec loss / KR are skipped
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(victim).suspect_flag_until_unix = now + 900;
    }
    let att_sec_before = ctx.db.with_snapshot(|s| s.characters[&attacker].security_status);
    let dir_res =
        execute_death_pipeline_sync(&ctx, sys_hi, victim, direct_ship, attacker, DVec3::ZERO, 1)
            .unwrap();
    assert_eq!(dir_res.insurance_payout_isk, 333_333.0);
    assert_eq!(dir_res.ejected_capsule_id, None);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&attacker].security_status),
        att_sec_before
    );

    // 5. SelfDestructManager::abort_self_destruct(active) returns true, execute_self_destruct without assist credits self
    let mut mgr = SelfDestructManager::new();
    let sd_ship = 200_200_040_i64;
    spawn_ship(&ctx, sys_hi, sd_ship, victim, 587, DVec3::ZERO, 0.0);
    mgr.start_self_destruct(sd_ship, victim, sys_hi, now);
    assert!(mgr.abort_self_destruct(sd_ship));
    assert!(!mgr.is_self_destructing(sd_ship));

    let self_res = execute_self_destruct(&ctx, sys_hi, victim, sd_ship, DVec3::ZERO, 5)
        .await
        .unwrap();
    let self_km = ctx.db.with_snapshot(|s| s.killmails[&self_res.killmail_id].clone());
    assert_eq!(self_km.attacker_character_id, victim);

    // 6. execute_live_self_destruct_sync rejects missing ship, wrong system, structure_id, wrong active_ship_id
    assert!(execute_live_self_destruct_sync(&ctx, sys_hi, victim, 999_777_666, DVec3::ZERO, now)
        .unwrap()
        .is_none());
    let sd2 = 200_200_041_i64;
    spawn_ship(&ctx, sys_hi, sd2, victim, 587, DVec3::ZERO, 0.0);
    // Wrong location_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&sd2).unwrap().location_id = 30_000_144;
    });
    assert!(execute_live_self_destruct_sync(&ctx, sys_hi, victim, sd2, DVec3::ZERO, now)
        .unwrap()
        .is_none());
    // Docked in structure
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&sd2).unwrap().location_id = i64::from(sys_hi);
        let ch = snap.characters.get_mut(&victim).unwrap();
        ch.structure_id = Some(999_001);
        ch.active_ship_id = sd2;
    });
    assert!(execute_live_self_destruct_sync(&ctx, sys_hi, victim, sd2, DVec3::ZERO, now)
        .unwrap()
        .is_none());
    // Wrong active_ship_id
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&victim).unwrap();
        ch.structure_id = None;
        ch.active_ship_id = sd2 + 1;
    });
    assert!(execute_live_self_destruct_sync(&ctx, sys_hi, victim, sd2, DVec3::ZERO, now)
        .unwrap()
        .is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b3_destruction_pipeline_idempotency_unpiloted_hull_kills_and_sqlite_reload() {
    use eve_machonet::SessionState;
    use eve_proto::PyRep;

    let db_path = std::env::temp_dir().join(format!(
        "b3_destruction_{}_{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&db_path);
    let sde = {
        let base_ctx = test_ctx();
        Arc::clone(&base_ctx.sde)
    };
    let repo = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
    repo.with_mut_snapshot(|snap| {
        *snap = eve_db::WorldRepository::create_demo_snapshot();
    });
    let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), repo)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(
        1_800_500_000,
    )));

    let sys_id = 30_000_142_u32;
    let victim = 90_000_001_i64;
    let attacker = 90_000_002_i64;
    let now = ctx.now_unix();

    // 1. Idempotent death transaction + no module resurrection (`#918` AC 1 & AC 4, `#858`):
    //    Two destroy requests for one ship produce 1 wreck, 1 capsule, 1 killmail, 1 insurance payout,
    //    and a stale cached module in `ctx.ship_fittings` that was removed from `snap.items` is never resurrected.
    let dup_ship = 300_100_001_i64;
    let ghost_mod = 300_100_002_i64;
    let real_cargo = 300_100_003_i64;
    spawn_ship(
        &ctx,
        sys_id,
        dup_ship,
        victim,
        587,
        DVec3::new(1_000.0, 0.0, 0.0),
        400_000.0,
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            real_cargo,
            make_item(real_cargo, 34, victim, dup_ship, InventoryFlag::Cargo, 50, 0.0),
        );
    });
    {
        let mut guard = ctx.ship_fittings.write().unwrap();
        let mut fit = eve_dogma::ShipFittingEngine::from_sde(dup_ship, 587, &ctx.sde);
        fit.modules.insert(
            ghost_mod,
            eve_dogma::FittedModule {
                item_id: ghost_mod,
                type_id: 2048,
                flag_id: 11,
                state: eve_dogma::ModuleState::Online,
                damage_hp: 0.0,
                max_hp: 40.0,
                charge_type_id: None,
                charge_quantity: 0,
                dynamic_attribute_overrides: HashMap::new(),
            },
        );
        guard.insert(dup_ship, fit);
    }
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&victim].balance);
    let km_count_before = ctx.db.with_snapshot(|s| s.killmails.len());

    let first_kill = on_ball_destroyed_sync(
        &ctx,
        sys_id,
        dup_ship,
        Some(attacker),
        DVec3::new(1_000.0, 0.0, 0.0),
    )
    .expect("first destroy succeeds")
    .expect("ship death result");
    let first_cap_id = first_kill.ejected_capsule_id.expect("piloted ship ejects capsule");
    assert_eq!(first_kill.insurance_payout_isk, 400_000.0);
    assert!(!first_kill.loot_dropped_item_ids.contains(&ghost_mod));
    assert!(!first_kill.loot_destroyed_item_ids.contains(&ghost_mod));

    // Second destroy request for the same ship must fail without creating a second wreck/killmail/payout
    let second_kill = execute_death_pipeline_sync(
        &ctx,
        sys_id,
        victim,
        dup_ship,
        attacker,
        DVec3::new(1_000.0, 0.0, 0.0),
        now,
    );
    assert!(second_kill.is_err(), "duplicate destroy must return Err");

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&dup_ship));
        assert!(!snap.items.contains_key(&ghost_mod));
        assert_eq!(snap.characters[&victim].active_ship_id, first_cap_id);
        assert_eq!(
            snap.characters[&victim].balance,
            bal_before
                .checked_add(IskCents::from_f64_isk(400_000.0))
                .unwrap()
        );
        assert_eq!(snap.killmails.len(), km_count_before + 1);
    });

    // Reload from SQLite (`#918` AC 4): destroyed ship is absent, and exactly one wreck and killmail exist
    {
        let reloaded = eve_db::WorldRepository::open_sqlite(&db_path).expect("reload sqlite");
        reloaded.with_snapshot(|snap| {
            assert!(!snap.items.contains_key(&dup_ship));
            assert!(!snap.item_positions.contains_key(&dup_ship));
            assert!(snap.items.contains_key(&first_kill.wreck_item_id));
            assert!(snap.killmails.contains_key(&first_kill.killmail_id));
            assert_eq!(snap.killmails.len(), km_count_before + 1);
        });
    }

    // 2. Eject at low hull + unpiloted ship destruction (`#918` AC 2 & AC 3, `#835`, `T-3`):
    //    Board a ship in space, eject via `ship.Eject`, verify `snap.item_positions` has the abandoned hull's
    //    position immediately, restart from SQLite, verify actor boot restores the unpiloted hull ball and
    //    `ship.Board` re-boards it, then eject again and destroy the unpiloted hull across all 3 combat paths!
    let eject_ship = 300_200_001_i64;
    let eject_cargo = 300_200_002_i64;
    let eject_pos = DVec3::new(5_000.0, 2_000.0, -1_000.0);
    spawn_ship(&ctx, sys_id, eject_ship, victim, 587, eject_pos, 250_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            eject_cargo,
            make_item(eject_cargo, 34, victim, eject_ship, InventoryFlag::Cargo, 100, 0.0),
        );
    });

    let mut sess = SessionState::new(701);
    sess.user_id = Some(1);
    sess.character_id = Some(victim);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(eject_ship);

    let kw = HashMap::new();
    crate::router::session_transition::clear_all_session_change_timers(&ctx);
    let eject_resp = ctx
        .dispatch_rpc(&mut sess, "ship", None, "Eject", &[], &kw, None)
        .await
        .expect("ship.Eject succeeds");
    let ejected_pod_id = match eject_resp {
        PyRep::Integer(cid) => cid,
        PyRep::KeyVal(kv) => kv
            .iter()
            .find_map(|(k, v)| (k == "capsuleID" || k == "shipID").then_some(v).and_then(|v| v.as_i64()))
            .expect("capsuleID in ship.Eject KeyVal"),
        other => panic!("expected capsule id from ship.Eject, got {other:?}"),
    };
    assert_ne!(ejected_pod_id, eject_ship);
    assert_eq!(sess.ship_id, Some(ejected_pod_id));

    // Verify `T-3`: `snap.item_positions` persisted both `eject_ship` and `ejected_pod_id` inside the eject transaction
    let saved_hull_pos = ctx
        .db
        .with_snapshot(|s| s.item_positions.get(&eject_ship).copied())
        .expect("abandoned hull position saved in snap.item_positions");
    assert_eq!(DVec3::from_array(saved_hull_pos), eject_pos);

    // Simulate server restart from SQLite and verify actor boot restores the abandoned hull and allows `ship.Board` (`#918` AC 3)
    let restarted_repo =
        Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let restarted_ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), restarted_repo)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let restarted_sys = restarted_ctx.systems.get_or_spawn(sys_id);
    let restored_hull_ball = restarted_sys
        .get_ball_snapshot(eject_ship)
        .await
        .expect("query ball")
        .expect("abandoned hull restored on actor boot");
    assert_eq!(restored_hull_ball.1.type_id, 587);
    assert_eq!(restored_hull_ball.1.position, eject_pos);

    let mut restarted_sess = SessionState::new(702);
    restarted_sess.user_id = Some(1);
    restarted_sess.character_id = Some(victim);
    restarted_sess.solar_system_id = Some(sys_id);
    restarted_sess.ship_id = Some(ejected_pod_id);

    crate::router::session_transition::clear_all_session_change_timers(&restarted_ctx);
    restarted_ctx
        .dispatch_rpc(
            &mut restarted_sess,
            "ship",
            None,
            "Board",
            &[PyRep::Integer(eject_ship)],
            &kw,
            None,
        )
        .await
        .expect("ship.Board on restored abandoned hull after restart succeeds");
    assert_eq!(restarted_sess.ship_id, Some(eject_ship));

    // Eject again on `restarted_ctx` so `eject_ship` is unpiloted in space, then destroy it via `on_ball_destroyed`
    crate::router::session_transition::clear_all_session_change_timers(&restarted_ctx);
    let _ = restarted_ctx
        .dispatch_rpc(&mut restarted_sess, "ship", None, "Eject", &[], &kw, None)
        .await
        .expect("second ship.Eject succeeds");
    let pod_after_second_eject = restarted_sess.ship_id.expect("pod id");
    assert_ne!(pod_after_second_eject, eject_ship);

    let unpiloted_kill = on_ball_destroyed(&restarted_ctx, sys_id, eject_ship, Some(attacker), eject_pos)
        .await
        .expect("unpiloted hull kill succeeds")
        .expect("unpiloted hull produces DeathPipelineResult");
    assert_eq!(
        unpiloted_kill.ejected_capsule_id, None,
        "unpiloted hull kill must NOT spawn a capsule"
    );
    assert!(unpiloted_kill.wreck_item_id > 0);
    assert!(unpiloted_kill.killmail_id > 0);
    assert_eq!(unpiloted_kill.insurance_payout_isk, 250_000.0);
    restarted_ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&eject_ship));
        assert_eq!(
            snap.characters[&victim].active_ship_id, pod_after_second_eject,
            "owner active_ship_id must remain their existing pod"
        );
        assert_eq!(
            snap.killmails[&unpiloted_kill.killmail_id].victim_character_id,
            victim
        );
    });

    // Also verify missile/AoE (`handle_destroyed_ball_from_missile_or_aoe`) and NPC AI (`handle_npc_ai_ball_destructions`)
    // on unpiloted ships produce wrecks & killmails without transitioning the owner!
    let unpiloted_missile_ship = 300_200_010_i64;
    let unpiloted_npc_ship = 300_200_011_i64;
    restarted_ctx.db.with_mut_snapshot(|snap| {
        for sid in [unpiloted_missile_ship, unpiloted_npc_ship] {
            snap.items.insert(
                sid,
                make_item(sid, 587, victim, sys_id as i64, InventoryFlag::None, 1, 0.0),
            );
            snap.item_positions.insert(sid, [eject_pos.x, eject_pos.y, eject_pos.z]);
        }
    });
    let kms_before = restarted_ctx.db.with_snapshot(|s| s.killmails.len());
    crate::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
        &restarted_ctx,
        sys_id,
        attacker,
        unpiloted_missile_ship,
        587,
        eject_pos,
        now,
    )
    .await;
    crate::combat::npc_ai::handle_npc_ai_ball_destructions(
        &restarted_ctx,
        sys_id,
        vec![(
            1_895_000_001,
            unpiloted_npc_ship,
            Ball {
                id: unpiloted_npc_ship,
                type_id: 587,
                position: eject_pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )],
    );
    restarted_ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&unpiloted_missile_ship));
        assert!(!snap.items.contains_key(&unpiloted_npc_ship));
        assert_eq!(snap.killmails.len(), kms_before + 2);
        assert_eq!(
            snap.characters[&victim].active_ship_id,
            pod_after_second_eject
        );
    });
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn issue_1008_1020_destroyed_player_drone_emits_clear_notifications() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let drone_id = 1_897_065_269_i64;
    let drone_type = 2_454_u32; // Hobgoblin I

    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid, 501, Some(char_id), false, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(char_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            drone_id,
            InventoryItemRecord {
                item_id: drone_id,
                type_id: drone_type,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let drone_ball = Ball {
        id: drone_id,
        type_id: drone_type,
        position: DVec3::new(3_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 2_600.0,
        inertia: 0.4,
        mass: 5_000.0,
        radius: 5.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle.spawn_ball(drone_ball.clone()).await;
    crate::services::drone_test_helpers::seed_active_drone(
        &ctx,
        crate::services::combat_crimewatch::DroneSpaceState {
            ball_id: drone_id,
            drone_item_id: drone_id,
            type_id: drone_type,
            group_id: 100,
            owner_char_id: char_id,
            original_owner_char_id: char_id,
            parent_ship_id: ship_id,
            solar_system_id: sys_id,
            target_id: Some(1_897_065_293),
            activity_state: 1,
            assist_char_id: None,
            guard_char_id: None,
            is_abandoned: false,
            is_sentry: false,
            bandwidth_used: 5.0,
            max_velocity: 2_600.0,
            optimal_range: 1_500.0,
            damage_vector: eve_destiny::DamageVector::default(),
            shield_repair_amount: 0.0,
            armor_repair_amount: 0.0,
            hull_repair_amount: 0.0,
            web_speed_factor: 0.0,
            neut_amount: 0.0,
            ecm_strength: 0.0,
            tp_sig_bonus_pct: 0.0,
        },
    );

    crate::combat::npc_ai::handle_npc_ai_ball_destructions(
        &ctx,
        sys_id,
        vec![(1_897_065_293, drone_id, drone_ball)],
    );

    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.items.contains_key(&drone_id),
            "destroyed drone item must be removed from snapshot"
        );
        assert!(snap
            .combat_crimewatch_state
            .is_recently_destroyed_item_id(drone_id));
    });
    assert!(!crate::services::drone_test_helpers::has_active_drone(&ctx, drone_id));
    assert!(
        sys_handle.get_ball_snapshot(drone_id).await.unwrap().is_none(),
        "destroyed drone ball must be removed from Ballpark"
    );

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnDroneStateChange"
                && n.args.first().and_then(|v| v.as_i64()) == Some(drone_id)
                && n.args.get(1).and_then(|v| v.as_i64()) == Some(0)
                && n.args.get(2).and_then(|v| v.as_i64()) == Some(0)
                && n.args.get(3).and_then(|v| v.as_i64()) == Some(0)
        }),
        "destroyed drone must emit OnDroneStateChange(0, 0, 0) to clear michelle.stateByDroneID"
    );
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnDroneControlLost"
                && n.args.first().and_then(|v| v.as_i64()) == Some(drone_id)
        }),
        "destroyed drone must emit OnDroneControlLost"
    );
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnDroneActivityChange"
                && n.args.first().and_then(|v| v.as_i64()) == Some(drone_id)
                && n.args.get(1).and_then(|v| v.as_i64()) == Some(0)
        }),
        "destroyed drone must clear activity via OnDroneActivityChange"
    );
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnItemChange"
        }),
        "destroyed drone must emit tombstone OnItemChange"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unpiloted_capsule_destruction_preserves_owner_implants_ship_and_location() {
    let ctx = test_ctx();
    let sys_id = 30_002_813_u32; // Low-sec system distinct from Jita home station (30_000_142 / 60_003_760)
    let victim = 90_000_001_i64;
    let attacker = 90_000_002_i64;
    let home_station_id = 60_003_760_i64;
    let now = ctx.now_unix();

    let piloted_ship_id = 400_100_001_i64;
    let unpiloted_pod_id = 400_100_002_i64;
    let implant_id = 400_100_003_i64;
    let booster_id = 400_100_004_i64;
    let pod_pos = DVec3::new(12_000.0, -500.0, 2_000.0);

    // Owner is piloting another ship in space, with implants, boosters, and a Zarzakh gate lock
    spawn_ship(&ctx, sys_id, piloted_ship_id, victim, 587, DVec3::ZERO, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim) {
            ch.home_station_id = home_station_id;
        }
        snap.items.insert(
            unpiloted_pod_id,
            make_item(
                unpiloted_pod_id,
                670,
                victim,
                sys_id as i64,
                InventoryFlag::None,
                1,
                0.0,
            ),
        );
        snap.item_positions
            .insert(unpiloted_pod_id, [pod_pos.x, pod_pos.y, pod_pos.z]);
        snap.active_implants.insert(victim, vec![9_899, 10_208]);
        snap.items.insert(
            implant_id,
            make_item(implant_id, 9_899, victim, victim, InventoryFlag::Implant, 1, 0.0),
        );
        snap.items.insert(
            booster_id,
            make_item(booster_id, 28_672, victim, victim, InventoryFlag::Booster, 1, 0.0),
        );
        snap.zarzakh_gate_locks.insert(victim, (40_000_001, now + 3_600));
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: unpiloted_pod_id,
            type_id: 670,
            position: pod_pos,
            velocity: DVec3::ZERO,
            max_velocity: 335.0,
            inertia: 0.25,
            mass: 16_000.0,
            radius: 15.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        ctx.build_type_combat_state(670),
    );
    let _ = ctx.drain_notifications_for_char(victim);

    let res = on_ball_destroyed_sync(&ctx, sys_id, unpiloted_pod_id, Some(attacker), pod_pos)
        .expect("unpiloted capsule destruction succeeds")
        .expect("unpiloted capsule produces DeathPipelineResult");
    assert_eq!(res.wreck_type_id, 25);
    assert_eq!(res.ejected_capsule_id, None);
    assert_eq!(res.insurance_payout_isk, 0.0);

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&unpiloted_pod_id));
        assert!(!snap.item_positions.contains_key(&unpiloted_pod_id));
        assert!(snap.items.contains_key(&res.wreck_item_id));
        assert!(snap.killmails.contains_key(&res.killmail_id));

        let ch = &snap.characters[&victim];
        assert_eq!(
            ch.active_ship_id, piloted_ship_id,
            "destroying an unpiloted capsule must NOT overwrite owner's active_ship_id"
        );
        assert_eq!(ch.solar_system_id, sys_id);
        assert_eq!(ch.station_id, None);
        assert_eq!(ch.structure_id, None);

        assert_eq!(
            snap.active_implants.get(&victim),
            Some(&vec![9_899, 10_208]),
            "destroying an unpiloted capsule must NOT strip owner's active_implants"
        );
        assert!(snap.items.contains_key(&implant_id));
        assert!(snap.items.contains_key(&booster_id));
        assert!(snap.zarzakh_gate_locks.contains_key(&victim));
    });

    ctx.db.with_snapshot(|snap| {
        let dkm = &snap.combat_crimewatch_state.detailed_killmails[&res.killmail_id];
        assert!(
            !dkm
                .destroyed_items
                .iter()
                .any(|i| i.type_id == 9_899 || i.type_id == 10_208),
            "unpiloted capsule killmail must NOT list owner's implants"
        );
    });
    assert!(
        crate::services::space_destiny::get_session_change_timer(&ctx, victim).is_none(),
        "destroying an unpiloted capsule must NOT trigger a podding session-change timer"
    );
    let notifs = ctx.drain_notifications_for_char(victim);
    assert!(
        !notifs.iter().any(|n| matches!(
            n.event.as_str(),
            "OnPodDeath" | "OnPlayerPodDeath" | "OnPodKilled" | "OnPlayerPodRespawn"
        )),
        "destroying an unpiloted capsule must NOT emit pod death/respawn notifications"
    );

    // Subcase B: Owner docked in a non-home station also keeps implants and stays docked when an unpiloted capsule is destroyed
    let docked_station_id = 60_003_761_i64;
    let docked_pod_id = 400_100_005_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim) {
            ch.station_id = Some(docked_station_id);
            ch.solar_system_id = 30_000_142;
            ch.active_ship_id = piloted_ship_id;
        }
        snap.items.insert(
            docked_pod_id,
            make_item(
                docked_pod_id,
                33_328,
                victim,
                sys_id as i64,
                InventoryFlag::None,
                1,
                0.0,
            ),
        );
    });
    let res2 = execute_death_pipeline_sync(
        &ctx,
        sys_id,
        victim,
        docked_pod_id,
        attacker,
        DVec3::ZERO,
        99,
    )
    .expect("second unpiloted capsule kill succeeds");
    assert_eq!(res2.wreck_type_id, 25);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&docked_pod_id));
        let ch = &snap.characters[&victim];
        assert_eq!(ch.active_ship_id, piloted_ship_id);
        assert_eq!(ch.station_id, Some(docked_station_id));
        assert_eq!(snap.active_implants.get(&victim), Some(&vec![9_899, 10_208]));
        assert!(snap.items.contains_key(&implant_id));
        assert!(snap.items.contains_key(&booster_id));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn piloted_pod_death_respawns_atomically_in_single_transaction_and_rolls_back_cleanly() {
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct CommitSnapshot {
        pod_exists: bool,
        active_ship_id: i64,
        active_ship_exists: bool,
        station_id: Option<i64>,
        solar_system_id: u32,
        implants_present: bool,
        zarzakh_present: bool,
        corpse_exists: bool,
    }

    // 1. Single-transaction atomicity: pod deletion + implant clearing + home-station respawn
    //    commit together in one DB transaction (never an intermediate dangling active_ship_id).
    let ctx = test_ctx();
    let sys_id = 30_002_813_u32; // Low-sec
    let home_station_id = 60_003_760_i64; // Jita 4-4 in 30_000_142
    let victim = 90_000_001_i64;
    let now = ctx.now_unix();

    let pod_id = 400_200_001_i64;
    let implant_id = 400_200_002_i64;
    let booster_id = 400_200_003_i64;
    spawn_ship(&ctx, sys_id, pod_id, victim, 670, DVec3::new(1_000.0, 0.0, 0.0), 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim) {
            ch.home_station_id = home_station_id;
        }
        snap.active_implants.insert(victim, vec![9_899, 10_208]);
        snap.items.insert(
            implant_id,
            make_item(implant_id, 9_899, victim, victim, InventoryFlag::Implant, 1, 0.0),
        );
        snap.items.insert(
            booster_id,
            make_item(booster_id, 28_672, victim, victim, InventoryFlag::Booster, 1, 0.0),
        );
        snap.zarzakh_gate_locks.insert(victim, (40_000_001, now + 3_600));
        snap.sp6_lifecycle.disconnected_ships.insert(
            victim,
            DisconnectedShipEntry {
                char_id: victim,
                ship_id: pod_id,
                ship_type_id: 670,
                solar_system_id: sys_id,
                disconnected_at_unix: now,
                remove_at_unix: now + 60,
                emergency_warp_dest: None,
                last_position: [1_000.0, 0.0, 0.0],
                last_velocity: [0.0, 0.0, 0.0],
                removed_from_space: false,
                pre_warp_position: None,
            },
        );
    });

    let commits = Arc::new(std::sync::Mutex::new(Vec::<CommitSnapshot>::new()));
    let commits_cb = Arc::clone(&commits);
    ctx.db.register_post_commit_listener(Arc::new(move |snap, _ch, _eff| {
        let ch = &snap.characters[&victim];
        let corpse_exists = snap.items.values().any(|i| i.type_id == 25 && i.owner_id == victim);
        commits_cb.lock().unwrap().push(CommitSnapshot {
            pod_exists: snap.items.contains_key(&pod_id),
            active_ship_id: ch.active_ship_id,
            active_ship_exists: snap.items.contains_key(&ch.active_ship_id),
            station_id: ch.station_id,
            solar_system_id: ch.solar_system_id,
            implants_present: snap.active_implants.contains_key(&victim),
            zarzakh_present: snap.zarzakh_gate_locks.contains_key(&victim),
            corpse_exists,
        });
    }));

    let pod_res = execute_death_pipeline_sync(&ctx, sys_id, victim, pod_id, 0, DVec3::ZERO, 77)
        .expect("piloted pod death succeeds");
    assert_eq!(pod_res.wreck_type_id, 25);
    assert_eq!(pod_res.ejected_capsule_id, None);

    let recorded = commits.lock().unwrap().clone();
    assert!(
        !recorded.is_empty(),
        "piloted pod death must record at least one post-commit snapshot"
    );
    let c0 = &recorded[0];
    assert!(
        recorded.iter().all(|c| c == c0),
        "piloted pod death must execute deletion, implant stripping, and home-station respawn in a single transaction without intermediate death-state mutations: {recorded:?}"
    );
    assert!(!c0.pod_exists);
    assert_ne!(c0.active_ship_id, pod_id);
    assert!(
        c0.active_ship_exists,
        "committed active_ship_id must immediately point to the existing/spawned home-station capsule"
    );
    assert_eq!(c0.station_id, Some(home_station_id));
    assert_eq!(c0.solar_system_id, 30_000_142);
    assert!(!c0.implants_present);
    assert!(!c0.zarzakh_present);
    assert!(c0.corpse_exists);

    // 2. Commit failure (FailAtCommit) rolls back the entire single transaction and preserves pre-death state
    let rb_ctx = test_ctx();
    let rb_pod = 400_300_001_i64;
    let rb_imp = 400_300_002_i64;
    let rb_bst = 400_300_003_i64;
    spawn_ship(&rb_ctx, sys_id, rb_pod, victim, 670, DVec3::ZERO, 0.0);
    rb_ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim) {
            ch.home_station_id = home_station_id;
        }
        snap.active_implants.insert(victim, vec![9_899]);
        snap.items.insert(
            rb_imp,
            make_item(rb_imp, 9_899, victim, victim, InventoryFlag::Implant, 1, 0.0),
        );
        snap.items.insert(
            rb_bst,
            make_item(rb_bst, 28_672, victim, victim, InventoryFlag::Booster, 1, 0.0),
        );
        snap.zarzakh_gate_locks.insert(victim, (40_000_001, now + 3_600));
    });
    let kms_before = rb_ctx.db.with_snapshot(|s| s.killmails.len());
    rb_ctx.db.set_fault_plan_for_test(eve_db::TxFaultPlan::FailAtCommit);

    let fail_res =
        execute_death_pipeline_sync(&rb_ctx, sys_id, victim, rb_pod, 0, DVec3::ZERO, 77);
    assert!(fail_res.is_err(), "FailAtCommit must fail the death pipeline");
    rb_ctx.db.with_snapshot(|snap| {
        assert!(snap.items.contains_key(&rb_pod), "pod item preserved on rollback");
        assert!(snap.items.contains_key(&rb_imp), "implant item preserved on rollback");
        assert!(snap.items.contains_key(&rb_bst), "booster item preserved on rollback");
        assert_eq!(snap.active_implants.get(&victim), Some(&vec![9_899]));
        assert!(snap.zarzakh_gate_locks.contains_key(&victim));
        assert_eq!(snap.characters[&victim].active_ship_id, rb_pod);
        assert_eq!(snap.characters[&victim].solar_system_id, sys_id);
        assert_eq!(snap.characters[&victim].station_id, None);
        assert_eq!(snap.killmails.len(), kms_before);
        assert!(!snap.items.values().any(|i| i.type_id == 25 && i.owner_id == victim));
    });

    // 3. Mid-transaction persistence abort (FailNthStatement) after partial SQLite writes
    //    rolls back corpse creation, pod deletion, and implant removal cleanly and emits 0 post-commit callbacks.
    let rb_commits = Arc::new(std::sync::Mutex::new(Vec::<CommitSnapshot>::new()));
    let rb_commits_cb = Arc::clone(&rb_commits);
    rb_ctx.db.register_post_commit_listener(Arc::new(move |snap, _ch, _eff| {
        if let Some(ch) = snap.characters.get(&victim) {
            rb_commits_cb.lock().unwrap().push(CommitSnapshot {
                pod_exists: snap.items.contains_key(&rb_pod),
                active_ship_id: ch.active_ship_id,
                active_ship_exists: snap.items.contains_key(&ch.active_ship_id),
                station_id: ch.station_id,
                solar_system_id: ch.solar_system_id,
                implants_present: snap.active_implants.contains_key(&victim),
                zarzakh_present: snap.zarzakh_gate_locks.contains_key(&victim),
                corpse_exists: snap.items.values().any(|i| i.type_id == 25 && i.owner_id == victim),
            });
        }
    }));
    rb_ctx.db.set_fault_plan_for_test(eve_db::TxFaultPlan::FailNthStatement(2));
    let abort_err =
        execute_death_pipeline_sync(&rb_ctx, sys_id, victim, rb_pod, 0, DVec3::ZERO, 77)
            .unwrap_err();
    assert!(
        abort_err.contains("death transaction failed"),
        "expected FailNthStatement abort error, got: {abort_err}"
    );
    assert!(
        rb_commits.lock().unwrap().is_empty(),
        "aborted transaction must not trigger post-commit callbacks"
    );
    rb_ctx.db.with_snapshot(|snap| {
        assert!(snap.items.contains_key(&rb_pod));
        assert!(snap.items.contains_key(&rb_imp));
        assert!(snap.items.contains_key(&rb_bst));
        assert_eq!(snap.active_implants.get(&victim), Some(&vec![9_899]));
        assert!(snap.zarzakh_gate_locks.contains_key(&victim));
        assert_eq!(snap.characters[&victim].active_ship_id, rb_pod);
        assert_eq!(snap.characters[&victim].solar_system_id, sys_id);
        assert_eq!(snap.characters[&victim].station_id, None);
        assert_eq!(snap.killmails.len(), kms_before);
        assert!(!snap.items.values().any(|i| i.type_id == 25 && i.owner_id == victim));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn piloted_ship_destruction_clears_modules_runtime_state_and_emits_tombstones() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let victim = 90_000_001_i64;
    let attacker = 90_000_002_i64;
    let ship_id = 400_300_001_i64;
    let cycled_mod_id = 400_300_011_i64;
    let uncycled_active_mod_id = 400_300_012_i64;
    let offline_mod_id = 400_300_013_i64;
    let cargo_item_id = 400_300_014_i64;
    let now = ctx.now_unix();

    let victim_uid = ctx.db.with_snapshot(|s| s.characters[&victim].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(victim_uid, 701, Some(victim), false, now);
    spawn_ship(&ctx, sys_id, ship_id, victim, 587, DVec3::ZERO, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cycled_mod_id,
            make_item(
                cycled_mod_id,
                2048,
                victim,
                ship_id,
                InventoryFlag::from_u16(11),
                1,
                0.0,
            ),
        );
        snap.items.insert(
            uncycled_active_mod_id,
            make_item(
                uncycled_active_mod_id,
                2048,
                victim,
                ship_id,
                InventoryFlag::from_u16(12),
                1,
                0.0,
            ),
        );
        snap.items.insert(
            offline_mod_id,
            make_item(
                offline_mod_id,
                2048,
                victim,
                ship_id,
                InventoryFlag::from_u16(13),
                1,
                0.0,
            ),
        );
        snap.items.insert(
            cargo_item_id,
            make_item(
                cargo_item_id,
                232,
                victim,
                ship_id,
                InventoryFlag::Cargo,
                10,
                0.0,
            ),
        );
        snap.module_active_states.insert(cycled_mod_id);
        snap.module_active_states.insert(uncycled_active_mod_id);
        snap.module_active_charges.insert(cycled_mod_id, 232);
        snap.module_active_charges.insert(uncycled_active_mod_id, 232);
        snap.module_active_charges.insert(offline_mod_id, 232);
        snap.transient_item_attributes
            .entry(cycled_mod_id)
            .or_default()
            .insert(267, 0.75);
        snap.transient_item_attributes
            .entry(uncycled_active_mod_id)
            .or_default()
            .insert(267, 0.5);
        snap.transient_item_attributes
            .entry(offline_mod_id)
            .or_default()
            .insert(1176, 0.25);
        snap.transient_item_attributes
            .entry(ship_id)
            .or_default()
            .insert(267, 0.9);
        snap.sp6_lifecycle.self_destructs.insert(
            ship_id,
            eve_db::SelfDestructPersistedEntry {
                ship_id,
                char_id: victim,
                solar_system_id: sys_id,
                started_unix: now,
                detonate_at_unix: now + 120,
            },
        );
    });
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.self_destruct_countdowns.insert(
            ship_id,
            crate::services::combat_crimewatch::SelfDestructCountdown {
                ship_id,
                char_id: victim,
                solar_system_id: sys_id,
                started_unix: now,
                detonate_at_unix: now + 120,
            },
        );
    });
    {
        let mut fittings = ctx.ship_fittings.write().unwrap();
        fittings.insert(
            ship_id,
            eve_dogma::ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde),
        );
    }
    {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        sys_handle
            .test_with_actor_state_mut_async(move |st| {
                st.active_module_cycles
                    .push(crate::system_actor::ActiveModuleCycle {
                        ship_id,
                        module_id: cycled_mod_id,
                        module_type_id: 2048,
                        owner_char_id: Some(victim),
                        effect_name: "online".to_string(),
                        duration_s: 10.0,
                        ..Default::default()
                    });
            })
            .await;
    }
    let _ = ctx.drain_notifications_for_char(victim);

    let res = on_ball_destroyed_sync(&ctx, sys_id, ship_id, Some(attacker), DVec3::ZERO)
        .expect("piloted ship destruction succeeds")
        .expect("piloted ship destruction produces DeathPipelineResult");
    assert!(res.ejected_capsule_id.is_some());

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&ship_id));
        for id in [cycled_mod_id, uncycled_active_mod_id, offline_mod_id] {
            assert!(!snap.module_active_states.contains(&id));
            assert!(!snap.module_active_charges.contains_key(&id));
            assert!(!snap.transient_item_attributes.contains_key(&id));
        }
        assert!(!snap.transient_item_attributes.contains_key(&ship_id));
        assert!(!snap.sp6_lifecycle.self_destructs.contains_key(&ship_id));
        for id in [
            cycled_mod_id,
            uncycled_active_mod_id,
            offline_mod_id,
            cargo_item_id,
        ] {
            if let Some(surviving) = snap.items.get(&id) {
                assert_ne!(
                    surviving.location_id, ship_id,
                    "surviving child item {id} must be moved out of destroyed ship {ship_id}"
                );
            }
        }
    });
    crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        assert!(!rt.self_destruct_countdowns.contains_key(&ship_id));
    });
    assert!(!ctx.ship_fittings.read().unwrap().contains_key(&ship_id));
    assert!(!ctx
        .systems
        .get_or_spawn(sys_id)
        .actor_state
        .read()
        .unwrap()
        .active_module_cycles
        .iter()
        .any(|c| c.ship_id == ship_id));

    let notifs = ctx.drain_notifications_for_char(victim);
    for active_mid in [cycled_mod_id, uncycled_active_mod_id] {
        let stop_count = notifs
            .iter()
            .filter(|n| {
                n.event == "OnGodmaShipEffect"
                    && n.args.first().and_then(|v| v.as_i64()) == Some(active_mid)
                    && n.args.get(3).and_then(|v| v.as_i64()) == Some(0)
            })
            .count();
        assert_eq!(
            stop_count, 1,
            "module {active_mid} must emit exactly one OnGodmaShipEffect stop notification"
        );
    }

    assert!(
        !notifs.iter().any(|n| n.event == "OnItemChange"),
        "ship destruction is a scene/ship transition and must emit zero OnItemChange notifications (#931): {notifs:?}"
    );
}

#[test]
fn transient_items_materialize_only_on_surviving_loot_drop() {
    // parity: feat:serverops.persistence
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let npc_ship_id = 200_880_001_i64;
    let drop_mod_id = 200_880_002_i64;
    let destroy_mod_id = 200_880_003_i64;
    let wreck_id = 200_880_004_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mk = |item_id: i64, location_id: i64, flag: InventoryFlag| InventoryItemRecord {
            item_id,
            type_id: 587,
            owner_id: 1_000_180,
            location_id,
            flag,
            quantity: 1,
            singleton: true,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        };
        snap.insert_transient_item(mk(npc_ship_id, i64::from(sys_id), InventoryFlag::None));
        snap.insert_transient_item(mk(drop_mod_id, npc_ship_id, InventoryFlag::Cargo));
        snap.insert_transient_item(mk(destroy_mod_id, npc_ship_id, InventoryFlag::Cargo));

        let out = resolve_destroyed_ship_contents_in_snap(
            snap,
            &ctx.sde,
            npc_ship_id,
            wreck_id,
            Some(90_000_001),
            None,
            false,
            0.5,
            |_| true,
            |iid| if iid == drop_mod_id { 0.1 } else { 0.9 },
        );

        assert!(out.dropped_ids.contains(&drop_mod_id));
        assert!(out.destroyed_ids.contains(&destroy_mod_id));
        assert!(!snap.is_transient_item(drop_mod_id));
        assert!(snap.items.contains_key(&drop_mod_id), "surviving drop must materialize into persistent items");
        assert_eq!(snap.items[&drop_mod_id].location_id, wreck_id);
        assert_eq!(snap.items[&drop_mod_id].owner_id, 90_000_001);
        assert!(!snap.is_transient_item(destroy_mod_id));
        assert!(!snap.items.contains_key(&destroy_mod_id), "destroyed transient item must never persist");
    });
}

#[tokio::test]
async fn death_pipeline_insurance_payout_rolls_back_atomically_on_tx_err_and_panic_then_retries_once() {
    struct FaultResetGuard;
    impl Drop for FaultResetGuard {
        fn drop(&mut self) {
            set_post_insurance_tx_fault(None);
        }
    }

    let _guard = FaultResetGuard;
    let ctx = test_ctx();
    let sys_id = 30_002_813; // Low-sec
    let victim = 90_000_001;
    let attacker = 90_000_002;
    let ship_id = 200_991_001_i64;
    let now = ctx.now_unix();

    spawn_ship(&ctx, sys_id, ship_id, victim, 587, DVec3::ZERO, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurance_contracts.insert(
            ship_id,
            eve_db::PersistedInsuranceContractRecord {
                ship_id,
                ship_type_id: 587,
                owner_id: victim,
                level_name: "Platinum".into(),
                level_fraction: 1.0,
                cost_isk: 200_000.0,
                payout_isk: 850_000.0,
                start_unix: now.saturating_sub(100),
                end_unix: now + 3_600,
            },
        );
    });

    let (bal_before, journal_before, mails_before, km_before) = ctx.db.with_snapshot(|snap| {
        (
            snap.characters[&victim].balance,
            snap.wallet_journal.len(),
            snap.mails.len(),
            snap.killmails.len(),
        )
    });

    // 1. Mid-tx Err after insurance settlement rolls back wallet, contract, journal, mail, item, and idempotency key.
    set_post_insurance_tx_fault(Some(PostInsuranceTxFault::Err));
    let err_res = execute_death_pipeline_sync(&ctx, sys_id, victim, ship_id, attacker, DVec3::ZERO, 1);
    assert!(err_res.is_err(), "injected mid-tx error must abort death pipeline");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&victim].balance, bal_before);
        assert!(snap.items.contains_key(&ship_id));
        assert!(snap.items[&ship_id].insured_payout_isk >= 0.0);
        assert!(snap.insurance_contracts.contains_key(&ship_id));
        assert!(!snap
            .committed_idempotency_keys
            .contains(&(ship_id, "ship_insurance_payout".to_string())));
        assert_eq!(snap.wallet_journal.len(), journal_before);
        assert_eq!(snap.mails.len(), mails_before);
        assert_eq!(snap.killmails.len(), km_before);
    });

    // 2. Mid-tx panic after insurance settlement is caught by `ctx.db.tx`, rolls back cleanly without double/phantom payout.
    set_post_insurance_tx_fault(Some(PostInsuranceTxFault::Panic));
    let panic_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        execute_death_pipeline_sync(&ctx, sys_id, victim, ship_id, attacker, DVec3::ZERO, 1)
    }));
    assert!(
        matches!(&panic_res, Err(_) | Ok(Err(_))),
        "injected mid-tx panic must abort death pipeline cleanly: {panic_res:?}"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&victim].balance, bal_before);
        assert!(snap.items.contains_key(&ship_id));
        assert!(snap.items[&ship_id].insured_payout_isk >= 0.0);
        assert!(snap.insurance_contracts.contains_key(&ship_id));
        assert!(!snap
            .committed_idempotency_keys
            .contains(&(ship_id, "ship_insurance_payout".to_string())));
        assert_eq!(snap.wallet_journal.len(), journal_before);
        assert_eq!(snap.mails.len(), mails_before);
        assert_eq!(snap.killmails.len(), km_before);
    });

    // 3. Clean retry settles insurance, consumes contract, records idempotency key, and pays exactly once.
    let ok_res = execute_death_pipeline_sync(&ctx, sys_id, victim, ship_id, attacker, DVec3::ZERO, 1)
        .expect("clean retry after rollbacks must succeed");
    assert!((ok_res.insurance_payout_isk - 850_000.0).abs() < 1e-6);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.characters[&victim].balance,
            bal_before.checked_add(IskCents::from_f64_isk(850_000.0)).unwrap()
        );
        assert!(!snap.items.contains_key(&ship_id));
        assert!(!snap.insurance_contracts.contains_key(&ship_id));
        assert!(snap
            .committed_idempotency_keys
            .contains(&(ship_id, "ship_insurance_payout".to_string())));
        assert_eq!(snap.wallet_journal.len(), journal_before + 1);
        assert_eq!(snap.mails.len(), mails_before + 1);
        assert_eq!(snap.killmails.len(), km_before + 1);
    });

    // 4. Direct re-settlement check hits idempotency key and negative ship item marker without double-pay.
    let bal_after_retry = ctx.db.with_snapshot(|s| s.characters[&victim].balance);
    ctx.db.with_mut_snapshot(|snap| {
        let dup_via_key =
            crate::services::combat_crimewatch::combat_wrecks_killmails::settle_ship_insurance_payout_in_snap(
                snap,
                &ctx.sde,
                victim,
                ship_id,
                587,
                false,
                true,
                now,
            );
        assert_eq!(dup_via_key.payout_isk, 0.0);

        let marked_ship = 200_991_002_i64;
        snap.items.insert(
            marked_ship,
            InventoryItemRecord {
                item_id: marked_ship,
                type_id: 587,
                owner_id: victim,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: -1.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        let dup_via_marker =
            crate::services::combat_crimewatch::combat_wrecks_killmails::settle_ship_insurance_payout_in_snap(
                snap,
                &ctx.sde,
                victim,
                marked_ship,
                587,
                false,
                true,
                now,
            );
        assert_eq!(dup_via_marker.payout_isk, 0.0);
        assert_eq!(snap.characters[&victim].balance, bal_after_retry);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mto_site_ball_destruction_bypasses_capsuleer_pipeline_and_completes_mto() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let attacker = 90_000_001i64;
    let victim = 90_000_002i64;
    let attacker_ship = 1_990_000_910i64;
    let mto_ball_sync = 1_990_000_911i64;
    let mto_ball_weapon = 1_990_000_912i64;
    let unpiloted_hull = 1_990_000_913i64;
    let victim_pod = 1_990_000_914i64;
    let den_id = 1_090_000_011i64;
    let now = ctx.now_unix();
    let pos = DVec3::new(10_000.0, 0.0, 0.0);

    spawn_ship(&ctx, sys_id, attacker_ship, attacker, 587, DVec3::ZERO, 0.0);
    spawn_ship(&ctx, sys_id, unpiloted_hull, victim, 587, pos, 0.0);
    spawn_ship(&ctx, sys_id, victim_pod, victim, 670, pos, 0.0);

    let act_hex_1 = "cd".repeat(16);
    let act_hex_2 = "ef".repeat(16);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    for mto_bid in [mto_ball_sync, mto_ball_weapon] {
        sys_handle.try_spawn_ball_with_combat(
            Ball {
                id: mto_bid,
                type_id: 24_690,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 12_000_000.0,
                radius: 200.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(24_690),
        );
    }

    let (bal_before, npc_kills_before) = ctx.db.with_mut_snapshot(|snap| {
        for (mto_bid, act_hex, uuid_byte) in [
            (mto_ball_sync, &act_hex_1, 0xCDu8),
            (mto_ball_weapon, &act_hex_2, 0xEFu8),
        ] {
            snap.items.insert(
                mto_bid,
                make_item(
                    mto_bid,
                    24_690,
                    500_010,
                    i64::from(sys_id),
                    InventoryFlag::None,
                    1,
                    0.0,
                ),
            );
            snap.item_positions.insert(mto_bid, [pos.x, pos.y, pos.z]);
            snap.sp6_lifecycle
                .space_object_meta
                .insert(mto_bid, eve_db::PersistedSpaceObjectMeta::default());
            snap.sp6_lifecycle
                .ship_combat_hp
                .insert(mto_bid, eve_db::PersistedCombatHp::default());
            snap.sov_runtime.mto_site_balls.insert(mto_bid, act_hex.clone());
            snap.sov_runtime.mercenary_activities.insert(
                act_hex.clone(),
                eve_db::PersistedMercenaryActivity {
                    uuid: vec![uuid_byte; 16],
                    den_id,
                    solar_system_id: sys_id,
                    dungeon_id: 1,
                    started: true,
                    started_by: attacker,
                    expiry_unix: now + 3_600,
                    development_impact: 10,
                    anarchy_impact: -10,
                    infomorph_bonus: 25,
                },
            );
        }
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            eve_db::PersistedMercenaryDen {
                den_id,
                type_id: crate::services::sov_gameplay::TYPE_MERCENARY_DEN,
                owner_char_id: attacker,
                skyhook_id: 1_960_000_099,
                solar_system_id: sys_id,
                planet_id: 40_000_001,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 40,
                anarchy_level: 40,
                infomorphs: 10,
                simulated_at_unix: now,
                next_activity_generation_unix: now + 3_600,
            },
        );
        let npc_kills_before: u32 = snap
            .system_kill_events
            .iter()
            .filter(|e| e.solar_system_id == sys_id)
            .map(|e| e.npc_kills)
            .sum();
        (snap.characters[&attacker].balance, npc_kills_before)
    });

    assert_eq!(
        crate::services::combat_crimewatch::combat_missiles_aoe::active_ship_victim_char_id(
            &ctx,
            mto_ball_sync
        ),
        None
    );
    assert_eq!(
        crate::services::combat_crimewatch::combat_missiles_aoe::active_ship_victim_char_id(
            &ctx,
            mto_ball_weapon
        ),
        None
    );
    assert_eq!(
        crate::services::combat_crimewatch::combat_missiles_aoe::active_ship_victim_char_id(
            &ctx,
            unpiloted_hull
        ),
        Some(victim)
    );

    // 1. Direct `on_ball_destroyed_sync` on an MTO site ball completes MTO without player death.
    let sync_res = on_ball_destroyed_sync(&ctx, sys_id, mto_ball_sync, Some(attacker_ship), pos)
        .expect("on_ball_destroyed_sync succeeds for MTO ball");
    assert!(
        sync_res.is_none(),
        "MTO site ball must bypass capsuleer execute_death_pipeline"
    );
    assert!(sys_handle.get_ball_snapshot(mto_ball_sync).await.unwrap().is_none());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.mto_site_balls.contains_key(&mto_ball_sync));
        assert!(!snap.sov_runtime.mercenary_activities.contains_key(&act_hex_1));
        assert!(!snap.items.contains_key(&mto_ball_sync));
        assert!(!snap.item_positions.contains_key(&mto_ball_sync));
        assert!(!snap.sp6_lifecycle.space_object_meta.contains_key(&mto_ball_sync));
        assert!(!snap.sp6_lifecycle.ship_combat_hp.contains_key(&mto_ball_sync));
        let den = snap.sov_runtime.mercenary_dens.get(&den_id).unwrap();
        assert_eq!(den.development_level, 50);
        assert_eq!(den.anarchy_level, 30);
        assert!(snap.killmails.values().all(|km| km.victim_character_id != 500_010));
        // Physical removal happens BEFORE `on_space_entity_destroyed`, crediting military index (`HF1304-P2-1`).
        let npc_kills_mid: u32 = snap
            .system_kill_events
            .iter()
            .filter(|e| e.solar_system_id == sys_id)
            .map(|e| e.npc_kills)
            .sum();
        assert_eq!(npc_kills_mid, npc_kills_before + 1);
    });

    // 2. Weapon destruction path (`handle_ship_or_npc_destruction`) completes the second MTO.
    let weapon_ball = Ball {
        id: mto_ball_weapon,
        type_id: 24_690,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 1.0,
        mass: 12_000_000.0,
        radius: 200.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    crate::services::combat_crimewatch::combat_weapons_spool::handle_ship_or_npc_destruction(
        &ctx,
        &sys_handle,
        attacker,
        attacker_ship,
        587,
        486,
        mto_ball_weapon,
        &weapon_ball,
        None,
        sys_id,
        500.0,
        now,
    )
    .await;
    assert!(sys_handle.get_ball_snapshot(mto_ball_weapon).await.unwrap().is_none());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sov_runtime.mto_site_balls.contains_key(&mto_ball_weapon));
        assert!(!snap.sov_runtime.mercenary_activities.contains_key(&act_hex_2));
        assert!(!snap.items.contains_key(&mto_ball_weapon));
        let den = snap.sov_runtime.mercenary_dens.get(&den_id).unwrap();
        assert_eq!(den.development_level, 60);
        assert_eq!(den.anarchy_level, 20);
        assert_eq!(
            snap.characters[&attacker].balance,
            bal_before
                .checked_add(IskCents::from_isk(15_000_000))
                .unwrap()
        );
        assert!(snap.killmails.values().all(|km| km.victim_character_id != 500_010));
        let npc_kills_after: u32 = snap
            .system_kill_events
            .iter()
            .filter(|e| e.solar_system_id == sys_id)
            .map(|e| e.npc_kills)
            .sum();
        assert_eq!(npc_kills_after, npc_kills_before + 2);
    });

    // 3. Unpiloted character-owned hull (`#835`) still routes through `execute_death_pipeline`.
    let unpiloted_res =
        on_ball_destroyed_sync(&ctx, sys_id, unpiloted_hull, Some(attacker_ship), pos)
            .expect("unpiloted hull kill succeeds")
            .expect("unpiloted hull produces DeathPipelineResult");
    assert_eq!(unpiloted_res.ejected_capsule_id, None);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.killmails[&unpiloted_res.killmail_id].victim_character_id,
            victim
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unpiloted_corp_ship_and_non_mto_npc_hull_destroy_cleanly() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let attacker = 90_000_001i64;
    let attacker_ship = 1_990_000_930i64;
    let corp_id = 98_000_099i64;
    let ally_id = 99_000_099i64;
    let corp_ship = 1_990_000_931i64;
    let corp_cargo = 1_990_000_932i64;
    let expired_mto_hull = 1_990_000_933i64;
    let now = ctx.now_unix();
    let pos = DVec3::new(12_000.0, 0.0, 0.0);

    spawn_ship(&ctx, sys_id, attacker_ship, attacker, 587, DVec3::ZERO, 0.0);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    for (bid, tid) in [(corp_ship, 587u32), (expired_mto_hull, 24_690u32)] {
        sys_handle.try_spawn_ball_with_combat(
            Ball {
                id: bid,
                type_id: tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(tid),
        );
    }

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Target Corp".to_string();
        corp.ticker = "TCORP".to_string();
        corp.alliance_id = Some(ally_id);
        snap.corporations.insert(corp_id, corp);
        snap.items.insert(
            corp_ship,
            make_item(
                corp_ship,
                587,
                corp_id,
                i64::from(sys_id),
                InventoryFlag::None,
                1,
                0.0,
            ),
        );
        snap.item_positions.insert(corp_ship, [pos.x, pos.y, pos.z]);
        snap.sp6_lifecycle
            .space_object_meta
            .insert(corp_ship, eve_db::PersistedSpaceObjectMeta::default());
        snap.sp6_lifecycle
            .ship_combat_hp
            .insert(corp_ship, eve_db::PersistedCombatHp::default());
        snap.items.insert(
            corp_cargo,
            make_item(
                corp_cargo,
                34,
                corp_id,
                corp_ship,
                InventoryFlag::Cargo,
                100,
                0.0,
            ),
        );

        // Non-MTO / expired-MTO `category_id == 6` NPC hull (`owner_id = 500_010`, not in `mto_site_balls`).
        snap.items.insert(
            expired_mto_hull,
            make_item(
                expired_mto_hull,
                24_690,
                500_010,
                i64::from(sys_id),
                InventoryFlag::None,
                1,
                0.0,
            ),
        );
        snap.item_positions
            .insert(expired_mto_hull, [pos.x, pos.y, pos.z]);
        snap.sp6_lifecycle
            .space_object_meta
            .insert(expired_mto_hull, eve_db::PersistedSpaceObjectMeta::default());
        snap.sp6_lifecycle
            .ship_combat_hp
            .insert(expired_mto_hull, eve_db::PersistedCombatHp::default());
    });

    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            resolve_capsuleer_ship_char_in_snap(snap, &ctx.sde, corp_ship),
            None
        );
        assert_eq!(
            resolve_corp_ship_owner_in_snap(snap, &ctx.sde, corp_ship),
            Some(corp_id)
        );
        assert_eq!(
            resolve_corp_ship_owner_in_snap(snap, &ctx.sde, expired_mto_hull),
            None
        );
    });

    // 1. Unpiloted corp-owned ship produces wreck + loot + corporate killmail without pod ejection or zombie ball.
    let corp_res = on_ball_destroyed_sync(&ctx, sys_id, corp_ship, Some(attacker_ship), pos)
        .expect("corp ship kill succeeds")
        .expect("corp ship produces DeathPipelineResult");
    assert_eq!(corp_res.ejected_capsule_id, None);
    assert!(sys_handle.get_ball_snapshot(corp_ship).await.unwrap().is_none());
    let wreck_slim = sys_handle
        .get_ball_slim_item(corp_res.wreck_item_id)
        .await
        .unwrap()
        .expect("corp ship wreck spawned in Ballpark");
    assert_eq!(
        wreck_slim.dict_get("ownerID").and_then(eve_proto::PyRep::as_i64),
        Some(corp_id)
    );
    assert_eq!(
        wreck_slim.dict_get("corpID").and_then(eve_proto::PyRep::as_i64),
        Some(corp_id)
    );
    assert_eq!(
        wreck_slim
            .dict_get("allianceID")
            .and_then(eve_proto::PyRep::as_i64),
        Some(ally_id)
    );

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&corp_ship));
        assert!(!snap.item_positions.contains_key(&corp_ship));
        assert!(!snap.sp6_lifecycle.space_object_meta.contains_key(&corp_ship));
        assert!(!snap.sp6_lifecycle.ship_combat_hp.contains_key(&corp_ship));
        assert_eq!(snap.items[&corp_res.wreck_item_id].owner_id, corp_id);
        let km = &snap.killmails[&corp_res.killmail_id];
        assert_eq!(km.victim_character_id, 0);
        assert_eq!(km.victim_corp_id, corp_id);
        assert_eq!(km.victim_alliance_id, Some(ally_id));
        assert_eq!(km.attacker_character_id, attacker);
        assert_eq!(snap.characters[&attacker].active_ship_id, attacker_ship);
    });
    ctx.db.with_snapshot(|snap| {
        let dkm = snap
            .combat_crimewatch_state
            .detailed_killmails
            .get(&corp_res.killmail_id)
            .expect("DetailedKillmail recorded for corp ship");
        assert_eq!(dkm.victim_character_id, 0);
        assert_eq!(dkm.victim_corporation_id, corp_id);
        assert_eq!(dkm.victim_alliance_id, Some(ally_id));
    });

    // 2. Non-MTO / expired-MTO `category_id == 6` NPC hull cleans up cleanly instead of stranding at 0 HP.
    let km_count_before = ctx.db.with_snapshot(|snap| snap.killmails.len());
    let npc_res = on_ball_destroyed_sync(&ctx, sys_id, expired_mto_hull, Some(attacker_ship), pos)
        .expect("non-MTO NPC hull cleanup succeeds");
    assert!(npc_res.is_none());
    assert!(
        sys_handle
            .get_ball_snapshot(expired_mto_hull)
            .await
            .unwrap()
            .is_none()
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&expired_mto_hull));
        assert!(!snap.item_positions.contains_key(&expired_mto_hull));
        assert!(
            !snap
                .sp6_lifecycle
                .space_object_meta
                .contains_key(&expired_mto_hull)
        );
        assert!(
            !snap
                .sp6_lifecycle
                .ship_combat_hp
                .contains_key(&expired_mto_hull)
        );
        assert_eq!(snap.killmails.len(), km_count_before);
        let _ = now;
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unpiloted_corp_ship_insured_payout_and_ceo_misattribution_guard() {
    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let attacker = 90_000_001i64;
    let attacker_ship = 1_990_000_940i64;
    let ceo_id = 90_999_941i64;
    let ceo_ship = 1_990_000_941i64;
    let corp_id = 98_000_941i64;
    let ally_id = 99_000_941i64;
    let insured_corp_ship = 1_990_000_942i64;
    let direct_corp_ship = 1_990_000_943i64;
    let now = ctx.now_unix();
    let pos = DVec3::new(14_000.0, 0.0, 0.0);

    spawn_ship(&ctx, sys_id, attacker_ship, attacker, 587, DVec3::ZERO, 0.0);
    spawn_ship(&ctx, sys_id, ceo_ship, ceo_id, 587, pos, 0.0);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ceo = snap.characters[&attacker].clone();
        ceo.character_id = ceo_id;
        ceo.name = "Corp CEO".to_string();
        ceo.corporation_id = corp_id;
        ceo.alliance_id = Some(ally_id);
        ceo.active_ship_id = ceo_ship;
        snap.characters.insert(ceo_id, ceo);

        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Insured Corp".to_string();
        corp.ticker = "INSC".to_string();
        corp.ceo_id = ceo_id;
        corp.alliance_id = Some(ally_id);
        snap.corporations.insert(corp_id, corp);

        for (bid, payout) in [(insured_corp_ship, 500_000.0), (direct_corp_ship, 250_000.0)] {
            snap.items.insert(
                bid,
                make_item(bid, 587, corp_id, i64::from(sys_id), InventoryFlag::None, 1, 0.0),
            );
            snap.item_positions.insert(bid, [pos.x, pos.y, pos.z]);
            snap.insurance_contracts.insert(
                bid,
                eve_db::PersistedInsuranceContractRecord {
                    ship_id: bid,
                    ship_type_id: 587,
                    owner_id: corp_id,
                    level_name: "Platinum".into(),
                    level_fraction: 1.0,
                    cost_isk: 50_000.0,
                    payout_isk: payout,
                    start_unix: now.saturating_sub(100),
                    end_unix: now + 3_600,
                },
            );
        }
    });

    for bid in [insured_corp_ship, direct_corp_ship] {
        sys_handle.try_spawn_ball_with_combat(
            Ball {
                id: bid,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        );
    }

    let (corp_bal_before, mails_before) = ctx
        .db
        .with_snapshot(|s| (s.corporations[&corp_id].master_wallet_balance(), s.mails.len()));

    // 1. Direct `settle_ship_insurance_payout_in_snap` with `victim_char_id == 0` credits corp div 1 without creating a `recipient_id == 0` mail.
    let direct_settlement = ctx.db.with_mut_snapshot(|snap| {
        crate::services::combat_crimewatch::combat_wrecks_killmails::settle_ship_insurance_payout_in_snap(
            snap,
            &ctx.sde,
            0,
            direct_corp_ship,
            587,
            false,
            true,
            now,
        )
    });
    assert!((direct_settlement.payout_isk - 250_000.0).abs() < 1e-6);
    assert!(direct_settlement.is_corp);
    assert_eq!(direct_settlement.credited_owner_id, corp_id);
    assert_eq!(direct_settlement.mail_id, None);
    assert!(direct_settlement.mail_subject.is_empty());

    // 2. Passing stale `victim_char_id = ceo_id` to `execute_death_pipeline_sync` for an unpiloted corp hull
    //    must still resolve `corp_owner_id = Some(corp_id)` and `resolved_vic = 0`, credit corp div 1, and emit no char-0 mail.
    let res = execute_death_pipeline_sync(
        &ctx,
        sys_id,
        ceo_id,
        insured_corp_ship,
        attacker,
        pos,
        1,
    )
    .expect("insured unpiloted corp ship pipeline succeeds");
    assert_eq!(res.ejected_capsule_id, None);
    assert!((res.insurance_payout_isk - 500_000.0).abs() < 1e-6);

    ctx.db.with_snapshot(|snap| {
        let corp_bal_after = snap.corporations[&corp_id].master_wallet_balance();
        assert_eq!(
            corp_bal_after,
            corp_bal_before
                .checked_add(IskCents::from_f64_isk(750_000.0))
                .unwrap(),
            "corp division 1 wallet must receive both insurance payouts"
        );
        assert_eq!(snap.mails.len(), mails_before, "unpiloted corp insurance must not create recipient_id=0 mail");
        assert!(!snap.mails.iter().any(|m| m.recipient_id <= 0));
        assert!(!snap.mail_assigned_labels.keys().any(|(r, _)| *r <= 0));
        assert_eq!(snap.items[&res.wreck_item_id].owner_id, corp_id);
        let km = &snap.killmails[&res.killmail_id];
        assert_eq!(km.victim_character_id, 0);
        assert_eq!(km.victim_corp_id, corp_id);
        assert_eq!(km.victim_alliance_id, Some(ally_id));
        assert_eq!(snap.characters[&ceo_id].active_ship_id, ceo_ship);
    });
}

#[tokio::test]
async fn guarded_self_destruct_timer_expiry_emits_immediate_notices_and_aborted_or_docked_emits_none() {
    use eve_db::ServerClock;

    let ctx = test_ctx();
    let vclock = std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_767_225_600));
    ctx.set_clock(vclock.clone());
    let now = ctx.now_unix();

    let sys_id = 30_000_142_u32;
    let victim = 90_000_001_i64;
    let observer = 90_000_002_i64;
    let ship_id = 100_231_001_i64;
    let obs_ship_id = 100_231_002_i64;
    let home_station_id = 60_003_760_i64;

    spawn_ship(&ctx, sys_id, ship_id, victim, 587, DVec3::ZERO, 0.0);
    spawn_ship(&ctx, sys_id, obs_ship_id, observer, 587, DVec3::new(2_500.0, 0.0, 0.0), 0.0);

    let (pilot_uid, obs_uid, pilot_ch) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.home_station_id = home_station_id;
        }
        if let Some(ch) = snap.characters.get_mut(&observer) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = obs_ship_id;
        }
        snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
        snap.item_positions.insert(obs_ship_id, [2_500.0, 0.0, 0.0]);
        (
            snap.characters[&victim].user_id,
            snap.characters[&observer].user_id,
            snap.characters[&victim].clone(),
        )
    });
    ctx.db
        .acquire_online_lock(pilot_uid, 93_101, Some(victim), true, now)
        .unwrap();
    ctx.db
        .acquire_online_lock(obs_uid, 93_102, Some(observer), true, now)
        .unwrap();

    let mut sess = eve_machonet::SessionState::new(93_101);
    sess.user_id = Some(pilot_uid);
    sess.character_id = Some(victim);
    crate::router::apply_session_from_character(&mut sess, &pilot_ch, None, Some(&ctx.sde));
    let empty = HashMap::new();
    let _ = ctx.drain_notifications_for_char(victim);
    let _ = ctx.drain_notifications_for_char(observer);

    // 1. Start non-instant 120s countdown via a single `ship.SelfDestruct([ship_id])` RPC.
    let sd_start = crate::services::combat_crimewatch::combat_wrecks_killmails::handle_wrecks_killmails(
        &ctx,
        &mut sess,
        "ship",
        "SelfDestruct",
        &[eve_proto::PyRep::Integer(ship_id)],
        &empty,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(sd_start.dict_get("countdownStarted"), Some(&eve_proto::PyRep::Bool(true)));
    assert_eq!(sd_start.dict_get("remainingSeconds"), Some(&eve_proto::PyRep::Integer(120)));

    let ship_gen = crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.self_destruct_generation(ship_id)
    });
    let _ = ctx.drain_notifications_for_char(victim);
    let _ = ctx.drain_notifications_for_char(observer);

    // 2. Advance 120s and execute timer-expiry path directly (no second SelfDestruct RPC).
    vclock.advance_secs(120);
    let res = execute_guarded_self_destruct_sync(
        &ctx,
        sys_id,
        victim,
        ship_id,
        DVec3::ZERO,
        ctx.now_unix(),
        Some(ship_gen),
        true,
    )
    .expect("guarded expiry ok")
    .expect("ship self-destruct must commit at expiry");
    let cap_id = res.ejected_capsule_id.expect("ship self-destruct ejects capsule");

    let pilot_notifs = ctx.drain_notifications_for_char(victim);
    assert!(
        pilot_notifs.iter().any(|n| n.event == "OnSessionChanged"),
        "OnSessionChanged must be emitted on pod ejection during countdown expiry"
    );
    assert!(
        pilot_notifs.iter().any(|n| n.event == "OnShipDeath"),
        "OnShipDeath must be emitted during countdown expiry"
    );
    let pilot_imm: Vec<_> = pilot_notifs
        .iter()
        .filter(|n| {
            n.event == "OnRemoteMessage"
                && n.args.first() == Some(&eve_proto::PyRep::String("SelfDestructImmediate".into()))
        })
        .collect();
    assert_eq!(pilot_imm.len(), 1);
    assert_eq!(
        pilot_imm[0].args[1].dict_get("shipTypeID"),
        Some(&eve_proto::PyRep::Integer(587))
    );

    let obs_imm: Vec<_> = ctx
        .drain_notifications_for_char(observer)
        .into_iter()
        .filter(|n| {
            n.event == "OnRemoteMessage"
                && n.args.first()
                    == Some(&eve_proto::PyRep::String("SelfDestructImmediateOther".into()))
        })
        .collect();
    assert_eq!(obs_imm.len(), 1);
    assert_eq!(
        obs_imm[0].args[1].dict_get("charID"),
        Some(&eve_proto::PyRep::Integer(victim))
    );
    assert_eq!(
        obs_imm[0].args[1].dict_get("shipTypeID"),
        Some(&eve_proto::PyRep::Integer(587))
    );

    // 3. Aborted countdown returning `Ok(None)` must emit zero detonation notices.
    let cap_ch = ctx.db.get_character(victim).unwrap();
    crate::router::apply_session_from_character(&mut sess, &cap_ch, None, Some(&ctx.sde));
    assert_eq!(sess.ship_id, Some(cap_id));

    let _ = crate::services::combat_crimewatch::combat_wrecks_killmails::handle_wrecks_killmails(
        &ctx,
        &mut sess,
        "ship",
        "SelfDestruct",
        &[eve_proto::PyRep::Integer(cap_id)],
        &empty,
    )
    .await
    .unwrap()
    .unwrap();
    let stale_cap_gen = crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.self_destruct_generation(cap_id)
    });
    let _ = crate::services::combat_crimewatch::combat_wrecks_killmails::handle_wrecks_killmails(
        &ctx,
        &mut sess,
        "ship",
        "AbortSelfDestruct",
        &[eve_proto::PyRep::Integer(cap_id)],
        &empty,
    )
    .await
    .unwrap()
    .unwrap();
    let _ = ctx.drain_notifications_for_char(victim);
    let _ = ctx.drain_notifications_for_char(observer);

    vclock.advance_secs(120);
    assert!(
        execute_guarded_self_destruct_sync(
            &ctx,
            sys_id,
            victim,
            cap_id,
            DVec3::ZERO,
            ctx.now_unix(),
            Some(stale_cap_gen),
            true,
        )
        .expect("aborted expiry ok")
        .is_none()
    );
    for target in [victim, observer] {
        assert!(
            !ctx.drain_notifications_for_char(target)
                .iter()
                .any(|n| n.event == "OnRemoteMessage"
                    && matches!(
                        n.args.first(),
                        Some(eve_proto::PyRep::String(s)) if s.starts_with("SelfDestructImmediate")
                    )),
            "aborted Ok(None) must emit zero SelfDestructImmediate* notices"
        );
    }

    // 4. Docked guard returning `Ok(None)` emits zero detonation notices, and capsule self-destruct expiry
    //    captures shipTypeID=670 + grid observers BEFORE pod respawn relocates pilot to home station.
    let _ = crate::services::combat_crimewatch::combat_wrecks_killmails::handle_wrecks_killmails(
        &ctx,
        &mut sess,
        "ship",
        "SelfDestruct",
        &[eve_proto::PyRep::Integer(cap_id)],
        &empty,
    )
    .await
    .unwrap()
    .unwrap();
    let active_cap_gen = crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.self_destruct_generation(cap_id)
    });
    vclock.advance_secs(120);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&victim).unwrap().station_id = Some(home_station_id);
    });
    let _ = ctx.drain_notifications_for_char(victim);
    let _ = ctx.drain_notifications_for_char(observer);
    assert!(
        execute_guarded_self_destruct_sync(
            &ctx,
            sys_id,
            victim,
            cap_id,
            DVec3::ZERO,
            ctx.now_unix(),
            Some(active_cap_gen),
            true,
        )
        .expect("docked guard ok")
        .is_none()
    );
    for target in [victim, observer] {
        assert!(
            !ctx.drain_notifications_for_char(target)
                .iter()
                .any(|n| n.event == "OnRemoteMessage"
                    && matches!(
                        n.args.first(),
                        Some(eve_proto::PyRep::String(s)) if s.starts_with("SelfDestructImmediate")
                    )),
            "docked Ok(None) must emit zero SelfDestructImmediate* notices"
        );
    }

    // Re-undock and start a fresh capsule countdown to verify pod expiry captures shipTypeID=670
    // and grid observers BEFORE pod respawn relocates the pilot to home station.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&victim).unwrap().station_id = None;
    });
    let _ = crate::services::combat_crimewatch::combat_wrecks_killmails::handle_wrecks_killmails(
        &ctx,
        &mut sess,
        "ship",
        "SelfDestruct",
        &[eve_proto::PyRep::Integer(cap_id)],
        &empty,
    )
    .await
    .unwrap()
    .unwrap();
    let fresh_cap_gen = crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.self_destruct_generation(cap_id)
    });
    let _ = ctx.drain_notifications_for_char(victim);
    let _ = ctx.drain_notifications_for_char(observer);
    vclock.advance_secs(120);
    let pod_res = execute_guarded_self_destruct_sync(
        &ctx,
        sys_id,
        victim,
        cap_id,
        DVec3::ZERO,
        ctx.now_unix(),
        Some(fresh_cap_gen),
        true,
    )
    .expect("pod expiry ok")
    .expect("capsule self-destruct must commit once undocked");
    assert_eq!(pod_res.ejected_capsule_id, None);
    let pod_pilot_imm: Vec<_> = ctx
        .drain_notifications_for_char(victim)
        .into_iter()
        .filter(|n| {
            n.event == "OnRemoteMessage"
                && n.args.first() == Some(&eve_proto::PyRep::String("SelfDestructImmediate".into()))
        })
        .collect();
    assert_eq!(pod_pilot_imm.len(), 1);
    assert_eq!(
        pod_pilot_imm[0].args[1].dict_get("shipTypeID"),
        Some(&eve_proto::PyRep::Integer(670))
    );
    let pod_obs_imm: Vec<_> = ctx
        .drain_notifications_for_char(observer)
        .into_iter()
        .filter(|n| {
            n.event == "OnRemoteMessage"
                && n.args.first()
                    == Some(&eve_proto::PyRep::String("SelfDestructImmediateOther".into()))
        })
        .collect();
    assert_eq!(pod_obs_imm.len(), 1);
    assert_eq!(
        pod_obs_imm[0].args[1].dict_get("shipTypeID"),
        Some(&eve_proto::PyRep::Integer(670))
    );
}

#[tokio::test]
async fn abandoned_destroyed_drone_scoping_drops_on_wire_for_docked_owner_1190_1565() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let drone_type = 2_454_u32;
    let drone_a = 1_897_065_901_i64;
    let drone_b = 1_897_065_902_i64;
    let killer_id = 1_897_065_903_i64;

    let uid = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        let uid = ch.user_id;
        for did in [drone_a, drone_b] {
            snap.items.insert(
                did,
                InventoryItemRecord {
                    item_id: did,
                    type_id: drone_type,
                    owner_id: char_id,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        uid
    });
    ctx.db
        .acquire_online_lock(uid, 9_501, Some(char_id), true, ctx.now_unix())
        .expect("acquire_online_lock");
    let _ = ctx.drain_notifications_for_char(char_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let make_drone_ball = |did: i64| Ball {
        id: did,
        type_id: drone_type,
        position: DVec3::new(3_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 2_600.0,
        inertia: 0.4,
        mass: 5_000.0,
        radius: 5.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let ball_a = make_drone_ball(drone_a);
    let ball_b = make_drone_ball(drone_b);
    let _ = sys_handle.spawn_ball(ball_a.clone()).await;
    let _ = sys_handle.spawn_ball(ball_b.clone()).await;
    crate::services::drone_test_helpers::seed_active_drones(
        &ctx,
        [drone_a, drone_b].map(|did| crate::services::combat_crimewatch::DroneSpaceState {
            ball_id: did,
            drone_item_id: did,
            type_id: drone_type,
            group_id: 100,
            owner_char_id: char_id,
            original_owner_char_id: char_id,
            parent_ship_id: ship_id,
            solar_system_id: sys_id,
            target_id: None,
            activity_state: 0,
            assist_char_id: None,
            guard_char_id: None,
            is_abandoned: true,
            is_sentry: false,
            bandwidth_used: 5.0,
            max_velocity: 2_600.0,
            optimal_range: 1_500.0,
            damage_vector: eve_destiny::DamageVector::default(),
            shield_repair_amount: 0.0,
            armor_repair_amount: 0.0,
            hull_repair_amount: 0.0,
            web_speed_factor: 0.0,
            neut_amount: 0.0,
            ecm_strength: 0.0,
            tp_sig_bonus_pct: 0.0,
        }),
    );

    // 1. Destroy abandoned drone `drone_a`: emitted `OnDroneStateChange` carries in-space scope.
    crate::combat::npc_ai::handle_npc_ai_ball_destructions(
        &ctx,
        sys_id,
        vec![(killer_id, drone_a, ball_a)],
    );
    let pending = ctx.drain_notifications_for_char(char_id);
    let drone_a_notif = pending
        .iter()
        .find(|n| {
            n.event == "OnDroneStateChange"
                && n.args.first().and_then(|v| v.as_i64()) == Some(drone_a)
        })
        .expect("destroyed abandoned drone_a must enqueue OnDroneStateChange");
    assert_eq!(
        drone_a_notif.scene_scope,
        Some(crate::outbound_scene_scope::OutboundSceneScope::space(
            Some(sys_id),
            Some(ctx.session_scene_epoch(9_501)),
        ))
    );

    // 2. Destroy `drone_b` while owner docks before wire drain: wire sequencer drops `OnDroneStateChange`.
    crate::combat::npc_ai::handle_npc_ai_ball_destructions(
        &ctx,
        sys_id,
        vec![(killer_id, drone_b, ball_b)],
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(60_003_760);
    });
    let mut docked_sess = eve_machonet::SessionState::new(9_501);
    docked_sess.user_id = Some(uid);
    docked_sess.character_id = Some(char_id);
    ctx.sync_session(&mut docked_sess);
    let mut wire = crate::router::client_wire::ClientWireSession::from_session(&ctx, docked_sess);
    let (frames, _) = wire.drain_pending_frames();
    assert!(
        !frames.iter().any(|f| matches!(
            f,
            crate::outbound_sequencer::OutFrame::Notification { event, .. }
                if event == "OnDroneStateChange"
        )),
        "docked owner must never receive OnDroneStateChange on wire: {frames:?}"
    );
    assert!(
        wire.wire_state.scene_drop_count() >= 1,
        "expected scene_drop_count >= 1 for docked owner OnDroneStateChange"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_system_destroyed_player_drone_skips_ballpark_on_drone_state_change_without_panic() {
    let ctx = ServerContext::new();
    let owner_char_id = 90_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&owner_char_id).unwrap().solar_system_id = 0;
    });
    let drone_item_id = 990_000_123_i64;
    let ball_id = 990_000_124_i64;
    crate::services::drone_test_helpers::seed_active_drone(
        &ctx,
        crate::services::combat_crimewatch::DroneSpaceState {
            ball_id,
            drone_item_id,
            type_id: 2454,
            group_id: 100,
            owner_char_id,
            original_owner_char_id: owner_char_id,
            parent_ship_id: 100_000_001,
            solar_system_id: 0,
            target_id: None,
            activity_state: 0,
            assist_char_id: None,
            guard_char_id: None,
            is_abandoned: false,
            is_sentry: false,
            bandwidth_used: 5.0,
            max_velocity: 400.0,
            optimal_range: 10_000.0,
            damage_vector: eve_destiny::DamageVector::default(),
            shield_repair_amount: 0.0,
            armor_repair_amount: 0.0,
            hull_repair_amount: 0.0,
            web_speed_factor: 0.0,
            neut_amount: 0.0,
            ecm_strength: 0.0,
            tp_sig_bonus_pct: 0.0,
        },
    );
    let zero_sys_handle = ctx.systems.get_or_spawn(0);
    let _ = ctx.drain_notifications_for_char(owner_char_id);
    let removed = crate::combat::death_pipeline::cleanup_destroyed_player_drone_sync(
        &ctx,
        &zero_sys_handle,
        ball_id,
    );
    assert!(removed);
    let notifs = ctx.drain_notifications_for_char(owner_char_id);
    assert!(
        !notifs.iter().any(|n| n.event == "OnDroneStateChange"),
        "zero-system drone cleanup must skip OnDroneStateChange instead of panicking: {notifs:?}"
    );
    assert!(
        crate::router::notify_registry::BallparkScene::try_from_outbound_scope(
            ctx.character_space_scene_scope(owner_char_id, Some(0)),
            0,
        )
        .is_none()
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&owner_char_id) {
            ch.solar_system_id = 30_000_142;
        }
    });
    assert_eq!(
        ctx.character_space_scene_scope(owner_char_id, Some(0)).scene_sys_id,
        None
    );
    assert!(
        crate::router::notify_registry::BallparkScene::try_from_outbound_scope(
            ctx.character_space_scene_scope(owner_char_id, Some(0)),
            0,
        )
        .is_none()
    );
    assert!(ctx.ballpark_scene_for_char(owner_char_id, 0).is_none());
}
