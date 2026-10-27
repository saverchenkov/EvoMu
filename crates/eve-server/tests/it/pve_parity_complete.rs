//! PvE Parity Integration Tests:
//! - `feat:pve.hacking-minigame`
//! - `feat:pve.abyssal-gates`
//! - `feat:pve.abyssal-tiers`
//! - `feat:pve.insurgency`
//! - `feat:pve.pirate-strongholds`

use std::collections::HashMap;
use glam::DVec3;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::insurgency_services::{
    ensure_stronghold_in_space, init_insurgency_campaign, with_insurgency_runtime,
    FACTION_GURISTAS, PHASE_WINDDOWN, PIRATE_STRONGHOLD_GURISTAS_TYPE,
    PIRATE_STRONGHOLD_WRECK_TYPE,
};
use eve_server::services::pve_exploration::{
    ensure_system_pve_sites_seeded, with_pve_exploration_reg,
};
use eve_server::services::pve_special_sites::{
    classify_abyssal_filament, with_pve_special_reg,
};

const SYS_JITA: u32 = 30_000_142;
const PILOT_HACKER: i64 = 90_000_001;
const PILOT_ABYSS: i64 = 90_000_001;
const PILOT_INSURGENT: i64 = 90_000_001;

async fn setup_pilot_session(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    ship_id: i64,
    ship_type_id: u32,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(char_id as u64);

    let _ = ctx.dispatch_rpc(
        &mut sess, "authentication", None, "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &kw, None,
    ).await;

    let _ = ctx.dispatch_rpc(
        &mut sess, "charUnboundMgr", None, "SelectCharacterID",
        &[PyRep::Integer(char_id)], &kw, None,
    ).await;

    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id,
        type_id: ship_type_id,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 45.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;

    sess
}

// ============================================================================
// 1. Hacking Minigame
// ============================================================================
#[tokio::test]
// parity: feat:pve.hacking-minigame
async fn test_pve_hacking_minigame_complete() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let ship_id = 91_000_501;

    let mut sess = setup_pilot_session(&ctx, PILOT_HACKER, SYS_JITA, ship_id, 621).await;

    // Equip Data Analyzer I (22175) and Relic Analyzer I (22177)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id + 1,
            InventoryItemRecord {
                item_id: ship_id + 1,
                type_id: 22175,
                owner_id: PILOT_HACKER,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id + 2,
            InventoryItemRecord {
                item_id: ship_id + 2,
                type_id: 22177,
                owner_id: PILOT_HACKER,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // Seed exploration sites in system
    ensure_system_pve_sites_seeded(&ctx, SYS_JITA).await;
    let (can_id, site_pos) = with_pve_exploration_reg(&ctx, |reg| {
        let site = reg.sites_by_system[&SYS_JITA]
            .iter()
            .find(|s| s.site_category == "Data" || s.site_category == "Relic")
            .expect("seeded exploration site");
        (site.hackable_can_ids[0], site.position)
    });
    let jita_sys_handle = ctx.systems.get_or_spawn(SYS_JITA);
    let _ = jita_sys_handle.spawn_ball(Ball {
        id: can_id, type_id: 28359, position: DVec3::from_array(site_pos),
        velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1000.0, radius: 20.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    // BF-PVE-HACK-PROXIMITY-FAILOPEN-04: Reject hacking while docked, without ship, or ship not in space
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&PILOT_HACKER).unwrap().station_id = Some(60_003_760); });
    let docked_err = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can_id)], &kw, None).await.unwrap_err();
    assert_eq!(docked_err.code, "ShipDocked");
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&PILOT_HACKER).unwrap().station_id = None; });

    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&PILOT_HACKER).unwrap().active_ship_id = 0; });
    sess.ship_id = None;
    let no_ship_err = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can_id)], &kw, None).await.unwrap_err();
    assert_eq!(no_ship_err.code, "NoActiveShip");

    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&PILOT_HACKER).unwrap().active_ship_id = 999_888_777; });
    sess.ship_id = Some(999_888_777);
    let no_ball_err = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can_id)], &kw, None).await.unwrap_err();
    assert_eq!(no_ball_err.code, "ShipNotInSpace");
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&PILOT_HACKER).unwrap().active_ship_id = ship_id; });
    sess.ship_id = Some(ship_id);

    // BF-PVE-HACK-PROXIMITY-01: Out-of-range rejection when ship is at [0,0,0] (>40,000 km from site)
    let range_err = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can_id)], &kw, None)
        .await
        .expect_err("StartNewGameInstance out of analyzer range must fail");
    assert_eq!(range_err.code, "OutOfRange");

    // Missing container ID in args rejected
    let no_target_err = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::None], &kw, None)
        .await
        .expect_err("StartNewGameInstance without container ID must fail");
    assert_eq!(no_target_err.code, "TargetNotFound");

    // Move pilot ship to within analyzer maxRange of container
    let close_pos = DVec3::from_array(site_pos) + DVec3::new(500.0, 0.0, 0.0);
    let _ = jita_sys_handle.set_ball_position(ship_id, close_pos).await;

    // 1. StartNewGameInstance starts node-grid minigame with V24 [None, containerID] args
    let start_rep = ctx
        .dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::None, PyRep::Integer(can_id)], &kw, None)
        .await
        .expect("StartNewGameInstance");

    assert_eq!(start_rep.dict_get("containerID"), Some(&PyRep::Integer(can_id)));
    assert_eq!(start_rep.dict_get("status"), Some(&PyRep::String("InProgress".into())));
    let virus_coh = start_rep.dict_get("virusCoherence").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(virus_coh > 0, "Initial virus coherence must be > 0");

    // 2. Discover utility drop (DataCache with PolymorphicShield) along connected path from tile 0
    let shield_idx = with_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games[&(PILOT_HACKER, can_id)].tiles.iter().find(|t| format!("{:?}", t.utility_drop).contains("PolymorphicShield")).unwrap().tile_index
    });
    let mut shield_path: Vec<u32> = (1..=(shield_idx / 6)).map(|r| r * 6).collect();
    shield_path.extend(((shield_idx / 6) * 6 + 1)..=shield_idx);
    for idx in shield_path {
        for _ in 0..5 {
            let r = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(idx as i64)], &kw, None).await.expect("ClickedOnTile");
            if r.dict_get("nodeCoherence").and_then(|v| v.as_i64()).unwrap_or(0) <= 0 { break; }
        }
    }
    let shield_slot = with_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games[&(PILOT_HACKER, can_id)].utilities.iter().position(|u| format!("{:?}", u).contains("PolymorphicShield"))
    }).expect("Discovered PolymorphicShield utility in slot");

    // 3. UsedUtilityElement activates the PolymorphicShield element
    let util_rep = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "UsedUtilityElement", &[PyRep::Integer(can_id), PyRep::Integer(shield_slot as i64), PyRep::Integer(0)], &kw, None).await.expect("UsedUtilityElement");
    assert_eq!(util_rep.dict_get("containerID"), Some(&PyRep::Integer(can_id)));
    let shield_charges = util_rep.dict_get("polymorphicShieldCharges").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(shield_charges >= 2, "PolymorphicShield utility granted shield charges");

    // 4. Click tiles to navigate towards System Core and win game
    let core_idx = with_pve_exploration_reg(&ctx, |reg| {
        reg.hacking_games[&(PILOT_HACKER, can_id)].tiles.iter().find(|t| format!("{:?}", t.kind) == "SystemCore").unwrap().tile_index
    });
    let mut path: Vec<u32> = (1..=(core_idx / 6)).map(|r| r * 6).collect();
    path.extend(((core_idx / 6) * 6 + 1)..=core_idx);
    let mut win_rep = PyRep::None;
    for idx in path {
        for _ in 0..6 {
            let r = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(idx as i64)], &kw, None).await.expect("ClickedOnTile traversal");
            let done = r.dict_get("status").and_then(|v| v.as_str()) == Some("Won") || r.dict_get("nodeCoherence").and_then(|v| v.as_i64()).unwrap_or(0) <= 0;
            win_rep = r;
            if done { break; }
        }
    }

    // 5. Success spews loot mini-containers into space
    assert_eq!(win_rep.dict_get("status"), Some(&PyRep::String("Won".into())));
    let spewed = win_rep.dict_get("spewedContainers").expect("spewedContainers present on win");
    let PyRep::List(spewed_list) = spewed else { panic!("Expected List for spewedContainers") };
    assert!(!spewed_list.is_empty(), "Success must spew mini-containers with loot");

    // Verify mini-containers are materialized in world state
    let first_spew_id = spewed_list[0].dict_get("canItemID").and_then(|v| v.as_i64()).unwrap();
    let can_exists = ctx.db.with_snapshot(|snap| snap.items.contains_key(&first_spew_id));
    assert!(can_exists, "Spewed container materialized in world items");
}

// ============================================================================
// 2. Abyssal Gates & Tiers
// ============================================================================
#[tokio::test]
// parity: feat:pve.abyssal-gates, feat:pve.abyssal-tiers
async fn test_pve_abyssal_gates_and_tiers_complete() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let ship_id = 91_000_502;
    let t1_fil_item_id = 1_400_000_101;
    let t3_fil_item_id = 1_400_000_102;
    let t4_fil_item_id = 1_400_000_103;
    let fil_type_t1 = 47765; // Calm Electrical (T1)
    let fil_type_t3 = 47905; // Fierce Electrical (T3)
    let fil_type_t4 = 47906; // Raging Electrical (T4)

    // 1. Verify filament tier classification (T0 - T6)
    let (t0_tier, _) = classify_abyssal_filament(56131).expect("T0 filament");
    let (t1_tier, _) = classify_abyssal_filament(fil_type_t1).expect("T1 filament");
    let (t2_tier, _) = classify_abyssal_filament(47904).expect("T2 filament");
    let (t3_tier, _) = classify_abyssal_filament(47905).expect("T3 filament");
    let (t4_tier, _) = classify_abyssal_filament(fil_type_t4).expect("T4 filament");
    let (t5_tier, _) = classify_abyssal_filament(47907).expect("T5 filament");
    let (t6_tier, _) = classify_abyssal_filament(56139).expect("T6 filament");
    assert_eq!((t0_tier, t1_tier, t2_tier, t3_tier, t4_tier, t5_tier, t6_tier), (0, 1, 2, 3, 4, 5, 6));

    let mut sess = setup_pilot_session(&ctx, PILOT_ABYSS, SYS_JITA, ship_id, 621).await;

    // Seed filaments into ship cargo
    ctx.db.with_mut_snapshot(|snap| {
        let rec = |item_id, type_id| InventoryItemRecord {
            item_id,
            type_id,
            owner_id: PILOT_ABYSS,
            location_id: ship_id,
            flag: InventoryFlag::Cargo,
            quantity: 1,
            singleton: false,
            ..Default::default()
        };
        snap.items.insert(t1_fil_item_id, rec(t1_fil_item_id, fil_type_t1));
        snap.items.insert(t3_fil_item_id, rec(t3_fil_item_id, fil_type_t3));
        snap.items.insert(t4_fil_item_id, rec(t4_fil_item_id, fil_type_t4));
    });

    // 2. High-sec restriction: T4-T6 filaments blocked in high security space (Jita = 0.95 sec)
    let t4_deploy_err = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(t4_fil_item_id)], &kw, None)
        .await
        .expect_err("T4 filament deployment in Jita must fail");
    assert_eq!(t4_deploy_err.code, "FilamentDisallowedInHighSec");

    // 3. Deploy T1 Calm Electrical filament in Jita -> spawns trace
    let deploy_rep = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(t1_fil_item_id)], &kw, None)
        .await
        .expect("AbyssalEntranceDeployment T1");
    let trace_id = deploy_rep.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();

    // 4. Activate entrance gate into Abyssal Deadspace (Room 1)
    let enter_rep = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_id)], &kw, None)
        .await
        .expect("AbyssalEntranceGateActivation");
    assert_eq!(enter_rep.dict_get("currentRoom"), Some(&PyRep::Integer(1)));
    assert_eq!(enter_rep.dict_get("tier"), Some(&PyRep::Integer(1)));
    assert_eq!(enter_rep.dict_get("timerSeconds"), Some(&PyRep::Integer(1200)));

    // Verify T1 pocket difficulty mult (1.0 + 1 * 1.25 = 2.25)
    let t1_diff = with_pve_special_reg(&ctx, |reg| reg.abyssal_pockets.get(&PILOT_ABYSS).unwrap().npc_difficulty_mult);
    assert!((t1_diff - 2.25).abs() < 1e-3);

    // ClientIsReady handshake
    let ready_rep = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "ClientIsReady", &[], &kw, None)
        .await
        .expect("ClientIsReady");
    assert_eq!(ready_rep.dict_get("ready"), Some(&PyRep::Bool(true)));

    // BF-PVE-ABYSS-GATE-VALIDATION-01: Invalid gate ID rejected
    let bad_gate_err = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(999_999)], &kw, None)
        .await
        .expect_err("AbyssalGateActivation with invalid gate ID must fail");
    assert_eq!(bad_gate_err.code, "InvalidGate");

    // Verify Room 1 gate lock: AbyssalGateActivation blocked while Room 1 hostile entities alive
    let locked_r1 = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(1_895_000_001)], &kw, None)
        .await
        .expect_err("AbyssalGateActivation blocked while Room 1 hostile entities alive");
    assert_eq!(locked_r1.code, "GateLocked");

    // Verify AbyssalEndGateActivation rejected when not in room 3
    let early_exit = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEndGateActivation", &[PyRep::Integer(1_895_000_001)], &kw, None)
        .await
        .expect_err("End gate rejected in room 1");
    assert_eq!(early_exit.code, "NotInFinalRoom");

    // Clear Room 1 NPCs and check cache loot (T1 Calm filament -> 2 Survey Databases)
    crate::abyssal_room_combat::assert_room_cache_loot(&ctx, PILOT_ABYSS, 0, 2, fil_type_t1).await;
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_ABYSS, 0).await;

    // BF-PVE-ABYSS-GATE-VALIDATION-01: Proximity check rejects gate activation outside 2500m
    let abyss_sys = sess.solar_system_id.unwrap();
    let abyss_handle = ctx.systems.get_or_spawn(abyss_sys);
    let _ = abyss_handle.set_ball_position(ship_id, DVec3::new(0.0, 0.0, -10_000.0)).await;
    let far_gate_err = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(1_895_000_001)], &kw, None)
        .await
        .expect_err("AbyssalGateActivation outside 2500m must fail");
    assert_eq!(far_gate_err.code, "GateOutOfRange");
    let _ = abyss_handle.set_ball_position(ship_id, DVec3::new(100.0, 0.0, 0.0)).await;

    // 5. AbyssalGateActivation advances to Room 2
    let gate1_rep = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(1_895_000_001)], &kw, None)
        .await
        .expect("AbyssalGateActivation R1->R2");
    assert_eq!(gate1_rep.dict_get("room"), Some(&PyRep::Integer(2)));
    assert_eq!(gate1_rep.dict_get("gateActivated"), Some(&PyRep::Bool(true)));

    // Clear Room 2 NPCs and cache loot
    crate::abyssal_room_combat::assert_room_cache_loot(&ctx, PILOT_ABYSS, 1, 2, fil_type_t1).await;
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_ABYSS, 1).await;

    // 6. AbyssalGateActivation advances to Room 3
    let gate2_rep = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(1_895_000_002)], &kw, None)
        .await
        .expect("AbyssalGateActivation R2->R3");
    assert_eq!(gate2_rep.dict_get("room"), Some(&PyRep::Integer(3)));

    // Clear Room 3 NPCs
    crate::abyssal_room_combat::assert_room_cache_loot(&ctx, PILOT_ABYSS, 2, 2, fil_type_t1).await;
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_ABYSS, 2).await;

    // BF-PVE-ABYSS-GATE-VALIDATION-01: Invalid gate ID for exit conduit rejected
    let bad_exit = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEndGateActivation", &[PyRep::Integer(999_999)], &kw, None)
        .await
        .expect_err("End gate invalid ID");
    assert_eq!(bad_exit.code, "InvalidGate");

    // 7. AbyssalEndGateActivation exits Abyssal Deadspace and returns ship to Jita
    let exit_rep = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEndGateActivation", &[PyRep::Integer(1_895_000_003)], &kw, None)
        .await
        .expect("AbyssalEndGateActivation");
    assert_eq!(exit_rep.dict_get("solarSystemID"), Some(&PyRep::Integer(SYS_JITA as i64)));
    assert_eq!(sess.solar_system_id, Some(SYS_JITA));

    // Verify pocket is cleaned up from active registry on exit
    assert!(!with_pve_special_reg(&ctx, |reg| reg.abyssal_pockets.contains_key(&PILOT_ABYSS)));

    // 8. Deploy T3 Fierce Electrical filament (allowed in highsec, tier 3 difficulty mult 4.75 > 2.25)
    let deploy3_rep = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(t3_fil_item_id)], &kw, None)
        .await
        .expect("AbyssalEntranceDeployment T3");
    let trace3_id = deploy3_rep.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();

    let enter3_rep = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceGateActivation", &[PyRep::Integer(trace3_id)], &kw, None)
        .await
        .expect("AbyssalEntranceGateActivation T3");
    let t3_pid = enter3_rep.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(enter3_rep.dict_get("currentRoom"), Some(&PyRep::Integer(1)));
    assert_eq!(enter3_rep.dict_get("tier"), Some(&PyRep::Integer(3)));
    assert_eq!(enter3_rep.dict_get("timerSeconds"), Some(&PyRep::Integer(1200)));

    // Verify T3 pocket difficulty mult is 4.75 (1.0 + 3 * 1.25), higher than T1 (2.25)
    let t3_diff = with_pve_special_reg(&ctx, |reg| reg.abyssal_pockets.get(&PILOT_ABYSS).unwrap().npc_difficulty_mult);
    assert!((t3_diff - 4.75).abs() < 1e-3);
    assert!(t3_diff > t1_diff, "T3 difficulty mult ({t3_diff}) must exceed T1 difficulty mult ({t1_diff})");

    let _ = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "ClientIsReady", &[], &kw, None).await.expect("ClientIsReady T3");

    // Verify T3 Room 1 cache loot scales to 4 Survey Databases (1 + 3 = 4, > 2 from T1)
    crate::abyssal_room_combat::assert_room_cache_loot(&ctx, PILOT_ABYSS, 0, 4, fil_type_t3).await;
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_ABYSS, 0).await;

    // Advance through T3 rooms and exit
    let _ = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(1_895_000_000 + t3_pid * 10 + 1)], &kw, None).await.expect("T3 Gate R1->R2");
    crate::abyssal_room_combat::assert_room_cache_loot(&ctx, PILOT_ABYSS, 1, 4, fil_type_t3).await;
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_ABYSS, 1).await;

    let _ = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalGateActivation", &[PyRep::Integer(1_895_000_000 + t3_pid * 10 + 2)], &kw, None).await.expect("T3 Gate R2->R3");
    crate::abyssal_room_combat::assert_room_cache_loot(&ctx, PILOT_ABYSS, 2, 4, fil_type_t3).await;
    crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, PILOT_ABYSS, 2).await;

    let exit3_rep = ctx.dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEndGateActivation", &[PyRep::Integer(1_895_000_000 + t3_pid * 10 + 3)], &kw, None).await.expect("T3 AbyssalEndGateActivation");
    assert_eq!(exit3_rep.dict_get("solarSystemID"), Some(&PyRep::Integer(SYS_JITA as i64)));
    assert!(!with_pve_special_reg(&ctx, |reg| reg.abyssal_pockets.contains_key(&PILOT_ABYSS)));
}

// ============================================================================
// 3. Pirate Insurgencies & Pirate Strongholds
// ============================================================================
#[tokio::test]
// parity: feat:pve.insurgency, feat:pve.pirate-strongholds
async fn test_pve_insurgency_campaigns_and_strongholds_complete() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let ship_id = 91_000_503;
    let fob_sys = 30_002_048; // Bei
    let camp_sys = 30_003_076; // Gammel
    let camp_id = 801;

    let mut sess = setup_pilot_session(&ctx, PILOT_INSURGENT, camp_sys, ship_id, 621).await;

    // 1. Initialize insurgency campaign with FOB system
    init_insurgency_campaign(&ctx, camp_id, FACTION_GURISTAS, 500_001, fob_sys, vec![camp_sys]);

    // BF-PVE-STRONGHOLD-VULN-NOTIFY-01: Stronghold is invulnerable in Forecasting phase
    let stronghold_id = ensure_stronghold_in_space(&ctx, camp_id).await.expect("ensure stronghold in space");
    let fob_handle = ctx.systems.get_or_spawn(fob_sys);
    let early_destroy = eve_server::services::insurgency_services::on_insurgency_stronghold_destroyed_core(
        &ctx.systems, &ctx.db, fob_sys, stronghold_id, Some(PILOT_INSURGENT),
    ).await;
    assert!(early_destroy.is_none(), "Stronghold cannot be destroyed during Forecasting phase");

    // BF-PVE-STRONGHOLD-INVULN-02: Combat damage during Forecasting phase is zeroed and ball survives
    let sh_cs_before = fob_handle.get_combat_state(stronghold_id).await.unwrap().expect("CS before damage");
    let test_dmg = eve_destiny::DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 };
    let (dmg_app, dmg_dest, _) = fob_handle.apply_damage_with_attacker(stronghold_id, test_dmg, Some(PILOT_INSURGENT)).await.unwrap().expect("damage response");
    assert_eq!(dmg_app, 0.0, "Damage to stronghold during Forecasting must be 0");
    assert!(!dmg_dest, "Stronghold survives damage during Forecasting");
    let sh_cs_after = fob_handle.get_combat_state(stronghold_id).await.unwrap().expect("CS after damage");
    assert_eq!(sh_cs_after.structure_hp, sh_cs_before.structure_hp);

    // Transition campaign to Active phase
    let now = ctx.now_unix();
    with_insurgency_runtime(&ctx, |rt| {
        if let Some(c) = rt.campaigns.get_mut(&camp_id) {
            c.forecasting_end_unix = now.saturating_sub(10);
        }
    });

    let (_, sh_ball) = fob_handle.get_ball_snapshot(stronghold_id).await.unwrap().expect("Stronghold ball in space");
    assert_eq!(sh_ball.type_id, PIRATE_STRONGHOLD_GURISTAS_TYPE);
    let sh_cs = fob_handle.get_combat_state(stronghold_id).await.unwrap().expect("Stronghold has CombatState");
    assert!(sh_cs.max_structure_hp > 0.0);

    // Stronghold site listed in dungeonInstanceCacheMgr
    let instances_rep = ctx
        .dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetPirateInsurgencyInstances", &[PyRep::Integer(fob_sys as i64)], &kw, None)
        .await
        .expect("GetPirateInsurgencyInstances");
    let PyRep::List(inst_list) = instances_rep else { panic!("Expected List") };
    let stronghold_inst = inst_list
        .iter()
        .find(|i| i.dict_get("siteType").and_then(|v| v.as_str()) == Some("PirateStronghold"))
        .expect("PirateStronghold site spawned in FOB system");
    assert_eq!(stronghold_inst.dict_get("instanceID"), Some(&PyRep::Integer(stronghold_id)));
    assert_eq!(stronghold_inst.dict_get("completed"), Some(&PyRep::Bool(false)));

    // 3. insurgencySolarsystem.GetAllVisibleCampaigns returns campaign details
    let all_camps = ctx
        .dispatch_rpc(&mut sess, "insurgencySolarsystem", None, "GetAllVisibleCampaigns", &[], &kw, None)
        .await
        .expect("GetAllVisibleCampaigns");
    let PyRep::List(camp_list) = all_camps else { panic!("Expected List") };
    let camp_rep = camp_list.iter().find(|c| c.dict_get("campaignID") == Some(&PyRep::Integer(camp_id))).expect("campaign found");
    assert_eq!(camp_rep.dict_get("fobSolarSystemID"), Some(&PyRep::Integer(fob_sys as i64)));

    // 4. Initial snapshot shows 0 corruption and 0 suppression
    let snap_rep = ctx
        .dispatch_rpc(&mut sess, "insurgencySolarsystem", None, "GetLocalCampaignClientSnapshot", &[], &kw, None)
        .await
        .expect("GetLocalCampaignClientSnapshot initial");
    let PyRep::Tuple(snap_tuple) = snap_rep else { panic!("Expected Tuple") };
    assert_eq!(snap_tuple[0], PyRep::Integer(camp_sys as i64));
    assert_eq!(snap_tuple[1].dict_get("corruptionPoints"), Some(&PyRep::Integer(0)));
    assert_eq!(snap_tuple[1].dict_get("suppressionPoints"), Some(&PyRep::Integer(0)));

    // 5. Player activity: Setup FW complex beacon in space and position pilot near beacon
    let beacon_pos = [10_000.0, 0.0, 0.0];
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(camp_sys).or_default();
        st.fw_complex_position = Some(beacon_pos);
        st.last_fw_capture_tick = 0;
        st.fw_capture_duration_seconds = 600;
    });
    let camp_handle = ctx.systems.get_or_spawn(camp_sys);
    let _ = camp_handle.spawn_ball(Ball {
        id: ship_id,
        type_id: 621,
        position: DVec3::from_array(beacon_pos),
        velocity: DVec3::ZERO,
        max_velocity: 1000.0,
        inertia: 1.0,
        mass: 1000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;

    // Join Guristas militia via facWarMgr.JoinFactionAsCharacter
    ctx.dispatch_rpc(&mut sess, "facWarMgr", None, "JoinFactionAsCharacter", &[PyRep::Integer(500_010)], &kw, None)
        .await
        .expect("Join Guristas");

    // BF-PVE-INSURGENCY-CAPTURE-UNWIRED-01: Advance tick and trigger capture pulse via production system tick
    let _ = camp_handle.step_manual(1.0).await.unwrap();
    eve_server::services::insurgency_services::step_system_insurgency_tick(&ctx, camp_sys).await;

    // BF-PVE-INSURGENCY-GET-MUTATES-01: GetLocalCampaignClientSnapshot is strictly read-only
    let snap_c = ctx
        .dispatch_rpc(&mut sess, "insurgencySolarsystem", None, "GetLocalCampaignClientSnapshot", &[], &kw, None)
        .await
        .expect("GetLocalCampaignClientSnapshot after corruption pulse");
    let snap_c2 = ctx
        .dispatch_rpc(&mut sess, "insurgencySolarsystem", None, "GetLocalCampaignClientSnapshot", &[], &kw, None)
        .await
        .expect("GetLocalCampaignClientSnapshot second read");
    assert_eq!(snap_c, snap_c2, "GetLocalCampaignClientSnapshot must not mutate campaign state on read");
    let PyRep::Tuple(snap_c_tup) = snap_c else { panic!("Expected Tuple") };
    let c_pts = snap_c_tup[1].dict_get("corruptionPoints").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(c_pts > 0, "Guristas complex capture pulse near beacon awards corruption points");

    // 6. Player activity: Join Caldari militia and capture for suppression
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(camp_sys).or_default();
        st.last_fw_capture_tick = 0;
    });
    ctx.dispatch_rpc(&mut sess, "facWarMgr", None, "JoinFactionAsCharacter", &[PyRep::Integer(500_001)], &kw, None)
        .await
        .expect("Join Caldari");
    let _ = camp_handle.step_manual(1.0).await.unwrap();
    eve_server::services::insurgency_services::step_system_insurgency_tick(&ctx, camp_sys).await;

    let snap_s = ctx
        .dispatch_rpc(&mut sess, "insurgencySolarsystem", None, "GetLocalCampaignClientSnapshot", &[], &kw, None)
        .await
        .expect("GetLocalCampaignClientSnapshot after suppression pulse");
    let PyRep::Tuple(snap_s_tup) = snap_s else { panic!("Expected Tuple") };
    let s_pts = snap_s_tup[1].dict_get("suppressionPoints").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(s_pts > 0, "Caldari complex capture pulse near beacon awards suppression points");

    // 7. Pirate stronghold destruction via real combat damage
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_INSURGENT) {
            ch.solar_system_id = fob_sys;
        }
    });
    sess.solar_system_id = Some(fob_sys);

    // BF-PVE-STRONGHOLD-NOTIFY-TEST-03: Drain prior notifications and Quasar cache before destruction
    ctx.drain_notifications_for_char(PILOT_INSURGENT);
    {
        let mut q = ctx.quasar_gateway.routing_cache.write().unwrap();
        q.entries.clear();
        q.current_bytes = 0;
    }

    let dmg = eve_destiny::DamageVector { em: 1.0e7, thermal: 1.0e7, kinetic: 1.0e7, explosive: 1.0e7 };
    let res = fob_handle.apply_damage_with_attacker(stronghold_id, dmg, Some(PILOT_INSURGENT)).await;
    assert!(matches!(res, Ok(Some((_, true, _)))), "Stronghold structure ball must be destroyed by combat damage");
    assert!(fob_handle.get_ball_snapshot(stronghold_id).await.unwrap().is_none(), "Destroyed stronghold removed from ballpark");

    // Stronghold site is marked completed in dungeonInstanceCacheMgr
    let inst_after = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetPirateInsurgencyInstances", &[PyRep::Integer(fob_sys as i64)], &kw, None).await.expect("GetPirateInsurgencyInstances after destruction");
    let PyRep::List(inst_list_after) = inst_after else { panic!("Expected List") };
    let sh_after = inst_list_after.iter().find(|i| i.dict_get("instanceID") == Some(&PyRep::Integer(stronghold_id))).unwrap();
    assert_eq!(sh_after.dict_get("completed"), Some(&PyRep::Bool(true)));

    // Campaign transitioned to WindDown and FOB suppression increased
    let fob_snap = ctx.dispatch_rpc(&mut sess, "insurgencySolarsystem", None, "GetLocalCampaignClientSnapshot", &[], &kw, None).await.expect("GetLocalCampaignClientSnapshot for FOB system");
    let PyRep::Tuple(fob_tup) = fob_snap else { panic!("Expected Tuple") };
    assert_eq!(fob_tup[1].dict_get("phase"), Some(&PyRep::String(PHASE_WINDDOWN.into())));
    let fob_supp = fob_tup[1].dict_get("suppressionPoints").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(fob_supp >= 500, "Stronghold destruction awards massive suppression to FOB system (got {fob_supp})");

    // FOB wreck ball (type 79386) spawned in space
    let wreck_ball = fob_handle.ballpark.read().ok().and_then(|bp| bp.balls.values().find(|b| b.type_id == PIRATE_STRONGHOLD_WRECK_TYPE).cloned());
    assert!(wreck_ball.is_some(), "FOB wreck ball (type 79386) spawned in space upon stronghold destruction");

    // BF-PVE-STRONGHOLD-NOTIFY-TEST-03 / BF-PVE-INSURGENCY-NOTIFY-SHAPE-01: Stronghold destruction queued Machonet & Quasar notifications
    let notifs = ctx.drain_notifications_for_char(PILOT_INSURGENT);
    let updated_notif = notifs
        .iter()
        .find(|n| n.event == "OnInsurgencyCampaignUpdatedForLocation")
        .expect("Campaign updated notification emitted");
    assert_eq!(updated_notif.args.len(), 1, "Must carry 1 argument (campaignSnapshot)");
    assert_eq!(updated_notif.args[0].dict_get("campaignID"), Some(&PyRep::Integer(camp_id)));
    assert!(updated_notif.args[0].dict_get("coveredSolarsystemIDs").is_some());

    let q_entries = ctx.quasar_gateway.routing_cache.read().unwrap();
    assert!(q_entries.entries.iter().any(|e| {
        e.notice.payload.as_ref().map(|p| p.type_url.contains("eve_public.pirate.suppression.api.IncreasedNotice")).unwrap_or(false)
    }), "IncreasedNotice published to Quasar gateway");
}
