//! Unit and mutation-killing tests for `combat::death_pipeline`.

use super::*;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{AssistLogEntry, InsuranceContractState};
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
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.insurance_contracts.insert(
            exp_ship,
            InsuranceContractState {
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
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.insurance_contracts.insert(
            corp_ship,
            InsuranceContractState {
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

    // 5. Active cs_contract (container_ship_state) pays out cs_contract.payout_isk
    let cs_ship = 100_200_005;
    spawn_ship(&ctx, sys_id, cs_ship, victim, 587, DVec3::ZERO, 0.0);
    crate::services::inventory_fitting::fitting_containers_ships::write_container_ship_state(
        &ctx,
        |st| {
            st.insurance_contracts.insert(
                cs_ship,
                crate::services::inventory_fitting::fitting_containers_ships::InsuranceContractRecord {
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
        },
    );
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
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _| {
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
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _| {
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
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _| {
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
        events.iter().any(|e| matches!(
            e,
            eve_destiny::DestinyEvent::SetState { observer_ball_id } if *observer_ball_id == cap_id
        )),
        "death pipeline must queue SetState for ejected capsule (#729)"
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
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.insurance_contracts.insert(
            bs_id,
            InsuranceContractState {
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
    crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        let dkm = &rt.detailed_killmails[&bs_res.killmail_id];
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
    crate::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        let dkm = &rt.detailed_killmails[&pod_res.killmail_id];
        assert!(dkm.destroyed_items.iter().any(|i| i.type_id == 9_899));
        assert!(dkm.destroyed_items.iter().any(|i| i.type_id == 10_208));
    });

    // 4. Expired cs_contract falls back to 40% base quote; insured_payout > 0 pays directly when no contract;
    //    victim controlling structure suppresses capsule ejection; null-sec skips sec loss & kill rights
    let expired_cs_ship = 200_200_030_i64;
    spawn_ship(&ctx, sys_null, expired_cs_ship, victim, 587, DVec3::ZERO, 0.0);
    crate::services::inventory_fitting::fitting_containers_ships::write_container_ship_state(
        &ctx,
        |st| {
            st.insurance_contracts.insert(
                expired_cs_ship,
                crate::services::inventory_fitting::fitting_containers_ships::InsuranceContractRecord {
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
        },
    );
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
    use crate::router::SessionState;
    use eve_proto::PyRep;

    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("b3_destruction.sqlite");
    let sde = {
        let base_ctx = test_ctx();
        Arc::clone(&base_ctx.sde)
    };
    let repo = Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
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
    let eject_resp = ctx
        .dispatch_rpc(&mut sess, "ship", None, "Eject", &[], &kw, None)
        .await
        .expect("ship.Eject succeeds");
    let ejected_pod_id = match eject_resp {
        PyRep::Integer(cid) => cid,
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
    crate::services::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
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
}
