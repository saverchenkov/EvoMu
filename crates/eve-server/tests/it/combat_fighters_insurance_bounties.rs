#![allow(unused_imports)]
//! Integration tests for Carrier/Supercarrier Fighter Squadrons (`C-6`), Ship Insurance (`C-3`),
//! Player Bounties & Leaderboards, GM Bounty Tools, and Null-Sec ESS Main/Reserve Banks & Dynamic Bounty System.

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, SystemDynamicState};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, TargetLock};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{apply_damage_to_fighter_squadron, compute_ship_insurance_quote_levels, dispatch_combat_rpc, step_dynamic_bounty_multiplier};
use glam::DVec3;
use std::collections::HashMap;

use self::build_ctx_make_session::*;

#[tokio::test]
async fn ship_insurance_quotes_purchase_contracts_void_link_and_payout_vs_concord() {
    // parity: feat:combat.insurance-quote
    // parity: feat:combat.insurance-buy
    // parity: feat:combat.insurance-payout
    // parity: feat:ships.ship-insurance-link
    eve_testkit::attest(&["feat:combat.insurance-quote", "feat:combat.insurance-buy", "feat:combat.insurance-payout", "feat:ships.ship-insurance-link"]);

    let ctx = build_ctx();
    let lowsec_sys_id: u32 = 30_002_187; // Amamake (low-sec 0.4)
    let highsec_sys_id: u32 = 30_000_142; // Jita (high-sec 0.9)
    let killer_id: i64 = 90_000_001;
    let victim_id: i64 = 90_000_002;
    let killer_ship_id: i64 = 920_001;
    let killer_gun_id: i64 = 920_002;
    let killer_ammo_id: i64 = 920_003;
    let victim_ship_id: i64 = 920_010;
    let traded_ship_id: i64 = 920_020;

    // 1. Verify 6-tier insurance quotes (`0.5` Basic .. `1.0` Platinum) (`feat:combat.insurance-quote`)
    let (ref_val, quote_levels) = compute_ship_insurance_quote_levels(&ctx, RIFTER_TYPE_ID);
    assert!(ref_val > 0.0);
    assert_eq!(quote_levels.len(), 6);
    assert_eq!(quote_levels[0].1, "Basic");
    assert_eq!(quote_levels[5].1, "Platinum");
    assert!(quote_levels[5].3 > quote_levels[0].3);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(k) = snap.characters.get_mut(&killer_id) {
            k.station_id = None;
            k.solar_system_id = lowsec_sys_id;
            k.active_ship_id = killer_ship_id;
            k.balance = IskCents::from_isk(100_000_000);
        }
        if let Some(v) = snap.characters.get_mut(&victim_id) {
            v.station_id = None;
            v.solar_system_id = lowsec_sys_id;
            v.active_ship_id = victim_ship_id;
            v.balance = IskCents::from_isk(10_000_000);
        }
        for (sid, oid, loc) in [(killer_ship_id, killer_id, lowsec_sys_id as i64), (victim_ship_id, victim_id, lowsec_sys_id as i64), (traded_ship_id, victim_id, 60_003_760)] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: RIFTER_TYPE_ID,
                    owner_id: oid,
                    location_id: loc,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
        snap.items.insert(
            killer_gun_id,
            InventoryItemRecord {
                item_id: killer_gun_id,
                type_id: AUTOCANNON_TYPE_ID,
                owner_id: killer_id,
                location_id: killer_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            killer_ammo_id,
            InventoryItemRecord {
                item_id: killer_ammo_id,
                type_id: EMP_S_TYPE_ID,
                owner_id: killer_id,
                location_id: killer_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 200,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let mut s_killer = make_session(10, 1, killer_id, lowsec_sys_id, killer_ship_id, None);
    let mut s_victim = make_session(11, 2, victim_id, lowsec_sys_id, victim_ship_id, None);
    let kw = HashMap::new();

    // 2. Quote & buy Platinum insurance on both victim ships (`feat:combat.insurance-quote`, `feat:combat.insurance-buy`)
    let quote_rep = ctx.dispatch_rpc(&mut s_victim, "insuranceSvc", None, "GetInsurancePrice", &[PyRep::Integer(victim_ship_id)], &kw, None).await.expect("GetInsurancePrice");
    let plat_payout = quote_rep.as_f64().unwrap();
    assert!(plat_payout > 0.0);

    let insure_kv = kv_map(
        &ctx.dispatch_rpc(&mut s_victim, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(victim_ship_id), PyRep::Real(1.0)], &kw, None)
            .await
            .expect("InsureShip victim_ship_id"),
    );
    let insured_payout = insure_kv.get("payoutISK").and_then(|v| v.as_f64()).unwrap();
    assert!((insured_payout - plat_payout).abs() < 1e-2);

    ctx.dispatch_rpc(&mut s_victim, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(traded_ship_id), PyRep::Real(1.0)], &kw, None).await.expect("InsureShip traded_ship_id");

    // 3. Verify `GetContracts` lists both contracts, and repackaging/transferring `traded_ship_id` voids its contract (`feat:combat.insurance-contracts`, `feat:ships.ship-insurance-link`)
    let PyRep::List(contracts_before) = ctx.dispatch_rpc(&mut s_victim, "insuranceSvc", None, "GetContracts", &[], &kw, None).await.expect("GetContracts before repackage") else {
        panic!("Expected list from GetContracts");
    };
    assert_eq!(contracts_before.len(), 2);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&traded_ship_id) {
            it.singleton = false; // Repackaged ship voids insurance link
        }
    });
    let PyRep::List(contracts_after_repack) = ctx.dispatch_rpc(&mut s_victim, "insuranceSvc", None, "GetContracts", &[], &kw, None).await.expect("GetContracts after repackage")
    else {
        panic!("Expected list from GetContracts");
    };
    assert_eq!(contracts_after_repack.len(), 1, "Repackaging a ship must void its active insurance contract");

    // 4. Destroy `victim_ship_id` in low-sec PvP and verify insurance payout credited (`feat:combat.insurance-payout`)
    let lowsec_handle = ctx.systems.get_or_spawn(lowsec_sys_id);
    lowsec_handle
        .spawn_ball_with_combat(
            Ball {
                id: killer_ship_id,
                type_id: RIFTER_TYPE_ID,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(RIFTER_TYPE_ID),
        )
        .await
        .unwrap();
    lowsec_handle
        .spawn_ball_with_combat(
            Ball {
                id: victim_ship_id,
                type_id: RIFTER_TYPE_ID,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 400.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState { shield_hp: 0.0, max_shield_hp: 450.0, armor_hp: 0.0, max_armor_hp: 450.0, structure_hp: 5.0, max_structure_hp: 350.0, ..CombatState::default() },
        )
        .await
        .unwrap();

    let victim_bal_before_loss = ctx.db.with_snapshot(|s| s.characters[&victim_id].balance.to_wire_f64());
    let _ = ctx
        .systems
        .get_or_spawn(s_killer.solar_system_id.unwrap())
        .add_target_lock(killer_ship_id, TargetLock { target_id: victim_ship_id, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None })
        .await;
    let shot_kv = kv_map(
        &dispatch_combat_rpc(&ctx, &mut s_killer, "dogmaIM", "Activate", &[PyRep::Integer(killer_gun_id), PyRep::Integer(victim_ship_id)], &kw)
            .await
            .expect("dogmaIM.Activate kill victim ship"),
    );
    assert_eq!(shot_kv.get("destroyed"), Some(&PyRep::Bool(true)));
    let victim_bal_after_loss = ctx.db.with_snapshot(|s| s.characters[&victim_id].balance.to_wire_f64());
    assert!((victim_bal_after_loss - (victim_bal_before_loss + insured_payout)).abs() < 1.0, "Victim must receive Platinum insurance payout on legitimate PvP ship loss");

    // 5. Verify CONCORD kill of a high-sec criminal voids insurance payout (`feat:combat.insurance-payout`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(k) = snap.characters.get_mut(&killer_id) {
            k.solar_system_id = highsec_sys_id;
            k.active_ship_id = killer_ship_id;
        }
        if let Some(it) = snap.items.get_mut(&killer_ship_id) {
            it.location_id = highsec_sys_id as i64;
        }
    });
    s_killer.solar_system_id = Some(highsec_sys_id);
    ctx.dispatch_rpc(&mut s_killer, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(killer_ship_id), PyRep::Real(1.0)], &kw, None).await.expect("Insure criminal ship");

    let jita_handle = ctx.systems.get_or_spawn(highsec_sys_id);
    let victim_jita_pod: i64 = 920_099;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(v) = snap.characters.get_mut(&victim_id) {
            v.solar_system_id = highsec_sys_id;
            v.active_ship_id = victim_jita_pod;
        }
    });
    jita_handle
        .spawn_ball_with_combat(
            Ball {
                id: killer_ship_id,
                type_id: RIFTER_TYPE_ID,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(RIFTER_TYPE_ID),
        )
        .await
        .unwrap();
    jita_handle
        .spawn_ball_with_combat(
            Ball {
                id: victim_jita_pod,
                type_id: 670,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 335.0,
                inertia: 1.0,
                mass: 16_000.0,
                radius: 15.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(670),
        )
        .await
        .unwrap();

    let criminal_bal_before_concord = ctx.db.with_snapshot(|s| s.characters[&killer_id].balance.to_wire_f64());
    let concord_kv = kv_map(
        &ctx.dispatch_rpc(
            &mut s_killer,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(victim_id)],
            &kw,
            None,
        )
        .await
        .expect("Illegal high-sec attack triggers CONCORD"),
    );
    assert_eq!(concord_kv.get("criminalCharacterID"), Some(&PyRep::Integer(killer_id)));
    let criminal_bal_after_concord = ctx.db.with_snapshot(|s| s.characters[&killer_id].balance.to_wire_f64());
    assert!((criminal_bal_after_concord - criminal_bal_before_concord).abs() < 1e-2, "CONCORD destruction of a criminal ship must void insurance payout (0 ISK paid)");

    // BF-05: Verify legacy FireWeapon and CmdAttackTarget return UnimplementedRPC regardless of arity
    // rpc-lint: allow negative test (pre-existing): a non-client method must return UnimplementedRPC
    let fw_err = ctx.dispatch_rpc(&mut s_killer, "dogmaIM", None, "FireWeapon", &[PyRep::Integer(victim_id)], &kw, None)
        .await.expect_err("dogmaIM.FireWeapon must return UnimplementedRPC");
    assert_eq!(fw_err.code, "UnimplementedRPC");
    // rpc-lint: allow negative test (pre-existing): a non-client method must return UnimplementedRPC
    let cmd_err = ctx.dispatch_rpc(&mut s_killer, "beyonce", None, "CmdAttackTarget", &[PyRep::Integer(victim_id)], &kw, None)
        .await.expect_err("beyonce.CmdAttackTarget must return UnimplementedRPC");
    assert_eq!(cmd_err.code, "UnimplementedRPC");
}

#[tokio::test]
async fn player_bounties_claim_leaderboards_and_gm_bounty_tools() {
    // parity: feat:combat.player-bounties
    // parity: feat:combat.bounty-leaderboards
    // parity: feat:admin.gm-bounty-tools
    eve_testkit::attest(&["feat:combat.player-bounties", "feat:combat.bounty-leaderboards", "feat:admin.gm-bounty-tools"]);

    let ctx = build_ctx();
    let lowsec_sys_id: u32 = 30_002_187;
    let hunter_id: i64 = 90_000_001;
    let outlaw_id: i64 = 90_000_002;
    let hunter_ship_id: i64 = 930_001;
    let hunter_gun_id: i64 = 930_002;
    let hunter_ammo_id: i64 = 930_003;
    let outlaw_ship_id: i64 = 930_010;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(h) = snap.characters.get_mut(&hunter_id) {
            h.station_id = None;
            h.solar_system_id = lowsec_sys_id;
            h.active_ship_id = hunter_ship_id;
            h.balance = IskCents::from_isk(50_000_000);
        }
        if let Some(o) = snap.characters.get_mut(&outlaw_id) {
            o.station_id = None;
            o.solar_system_id = lowsec_sys_id;
            o.active_ship_id = outlaw_ship_id;
            o.balance = IskCents::from_isk(10_000_000);
        }
        for (sid, oid) in [(hunter_ship_id, hunter_id), (outlaw_ship_id, outlaw_id)] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: RIFTER_TYPE_ID,
                    owner_id: oid,
                    location_id: lowsec_sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
        snap.items.insert(
            hunter_gun_id,
            InventoryItemRecord {
                item_id: hunter_gun_id,
                type_id: AUTOCANNON_TYPE_ID,
                owner_id: hunter_id,
                location_id: hunter_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            hunter_ammo_id,
            InventoryItemRecord {
                item_id: hunter_ammo_id,
                type_id: EMP_S_TYPE_ID,
                owner_id: hunter_id,
                location_id: hunter_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 200,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let mut s_hunter = make_session(20, 1, hunter_id, lowsec_sys_id, hunter_ship_id, None);
    let mut s_outlaw = make_session(21, 2, outlaw_id, lowsec_sys_id, outlaw_ship_id, None);
    let kw = HashMap::new();

    // 1. Place bounty on `outlaw_id` and `hunter_id` (`feat:combat.player-bounties`)
    let add_kv = kv_map(
        &ctx.dispatch_rpc(&mut s_hunter, "bountyProxy", None, "AddToBounty", &[PyRep::Integer(outlaw_id), PyRep::Real(5_000_000.0)], &kw, None).await.expect("AddToBounty outlaw"),
    );
    assert_eq!(add_kv.get("totalBountyISK"), Some(&PyRep::Real(5_000_000.0)));

    ctx.dispatch_rpc(&mut s_outlaw, "bountyProxy", None, "AddToBounty", &[PyRep::Integer(hunter_id), PyRep::Real(1_000_000.0)], &kw, None).await.expect("AddToBounty hunter");

    // 2. Verify bounty leaderboards rank `outlaw_id` (5M) above `hunter_id` (1M) (`feat:combat.bounty-leaderboards`)
    let PyRep::List(top_bounties) = ctx.dispatch_rpc(&mut s_hunter, "bountyProxy", None, "GetTopPilotBounties", &[], &kw, None).await.expect("GetTopPilotBounties") else {
        panic!("Expected list from GetTopPilotBounties");
    };
    assert_eq!(top_bounties.len(), 2);
    assert_eq!(kv_map(&top_bounties[0]).get("targetID"), Some(&PyRep::Integer(outlaw_id)));

    // 3. Destroy `outlaw_ship_id` and verify bounty payout (20% of destroyed ship reference value) (`feat:combat.player-bounties`)
    let lowsec_handle = ctx.systems.get_or_spawn(lowsec_sys_id);
    lowsec_handle
        .spawn_ball_with_combat(
            Ball {
                id: hunter_ship_id,
                type_id: RIFTER_TYPE_ID,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(RIFTER_TYPE_ID),
        )
        .await
        .unwrap();
    lowsec_handle
        .spawn_ball_with_combat(
            Ball {
                id: outlaw_ship_id,
                type_id: RIFTER_TYPE_ID,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 400.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState { shield_hp: 0.0, max_shield_hp: 450.0, armor_hp: 0.0, max_armor_hp: 450.0, structure_hp: 5.0, max_structure_hp: 350.0, ..CombatState::default() },
        )
        .await
        .unwrap();

    let hunter_bal_before = ctx.db.with_snapshot(|s| s.characters[&hunter_id].balance.to_wire_f64());
    let _ = ctx
        .systems
        .get_or_spawn(s_hunter.solar_system_id.unwrap())
        .add_target_lock(hunter_ship_id, TargetLock { target_id: outlaw_ship_id, lock_time_ms: 100.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None })
        .await;
    dispatch_combat_rpc(&ctx, &mut s_hunter, "dogmaIM", "Activate", &[PyRep::Integer(hunter_gun_id), PyRep::Integer(outlaw_ship_id)], &kw).await.expect("Destroy outlaw ship");
    let hunter_bal_after = ctx.db.with_snapshot(|s| s.characters[&hunter_id].balance.to_wire_f64());
    assert!(hunter_bal_after > hunter_bal_before, "Bounty hunter must receive 20% ship value bounty payout");

    let res = ctx.dispatch_rpc(&mut s_hunter, "bountyProxy", None, "GetTopPilotBountyHunters", &[], &kw, None).await.expect("GetTopPilotBountyHunters");
    let top_hunters = match res {
        PyRep::Tuple(t) => match t.into_iter().next() {
            Some(PyRep::List(l)) => l,
            _ => panic!("Expected list in tuple"),
        },
        PyRep::List(l) => l,
        _ => panic!("Expected list from GetTopPilotBountyHunters"),
    };
    assert_eq!(kv_map(&top_hunters[0]).get("targetID"), Some(&PyRep::Integer(hunter_id)));

    // 4. Verify GM Bounty Tools (`feat:admin.gm-bounty-tools`)
    let non_gm_err = ctx.dispatch_rpc(&mut s_hunter, "bountyProxy", None, "GMReimburseBounties", &[PyRep::Integer(outlaw_id)], &kw, None).await.unwrap_err();
    assert_eq!(non_gm_err.code, "InsufficientRole");

    s_hunter.role |= service_roles::ROLE_GMH;
    let reimburse_kv =
        kv_map(&ctx.dispatch_rpc(&mut s_hunter, "bountyProxy", None, "GMReimburseBounties", &[PyRep::Integer(outlaw_id)], &kw, None).await.expect("GMReimburseBounties"));
    assert_eq!(reimburse_kv.get("amountISK"), Some(&PyRep::Real(4_920_000.0)));

    let clear_kv = kv_map(&ctx.dispatch_rpc(&mut s_hunter, "bountyProxy", None, "ClearBounty", &[PyRep::Integer(hunter_id)], &kw, None).await.expect("ClearBounty"));
    assert_eq!(clear_kv.get("amountISK"), Some(&PyRep::Real(1_000_000.0)));
}

#[tokio::test]
async fn nullsec_ess_main_bank_reserve_security_key_and_dynamic_bounty_multiplier() {
    // parity: feat:pve.ess
    // parity: feat:pve.ess-reserve
    // parity: feat:pve.dynamic-bounty
    eve_testkit::attest(&["feat:pve.ess", "feat:pve.ess-reserve", "feat:pve.dynamic-bounty"]);

    let ctx = build_ctx();
    let nullsec_sys_id: u32 = 30_004_759; // VFK-IV (K-space null-sec)
    let char_id: i64 = 90_000_001;
    let ship_id: i64 = 940_001;
    let ess_ball_id: i64 = 940_010;
    let key_item_id: i64 = 940_020;

    // 1. Verify Dynamic Bounty System (`dbs_multiplier`) decay under ratting and recovery on PvP kills (`feat:pve.dynamic-bounty`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            nullsec_sys_id,
            SystemDynamicState {
                solar_system_id: nullsec_sys_id,
                dbs_multiplier: 1.35,
                ess_main_bank: IskCents::from_isk(25_000_000),
                ess_reserve_bank: IskCents::from_isk(40_000_000),
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                sov_alliance_id: None,
                military_index: 1,
                industrial_index: 1,
                strategic_index: 1,
                fw_contested_pct: 0.0,
                insurgency_corruption_stage: 0,
                insurgency_suppression_stage: 0,
                wormhole_remaining_mass_kg: 0.0,
                wormhole_destination_system_id: None,
                signature_position: None,
                fw_complex_position: None,
                fw_capture_seconds_elapsed: 0,
                fw_capture_duration_seconds: 0,
                last_fw_capture_tick: 0,
                last_sov_claim_tick: 0, ess_cycle_started_unix: 0, ess_main_bank_contributors: Default::default(),
            },
        );
    });
    let after_ratting = step_dynamic_bounty_multiplier(&ctx, nullsec_sys_id, 100_000_000.0, 0.0);
    assert!(after_ratting < 1.35 && after_ratting >= 0.50, "Ratting volume must decay dbs_multiplier toward 0.50 floor");
    let after_pvp = step_dynamic_bounty_multiplier(&ctx, nullsec_sys_id, 0.0, 75_000_000.0);
    assert!(after_pvp > after_ratting && after_pvp <= 2.00, "Capsuleer PvP destruction must boost dbs_multiplier toward 2.00 ceiling");

    // 2. Seed ship and ESS beacon in VFK-IV (`feat:pve.ess`, `feat:pve.ess-reserve`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = nullsec_sys_id;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(10_000_000);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: RIFTER_TYPE_ID,
                owner_id: char_id,
                location_id: nullsec_sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(nullsec_sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: RIFTER_TYPE_ID,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(RIFTER_TYPE_ID),
        )
        .await
        .unwrap();
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ess_ball_id,
                type_id: ESS_BEACON_TYPE_ID,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(ESS_BEACON_TYPE_ID),
        )
        .await
        .unwrap();

    let mut s1 = make_session(30, 1, char_id, nullsec_sys_id, ship_id, None);
    let kw = HashMap::new();

    // 3. Link to ESS Main Bank, step 5 Destiny ticks, and steal 25M ISK (`feat:pve.ess`)
    let main_start =
        kv_map(&ctx.dispatch_rpc(&mut s1, "essMgr", None, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)], &kw, None).await.expect("AttemptLinkToMainBank start"));
    assert_eq!(main_start.get("linkStartedOnly"), Some(&PyRep::Bool(true)));

    for _ in 0..5 {
        let _ = sys_handle.step_manual(1.0).await.unwrap();
    }
    let main_complete =
        kv_map(&ctx.dispatch_rpc(&mut s1, "essMgr", None, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball_id)], &kw, None).await.expect("AttemptLinkToMainBank complete"));
    assert_eq!(main_complete.get("stolenISK"), Some(&PyRep::Real(25_000_000.0)));

    // 4. Verify `AttemptLinkToReserveBank` rejects without a Reserve Security Key, then consumes key and pays out (`feat:pve.ess-reserve`)
    let no_key_err = ctx.dispatch_rpc(&mut s1, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)], &kw, None).await.unwrap_err();
    assert_eq!(no_key_err.code, "MissingEssReserveSecurityKey");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            key_item_id,
            InventoryItemRecord {
                item_id: key_item_id,
                type_id: ESS_RESERVE_KEY_TYPE_ID,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let res_start = kv_map(
        &ctx.dispatch_rpc(&mut s1, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)], &kw, None).await.expect("AttemptLinkToReserveBank start with key"),
    );
    assert_eq!(res_start.get("linkStartedOnly"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&key_item_id)), "ESS Reserve Security Key must be consumed when starting Reserve Bank link");

    for _ in 0..5 {
        let _ = sys_handle.step_manual(1.0).await.unwrap();
    }
    let res_complete =
        kv_map(&ctx.dispatch_rpc(&mut s1, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_ball_id)], &kw, None).await.expect("AttemptLinkToReserveBank complete"));
    let stolen = match res_complete.get("stolenISK") {
        Some(PyRep::Real(v)) => *v,
        other => panic!("expected Real stolenISK, got {:?}", other),
    };
    let expected_p1 = (eve_server::services::pve_ess_reserve_banks::compute_ess_reserve_pulse_payout_cents(
        4_000_000_000,
        15,
        15,
    ) as f64)
        / 100.0;
    assert!((stolen - expected_p1).abs() < 0.1, "stolen: {stolen}, expected: {expected_p1}");
}

pub(crate) mod build_ctx_make_session {
//! Integration tests for Carrier/Supercarrier Fighter Squadrons (`C-6`), Ship Insurance (`C-3`),
//! Player Bounties & Leaderboards, GM Bounty Tools, and Null-Sec ESS Main/Reserve Banks & Dynamic Bounty System.

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, SystemDynamicState};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, TargetLock};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    apply_damage_to_fighter_squadron, compute_ship_insurance_quote_levels, dispatch_combat_rpc, step_dynamic_bounty_multiplier,
};
use glam::DVec3;
use std::collections::HashMap;

pub(crate) const ARCHON_CARRIER_TYPE_ID: u32 = 23757; // Archon Carrier
pub(crate) const DRAGONFLY_FIGHTER_TYPE_ID: u32 = 23057; // Dragonfly I Light Fighter (max squadron size 9)
pub(crate) const RIFTER_TYPE_ID: u32 = 587; // Rifter Frigate
pub(crate) const AUTOCANNON_TYPE_ID: u32 = 2889; // 200mm AutoCannon I
pub(crate) const EMP_S_TYPE_ID: u32 = 185; // EMP S
pub(crate) const GURISTAS_PIRATE_TYPE_ID: u32 = 1001; // Guristas Pirate NPC
pub(crate) const ESS_BEACON_TYPE_ID: u32 = 32882; // Encounter Surveillance System
pub(crate) const ESS_RESERVE_KEY_TYPE_ID: u32 = 57720; // ESS-SW-15 Reserve Security Key (group 4102, Delve)

pub(crate) fn build_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx
}

pub(crate) fn make_session(session_id: u64, user_id: u64, char_id: i64, sys_id: u32, ship_id: i64, station_id: Option<i64>) -> SessionState {
    let mut s = SessionState::new(session_id);
    s.user_id = Some(user_id);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1_000_009);
    s.solar_system_id = Some(sys_id);
    s.constellation_id = Some(20_000_020);
    s.region_id = Some(10_000_002);
    s.ship_id = Some(ship_id);
    s.station_id = station_id;
    s.role = service_roles::ROLE_PLAYER | service_roles::ROLE_LOGIN;
    s
}

pub(crate) fn kv_map(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.clone(),
        PyRep::Dict(entries) => entries
            .iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(s) | PyRep::Token(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect(),
        _ => panic!("Expected KeyVal/Dict, got {rep:?}"),
    }
}

#[tokio::test]
async fn carrier_fighter_tubes_launch_movement_abilities_losses_and_abandon() {
    // parity: feat:drones.fighter-squadron-losses
    eve_testkit::attest(&["feat:drones.fighter-squadron-losses"]);

    let ctx = build_ctx();
    crate::common::pin_clock(&ctx);
    let sys_id: u32 = 30_000_142;
    let char_id: i64 = 90_000_001;
    let carrier_id: i64 = 910_001;
    let squadron_item_id: i64 = 910_010;
    let spare_fighters_item_id: i64 = 910_011;
    let npc_ball_id: i64 = 910_050;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = carrier_id;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        snap.items.insert(
            carrier_id,
            InventoryItemRecord {
                item_id: carrier_id,
                type_id: ARCHON_CARRIER_TYPE_ID,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Primary squadron of 9 Dragonfly I fighters in FighterBay
        snap.items.insert(
            squadron_item_id,
            InventoryItemRecord {
                item_id: squadron_item_id,
                type_id: DRAGONFLY_FIGHTER_TYPE_ID,
                owner_id: char_id,
                location_id: carrier_id,
                flag: InventoryFlag::FighterBay,
                quantity: 9,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Reserve stack of 5 Dragonfly I fighters in FighterBay for replacement refill upon recall
        snap.items.insert(
            spare_fighters_item_id,
            InventoryItemRecord {
                item_id: spare_fighters_item_id,
                type_id: DRAGONFLY_FIGHTER_TYPE_ID,
                owner_id: char_id,
                location_id: carrier_id,
                flag: InventoryFlag::FighterBay,
                quantity: 5,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: carrier_id,
                type_id: ARCHON_CARRIER_TYPE_ID,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 85.0,
                inertia: 1.0,
                mass: 1_200_000_000.0,
                radius: 1_500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(ARCHON_CARRIER_TYPE_ID),
        )
        .await
        .unwrap();
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: npc_ball_id,
                type_id: GURISTAS_PIRATE_TYPE_ID,
                position: DVec3::new(3_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 45.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 5_000.0,
                max_shield_hp: 5_000.0,
                armor_hp: 5_000.0,
                max_armor_hp: 5_000.0,
                structure_hp: 5_000.0,
                max_structure_hp: 5_000.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();

    let mut s1 = make_session(1, 1, char_id, sys_id, carrier_id, None);
    let kw = HashMap::new();

    // 1. Load fighter squadron into Tube 0 (`feat:drones.fighter-load-tubes`), unload back to FighterBay, then reload
    let load_res = ctx
        .dispatch_rpc(&mut s1, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(squadron_item_id)], &kw, None)
        .await
        .expect("LoadFightersToTube");
    let load_kv = kv_map(&load_res);
    assert_eq!(load_kv.get("tubeIndex"), Some(&PyRep::Integer(0)));
    assert_eq!(load_kv.get("fighterID"), Some(&PyRep::Integer(squadron_item_id)));

    let unload_res =
        ctx.dispatch_rpc(&mut s1, "fighterMgr", None, "UnloadTubeToFighterBay", &[PyRep::Integer(0)], &kw, None).await.expect("UnloadTubeToFighterBay");
    assert_eq!(unload_res, PyRep::Bool(true));

    ctx.dispatch_rpc(&mut s1, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(squadron_item_id)], &kw, None)
        .await
        .expect("Reload fighters into Tube 0");

    // 2. Launch fighters from Tube 0 into space (`feat:drones.fighter-launch-recall`)
    let launch_res = ctx
        .dispatch_rpc(&mut s1, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None)
        .await
        .expect("LaunchFightersFromTubes");
    let PyRep::List(launched_list) = launch_res else {
        panic!("Expected list from LaunchFightersFromTubes");
    };
    assert_eq!(launched_list.len(), 1);
    let fighter_ball_id = launched_list[0].as_i64().unwrap();
    assert!(sys_handle.get_ball_snapshot(fighter_ball_id).await.unwrap().is_some());

    // 3. Command fighter movement in space (`feat:drones.fighter-movement`)
    let move_res = ctx
        .dispatch_rpc(
            &mut s1,
            "fighterMgr",
            None,
            "ExecuteMovementCommandOnFighters",
            &[PyRep::List(vec![PyRep::Integer(fighter_ball_id)]), PyRep::String("ORBIT".into()), PyRep::Integer(npc_ball_id), PyRep::Real(2_500.0)],
            &kw,
            None,
        )
        .await
        .expect("ExecuteMovementCommandOnFighters");
    assert_eq!(move_res, PyRep::Bool(true));

    // Lock target on the carrier so targeted fighter abilities can engage (`feat:drones.fighter-abilities`)
    sys_handle
        .add_target_lock(
            carrier_id,
            TargetLock { target_id: npc_ball_id, locked: true, lock_time_ms: 1_000.0, remaining_lock_ms: 0.0, max_range_m: Some(100_000.0) },
        )
        .await
        .unwrap();

    // 4. Activate Fighter Ability Slots 0 (Attack) and 2 (Heavy Rocket Salvo) (`feat:drones.fighter-abilities`)
    let ab0 = kv_map(
        &ctx.dispatch_rpc(
            &mut s1,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::List(vec![PyRep::Integer(fighter_ball_id)]), PyRep::Integer(0), PyRep::Integer(npc_ball_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots slot 0"),
    );
    assert_eq!(ab0.get("success"), Some(&PyRep::Bool(true)));

    let ab2 = kv_map(
        &ctx.dispatch_rpc(
            &mut s1,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::List(vec![PyRep::Integer(fighter_ball_id)]), PyRep::Integer(2), PyRep::Integer(npc_ball_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots slot 2"),
    );
    assert_eq!(ab2.get("success"), Some(&PyRep::Bool(true)));
    let charge_count = ab2.get("chargeCount").and_then(|v| v.as_i64()).unwrap_or(0);
    let rem_charges = ab2.get("remainingCharges").and_then(|v| v.as_i64()).unwrap_or(0);
    assert_eq!(rem_charges, charge_count - 1);

    // 5. Apply damage to fighter squadron and verify per-unit losses (`feat:drones.fighter-squadron-losses`)
    let (units_lost, remaining_size, sq_destroyed) =
        apply_damage_to_fighter_squadron(&ctx, sys_id, fighter_ball_id, DamageVector { em: 5_000.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 }).await;
    assert_eq!(units_lost, 1, "Fighter squadron must lose 1 unit when damaged past unit_hp");
    assert_eq!(remaining_size, 6 - units_lost);
    assert!(!sq_destroyed);

    // 6. Recall fighters to tube (`feat:drones.fighter-launch-recall`, `feat:drones.fighter-squadron-losses`)
    // Verify squadron refills lost units from FighterBay reserve back to max squadron size (6) and rearms Slot 2 charges
    let recall_res = ctx
        .dispatch_rpc(&mut s1, "fighterMgr", None, "RecallFightersToTubes", &[PyRep::List(vec![PyRep::Integer(fighter_ball_id)])], &kw, None)
        .await
        .expect("RecallFightersToTubes");
    let PyRep::List(recalled_list) = recall_res else {
        panic!("Expected list from RecallFightersToTubes");
    };
    assert_eq!(recalled_list.len(), 1);
    let sq_qty_after_recall = ctx.db.with_snapshot(|snap| snap.items.get(&squadron_item_id).unwrap().quantity);
    assert_eq!(sq_qty_after_recall, 6, "Recalled squadron must refill lost units from FighterBay reserve back to max squadron size");
    assert!(ctx.fighter_charge_state.read().unwrap().is_empty(), "Recalled squadron must rearm ability charges");

    // 7. Re-launch and abandon in space (`feat:drones.fighter-abandon`)
    let relaunch_early = ctx.dispatch_rpc(&mut s1, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None).await;
    assert_eq!(relaunch_early.unwrap_err().code, "TubeRearming");
    ctx.advance_virtual_clock(16);

    let relaunch_res = ctx
        .dispatch_rpc(&mut s1, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None)
        .await
        .expect("Re-launch fighters");
    let PyRep::List(relaunched) = relaunch_res else {
        panic!("Expected list");
    };
    let relaunched_ball_id = relaunched[0].as_i64().unwrap();

    let PyRep::List(abandoned_list) =
        ctx.dispatch_rpc(&mut s1, "fighterMgr", None, "CmdAbandonFighter", &[PyRep::Integer(relaunched_ball_id)], &kw, None).await.expect("CmdAbandonFighter")
    else {
        panic!("Expected list from CmdAbandonFighter");
    };
    assert_eq!(abandoned_list, vec![PyRep::Integer(relaunched_ball_id)]);
    assert!(ctx.active_fighters.read().unwrap().get(&relaunched_ball_id).unwrap().is_abandoned);
}

}

// Folded submodule: parity_crimewatch_insurance_closure
#[allow(unused_imports, dead_code)]
pub(crate) mod crimewatch_kill_rights_and_sec_status {
    use super::*;

// End-to-end closure tests for Crimewatch & Insurance `feat:*` rows:
// - `feat:crimewatch.kill-rights-gain`
// - `feat:crimewatch.kill-rights-sell`
// - `feat:crimewatch.sec-status-gain-caps`
// - `feat:crimewatch.sec-status-loss`
// - `feat:crimewatch.sec-status-transactions`
// - `feat:crimewatch.sec-tag-tier-bands`
// - `feat:combat.insurance-void-and-expiry`


use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::crimewatch::{
    compute_sec_status_loss, AggressionEvaluation, CrimeWatchDirector,
};
use eve_server::combat::death_pipeline::execute_death_pipeline;
use eve_server::combat::npc_ai::{NpcAiState, NpcCombatEntity, NpcSpawnManager};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, String> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
        .map_err(|e| format!("{e:?}"))
}

fn field(rep: &PyRep, key: &str) -> PyRep {
    rep.dict_get(key).cloned().unwrap_or(PyRep::None)
}

fn spawn_ship(
    ctx: &ServerContext,
    owner_id: i64,
    sys_id: u32,
    type_id: u32,
    _pos: DVec3,
) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let sid = snap.next_entity_id;
        snap.items.insert(
            sid,
            InventoryItemRecord {
                item_id: sid,
                type_id,
                owner_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if type_id != 670 {
            snap.next_entity_id += 1;
            let gun_id = snap.next_entity_id;
            snap.items.insert(
                gun_id,
                InventoryItemRecord {
                    item_id: gun_id,
                    type_id: 487,
                    owner_id,
                    location_id: sid,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(gun_id, true);
            snap.next_entity_id += 1;
            let ammo_id = snap.next_entity_id;
            snap.items.insert(
                ammo_id,
                InventoryItemRecord {
                    item_id: ammo_id,
                    type_id: 183,
                    owner_id,
                    location_id: sid,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 100,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.active_ship_id = sid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
        }
        sid
    })
}

#[tokio::test]
async fn test_e2e_crimewatch_kill_rights_gain_and_sell() {
    // parity: feat:crimewatch.kill-rights-gain, feat:crimewatch.kill-rights-sell
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();

    let highsec_sys = 30_000_142_u32;
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.0 && s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_510);
    let victim_id = 90_000_001_i64;
    let criminal_id = 90_000_002_i64;
    let buyer_id = 90_000_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&buyer_id) {
            if let Some(mut c3) = snap.characters.get(&victim_id).cloned() {
                c3.character_id = buyer_id;
                c3.name = "Buyer Pilot".to_string();
                snap.characters.insert(buyer_id, c3);
            }
        }
        for (cid, uid, bal) in [
            (victim_id, 1, 10_000_000.0),
            (criminal_id, 2, 10_000_000.0),
            (buyer_id, 3, 20_000_000.0),
        ] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.user_id = uid;
                ch.balance = IskCents::from_f64_isk(bal);
                ch.solar_system_id = lowsec_sys;
                ch.station_id = None;
            }
        }
    });

    // 1. Low-sec ship aggression via dogmaIM.Activate deducts sec status but grants NO kill right;
    //    subsequent low-sec capsule (pod) aggression grants 1 private 30-day kill right.
    let lowsec_handle = ctx.systems.get_or_spawn(lowsec_sys);
    let low_crim_ship = spawn_ship(&ctx, criminal_id, lowsec_sys, 587, DVec3::new(1_000.0, 0.0, 0.0));
    let low_vic_ship = spawn_ship(&ctx, victim_id, lowsec_sys, 587, DVec3::new(500.0, 0.0, 0.0));
    let low_vic_pod = spawn_ship(&ctx, victim_id, lowsec_sys, 670, DVec3::ZERO);
    for (id, tid, pos) in [
        (low_crim_ship, 587_u32, DVec3::new(1_000.0, 0.0, 0.0)),
        (low_vic_ship, 587_u32, DVec3::new(500.0, 0.0, 0.0)),
        (low_vic_pod, 670_u32, DVec3::ZERO),
    ] {
        let mut cs = ctx.build_type_combat_state(tid);
        cs.shield_hp = 500.0;
        let _ = lowsec_handle
            .spawn_ball_with_combat(
                Ball {
                    id,
                    type_id: tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
    }

    let gun_id = ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&victim_id).unwrap().active_ship_id = low_vic_ship;
        s.items
            .values()
            .find(|i| i.location_id == low_crim_ship && i.type_id == 487)
            .unwrap()
            .item_id
    });
    for target in [low_vic_ship, low_vic_pod] {
        let _ = lowsec_handle
            .add_target_lock(
                low_crim_ship,
                eve_destiny::TargetLock {
                    target_id: target,
                    lock_time_ms: 0.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await;
    }

    let mut sess_crim_low = SessionState::default();
    sess_crim_low.user_id = Some(2);
    sess_crim_low.character_id = Some(criminal_id);
    sess_crim_low.solar_system_id = Some(lowsec_sys);
    sess_crim_low.ship_id = Some(low_crim_ship);

    let _ = rpc(
        &ctx,
        &mut sess_crim_low,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(low_vic_ship),
        ],
    )
    .await
    .expect("Activate lowsec ship");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.kill_rights.len()),
        0,
        "Low-sec ship aggression must not grant a kill right"
    );

    for _ in 0..10 {
        let _ = lowsec_handle.step_manual(1.0).await;
    }
    let _ = rpc(
        &ctx,
        &mut sess_crim_low,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(gun_id)],
    )
    .await;
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&victim_id).unwrap().active_ship_id = low_vic_pod;
    });
    let _ = rpc(
        &ctx,
        &mut sess_crim_low,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(low_vic_pod),
        ],
    )
    .await
    .expect("Activate lowsec pod");

    let (kr_count_pod, low_pod_kr_id) = ctx.db.with_snapshot(|s| {
        let krs: Vec<_> = s
            .kill_rights
            .values()
            .filter(|kr| kr.owner_id == victim_id && kr.target_id == criminal_id)
            .collect();
        (krs.len(), krs[0].kill_right_id)
    });
    assert_eq!(kr_count_pod, 1, "Low-sec pod aggression must grant 1 kill right");

    // 2. High-sec criminal aggression triggers CONCORD arrival and grants a 30-day private kill right.
    ctx.db.with_mut_snapshot(|s| {
        s.kill_rights.remove(&low_pod_kr_id);
        for cid in [victim_id, criminal_id, buyer_id] {
            if let Some(ch) = s.characters.get_mut(&cid) {
                ch.solar_system_id = highsec_sys;
                ch.station_id = None;
            }
        }
    });
    let victim_ship = spawn_ship(&ctx, victim_id, highsec_sys, 587, DVec3::ZERO);
    let crim_ship = spawn_ship(&ctx, criminal_id, highsec_sys, 587, DVec3::new(1_000.0, 0.0, 0.0));
    let _buyer_ship = spawn_ship(&ctx, buyer_id, highsec_sys, 587, DVec3::new(2_000.0, 0.0, 0.0));
    let sys_handle = ctx.systems.get_or_spawn(highsec_sys);
    for (id, pos) in [(victim_ship, DVec3::ZERO), (crim_ship, DVec3::new(1_000.0, 0.0, 0.0))] {
        let mut cs = ctx.build_type_combat_state(587);
        cs.shield_hp = 500.0;
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id,
                    type_id: 587,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
    }

    let mut cw = CrimeWatchDirector::new();
    let hi_eval = cw.handle_combat_aggression_with_pod(
        criminal_id,
        victim_id,
        crim_ship,
        highsec_sys,
        0.95,
        false,
        false,
        now,
    );
    assert!(matches!(hi_eval, AggressionEvaluation::CriminalFlagTriggered { .. }));
    let reports = cw.step_concord(&ctx, now + 10, 42).await;
    assert_eq!(reports.len(), 1);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&criminal_id) {
            ch.station_id = None;
            ch.solar_system_id = highsec_sys;
        }
    });
    let kr_id = reports[0].kill_right_id;
    assert!(kr_id > 0);

    // 3. SellKillRight -> CancelSellKillRight -> SellKillRight -> BuyKillRight (15-min suspect + ISK transfer)
    let mut sess_victim = SessionState::default();
    sess_victim.user_id = Some(1);
    sess_victim.character_id = Some(victim_id);
    sess_victim.solar_system_id = Some(highsec_sys);

    let listed = rpc(
        &ctx,
        &mut sess_victim,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(2_500_000.0)],
    )
    .await
    .expect("SellKillRight");
    if let PyRep::List(items) = listed {
        assert_eq!(field(&items[0], "forSale"), PyRep::Bool(true));
        assert_eq!(field(&items[0], "price"), PyRep::Real(2_500_000.0));
    } else {
        panic!("expected List from SellKillRight");
    }

    let cancelled = rpc(
        &ctx,
        &mut sess_victim,
        "bountyProxy",
        "CancelSellKillRight",
        &[PyRep::Integer(kr_id)],
    )
    .await
    .expect("CancelSellKillRight");
    assert_eq!(field(&cancelled, "forSale"), PyRep::Bool(false));

    let _ = rpc(
        &ctx,
        &mut sess_victim,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(2_500_000.0)],
    )
    .await
    .expect("SellKillRight again");

    let mut sess_buyer = SessionState::default();
    sess_buyer.user_id = Some(3);
    sess_buyer.character_id = Some(buyer_id);
    sess_buyer.solar_system_id = Some(highsec_sys);

    let bought = rpc(
        &ctx,
        &mut sess_buyer,
        "killRightMgr",
        "BuyKillRight",
        &[PyRep::Integer(kr_id)],
    )
    .await
    .expect("BuyKillRight");
    assert_eq!(field(&bought, "suspectFlagActive"), PyRep::Bool(true));
    assert_eq!(field(&bought, "feePaidISK"), PyRep::Real(2_500_000.0));

    let (victim_bal, buyer_bal) = ctx.db.with_snapshot(|s| {
        (
            s.characters.get(&victim_id).unwrap().balance.to_wire_f64(),
            s.characters.get(&buyer_id).unwrap().balance.to_wire_f64(),
        )
    });
    assert!((victim_bal - 12_500_000.0).abs() < 1e-2);
    assert!((buyer_bal - 17_500_000.0).abs() < 1e-2);
}

#[tokio::test]
async fn test_e2e_crimewatch_sec_status_loss_caps_tags_and_transactions() {
    // parity: feat:crimewatch.sec-status-loss, feat:crimewatch.sec-status-gain-caps, feat:crimewatch.sec-status-transactions, feat:crimewatch.sec-tag-tier-bands
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();

    let char_id = 90_000_001_i64;
    let victim_id = 90_000_002_i64;
    let security_office_svc_id = ctx
        .sde
        .station_services
        .values()
        .find(|s| s.name == "Security Office")
        .map(|s| s.service_id)
        .unwrap_or(26);
    let (lowsec_station_id, lowsec_sys_id) = ctx
        .sde
        .stations
        .values()
        .find(|st| {
            let is_concord = matches!(st.owner_corporation_id, 1_000_125 | 1_000_137)
                || st.owner_faction_id == Some(500_006);
            let is_lowsec = ctx
                .sde
                .solar_systems
                .get(&st.solar_system_id)
                .is_some_and(|s| s.security_status > 0.0 && s.security_status < 0.45);
            let has_office = ctx
                .sde
                .station_operations
                .get(&st.operation_id)
                .is_some_and(|op| op.services.contains(&security_office_svc_id));
            is_concord && is_lowsec && has_office
        })
        .map(|st| (st.station_id, st.solar_system_id))
        .expect("low-sec CONCORD Security Office station in SDE");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = 1;
            ch.security_status = 0.0;
            ch.balance = IskCents::from_f64_isk(50_000_000.0);
            ch.station_id = None;
            ch.solar_system_id = 30_000_142;
        }
        if let Some(vic) = snap.characters.get_mut(&victim_id) {
            vic.user_id = 2;
            vic.security_status = 5.0;
            vic.station_id = None;
            vic.solar_system_id = 30_000_142;
        }
        for &tag_type in &[33138_u32, 33139, 33140, 33141] {
            snap.next_entity_id += 1;
            let iid = snap.next_entity_id;
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tag_type,
                    owner_id: char_id,
                    location_id: lowsec_station_id as i64,
                    flag: InventoryFlag::Hangar,
                    quantity: 5,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
    });

    // 1. High-sec illegal PvP via dogmaIM.Activate deducts sec status once;
    //    CONCORD arrival + ship destruction must NOT double-deduct.
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    let att_ship = spawn_ship(&ctx, char_id, 30_000_142, 587, DVec3::new(1_000.0, 0.0, 0.0));
    let vic_ship = spawn_ship(&ctx, victim_id, 30_000_142, 587, DVec3::ZERO);
    for (id, pos) in [(att_ship, DVec3::new(1_000.0, 0.0, 0.0)), (vic_ship, DVec3::ZERO)] {
        let mut cs = ctx.build_type_combat_state(587);
        cs.shield_hp = 500.0;
        let _ = jita_handle
            .spawn_ball_with_combat(
                Ball {
                    id,
                    type_id: 587,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
    }
    let gun_id = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|i| i.location_id == att_ship && i.type_id == 487)
            .unwrap()
            .item_id
    });
    let _ = jita_handle
        .add_target_lock(
            att_ship,
            eve_destiny::TargetLock {
                target_id: vic_ship,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;

    let mut sess_att = SessionState::default();
    sess_att.user_id = Some(1);
    sess_att.character_id = Some(char_id);
    sess_att.solar_system_id = Some(30_000_142);
    sess_att.ship_id = Some(att_ship);

    let jita_sec = ctx.sde.solar_systems[&30_000_142].security_status;
    let expected_jita_loss = compute_sec_status_loss(0.0, 5.0, false, jita_sec);

    let _ = rpc(
        &ctx,
        &mut sess_att,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(vic_ship),
        ],
    )
    .await
    .expect("Activate highsec ship");

    let mut cw_concord = CrimeWatchDirector::new();
    cw_concord.states = ctx.crimewatch.read().unwrap().states.clone();
    let _ = cw_concord.step_concord(&ctx, now + 10, 99).await;
    let _ = execute_death_pipeline(
        &ctx,
        30_000_142,
        victim_id,
        vic_ship,
        char_id,
        DVec3::ZERO,
        7,
    )
    .await;

    let sec_after_aggr = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!(
        (sec_after_aggr - (-expected_jita_loss)).abs() < 1e-6,
        "High-sec PvP + CONCORD + kill must deduct sec status once ({} vs {})",
        sec_after_aggr,
        -expected_jita_loss
    );

    // 2. NPC kill sec-status gain caps at +5.0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = 4.99;
        }
    });
    let mut npc_dir = NpcSpawnManager::new();
    let make_npc = |ball_id: i64| NpcCombatEntity {
        ball_id,
        type_id: 12000,
        faction_id: 500_010,
        state: NpcAiState::Idle,
        spawn_group_id: 1,
        spawn_pos: DVec3::ZERO,
        target_id: None,
        bounty_isk: 10_000_000.0,
        optimal_range: 10_000.0,
        orbit_range: 5_000.0,
        base_dps: 50.0,
        damage_vector: DamageVector {
            em: 10.0,
            thermal: 10.0,
            kinetic: 15.0,
            explosive: 15.0,
        },
        last_attack_tick: 0,
        has_shield_rep: false,
        has_armor_rep: false,
    };
    npc_dir.entities.insert(80_000_001, make_npc(80_000_001));
    let _ = npc_dir
        .handle_npc_destroyed(&ctx, lowsec_sys_id, 80_000_001, char_id, now)
        .expect("handle_npc_destroyed");
    let sec_after_rat = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!(sec_after_rat > 4.99 && sec_after_rat <= 5.0);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = 5.0;
        }
    });
    npc_dir.entities.insert(80_000_002, make_npc(80_000_002));
    let _ = npc_dir
        .handle_npc_destroyed(&ctx, lowsec_sys_id, 80_000_002, char_id, now)
        .expect("handle_npc_destroyed at cap");
    assert!((ctx.db.with_snapshot(|s| s.characters[&char_id].security_status) - 5.0).abs() < 1e-6);

    // 3. All 4 Clone Soldier Tag bands, high-sec rejection, ISK fees, and 0.0 cap
    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = -9.0;
            ch.station_id = Some(60_003_760);
            ch.solar_system_id = 30_000_142;
        }
    });
    let hi_err = rpc(
        &ctx,
        &mut sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Real(-8.5), PyRep::Dict(vec![(PyRep::Integer(33138), PyRep::Integer(1))])],
    )
    .await
    .unwrap_err();
    assert!(hi_err.contains("SecurityOfficeRequired"));

    sess.station_id = Some(lowsec_station_id);
    sess.solar_system_id = Some(lowsec_sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(lowsec_station_id);
        ch.solar_system_id = lowsec_sys_id;
    });

    for (start_sec, valid_tag, invalid_tag, req_qty, target_sec, expected_sec) in [
        (-9.0_f64, 33138_i64, 33139_i64, 1_i64, -8.5_f64, -8.5_f64),
        (-6.5_f64, 33139_i64, 33140_i64, 1_i64, -6.0_f64, -6.0_f64),
        (-3.5_f64, 33140_i64, 33141_i64, 1_i64, -3.0_f64, -3.0_f64),
        (-0.2_f64, 33141_i64, 33138_i64, 5_i64, 0.0_f64, 0.0_f64),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().security_status = start_sec;
        });
        let bad = rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(target_sec), PyRep::Dict(vec![(PyRep::Integer(invalid_tag), PyRep::Integer(1))])],
        )
        .await
        .unwrap_err();
        assert!(bad.contains("SecurityTagBandMismatch"));

        let ok = rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(target_sec), PyRep::Dict(vec![(PyRep::Integer(valid_tag), PyRep::Integer(req_qty))])],
        )
        .await
        .expect("ApplyTagsForSecStatus");
        let PyRep::Real(new_sec) = field(&ok, "newSecurityStatus") else {
            panic!("expected Real newSecurityStatus");
        };
        assert!((new_sec - expected_sec).abs() < 1e-6);
    }

    // 4. V24 GetSecurityStatusTransactions column names
    let txs = rpc(&ctx, &mut sess, "crimewatch", "GetSecurityStatusTransactions", &[])
        .await
        .expect("GetSecurityStatusTransactions");
    let PyRep::Rowset { columns, rows, .. } = txs else {
        panic!("expected Rowset");
    };
    assert_eq!(rows.len(), 5, "1 illegal PvP aggression + 4 tag turn-ins");
    let row_map: std::collections::HashMap<&str, &PyRep> =
        columns.iter().map(|s| s.as_str()).zip(rows[1].iter()).collect();
    assert!(row_map.contains_key("eventDate"));
    assert_eq!(row_map.get("eventTypeID"), Some(&&PyRep::Integer(385)));
    assert_eq!(row_map.get("locationID"), Some(&&PyRep::Integer(lowsec_sys_id as i64)));
    assert_eq!(row_map.get("modification"), Some(&&PyRep::Real(0.5)));
    assert_eq!(row_map.get("newValue"), Some(&&PyRep::Real(-8.5)));
    assert_eq!(row_map.get("otherOwnerID"), Some(&&PyRep::None));
    assert_eq!(row_map.get("otherTypeID"), Some(&&PyRep::None));
}
}

// Folded submodule: parity_crimewatch_insurance_closure_part2
#[allow(unused_imports, dead_code)]
pub(crate) mod combat_insurance_void_and_expiry {
    use super::*;

// End-to-end closure tests for Insurance voiding & 12-week expiry (`feat:combat.insurance-void-and-expiry`).


use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::death_pipeline::execute_death_pipeline;
use eve_server::formulas::{compute_insurance_quote, compute_ship_insurance_reference_value};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

async fn insure_new_ship(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    station_id: i64,
) -> i64 {
    let sid = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        id
    });
    let _ = ctx
        .dispatch_rpc(
            sess,
            "insuranceSvc",
            None,
            "InsureShip",
            &[
                PyRep::Integer(sid),
                PyRep::Real(1.0),
                PyRep::Bool(false),
                PyRep::Bool(true),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("InsureShip");
    sid
}

#[tokio::test]
async fn test_e2e_insurance_void_and_expiry() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = 1;
            ch.balance = IskCents::from_f64_isk(500_000_000.0);
            ch.solar_system_id = sys_id;
            ch.station_id = Some(station_id);
        }
    });

    let ref_val = compute_ship_insurance_reference_value(&ctx.sde, 587);
    let (_, basic_40_payout) = compute_insurance_quote(ref_val, 0.40);
    let (_, plat_payout) = compute_insurance_quote(ref_val, 1.0);
    assert!(plat_payout > basic_40_payout);

    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = Some(station_id);

    // 1. Active Platinum contract pays out immediately on real ship destruction + sends EVE-mail
    let ship_a = insure_new_ship(&ctx, &mut sess, char_id, station_id).await;
    let bal_before_a = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    let mails_before_a = ctx.db.with_snapshot(|s| s.mails.len());
    let _ = execute_death_pipeline(
        &ctx,
        sys_id,
        char_id,
        ship_a,
        90_000_002,
        DVec3::ZERO,
        11,
    )
    .await;
    let bal_after_a = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    assert!((bal_after_a - bal_before_a - plat_payout).abs() < 1.0);
    assert!(ctx.db.with_snapshot(|s| {
        s.mails.len() > mails_before_a
            && s.mails
                .iter()
                .any(|m| m.recipient_id == char_id && m.subject.contains(&format!("Ship {ship_a}")))
    }));

    // 2. Repackaging voids insurance -> falls back to 40% Basic payout on destruction
    let ship_b = insure_new_ship(&ctx, &mut sess, char_id, station_id).await;
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "repackagingSvc",
            None,
            "RepackageItems",
            &[PyRep::List(vec![PyRep::Integer(ship_b)])],
            &HashMap::new(),
            None,
        )
        .await
        .expect("RepackageItems");
    let c_b = ctx
        .dispatch_rpc(
            &mut sess,
            "insuranceSvc",
            None,
            "GetContractForShip",
            &[PyRep::Integer(ship_b)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetContractForShip after repackage");
    assert!(matches!(c_b, PyRep::None), "Repackaging must void insurance contract");

    let bal_before_b = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    let _ = execute_death_pipeline(
        &ctx,
        sys_id,
        char_id,
        ship_b,
        90_000_002,
        DVec3::ZERO,
        12,
    )
    .await;
    let bal_after_b = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    assert!((bal_after_b - bal_before_b - basic_40_payout).abs() < 1.0);

    // 3. Moving ship into Corporation Hangar voids insurance
    let ship_c = insure_new_ship(&ctx, &mut sess, char_id, station_id).await;
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(ship_c),
                PyRep::Integer(station_id),
                PyRep::Integer(InventoryFlag::CorpSAG1 as i64),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("invbroker.Add to CorpSAG1");
    let c_c = ctx
        .dispatch_rpc(
            &mut sess,
            "insuranceSvc",
            None,
            "GetContractForShip",
            &[PyRep::Integer(ship_c)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetContractForShip after corp hangar move");
    assert!(matches!(c_c, PyRep::None), "Moving ship to corp hangar must void insurance");

    // 4. Contracting ship voids insurance
    let ship_d = insure_new_ship(&ctx, &mut sess, char_id, station_id).await;
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(1),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(station_id),
                PyRep::Integer(station_id),
                PyRep::Real(50_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
                PyRep::String("".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(ship_d)]),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CreateContract");
    let c_d = ctx
        .dispatch_rpc(
            &mut sess,
            "insuranceSvc",
            None,
            "GetContractForShip",
            &[PyRep::Integer(ship_d)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("GetContractForShip after contract");
    assert!(matches!(c_d, PyRep::None), "Contracting ship must void insurance");

    // 5. 12-week expiry (84 * 86,400 s) falls back to 40% Basic payout on destruction
    let ship_e = insure_new_ship(&ctx, &mut sess, char_id, station_id).await;
    ctx.advance_secs(12 * 7 * 86_400 + 10);
    let bal_before_e = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    let _ = execute_death_pipeline(
        &ctx,
        sys_id,
        char_id,
        ship_e,
        90_000_002,
        DVec3::ZERO,
        13,
    )
    .await;
    let bal_after_e = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    assert!((bal_after_e - bal_before_e - basic_40_payout).abs() < 1.0);

    // 6. CONCORD kill in high-sec as criminal yields 0.0 payout
    let ship_f = insure_new_ship(&ctx, &mut sess, char_id, station_id).await;
    let now_f = ctx.now_unix();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let _ = cw.handle_combat_aggression_with_pod(
            char_id,
            90_000_002,
            ship_f,
            sys_id,
            0.95,
            false,
            false,
            now_f,
        );
    }
    let bal_before_f = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    let _ = execute_death_pipeline(
        &ctx,
        sys_id,
        char_id,
        ship_f,
        1_000_125,
        DVec3::ZERO,
        14,
    )
    .await;
    let bal_after_f = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.to_wire_f64());
    assert!((bal_after_f - bal_before_f).abs() < 1e-6, "CONCORD kill as criminal must yield 0 payout");
}
}

#[allow(unused_imports)]
pub(crate) mod features_combat_part2 {
//! Dedicated end-to-end integration tests for `feat:drones.*` fighter parity rows (Part 2).

use eve_db::{InventoryFlag, IskCents};
use eve_destiny::{Ball, DamageVector, DVec3, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    apply_damage_to_fighter_squadron, dispatch_combat_rpc, with_combat_runtime,
};
use std::collections::HashMap;

const ARCHON_CARRIER: u32 = 23757;
const AMETAT_HEAVY_FIGHTER: u32 = 40362;
const EQUITE_LIGHT_FIGHTER: u32 = 40358;
const SIREN_SUPPORT_FIGHTER: u32 = 40559;
const DROMI_SUPPORT_FIGHTER: u32 = 40561;
const CENOBITE_SUPPORT_FIGHTER: u32 = 40560;
const SCARAB_SUPPORT_FIGHTER: u32 = 40562;
const SYS_ID: u32 = 30_004_759;

fn make_sess(char_id: i64, ship_id: i64) -> SessionState {
    let mut s = SessionState::new(6001);
    s.user_id = Some(9001);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1_000_009);
    s.solar_system_id = Some(SYS_ID);
    s.constellation_id = Some(20_000_020);
    s.region_id = Some(10_000_002);
    s.ship_id = Some(ship_id);
    s.role = service_roles::ROLE_PLAYER | service_roles::ROLE_LOGIN;
    s
}

async fn setup_carrier(ctx: &ServerContext, char_id: i64, ship_id: i64) {
    crate::features_bounties_ess::give_item(ctx, ship_id, ARCHON_CARRIER, char_id, SYS_ID as i64, InventoryFlag::None, 1);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.solar_system_id = SYS_ID;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(500_000_000);
        }
    });
    let sys = ctx.systems.get_or_spawn(SYS_ID);
    let mut cs = ctx.build_type_combat_state(ARCHON_CARRIER);
    cs.capacitor = 100_000.0;
    cs.max_capacitor = 100_000.0;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ARCHON_CARRIER,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 400.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;
}

async fn setup_target_cruiser(ctx: &ServerContext, char_id: i64, ship_id: i64, pos: DVec3) {
    crate::features_bounties_ess::give_item(ctx, ship_id, 626, char_id, SYS_ID as i64, InventoryFlag::None, 1);
    let sys = ctx.systems.get_or_spawn(SYS_ID);
    let mut cs = ctx.build_type_combat_state(626);
    if cs.max_shield_hp <= 0.0 {
        cs.max_shield_hp = 5_000.0;
        cs.shield_hp = 5_000.0;
        cs.max_armor_hp = 5_000.0;
        cs.armor_hp = 5_000.0;
        cs.max_structure_hp = 5_000.0;
        cs.structure_hp = 5_000.0;
    }
    cs.capacitor = 5_000.0;
    cs.max_capacitor = 5_000.0;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 626,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 400.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 150.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;
}

#[tokio::test]
async fn test_fighter_heavy_long_range_bomb() {
    let ctx = crate::modules_advanced_tactical::build_ctx();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let carrier_id = 104_000_001;
    let victim_near = 104_000_010;
    let victim_far = 104_000_011;

    setup_carrier(&ctx, char_id, carrier_id).await;
    setup_target_cruiser(&ctx, 90_000_002, victim_near, DVec3::new(26_000.0, 0.0, 0.0)).await;
    setup_target_cruiser(&ctx, 90_000_002, victim_far, DVec3::new(55_000.0, 0.0, 0.0)).await;
    let mut sess = make_sess(char_id, carrier_id);

    let item_id = 410_001;
    crate::features_bounties_ess::give_item(&ctx, item_id, AMETAT_HEAVY_FIGHTER, char_id, carrier_id, InventoryFlag::FighterBay, 3);
    dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(item_id)], &kw)
        .await
        .expect("LoadFightersToTube");
    let launched = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(0)])],
        &kw,
    )
    .await
    .expect("LaunchFightersFromTubes");
    let PyRep::List(balls) = launched else { panic!("Expected List"); };
    let fighter_ball = balls[0].as_i64().unwrap();

    let sys = ctx.systems.get_or_spawn(SYS_ID);
    let near_before = sys.get_combat_state(victim_near).await.unwrap().unwrap().shield_hp;
    let far_before = sys.get_combat_state(victim_far).await.unwrap().unwrap().shield_hp;

    let pt = PyRep::Tuple(vec![PyRep::Real(25_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]);
    let res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "fighterMgr",
        "CmdActivateAbilitySlots",
        &[PyRep::Integer(fighter_ball), PyRep::Integer(2), pt],
        &kw,
    )
    .await
    .expect("Activate Heavy Fighter Bomb");
    assert_eq!(res.dict_get("success"), Some(&PyRep::Bool(true)));

    let near_after = sys.get_combat_state(victim_near).await.unwrap().unwrap().shield_hp;
    let far_after = sys.get_combat_state(victim_far).await.unwrap().unwrap().shield_hp;
    assert!(near_after < near_before, "Ship within bomb blast radius must take AoE damage");
    assert_eq!(far_after, far_before, "Ship 30km away must be outside bomb blast radius");

    let charges = res.dict_get("chargesRemaining").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(charges < 3, "Launching Bomb must consume charge ({charges} < 3)");
}

#[tokio::test]
async fn test_fighter_refuel_rearm_repair() {
    let ctx = crate::modules_advanced_tactical::build_ctx();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let carrier_id = 104_000_001;
    setup_carrier(&ctx, char_id, carrier_id).await;
    let mut sess = make_sess(char_id, carrier_id);

    let tube_item = 410_010;
    let bay_spare = 410_011;
    crate::features_bounties_ess::give_item(&ctx, tube_item, EQUITE_LIGHT_FIGHTER, char_id, carrier_id, InventoryFlag::FighterBay, 9);
    crate::features_bounties_ess::give_item(&ctx, bay_spare, EQUITE_LIGHT_FIGHTER, char_id, carrier_id, InventoryFlag::FighterBay, 6);

    dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(tube_item)], &kw)
        .await
        .expect("LoadFightersToTube");
    let launched = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(0)])],
        &kw,
    )
    .await
    .expect("LaunchFightersFromTubes");
    let PyRep::List(balls) = launched else { panic!("Expected List"); };
    let fighter_ball = balls[0].as_i64().unwrap();

    // Destroy 2 fighter units via damage
    let (lost, remaining, wiped) = apply_damage_to_fighter_squadron(&ctx, SYS_ID, fighter_ball, DamageVector::new(2_500.0, 0.0, 0.0, 0.0)).await;
    assert!(!wiped && lost >= 1 && remaining < 9, "Damaged squadron must lose fighter units ({lost} lost, {remaining} remaining)");

    // Recall to tube -> triggers rearm/repair/replenish timer
    dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "fighterMgr",
        "RecallFightersToTubes",
        &[PyRep::List(vec![PyRep::Integer(fighter_ball)])],
        &kw,
    )
    .await
    .expect("RecallFightersToTubes");

    let is_rearming = with_combat_runtime(&ctx, |rt| {
        rt.fighter_tube_rearm_until
            .get(&(carrier_id, 0))
            .map(|&exp| exp > ctx.now_unix())
            .unwrap_or(false)
    });
    assert!(is_rearming, "Recalled squadron must enter tube rearm/repair timer");
    assert!(
        dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw)
            .await
            .is_err(),
        "Relaunching tube while rearm/repair timer is active must be rejected"
    );

    // Advance past rearm timer -> relaunch succeeds!
    ctx.advance_time_seconds(60);
    assert!(
        dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw)
            .await
            .is_ok(),
        "Relaunch after rearm/repair completion must succeed"
    );
}

#[tokio::test]
async fn test_fighter_support_abilities_and_squadron_scaling() {
    let ctx = crate::modules_advanced_tactical::build_ctx();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let carrier_id = 104_000_001;
    let target_id = 104_000_020;

    setup_carrier(&ctx, char_id, carrier_id).await;
    setup_target_cruiser(&ctx, 90_000_002, target_id, DVec3::new(5_000.0, 0.0, 0.0)).await;
    let mut sess = make_sess(char_id, carrier_id);

    // Archon has class limit 2 for Support Fighters: test Siren + Cenobite, then recall & test Dromi + Scarab!
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(target_id)], &kw, None)
        .await
        .expect("AddTarget");
    let sys_t = ctx.systems.get_or_spawn(SYS_ID);
    for _ in 0..25 {
        ctx.advance_time_seconds(1);
        let _ = sys_t.try_step_full_tick(1.0);
    }

    for &(pair, base_id) in &[
        ([SIREN_SUPPORT_FIGHTER, CENOBITE_SUPPORT_FIGHTER], 410_020i64),
        ([DROMI_SUPPORT_FIGHTER, SCARAB_SUPPORT_FIGHTER], 410_030i64),
    ] {
        for (idx, tid) in pair.into_iter().enumerate() {
            let iid = base_id + idx as i64;
            crate::features_bounties_ess::give_item(&ctx, iid, tid, char_id, carrier_id, InventoryFlag::FighterBay, 3);
            dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(idx as i64), PyRep::Integer(iid)], &kw)
                .await
                .expect("Load support fighter");
        }
        let launched = dispatch_combat_rpc(
            &ctx,
            &mut sess,
            "fighterMgr",
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)])],
            &kw,
        )
        .await
        .expect("Launch support squadrons");
        let PyRep::List(balls) = launched else { panic!("Expected List"); };
        assert_eq!(balls.len(), 2);

        let sys = ctx.systems.get_or_spawn(SYS_ID);

        for (idx, b) in balls.iter().enumerate() {
            let bid = b.as_i64().unwrap();
            let slot = if pair[idx] == CENOBITE_SUPPORT_FIGHTER { 0 } else { 2 };
            let res = dispatch_combat_rpc(
                &ctx,
                &mut sess,
                "fighterMgr",
                "CmdActivateAbilitySlots",
                &[PyRep::Integer(bid), PyRep::Integer(slot), PyRep::Integer(target_id)],
                &kw,
            )
            .await
            .expect("Activate support fighter EWAR ability");
            assert_eq!(res.dict_get("success"), Some(&PyRep::Bool(true)));
        }

        if pair[0] == SIREN_SUPPORT_FIGHTER {
            let scramble = sys.get_combat_state(target_id).await.unwrap().unwrap().warp_scramble_strength;
            assert!(scramble >= 3, "Siren support fighter squadron (3 units) must apply >= 3 warp disruption points (got {scramble})");
            let siren_ball = balls[0].as_i64().unwrap();
            let (lost, rem, _) = apply_damage_to_fighter_squadron(&ctx, SYS_ID, siren_ball, DamageVector::new(4_500.0, 0.0, 0.0, 0.0)).await;
            assert!(lost >= 1 && rem < 3, "Siren squadron must lose unit(s) under fire ({lost} lost, {rem} remaining)");
        }
        dispatch_combat_rpc(
            &ctx,
            &mut sess,
            "fighterMgr",
            "RecallFightersToTubes",
            &[PyRep::List(balls)],
            &kw,
        )
        .await
        .expect("Recall support squadrons");
        ctx.advance_time_seconds(60);
    }
}

#[tokio::test]
async fn test_light_fighter_space_superiority() {
    let ctx = crate::modules_advanced_tactical::build_ctx();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let carrier_id = 104_000_001;
    let enemy_carrier_id = 104_000_099;

    setup_carrier(&ctx, char_id, carrier_id).await;
    setup_target_cruiser(&ctx, 90_000_002, enemy_carrier_id, DVec3::new(15_000.0, 0.0, 0.0)).await;
    let mut sess = make_sess(char_id, carrier_id);

    let equite_id = 410_050;
    crate::features_bounties_ess::give_item(&ctx, equite_id, EQUITE_LIGHT_FIGHTER, char_id, carrier_id, InventoryFlag::FighterBay, 9);
    dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(equite_id)], &kw)
        .await
        .expect("Load Equite");
    let launched = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(0)])],
        &kw,
    )
    .await
    .expect("Launch Equite");
    let PyRep::List(balls) = launched else { panic!("Expected List"); };
    let equite_ball = balls[0].as_i64().unwrap();

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(enemy_carrier_id)], &kw, None)
        .await
        .expect("AddTarget");
    let sys_t = ctx.systems.get_or_spawn(SYS_ID);
    for _ in 0..25 {
        ctx.advance_time_seconds(1);
        let _ = sys_t.try_step_full_tick(1.0);
    }

    // Activate Evasive Maneuvers / MWD (slot 1) and Anti-Fighter Missile Volley (slot 2)
    let evasive = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "fighterMgr",
        "CmdActivateAbilitySlots",
        &[PyRep::Integer(equite_ball), PyRep::Integer(1), PyRep::None],
        &kw,
    )
    .await
    .expect("Evasive Maneuvers");
    assert_eq!(evasive.dict_get("success"), Some(&PyRep::Bool(true)));

    let volley = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "fighterMgr",
        "CmdActivateAbilitySlots",
        &[PyRep::Integer(equite_ball), PyRep::Integer(2), PyRep::Integer(enemy_carrier_id)],
        &kw,
    )
    .await
    .expect("Anti-Fighter Missile Volley");
    assert_eq!(volley.dict_get("success"), Some(&PyRep::Bool(true)));

    // Immediate re-activation of Anti-Fighter Missile Volley must be rejected while on cooldown!
    assert!(
        dispatch_combat_rpc(
            &ctx,
            &mut sess,
            "fighterMgr",
            "CmdActivateAbilitySlots",
            &[PyRep::Integer(equite_ball), PyRep::Integer(2), PyRep::Integer(enemy_carrier_id)],
            &kw,
        )
        .await
        .is_err(),
        "Re-activating Equite anti-fighter missile volley while on cooldown must be rejected"
    );
}
}

#[allow(unused_imports)]
pub(crate) mod fighters_lifecycle {
//! Lifecycle test suite for:
//! - Heavy fighter point-targeted AoE bomb ability
//! - Fighter tube refueling, rearming, and HP repair cycles
//! - Support fighters EWAR application and scaling
//! - Space superiority light fighters anti-fighter bonus & Evasive Maneuvers
//! - Industry pause timing and CompleteManyJobs (`feat:lifecycle.industry-pause-and-many`)
//! - Quitting corporation full cleanup and bloodline NPC assignment (`feat:lifecycle.corp-quit-cleanup`)

use crate::features_bounties_ess::give_item;
use eve_db::{
    corp_roles, ChatChannelRecord, CorporationRecord, InventoryFlag, IskCents,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::context::ActiveFighterSquadron;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_combat_runtime;
use eve_server::services::industry_pi::*;
use std::collections::{HashMap, HashSet};

const NULLSEC_SYS_ID: u32 = 30_004_759; // 1DQ1-A (-0.38 nullsec)
const NYX_SUPERCARRIER_TYPE_ID: u32 = 23913;
const ARCHON_CARRIER_TYPE_ID: u32 = 23757;
const TARGET_CRUISER_TYPE_ID: u32 = 626; // Vexor
const AMETAT_HEAVY_FIGHTER_TYPE_ID: u32 = 40362;
const SIREN_SUPPORT_FIGHTER_TYPE_ID: u32 = 40346;
const DROMI_SUPPORT_FIGHTER_TYPE_ID: u32 = 40347;
const SCARAB_SUPPORT_FIGHTER_TYPE_ID: u32 = 40345;
const EQUITE_LIGHT_FIGHTER_TYPE_ID: u32 = 40358;

fn make_session(char_id: i64, ship_id: i64, sys_id: u32, corp_id: i64) -> SessionState {
    let mut s = SessionState::new(char_id as u64);
    s.user_id = Some((char_id as u64).wrapping_add(10_000));
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.constellation_id = Some(20_000_020);
    s.region_id = Some(10_000_002);
    s.ship_id = Some(ship_id);
    s.role = service_roles::ROLE_PLAYER | service_roles::ROLE_LOGIN;
    s
}

async fn rpc(
    ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

async fn setup_ship_ball(ctx: &ServerContext, ship_id: i64, type_id: u32, char_id: i64, sys_id: u32, pos: DVec3) {
    give_item(ctx, ship_id, type_id, char_id, sys_id as i64, InventoryFlag::None, 1);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(base) = snap.characters.get(&90_000_001).cloned() {
            let mut ch = base;
            ch.character_id = char_id;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            snap.characters.insert(char_id, ch);
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_type_combat_state(type_id);
    if cs.max_shield_hp <= 0.0 {
        cs.max_shield_hp = 10_000.0; cs.shield_hp = 10_000.0;
        cs.max_armor_hp = 10_000.0; cs.armor_hp = 10_000.0;
        cs.max_structure_hp = 10_000.0; cs.structure_hp = 10_000.0;
    }
    cs.capacitor = 100_000.0; cs.max_capacitor = 100_000.0;
    let _ = sys.spawn_ball_with_combat(Ball {
        id: ship_id, type_id, position: pos, velocity: DVec3::ZERO,
        max_velocity: 200.0, inertia: 1.0, mass: 10_000_000.0, radius: 150.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }, cs).await;
}

#[tokio::test]
async fn test_parity_fighters_heavy_bomb_and_tube_rearm_repair() {
    let ctx = crate::modules_advanced_tactical::build_ctx();
    let carrier_char_id = 92_100_001i64;
    let carrier_ship_id = 92_100_002i64;
    let target_char_id = 92_100_003i64;
    let target_ship_id = 92_100_004i64;
    let sys_id = NULLSEC_SYS_ID;

    setup_ship_ball(&ctx, carrier_ship_id, NYX_SUPERCARRIER_TYPE_ID, carrier_char_id, sys_id, DVec3::ZERO).await;
    setup_ship_ball(&ctx, target_ship_id, TARGET_CRUISER_TYPE_ID, target_char_id, sys_id, DVec3::new(25_000.0, 0.0, 0.0)).await;

    let mut sess = make_session(carrier_char_id, carrier_ship_id, sys_id, 1_000_009);
    let heavy_item_id = 92_100_101i64;
    let reserve_item_id = 92_100_102i64;
    give_item(&ctx, heavy_item_id, AMETAT_HEAVY_FIGHTER_TYPE_ID, carrier_char_id, carrier_ship_id, InventoryFlag::FighterBay, 6);
    give_item(&ctx, reserve_item_id, AMETAT_HEAVY_FIGHTER_TYPE_ID, carrier_char_id, carrier_ship_id, InventoryFlag::FighterBay, 5);

    rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(heavy_item_id)])
        .await.expect("Load Ametat heavy fighter to tube 0 on Nyx Supercarrier");

    let launch_rep = rpc(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])])
        .await.expect("Launch heavy fighter squadron");
    let PyRep::List(launched) = launch_rep else { panic!("Expected List of launched fighters"); };
    let heavy_ball_id = launched[0].as_i64().expect("fighter ball id");

    let sys = ctx.systems.get_or_spawn(sys_id);
    let target_before = sys.get_combat_state(target_ship_id).await.unwrap().unwrap().shield_hp;

    let bomb_target_pt = PyRep::Tuple(vec![PyRep::Real(25_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]);
    let bomb_rep = rpc(&ctx, &mut sess, "fighterMgr", "ActivateAbilitySlotsAtPoint", &[PyRep::Integer(heavy_ball_id), PyRep::Integer(2), bomb_target_pt.clone()])
        .await.expect("ActivateAbilitySlotsAtPoint bomb detonation");
    assert_eq!(bomb_rep.dict_get("remainingCharges").and_then(|v| v.as_i64()).unwrap(), 2, "1st bomb activation leaves 2 charges");

    let target_after = sys.get_combat_state(target_ship_id).await.unwrap().unwrap().shield_hp;
    assert!(target_after < target_before, "AoE bomb must deal shield damage at target coordinate");

    let cd_err = rpc(&ctx, &mut sess, "fighterMgr", "ActivateAbilitySlotsAtPoint", &[PyRep::Integer(heavy_ball_id), PyRep::Integer(2), bomb_target_pt.clone()])
        .await.expect_err("Immediate reactivation must be blocked on cooldown");
    assert_eq!(cd_err.code, "AbilityOnCooldown");

    ctx.advance_secs(95);
    let b2 = rpc(&ctx, &mut sess, "fighterMgr", "ActivateAbilitySlotsAtPoint", &[PyRep::Integer(heavy_ball_id), PyRep::Integer(2), bomb_target_pt.clone()]).await.expect("2nd bomb");
    assert_eq!(b2.dict_get("remainingCharges").and_then(|v| v.as_i64()), Some(1));

    ctx.advance_secs(95);
    let b3 = rpc(&ctx, &mut sess, "fighterMgr", "ActivateAbilitySlotsAtPoint", &[PyRep::Integer(heavy_ball_id), PyRep::Integer(2), bomb_target_pt.clone()]).await.expect("3rd bomb");
    assert_eq!(b3.dict_get("remainingCharges").and_then(|v| v.as_i64()), Some(0));

    ctx.advance_secs(95);
    let ooc_err = rpc(&ctx, &mut sess, "fighterMgr", "ActivateAbilitySlotsAtPoint", &[PyRep::Integer(heavy_ball_id), PyRep::Integer(2), bomb_target_pt])
        .await.expect_err("4th bomb activation with 0 charges must fail");
    assert_eq!(ooc_err.code, "OutOfCharges");

    ctx.active_fighters.write().unwrap().get_mut(&heavy_ball_id).unwrap().squadron_size = 5;
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.fighter_unit_damage_accum.insert(heavy_ball_id, 450.0);
    });

    let recall_rep = rpc(&ctx, &mut sess, "fighterMgr", "RecallFightersToTubes", &[PyRep::List(vec![PyRep::Integer(heavy_ball_id)])]).await.expect("RecallFightersToTubes");
    assert_eq!(recall_rep, PyRep::List(vec![PyRep::Integer(heavy_ball_id)]));

    let relaunch_err = rpc(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])])
        .await.expect_err("Relaunch during rearm/refuel cycle must be blocked");
    assert_eq!(relaunch_err.code, "TubeRearming");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&heavy_item_id).unwrap().quantity, 6, "Destroyed fighter refilled from reserve in FighterBay");
        assert_eq!(snap.items.get(&reserve_item_id).unwrap().quantity, 4, "Reserve stack in FighterBay decremented by 1");
    });
    assert!(with_combat_runtime(&ctx, |rt| !rt.fighter_unit_damage_accum.contains_key(&heavy_ball_id)), "Unit damage accumulator cleared on recall");

    ctx.advance_secs(20);
    let relaunch_rep = rpc(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await.expect("Relaunch after rearm");
    let PyRep::List(relaunched) = relaunch_rep else { panic!("Expected list") };
    let new_ball_id = relaunched[0].as_i64().unwrap();
    assert_eq!(ctx.active_fighters.read().unwrap().get(&new_ball_id).unwrap().squadron_size, 6, "Relaunched squadron has full size");
}

#[tokio::test]
async fn test_parity_fighters_support_and_space_superiority() {
    let ctx = crate::modules_advanced_tactical::build_ctx();
    let carrier_char_id = 92_200_001i64;
    let carrier_ship_id = 92_200_002i64;
    let target_char_id = 92_200_003i64;
    let target_ship_id = 92_200_004i64;
    let sys_id = 30_004_759;

    setup_ship_ball(&ctx, carrier_ship_id, ARCHON_CARRIER_TYPE_ID, carrier_char_id, sys_id, DVec3::ZERO).await;
    setup_ship_ball(&ctx, target_ship_id, TARGET_CRUISER_TYPE_ID, target_char_id, sys_id, DVec3::new(6_000.0, 0.0, 0.0)).await;

    let mut sess = make_session(carrier_char_id, carrier_ship_id, sys_id, 1_000_009);
    let (s_iid, d_iid, sc_iid, eq_iid) = (92_200_101i64, 92_200_102i64, 92_200_103i64, 92_200_104i64);
    give_item(&ctx, s_iid, SIREN_SUPPORT_FIGHTER_TYPE_ID, carrier_char_id, carrier_ship_id, InventoryFlag::FighterBay, 3);
    give_item(&ctx, d_iid, DROMI_SUPPORT_FIGHTER_TYPE_ID, carrier_char_id, carrier_ship_id, InventoryFlag::FighterBay, 3);
    give_item(&ctx, sc_iid, SCARAB_SUPPORT_FIGHTER_TYPE_ID, carrier_char_id, carrier_ship_id, InventoryFlag::FighterBay, 3);
    give_item(&ctx, eq_iid, EQUITE_LIGHT_FIGHTER_TYPE_ID, carrier_char_id, carrier_ship_id, InventoryFlag::FighterBay, 9);

    for (tidx, iid) in [(0, s_iid), (1, d_iid), (2, eq_iid)] {
        rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(tidx), PyRep::Integer(iid)]).await.unwrap();
    }

    let launched = rpc(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![
        PyRep::Integer(0), PyRep::Integer(1), PyRep::Integer(2),
    ])]).await.expect("Launch 3 squadrons");
    let PyRep::List(balls) = launched else { panic!("Expected list") };
    let (siren_ball, dromi_ball, equite_ball) = (balls[0].as_i64().unwrap(), balls[1].as_i64().unwrap(), balls[2].as_i64().unwrap());

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.add_target_lock(carrier_ship_id, eve_destiny::TargetLock { target_id: target_ship_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0) }).await;

    // 1. Siren Warp Disruption scaled by squadron size
    rpc(&ctx, &mut sess, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::Integer(siren_ball), PyRep::Integer(0), PyRep::Integer(target_ship_id)]).await.unwrap();
    assert_eq!(sys.get_combat_state(target_ship_id).await.unwrap().unwrap().warp_scramble_strength, 3, "Siren full squad (3) applies 3 scram points");

    ctx.advance_secs(12);
    ctx.active_fighters.write().unwrap().get_mut(&siren_ball).unwrap().squadron_size = 1;
    rpc(&ctx, &mut sess, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::Integer(siren_ball), PyRep::Integer(0), PyRep::Integer(target_ship_id)]).await.unwrap();
    assert_eq!(sys.get_combat_state(target_ship_id).await.unwrap().unwrap().warp_scramble_strength, 1, "Siren scaled down to 1 point at size 1");

    // 2. Dromi Stasis Webification
    let _ = sys.set_speed_fraction(target_ship_id, 1.0).await;
    rpc(&ctx, &mut sess, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::Integer(dromi_ball), PyRep::Integer(0), PyRep::Integer(target_ship_id)]).await.unwrap();
    assert!(sys.get_ball_snapshot(target_ship_id).await.unwrap().unwrap().1.speed_fraction < 1.0, "Dromi web reduces target speed");

    // 3. Scarab ECM Jamming: recall Dromi from tube 1, unload tube 1 to FighterBay, and load Scarab into tube 1
    rpc(&ctx, &mut sess, "fighterMgr", "RecallFightersToTubes", &[PyRep::List(vec![PyRep::Integer(dromi_ball)])]).await.unwrap();
    ctx.advance_secs(20);
    rpc(&ctx, &mut sess, "fighterMgr", "UnloadTubeToFighterBay", &[PyRep::Integer(1)]).await.unwrap();
    rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(1), PyRep::Integer(sc_iid)]).await.unwrap();
    let sc_launch = rpc(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(1)])]).await.unwrap();
    let PyRep::List(sc_balls) = sc_launch else { panic!("Expected list") };
    let scarab_ball = sc_balls[0].as_i64().unwrap();

    let _ = sys.add_target_lock(target_ship_id, eve_destiny::TargetLock { target_id: carrier_ship_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0) }).await;
    assert!(sys.get_target_locks(target_ship_id).await.unwrap().iter().any(|l| l.target_id == carrier_ship_id && l.locked));

    rpc(&ctx, &mut sess, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::Integer(scarab_ball), PyRep::Integer(0), PyRep::Integer(target_ship_id)]).await.unwrap();
    let locks_after = sys.get_target_locks(target_ship_id).await.unwrap();
    assert!(locks_after.is_empty() || !locks_after.iter().any(|l| l.target_id == carrier_ship_id && l.locked), "Scarab ECM clears lock on carrier");

    // 4. Equite Space Superiority: Evasive Maneuvers and anti-fighter missile volley
    let orig_sig = sys.get_combat_state(equite_ball).await.unwrap().unwrap().signature_radius;
    let orig_vel = sys.get_ball_snapshot(equite_ball).await.unwrap().unwrap().1.max_velocity;

    rpc(&ctx, &mut sess, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::Integer(equite_ball), PyRep::Integer(1), PyRep::None]).await.unwrap();
    let evasive_cs = sys.get_combat_state(equite_ball).await.unwrap().unwrap();
    let evasive_vel = sys.get_ball_snapshot(equite_ball).await.unwrap().unwrap().1.max_velocity;
    assert!(evasive_vel > orig_vel, "Evasive Maneuvers increases velocity");
    assert!(evasive_cs.signature_radius < orig_sig, "Evasive Maneuvers reduces signature radius");

    let cd_err = rpc(&ctx, &mut sess, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::Integer(equite_ball), PyRep::Integer(1), PyRep::None]).await.unwrap_err();
    assert_eq!(cd_err.code, "AbilityOnCooldown");

    rpc(&ctx, &mut sess, "fighterMgr", "CmdDeactivateAbilitySlots", &[PyRep::Integer(equite_ball), PyRep::Integer(1)]).await.unwrap();
    assert_eq!(sys.get_combat_state(equite_ball).await.unwrap().unwrap().signature_radius, orig_sig, "Deactivation restores sig radius");

    // Anti-fighter missile volley vs enemy fighter
    let enemy_fighter_ball = 92_200_999i64;
    ctx.active_fighters.write().unwrap().insert(enemy_fighter_ball, ActiveFighterSquadron {
        ball_id: enemy_fighter_ball, parent_ship_id: target_ship_id, owner_char_id: target_char_id,
        tube_index: 0, type_id: EQUITE_LIGHT_FIGHTER_TYPE_ID, item_id: 92_200_998,
        squadron_size: 9, target_id: None, mwd_active: false, is_abandoned: false,
    });
    let mut ef_cs = ctx.build_type_combat_state(EQUITE_LIGHT_FIGHTER_TYPE_ID);
    ef_cs.shield_hp = 10_000.0; ef_cs.max_shield_hp = 10_000.0;
    ef_cs.armor_hp = 10_000.0; ef_cs.max_armor_hp = 10_000.0;
    ef_cs.structure_hp = 10_000.0; ef_cs.max_structure_hp = 10_000.0;
    let _ = sys.spawn_ball_with_combat(Ball {
        id: enemy_fighter_ball, type_id: EQUITE_LIGHT_FIGHTER_TYPE_ID, position: DVec3::new(4_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 1000.0, inertia: 1.0, mass: 10_000.0, radius: 35.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }, ef_cs).await;
    let _ = sys.add_target_lock(carrier_ship_id, eve_destiny::TargetLock { target_id: enemy_fighter_ball, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0) }).await;

    let ef_before = sys.get_combat_state(enemy_fighter_ball).await.unwrap().unwrap().shield_hp;
    rpc(&ctx, &mut sess, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::Integer(equite_ball), PyRep::Integer(2), PyRep::Integer(enemy_fighter_ball)]).await.unwrap();
    let ef_after = sys.get_combat_state(enemy_fighter_ball).await.unwrap().unwrap().shield_hp;
    assert!(ef_after < ef_before, "Anti-fighter missile volley deals damage to enemy fighter");
}

#[tokio::test]
async fn test_parity_lifecycle_industry_and_corp_quit() {
    // parity: feat:lifecycle.industry-pause-and-many
    eve_testkit::attest(&["feat:lifecycle.industry-pause-and-many"]);

    let ctx = crate::modules_advanced_tactical::build_ctx();
    let pilot_char_id = 92_300_001i64;
    let player_corp_id = 98_000_101i64;
    let struct_id = 92_300_500i64;
    let sys_id = 30_000_142u32; // Jita

    ctx.db.with_mut_snapshot(|s| {
        if let Some(base) = s.characters.get(&90_000_001).cloned() {
            let mut ch = base;
            ch.character_id = pilot_char_id;
            ch.corporation_id = player_corp_id;
            ch.bloodline_id = 2; // Civire bloodline -> default NPC corp 1000009
            ch.solar_system_id = sys_id;
            ch.structure_id = Some(struct_id);
            ch.active_ship_id = 92_300_002;
            s.characters.insert(pilot_char_id, ch);
        }
        if let Some(s_map) = s.skills.get(&90_000_001).cloned() {
            s.skills.insert(pilot_char_id, s_map);
        }
        s.skills.entry(pilot_char_id).or_default().insert(3380, eve_db::TrainedSkillRecord {
            type_id: 3380, skill_points: 250_000.0, level: 5,
        });
        s.skills.entry(pilot_char_id).or_default().insert(3387, eve_db::TrainedSkillRecord {
            type_id: 3387, skill_points: 250_000.0, level: 5,
        });

        let mut members = HashSet::new();
        members.insert(pilot_char_id); members.insert(90_000_002i64);
        let mut member_roles = HashMap::new();
        member_roles.insert(pilot_char_id, corp_roles::ROLE_FACTORY_MANAGER);
        s.corporations.insert(player_corp_id, CorporationRecord {
            corporation_id: player_corp_id, name: "Frontier Industrial LLC".into(), ticker: "FI-LLC".into(),
            ceo_id: 90_000_002, alliance_id: None, tax_rate: 0.05, master_wallet: IskCents(100_000_000_00),
            members, member_roles, applications: HashMap::new(), wallet_divisions: Vec::new(),
            description: String::new(), url: String::new(), hq_station_id: None,
        });

        let gov = s.corp_governance.entry(player_corp_id).or_default();
        gov.member_titles.insert(pilot_char_id, 1);
        gov.member_account_keys.insert(pilot_char_id, 1000);
        gov.pending_role_grants.insert(pilot_char_id, (corp_roles::ROLE_DIRECTOR, 9999999999));
        gov.shares.insert(pilot_char_id, 250);
        gov.offices.insert(sys_id as i64, 92_300_888);

        s.chat_channels.insert(
            format!("corpid:{player_corp_id}"),
            ChatChannelRecord {
                channel_id: format!("corpid:{player_corp_id}"),
                name: "Corp Chat".into(),
                motd: String::new(),
                owner_id: None,
                members: [pilot_char_id].into_iter().collect(),
                is_system_local: false,
                solar_system_id: None,
                is_private: false,
                operators: HashSet::new(),
                password: None,
                is_delayed_local: false,
            },
        );

        s.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35825, // Raitaru
                owner_corp_id: player_corp_id,
                solar_system_id: sys_id,
                name: "Test Raitaru".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 5_000,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    let s_mod_id = struct_id + 1;
    give_item(&ctx, s_mod_id, 35878, player_corp_id, struct_id, InventoryFlag::ServiceSlot0, 1);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(it) = s.items.get_mut(&s_mod_id) {
            it.dynamic_attributes.insert(2, 1.0);
        }
        s.module_online_states.insert(s_mod_id, true);
    });

    for bp_id in [92_300_101i64, 92_300_102i64, 92_300_103i64] {
        give_item(&ctx, bp_id, 681, pilot_char_id, struct_id, InventoryFlag::Hangar, 1);
    }
    for (idx, m_tid) in (34_u32..=40_u32).enumerate() {
        give_item(&ctx, 92_300_200 + idx as i64, m_tid, pilot_char_id, struct_id, InventoryFlag::Hangar, 50_000);
    }

    let mut sess = make_session(pilot_char_id, 92_300_002, sys_id, player_corp_id);
    sess.structure_id = Some(struct_id);

    // 1. Install Job 1 (1 run, 50s) and Job 2 (5 runs, 250s) via InstallJob
    let j1_res = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(92_300_101)), ("facilityID", PyRep::Integer(struct_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.expect("InstallJob 1");
    let job1_id = j1_res.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    let j2_res = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(92_300_102)), ("facilityID", PyRep::Integer(struct_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(5)),
    ])]).await.expect("InstallJob 2");
    let job2_id = j2_res.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    ctx.advance_secs(500);
    let offline_moment = ctx.now_unix();

    // Facility loses service / goes offline
    ctx.db.with_mut_snapshot(|s| {
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(s);
        ext.lifecycle.entry(struct_id).or_default().low_power_since_unix = Some(offline_moment);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(s, &ext);
        s.module_online_states.insert(struct_id + 1, false);
    });

    ctx.advance_secs(120);
    ctx.db.with_mut_snapshot(|snap| sync_industry_job_pause_states(snap, ctx.now_unix()));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.industry_jobs.get(&job1_id).unwrap().status, "Active", "Job finished before offline is not paused");
        assert_eq!(snap.industry_jobs.get(&job2_id).unwrap().status, "Paused", "Active job is paused on offline");
        let meta = load_job_cost_meta(snap, job2_id).unwrap();
        assert_eq!(meta.paused_at_unix, Some(offline_moment), "Pause starts at offline moment, not first read");
    });

    let pause_err = rpc(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(job2_id)]).await.expect_err("Delivering paused job fails");
    assert_eq!(pause_err.code, "FacilityServiceOffline");

    // Online facility service again
    ctx.db.with_mut_snapshot(|s| {
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(s);
        ext.lifecycle.entry(struct_id).or_default().low_power_since_unix = None;
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(s, &ext);
        s.module_online_states.insert(struct_id + 1, true);
        sync_industry_job_pause_states(s, ctx.now_unix());
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.industry_jobs.get(&job2_id).unwrap().status, "Active", "Job resumes Active without progress loss");
    });

    ctx.advance_secs(2000);
    let complete_many = rpc(&ctx, &mut sess, "industryManager", "CompleteManyJobs", &[PyRep::List(vec![
        PyRep::Integer(job1_id), PyRep::Integer(job2_id),
    ])]).await.expect("CompleteManyJobs delivers ready jobs");
    let PyRep::List(delivered_kvs) = complete_many else { panic!("Expected list") };
    assert_eq!(delivered_kvs.len(), 2, "CompleteManyJobs delivers both ready jobs");

    // Ex-members cannot cancel or deliver corp jobs: install corp job
    let corp_job_res = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(92_300_103)), ("facilityID", PyRep::Integer(struct_id)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)), ("useCorp", PyRep::Bool(true)),
    ])]).await.expect("Install corp job");
    let corp_job_id = corp_job_res.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    // 2. Corp quit cleanup (`feat:lifecycle.corp-quit-cleanup`)
    let quit_rep = rpc(&ctx, &mut sess, "corpRegistry", "KickOutMember", &[PyRep::Integer(pilot_char_id)]).await.expect("KickOutMember");
    assert_eq!(quit_rep.dict_get("removed"), Some(&PyRep::Bool(true)));

    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&pilot_char_id).unwrap();
        let expected_npc_corp = snap.characters.get(&pilot_char_id).and_then(|c| ctx.sde.bloodline(c.bloodline_id)).map(|b| b.corporation_id).unwrap();
        assert_eq!(expected_npc_corp, 1_000_009, "Civire bloodline resolves to NPC corp 1000009");
        assert_eq!(ch.corporation_id, expected_npc_corp, "Quit pilot moved to bloodline default NPC corp");

        let gov = snap.corp_governance.get(&player_corp_id).unwrap();
        assert!(!gov.member_titles.contains_key(&pilot_char_id), "Titles cleaned up on quit");
        assert!(!gov.member_account_keys.contains_key(&pilot_char_id), "Account keys cleaned up on quit");
        assert!(!gov.pending_role_grants.contains_key(&pilot_char_id), "Pending role grants cleaned up on quit");
        assert!(!gov.shares.contains_key(&pilot_char_id), "Shares returned to company shares on quit");

        let old_chan = snap.chat_channels.get(&format!("corpid:{player_corp_id}"));
        if let Some(c) = old_chan {
            assert!(!c.members.contains(&pilot_char_id), "Removed from player corp chat");
        }
        let npc_chan = snap.chat_channels.get(&format!("corpid:{expected_npc_corp}")).unwrap();
        assert!(npc_chan.members.contains(&pilot_char_id), "Added to NPC corp chat channel");
    });

    let cancel_err = rpc(&ctx, &mut sess, "industryManager", "CancelJob", &[PyRep::Integer(corp_job_id)]).await.expect_err("Ex-member cannot cancel corp job");
    assert_eq!(cancel_err.code, "JobOwnershipViolation");

    let deliver_err = rpc(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(corp_job_id)]).await.expect_err("Ex-member cannot deliver corp job");
    assert_eq!(deliver_err.code, "JobOwnershipViolation");
}
}
