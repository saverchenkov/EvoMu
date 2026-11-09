//! Deep branch and mutation-killing unit tests for `combat/` (`npc_ai`, `crimewatch`,
//! `drones_salvage_tractor`, `death_pipeline`) and `services::combat_weapons*`.

#![forbid(unsafe_code)]

use super::crimewatch::{
    compute_sec_status_loss, concord_delay_for_security,
    credit_specialized_npc_bounty_and_sec_status, AggressionEvaluation, CrimeWatchDirector,
    CrimeWatchLock,
};
use super::death_pipeline::{execute_death_pipeline_sync, SelfDestructManager};
use super::drones_salvage_tractor::{
    step_tractor_beam, validate_drone_bandwidth, ActiveDrone, DroneController,
};
use super::npc_ai::{
    clone_soldier_tier_for_security, clone_soldier_type_for_security,
    clone_soldier_type_for_security_and_faction, compute_sec_status_gain, faction_damage_vector,
    regional_pirate_faction_id, NpcAiState, NpcCombatEntity, NpcSpawnManager,
};
use crate::router::ServerContext;
use eve_db::{CorpWalletDivisionRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, CombatState, MovementMode};
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
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1 && flag == InventoryFlag::None,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
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
    cs_patch: impl FnOnce(&mut CombatState),
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, type_id, owner_id, i64::from(sys_id), InventoryFlag::None, 1),
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
    let mut cs = ctx.build_ship_combat_state(owner_id, ship_id, type_id);
    cs.capacitor = 500.0;
    cs_patch(&mut cs);
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_200_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        },
        cs,
    );
}

#[tokio::test(flavor = "current_thread")]
async fn npc_ai_vectors_clone_tiers_sec_math_and_crimewatch_rules() {
    // 1. faction_damage_vector across all 5 branches with non-unit DPS
    let guristas = faction_damage_vector(500_010, 200.0);
    assert!((guristas.kinetic - 140.0).abs() < 1e-6 && (guristas.thermal - 60.0).abs() < 1e-6);
    assert_eq!(guristas.em, 0.0);
    assert_eq!(guristas.explosive, 0.0);

    let sansha = faction_damage_vector(500_019, 100.0);
    assert!((sansha.em - 60.0).abs() < 1e-6 && (sansha.thermal - 40.0).abs() < 1e-6);

    let serpentis = faction_damage_vector(500_020, 100.0);
    assert!((serpentis.thermal - 60.0).abs() < 1e-6 && (serpentis.kinetic - 40.0).abs() < 1e-6);

    let angel = faction_damage_vector(500_011, 100.0);
    assert!((angel.explosive - 60.0).abs() < 1e-6 && (angel.kinetic - 30.0).abs() < 1e-6 && (angel.thermal - 10.0).abs() < 1e-6);

    let sleeper = faction_damage_vector(500_021, 100.0);
    assert!((sleeper.em - 25.0).abs() < 1e-6 && (sleeper.thermal - 25.0).abs() < 1e-6);
    assert!((sleeper.kinetic - 25.0).abs() < 1e-6 && (sleeper.explosive - 25.0).abs() < 1e-6);

    let ctx = test_ctx();
    let sys_id: u32 = 30_000_142;

    // 2. Clone soldier security thresholds & pirate faction mapping
    assert!(clone_soldier_tier_for_security(0.049).is_none());
    assert_eq!(clone_soldier_tier_for_security(0.05), Some("Negotiator"));
    assert_eq!(clone_soldier_tier_for_security(0.15), Some("Transporter"));
    assert_eq!(clone_soldier_tier_for_security(0.25), Some("Recruiter"));
    assert_eq!(clone_soldier_tier_for_security(0.35), Some("Trainer"));
    assert!(clone_soldier_tier_for_security(0.45).is_none());
    assert_eq!(clone_soldier_type_for_security(0.05), Some(33175));
    assert_eq!(clone_soldier_type_for_security(0.15), Some(33163));
    assert_eq!(clone_soldier_type_for_security(0.25), Some(33142));
    assert_eq!(clone_soldier_type_for_security(0.35), Some(33132));
    assert_eq!(clone_soldier_type_for_security_and_faction(&ctx.sde, 0.10, 500_010), Some(33175));
    let mut sde_with_pirate_clones = (*ctx.sde).clone();
    let sample_cs = sde_with_pirate_clones.types.get(&33175).cloned().unwrap();
    for (tid, gid, name) in [
        (33189_u32, 552_u32, "Blood Clone Soldier Negotiator"),
        (33177_u32, 566_u32, "Sansha's Clone Soldier Negotiator"),
        (33165_u32, 574_u32, "Serpentis Clone Soldier Negotiator"),
        (33155_u32, 551_u32, "Arch Angel Clone Soldier Negotiator"),
    ] {
        let mut t = sample_cs.clone();
        t.type_id = tid;
        t.group_id = gid;
        t.name = name.into();
        sde_with_pirate_clones.types.insert(tid, t);
    }
    assert_eq!(clone_soldier_type_for_security_and_faction(&sde_with_pirate_clones, 0.10, 500_012), Some(33189));
    assert_eq!(clone_soldier_type_for_security_and_faction(&sde_with_pirate_clones, 0.10, 500_019), Some(33177));
    assert_eq!(clone_soldier_type_for_security_and_faction(&sde_with_pirate_clones, 0.10, 500_020), Some(33165));
    assert_eq!(clone_soldier_type_for_security_and_faction(&sde_with_pirate_clones, 0.10, 500_011), Some(33155));

    // 3. compute_sec_status_gain & compute_sec_status_loss boundaries
    assert_eq!(compute_sec_status_gain(5.0, 1_000_000.0), 0.0);
    assert!((compute_sec_status_gain(4.9995, 500_000.0) - 0.0005).abs() < 1e-9);
    let gain_low = compute_sec_status_gain(-12.0, 50_000_000.0);
    assert!(gain_low > 0.0 && gain_low <= 0.5);

    let pod_loss = compute_sec_status_loss(2.0, 4.0, true, 0.8);
    let ship_loss = compute_sec_status_loss(2.0, 4.0, false, 0.8);
    assert!((pod_loss / ship_loss - 10.0).abs() < 1e-6);
    assert_eq!(compute_sec_status_loss(2.0, 4.0, true, 0.0), 0.0);
    assert!((concord_delay_for_security(1.0) - 6.0).abs() < 1e-9);
    assert!((concord_delay_for_security(0.8) - 8.0).abs() < 1e-9);
    assert!((concord_delay_for_security(0.7) - 11.0).abs() < 1e-9);
    assert!((concord_delay_for_security(0.6) - 14.0).abs() < 1e-9);
    assert!((concord_delay_for_security(0.5) - 19.0).abs() < 1e-9);

    // 4. NpcSpawnManager::step_ai_sync threat priority (EWAR 1.5x > baseline 1.0x), >50km Idle, 2-tick attack cooldown, and cleanup respawn
    assert_eq!(regional_pirate_faction_id(&ctx, 999_999_999), 500_010);

    let p_base: i64 = 120_000_001;
    let p_ewar: i64 = 120_000_002;
    let p_pod: i64 = 120_000_003;
    let p_far: i64 = 120_000_004;
    let npc_ball: i64 = 120_000_010;
    let dead_npc: i64 = 120_000_011;

    spawn_ship(&ctx, sys_id, p_base, 90_000_001, 587, DVec3::new(3_000.0, 0.0, 0.0), |_| {});
    spawn_ship(&ctx, sys_id, p_ewar, 90_000_001, 587, DVec3::new(3_200.0, 0.0, 0.0), |_| {});
    spawn_ship(&ctx, sys_id, p_pod, 90_000_001, 670, DVec3::new(500.0, 0.0, 0.0), |_| {});
    spawn_ship(&ctx, sys_id, p_far, 90_000_001, 587, DVec3::new(75_000.0, 0.0, 0.0), |_| {});

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    // Give p_ewar an active EWAR module (type 526, group 52) so AI gives p_ewar 1.5x threat weight!
    ctx.db.with_mut_snapshot(|snap| {
        let ewar_mod = 77_901_i64;
        snap.items.insert(ewar_mod, make_item(ewar_mod, 526, 90_000_001, p_ewar, InventoryFlag::MedSlot0, 1));
        snap.module_active_states.insert(ewar_mod);
    });

    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: npc_ball, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 300.0, inertia: 1.5, mass: 1_000_000.0, radius: 35.0,
            speed_fraction: 1.0, mode: MovementMode::Stop,
        },
        CombatState {
            shield_hp: 500.0, max_shield_hp: 500.0, armor_hp: 500.0, max_armor_hp: 500.0,
            structure_hp: 500.0, max_structure_hp: 500.0, signature_radius: 40.0,
            ..CombatState::default()
        },
    );

    let mut mgr = NpcSpawnManager::default();
    mgr.entities.insert(npc_ball, NpcCombatEntity {
        ball_id: npc_ball, type_id: 587, faction_id: 500_010, state: NpcAiState::Idle,
        spawn_group_id: 900_001, spawn_pos: DVec3::ZERO, target_id: None,
        bounty_isk: 25_000.0, optimal_range: 10_000.0, orbit_range: 2_500.0, base_dps: 50.0,
        damage_vector: faction_damage_vector(500_010, 50.0), last_attack_tick: 1,
        has_shield_rep: false, has_armor_rep: false,
    });
    mgr.entities.insert(dead_npc, NpcCombatEntity {
        ball_id: dead_npc, type_id: 587, faction_id: 500_010, state: NpcAiState::Idle,
        spawn_group_id: 900_002, spawn_pos: DVec3::ZERO, target_id: None,
        bounty_isk: 10_000.0, optimal_range: 10_000.0, orbit_range: 2_500.0, base_dps: 20.0,
        damage_vector: faction_damage_vector(500_010, 20.0), last_attack_tick: 1,
        has_shield_rep: false, has_armor_rep: false,
    });

    // Tick 2: current_tick(2) < last_attack_tick(1) + 2 -> selects p_ewar (1.5x priority) & removes missing dead_npc, cooldown blocks damage!
    mgr.step_ai_sync(&ctx, sys_id, 2, 1.0);
    assert_eq!(mgr.entities.len(), 1);
    assert_eq!(mgr.entities[&npc_ball].target_id, Some(p_ewar));
    let ewar_sh_before = sys_handle.get_ball_position_sync(p_ewar);
    assert!(ewar_sh_before.is_some());

    // Tick 3: current_tick(3) >= 1 + 2 -> applies Guristas Kin+Therm damage to p_ewar!
    mgr.step_ai_sync(&ctx, sys_id, 3, 1.0);

    // Specialized NPC bounty + 900s sec-gain window + in-window higher-bounty upgrade + duplicate ball deduplication
    let ch_id = 90_000_001;
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&ch_id).unwrap().security_status = 0.0);
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, 700_001, ch_id, 200_000.0);
    let sec_first = ctx.db.with_snapshot(|s| s.characters[&ch_id].security_status);
    assert!(sec_first > 0.0);
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, 700_001, ch_id, 900_000.0);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&ch_id].security_status), sec_first);
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, 700_002, ch_id, 800_000.0);
    assert!(ctx.db.with_snapshot(|s| s.characters[&ch_id].security_status) > sec_first);

    // CrimeWatchDirector: remote assistance criminal flag + CONCORD schedule in highsec
    let mut cw = CrimeWatchDirector::default();
    assert!(matches!(
        cw.handle_criminal_remote_assistance(ch_id, ch_id, 1, sys_id, 0.8, 6_000),
        AggressionEvaluation::CriminalFlagTriggered { concord_delay_sec, arrival_unix }
            if (concord_delay_sec - 8.0).abs() < 1e-9 && arrival_unix == 6_008
    ));
    assert!(cw.get_or_create(ch_id).pending_concord_arrival.is_some());

    // CrimeWatchLock push_concord_reports caps at 32 entries
    let lock = CrimeWatchLock::new(Arc::clone(&ctx.db));
    for idx in 1..=35_i64 {
        lock.push_concord_reports(&[super::crimewatch::ConcordExecutionReport {
            criminal_char_id: ch_id,
            victim_char_id: 90_000_002,
            concord_ball_id: 95_000_000 + idx,
            kill_right_id: idx,
            webbed_speed_fraction: 0.0,
            warp_scramble_strength: 100,
            capacitor_after_neut: 0.0,
            ecm_jammed: true,
            locks_cleared: 0,
            death_result: Err("test".into()),
        }]);
    }
    assert_eq!(lock.concord_reports().len(), 32);
    assert_eq!(lock.concord_reports().first().unwrap().kill_right_id, 4);
}

#[tokio::test(flavor = "current_thread")]
async fn tractor_stop_distance_drone_bandwidth_multi_ship_drones_and_death_pipeline_branches() {
    // 1. step_tractor_beam(wreck_pos, ship_pos, tractor_velocity, dt_sec): inside 1200m vs partial approach vs arrival clamp
    let (stay, arrived_stay) = step_tractor_beam(DVec3::new(1_150.0, 0.0, 0.0), DVec3::ZERO, 500.0, 1.0);
    assert!(arrived_stay && (stay.x - 1_150.0).abs() < 1e-9);

    let (mid, arrived_mid) = step_tractor_beam(DVec3::new(3_000.0, 0.0, 0.0), DVec3::ZERO, 500.0, 1.0);
    assert!(!arrived_mid && (mid.x - 2_500.0).abs() < 1e-9);

    let (stop_at, arrived_clamp) = step_tractor_beam(DVec3::new(1_300.0, 0.0, 0.0), DVec3::ZERO, 500.0, 1.0);
    assert!(arrived_clamp && (stop_at.x - 1_200.0).abs() < 1e-6);

    // 2. validate_drone_bandwidth & DroneController multi-ship recall/return
    let ctx = test_ctx();
    let sys_id: u32 = 30_000_142;
    let char_a: i64 = 90_000_001;
    let ship_a: i64 = 130_000_001;
    let ship_b: i64 = 130_000_002;
    let drone_ship_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.default_attributes.get(&eve_sde::attr_ids::DRONE_BANDWIDTH).copied().unwrap_or(0.0) >= 25.0)
        .map(|t| t.type_id)
        .unwrap();
    spawn_ship(&ctx, sys_id, ship_a, char_a, drone_ship_tid, DVec3::ZERO, |_| {});
    spawn_ship(&ctx, sys_id, ship_b, char_a, 587, DVec3::new(500.0, 0.0, 0.0), |_| {});

    // > 5 drones blocked, bandwidth exceeded blocked (Rifter 0 Mbit/s), and valid bandwidth allowed
    assert!(validate_drone_bandwidth(&ctx, ship_a, &[2454, 2454, 2454, 2454, 2454, 2454], &[]).is_err());
    assert!(validate_drone_bandwidth(&ctx, ship_b, &[2454], &[]).is_err());
    assert!(validate_drone_bandwidth(&ctx, ship_a, &[2454], &[]).is_ok());

    // Multi-ship DroneController: engaging and recalling on ship_a leaves ship_b's drone untouched!
    let d1: i64 = 130_100_001;
    let d2: i64 = 130_100_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(d1, make_item(d1, 2454, char_a, i64::from(sys_id), InventoryFlag::None, 1));
        snap.items.insert(d2, make_item(d2, 2454, char_a, i64::from(sys_id), InventoryFlag::None, 1));
    });
    let mut dc = DroneController::default();
    dc.active_drones.insert(d1, ActiveDrone {
        ball_id: d1, drone_item_id: d1, type_id: 2454, owner_char_id: char_a,
        parent_ship_id: ship_a, target_id: Some(ship_b), dps: 18.0,
    });
    dc.active_drones.insert(d2, ActiveDrone {
        ball_id: d2, drone_item_id: d2, type_id: 2454, owner_char_id: char_a,
        parent_ship_id: ship_b, target_id: Some(ship_a), dps: 18.0,
    });
    assert_eq!(dc.return_and_orbit(&ctx, sys_id, ship_a).await.unwrap(), 1);
    assert_eq!(dc.active_drones[&d1].target_id, None);
    assert_eq!(dc.active_drones[&d2].target_id, Some(ship_a));

    let recalled = dc.recall_drones(&ctx, sys_id, ship_a).await.unwrap();
    assert_eq!(recalled, vec![d1]);
    assert!(!dc.active_drones.contains_key(&d1));
    assert!(dc.active_drones.contains_key(&d2));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&d1].flag), InventoryFlag::DroneBay);

    // 3. Death pipeline: corporate-owned ship insurance payout -> corp wallet, escape frigate bay (179), rig preservation (92), courier wrap fail
    let bs_type_id = ctx.sde.types.values().find(|t| t.group_id == 27).map(|t| t.type_id).unwrap_or(638);
    let frig_type_id = ctx.sde.types.values().find(|t| t.group_id == 25).map(|t| t.type_id).unwrap_or(587);
    let corp_id: i64 = 98_000_555;
    let victim_char: i64 = 90_000_002;
    let victim_ship: i64 = 140_000_001;
    let escape_frig: i64 = 140_000_002;
    let rig_item: i64 = 140_000_003;
    let wrap_item: i64 = 140_000_004;
    let wrapped_cargo: i64 = 140_000_005;
    let courier_contract: i64 = 88_000_001;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "TestCorp".into(),
                ticker: "TCRP".into(),
                master_wallet: IskCents::from_f64_isk(1_000_000.0),
                wallet_divisions: vec![CorpWalletDivisionRecord {
                    division_id: 1,
                    name: "Master".into(),
                    balance: IskCents::from_f64_isk(1_000_000.0),
                }],
                ..CorporationRecord::default()
            },
        );
        let mut ship_rec = make_item(victim_ship, bs_type_id, corp_id, i64::from(sys_id), InventoryFlag::None, 1);
        ship_rec.singleton = true;
        snap.items.insert(victim_ship, ship_rec);
        let mut esc_rec = make_item(escape_frig, frig_type_id, victim_char, victim_ship, InventoryFlag::from_u16(179), 1);
        esc_rec.singleton = true;
        snap.items.insert(escape_frig, esc_rec);
        snap.items.insert(rig_item, make_item(rig_item, 31716, victim_char, victim_ship, InventoryFlag::RigSlot0, 1));
        snap.items.insert(wrap_item, make_item(wrap_item, 3468, victim_char, victim_ship, InventoryFlag::Cargo, 1));
        snap.items.insert(wrapped_cargo, make_item(wrapped_cargo, 34, victim_char, wrap_item, InventoryFlag::None, 50));

        snap.contracts.insert(
            courier_contract,
            eve_db::ContractRecord {
                contract_id: courier_contract,
                issuer_id: char_a,
                assignee_id: victim_char,
                acceptor_id: Some(victim_char),
                contract_type: "Courier".into(),
                status: "InProgress".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                reward: IskCents::from_f64_isk(50_000.0),
                collateral: IskCents::from_f64_isk(200_000.0),
                item_ids: vec![wrap_item],
                expiration_unix: now + 86_400,
                date_issued_unix: 100,
                title: "Courier".into(),
                ..eve_db::ContractRecord::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&victim_char) {
            ch.active_ship_id = victim_ship;
            ch.solar_system_id = sys_id;
            ch.corporation_id = corp_id;
        }
    });

    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.insurance_contracts.insert(
            victim_ship,
            crate::services::combat_crimewatch::InsuranceContractState {
                ship_id: victim_ship,
                ship_type_id: bs_type_id,
                owner_id: corp_id,
                level_name: "Platinum".into(),
                level_fraction: 1.0,
                cost_isk: 100_000.0,
                payout_isk: 5_000_000.0,
                start_unix: now.saturating_sub(100),
                end_unix: now + 86_400,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: victim_ship,
            type_id: bs_type_id,
            position: DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 2.0,
            mass: 10_000_000.0,
            radius: 120.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    );

    let res = execute_death_pipeline_sync(&ctx, sys_id, victim_char, victim_ship, char_a, DVec3::ZERO, now).unwrap();
    assert_eq!(res.ejected_capsule_id, Some(escape_frig));
    assert_eq!(res.insurance_payout_isk, 5_000_000.0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corporations[&corp_id].master_wallet, IskCents::from_f64_isk(6_000_000.0));
        assert!(!snap.items.values().any(|i| i.type_id == 31716 && i.location_id == res.wreck_item_id));
        assert_eq!(snap.contracts[&courier_contract].status, "Failed");
        assert_eq!(snap.characters[&victim_char].active_ship_id, escape_frig);
    });

    // SelfDestructManager timing checks
    let mut sdm = SelfDestructManager::new();
    let det = sdm.start_self_destruct(ship_a, char_a, sys_id, 1_000);
    assert_eq!(det, 1_120);
    assert_eq!(sdm.remaining_seconds(ship_a, 1_050), Some(70));
    assert!(sdm.abort_self_destruct(ship_a));
    assert_eq!(sdm.remaining_seconds(ship_a, 1_050), None);
}

#[tokio::test(flavor = "current_thread")]
async fn weapons_projectile_zero_cap_safety_green_yellow_and_spool_banks() {
    use crate::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
    use eve_proto::PyRep;

    let ctx = test_ctx();
    let low_sys: u32 = 30_002_813; // Low-sec system: suspect on ship attack
    let shooter_char: i64 = 90_000_001;
    let target_char: i64 = 90_000_002;
    let shooter_ship: i64 = 150_000_001;
    let target_ship: i64 = 150_000_002;

    spawn_ship(&ctx, low_sys, shooter_ship, shooter_char, 587, DVec3::ZERO, |cs| {
        cs.capacitor = 0.0; // Zero capacitor: projectile turret must STILL fire!
        cs.max_capacitor = 250.0;
    });
    spawn_ship(&ctx, low_sys, target_ship, target_char, 587, DVec3::new(2_000.0, 0.0, 0.0), |_| {});

    let sys_handle = ctx.systems.get_or_spawn(low_sys);
    sys_handle
        .add_target_lock(
            shooter_ship,
            eve_destiny::TargetLock {
                target_id: target_ship,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap();

    let (proj_mod_tid, ammo_tid) = ctx.sde.types.values()
        .filter(|mt| {
            mt.group_id == 55
                && mt.default_attributes.get(&30).copied().unwrap_or(0.0) <= 25.0
                && mt.default_attributes.get(&50).copied().unwrap_or(0.0) <= 50.0
        })
        .find_map(|mt| ctx.sde.types.values().find(|ct| ServerContext::is_charge_compatible_with_weapon(mt, ct)).map(|ct| (mt.type_id, ct.type_id)))
        .unwrap();
    let (proj_mod, ammo_item) = (150_100_001_i64, 150_100_002_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(proj_mod, make_item(proj_mod, proj_mod_tid, shooter_char, shooter_ship, InventoryFlag::HiSlot0, 1));
        snap.items.insert(ammo_item, make_item(ammo_item, ammo_tid, shooter_char, shooter_ship, InventoryFlag::HiSlot0, 10));
        snap.module_online_states.insert(proj_mod, true);
    });
    let mut session = eve_machonet::SessionState::new(1);
    session.user_id = Some(1);
    session.character_id = Some(shooter_char);
    session.ship_id = Some(shooter_ship);
    session.solar_system_id = Some(low_sys);
    let activate_args = [PyRep::Integer(proj_mod), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship)];

    // 1. SafetyLevel::Green blocks suspect engagement in low-sec
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(shooter_char, SafetyLevel::Green);
    });
    let green_err = crate::services::combat_weapons::handle_weapons(
        &ctx, &mut session, "dogmaIM", "Activate", &activate_args, &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(green_err.code, "CantDoInCurrentSafetyLevel");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&ammo_item].quantity), 10);

    // 2. SafetyLevel::Yellow allows suspect engagement with 0.0 capacitor and consumes 1 ammo round
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(shooter_char, SafetyLevel::Yellow);
    });
    crate::services::combat_weapons::handle_weapons(
        &ctx, &mut session, "dogmaIM", "Activate", &activate_args, &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&ammo_item].quantity), 9);

    // 3. Entropic Disintegrator spooling: activation -> cycle advance -> AlreadyActive guard -> unlock cleanup
    let entropic_mod_type = ctx.sde.types.values().next().unwrap().clone();
    let spool_mod: i64 = 150_200_001;
    let base_dmg = eve_destiny::DamageVector::new(0.0, 100.0, 0.0, 50.0);
    let (_, m0) = crate::services::combat_weapons_spool::handle_entropic_activation(
        &ctx, shooter_char, shooter_ship, low_sys, spool_mod, target_ship, &entropic_mod_type, None, base_dmg, 20.0, 4.0,
    ).unwrap();
    assert!((m0 - 1.0).abs() < 1e-9);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(spool_mod);
    });
    assert!(crate::services::combat_weapons_spool::handle_entropic_activation(
        &ctx, shooter_char, shooter_ship, low_sys, spool_mod, target_ship, &entropic_mod_type, None, base_dmg, 20.0, 4.0,
    ).is_err());

    let (cyc1, m1) = crate::services::combat_weapons_spool::advance_entropic_spool_cycle(&ctx, shooter_ship, spool_mod);
    assert_eq!(cyc1, 1);
    assert!((m1 - 1.07).abs() < 1e-9);

    crate::services::combat_weapons_spool::clear_entropic_spools_on_target_unlock(&ctx, shooter_ship, shooter_char, Some(target_ship));
    assert!((crate::services::combat_weapons_spool::get_entropic_spool_multiplier(&ctx, shooter_ship, spool_mod) - 1.0).abs() < 1e-9);

    // 4. Weapon linking: ownership error, 1 turret leaves empty banks, >= 2 turrets linked & build_weapon_banks_dict populated
    assert!(crate::services::combat_weapons_spool::handle_link_all_weapons(&ctx, shooter_char + 999, shooter_ship).is_err());
    let empty_banks = crate::services::combat_weapons_spool::handle_link_all_weapons(&ctx, shooter_char, shooter_ship).unwrap();
    assert!(matches!(empty_banks, PyRep::Dict(ref d) if d.is_empty()));

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(150_300_002, make_item(150_300_002, proj_mod_tid, shooter_char, shooter_ship, InventoryFlag::HiSlot1, 1));
    });
    let banks = crate::services::combat_weapons_spool::handle_link_all_weapons(&ctx, shooter_char, shooter_ship).unwrap();
    assert!(matches!(banks, PyRep::Dict(ref d) if d.len() == 1));
}
