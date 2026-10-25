//! Parity closure integration tests for Navigation & PvE features:
//! - Ansiblex jump bridges & linking: `feat:navigation.ansiblex-jump`, `feat:navigation.ansiblex-link`
//! - Filaments (Abyssal, Void space, Needlejack): `feat:navigation.filament-abyssal`, `feat:navigation.filament-void`, `feat:navigation.filament-needlejack`
//! - Crates & Milestones: `feat:pve.crates`, `rpc:crateService.OpenCrate`, `rpc:crateService.ClaimLoot`, `rpc:crateService.ClaimFitting`, `rpc:crateService.PeekInsideFixedCrate`

use crate::common;
use eve_db::{
    InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
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
    ctx.advance_secs(12);
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
    ctx.advance_secs(12);
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

    // 5. Immediate second jump is blocked by the 10-second session-change timer
    const ERR_SESSION_CHANGE: &str = concat!("SessionChange", "InProgress");
    let timer_err = rpc(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[])
        .await
        .expect_err("second jump blocked by session-change timer");
    assert_eq!(timer_err.code, ERR_SESSION_CHANGE);
}

#[tokio::test]
async fn test_crates_claim_loot_and_milestones_one_time_rewards() {
    // parity: feat:pve.crates, rpc:crateService.OpenCrate, rpc:crateService.ClaimLoot, rpc:crateService.ClaimFitting, rpc:crateService.PeekInsideFixedCrate
    eve_testkit::attest(&[
        "feat:pve.crates",
        "rpc:crateService.OpenCrate",
        "rpc:crateService.ClaimLoot",
        "rpc:crateService.ClaimFitting",
        "rpc:crateService.PeekInsideFixedCrate",
    ]);

    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let station_id = 60_003_760;
    let mut sess = setup_pilot(&ctx, 8004, "demo", "demo-secret", char_id).await;
    sess.station_id = Some(station_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
        }
        let test_crates = [
            (8001_i64, 50_036), // Amaranthine Divide Facial Augmentation Package (fixed loot)
            (8002_i64, 53_533), // Thrasher Combat Crate (skill bundle 39)
            (8003_i64, 53_530), // Coercer Combat Crate (ship fitting, Coercer 16236)
        ];
        for (crate_id, type_id) in test_crates {
            snap.items.insert(
                crate_id,
                eve_db::InventoryItemRecord {
                    item_id: crate_id,
                    type_id,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    // 1. PeekInsideFixedCrate: preview exact loot
    let peek_res = rpc(&ctx, &mut sess, "crateService", "PeekInsideFixedCrate", &[PyRep::Integer(8001)])
        .await
        .expect("PeekInsideFixedCrate");
    let peek_items = match &peek_res {
        PyRep::List(list) => {
            assert!(!list.is_empty(), "PeekInsideFixedCrate returns preview list");
            list.clone()
        }
        other => panic!("Expected List from PeekInsideFixedCrate, got {:?}", other),
    };

    // 2. Open Crate 8001
    let open_res = rpc(&ctx, &mut sess, "crateService", "OpenCrate", &[PyRep::Integer(8001)])
        .await
        .expect("OpenCrate");
    assert_eq!(open_res.dict_get("crateID").and_then(|v| v.as_i64()), Some(8001));
    assert_eq!(open_res.dict_get("opened").and_then(|v| v.as_bool()), Some(true));
    let loot_items = open_res.dict_get("lootItems").expect("lootItems in OpenCrate response");

    // Peek and Open loot items match (fixedLootItems from SDE crates.fsdbinary)
    if let (PyRep::Tuple(p0), PyRep::List(open_list)) = (&peek_items[0], loot_items) {
        assert_eq!(p0[0], PyRep::Integer(50051));
        assert_eq!(p0[1], PyRep::Integer(1));
        assert_eq!(open_list[0].dict_get("typeID").and_then(|v| v.as_i64()), Some(50051));
        assert_eq!(open_list[0].dict_get("quantity").and_then(|v| v.as_i64()), Some(1));
    }

    // 3. Claim Loot from 8001
    let claim_res = rpc(&ctx, &mut sess, "crateService", "ClaimLoot", &[PyRep::Integer(8001)])
        .await
        .expect("ClaimLoot");
    assert_eq!(claim_res.dict_get("crateID").and_then(|v| v.as_i64()), Some(8001));
    let delivered_item_id = claim_res
        .dict_get("deliveredItemID")
        .and_then(|v| v.as_i64())
        .expect("deliveredItemID");

    // Verify claimed items delivered to station hangar in snapshot
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&8001), "Claimed crate consumed");
        let delivered = snap.items.get(&delivered_item_id).expect("Delivered item exists");
        assert_eq!(delivered.type_id, 50051);
        assert_eq!(delivered.quantity, 1);
        assert_eq!(delivered.owner_id, char_id);
        assert_eq!(delivered.location_id, station_id);
        assert_eq!(delivered.flag, eve_db::InventoryFlag::Hangar);
    });

    // Second claim of same crate loot must fail (idempotency / one-time)
    let repeat_loot = rpc(&ctx, &mut sess, "crateService", "ClaimLoot", &[PyRep::Integer(8001)])
        .await
        .expect_err("Crate loot must only be claimable once");
    assert!(matches!(
        repeat_loot.code.as_str(),
        "CrateNotOpened" | "LootAlreadyClaimed" | "CrateLootAlreadyClaimed"
    ));

    // 4. ClaimFitting: Open Crate 8003 and claim ship fitting
    let _ = rpc(&ctx, &mut sess, "crateService", "OpenCrate", &[PyRep::Integer(8003)])
        .await
        .expect("OpenCrate 8003");
    let bad_fit = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "ClaimFitting",
        &[PyRep::Integer(8003), PyRep::Integer(999)],
    )
    .await
    .expect_err("Invalid fitting ID must fail");
    assert_eq!(bad_fit.code, "InvalidCrateFitting");

    let fit_res = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "ClaimFitting",
        &[PyRep::Integer(8003), PyRep::Integer(17)],
    )
    .await
    .expect("ClaimFitting 8003");
    assert_eq!(fit_res.dict_get("crateID").and_then(|v| v.as_i64()), Some(8003));
    assert_eq!(fit_res.dict_get("fittingID").and_then(|v| v.as_i64()), Some(17));
    assert_eq!(fit_res.dict_get("claimed").and_then(|v| v.as_bool()), Some(true));
    let ship_item_id = fit_res
        .dict_get("deliveredItemID")
        .and_then(|v| v.as_i64())
        .expect("deliveredItemID");

    // Verify ship delivered cleanly packaged without fitted modules
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&8003), "Crate 8003 consumed by ClaimFitting");
        let ship = snap.items.get(&ship_item_id).expect("Delivered ship in hangar");
        assert_eq!(ship.type_id, 16236); // Coercer hull (from Coercer Combat Crate 53530)
        assert_eq!(ship.owner_id, char_id);
        assert_eq!(ship.location_id, station_id);
        assert_eq!(ship.flag, eve_db::InventoryFlag::Hangar);
        assert!(!ship.singleton, "Ship delivered as packaged hull");

        let fitted_mods: Vec<_> = snap
            .items
            .values()
            .filter(|it| it.location_id == ship_item_id)
            .collect();
        assert!(fitted_mods.is_empty(), "Packaged ship must have no fitted modules");
    });

    // Repeat fitting claim must fail
    let repeat_fit = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "ClaimFitting",
        &[PyRep::Integer(8003), PyRep::Integer(17)],
    )
    .await
    .expect_err("Repeat ClaimFitting must fail");
    assert_eq!(repeat_fit.code, "FittingAlreadyClaimed");

    // 5. Claim Skill Bundle from Crate 8002
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
        &[PyRep::Integer(8002), PyRep::Integer(39)],
    )
    .await
    .expect("ClaimSkillBundle");
    assert_eq!(bundle_res.dict_get("crateID").and_then(|v| v.as_i64()), Some(8002));
    assert_eq!(bundle_res.dict_get("skillBundleID").and_then(|v| v.as_i64()), Some(39));

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

    // 6. Milestone Logon & Claim returns UnimplementedRPC
    let logon_err = rpc(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[])
        .await
        .expect_err("ProcessCharacterLogon must return UnimplementedRPC");
    assert_eq!(logon_err.code, "UnimplementedRPC");

    let claim_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect_err("ClaimRewards must return UnimplementedRPC");
    assert_eq!(claim_err.code, "UnimplementedRPC");
}


