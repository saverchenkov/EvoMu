//! Parity closure integration tests for Navigation & PvE features:
//! - Ansiblex jump bridges & linking: `feat:navigation.ansiblex-jump`, `feat:navigation.ansiblex-link`
//! - Filaments (Abyssal, Void space, Needlejack): `feat:navigation.filament-abyssal`, `feat:navigation.filament-void`, `feat:navigation.filament-needlejack`
//! - AIR NPE & Career Program: `feat:pve.air-npe`, `feat:pve.air-career-program`
//! - Crates & Milestones: `feat:pve.crates`, `feat:pve.milestones`
//! - Daily & Seasonal Login campaigns: `feat:pve.login-campaign`, `feat:pve.seasonal-login`
//! - Agency Content Provider: `feat:pve.agency-content`
//! - COSMOS constellations, storyline agents & unique sites: `feat:pve.cosmos`


use crate::common;
use eve_db::{
    InventoryFlag, InventoryItemRecord, MiningLedgerRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

#[tokio::test]
async fn test_ansiblex_jump_bridge_access_liquid_ozone_and_destination() {
    // parity: feat:navigation.ansiblex-jump, feat:navigation.ansiblex-link
    eve_testkit::attest(&[
        "feat:navigation.ansiblex-jump",
        "feat:navigation.ansiblex-link",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_a = 30_000_142; // Jita
    let sys_b = 30_000_144; // Perimeter
    let mut sess = setup_pilot(&ctx, 8001, "demo", "demo-secret", char_id).await;

    let gate_a_id = 1_100_000_001_i64;
    let gate_b_id = 1_100_000_002_i64;
    let gate_a_pos = [10_500.0, 0.0, 0.0];
    let gate_b_pos = [20_000.0, 0.0, 0.0];

    // Seed pilot ship in space near Gate A in System A
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_a;
            ch.station_id = None;
            ch.structure_id = None;
            ch.corporation_id = 98_000_001;
        }
        snap.structures.insert(
            gate_a_id,
            UpwellStructureRecord {
                structure_id: gate_a_id,
                type_id: 35841, // Ansiblex Jump Gate
                owner_corp_id: 98_000_001,
                solar_system_id: sys_a,
                name: "Ansiblex Jita".into(),
                position: gate_a_pos,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        snap.structures.insert(
            gate_b_id,
            UpwellStructureRecord {
                structure_id: gate_b_id,
                type_id: 35841, // Ansiblex Jump Gate
                owner_corp_id: 98_000_001,
                solar_system_id: sys_b,
                name: "Ansiblex Perimeter".into(),
                position: gate_b_pos,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        // Add 1,000 units of Liquid Ozone fuel to Gate A
        snap.items.insert(
            1_200_000_001,
            InventoryItemRecord {
                item_id: 1_200_000_001,
                type_id: 16273, // Liquid Ozone
                owner_id: 98_000_001,
                location_id: gate_a_id,
                flag: InventoryFlag::Cargo,
                quantity: 1_000,
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

    sess.solar_system_id = Some(sys_a);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.corporation_id = Some(98_000_001);

    let handle_a = ctx.systems.get_or_spawn(sys_a);
    let _ = handle_a
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 601, // Ibis
                position: DVec3::new(10_000.0, 0.0, 0.0), // 500m from gate A
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 601),
        )
        .await;

    // 1. structureDirectory.GetJumpBridgesWithMyAccess
    let jb_list = rpc(&ctx, &mut sess, "structureDirectory", "GetJumpBridgesWithMyAccess", &[])
        .await
        .expect("GetJumpBridgesWithMyAccess");
    let PyRep::List(entries) = jb_list else {
        panic!("Expected list from GetJumpBridgesWithMyAccess");
    };
    assert!(!entries.is_empty(), "Must return accessible jump bridges");
    let found_gate_a = entries.iter().any(|e| {
        e.dict_get("structureID").and_then(|v| v.as_i64()) == Some(gate_a_id)
            && e.dict_get("destinationSolarSystemID").and_then(|v| v.as_i64()) == Some(sys_b as i64)
    });
    assert!(found_gate_a, "Gate A must be present with destination System B");

    // 2. structureJumpBridgeMgr.GetLinkedStructure
    let linked = rpc(&ctx, &mut sess, "structureJumpBridgeMgr", "GetLinkedStructure", &[PyRep::Integer(gate_a_id)])
        .await
        .expect("GetLinkedStructure");
    assert_eq!(linked.as_i64(), Some(gate_b_id));

    // 3. structureJumpBridgeMgr.GetJbStructureDestination
    let dest = rpc(&ctx, &mut sess, "structureJumpBridgeMgr", "GetJbStructureDestination", &[PyRep::Integer(gate_a_id)])
        .await
        .expect("GetJbStructureDestination");
    assert_eq!(dest.as_i64(), Some(sys_b as i64));

    // 4. ACL Check: change gate owner to an unallied player corporation
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&gate_a_id) {
            st.owner_corp_id = 98_000_002;
        }
    });
    let acl_err = rpc(
        &ctx,
        &mut sess,
        "structureJumpBridgeMgr",
        "CmdJumpThroughStructureStargate",
        &[PyRep::Integer(gate_a_id)],
    )
    .await
    .expect_err("Access list must deny non-member pilot");
    assert_eq!(acl_err.code, "AccessDenied");

    // Restore ACL access
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&gate_a_id) {
            st.owner_corp_id = 98_000_001;
        }
    });

    // 5. Liquid Ozone fuel check: empty the ozone fuel
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(item) = snap.items.get_mut(&1_200_000_001) {
            item.quantity = 100; // less than 500 required
        }
    });
    let fuel_err = rpc(
        &ctx,
        &mut sess,
        "structureJumpBridgeMgr",
        "CmdJumpThroughStructureStargate",
        &[PyRep::Integer(gate_a_id)],
    )
    .await
    .expect_err("Must fail when Liquid Ozone is insufficient");
    assert_eq!(fuel_err.code, "InsufficientLiquidOzone");

    // Refuel structure to 1,000 units
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(item) = snap.items.get_mut(&1_200_000_001) {
            item.quantity = 1_000;
        }
    });

    // 6. CmdJumpThroughStructureStargate execution
    let jump_res = rpc(
        &ctx,
        &mut sess,
        "structureJumpBridgeMgr",
        "CmdJumpThroughStructureStargate",
        &[PyRep::Integer(gate_a_id)],
    )
    .await
    .expect("Successful Ansiblex jump");
    assert_eq!(
        jump_res.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(sys_b as i64)
    );
    assert!(jump_res.dict_get("beyonceBoundID").is_some());

    // Verify 500 units of Liquid Ozone were consumed
    let remaining_ozone = ctx.db.with_snapshot(|snap| {
        snap.items.get(&1_200_000_001).map(|i| i.quantity).unwrap_or(0)
    });
    assert_eq!(remaining_ozone, 500, "500 units of Liquid Ozone must be consumed");

    // Verify pilot session and ball in System B
    assert_eq!(sess.solar_system_id, Some(sys_b));
    let handle_b = ctx.systems.get_or_spawn(sys_b);
    let ball_b = handle_b.get_ball_snapshot(ship_id).await.unwrap();
    assert!(ball_b.is_some(), "Ship ball must be spawned in destination system");
}

#[tokio::test]
async fn test_filaments_abyssal_pocket_void_space_and_needlejack() {
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let start_sys = 30_000_142; // Jita
    let mut sess = setup_pilot(&ctx, 8002, "demo", "demo-secret", char_id).await;

    // Seed pilot in space in Jita with filaments in cargo
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = start_sys;
            ch.station_id = None;
            ch.structure_id = None;
        }
        // Abyssal filament: type 47763 (Calm Firestorm Filament)
        snap.items.insert(
            1_300_000_001,
            InventoryItemRecord {
                item_id: 1_300_000_001,
                type_id: 47763,
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
        // Needlejack filament: type 53977 (Noise-5 'Needlejack' Filament)
        snap.items.insert(
            1_300_000_002,
            InventoryItemRecord {
                item_id: 1_300_000_002,
                type_id: 53977,
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

    sess.solar_system_id = Some(start_sys);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    let handle_start = ctx.systems.get_or_spawn(start_sys);
    let _ = handle_start
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 601,
                position: DVec3::new(50_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 601),
        )
        .await;

    // 1. Abyssal filament deployment returns UnimplementedRPC
    let abyssal_err = rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[])
        .await
        .expect_err("AbyssalEntranceDeployment returns UnimplementedRPC");
    assert_eq!(abyssal_err.code, "UnimplementedRPC");
    assert_eq!(sess.solar_system_id, Some(start_sys));

    // 2. Abyssal entrance gate activation returns UnimplementedRPC (stripped down)
    let gate_err = rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[])
        .await
        .expect_err("AbyssalEntranceGateActivation returns UnimplementedRPC");
    assert_eq!(gate_err.code, "UnimplementedRPC");

    // 3. Void Space jump returns UnimplementedRPC (stripped down)
    let void_err = rpc(&ctx, &mut sess, "voidSpaceMgr", "VoidSpacePlayerJump", &[])
        .await
        .expect_err("VoidSpacePlayerJump returns UnimplementedRPC");
    assert_eq!(void_err.code, "UnimplementedRPC");

    // 4. Needlejack random jump filament
    let random_res = rpc(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[])
        .await
        .expect("ActivateRandomJumpFilament");
    let dest_sys = random_res
        .dict_get("solarSystemID")
        .and_then(|v| v.as_i64())
        .expect("solarSystemID") as u32;

    // Verify destination system is a null-sec system in SDE
    let sys_record = ctx.sde.solar_systems.get(&dest_sys).expect("solar system in SDE");
    assert!(
        sys_record.security_status < 0.0,
        "Needlejack must jump to a null-sec system (sec: {})",
        sys_record.security_status
    );
    assert_eq!(sess.solar_system_id, Some(dest_sys));

    // Verify needlejack filament consumed
    let needle_exists = ctx.db.with_snapshot(|snap| snap.items.contains_key(&1_300_000_002));
    assert!(!needle_exists, "Needlejack filament was consumed");
}

#[tokio::test]
async fn test_air_npe_state_persistence_and_career_goals() {
    // parity: feat:pve.air-npe, feat:pve.air-career-program
    eve_testkit::attest(&[
        "feat:pve.air-npe",
        "feat:pve.air-career-program",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 8003, "demo", "demo-secret", char_id).await;

    // 1. Initial AIR NPE state
    let init_state = rpc(&ctx, &mut sess, "air_npe", "get_air_npe_state", &[])
        .await
        .expect("get_air_npe_state initial");
    assert_eq!(init_state.dict_get("nodeGraphID").and_then(|v| v.as_i64()), Some(473));
    assert_eq!(init_state.dict_get("currentNodeID").and_then(|v| v.as_i64()), Some(1));

    // 2. Skip AIR NPE
    let skip_state = rpc(&ctx, &mut sess, "air_npe", "skip_air_npe", &[])
        .await
        .expect("skip_air_npe");
    assert_eq!(skip_state.dict_get("currentNodeID").and_then(|v| v.as_i64()), Some(4));
    assert_eq!(skip_state.dict_get("completed").and_then(|v| v.as_bool()), Some(true));

    // 3. Verify persistence of completed state
    let get_state = rpc(&ctx, &mut sess, "air_npe", "get_air_npe_state", &[])
        .await
        .expect("get_air_npe_state after skip");
    assert_eq!(get_state.dict_get("currentNodeID").and_then(|v| v.as_i64()), Some(4));
    assert_eq!(get_state.dict_get("completed").and_then(|v| v.as_bool()), Some(true));

    // 4. AIR Career Program Goal Progress (Mining Goal 2)
    let initial_goal = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_character_progress_by_character_id",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("get_character_progress initial");
    assert_eq!(
        initial_goal.dict_get("currentProgress").and_then(|v| v.as_i64()),
        Some(0)
    );

    // Record server-observed mining events in the mining ledger
    ctx.db.with_mut_snapshot(|snap| {
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id,
            solar_system_id: 30_000_142,
            type_id: 1230, // Veldspar
            quantity: 1_000,
            date: "2026-01-01".into(),
            timestamp_unix: 1_704_067_200,
        });
    });

    // Verify progress updated from server events
    let updated_goal = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_character_progress_by_character_id",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("get_character_progress after mining");
    assert_eq!(
        updated_goal.dict_get("currentProgress").and_then(|v| v.as_i64()),
        Some(1000)
    );

    // Claim goal reward
    let sp_before = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| c.free_skill_points).unwrap_or(0.0)
    });
    let claim_res = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_goal_reward",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("claim_goal_reward");
    assert_eq!(claim_res.dict_get("goalID").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(
        claim_res.dict_get("spReward").and_then(|v| v.as_f64()),
        Some(50_000.0)
    );

    let sp_after = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| c.free_skill_points).unwrap_or(0.0)
    });
    assert!(sp_after > sp_before, "SP reward must be credited to character");

    // Second claim must be rejected
    let repeat_claim = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_goal_reward",
        &[PyRep::Integer(2)],
    )
    .await
    .expect_err("Duplicate claim must be rejected");
    // rpc-lint: allow error code check
    assert_eq!(repeat_claim.code, "GoalAlreadyClaimed");
}

#[tokio::test]
async fn test_crates_claim_loot_and_milestones_one_time_rewards() {
    // parity: feat:pve.crates, feat:pve.milestones
    eve_testkit::attest(&["feat:pve.crates", "feat:pve.milestones"]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 8004, "demo", "demo-secret", char_id).await;

    ctx.db.with_mut_snapshot(|snap| {
        for crate_id in [8001_i64, 8002_i64] {
            snap.items.insert(
                crate_id,
                eve_db::InventoryItemRecord {
                    item_id: crate_id,
                    type_id: 34_149,
                    owner_id: char_id,
                    location_id: 60_003_760,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    // 1. Open Crate
    let open_res = rpc(&ctx, &mut sess, "crateService", "OpenCrate", &[PyRep::Integer(8001)])
        .await
        .expect("OpenCrate");
    assert_eq!(open_res.dict_get("crateID").and_then(|v| v.as_i64()), Some(8001));
    assert!(open_res.dict_get("lootItems").is_some());

    // 2. Claim Loot
    let claim_res = rpc(&ctx, &mut sess, "crateService", "ClaimLoot", &[PyRep::Integer(8001)])
        .await
        .expect("ClaimLoot");
    assert_eq!(claim_res.dict_get("crateID").and_then(|v| v.as_i64()), Some(8001));

    // Second claim of same crate loot must fail
    let repeat_loot = rpc(&ctx, &mut sess, "crateService", "ClaimLoot", &[PyRep::Integer(8001)])
        .await
        .expect_err("Crate loot must only be claimable once");
    assert!(matches!(
        repeat_loot.code.as_str(),
        "CrateNotOpened" | "LootAlreadyClaimed" | "CrateLootAlreadyClaimed"
    ));

    // 3. Claim Skill Bundle
    let _ = rpc(&ctx, &mut sess, "crateService", "OpenCrate", &[PyRep::Integer(8002)])
        .await
        .expect("OpenCrate 8002");
    let sp_before = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| c.free_skill_points).unwrap_or(0.0)
    });
    let bundle_res = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "ClaimSkillBundle",
        &[PyRep::Integer(8002), PyRep::Integer(1)],
    )
    .await
    .expect("ClaimSkillBundle");
    assert_eq!(bundle_res.dict_get("crateID").and_then(|v| v.as_i64()), Some(8002));
    assert_eq!(bundle_res.dict_get("skillBundleID").and_then(|v| v.as_i64()), Some(1));

    let sp_after = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| c.free_skill_points).unwrap_or(0.0)
    });
    assert_eq!(sp_after - sp_before, 25_000.0);

    // Second claim of same skill bundle must fail
    let repeat_bundle = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "ClaimSkillBundle",
        &[PyRep::Integer(8002), PyRep::Integer(1)],
    )
    .await
    .expect_err("Skill bundle must only be claimable once");
    assert!(matches!(
        repeat_bundle.code.as_str(),
        "CrateNotOpened" | "SkillBundleAlreadyClaimed"
    ));

    // 4. Milestone Logon & Claim
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = eve_server::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap);
        ext.completed_milestones.entry(char_id).or_default().push(1);
        eve_server::services::cosmetics_ui_ops::save_cosmetics_ui_state(snap, &ext);
    });
    let logon_res = rpc(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[])
        .await
        .expect("ProcessCharacterLogon");
    assert_eq!(logon_res.dict_get("characterID").and_then(|v| v.as_i64()), Some(char_id));

    let milestone_res = rpc(
        &ctx,
        &mut sess,
        "milestoneMgr",
        "ClaimRewards",
        &[PyRep::Integer(1)],
    )
    .await
    .expect("ClaimRewards milestone 1");
    assert_eq!(milestone_res.dict_get("milestoneID").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(
        milestone_res.dict_get("spRewarded").and_then(|v| v.as_f64()),
        Some(10_000.0)
    );

    // Second claim of milestone 1 must fail
    let repeat_milestone = rpc(
        &ctx,
        &mut sess,
        "milestoneMgr",
        "ClaimRewards",
        &[PyRep::Integer(1)],
    )
    .await
    .expect_err("Milestone must only be claimable once");
    assert_eq!(repeat_milestone.code, "MilestoneAlreadyClaimed");
}

#[tokio::test]
async fn test_daily_and_seasonal_login_campaign_rewards() {
    // parity: feat:pve.login-campaign, feat:pve.seasonal-login
    eve_testkit::attest(&["feat:pve.login-campaign", "feat:pve.seasonal-login"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 8005, "demo", "demo-secret", char_id).await;

    // 1. Daily login campaign
    let state = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[])
        .await
        .expect("get_client_campaign_state");
    assert_eq!(state.dict_get("canClaimToday").and_then(|v| v.as_bool()), Some(true));

    let claim1 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward day 1");
    assert_eq!(claim1.dict_get("spGranted").and_then(|v| v.as_f64()), Some(5_000.0));

    // Duplicate claim on same day must be rejected
    let repeat_day1 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect_err("Duplicate daily claim on same day must fail");
    // rpc-lint: allow error code check
    assert_eq!(repeat_day1.code, "RewardAlreadyClaimedToday");

    // Advance clock by 1 full day (86,400 seconds)
    ctx.advance_virtual_clock(86_400);

    let state_day2 = rpc(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[])
        .await
        .expect("get_client_campaign_state day 2");
    assert_eq!(state_day2.dict_get("canClaimToday").and_then(|v| v.as_bool()), Some(true));

    let claim2 = rpc(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward day 2");
    assert_eq!(claim2.dict_get("spGranted").and_then(|v| v.as_f64()), Some(5_000.0));

    // 2. Seasonal login campaign
    let active_seasonal = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_active_campaign",
        &[],
    )
    .await
    .expect("get_active_campaign");
    assert_eq!(
        active_seasonal.dict_get("campaignID").and_then(|v| v.as_i64()),
        Some(202)
    );
    assert_eq!(
        active_seasonal.dict_get("daysRemaining").and_then(|v| v.as_i64()),
        Some(14)
    );

    let seasonal_claim = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "claim_reward",
        &[],
    )
    .await
    .expect("seasonal claim_reward");
    assert_eq!(
        seasonal_claim.dict_get("spGranted").and_then(|v| v.as_f64()),
        Some(15_000.0)
    );

    // Duplicate seasonal claim on same day must fail
    let repeat_seasonal = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "claim_reward",
        &[],
    )
    .await
    .expect_err("Duplicate seasonal claim on same day must fail");
    // rpc-lint: allow error code check
    assert_eq!(repeat_seasonal.code, "SeasonalRewardAlreadyClaimedToday");

    let history = rpc(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_claim_history",
        &[],
    )
    .await
    .expect("get_claim_history");
    let PyRep::List(days) = history else {
        panic!("Expected list from get_claim_history");
    };
    assert!(!days.is_empty(), "Claim history must record claimed day");
}

#[tokio::test]
async fn test_custom_agency_provider_content_nearby_sites() {
    // parity: feat:pve.agency-content
    eve_testkit::attest(&["feat:pve.agency-content"]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let sys_id = 30_002_053; // HEK
    let mut sess = setup_pilot(&ctx, 8006, "demo", "demo-secret", char_id).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;

    let content = rpc(&ctx, &mut sess, "custom_agency_provider", "get_content_data", &[])
        .await
        .expect("get_content_data");
    let PyRep::List(entries) = content else {
        panic!("Expected list from get_content_data");
    };
    assert!(!entries.is_empty(), "Agency must return nearby system content");

    let mut has_combat = false;
    let mut has_sig = false;
    for entry in entries {
        if entry.dict_get("solarSystemID").and_then(|v| v.as_i64()) == Some(sys_id as i64) {
            match entry.dict_get("contentType").and_then(|v| v.as_str()) {
                Some("combatAnomaly") => has_combat = true,
                Some("signature") => has_sig = true,
                _ => {}
            }
        }
    }
    assert_eq!(has_combat, true, "Agency content must include combat anomalies");
    assert_eq!(has_sig, true, "Agency content must include signatures");
}

#[tokio::test]
async fn test_cosmos_constellations_storyline_agents_and_sites() {
    // parity: feat:pve.cosmos
    eve_testkit::attest(&["feat:pve.cosmos"]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 8007, "demo", "demo-secret", char_id).await;

    // 1. Verify COSMOS constellations in SDE contain storyline agents
    let vaarma = ctx.sde.constellations.get(&20_000_449).expect("Vaarma constellation");
    assert_eq!(vaarma.name, "Vaarma");

    let cosmos_agents_in_vaarma: Vec<i64> = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.agent_type_id == 9
                && a.solar_system_id
                    .and_then(|sid| ctx.sde.solar_systems.get(&sid))
                    .map(|s| s.constellation_id == 20_000_449)
                    .unwrap_or(false)
        })
        .map(|a| a.agent_id)
        .collect();
    assert!(
        !cosmos_agents_in_vaarma.is_empty(),
        "Vaarma COSMOS constellation must contain COSMOS storyline agents"
    );

    // 2. Verify unique COSMOS sites in SDE
    let cosmos_dungeons: Vec<&eve_sde::DungeonTemplate> = ctx
        .sde
        .dungeons
        .values()
        .filter(|d| d.archetype_id == Some(26) || d.name.contains("Contested"))
        .collect();
    assert!(!cosmos_dungeons.is_empty(), "Unique COSMOS sites must exist in SDE");
    let has_caldari_mining = cosmos_dungeons
        .iter()
        .any(|d| d.name.contains("Contested Caldari Mining Facilities"));
    assert!(
        has_caldari_mining,
        "Contested Caldari Mining Facilities COSMOS site must exist in SDE"
    );

    // 3. COSMOS Storyline Agent Interaction (Level 1 COSMOS Agent 3019001)
    let agent_id = 3_019_001;
    let cosmos_agent = ctx.sde.agents.get(&agent_id).expect("COSMOS agent 3019001");
    assert_eq!(cosmos_agent.agent_type_id, 9, "Agent type must be 9 (COSMOS)");
    let agent_corp_id = cosmos_agent.corporation_id;
    let agent_sys_id = cosmos_agent.solar_system_id.expect("Agent system");
    let agent_station_id = cosmos_agent.station_id.expect("Agent station");

    // Colocate pilot with agent and set standing >= 2.0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = agent_sys_id;
            ch.station_id = Some(agent_station_id);
            ch.structure_id = None;
            ch.standings.insert(agent_corp_id, 5.0);
        }
    });
    sess.solar_system_id = Some(agent_sys_id);
    sess.station_id = Some(agent_station_id);
    sess.structure_id = None;

    // Request mission offer from COSMOS agent
    let offer = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)])
        .await
        .expect("COSMOS agent offer");
    let mission_id = offer
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .expect("missionID") as u32;

    // Accept mission
    rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mission_id as i64)])
        .await
        .expect("Accept COSMOS mission");

    // Complete mission
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
            m.state = "Completed".into();
        }
    });

    // Subsequent mission requests must be rejected due to one-time completion
    let repeat_err = rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)])
        .await
        .expect_err("Repeat COSMOS agent mission must be rejected");
    assert_eq!(repeat_err.code, "OneTimeAgentMissionCompleted");
}
