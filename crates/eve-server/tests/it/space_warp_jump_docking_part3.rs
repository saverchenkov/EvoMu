//! Integration tests for `SP-4` (Stargate Capital Restrictions, Session-Change Timers, & Jump Cloak Immunities).

use crate::common;

use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_sp4_stargate_capital_restriction_session_timers_and_jump_cloak_immunities() {
    // parity: plan:SP-4
    // parity: feat:navigation.stargate-capital-restriction, feat:navigation.jump-cloak-immunities, rpc:beyonce.SetSessionCheck, rpc:charUnboundMgr.SelectCharacterID
    eve_testkit::attest(&["feat:navigation.stargate-capital-restriction", "feat:navigation.jump-cloak-immunities"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(3003);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
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

    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_res else {
        panic!("Expected KeyVal from Undock");
    };
    let beyonce_id = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let jita_sys = 30_000_142;
    let jita_handle = ctx.systems.get_or_spawn(jita_sys);

    let jita_gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == jita_sys)
        .cloned()
        .expect("Jita stargate");
    let gate_id = jita_gate.stargate_id;
    let gate_pos = jita_gate.position;

    jita_handle
        .set_ball_position(ship_id, gate_pos + DVec3::new(1_000.0, 0.0, 0.0))
        .await;

    // 1. Stargate Capital Ship Restrictions (`feat:navigation.stargate-capital-restriction`)
    for &(cap_type_id, expected_group_id) in &[
        (23757_u32, 547_u32),  // Archon (Carrier)
        (19720_u32, 485_u32),  // Revelation (Dreadnought)
        (37604_u32, 1538_u32), // Apostle (Force Auxiliary)
        (23919_u32, 659_u32),  // Aeon (Supercarrier)
        (11567_u32, 30_u32),   // Avatar (Titan)
        (77281_u32, 4594_u32), // Bane (Lancer Dreadnought)
        (28352_u32, 883_u32),  // Rorqual (Capital Industrial Ship)
    ] {
        assert_eq!(ctx.sde.types[&cap_type_id].group_id, expected_group_id);
        let (_, mut b) = jita_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        b.type_id = cap_type_id;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(it) = snap.items.get_mut(&ship_id) {
                it.type_id = cap_type_id;
            }
        });
        jita_handle.spawn_ball(b).await.unwrap();

        ctx.advance_secs(12);
        let err = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(beyonce_id),
                "CmdStargateJump",
                &[PyRep::Integer(gate_id)],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "CapitalShipCannotJumpStargate");
    }

    for &(allowed_type_id, expected_group_id) in &[
        (20183_u32, 513_u32), // Providence (Freighter)
        (28844_u32, 902_u32), // Ark (Jump Freighter)
        (28606_u32, 941_u32), // Orca (Industrial Command Ship)
        (34328_u32, 513_u32), // Bowhead (Freighter)
    ] {
        assert_eq!(ctx.sde.types[&allowed_type_id].group_id, expected_group_id);
    }

    let (_, mut orca_ball) = jita_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    orca_ball.type_id = 28606; // Orca
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.type_id = 28606;
        }
    });
    jita_handle.spawn_ball(orca_ball).await.unwrap();

    let jump_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdStargateJump",
            &[PyRep::Integer(gate_id)],
            &kw,
            None,
        )
        .await
        .expect("Orca (28606) must be allowed to jump normal stargates");
    let PyRep::KeyVal(jkv) = jump_ok else {
        panic!("Expected KeyVal from CmdStargateJump");
    };
    let dest_sys_id = jkv.get("solarSystemID").unwrap().as_i64().unwrap() as u32;
    let dest_beyonce_id = jkv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    assert_eq!(dest_sys_id, jita_gate.to_solar_system_id);
    assert_eq!(jkv.get("jumpFatigueMinutes").unwrap().as_f64().unwrap(), 0.0);
    assert_eq!(jkv.get("jumpCooldownMinutes").unwrap().as_f64().unwrap(), 0.0);

    let dest_handle = ctx.systems.get_or_spawn(dest_sys_id);

    // 2. Post-Jump Cloak (60s) Immunities (`feat:navigation.jump-cloak-immunities`)
    let dest_cs = dest_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(dest_cs.is_cloaked);
    assert_eq!(dest_cs.gate_cloak_remaining_ticks, 60);

    let shield_before_bomb = dest_cs.shield_hp;
    dest_handle
        .apply_damage(
            ship_id,
            eve_destiny::DamageVector {
                em: 500.0,
                thermal: 500.0,
                kinetic: 500.0,
                explosive: 500.0,
            },
        )
        .await
        .unwrap();
    let dest_cs_after_bomb = dest_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(dest_cs_after_bomb.shield_hp, shield_before_bomb);

    let (_, jumped_ball) = dest_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let ecm_source_id = 930_001;
    dest_handle
        .spawn_ball_with_combat(
            Ball {
                id: ecm_source_id,
                type_id: 587,
                position: jumped_ball.position + DVec3::new(100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await
        .unwrap();
    let jammed_ids = dest_handle
        .apply_ecm_burst(
            ecm_source_id,
            5_000.0,
            [100.0, 100.0, 100.0, 100.0],
            1.0,
            0.0,
        )
        .await
        .unwrap();
    assert!(!jammed_ids.contains(&ship_id));

    let bumper_id = 930_002;
    dest_handle
        .spawn_ball_with_combat(
            Ball {
                id: bumper_id,
                type_id: 642,
                position: jumped_ball.position + DVec3::new(10.0, 0.0, 0.0),
                velocity: DVec3::new(-500.0, 0.0, 0.0),
                max_velocity: 500.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 200.0,
                speed_fraction: 1.0,
                mode: MovementMode::GotoDirection {
                    direction: DVec3::NEG_X,
                },
            },
            ctx.build_type_combat_state(642),
        )
        .await
        .unwrap();
    dest_handle
        .step_manual(1.0)
        .await
        .expect("step with overlapping bumper");
    let (_, jumped_ball_after_bump) = dest_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let cs_after_bump = dest_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_after_bump.is_cloaked && cs_after_bump.gate_cloak_remaining_ticks == 59);
    assert_eq!(jumped_ball_after_bump.velocity, DVec3::ZERO);
    let _ = dest_handle.remove_ball(bumper_id).await;
    let _ = dest_handle.remove_ball(ecm_source_id).await;

    // 3. 10s Session-Change Timer Triggers (`feat:navigation.session-change-timer-triggers`)
    let return_gate_id = jita_gate.destination_stargate_id;
    let return_gate_pos = ctx.sde.jumps[&return_gate_id].position;
    dest_handle
        .set_ball_position(ship_id, return_gate_pos + DVec3::new(1_000.0, 0.0, 0.0))
        .await;

    let early_jump_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(dest_beyonce_id),
            "CmdStargateJump",
            &[PyRep::Integer(return_gate_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(early_jump_err.code, "SessionChangeInProgress");

    for _ in 0..10 {
        dest_handle.step_manual(1.0).await.unwrap();
    }

    for trigger in [
        "login",
        "wormhole_jump",
        "jump_bridge",
        "jump_drive",
        "dock",
        "undock",
        "fleet_join",
        "fleet_position_change",
        "ship_board",
        "ship_eject",
        "pod_transition",
        "clone_jump",
        "clone_swap",
        "corp_join",
        "corp_leave",
        "wallet_division_change",
    ] {
        let trig_res = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(dest_beyonce_id),
                "SetSessionCheck",
                &[PyRep::String(trigger.into())],
                &kw,
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("SetSessionCheck({trigger}) failed: {e:?}"));
        let PyRep::KeyVal(tkv) = trig_res else {
            panic!("Expected KeyVal from SetSessionCheck");
        };
        assert_eq!(
            tkv.get("sessionChangeTimerSeconds")
                .unwrap()
                .as_i64()
                .unwrap(),
            10
        );
        if trigger == "jump_drive" {
            assert!(
                !eve_server::services::space_destiny::is_location_change_invulnerable(
                    &ctx,
                    char_id,
                    ctx.now_unix()
                ),
                "#48: client SetSessionCheck(\"jump_drive\") must not grant location-change invulnerability"
            );
        }

        let blocked = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(dest_beyonce_id),
                "SetSessionCheck",
                &[PyRep::String(trigger.into())],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(blocked.code, "SessionChangeInProgress");

        for _ in 0..10 {
            dest_handle.step_manual(1.0).await.unwrap();
        }
    }

    let jump_back = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(dest_beyonce_id),
            "CmdStargateJump",
            &[PyRep::Integer(return_gate_id)],
            &kw,
            None,
        )
        .await
        .expect("Jump back to Jita after 10s session timer elapsed");
    let PyRep::KeyVal(jb_kv) = jump_back else {
        panic!("Expected KeyVal from CmdStargateJump");
    };
    let jita_b_id = jb_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let cs_cloaked = jita_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_cloaked.is_cloaked && cs_cloaked.gate_cloak_remaining_ticks == 60);

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(jita_b_id),
        "CmdSetSpeedFraction",
        &[PyRep::Real(1.0)],
        &kw,
        None,
    )
    .await
    .expect("Move ship to break gate cloak");
    let cs_uncloaked = jita_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_uncloaked.is_cloaked && cs_uncloaked.gate_cloak_remaining_ticks == 0);
}

#[tokio::test]
async fn test_r_nav_01_structure_fuel_flag_172_ansiblex_and_jettison() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(3004);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");
    let corp_id = ctx.db.get_character(char_id).unwrap().corporation_id;
    let ship_id = ctx.db.get_character(char_id).unwrap().active_ship_id;
    let structure_id = 1_000_000_000_001i64;
    let fuel_item_id = 990_001i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(structure_id);
            ch.solar_system_id = 30_000_142;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = structure_id;
            ship.flag = eve_db::InventoryFlag::Hangar;
        }
        snap.structures.insert(structure_id, eve_db::UpwellStructureRecord {
            structure_id,
            solar_system_id: 30_000_142,
            type_id: 35832,
            owner_corp_id: corp_id,
            name: "Test Structure".to_string(),
            position: [0.0, 0.0, 0.0],
            state: "ShieldVulnerable".to_string(),
            quantum_core_installed: true,
            fuel_blocks: 0,
            shield_hp: 10_000_000.0,
            armor_hp: 10_000_000.0,
            hull_hp: 10_000_000.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        });
        snap.items.insert(fuel_item_id, eve_db::InventoryItemRecord {
            item_id: fuel_item_id,
            type_id: 4247, // Nitrogen Fuel Block
            owner_id: char_id,
            location_id: ship_id,
            flag: eve_db::InventoryFlag::Cargo,
            quantity: 500,
            singleton: false,
            ..Default::default()
        });
    });

    assert_eq!(eve_db::InventoryFlag::StructureFuel as u16, 172);
    assert_eq!(eve_db::InventoryFlag::from_u16(172), eve_db::InventoryFlag::StructureFuel);

    sess.structure_id = Some(structure_id);
    sess.station_id = Some(structure_id);
    sess.corporation_id = Some(corp_id);
    sess.ship_id = Some(ship_id);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(corp) = s.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER);
        }
    });

    // 1. Verify invbroker.Add whitelist rejects restricted flags 158..=163
    for bad_flag in 158..=163 {
        let err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(fuel_item_id), PyRep::Integer(structure_id), PyRep::Integer(bad_flag)], &kw, None).await.expect_err("rejected");
        assert_eq!(err.code, "InvalidInventoryFlag");
    }

    // 2. R-FUEL-03: Deposit fuel at flag 172 -> verify atomic fuel_blocks increment from 0 to 500
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(fuel_item_id), PyRep::Integer(structure_id), PyRep::Integer(172)], &kw, None).await.expect("invbroker.Add flag 172");
    let st_after_dep = ctx.db.with_snapshot(|s| s.structures.get(&structure_id).cloned().unwrap());
    assert_eq!(st_after_dep.fuel_blocks, 500, "R-FUEL-03: deposit at 172 must increment fuel_blocks to 500");

    // 3. JettisonStructureFuel -> verify post-jettison state and fuel_blocks decremented to 0
    ctx.dispatch_rpc(&mut sess, "structure", None, "JettisonStructureFuel", &[PyRep::List(vec![PyRep::Integer(fuel_item_id)])], &kw, None).await.expect("JettisonStructureFuel");
    let moved_fuel = ctx.db.get_item(fuel_item_id).expect("fuel item post-jettison");
    assert_eq!(moved_fuel.flag, eve_db::InventoryFlag::Cargo);
    assert_ne!(moved_fuel.location_id, structure_id);
    assert_eq!(moved_fuel.quantity, 500);
    let spawned_container = ctx.db.get_item(moved_fuel.location_id).expect("spawned space container");
    assert_eq!(spawned_container.type_id, 23);
    assert_eq!(spawned_container.location_id, 30_000_142);
    let st_after_jet = ctx.db.with_snapshot(|s| s.structures.get(&structure_id).cloned().unwrap());
    assert_eq!(st_after_jet.fuel_blocks, 0, "R-FUEL-03: jettison must decrement fuel_blocks to 0");

    // 4. R-FUEL-01: Withdrawing fuel from flag 172 requires fuel bay corp role
    let fuel2_id = 990_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&structure_id) { st.fuel_blocks = 20; }
        snap.items.insert(fuel2_id, eve_db::InventoryItemRecord {
            item_id: fuel2_id, type_id: 4247, owner_id: corp_id,
            location_id: structure_id, flag: eve_db::InventoryFlag::StructureFuel,
            quantity: 20, singleton: false, ..Default::default()
        });
    });
    sess.corp_role = 0;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(corp) = s.corporations.get_mut(&corp_id) { corp.member_roles.clear(); }
    });
    let denied = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(fuel2_id), PyRep::Integer(ship_id), PyRep::Integer(5)], &kw, None).await;
    let err_denied = denied.expect_err("R-FUEL-01: withdrawal without role must be rejected");
    assert_eq!(err_denied.code, "CorpRoleDenied");
    let item_denied = ctx.db.get_item(fuel2_id).unwrap();
    assert_eq!(item_denied.owner_id, corp_id);
    assert_eq!(item_denied.location_id, structure_id);

    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(corp) = s.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
        }
    });
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(fuel2_id), PyRep::Integer(ship_id), PyRep::Integer(5)], &kw, None).await.expect("withdrawal with role");
    let item_withdrawn = ctx.db.get_item(fuel2_id).unwrap();
    assert_eq!(item_withdrawn.owner_id, char_id);
    assert_eq!(item_withdrawn.location_id, ship_id);
    let st_after_w = ctx.db.with_snapshot(|s| s.structures.get(&structure_id).cloned().unwrap());
    assert_eq!(st_after_w.fuel_blocks, 0, "R-FUEL-01: withdrawal must decrement fuel_blocks");

    // 5. R-FUEL-02: Legacy quantum core stored at flag 172 is locked against removal
    let legacy_core_id = 990_003i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(legacy_core_id, eve_db::InventoryItemRecord {
            item_id: legacy_core_id, type_id: 56201, // Astrahus Upwell Quantum Core
            owner_id: corp_id, location_id: structure_id,
            flag: eve_db::InventoryFlag::StructureFuel, // legacy flag 172
            quantity: 1, singleton: false, ..Default::default()
        });
    });
    let core_rem = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(legacy_core_id), PyRep::Integer(ship_id), PyRep::Integer(5)], &kw, None).await;
    let core_err = core_rem.expect_err("R-FUEL-02: removal of quantum core at flag 172 must fail");
    assert_eq!(core_err.code, "ItemLocked");
    let core_item = ctx.db.get_item(legacy_core_id).unwrap();
    assert_eq!(core_item.location_id, structure_id, "core must remain in structure");
}

#[tokio::test]
async fn test_r_nav_02_sync_tether_immobilizing_module_lock_order() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let ship_id = 100_000_001;
    let sys_id = 30_000_142;
    let structure_id = 1_000_000_000_002i64;

    let corp_id = ctx.db.get_character(char_id).unwrap().corporation_id;
    let structure_pos = DVec3::new(100.0, 0.0, 0.0);

    let siege_mod_id = 880_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
        }
        snap.structures.insert(structure_id, eve_db::UpwellStructureRecord {
            structure_id,
            solar_system_id: sys_id,
            type_id: 35832, // Astrahus
            owner_corp_id: corp_id, // matching pilot corp for structure access
            name: "Test Tether Structure".to_string(),
            position: structure_pos.to_array(),
            state: "ShieldVulnerable".to_string(),
            quantum_core_installed: true,
            fuel_blocks: 500,
            shield_hp: 10_000_000.0,
            armor_hp: 10_000_000.0,
            hull_hp: 10_000_000.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        });
        // Siege module fitted on ship (initially inactive)
        snap.items.insert(siege_mod_id, eve_db::InventoryItemRecord {
            item_id: siege_mod_id,
            type_id: 20280, // Siege Module I (disallowTethering)
            owner_id: char_id,
            location_id: ship_id,
            flag: eve_db::InventoryFlag::HiSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        });
    });

    let handle = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id,
        type_id: 28659, // Paladin
        position: structure_pos, // inside tether range
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let cs = ctx.build_ship_combat_state(char_id, ship_id, 28659);
    handle.spawn_ball_with_combat(ball, cs).await.unwrap();

    // 1. Positive control: ship within range of accessible cored structure must tether
    eve_server::services::space_destiny::space_nav_travel::sync_tether(&ctx, char_id, sys_id, ship_id).await;
    let cs_tethered = handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_tethered.is_tethered, "Ship within range of accessible structure must tether (positive control)");

    // 2. Negative control & lock order: activate siege module (disallowTethering)
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(siege_mod_id);
    });

    // Verify sync_tether completes promptly without deadlock or recursive lock panic
    eve_server::services::space_destiny::space_nav_travel::sync_tether(&ctx, char_id, sys_id, ship_id).await;

    let cs_untethered = handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_untethered.is_tethered, "Ship with active siege module must break tether");
}
