//! Live integration tests for NPC Mining Op & Defense Fleet, Pochven Hypnosian Sleepers,
//! Zarzakh Gate Guns & Patrols, and Ship Insurance Lifecycle
//! (`feat:npc.mining-op-npc`, `feat:npc.pochven-hypnosian-sleepers`,
//! `feat:npc.zarzakh-gate-guns-patrols`, `feat:combat.insurance-void-and-expiry`).

#![forbid(unsafe_code)]

use crate::common;
use crate::npc_spawns_pve::{bind_beyonce, rpc, setup_docked_pilot, setup_space_pilot};
use eve_destiny::{DVec3, DamageVector};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    process_ship_insurance_payout, spawn_specialized_npc_entity, step_extended_npc_ai,
};

const FILETIME_TICKS_PER_SEC: i64 = 10_000_000;

#[tokio::test]
async fn test_npc_mining_op_and_defense_fleet_live() {
    // parity: feat:npc.mining-op-npc
    eve_testkit::attest(&["feat:npc.mining-op-npc"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let sys_id = 30_000_001u32; // Tanoo (high-sec with asteroid belt and stations)
    let belt_pos = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 9)
        .expect("Tanoo belt")
        .position;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let initial_asts = sys_handle.populate_asteroid_belt_entities().await.expect("populate belts");
    let initial_units: u64 = initial_asts.iter().map(|a| a.remaining_units).sum();

    // 1. Ensure NPC mining op is active in system
    let convoy_id = ctx.living_world.ensure_system_mining_op(sys_id, &ctx.sde);
    assert!(convoy_id > 0);

    // Step mining ops: miner Venture extracts ore from belt asteroids
    for _ in 0..4 {
        ctx.advance_secs(20);
        let _ = ctx.living_world.step_all_mining_ops(&ctx.systems, &ctx.db, &ctx.sde).await;
    }

    let post_asts = sys_handle.populate_asteroid_belt_entities().await.expect("post-mining belts");
    let post_units: u64 = post_asts.iter().map(|a| a.remaining_units).sum();
    assert!(
        post_units < initial_units,
        "NPC mining op must extract ore units from belt asteroids ({post_units} < {initial_units})"
    );

    // Miner Venture ball and companion Industrial hauler ball are present on belt grid
    let miner_snap = sys_handle.get_ball_snapshot(convoy_id).await.unwrap();
    assert!(miner_snap.is_some(), "NPC miner Venture must be spawned on belt grid");
    let hauler_snap = sys_handle.get_ball_snapshot(convoy_id + 1_000_000).await.unwrap();
    assert!(hauler_snap.is_some(), "Companion NPC hauler must be spawned on belt grid");

    // 2. Spawn defense escort fleet on grid protecting the mining op
    let defense_ship_tid = 52_938u32; // Caldari Navy Drake
    let def_pos = belt_pos + DVec3::new(2_000.0, 0.0, 0.0);
    let def_ball_id = spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        defense_ship_tid,
        "Navy",
        1001,
        def_pos,
    )
    .await;
    assert!(def_ball_id > 0, "Mining defense escort must spawn");

    // 3. Player arrives in combat ship to attack the mining op (T1 Frigate so miner Venture survives shot 1 before being destroyed)
    let (mut sess, guns) = setup_space_pilot(
        &ctx,
        8101,
        "demo",
        "demo-secret",
        90_000_001,
        sys_id,
        910_501,
        587,
        3074,
        215,
        925_000,
        belt_pos + DVec3::new(10_000.0, 0.0, 0.0),
    )
    .await;
    let _beyonce = bind_beyonce(&ctx, &mut sess, sys_id).await;

    // Player attacks the mining Venture via dogmaIM RPC
    rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(convoy_id)])
        .await
        .expect("AddTarget miner");
    for _ in 0..45 {
        if sys_handle.get_target_locks(910_501).await.unwrap().iter().any(|l| l.target_id == convoy_id && l.locked) {
            break;
        }
        ctx.advance_secs(1);
        let _ = sys_handle.step_manual(1.0).await;
    }
    rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(guns[0]), PyRep::Integer(convoy_id)])
        .await
        .expect("Activate guns on miner");

    let miner_cs_after = sys_handle.get_combat_state(convoy_id).await.unwrap().unwrap();
    assert!(
        miner_cs_after.shield_hp < miner_cs_after.max_shield_hp,
        "Miner Venture must take weapon damage from player attack"
    );

    // 4. Retaliation: defense escort detects hostile aggression against the mining op
    let player_cs_before = sys_handle.get_combat_state(910_501).await.unwrap().unwrap();
    step_extended_npc_ai(&ctx, sys_id).await;
    let player_cs_after = sys_handle.get_combat_state(910_501).await.unwrap().unwrap();
    assert!(
        player_cs_after.shield_hp < player_cs_before.shield_hp,
        "Mining defense escort must retaliate and damage player attacker"
    );

    // 5. Player destroys the defense escort and miner via live combat
    let kill_def = sys_handle
        .apply_damage_with_attacker(
            def_ball_id,
            DamageVector {
                em: 50_000.0,
                thermal: 50_000.0,
                kinetic: 50_000.0,
                explosive: 50_000.0,
            },
            Some(90_000_001),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(kill_def.1, "Defense escort must be destroyed");

    let kill_miner = sys_handle
        .apply_damage_with_attacker(
            convoy_id,
            DamageVector {
                em: 20_000.0,
                thermal: 20_000.0,
                kinetic: 20_000.0,
                explosive: 20_000.0,
            },
            Some(90_000_001),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(kill_miner.1, "NPC miner Venture must be destroyed");
    assert!(
        sys_handle.get_ball_snapshot(convoy_id).await.unwrap().is_none(),
        "Destroyed miner ball must be removed from space"
    );
}

#[tokio::test]
async fn test_pochven_hypnosian_sleepers_live() {
    // parity: feat:npc.pochven-hypnosian-sleepers
    eve_testkit::attest(&["feat:npc.pochven-hypnosian-sleepers"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let pochven_sys = 30_000_157u32; // Otela (Pochven region 10000070)
    let sleeper_tid = 56680u32; // Aroused Hypnosian Sleeper
    let sleeper_pos = DVec3::new(10_000.0, 0.0, 0.0);

    // 1. Spawn Pochven Hypnosian Sleeper
    let sleeper_ball_id = spawn_specialized_npc_entity(
        &ctx,
        pochven_sys,
        sleeper_tid,
        "PochvenHypnosianSleeper",
        2001,
        sleeper_pos,
    )
    .await;
    assert!(sleeper_ball_id > 0, "Pochven Sleeper must spawn");

    let sys_handle = ctx.systems.get_or_spawn(pochven_sys);
    let sleeper_cs = sys_handle.get_combat_state(sleeper_ball_id).await.unwrap().unwrap();
    assert!(
        sleeper_cs.max_shield_hp > 0.0 && sleeper_cs.max_armor_hp > 0.0,
        "Aroused Hypnosian Sleeper must have SDE shield and armor HP"
    );

    // 2. Setup player pilot in Pochven space near sleeper
    let (mut sess, _guns) = setup_space_pilot(
        &ctx,
        8102,
        "demo",
        "demo-secret",
        90_000_001,
        pochven_sys,
        910_502,
        54733,
        54754,
        54783,
        925_100,
        sleeper_pos + DVec3::new(15_000.0, 0.0, 0.0),
    )
    .await;
    let _beyonce = bind_beyonce(&ctx, &mut sess, pochven_sys).await;

    // 3. Step Sleeper AI: Sleeper auto-agroes neutral player (< 5.0 standing) and deals damage
    let player_cs_before = sys_handle.get_combat_state(910_502).await.unwrap().unwrap();
    step_extended_npc_ai(&ctx, pochven_sys).await;
    let player_cs_after = sys_handle.get_combat_state(910_502).await.unwrap().unwrap();
    assert!(
        player_cs_after.shield_hp < player_cs_before.shield_hp,
        "Pochven Hypnosian Sleeper must auto-agro and deal damage to player"
    );

    // 4. Destroy Sleeper via live combat damage
    let kill_evt = sys_handle
        .apply_damage_with_attacker(
            sleeper_ball_id,
            DamageVector {
                em: 200_000.0,
                thermal: 200_000.0,
                kinetic: 200_000.0,
                explosive: 200_000.0,
            },
            Some(90_000_001),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(kill_evt.1, "Pochven Hypnosian Sleeper must be destroyed in combat");
    assert!(
        sys_handle.get_ball_snapshot(sleeper_ball_id).await.unwrap().is_none(),
        "Destroyed Sleeper ball must be removed"
    );
}

#[tokio::test]
async fn test_zarzakh_gate_guns_and_patrols_live() {
    // parity: feat:npc.zarzakh-gate-guns-patrols
    eve_testkit::attest(&["feat:npc.zarzakh-gate-guns-patrols"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let zarzakh_sys = 30_100_000u32; // Zarzakh
    let gun_pos = ctx
        .sde
        .stargates()
        .values()
        .find(|g| g.from_solar_system_id == zarzakh_sys)
        .map(|g| g.position)
        .or_else(|| ctx.sde.stations.get(&60015187).map(|s| s.position))
        .unwrap_or(DVec3::ZERO);

    // 1. Spawn Zarzakh missile gate gun sentry (type 73815)
    let gate_gun_id = spawn_specialized_npc_entity(
        &ctx,
        zarzakh_sys,
        73815,
        "ZarzakhGateGun",
        4002,
        gun_pos,
    )
    .await;
    assert!(gate_gun_id > 0, "Zarzakh Gate Gun must spawn");

    // Spawn Deathless Vanguard patrol (type 73818)
    let patrol_id = spawn_specialized_npc_entity(
        &ctx,
        zarzakh_sys,
        73818,
        "ZarzakhPatrol",
        4002,
        gun_pos + DVec3::new(20_000.0, 0.0, 0.0),
    )
    .await;
    assert!(patrol_id > 0, "Deathless pirate patrol fleet must spawn in Zarzakh");

    let sys_handle = ctx.systems.get_or_spawn(zarzakh_sys);

    // 2. Setup hostile pilot in Zarzakh near the gate gun (suspect flag triggers ZarzakhGateGun ECM retaliation)
    let (mut sess_a, _guns_a) = setup_space_pilot(
        &ctx,
        8103,
        "demo",
        "demo-secret",
        90_000_001,
        zarzakh_sys,
        910_503,
        587,
        3074,
        215,
        925_200,
        gun_pos + DVec3::new(2_000.0, 0.0, 0.0),
    )
    .await;
    let _beyonce_a = bind_beyonce(&ctx, &mut sess_a, zarzakh_sys).await;
    ctx.crimewatch
        .write()
        .unwrap()
        .states
        .entry(90_000_001)
        .or_default()
        .suspect_flag_until_unix = ctx.now_unix() + 900;

    // Pilot initiates target lock on the Gate Gun
    rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(gate_gun_id)])
        .await
        .expect("AddTarget gate gun");
    let initial_locks = sys_handle.get_target_locks(910_503).await.unwrap();
    assert!(!initial_locks.is_empty(), "Pilot must initiate target lock on gate gun");

    // 3. Step AI: Zarzakh Gate Gun detects hostile pilot in Zarzakh, retaliating with missile damage and ECM jam
    let pilot_a_cs_before = sys_handle.get_combat_state(910_503).await.unwrap().unwrap();
    step_extended_npc_ai(&ctx, zarzakh_sys).await;
    let pilot_a_cs_after = sys_handle.get_combat_state(910_503).await.unwrap().unwrap();

    assert!(
        pilot_a_cs_after.shield_hp < pilot_a_cs_before.shield_hp || pilot_a_cs_after.armor_hp < pilot_a_cs_before.armor_hp,
        "Zarzakh gate guns must attack hostile pilot in Zarzakh"
    );
    assert!(
        pilot_a_cs_after.ecm_jammed_remaining_s > 0.0,
        "Zarzakh gate gun must apply ECM jam to hostile pilot"
    );

    // Verify ECM jam cleared Pilot's target locks
    let locks_after = sys_handle.get_target_locks(910_503).await.unwrap();
    assert!(
        locks_after.is_empty(),
        "Zarzakh gate gun ECM must clear player target locks"
    );

    // 4. Player destroys the gate gun and Deathless Vanguard patrol in live combat
    let kill_gun = sys_handle
        .apply_damage_with_attacker(
            gate_gun_id,
            DamageVector {
                em: 50_000.0,
                thermal: 50_000.0,
                kinetic: 50_000.0,
                explosive: 50_000.0,
            },
            Some(90_000_001),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(kill_gun.1, "Zarzakh Gate Gun must be destructible in live combat");

    let kill_patrol = sys_handle
        .apply_damage_with_attacker(
            patrol_id,
            DamageVector {
                em: 50_000.0,
                thermal: 50_000.0,
                kinetic: 50_000.0,
                explosive: 50_000.0,
            },
            Some(90_000_001),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(kill_patrol.1, "Deathless Vanguard patrol must be destructible in live combat");
}

#[tokio::test]
async fn test_combat_insurance_void_and_expiry_live() {
    // parity: feat:combat.insurance-void-and-expiry
    eve_testkit::attest(&["feat:combat.insurance-void-and-expiry"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let jita_sys = 30_000_142u32;
    let jita_stn = 60_003_760i64; // Jita 4-4 Navy Assembly Plant
    let ship_id = 103_000_801i64;
    let ship_tid = 587u32; // Rifter

    let (mut sess, _guns) = setup_docked_pilot(
        &ctx,
        8105,
        "demo",
        "demo-secret",
        90_000_001,
        jita_sys,
        jita_stn,
        ship_id,
        ship_tid,
        54754,
        54783,
        925_400,
    )
    .await;

    // 1. Query Insurance Quote via GetInsurancePrice
    let quote_rep = rpc(&ctx, &mut sess, "insuranceSvc", "GetInsurancePrice", &[PyRep::Integer(ship_id)])
        .await
        .expect("GetInsurancePrice quote");
    let PyRep::Real(ref_value) = quote_rep else {
        panic!("Expected Real reference price, got {quote_rep:?}");
    };
    assert!(ref_value > 0.0, "Ship must have positive insurance reference value");

    // Also verify GetInsurancePrices list shape
    let list_quote = rpc(&ctx, &mut sess, "insuranceSvc", "GetInsurancePrices", &[PyRep::List(vec![PyRep::Integer(ship_tid as i64)])])
        .await
        .expect("GetInsurancePrices");
    let PyRep::List(quotes) = list_quote else {
        panic!("GetInsurancePrices must return List of quotes, got {list_quote:?}");
    };
    assert!(!quotes.is_empty(), "GetInsurancePrices must return at least one quote");

    // 2. Buy Platinum Insurance (fraction = 1.0)
    let insure_res = rpc(&ctx, &mut sess, "insuranceSvc", "InsureShip", &[PyRep::Integer(ship_id), PyRep::Real(1.0)])
        .await
        .expect("InsureShip");
    assert_eq!(insure_res.dict_get("fraction").and_then(|v| v.as_f64()), Some(1.0));

    // 3. Verify active contract via GetContractForShip: 12-week (84-day) term
    let contract = rpc(&ctx, &mut sess, "insuranceSvc", "GetContractForShip", &[PyRep::Integer(ship_id)])
        .await
        .expect("GetContractForShip");
    assert!(!matches!(contract, PyRep::None), "Contract must be active");
    assert_eq!(contract.dict_get("fraction").and_then(|v| v.as_f64()), Some(1.0));
    let start_ft = contract.dict_get("startDate").and_then(|v| v.as_i64()).unwrap();
    let end_ft = contract.dict_get("endDate").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(
        end_ft - start_ft,
        84 * 86_400 * FILETIME_TICKS_PER_SEC,
        "Insurance contract duration must be exactly 12 weeks (84 days)"
    );

    // 4. Voiding via UnInsureShip
    let uninsure = rpc(&ctx, &mut sess, "insuranceSvc", "UnInsureShip", &[PyRep::Integer(ship_id)])
        .await
        .expect("UnInsureShip");
    assert_eq!(uninsure.dict_get("voided").and_then(|v| v.as_bool()), Some(true));

    let contract_after_void = rpc(&ctx, &mut sess, "insuranceSvc", "GetContractForShip", &[PyRep::Integer(ship_id)])
        .await
        .expect("GetContractForShip after void");
    assert!(matches!(contract_after_void, PyRep::None), "Voided contract must return None");

    // 5. Re-insure and test 12-week Expiry
    rpc(&ctx, &mut sess, "insuranceSvc", "InsureShip", &[PyRep::Integer(ship_id), PyRep::Real(1.0)])
        .await
        .expect("Re-InsureShip");
    let full_contract = rpc(&ctx, &mut sess, "insuranceSvc", "GetContractForShip", &[PyRep::Integer(ship_id)])
        .await
        .unwrap();
    let full_payout = full_contract.dict_get("payoutISK").and_then(|v| v.as_f64()).unwrap();

    // Advance clock past 12 weeks (84 days + 10s)
    ctx.advance_secs(84 * 86_400 + 10);

    let contract_expired = rpc(&ctx, &mut sess, "insuranceSvc", "GetContractForShip", &[PyRep::Integer(ship_id)])
        .await
        .unwrap();
    assert!(matches!(contract_expired, PyRep::None), "Expired contract must return None");

    // Process ship loss payout on expired contract: falls back to 40% default payout
    let expired_payout = process_ship_insurance_payout(&ctx, 90_000_001, ship_id, ship_tid, false);
    assert!(
        expired_payout > 0.0 && expired_payout < full_payout,
        "Expired ship loss payout must fall back to basic 40% ({expired_payout} < {full_payout})"
    );

    // EVE-mail delivered to pilot
    let has_mail = ctx.db.with_snapshot(|s| {
        s.mails.iter().any(|m| m.recipient_id == 90_000_001 && m.subject.contains("Insurance Payout"))
    });
    assert!(has_mail, "Victim must receive insurance payout EVE-mail");
}
