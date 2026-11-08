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

#[tokio::test]
async fn death_pipeline_rejects_non_ship_and_covers_all_insurance_branches() {
    let ctx = test_ctx();
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
    let corp_bal_before = ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet);
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
    let corp_bal_after = ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet);
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

#[tokio::test]
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

#[tokio::test]
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

#[tokio::test]
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
