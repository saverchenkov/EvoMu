//! Integration regression tests for disconnected ship emergency-warp blocks,
//! mid-tunnel warp preservation, bubble interception, and active cyno hold lifecycle.

#![forbid(unsafe_code)]

use crate::common::*;
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode, WarpPhase};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use eve_server::services::inventory_fitting::fitting_cyno_portal::is_ship_cyno_active;
use eve_server::services::pve_crab_beacons::{
    CrabBeaconPhase, CrabBeaconRecord, TYPE_CRAB_BEACON,
};
use eve_server::services::tactical_modules::ActiveEntosisLinkState;
use eve_server::sp6_lifecycle::{
    handle_character_logoff_or_disconnect, reconnect_character_in_space_sync,
};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

const CHAR_PRIMARY: i64 = 90_000_001;

fn seed_stack(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton,
                version: 1,
                ..Default::default()
            },
        );
    });
}

fn grant_skills_for(ctx: &ServerContext, char_id: i64, type_ids: &[u32]) {
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &tid in type_ids {
            if let Some(t) = ctx.sde.types.get(&tid) {
                for aid in [
                    attr_ids::REQUIRED_SKILL_1,
                    attr_ids::REQUIRED_SKILL_2,
                    attr_ids::REQUIRED_SKILL_3,
                ] {
                    if let Some(&req) = t.default_attributes.get(&aid) {
                        let req_id = req as u32;
                        if req_id > 0 {
                            entry.insert(
                                req_id,
                                eve_db::TrainedSkillRecord {
                                    type_id: req_id,
                                    level: 5,
                                    skill_points: 256_000.0,
                                },
                            );
                        }
                    }
                }
            }
        }
    });
}

async fn spawn_vessel(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    type_id: u32,
    owner_id: i64,
    pos: DVec3,
    cap: f64,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let t = &ctx.sde.types[&type_id];
    let mut cs = ctx.build_type_combat_state(type_id);
    cs.capacitor = cap;
    cs.max_capacitor = cap;
    let meta = ctx.build_ship_slim_meta(owner_id, type_id);
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.5,
            mass: t.mass.max(10_000_000.0),
            radius: t.radius.max(100.0),
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
        meta,
    )
    .await
    .expect("spawn_vessel");
}

fn make_structure(id: i64, type_id: u32, corp_id: i64, sys_id: u32) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id: id,
        type_id,
        owner_corp_id: corp_id,
        solar_system_id: sys_id,
        name: format!("TestStructure-{id}"),
        position: [100_000.0, 200_000.0, 300_000.0],
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks: 1_000,
        shield_hp: 1.0,
        armor_hp: 1.0,
        hull_hp: 1.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

#[tokio::test]
async fn disconnect_emergency_warp_blocked_for_nav_locks_immobilizers_and_structures() {
    let ctx = Arc::new(ServerContext::new());
    pin_clock(&ctx);

    let sys_id: u32 = 30_000_144;
    let now = ctx.now_unix_secs();
    let ship_id: i64 = 892_100_001;
    let origin = DVec3::new(100_000.0, 200_000.0, 300_000.0);

    seed_stack(&ctx, ship_id, 77_283, CHAR_PRIMARY, sys_id as i64, InventoryFlag::None, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&CHAR_PRIMARY).unwrap();
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
    });
    spawn_vessel(&ctx, sys_id, ship_id, 77_283, CHAR_PRIMARY, origin, 20_000.0).await;
    let sys = ctx.systems.get_or_spawn(sys_id);

    // 1. Orbital Skyhook link blocks emergency warp and cancels Aligning warp
    let _ = sys
        .set_movement_mode(
            ship_id,
            MovementMode::new_warp(origin, origin + DVec3::new(1_000_000.0, 0.0, 0.0), 3.0),
        )
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .skyhook_silo_links
            .insert(99_001, (CHAR_PRIMARY, ship_id, now + 600, 1_099_001.0));
    });
    let entry = handle_character_logoff_or_disconnect(&ctx, CHAR_PRIMARY)
        .await
        .expect("disconnected entry");
    assert!(entry.emergency_warp_dest.is_none());
    assert!(entry.pre_warp_position.is_none());
    let ball_mode = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode;
    assert!(matches!(ball_mode, MovementMode::Stop));
    assert!(entry.remove_at_unix >= now + 600);
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.skyhook_silo_links.clear();
        snap.sp6_lifecycle.disconnected_ships.remove(&CHAR_PRIMARY);
    });

    // 2. CRAB beacon link, Entosis link, Lance victim debuff, Clone Vat Bay, Immobilizer, Upwell Structure
    let db_key = Arc::as_ptr(&ctx.db) as usize;
    ctx.db.with_mut_snapshot(|snap| {
        snap.crab_beacons.beacons.insert(
            99_002,
            CrabBeaconRecord {
                beacon_id: 99_002,
                type_id: TYPE_CRAB_BEACON,
                deployer_char_id: CHAR_PRIMARY,
                solar_system_id: sys_id,
                position: [origin.x, origin.y, origin.z],
                deployed_at_sec: now,
                phase: CrabBeaconPhase::Linking,
                linked_char_id: Some(CHAR_PRIMARY),
                linked_ship_id: Some(ship_id),
                link_started_at_sec: now,
                link_completes_at_sec: now + 240,
                scan_completes_at_sec: 0,
                self_destruct_at_sec: 0,
                waves_spawned: 0,
                wave_npc_ball_ids: vec![],
                open_container_id: None,
            },
        );
    });
    let crab_entry = handle_character_logoff_or_disconnect(&ctx, CHAR_PRIMARY)
        .await
        .expect("crab blocked entry");
    assert!(crab_entry.emergency_warp_dest.is_none());
    ctx.db.with_mut_snapshot(|snap| snap.crab_beacons.beacons.clear());

    {
        let mut st = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
        st.entosis_active_links.insert(
            (db_key, ship_id),
            ActiveEntosisLinkState {
                ship_id,
                char_id: CHAR_PRIMARY,
                side_id: 1_000_009,
                module_id: 99_003,
                target_id: 1_091_200_099,
                cycle_index: 1,
                cycle_duration_s: 300.0,
                started_unix: now,
                expires_unix: now + 300,
                is_capital: true,
            },
        );
    }
    let ent_entry = handle_character_logoff_or_disconnect(&ctx, CHAR_PRIMARY).await.unwrap();
    assert!(ent_entry.emergency_warp_dest.is_none());
    ctx.tactical_state
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .entosis_active_links
        .clear();

    ctx.tactical_state
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .lance_victim_debuff_until
        .insert((db_key, ship_id), now + 60);
    assert!(
        handle_character_logoff_or_disconnect(&ctx, CHAR_PRIMARY)
            .await
            .unwrap()
            .emergency_warp_dest
            .is_none()
    );
    ctx.tactical_state
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .lance_victim_debuff_until
        .clear();

    ctx.tactical_state
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .clone_vat_bay_activated_ships
        .insert((db_key, ship_id));
    assert!(
        handle_character_logoff_or_disconnect(&ctx, CHAR_PRIMARY)
            .await
            .unwrap()
            .emergency_warp_dest
            .is_none()
    );
    ctx.tactical_state
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .clone_vat_bay_activated_ships
        .clear();

    seed_stack(&ctx, 99_004, 28_583, CHAR_PRIMARY, ship_id, InventoryFlag::HiSlot0, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(99_004, true);
        snap.module_active_states.insert(99_004);
    });
    assert!(
        handle_character_logoff_or_disconnect(&ctx, CHAR_PRIMARY)
            .await
            .unwrap()
            .emergency_warp_dest
            .is_none()
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&99_004);
        snap.structures
            .insert(ship_id, make_structure(ship_id, 35_832, 1_000_009, sys_id));
    });
    assert!(
        handle_character_logoff_or_disconnect(&ctx, CHAR_PRIMARY)
            .await
            .unwrap()
            .emergency_warp_dest
            .is_none()
    );
    ctx.advance_secs(30);
    assert!(sys.get_ball_position_sync(ship_id).unwrap().distance(origin) < 10.0);
}

#[tokio::test]
async fn mid_tunnel_warp_preservation_gatecamp_bubble_interception_and_active_cyno_hold() {
    let ctx = Arc::new(ServerContext::new());
    pin_clock(&ctx);

    let sys_id: u32 = 30_000_144;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let ship_id: i64 = 892_200_001;
    let cyno_mod_id: i64 = 892_200_002;
    let ozone_id: i64 = 892_200_003;

    let origin = DVec3::ZERO;
    let committed_dest = DVec3::new(0.0, 0.0, 2_000_000_000.0);

    grant_skills_for(&ctx, CHAR_PRIMARY, &[22_428, 21_096]);
    seed_stack(&ctx, ship_id, 22_428, CHAR_PRIMARY, sys_id as i64, InventoryFlag::None, 1, true);
    seed_stack(&ctx, cyno_mod_id, 21_096, CHAR_PRIMARY, ship_id, InventoryFlag::HiSlot0, 1, true);
    seed_stack(&ctx, ozone_id, 16_273, CHAR_PRIMARY, ship_id, InventoryFlag::Cargo, 5_000, false);

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&CHAR_PRIMARY).unwrap();
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.fleets.insert(
            99_300,
            eve_db::FleetRecord {
                fleet_id: 99_300,
                boss_id: CHAR_PRIMARY,
                members: vec![CHAR_PRIMARY],
                ..Default::default()
            },
        );
        snap.module_online_states.insert(cyno_mod_id, true);
    });
    spawn_vessel(&ctx, sys_id, ship_id, 22_428, CHAR_PRIMARY, origin, 50_000.0).await;

    // 1. Committed mid-tunnel warp (`phase == WarpPhase::Cruising`) preserves destination and sets pre_warp_position = None
    {
        let mut bp = sys.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let ball = bp.balls.get_mut(&ship_id).unwrap();
        ball.position = DVec3::new(0.0, 0.0, 500_000_000.0);
        ball.velocity = DVec3::new(0.0, 0.0, 500_000_000.0);
        ball.mode = MovementMode::Warp {
            origin,
            destination: committed_dest,
            warp_speed_au_s: 3.0,
            total_distance: 2_000_000_000.0,
            phase: WarpPhase::Cruising,
            elapsed_warp_time: 5.0,
        };
    }
    let warp_disc = handle_character_logoff_or_disconnect(&ctx, CHAR_PRIMARY)
        .await
        .expect("committed warp disconnect entry");
    assert_eq!(
        warp_disc.emergency_warp_dest,
        Some([committed_dest.x, committed_dest.y, committed_dest.z])
    );
    assert!(warp_disc.pre_warp_position.is_none());

    // Mid-tunnel reconnect preserves forward Warp mode toward committed destination
    let _ = reconnect_character_in_space_sync(&ctx, CHAR_PRIMARY);
    let recon_mode = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode;
    match recon_mode {
        MovementMode::Warp { destination, .. } => assert_eq!(destination, committed_dest),
        other => panic!("Expected preserved forward Warp mode after mid-tunnel reconnect, got {other:?}"),
    }

    // 2. Gatecamp interdiction bubble intercepts disconnected warp without teleporting to destination
    {
        let mut bp = sys.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let ball = bp.balls.get_mut(&ship_id).unwrap();
        ball.position = DVec3::new(0.0, 0.0, 1_900_000_000.0);
        ball.velocity = DVec3::ZERO;
        ball.mode = MovementMode::Stop;
        if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
            cs.warp_exit_invulnerable_remaining_ticks = 30;
        }
        bp.add_warp_bubble(99_888, DVec3::new(0.0, 0.0, 1_900_100_000.0), 20_000.0);
    }
    let (stopped_pos, arrived) = sys.reconcile_disconnected_warp_sync(ship_id, committed_dest);
    assert!(arrived);
    assert!(
        stopped_pos.distance(committed_dest) > 50_000.0,
        "Bubble-caught ship must NOT overshoot/teleport to warp destination"
    );
    assert_eq!(
        sys.get_combat_state(ship_id)
            .await
            .unwrap()
            .unwrap()
            .warp_exit_invulnerable_remaining_ticks,
        0
    );
    sys.ballpark
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .remove_warp_bubble(99_888);

    // 3. Active Cyno holds disconnected ship for full 600s duration and despawn cleans up module + beacon
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.45 && s.region_id != 10_000_070 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_813);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_PRIMARY) {
            ch.solar_system_id = lowsec_sys;
            ch.station_id = None;
        }
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.location_id = lowsec_sys as i64;
        }
    });
    let lowsec_actor = ctx.systems.get_or_spawn(lowsec_sys);
    spawn_vessel(&ctx, lowsec_sys, ship_id, 22_428, CHAR_PRIMARY, DVec3::ZERO, 10_000.0).await;
    let mut sess = SessionState::new(9201);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(CHAR_PRIMARY)],
        &HashMap::new(),
        None,
    )
    .await
    .unwrap();
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(
            9_100_001,
            eve_db::FleetRecord {
                fleet_id: 9_100_001,
                boss_id: CHAR_PRIMARY,
                members: vec![CHAR_PRIMARY],
                ..Default::default()
            },
        );
    });
    sess.character_id = Some(CHAR_PRIMARY);
    sess.fleet_id = Some(9_100_001);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.solar_system_id = Some(lowsec_sys);

    let cyno_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(cyno_mod_id), PyRep::String("cynosuralGeneration".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("activate cyno");
    let cyno_dur_secs = cyno_rep
        .dict_get("durationSeconds")
        .and_then(|v| v.as_f64())
        .unwrap_or(600.0)
        .round() as u64;
    assert!(cyno_dur_secs >= 300);
    assert!(is_ship_cyno_active(&ctx, ship_id));

    let cyno_disc = handle_character_logoff_or_disconnect(&ctx, CHAR_PRIMARY)
        .await
        .expect("cyno disconnect entry");
    assert!(cyno_disc.remove_at_unix >= cyno_disc.disconnected_at_unix + cyno_dur_secs);

    // Past normal 60s NPC timer while cyno beacon is still active, ship and cyno beacon remain active
    let mid_wait = cyno_dur_secs / 2 + 10;
    ctx.advance_secs(mid_wait);
    assert!(lowsec_actor.get_ball_snapshot(ship_id).await.unwrap().is_some());
    assert!(is_ship_cyno_active(&ctx, ship_id));

    // After full cyno cycle expiry, ship despawns and cleans up active modules + cyno state
    ctx.advance_secs(cyno_dur_secs - mid_wait + 10);
    assert!(lowsec_actor.get_ball_snapshot(ship_id).await.unwrap().is_none());
    assert!(!is_ship_cyno_active(&ctx, ship_id));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.module_active_states.contains(&cyno_mod_id));
        assert!(!snap.module_active_charges.contains_key(&cyno_mod_id));
    });
}
