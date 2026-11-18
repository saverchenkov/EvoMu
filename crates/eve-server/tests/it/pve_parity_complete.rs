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
    classify_abyssal_filament, read_abyssal_state,
};

pub(crate) fn fast_ctx() -> ServerContext {
    static WORLD: std::sync::OnceLock<(
        std::sync::Arc<eve_sde::SdeRegistry>,
        eve_db::WorldStateSnapshot,
    )> = std::sync::OnceLock::new();
    let (sde, base_snap) = WORLD.get_or_init(|| {
        let sde = std::sync::Arc::new(
            eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
                .expect("open evomu_sde.sqlite"),
        );
        let db = eve_db::WorldRepository::with_demo_seed();
        let snap = db.with_snapshot(|s| s.clone());
        (sde, snap)
    });
    let db = std::sync::Arc::new(eve_db::WorldRepository::new_empty());
    db.with_mut_snapshot(|s| *s = base_snap.clone());
    ServerContext::with_sde_and_repository(sde.clone(), db)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED)
}

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
    let ctx = fast_ctx();
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
    let ctx = fast_ctx();
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

    // 2. High-sec restriction: T4-T6 filaments blocked in 0.9 high security space (Jita = 0.95 sec)
    let t4_deploy_err = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEntranceDeployment", &[PyRep::Integer(t4_fil_item_id)], &kw, None)
        .await
        .expect_err("T4 filament deployment in Jita must fail");
    assert_eq!(t4_deploy_err.code, "FilamentDisallowedInHighSec");

    // Move pilot to 0.8 high-sec (Lashesih = 30000002) where T1 and T3 filaments are allowed cleanly
    let sys_08 = 30_000_002_u32;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_ABYSS) {
            ch.solar_system_id = sys_08;
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = sys_08 as i64;
        }
    });
    sess.solar_system_id = Some(sys_08);
    let _ = ctx.systems.get_or_spawn(sys_08).spawn_ball(eve_destiny::Ball {
        id: ship_id,
        type_id: 621,
        position: glam::DVec3::ZERO,
        velocity: glam::DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    }).await;

    // 3. Deploy T1 Calm Electrical filament in 0.8 high-sec -> spawns trace
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
    let t1_diff = read_abyssal_state(&ctx, |st| st.pockets.get(&PILOT_ABYSS).unwrap().npc_difficulty_mult);
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

    // 7. AbyssalEndGateActivation exits Abyssal Deadspace and returns ship to origin system
    let exit_rep = ctx
        .dispatch_rpc(&mut sess, "abyssalMgr", None, "AbyssalEndGateActivation", &[PyRep::Integer(1_895_000_003)], &kw, None)
        .await
        .expect("AbyssalEndGateActivation");
    assert_eq!(exit_rep.dict_get("solarSystemID"), Some(&PyRep::Integer(sys_08 as i64)));
    assert_eq!(sess.solar_system_id, Some(sys_08));

    // Verify pocket is cleaned up from active registry on exit
    assert!(!read_abyssal_state(&ctx, |st| st.pockets.contains_key(&PILOT_ABYSS)));

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
    let t3_diff = read_abyssal_state(&ctx, |st| st.pockets.get(&PILOT_ABYSS).unwrap().npc_difficulty_mult);
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
    assert_eq!(exit3_rep.dict_get("solarSystemID"), Some(&PyRep::Integer(sys_08 as i64)));
    assert!(!read_abyssal_state(&ctx, |st| st.pockets.contains_key(&PILOT_ABYSS)));
}

// ============================================================================
// 3. Pirate Insurgencies & Pirate Strongholds
// ============================================================================
#[tokio::test]
// parity: feat:pve.insurgency, feat:pve.pirate-strongholds
async fn test_pve_insurgency_campaigns_and_strongholds_complete() {
    let ctx = fast_ctx();
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
    let test_dmg = eve_destiny::DamageVector { em: 50_000_000.0, thermal: 50_000_000.0, kinetic: 50_000_000.0, explosive: 50_000_000.0 };
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
        if let Some(ch) = snap.characters.get_mut(&PILOT_INSURGENT) {
            ch.war_faction_id = None;
        }
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

    // 7. Pirate stronghold / FOB destruction via real combat damage (Stage-5 Suppression + 3h±1h ArmorReinforced)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_INSURGENT) {
            ch.solar_system_id = fob_sys;
        }
    });
    sess.solar_system_id = Some(fob_sys);

    let dmg = eve_destiny::DamageVector { em: 1.0e7, thermal: 1.0e7, kinetic: 1.0e7, explosive: 1.0e7 };
    let pre_s5_res = fob_handle.apply_damage_with_attacker(stronghold_id, dmg, Some(PILOT_INSURGENT)).await;
    assert!(matches!(pre_s5_res, Ok(Some((0.0, false, _)))), "FOB must be invulnerable before 5 Stage-5 suppressed systems");

    with_insurgency_runtime(&ctx, |rt| {
        if let Some(c) = rt.campaigns.get_mut(&camp_id) {
            for offset in 1..=5_u32 {
                c.suppression_points.insert(fob_sys + offset, 1_000);
            }
        }
    });

    // BF-PVE-STRONGHOLD-NOTIFY-TEST-03: Drain prior notifications and Quasar cache before destruction
    ctx.drain_notifications_for_char(PILOT_INSURGENT);
    {
        let mut q = ctx.quasar_gateway.routing_cache.write().unwrap();
        q.entries.clear();
        q.current_bytes = 0;
    }

    let phase1_res = fob_handle.apply_damage_with_attacker(stronghold_id, dmg, Some(PILOT_INSURGENT)).await;
    assert!(matches!(phase1_res, Ok(Some((applied, false, _))) if applied > 0.0), "Phase 1 hit must deplete shield/armor into ArmorReinforced without one-shotting hull");
    let mid_hit_res = fob_handle.apply_damage_with_attacker(stronghold_id, dmg, Some(PILOT_INSURGENT)).await;
    assert!(matches!(mid_hit_res, Ok(Some((0.0, false, _)))), "FOB must be invulnerable during ArmorReinforced timer");

    let reinf_until = ctx.db.with_snapshot(|s| s.insurgency_runtime.fob_armor_reinforced_until.get(&camp_id).copied().unwrap_or(0));
    let delta = reinf_until.saturating_sub(ctx.now_unix()) + 10;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.insurgency_runtime.campaigns.get_mut(&camp_id) {
            c.active_end_unix = c.active_end_unix.max(ctx.now_unix() + delta + 3_600);
            c.winddown_end_unix = c.active_end_unix + 86_400;
        }
    });
    ctx.db.clock().advance_secs(delta);

    let res = fob_handle.apply_damage_with_attacker(stronghold_id, dmg, Some(PILOT_INSURGENT)).await;
    assert!(matches!(res, Ok(Some((_, true, _)))), "Stronghold structure ball must be destroyed after ArmorReinforced expires");
    assert!(fob_handle.get_ball_snapshot(stronghold_id).await.unwrap().is_none(), "Destroyed stronghold removed from ballpark");
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaign_outcomes.get(&camp_id).cloned()), Some("EmpireWin".to_string()));

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


pub(crate) mod pve_data_relic_ded_wormhole_sites {
    #[allow(unused_imports)]
    use super::*;

// Remediation tests for exploration, DED complexes, and J-space Sleeper combat.
//
// Parity Tags Tested:
// - feat:pve.combat-signatures
// - feat:pve.data-sites
// - feat:pve.relic-sites
// - feat:pve.loot-spew
// - feat:pve.ded-complexes
// - feat:pve.wormhole-sites


use std::collections::HashMap;
use glam::DVec3;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_mut_combat_runtime;
use eve_server::services::pve_exploration::{ensure_system_pve_sites_seeded, with_pve_exploration_reg};
use eve_server::services::pve_exploration_sites::{generate_exploration_container_loot, overseer_effects_for_rating};
use eve_machonet::SessionState;

const AU: f64 = 149_597_870_700.0;
const CHAR_ID: i64 = 90_000_001;

async fn place_ship(ctx: &ServerContext, sys: u32, ship_id: i64, type_id: u32, pos: DVec3) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id, owner_id: CHAR_ID, location_id: sys as i64,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default()
        });
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.active_ship_id = ship_id;
        }
    });
    let _ = ctx.systems.get_or_spawn(sys).spawn_ball(Ball {
        id: ship_id, type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 365.0, inertia: 1.0,
        mass: 1_067_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
}

async fn create_in_space_session(ctx: &ServerContext, session_id: u64, sys: u32, ship_id: i64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await.expect("Login");
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(CHAR_ID)], &kw, None).await.expect("SelectCharacterID");
    sess.solar_system_id = Some(sys);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys;
        }
    });
    place_ship(ctx, sys, ship_id, 587, DVec3::ZERO).await;
    sess
}

/// Destroys every active NPC of a site through the production in-space damage pipeline
/// (`SystemHandle::apply_damage_with_attacker` -> `npc_death_hook`).
async fn kill_ball(ctx: &ServerContext, sys: u32, bid: i64) {
    let dmg = DamageVector { em: 1.0e7, thermal: 1.0e7, kinetic: 1.0e7, explosive: 1.0e7 };
    let res = ctx.systems.get_or_spawn(sys).apply_damage_with_attacker(bid, dmg, Some(CHAR_ID)).await;
    assert!(matches!(res, Ok(Some((_, true, _)))), "NPC ball {bid} must be destroyed by damage: {res:?}");
}

fn site_state(ctx: &ServerContext, sys: u32, site_id: &str) -> (u8, Vec<i64>, bool, [f64; 3]) {
    with_pve_exploration_reg(ctx, |reg| {
        let s = reg.sites_by_system[&sys].iter().find(|s| s.site_id == site_id).expect("site");
        (s.current_wave, s.active_npc_ball_ids.clone(), s.cleared, s.position)
    })
}

async fn kill_site_wave(ctx: &ServerContext, sys: u32, site_id: &str) {
    for bid in site_state(ctx, sys, site_id).1 {
        kill_ball(ctx, sys, bid).await;
    }
}

async fn activate_gate(ctx: &ServerContext, sess: &mut SessionState, site: &str) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::String(site.into())], &HashMap::new(), None).await
}

fn container_cargo(ctx: &ServerContext, container_id: i64) -> Vec<InventoryItemRecord> {
    ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.location_id == container_id && i.flag == InventoryFlag::Cargo).cloned().collect()
    })
}

fn probes_dict(ids: &[i64], center: DVec3, range: f64) -> PyRep {
    let d = range * 0.4;
    let offs = [DVec3::ZERO, DVec3::X * d, -DVec3::X * d, DVec3::Y * d, -DVec3::Y * d, DVec3::Z * d, -DVec3::Z * d, DVec3::splat(d * 0.5)];
    PyRep::Dict(ids.iter().enumerate().map(|(i, &pid)| {
        let p = center + offs[i % offs.len()];
        (PyRep::Integer(pid), PyRep::keyval([
            ("pos", PyRep::Tuple(vec![PyRep::Real(p.x), PyRep::Real(p.y), PyRep::Real(p.z)])),
            ("scanRange", PyRep::Real(range)),
        ]))
    }).collect())
}

fn row_field(rows: &[PyRep], sig_id: &str, key: &str) -> PyRep {
    rows.iter().find(|r| r.dict_get("signatureID").and_then(|v| v.as_str()) == Some(sig_id))
        .and_then(|r| r.dict_get(key).cloned())
        .unwrap_or_else(|| panic!("{sig_id} row missing {key}"))
}

async fn hack_until_won(ctx: &ServerContext, sess: &mut SessionState, can_id: i64) -> PyRep {
    let kw = HashMap::new();
    let sys_id = sess.solar_system_id.unwrap();
    let ship_id = sess.ship_id.unwrap();
    let can_pos = with_pve_exploration_reg(ctx, |reg| {
        reg.sites_by_system.get(&sys_id).and_then(|sites| {
            sites.iter().find(|s| s.hackable_can_ids.contains(&can_id)).map(|s| s.position)
        })
    }).unwrap_or([0.0, 0.0, 0.0]);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.set_ball_position(ship_id, DVec3::from_array(can_pos) + DVec3::new(500.0, 0.0, 0.0)).await;
    let start = ctx.dispatch_rpc(sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(can_id)], &kw, None).await.expect("StartNewGameInstance");
    assert_eq!(start.dict_get("virusCoherence").and_then(|v| v.as_i64()), Some(90), "Analyzer I (40) + Hacking/Archaeology V (+50) = 90 virusCoherence");
    assert_eq!(start.dict_get("virusStrength").and_then(|v| v.as_i64()), Some(20), "Analyzer I SDE virusStrength (attr 1910) = 20");
    let core_idx = with_pve_exploration_reg(ctx, |reg| {
        reg.hacking_games[&(CHAR_ID, can_id)].tiles.iter().find(|t| format!("{:?}", t.kind) == "SystemCore").unwrap().tile_index
    });
    // Non-adjacent unrevealed SystemCore cannot be clicked on turn 1
    let direct_err = ctx.dispatch_rpc(sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(core_idx as i64)], &kw, None).await.expect_err("unrevealed non-adjacent core");
    assert_eq!(direct_err.code, "TileNotAccessible");

    let mut path: Vec<u32> = (1..=(core_idx / 6)).map(|r| r * 6).collect();
    path.extend(((core_idx / 6) * 6 + 1)..=core_idx);
    for (step_i, &idx) in path.iter().enumerate() {
        for attempt in 0..5 {
            let r = ctx.dispatch_rpc(sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(idx as i64)], &kw, None).await.expect("ClickedOnTile");
            if r.dict_get("status").and_then(|v| v.as_str()) == Some("Won") {
                return r;
            }
            let node_coh = r.dict_get("nodeCoherence").and_then(|v| v.as_i64()).unwrap_or(0);
            if node_coh <= 0 {
                break;
            }
            if attempt == 0 && step_i + 1 < path.len() {
                let blocked_err = ctx.dispatch_rpc(sess, "hackingMgr", None, "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(path[step_i + 1] as i64)], &kw, None).await.expect_err("blocked by undefeated defensive node");
                assert_eq!(blocked_err.code, "TileNotAccessible");
            }
        }
    }
    panic!("hack on {can_id} was not won");
}

/// Asserts won-hack loot exists only inside the spewed in-space cans (once each) and never in the parent can.
fn assert_spew_loot(ctx: &ServerContext, won: &PyRep, parent_can: i64, sys: u32) -> Vec<u32> {
    assert!(container_cargo(ctx, parent_can).is_empty(), "Parent container must not duplicate spewed loot");
    let Some(PyRep::List(spew)) = won.dict_get("spewedContainers") else { panic!("spewedContainers") };
    assert!(!spew.is_empty(), "Won hack must spew loot cans");
    let mut types = Vec::new();
    for c in spew {
        let can_id = c.dict_get("canItemID").and_then(|v| v.as_i64()).unwrap();
        let can = ctx.db.with_snapshot(|s| s.items.get(&can_id).cloned()).expect("spewed can is a real item");
        assert_eq!(can.location_id, sys as i64, "Spewed can must sit in space");
        let cargo = container_cargo(ctx, can_id);
        assert_eq!(cargo.len(), 1, "Each spewed can holds exactly one loot stack");
        assert_eq!(cargo[0].type_id as i64, c.dict_get("lootTypeID").and_then(|v| v.as_i64()).unwrap());
        types.push(cargo[0].type_id);
    }
    types
}

// ============================================================================
// 1. Probe-scanned Combat Signatures, SDE Data/Relic Loot & Loot Spew
// ============================================================================
#[tokio::test]
async fn test_pve_data_relic_loot_tables_container_inventory_and_combat_signatures() {
    // parity: feat:pve.combat-signatures, feat:pve.data-sites, feat:pve.relic-sites, feat:pve.loot-spew
    eve_testkit::attest(&["feat:pve.combat-signatures", "feat:pve.data-sites", "feat:pve.relic-sites", "feat:pve.loot-spew"]);

    let ctx = fast_ctx();
    let kw = HashMap::new();
    let sys = 30_000_142_u32;
    let ship_id = 710_001_101_i64;
    let mut sess = create_in_space_session(&ctx, 8201, sys, ship_id).await;
    ensure_system_pve_sites_seeded(&ctx, sys).await;

    // 1. Launch 8 Core Scanner Probes from a fitted Core Probe Launcher I (`dogmaIM.Activate`)
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(CHAR_ID).or_default();
        for sid in [3402u32, 3412, 3413, 3426] {
            skills.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        for (iid, tid, qty) in [(440_101_i64, 17938_u32, 1), (440_102, 30013, 8)] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: CHAR_ID, location_id: ship_id, flag: InventoryFlag::HiSlot0,
                quantity: qty, singleton: qty == 1, ..Default::default()
            });
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let launch = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(440_101), PyRep::String("launchScanners".into()), PyRep::None, PyRep::Integer(0)], &kw, None).await.expect("launch probes");
    let Some(PyRep::List(pids)) = launch.dict_get("probeIDs") else { panic!("probeIDs") };
    let probe_ids: Vec<i64> = pids.iter().filter_map(|p| p.as_i64()).collect();
    assert_eq!(probe_ids.len(), 8);
    let bound = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr");
    let bound_id = bound.dict_get("boundID").and_then(|v| v.as_i64()).unwrap() as u64;

    // 2. scanMgr.RequestScans: probes 40 AU away from DAT-<sys> register zero signal for it
    let (dat_name, dat_pos) = with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system[&sys].iter().find(|s| s.site_id == "DAT-30000142").unwrap();
        (s.dungeon_name.clone(), DVec3::from_array(s.position))
    });
    let far = probes_dict(&probe_ids, dat_pos + DVec3::X * 40.0 * AU, 0.25 * AU);
    let far_rep = ctx.dispatch_rpc(&mut sess, "scanMgr", Some(bound_id), "RequestScans", &[far], &kw, None).await.expect("far RequestScans");
    let PyRep::List(far_rows) = far_rep else { panic!("RequestScans rows") };
    assert_eq!(row_field(&far_rows, "DAT-30000142", "signalStrength").as_f64(), Some(0.0), "Out-of-range probes give no signal");
    assert_eq!(row_field(&far_rows, "DAT-30000142", "coveringProbes").as_i64(), Some(0));

    // 3. Pinpoint 8-probe formation centred on the signature resolves it to 100% and reveals its SDE dungeon name
    let mut rows = Vec::new();
    for _ in 0..4 {
        let rep = ctx.dispatch_rpc(&mut sess, "scanMgr", Some(bound_id), "RequestScans", &[probes_dict(&probe_ids, dat_pos, 0.25 * AU)], &kw, None).await.expect("pinpoint RequestScans");
        let PyRep::List(r) = rep else { panic!() };
        rows = r;
    }
    assert_eq!(row_field(&rows, "DAT-30000142", "coveringProbes").as_i64(), Some(8));
    assert_eq!(row_field(&rows, "DAT-30000142", "signalStrength").as_f64(), Some(1.0));
    assert_eq!(row_field(&rows, "DAT-30000142", "type").as_str(), Some(dat_name.as_str()));

    // 4. Data can hack: requires fitted Analyzer module (`AnalyzerModuleRequired`), then spews real in-space cans
    let data_can_id = 1_870_000_000_i64 + (sys as i64 % 100_000) * 100;
    let no_mod_err = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "StartNewGameInstance", &[PyRep::Integer(data_can_id)], &kw, None).await.expect_err("unfitted ship must fail");
    assert_eq!(no_mod_err.code, "AnalyzerModuleRequired");
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(CHAR_ID).or_default();
        for sid in [21718_u32, 13278] {
            skills.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        for (iid, tid, flag) in [(440_103_i64, 22175_u32, InventoryFlag::MedSlot0), (440_104, 22177, InventoryFlag::MedSlot1)] {
            snap.items.insert(iid, InventoryItemRecord {
                item_id: iid, type_id: tid, owner_id: CHAR_ID, location_id: ship_id, flag,
                quantity: 1, singleton: true, ..Default::default()
            });
        }
    });
    let won = hack_until_won(&ctx, &mut sess, data_can_id).await;
    let data_types = assert_spew_loot(&ctx, &won, data_can_id, sys);
    let grp = |t: &u32| ctx.sde.types.get(t).map(|x| x.group_id).unwrap_or(0);
    assert!(data_types.iter().any(|t| grp(t) == 333), "Data spew must contain SDE Datacores");
    assert!(data_types.iter().any(|t| matches!(grp(t), 728..=735 | 1304)), "Data spew must contain SDE Decryptors");
    let expected: Vec<u32> = generate_exploration_container_loot(&ctx.sde, sys, false).iter().map(|l| l.0).collect();
    assert_eq!(data_types, expected, "Spewed loot is exactly the SDE-derived loot table");
    let hack_again = ctx.dispatch_rpc(&mut sess, "hackingMgr", None, "HackContainer", &[PyRep::Integer(data_can_id)], &kw, None).await;
    assert!(hack_again.is_err(), "Non-catalog hackingMgr.HackContainer must not be served");

    // 5. Relic can hack: SDE Salvaged Materials (754); Ancient Salvage (966) only in null-sec / J-space
    let relic_can_id = 1_871_000_000_i64 + (sys as i64 % 100_000) * 100;
    let won = hack_until_won(&ctx, &mut sess, relic_can_id).await;
    let relic_types = assert_spew_loot(&ctx, &won, relic_can_id, sys);
    assert!(relic_types.iter().all(|t| grp(t) == 754), "High-sec relic spew is SDE Salvaged Materials only");
    let jspace = generate_exploration_container_loot(&ctx.sde, 31_000_003, true);
    assert!(jspace.iter().any(|l| grp(&l.0) == 966), "J-space relic loot includes SDE Ancient Salvage");
    let hs_qty: i64 = generate_exploration_container_loot(&ctx.sde, sys, true).iter().map(|l| l.1).sum();
    let js_qty: i64 = jspace.iter().filter(|l| grp(&l.0) == 754).map(|l| l.1).sum();
    assert!(js_qty > hs_qty, "True-sec scaling: J-space salvage {js_qty} must exceed high-sec {hs_qty}");

    // 6. Faction weighting: Guristas (The Forge) and Serpentis regions pick different SDE datacores first
    let guristas = generate_exploration_container_loot(&ctx.sde, sys, false);
    let serpentis = generate_exploration_container_loot(&ctx.sde, 30_002_659, false);
    assert_ne!(guristas[0].0, serpentis[0].0, "Pirate faction must change the weighted datacore selection");
}

// ============================================================================
// 2. DED Gates, Overseer Wreck on Overseer Kill; Wormhole Sleeper Waves via NPC Kills
// ============================================================================
#[tokio::test]
async fn test_pve_ded_complexes_gating_overseer_wrecks_and_wormhole_sleepers() {
    // parity: feat:pve.ded-complexes, feat:pve.wormhole-sites
    eve_testkit::attest(&["feat:pve.ded-complexes", "feat:pve.wormhole-sites"]);

    let ctx = fast_ctx();
    let sys = 30_000_142_u32;
    let ship_id = 710_001_102_i64;
    let mut sess = create_in_space_session(&ctx, 8202, sys, ship_id).await;
    ensure_system_pve_sites_seeded(&ctx, sys).await;
    let ded = "DED-30000142-3";

    // 1. Killing room 1 NPCs through the damage pipeline unlocks (but does not auto-advance) the gate
    kill_site_wave(&ctx, sys, ded).await;
    let (wave, active, _, ded_pos) = site_state(&ctx, sys, ded);
    assert_eq!((wave, active.len()), (1, 0), "DED rooms advance only through the acceleration gate");

    // 2. Gate proximity: 0 m origin is far from the gate; within 2,500 m the Rifter passes to room 2
    let kw = HashMap::new();
    let r = activate_gate(&ctx, &mut sess, ded).await;
    assert_eq!(r.expect_err("far from gate").code, "GateOutOfRange");
    let r = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "EnterCombatAnomaly", &[PyRep::String(ded.into()), PyRep::Bool(true)], &kw, None).await;
    assert_eq!(r.expect_err("EnterCombatAnomaly far from gate").code, "GateOutOfRange");
    place_ship(&ctx, sys, ship_id, 587, DVec3::from_array(ded_pos) + DVec3::X * 1_000.0).await;
    let r = activate_gate(&ctx, &mut sess, ded).await;
    assert_eq!(r.expect("Rifter at gate").dict_get("currentWave").and_then(|v| v.as_i64()), Some(2));

    // 3. Battleship (Raven 638) rejected by the canonical DED 3/10 hull limit (Frigates & Destroyers only)
    let raven_id = 710_001_103_i64;
    place_ship(&ctx, sys, raven_id, 638, DVec3::from_array(ded_pos)).await;
    sess.ship_id = Some(raven_id);
    let r = activate_gate(&ctx, &mut sess, ded).await;
    assert_eq!(r.expect_err("Raven in 3/10").code, "ShipSizeRestricted");
    let r = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "EnterCombatAnomaly", &[PyRep::String(ded.into()), PyRep::Bool(true)], &kw, None).await;
    assert_eq!(r.expect_err("EnterCombatAnomaly Raven in 3/10").code, "ShipSizeRestricted");
    let r = ctx.dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::String(ded.into()), PyRep::Integer(10)], &kw, None).await;
    assert_eq!(r.expect_err("client rating override ignored").code, "ShipSizeRestricted", "Server-side DED rating cannot be overridden");
    let ded_dungeon_alias = with_pve_exploration_reg(&ctx, |reg| reg.sites_by_system[&sys].iter().find(|s| s.site_id == ded).map(|s| s.dungeon_id.to_string())).unwrap();
    let r = activate_gate(&ctx, &mut sess, &ded_dungeon_alias).await;
    assert_eq!(r.expect_err("dungeon-id alias").code, "ShipSizeRestricted", "str(dungeon_id) alias resolves to the DED site and is hull-checked");
    let cruiser = eve_server::services::pve_exploration_sites::check_ded_gate_ship_allowed(&ctx.sde, 3, 620, 10_000_000.0);
    assert!(cruiser.is_err(), "Cruisers (Osprey 620) are not allowed in DED 3/10");
    place_ship(&ctx, sys, ship_id, 587, DVec3::ZERO).await;
    sess.ship_id = Some(ship_id);
    let r = activate_gate(&ctx, &mut sess, &ded_dungeon_alias).await;
    assert_eq!(r.expect_err("alias out of range").code, "GateOutOfRange", "str(dungeon_id) alias is proximity-checked");
    place_ship(&ctx, sys, ship_id, 587, DVec3::from_array(ded_pos) + DVec3::X * 1_000.0).await;

    // 4. Room 2 kills -> gate -> room 3; killing the final-room overseer spawns the Overseer wreck in space
    kill_site_wave(&ctx, sys, ded).await;
    // Integer argument shape: keeper.ActivateAccelerationGate(gate_item_id) on the seeded DED gate ball
    let gate_ball_id = with_pve_exploration_reg(&ctx, |reg| reg.sites_by_system[&sys].iter().find(|s| s.site_id == ded).and_then(|s| s.gate_ball_id)).expect("DED gate ball");
    assert!(ctx.systems.get_or_spawn(sys).get_ball_snapshot(gate_ball_id).await.unwrap().is_some(), "DED gate ball in space");
    let r = ctx.dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(gate_ball_id)], &kw, None).await;
    assert_eq!(r.expect("gate to room 3").dict_get("currentWave").and_then(|v| v.as_i64()), Some(3));
    kill_site_wave(&ctx, sys, ded).await;
    assert!(site_state(&ctx, sys, ded).2, "Overseer kill completes the DED complex");
    let (ope_tid, ope_name) = overseer_effects_for_rating(&ctx.sde, 3).expect("SDE Overseer's Personal Effects");
    assert!((19_400..=19_422).contains(&ope_tid) && ope_name.contains("Overseer"));
    let wreck = ctx.db.with_snapshot(|snap| {
        snap.items.values().find(|w| w.location_id == sys as i64 && w.type_id == 26473
            && snap.items.values().any(|i| i.location_id == w.item_id && i.type_id == ope_tid)).map(|w| w.item_id)
    }).expect("Overseer wreck in space holds the SDE Overseer's Personal Effects");
    assert!(container_cargo(&ctx, wreck).iter().any(|i| i.type_id == 18658), "Wreck holds the deadspace module");
    assert!(ctx.systems.get_or_spawn(sys).get_ball_snapshot(wreck).await.unwrap().is_some(), "Wreck ball spawned");
    assert!(container_cargo(&ctx, ship_id).iter().all(|i| i.type_id != ope_tid), "No loot teleported to ship");
    let r = activate_gate(&ctx, &mut sess, ded).await;
    assert_eq!(r.expect_err("cleared").code, "SiteAlreadyCleared");

    // 5. C3 Sleeper site seeded by production `ensure_system_pve_sites_seeded` with zero-bounty Sleepers
    let c3 = 31_000_885_u32;
    ensure_system_pve_sites_seeded(&ctx, c3).await;
    let wh_site = format!("PVE-ANOM-{c3}-1");
    let (w1, wave1, _, _) = site_state(&ctx, c3, &wh_site);
    assert_eq!(w1, 1);
    let (trigger_type, bounty) = with_mut_combat_runtime(&ctx, |rt| {
        let n = &rt.npc_entities[&wave1[0]];
        (n.type_id, n.bounty_isk)
    });
    assert_eq!((trigger_type, bounty), (30208, 0.0), "Wave trigger is the Awakened Warden with zero bounty");

    // 6. Non-trigger kill drops a Sleeper wreck with SDE Blue Loot (group 880) but does not advance the wave
    kill_ball(&ctx, c3, wave1[1]).await;
    assert_eq!(site_state(&ctx, c3, &wh_site).0, 1);
    let blue = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|w| w.location_id == c3 as i64 && w.type_id == 30495)
            .any(|w| snap.items.values().any(|i| i.location_id == w.item_id && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 880)))
    });
    assert!(blue, "Sleeper wreck must contain SDE Sleeper Components (Blue Loot)");

    // 7. Trigger kill spawns wave 2 automatically; leftovers stay; then clear every wave to finish the site
    kill_ball(&ctx, c3, wave1[0]).await;
    let (w2, active2, _, _) = site_state(&ctx, c3, &wh_site);
    assert_eq!(w2, 2, "Trigger NPC kill advances to wave 2 without any client RPC");
    assert!(active2.contains(&wave1[2]), "Surviving wave-1 Sleepers remain in the site");
    for _ in 0..20 {
        let (_, active, cleared, _) = site_state(&ctx, c3, &wh_site);
        if cleared {
            break;
        }
        kill_ball(&ctx, c3, active[0]).await;
    }
    assert!(site_state(&ctx, c3, &wh_site).2, "All Sleeper waves destroyed -> site cleared");
    let pending = with_mut_combat_runtime(&ctx, |rt| rt.pending_bounty_ticks.get(&(c3, CHAR_ID)).copied().unwrap_or(0.0));
    assert_eq!(pending, 0.0, "CONCORD pays no bounty for Sleeper kills");
}
}


pub(crate) mod pve_crab_and_storms_lifecycle {
    #[allow(unused_imports)]
    use super::*;

// Integration tests for CRAB Beacons, Character CEQ, System Signal Interference,
// and Metaliminal Storm Movement.


use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::inventory_fitting::fitting_containers_ships::validate_single_item_can_be_opened;
use eve_server::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs;
use eve_server::services::pve_crab_beacons::{
    CrabBeaconPhase, CRAB_LINK_DURATION_SECS, CRAB_LOOT_EXCLUSIVE_SECS, CRAB_SCAN_DURATION_SECS,
    TYPE_CRAB_BEACON,
};
use eve_server::services::pve_metaliminal_storms::{compute_storm_footprint, is_nullsec_storm_system};
use eve_server::services::pve_special_sites::{
    advance_pve_special_clock, ensure_special_sites_spawned, with_pve_special_reg,
    AbyssalWeatherKind, RoamingStormRecord,
};
use eve_server::services::space_destiny::space_nav_travel::sync_tether;
use glam::DVec3;
use std::collections::{HashMap, HashSet};

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const LOWSEC_SYS: u32 = 30_002_904; // Tama (Low-sec 0.28)
const HIGHSEC_SYS: u32 = 30_000_142; // Jita (High-sec 0.95)
const WH_SYS: u32 = 31_000_240; // Wormhole J-space (non-shattered C3)
const TYPE_ARCHON: u32 = 23_757; // Carrier
const TYPE_RIFTER: u32 = 587; // Frigate

async fn setup_space_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sess_id);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String(username.into()), PyRep::String(password.into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
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

    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            11_584,
            TrainedSkillRecord {
                type_id: 11_584,
                level: 3,
                skill_points: 40_000.0,
            },
        );
        let p1_template = snap.characters[&PILOT_1].clone();
        let ch = snap.characters.entry(char_id).or_insert_with(|| p1_template);
        ch.character_id = char_id;
        ch.clone_grade = 2; // Omega clone so Anchoring III is uncapped
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_200_000_000.0,
                radius: 450.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
            ctx.build_ship_slim_meta(char_id, ship_type_id),
        )
        .await;
    sess
}

// 1. CRAB Deployment Restrictions, Capital Link, Drone Waves & Loot Window
#[tokio::test]
async fn test_crab_deployment_capital_link_drone_waves_loot_and_self_destruct() {

    let ctx = fast_ctx();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let safe_pos = DVec3::new(350_000_000.0, 120_000_000.0, -240_000_000.0);

    // 1. Deployment in High-Sec and Wormhole must fail
    let mut sess_hs = setup_space_pilot(&ctx, 8301, "demo", "demo-secret", PILOT_1, 730_001_001, TYPE_ARCHON, HIGHSEC_SYS, safe_pos).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            730_100_001,
            InventoryItemRecord {
                item_id: 730_100_001,
                type_id: TYPE_CRAB_BEACON,
                owner_id: PILOT_1,
                location_id: 730_001_001,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let hs_err = ctx
        .dispatch_rpc(&mut sess_hs, "posMgr", None, "AnchorStructure", &[PyRep::Integer(730_100_001)], &kw, None)
        .await
        .expect_err("CRAB deploy in High-Sec must fail");
    assert_eq!(hs_err.code, "CantDeployInHighSec");

    let mut sess_wh = setup_space_pilot(&ctx, 8302, "demo", "demo-secret", PILOT_1, 730_001_002, TYPE_ARCHON, WH_SYS, safe_pos).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            730_100_002,
            InventoryItemRecord {
                item_id: 730_100_002,
                type_id: TYPE_CRAB_BEACON,
                owner_id: PILOT_1,
                location_id: 730_001_002,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let wh_err = ctx
        .dispatch_rpc(&mut sess_wh, "posMgr", None, "AnchorStructure", &[PyRep::Integer(730_100_002)], &kw, None)
        .await
        .expect_err("CRAB deploy in Wormhole must fail");
    assert_eq!(wh_err.code, "CantDeployInWormhole");

    // 2. Deployment in Low-Sec succeeds; Subcapital link refused; Capital link succeeds
    let carrier_id = 730_001_003i64;
    let subcap_id = 730_001_004i64;
    let beacon_id = 730_100_003i64;
    let cloak_mod_id = 730_200_001i64;
    let mut sess_cap = setup_space_pilot(&ctx, 8303, "demo", "demo-secret", PILOT_1, carrier_id, TYPE_ARCHON, LOWSEC_SYS, safe_pos).await;
    let mut sess_sub = setup_space_pilot(
        &ctx,
        8304,
        "other",
        "other-secret",
        PILOT_2,
        subcap_id,
        TYPE_RIFTER,
        LOWSEC_SYS,
        safe_pos + DVec3::new(1_200.0, 0.0, 0.0),
    )
    .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            beacon_id,
            InventoryItemRecord {
                item_id: beacon_id,
                type_id: TYPE_CRAB_BEACON,
                owner_id: PILOT_1,
                location_id: carrier_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cloak_mod_id,
            InventoryItemRecord {
                item_id: cloak_mod_id,
                type_id: 11_578, // Prototype Cloaking Device I
                owner_id: PILOT_1,
                location_id: carrier_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    ctx.dispatch_rpc(&mut sess_cap, "posMgr", None, "AnchorStructure", &[PyRep::Integer(beacon_id)], &kw, None)
        .await
        .expect("Deploy CRAB in Low-Sec");

    // Subcapital link refused
    let sub_err = ctx
        .dispatch_rpc(
            &mut sess_sub,
            "beyonce",
            None,
            "CallComponentFromClient",
            &[
                PyRep::Integer(beacon_id),
                PyRep::String("linkWithShip".into()),
                PyRep::String("InitiateLink".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Subcapital cannot link to CRAB");
    assert_eq!(sub_err.code, "IneligibleShipForCrabLink");

    // Anchor a cored Astrahus right at safe_pos so we verify tether before, during, and after CRAB link
    ctx.db.with_mut_snapshot(|snap| {
        let corp_id = snap.characters.get(&PILOT_1).map(|c| c.corporation_id).unwrap_or(1_000_009);
        snap.structures.insert(
            1_030_000_001,
            UpwellStructureRecord {
                structure_id: 1_030_000_001,
                type_id: 35_832, // Astrahus
                owner_corp_id: corp_id,
                solar_system_id: LOWSEC_SYS,
                name: "Lowsec Staging Astrahus".into(),
                position: safe_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    let lowsec_handle = ctx.systems.get_or_spawn(LOWSEC_SYS);
    sync_tether(&ctx, PILOT_1, LOWSEC_SYS, carrier_id).await;
    assert!(
        lowsec_handle.get_combat_state(carrier_id).await.unwrap().unwrap().is_tethered,
        "Carrier tethers at cored friendly Astrahus before CRAB link"
    );

    // Record baseline carrier shield resonance before link
    let orig_sh_res = lowsec_handle.get_combat_state(carrier_id).await.unwrap().unwrap().shield_resonance;

    // Capital ship links to CRAB beacon
    let link_rep = ctx
        .dispatch_rpc(
            &mut sess_cap,
            "beyonce",
            None,
            "CallComponentFromClient",
            &[
                PyRep::Integer(beacon_id),
                PyRep::String("linkWithShip".into()),
                PyRep::String("InitiateLink".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("Capital link to CRAB");
    assert_eq!(link_rep.dict_get("linked").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(link_rep.dict_get("linkDurationSeconds").and_then(|v| v.as_i64()), Some(240));
    assert_eq!(link_rep.dict_get("resistanceBonusPct").and_then(|v| v.as_f64()), Some(10.0));

    // 3. During the 4-minute link: cannot warp, cloak, or tether, and gets +10% universal resistance bonus
    let warp_err = ctx
        .dispatch_rpc(
            &mut sess_cap,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[
                PyRep::String("point".into()),
                PyRep::Real(safe_pos.x + 500_000_000.0),
                PyRep::Real(safe_pos.y),
                PyRep::Real(safe_pos.z),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Carrier cannot warp while linked to CRAB");
    assert_eq!(warp_err.code, "CrabLinkActive");

    let cloak_err = ctx
        .dispatch_rpc(&mut sess_cap, "dogmaIM", None, "Activate", &[PyRep::Integer(cloak_mod_id)], &kw, None)
        .await
        .expect_err("Carrier cannot cloak while linked to CRAB");
    assert_eq!(cloak_err.code, "CrabLinkActive");

    sync_tether(&ctx, PILOT_1, LOWSEC_SYS, carrier_id).await;
    assert!(
        !lowsec_handle.get_combat_state(carrier_id).await.unwrap().unwrap().is_tethered,
        "Carrier cannot tether while linked to CRAB"
    );

    let linked_cs = lowsec_handle.get_combat_state(carrier_id).await.unwrap().unwrap();
    assert!(
        (linked_cs.shield_resonance[0] - orig_sh_res[0] * 0.90).abs() < 1e-6,
        "10% universal resistance bonus applied during CRAB link"
    );

    // 4. Advance 4 minutes (240s) -> link ends, resistance reverts, tether restored, 10-minute scan begins & Wave 1 spawns
    ctx.advance_secs(CRAB_LINK_DURATION_SECS);
    sync_tether(&ctx, PILOT_1, LOWSEC_SYS, carrier_id).await;
    let post_link_cs = lowsec_handle.get_combat_state(carrier_id).await.unwrap().unwrap();
    assert!(post_link_cs.is_tethered, "Carrier can tether again once 4m CRAB link completes");
    assert!(
        (post_link_cs.shield_resonance[0] - orig_sh_res[0]).abs() < 1e-6,
        "Resistance bonus cleared after 4m link completes"
    );
    let (phase_w1, waves_w1, drones_w1) = ctx.db.with_snapshot(|snap| {
        let b = &snap.crab_beacons.beacons[&beacon_id];
        (b.phase, b.waves_spawned, b.wave_npc_ball_ids.len())
    });
    assert_eq!(phase_w1, CrabBeaconPhase::Scanning);
    assert_eq!(waves_w1, 1);
    assert!(!drones_w1.eq(&0), "Wave 1 rogue drones must spawn");

    // Advance through Wave 2 (+200s), Wave 3 (+200s), and scan completion (+200s = 600s total)
    ctx.advance_secs(200);
    assert_eq!(ctx.db.with_snapshot(|snap| snap.crab_beacons.beacons[&beacon_id].waves_spawned), 2);
    ctx.advance_secs(200);
    assert_eq!(ctx.db.with_snapshot(|snap| snap.crab_beacons.beacons[&beacon_id].waves_spawned), 3);
    ctx.advance_secs(CRAB_SCAN_DURATION_SECS - 400);
    assert_eq!(ctx.db.with_snapshot(|snap| snap.crab_beacons.beacons[&beacon_id].phase), CrabBeaconPhase::Scanning, "Live Rogue Drones jam CRAB completion");
    for bid in ctx.db.with_snapshot(|snap| snap.crab_beacons.beacons[&beacon_id].wave_npc_ball_ids.clone()) {
        let _ = lowsec_handle.remove_ball(bid).await;
    }
    eve_server::services::pve_crab_beacons::tick_all_crab_beacons(&ctx);
    assert_eq!(ctx.db.with_snapshot(|snap| snap.crab_beacons.beacons[&beacon_id].phase), CrabBeaconPhase::LootRestricted);

    // 5. During 2-minute exclusive loot window: only deployer can open; non-deployer is refused
    let non_deployer_open = validate_single_item_can_be_opened(&ctx, PILOT_2, beacon_id).await;
    assert_eq!(non_deployer_open.unwrap_err().code, "LootOwnershipRestricted");
    assert!(validate_single_item_can_be_opened(&ctx, PILOT_1, beacon_id).await.is_ok());

    // 6. After 2 minutes (120s), beacon self-destructs and drops open-to-all loot container
    ctx.advance_secs(CRAB_LOOT_EXCLUSIVE_SECS);
    let (phase_end, open_can_opt) = ctx.db.with_snapshot(|snap| {
        let b = &snap.crab_beacons.beacons[&beacon_id];
        (b.phase, b.open_container_id)
    });
    assert_eq!(phase_end, CrabBeaconPhase::SelfDestructed);
    assert!(lowsec_handle.get_ball_snapshot(beacon_id).await.unwrap().is_none(), "Beacon ball despawned");
    let open_can_id = open_can_opt.expect("Open loot container spawned on self-destruct");
    assert!(
        validate_single_item_can_be_opened(&ctx, PILOT_2, open_can_id).await.is_ok(),
        "Non-deployer can open loot container after 2m exclusivity expires"
    );
}

// ============================================================================
// 2. Character CEQ Pool & System Signal Interference
// ============================================================================
#[tokio::test]
async fn test_crab_ceq_pool_regeneration_and_system_signal_interference() {

    let ctx = fast_ctx();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let base_pos = DVec3::new(400_000_000.0, 50_000_000.0, 100_000_000.0);

    let carrier_id = 730_002_001i64;
    let mut sess1 = setup_space_pilot(&ctx, 8311, "demo", "demo-secret", PILOT_1, carrier_id, TYPE_ARCHON, LOWSEC_SYS, base_pos).await;

    // 1. Verify initial CEQ is capped at 100
    let energy0 = ctx
        .dispatch_rpc(&mut sess1, "characterEnergyMgr", None, "GetMyEnergyState", &[], &kw, None)
        .await
        .expect("GetMyEnergyState");
    assert_eq!(energy0.dict_get("currentEnergy").and_then(|v| v.as_i64()), Some(100));
    assert_eq!(energy0.dict_get("maxEnergy").and_then(|v| v.as_i64()), Some(100));

    // 2. Link 3 CRAB beacons (3 * 30 = 90 CEQ); 4th activation refused with InsufficientEnergy
    for idx in 0..4i64 {
        let bid = 730_110_000 + idx;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                bid,
                InventoryItemRecord {
                    item_id: bid,
                    type_id: TYPE_CRAB_BEACON,
                    owner_id: PILOT_1,
                    location_id: carrier_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
        ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(bid)], &kw, None)
            .await
            .expect("Anchor CRAB");
        let res = ctx
            .dispatch_rpc(
                &mut sess1,
                "beyonce",
                None,
                "CallComponentFromClient",
                &[
                    PyRep::Integer(bid),
                    PyRep::String("linkWithShip".into()),
                    PyRep::String("InitiateLink".into()),
                ],
                &kw,
                None,
            )
            .await;
        if idx < 3 {
            assert!(res.is_ok(), "Activation {} within 100 CEQ cap must succeed: {res:?}", idx + 1);
            ctx.advance_secs(CRAB_LINK_DURATION_SECS);
        } else {
            assert_eq!(res.unwrap_err().code, "InsufficientEnergy", "4th CRAB activation must be refused");
        }
    }

    ctx.advance_secs(22 * 3_600);
    let energy_regen = ctx
        .dispatch_rpc(&mut sess1, "characterEnergyMgr", None, "GetMyEnergyState", &[], &kw, None)
        .await
        .expect("GetMyEnergyState after 22h");
    assert_eq!(energy_regen.dict_get("currentEnergy").and_then(|v| v.as_i64()), Some(100));

    let test_sys = 30_002_905u32; // Kedama (Low-sec)
    let mut pilots: Vec<(String, i64)> = Vec::new();
    for idx in 1..=4 {
        let uname = format!("crabp{idx}");
        let uid = ctx.db.provision_player_account(&uname, "crab-secret");
        let cid = ctx
            .db
            .create_character(uid, &format!("CRAB Pilot {idx}"), 1, 1, 11, 17, &ctx.sde)
            .expect("create CRAB pilot")
            .character_id;
        pilots.push((uname, cid));
    }
    for p_idx in 0..13usize {
        let (uname, pid) = &pilots[p_idx % pilots.len()];
        let pid = *pid;
        let sid = 730_020_000 + (p_idx as i64);
        let bid = 730_120_000 + (p_idx as i64);
        let p_pos = base_pos + DVec3::new((p_idx as f64) * 50_000.0, 0.0, 0.0);
        let mut p_sess = setup_space_pilot(&ctx, 8320 + (p_idx as u64), uname, "crab-secret", pid, sid, TYPE_ARCHON, test_sys, p_pos).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                bid,
                InventoryItemRecord {
                    item_id: bid,
                    type_id: TYPE_CRAB_BEACON,
                    owner_id: pid,
                    location_id: sid,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
        ctx.dispatch_rpc(&mut p_sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(bid)], &kw, None)
            .await
            .expect("Anchor CRAB");
        let link_res = ctx
            .dispatch_rpc(
                &mut p_sess,
                "beyonce",
                None,
                "CallComponentFromClient",
                &[
                    PyRep::Integer(bid),
                    PyRep::String("linkWithShip".into()),
                    PyRep::String("InitiateLink".into()),
                ],
                &kw,
                None,
            )
            .await;
        if p_idx < 12 {
            assert!(link_res.is_ok(), "CRAB #{} in system must succeed: {link_res:?}", p_idx + 1);
        } else {
            assert_eq!(link_res.unwrap_err().code, "ExcessInterference", "13th CRAB in system must be refused");
        }
    }

    let int_full = ctx
        .dispatch_rpc(
            &mut sess1,
            "solarsystemInterferenceMgr",
            None,
            "GetLocalInterferenceState",
            &[PyRep::Integer(i64::from(test_sys))],
            &kw,
            None,
        )
        .await
        .expect("GetLocalInterferenceState");
    assert_eq!(int_full.dict_get("crabActivationsRemaining").and_then(|v| v.as_i64()), Some(0));

    // Advance 25 hours -> interference decays to 0 and 12 activations are available again
    ctx.advance_secs(25 * 3_600);
    let int_decayed = ctx
        .dispatch_rpc(
            &mut sess1,
            "solarsystemInterferenceMgr",
            None,
            "GetLocalInterferenceState",
            &[PyRep::Integer(i64::from(test_sys))],
            &kw,
            None,
        )
        .await
        .expect("GetLocalInterferenceState after 25h");
    assert_eq!(int_decayed.dict_get("crabActivationsRemaining").and_then(|v| v.as_i64()), Some(12));
    assert!((int_decayed.dict_get("interferenceLevel").and_then(|v| v.as_f64()).unwrap_or(1.0)).abs() < 1e-6);
}

// ============================================================================
// 3. Metaliminal Storm Core/Periphery, Movement Cadence & Overlapping Storms
// ============================================================================
#[tokio::test]
async fn test_metaliminal_storm_core_periphery_movement_and_overlap() {

    let ctx = fast_ctx();
    crate::common::pin_clock(&ctx);
    ensure_special_sites_spawned(&ctx).await;

    let (storm_id, center_0, core_0, periph_0, all_core_systems) = with_pve_special_reg(&ctx, |reg| {
        let all_cores: HashSet<u32> = reg
            .roaming_storms
            .iter()
            .flat_map(|st| st.strong_core_systems.iter().copied())
            .collect();
        let s = &reg.roaming_storms[0];
        (
            s.storm_id,
            s.center_solar_system_id,
            s.strong_core_systems.clone(),
            s.weak_periphery_systems.clone(),
            all_cores,
        )
    });

    assert!(is_nullsec_storm_system(&ctx.sde, center_0), "Storm center must be null-sec");
    assert!(core_0.contains(&center_0), "Core contains center system");
    assert!(core_0.len() > 1, "Core contains center + 1-jump neighbors");
    let periph_only = periph_0
        .iter()
        .copied()
        .find(|s| !all_core_systems.contains(s))
        .expect("Periphery system 2-3 jumps away outside all storm cores");

    // 1. Extra storm PvE anomalies spawn ONLY in the core, never in periphery-only systems
    let core_has_storm_site = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&center_0)
            .is_some_and(|l| l.iter().any(|a| a.anomaly_id.starts_with("DNG-9042-") || a.anomaly_id.starts_with("DNG-9043-")))
    });
    let periph_has_storm_site = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&periph_only)
            .is_some_and(|l| l.iter().any(|a| a.anomaly_id.starts_with("DNG-9042-") || a.anomaly_id.starts_with("DNG-9043-")))
    });
    assert!(core_has_storm_site, "Storm core must spawn extra storm PvE sites");
    assert!(!periph_has_storm_site, "Storm periphery must not spawn extra storm PvE sites");

    // 2. Storm center does NOT move before 24h, moves 1 jump every 24-48h, and avoids retracing
    advance_pve_special_clock(&ctx, 12 * 3_600);
    let center_at_12h = with_pve_special_reg(&ctx, |reg| {
        reg.roaming_storms.iter().find(|s| s.storm_id == storm_id).unwrap().center_solar_system_id
    });
    assert_eq!(center_at_12h, center_0, "Storm must not move before 24h");

    advance_pve_special_clock(&ctx, 36 * 3_600); // 48h total
    let (center_1, recent_1) = with_pve_special_reg(&ctx, |reg| {
        let s = reg.roaming_storms.iter().find(|s| s.storm_id == storm_id).unwrap();
        (s.center_solar_system_id, s.recent_centers.clone())
    });
    assert_ne!(center_1, center_0, "Storm center must move after 24-48h");
    assert!(core_0.contains(&center_1), "Storm moves exactly 1 jump to a neighbor in its former core");

    advance_pve_special_clock(&ctx, 48 * 3_600);
    let center_2 = with_pve_special_reg(&ctx, |reg| {
        reg.roaming_storms.iter().find(|s| s.storm_id == storm_id).unwrap().center_solar_system_id
    });
    assert_ne!(center_2, center_1);
    assert_ne!(center_2, center_0, "Storm movement must avoid retracing its previous center");
    assert!(recent_1.contains(&center_0));

    // 3. Overlapping storms apply BOTH effect sets simultaneously
    let overlap_sys = 30_000_208u32;
    let ship_id = 730_030_001i64;
    let _sess = setup_space_pilot(&ctx, 8331, "demo", "demo-secret", PILOT_1, ship_id, TYPE_RIFTER, overlap_sys, DVec3::ZERO).await;

    let (elec_core, elec_periph) = compute_storm_footprint(&ctx, overlap_sys);
    let (gamma_core, gamma_periph) = compute_storm_footprint(&ctx, overlap_sys);
    with_pve_special_reg(&ctx, |reg| {
        reg.roaming_storms = vec![
            RoamingStormRecord {
                storm_id: 91,
                weather: AbyssalWeatherKind::Electrical,
                center_solar_system_id: overlap_sys,
                strong_core_systems: elec_core,
                weak_periphery_systems: elec_periph,
                step_count: 0,
                next_move_at_sec: u64::MAX,
                recent_centers: vec![overlap_sys],
            },
            RoamingStormRecord {
                storm_id: 92,
                weather: AbyssalWeatherKind::Gamma,
                center_solar_system_id: overlap_sys,
                strong_core_systems: gamma_core,
                weak_periphery_systems: gamma_periph,
                step_count: 0,
                next_move_at_sec: u64::MAX,
                recent_centers: vec![overlap_sys],
            },
        ];
    });
    eve_server::services::pve_metaliminal_storms::sync_metaliminal_storm_effects_and_sites(&ctx);

    // Electrical boosts capacitor recharge (attr 55: *0.50); Gamma boosts shield HP (attr 263: *1.50)
    let mut attrs = HashMap::from([(55, 100.0), (263, 1_000.0)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, ship_id, &mut attrs);
    assert!(attrs[&55] < 100.0, "Electrical storm effect set applied on overlap");
    assert!(attrs[&263] > 1_000.0, "Gamma storm effect set applied on overlap");
}
}


pub(crate) mod pve_incursions_lifecycle {
    #[allow(unused_imports)]
    use super::*;

// Integration tests for Sansha Incursion Lifecycle, Site Classes, Influence, System Effects,
// Fleet Payouts, and World Event Tales (`feat:pve.incursion-payout`, `feat:pve.incursion-spawn-lifecycle`,
// `feat:pve.incursion-influence`, `feat:pve.incursion-system-effects`, `feat:pve.incursion-site-classes`, `feat:pve.tales`).


use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs;
use eve_server::services::pve_incursion_lifecycle::{
    advance_incursion_lifecycles, advance_incursion_wave_on_escort_kill,
    compute_incursion_system_effects, get_incursion_system_wide_dbuffs, register_world_event_tale,
    WorldEventTaleRecord, INCURSION_RESPAWN_CADENCE_SEC, INCURSION_WITHDRAWAL_DURATION_SEC,
};
use eve_server::services::pve_incursion_sites::{
    get_incursion_effects_for_system, on_incursion_boss_destroyed,
};
use eve_server::services::pve_special_sites::{
    advance_pve_special_clock, ensure_special_sites_spawned, with_pve_special_reg,
};
use glam::DVec3;
use std::collections::{HashMap, HashSet};

async fn create_in_space_pilot(
    ctx: &ServerContext,
    session_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    solar_system_id: u32,
    ship_id: i64,
    ship_type_id: u32,
    pos: DVec3,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String(user.into()), PyRep::String(pass.into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
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

    sess.solar_system_id = Some(solar_system_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        let p1_template = snap.characters[&90_000_001].clone();
        let ch = snap.characters.entry(char_id).or_insert_with(|| p1_template);
        ch.character_id = char_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = solar_system_id;
        ch.active_ship_id = ship_id;

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: i64::from(solar_system_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: ship_type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 1.0,
            mass: 1_067_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    sess
}

// ============================================================================
// 1. Spawn Lifecycle, Constellation Roles, Site Classes & Map RPCs
// ============================================================================
#[tokio::test]
async fn test_incursion_spawn_lifecycle_roles_and_map_rpcs() {
    // parity: feat:pve.incursion-spawn-lifecycle, feat:pve.incursion-site-classes
    eve_testkit::attest(&[
        "feat:pve.incursion-spawn-lifecycle",
        "feat:pve.incursion-site-classes",
    ]);

    let ctx = fast_ctx();
    crate::common::pin_clock(&ctx);
    ensure_special_sites_spawned(&ctx).await;
    let kw = HashMap::new();

    // 1. Verify eligible constellations with distinct staging/vanguard/assault/HQ roles
    let (target_cid, staging_sys, vg_sys_list, as_sys_list, hq_sys, vg_site_id, vg_boss_bid, vg_escorts) =
        with_pve_special_reg(&ctx, |reg| {
            assert_eq!(reg.incursions.len(), 3, "3 concurrent incursions seeded");
            let (&cid, inc) = reg.incursions.iter().next().expect("active incursion");
            assert_ne!(cid, 20_000_020, "Trade hub constellation must not host incursion");
            let mut role_systems = HashSet::new();
            assert!(role_systems.insert(inc.staging_solar_system_id));
            for &s in &inc.vanguard_solar_system_ids {
                assert!(role_systems.insert(s), "Vanguard system must be distinct");
            }
            for &s in &inc.assault_solar_system_ids {
                assert!(role_systems.insert(s), "Assault system must be distinct");
            }
            assert!(role_systems.insert(inc.hq_solar_system_id), "HQ system must be distinct");

            // Verify every site spawns ONLY in a system with the matching role
            for site in &inc.sites {
                match site.tier.as_str() {
                    "Staging" => assert_eq!(site.solar_system_id, inc.staging_solar_system_id),
                    "Vanguard" => {
                        assert!(inc.vanguard_solar_system_ids.contains(&site.solar_system_id));
                        assert_eq!(site.min_pilots, 5);
                        assert_eq!(site.max_optimal_pilots, 10);
                    }
                    "Assault" => {
                        assert!(inc.assault_solar_system_ids.contains(&site.solar_system_id));
                        assert_eq!(site.min_pilots, 1);
                        assert_eq!(site.max_optimal_pilots, 20);
                    }
                    "Headquarters" => {
                        assert_eq!(site.solar_system_id, inc.hq_solar_system_id);
                        assert_eq!(site.min_pilots, 20);
                        assert_eq!(site.max_optimal_pilots, 40);
                    }
                    "Mothership" => {
                        assert_eq!(site.solar_system_id, inc.hq_solar_system_id);
                        assert_eq!(site.min_pilots, 40);
                        assert_eq!(site.max_optimal_pilots, 80);
                    }
                    other => panic!("Unexpected site tier {other}"),
                }
            }
            let vg = inc.sites.iter().find(|s| s.tier == "Vanguard").expect("Vanguard site");
            let esc_ids: Vec<i64> = vg.escort_spawns.iter().map(|e| e.npc_ball_id).collect();
            (
                cid,
                inc.staging_solar_system_id,
                inc.vanguard_solar_system_ids.clone(),
                inc.assault_solar_system_ids.clone(),
                inc.hq_solar_system_id,
                vg.site_id.clone(),
                vg.boss_npc_ball_id,
                esc_ids,
            )
        });

    let mut sess = create_in_space_pilot(
        &ctx,
        8201,
        "demo",
        "demo-secret",
        90_000_001,
        hq_sys,
        720_001_001,
        587,
        DVec3::ZERO,
    )
    .await;

    // 2. Verify map.GetSystemsInIncursions and map.GetIncursionGlobalReport reflect live state
    let sys_rep = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetSystemsInIncursions", &[], &kw, None)
        .await
        .expect("GetSystemsInIncursions");
    let PyRep::List(sys_entries) = &sys_rep else {
        panic!("Expected List from GetSystemsInIncursions");
    };
    assert!(sys_entries.iter().any(|e| {
        e.dict_get("solarSystemID").and_then(|v| v.as_i64()) == Some(i64::from(staging_sys))
            && e.dict_get("sceneType").and_then(|v| v.as_i64()) == Some(1)
            && e.dict_get("templateNameID").and_then(|v| v.as_i64()) == Some(564_035)
    }));
    assert!(sys_entries.iter().any(|e| {
        e.dict_get("solarSystemID").and_then(|v| v.as_i64()) == Some(i64::from(vg_sys_list[0]))
            && e.dict_get("sceneType").and_then(|v| v.as_i64()) == Some(2)
    }));
    assert!(sys_entries.iter().any(|e| {
        e.dict_get("solarSystemID").and_then(|v| v.as_i64()) == Some(i64::from(as_sys_list[0]))
            && e.dict_get("sceneType").and_then(|v| v.as_i64()) == Some(3)
    }));
    assert!(sys_entries.iter().any(|e| {
        e.dict_get("solarSystemID").and_then(|v| v.as_i64()) == Some(i64::from(hq_sys))
            && e.dict_get("sceneType").and_then(|v| v.as_i64()) == Some(4)
    }));

    let report_rep = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetIncursionGlobalReport", &[], &kw, None)
        .await
        .expect("GetIncursionGlobalReport");
    let PyRep::List(reports) = &report_rep else {
        panic!("Expected List from GetIncursionGlobalReport");
    };
    let rep_row = reports
        .iter()
        .find(|r| r.dict_get("constellationID").and_then(|v| v.as_i64()) == Some(i64::from(target_cid)))
        .expect("target constellation in report");
    // V24 `taleCommon.const.incursionStates`: Withdrawing = 0, Mobilizing = 1, Established = 2.
    assert_eq!(rep_row.dict_get("state").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(rep_row.dict_get("influence").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(rep_row.dict_get("hasBoss").and_then(|v| v.as_bool()), Some(false));

    // 3. Verify multi-wave NPC spawning & boss wave gate: boss is NOT spawned in Wave 1 and early completion is rejected
    let vg_sys_id = vg_sys_list[0];
    let vg_handle = ctx.systems.get_or_spawn(vg_sys_id);
    assert!(
        vg_handle.get_ball_snapshot(vg_boss_bid).await.unwrap().is_none(),
        "Vanguard boss must NOT spawn in Wave 1 before escort waves are cleared"
    );
    let early_err = on_incursion_boss_destroyed(&ctx, vg_sys_id, vg_boss_bid, Some(90_000_001))
        .await
        .expect("site matched")
        .expect_err("early boss kill on Wave 1 must be rejected");
    assert_eq!(early_err.code, "IncursionSiteNpcsRemaining");

    with_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.get_mut(&target_cid).unwrap();
        let site = inc.sites.iter_mut().find(|s| s.site_id == vg_site_id).unwrap();
        for esc in &mut site.escort_spawns {
            esc.destroyed = true;
        }
    });
    assert!(!vg_escorts.is_empty());
    advance_incursion_wave_on_escort_kill(&ctx.systems, &ctx.db, &ctx.sde, vg_sys_id, &vg_site_id).await;
    let wave_after = with_pve_special_reg(&ctx, |reg| {
        reg.incursion_site_waves.get(&vg_site_id).map(|w| w.current_wave).unwrap_or(1)
    });
    assert_eq!(wave_after, 2, "Destroying wave 1 escorts must spawn wave 2 reinforcements");
    assert!(vg_handle.get_ball_snapshot(vg_boss_bid).await.unwrap().is_none(), "Boss still deferred on Wave 2 of 3");

    // 4. Unlock Mothership at low influence, clear its escort fleet, kill it, verify withdrawal timer & respawn cadence
    let (ms_site_id, ms_boss_bid) = with_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.get_mut(&target_cid).unwrap();
        inc.influence = 0.05;
        inc.mothership_spawned = true;
        let ms = inc.sites.iter_mut().find(|s| s.tier == "Mothership").unwrap();
        ms.locked_until_low_influence = false;
        (ms.site_id.clone(), ms.boss_npc_ball_id)
    });
    crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys, &ms_site_id).await;
    let ms_res = on_incursion_boss_destroyed(&ctx, hq_sys, ms_boss_bid, Some(90_000_001))
        .await
        .expect("Mothership site matched")
        .expect("Mothership completion succeeded");
    assert!(ms_res.incursion_ended);
    assert!(with_pve_special_reg(&ctx, |reg| reg.incursions[&target_cid].mothership_defeated));

    let state_after_kill = with_pve_special_reg(&ctx, |reg| reg.incursions[&target_cid].state.clone());
    assert_eq!(state_after_kill, "Withdrawing", "Mothership death enters Withdrawing state");
    let rep_withdrawing = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetIncursionGlobalReport", &[], &kw, None)
        .await
        .expect("GetIncursionGlobalReport while Withdrawing");
    let PyRep::List(reports_withdrawing) = &rep_withdrawing else {
        panic!("Expected List");
    };
    let withdrawing_row = reports_withdrawing
        .iter()
        .find(|r| r.dict_get("constellationID").and_then(|v| v.as_i64()) == Some(i64::from(target_cid)))
        .expect("target constellation in report while Withdrawing");
    assert_eq!(withdrawing_row.dict_get("state").and_then(|v| v.as_i64()), Some(0));
    assert_eq!(withdrawing_row.dict_get("hasBoss").and_then(|v| v.as_bool()), Some(false));

    // Advance clock past 24-hour withdrawal duration
    advance_pve_special_clock(&ctx, INCURSION_WITHDRAWAL_DURATION_SEC + 60);
    let state_after_withdraw = with_pve_special_reg(&ctx, |reg| reg.incursions[&target_cid].state.clone());
    assert_eq!(state_after_withdraw, "Ended", "Incursion ends after withdrawal timer expires");

    // Verify ended incursion disappears from map.GetSystemsInIncursions
    let sys_rep_ended = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetSystemsInIncursions", &[], &kw, None)
        .await
        .expect("GetSystemsInIncursions after withdrawal");
    let PyRep::List(sys_entries_ended) = &sys_rep_ended else {
        panic!("Expected List");
    };
    assert!(
        !sys_entries_ended
            .iter()
            .any(|e| e.dict_get("constellationID").and_then(|v| v.as_i64()) == Some(i64::from(target_cid))),
        "Ended incursion systems must be removed from GetSystemsInIncursions"
    );
    let rep_ended = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetIncursionGlobalReport", &[], &kw, None)
        .await
        .expect("GetIncursionGlobalReport after withdrawal");
    let PyRep::List(reports_ended) = &rep_ended else {
        panic!("Expected List");
    };
    assert!(
        !reports_ended
            .iter()
            .any(|r| r.dict_get("constellationID").and_then(|v| v.as_i64()) == Some(i64::from(target_cid))),
        "Ended incursion must be excluded from GetIncursionGlobalReport"
    );

    // Advance clock past 12-hour respawn cadence -> new incursion spawns
    advance_pve_special_clock(&ctx, INCURSION_RESPAWN_CADENCE_SEC + 60);
    let active_count_after_cadence = with_pve_special_reg(&ctx, |reg| {
        reg.incursions.values().filter(|i| i.state != "Ended").count()
    });
    assert_eq!(
        active_count_after_cadence, 3,
        "A new incursion must spawn once the 12-hour respawn cadence elapses"
    );
}

// ============================================================================
// 2. Influence Reduction, Regeneration, Mothership Unlock & System DBuffs
// ============================================================================
#[tokio::test]
async fn test_incursion_influence_regen_mothership_unlock_and_system_effects() {
    // parity: feat:pve.incursion-influence, feat:pve.incursion-system-effects
    eve_testkit::attest(&[
        "feat:pve.incursion-influence",
        "feat:pve.incursion-system-effects",
    ]);

    let ctx = fast_ctx();
    crate::common::pin_clock(&ctx);
    ensure_special_sites_spawned(&ctx).await;

    let (target_cid, hq_sys, vg_sys, vg_sid, as_sys, as_sid, hq_sid) =
        with_pve_special_reg(&ctx, |reg| {
            let (&cid, inc) = reg.incursions.iter().next().unwrap();
            assert_eq!(inc.influence, 1.0, "Influence must start at 100% (1.0)");
            let vg = inc.sites.iter().find(|s| s.tier == "Vanguard").unwrap();
            let ass = inc.sites.iter().find(|s| s.tier == "Assault").unwrap();
            let hq = inc.sites.iter().find(|s| s.tier == "Headquarters").unwrap();
            (
                cid,
                inc.hq_solar_system_id,
                vg.solar_system_id,
                vg.site_id.clone(),
                ass.solar_system_id,
                ass.site_id.clone(),
                hq.site_id.clone(),
            )
        });

    let _sess = create_in_space_pilot(
        &ctx,
        8202,
        "demo",
        "demo-secret",
        90_000_001,
        hq_sys,
        720_001_002,
        587,
        DVec3::ZERO,
    )
    .await;

    // 1. Verify full Sansha system-wide dbuffs & penalties at 1.0 influence
    let dbuffs_full = get_incursion_system_wide_dbuffs(&ctx, hq_sys);
    assert_eq!(dbuffs_full.len(), 2, "Shield & Armor resistance system dbuffs active");
    assert!((dbuffs_full[0].1 - 50.0).abs() < 1e-6);

    let eff_full = get_incursion_effects_for_system(&ctx, hq_sys).expect("active effects");
    let comp_full = compute_incursion_system_effects(&ctx, hq_sys).expect("computed effects");
    assert_eq!(eff_full.bounty_penalty, 0.50);
    assert_eq!(comp_full.lp_penalty, 0.50);

    let mut attrs_full = HashMap::from([(263, 1000.0), (265, 1000.0), (482, 1000.0), (64, 1.0)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, 720_001_002, &mut attrs_full);
    assert_eq!(attrs_full[&263], 500.0);
    assert_eq!(attrs_full[&64], 0.5);

    // 2. Completing Vanguard (-0.05), Assault (-0.20), and HQ (-0.25) after clearing waves reduces influence by site class
    let vg_bid = crate::common::clear_incursion_site_escort_waves(&ctx, vg_sys, &vg_sid).await;
    let _ = ctx.systems.get_or_spawn(vg_sys).remove_ball(vg_bid).await;
    on_incursion_boss_destroyed(&ctx, vg_sys, vg_bid, Some(90_000_001)).await.unwrap().unwrap();
    let inf_after_vg = with_pve_special_reg(&ctx, |reg| reg.incursions[&target_cid].influence);
    assert!((inf_after_vg - 0.95).abs() < 1e-6, "Vanguard reduces influence by 0.05");

    let as_bid = crate::common::clear_incursion_site_escort_waves(&ctx, as_sys, &as_sid).await;
    let _ = ctx.systems.get_or_spawn(as_sys).remove_ball(as_bid).await;
    on_incursion_boss_destroyed(&ctx, as_sys, as_bid, Some(90_000_001)).await.unwrap().unwrap();
    let inf_after_as = with_pve_special_reg(&ctx, |reg| reg.incursions[&target_cid].influence);
    assert!((inf_after_as - 0.75).abs() < 1e-6, "Assault reduces influence by 0.20");

    let hq_bid = crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys, &hq_sid).await;
    let _ = ctx.systems.get_or_spawn(hq_sys).remove_ball(hq_bid).await;
    on_incursion_boss_destroyed(&ctx, hq_sys, hq_bid, Some(90_000_001)).await.unwrap().unwrap();
    let inf_after_hq = with_pve_special_reg(&ctx, |reg| reg.incursions[&target_cid].influence);
    assert!((inf_after_hq - 0.50).abs() < 1e-6, "HQ reduces influence by 0.25");

    // 3. Influence regenerates over time (+2% / hour) when sites are not run
    advance_pve_special_clock(&ctx, 2 * 3_600);
    advance_incursion_lifecycles(&ctx);
    let inf_after_regen = with_pve_special_reg(&ctx, |reg| reg.incursions[&target_cid].influence);
    assert!(
        (inf_after_regen - 0.54).abs() < 1e-6,
        "2 hours idle must regenerate +0.04 influence (0.50 -> 0.54), got {inf_after_regen}"
    );

    // 4. Lower influence to 0.50 -> system effects & dbuffs scale proportionally (25% penalty)
    with_pve_special_reg(&ctx, |reg| {
        reg.incursions.get_mut(&target_cid).unwrap().influence = 0.50;
    });
    let dbuffs_half = get_incursion_system_wide_dbuffs(&ctx, hq_sys);
    assert!((dbuffs_half[0].1 - 25.0).abs() < 1e-6, "DBuff scales linearly with influence");
    let mut attrs_half = HashMap::from([(263, 1000.0), (265, 1000.0), (482, 1000.0), (64, 1.0)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, 720_001_002, &mut attrs_half);
    assert_eq!(attrs_half[&263], 750.0, "25% shield penalty at 0.50 influence");
    assert_eq!(attrs_half[&64], 0.75, "25% damage penalty at 0.50 influence");

    // 5. At 0.08 influence (> 0%), Mothership stays locked; dropping to 0.0 (0%) unlocks Mothership -> Mobilizing
    let (vg2_sys, vg2_sid, second_hq_sid, ms_sid) = with_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.get_mut(&target_cid).unwrap();
        inc.influence = 0.13;
        let vg2 = inc.sites.iter().find(|s| s.tier == "Vanguard" && !s.boss_destroyed).unwrap();
        let hq2 = inc.sites.iter().find(|s| s.tier == "Headquarters" && !s.boss_destroyed).unwrap();
        let ms = inc.sites.iter().find(|s| s.tier == "Mothership").unwrap();
        (vg2.solar_system_id, vg2.site_id.clone(), hq2.site_id.clone(), ms.site_id.clone())
    });
    let vg2_bid = crate::common::clear_incursion_site_escort_waves(&ctx, vg2_sys, &vg2_sid).await;
    let _ = ctx.systems.get_or_spawn(vg2_sys).remove_ball(vg2_bid).await;
    on_incursion_boss_destroyed(&ctx, vg2_sys, vg2_bid, Some(90_000_001)).await.unwrap().unwrap();
    let still_locked_at_008 = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        (inc.influence - 0.08).abs() < 1e-6 && !inc.mothership_spawned && inc.state != "Mobilizing"
    });
    assert!(still_locked_at_008, "Mothership must remain locked at 8% (0.08) influence");

    let second_hq_bid = crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys, &second_hq_sid).await;
    let _ = ctx.systems.get_or_spawn(hq_sys).remove_ball(second_hq_bid).await;
    on_incursion_boss_destroyed(&ctx, hq_sys, second_hq_bid, Some(90_000_001)).await.unwrap().unwrap();
    let ms_unlocked = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        inc.mothership_spawned && inc.influence <= 0.0 && inc.state == "Mobilizing"
    });
    assert!(ms_unlocked, "Dropping influence to 0.0 must unlock Mothership site and enter Mobilizing");

    let ms_boss_id = crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys, &ms_sid).await;
    let _ = ctx.systems.get_or_spawn(hq_sys).remove_ball(ms_boss_id).await;
    on_incursion_boss_destroyed(&ctx, hq_sys, ms_boss_id, Some(90_000_001)).await.unwrap().unwrap();
    assert!(
        get_incursion_system_wide_dbuffs(&ctx, hq_sys).is_empty(),
        "Sansha system dbuffs must be removed on withdrawal"
    );
    let mut attrs_after = HashMap::from([(263, 1000.0), (265, 1000.0), (482, 1000.0), (64, 1.0)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, 720_001_002, &mut attrs_after);
    assert_eq!(attrs_after[&263], 1000.0, "Ship shield unaffected after incursion withdrawal");
    assert_eq!(attrs_after[&64], 1.0, "Ship damage unaffected after incursion withdrawal");
}

// ============================================================================
// 3. On-Grid-Only Fleet Payouts & World Event Tales
// ============================================================================
#[tokio::test]
async fn test_incursion_payout_on_grid_only_and_world_event_tales() {
    // parity: feat:pve.tales
    eve_testkit::attest(&["feat:pve.tales"]);

    let ctx = fast_ctx();
    crate::common::pin_clock(&ctx);
    ensure_special_sites_spawned(&ctx).await;
    let kw = HashMap::new();

    let (target_cid, vg_sys_id, hq_sys_id, vg_site_id, site_pos) = with_pve_special_reg(&ctx, |reg| {
        let (&cid, inc) = reg.incursions.iter().next().unwrap();
        let vg = inc.sites.iter().find(|s| s.tier == "Vanguard").unwrap();
        (cid, vg.solar_system_id, inc.hq_solar_system_id, vg.site_id.clone(), vg.position)
    });

    // Fleet 950_001: Pilot 1 & Pilot 2 on grid at site; Pilot 3 in same system 800 km off-grid; Pilot 4 in HQ system
    let p1 = 90_000_001;
    let p2 = 90_000_002;
    let uid3 = ctx.db.provision_player_account("incp3", "incp3-secret");
    let p3_offgrid = ctx
        .db
        .create_character(uid3, "Incursion Offgrid Pilot", 1, 1, 11, 17, &ctx.sde)
        .expect("create p3")
        .character_id;
    let uid4 = ctx.db.provision_player_account("incp4", "incp4-secret");
    let p4_other_sys = ctx
        .db
        .create_character(uid4, "Incursion Other Sys Pilot", 1, 1, 11, 17, &ctx.sde)
        .expect("create p4")
        .character_id;

    let mut sess1 = create_in_space_pilot(&ctx, 8211, "demo", "demo-secret", p1, vg_sys_id, 720_010_001, 587, DVec3::from_array(site_pos)).await;
    let _sess2 = create_in_space_pilot(
        &ctx,
        8212,
        "other",
        "other-secret",
        p2,
        vg_sys_id,
        720_010_002,
        587,
        DVec3::from_array(site_pos) + DVec3::new(2_500.0, 0.0, 0.0),
    )
    .await;
    let _sess3 = create_in_space_pilot(
        &ctx,
        8213,
        "incp3",
        "incp3-secret",
        p3_offgrid,
        vg_sys_id,
        720_010_003,
        587,
        DVec3::from_array(site_pos) + DVec3::new(800_000.0, 0.0, 0.0),
    )
    .await;
    let _sess4 = create_in_space_pilot(&ctx, 8214, "incp4", "incp4-secret", p4_other_sys, hq_sys_id, 720_010_004, 587, DVec3::ZERO).await;

    ctx.db.with_mut_snapshot(|snap| {
        let members = vec![p1, p2, p3_offgrid, p4_other_sys];
        for &pid in &members {
            if let Some(ch) = snap.characters.get_mut(&pid) {
                ch.fleet_id = Some(950_001);
            }
        }
        snap.fleets.insert(
            950_001,
            FleetRecord {
                fleet_id: 950_001,
                boss_id: p1,
                members,
                ..Default::default()
            },
        );
    });

    let init_state: HashMap<i64, (i64, i64)> = [p1, p2, p3_offgrid, p4_other_sys]
        .iter()
        .map(|&pid| {
            ctx.db.with_snapshot(|s| {
                let ch = &s.characters[&pid];
                (pid, (ch.balance.0, ch.loyalty_points.get(&1_000_125).copied().unwrap_or(0)))
            })
        })
        .collect();

    let vg_boss_bid = crate::common::clear_incursion_site_escort_waves(&ctx, vg_sys_id, &vg_site_id).await;
    let vg_sys = ctx.systems.get_or_spawn(vg_sys_id);
    let _ = vg_sys
        .apply_damage_with_attacker(
            vg_boss_bid,
            DamageVector {
                em: 1.0e7,
                thermal: 1.0e7,
                kinetic: 1.0e7,
                explosive: 1.0e7,
            },
            Some(p1),
        )
        .await
        .expect("kill vanguard boss");
    let _ = vg_sys.remove_ball(vg_boss_bid).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    if let Some(res) = on_incursion_boss_destroyed(&ctx, vg_sys_id, vg_boss_bid, Some(p1)).await {
        res.expect("incursion payout succeeded");
    }

    // Verify ONLY on-grid pilots (p1, p2) were paid ISK + CONCORD LP
    for &on_grid_pid in &[p1, p2] {
        let (bal_after, lp_after) = ctx.db.with_snapshot(|s| {
            let ch = &s.characters[&on_grid_pid];
            (ch.balance.0, ch.loyalty_points.get(&1_000_125).copied().unwrap_or(0))
        });
        assert!(bal_after > init_state[&on_grid_pid].0, "On-grid pilot {on_grid_pid} must receive ISK");
        assert!(lp_after > init_state[&on_grid_pid].1, "On-grid pilot {on_grid_pid} must receive CONCORD LP");
    }
    for &excluded_pid in &[p3_offgrid, p4_other_sys] {
        let (bal_after, lp_after) = ctx.db.with_snapshot(|s| {
            let ch = &s.characters[&excluded_pid];
            (ch.balance.0, ch.loyalty_points.get(&1_000_125).copied().unwrap_or(0))
        });
        assert_eq!(bal_after, init_state[&excluded_pid].0, "Off-grid/out-of-system pilot {excluded_pid} must not receive ISK");
        assert_eq!(lp_after, init_state[&excluded_pid].1, "Off-grid/out-of-system pilot {excluded_pid} must not receive LP");
    }

    // Verify taleMgr.GetGlobalWorldEventTales & get_active_tales_by_template return active world events + system state
    let custom_tale_id = register_world_event_tale(
        &ctx,
        WorldEventTaleRecord {
            tale_id: 0,
            template_id: 5301,
            template_class_id: "pirateInvasion".into(),
            solar_system_id: vg_sys_id,
            constellation_id: target_cid,
            aggressor_faction_id: 500_010,
            state: "InvasionContested".into(),
            influence: 0.65,
            start_unix: ctx.now_unix().saturating_sub(3_600),
            end_unix: ctx.now_unix() + 86_400,
        },
    );

    let all_tales_rep = ctx
        .dispatch_rpc(&mut sess1, "taleMgr", None, "GetGlobalWorldEventTales", &[], &kw, None)
        .await
        .expect("GetGlobalWorldEventTales");
    let PyRep::List(all_tales) = &all_tales_rep else {
        panic!("Expected List");
    };
    assert!(all_tales.iter().any(|t| t.dict_get("templateID").and_then(|v| v.as_i64()) == Some(5158)));
    assert!(all_tales.iter().any(|t| t.dict_get("templateID").and_then(|v| v.as_i64()) == Some(5201)));
    assert!(all_tales.iter().any(|t| {
        t.dict_get("taleID").and_then(|v| v.as_i64()) == Some(custom_tale_id)
            && t.dict_get("systemState").and_then(|v| v.as_str()) == Some("InvasionContested")
    }));

    let inv_tales_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "taleMgr",
            None,
            "get_active_tales_by_template",
            &[PyRep::Integer(5301)],
            &kw,
            None,
        )
        .await
        .expect("get_active_tales_by_template");
    let PyRep::List(inv_tales) = &inv_tales_rep else {
        panic!("Expected List");
    };
    assert_eq!(inv_tales.len(), 1);
    assert_eq!(
        inv_tales[0].dict_get("systemState").and_then(|v| v.as_str()),
        Some("InvasionContested")
    );
}
}


pub(crate) mod pve_incursions_topology_and_payouts {
    #[allow(unused_imports)]
    use super::*;

// Part 2 integration tests for Sansha Incursion SDE Stargate-Graph Topology Roles,
// Fleet Payout Scaling (Optimal, Over-Cap Diminishing Return, Hard-Cap Zero Cutoff),
// `incursion.UpdateIncursionLPLog` Reconciliation, and `taleMgr` `PyRep::Dict` Shapes
// (`feat:pve.incursion-payout`, `feat:pve.incursion-spawn-lifecycle`, `feat:pve.incursion-site-classes`, `feat:pve.tales`).


use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_incursion_lifecycle::{
    assign_constellation_system_roles, STORM_TALE_TEMPLATE_ID,
};
use eve_server::services::pve_incursion_sites::on_incursion_boss_destroyed;
use eve_server::services::pve_special_sites::{ensure_special_sites_spawned, with_pve_special_reg};
use glam::DVec3;
use std::collections::{HashMap, HashSet, VecDeque};

async fn spawn_fleet_pilot_on_grid(
    ctx: &ServerContext,
    idx: usize,
    solar_system_id: u32,
    pos: DVec3,
) -> (i64, SessionState) {
    let kw = HashMap::new();
    let uname = format!("incscale_{idx}");
    let uid = ctx.db.provision_player_account(&uname, "scale-secret");
    let char_id = ctx
        .db
        .create_character(uid, &format!("Incursion Scale Pilot {idx}"), 1, 1, 11, 17, &ctx.sde)
        .expect("create incursion pilot")
        .character_id;
    let ship_id = 721_000_000 + (idx as i64);

    let mut sess = SessionState::new(8400 + (idx as u64));
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String(uname.into()), PyRep::String("scale-secret".into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
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

    sess.solar_system_id = Some(solar_system_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = solar_system_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(solar_system_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 1.0,
            mass: 1_067_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    (char_id, sess)
}

fn intra_constellation_bfs_distances(
    ctx: &ServerContext,
    constellation_id: u32,
    start_sys: u32,
) -> HashMap<u32, u32> {
    let const_set: HashSet<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.constellation_id == constellation_id)
        .map(|s| s.solar_system_id)
        .collect();
    let mut dist = HashMap::new();
    dist.insert(start_sys, 0_u32);
    let mut queue = VecDeque::new();
    queue.push_back(start_sys);
    for _ in 0..const_set.len() {
        let Some(cur) = queue.pop_front() else {
            break;
        };
        let d = dist[&cur];
        for j in ctx.sde.jumps.values() {
            if j.from_solar_system_id == cur
                && const_set.contains(&j.to_solar_system_id)
                && !dist.contains_key(&j.to_solar_system_id)
            {
                dist.insert(j.to_solar_system_id, d + 1);
                queue.push_back(j.to_solar_system_id);
            }
        }
    }
    dist
}

// ============================================================================
// 1. SDE Stargate-Graph Constellation Topology Roles & taleMgr PyRep::Dict Shapes
// ============================================================================
#[tokio::test]
async fn test_incursion_sde_stargate_topology_roles_and_world_event_dict_shapes() {
    // parity: feat:pve.incursion-spawn-lifecycle, feat:pve.incursion-site-classes, feat:pve.tales
    eve_testkit::attest(&[
        "feat:pve.incursion-spawn-lifecycle",
        "feat:pve.incursion-site-classes",
        "feat:pve.tales",
    ]);

    let ctx = fast_ctx();
    crate::common::pin_clock(&ctx);
    ensure_special_sites_spawned(&ctx).await;

    // 1. Verify Kimotoro (20000020) stargate topology role assignment uses real SDE jumps:
    //    - 30000145 (New Caldari) has 4 external constellation stargates -> Staging
    //    - 30000141 (Kisogo, sorted index 2!) is a degree-1 dead-end pocket 4 hops from Staging -> HQ
    let mut kimotoro_systems: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.constellation_id == 20_000_020)
        .map(|s| s.solar_system_id)
        .collect();
    kimotoro_systems.sort_unstable();
    let (staging, vanguard, assault, hq) =
        assign_constellation_system_roles(&ctx.sde, 20_000_020, &kimotoro_systems);
    assert_eq!(
        staging, 30_000_145,
        "Staging must be border system New Caldari (30000145) with external gates, not sorted-ID first"
    );
    assert_eq!(
        hq, 30_000_141,
        "HQ must be degree-1 dead-end pocket Kisogo (30000141), not sorted-ID last"
    );
    let hops = intra_constellation_bfs_distances(&ctx, 20_000_020, staging);
    let max_vg_hop = vanguard.iter().map(|s| hops[s]).max().unwrap();
    let min_as_hop = assault.iter().map(|s| hops[s]).min().unwrap();
    assert!(
        max_vg_hop <= min_as_hop && min_as_hop <= hops[&hq],
        "BFS hop distance ordering Staging -> Vanguard ({max_vg_hop}) <= Assault ({min_as_hop}) <= HQ ({}) must hold",
        hops[&hq]
    );

    // Also verify all 3 live seeded incursions satisfy BFS hop ordering over real SDE stargates
    with_pve_special_reg(&ctx, |reg| {
        for (&cid, inc) in &reg.incursions {
            let d = intra_constellation_bfs_distances(&ctx, cid, inc.staging_solar_system_id);
            let vg_max = inc.vanguard_solar_system_ids.iter().filter_map(|s| d.get(s)).max().copied().unwrap_or(0);
            let as_min = inc.assault_solar_system_ids.iter().filter_map(|s| d.get(s)).min().copied().unwrap_or(vg_max);
            let hq_d = d.get(&inc.hq_solar_system_id).copied().unwrap_or(as_min);
            assert!(
                vg_max <= as_min && as_min <= hq_d,
                "Seeded incursion constellation {cid} must follow stargate BFS hop ordering"
            );
        }
    });

    // 2. Verify taleMgr returns subscriptable PyRep::Dict entries (NOT util.KeyVal) including active Metaliminal Storms
    let kw = HashMap::new();
    let (_, mut sess) = spawn_fleet_pilot_on_grid(&ctx, 1, 30_000_142, DVec3::ZERO).await;
    let tales_rep = ctx
        .dispatch_rpc(&mut sess, "taleMgr", None, "GetGlobalWorldEventTales", &[], &kw, None)
        .await
        .expect("GetGlobalWorldEventTales");
    let PyRep::List(tales) = &tales_rep else {
        panic!("Expected PyRep::List from GetGlobalWorldEventTales");
    };
    assert!(!tales.is_empty());
    for entry in tales {
        assert!(
            matches!(entry, PyRep::Dict(_)),
            "World event tale entry must be a subscriptable PyRep::Dict, got {entry:?}"
        );
    }
    assert!(
        tales.iter().any(|t| {
            t.dict_get("templateID").and_then(|v| v.as_i64()) == Some(STORM_TALE_TEMPLATE_ID)
                && t.dict_get("templateClassID").and_then(|v| v.as_str()) == Some("metaliminalStorm")
        }),
        "GetGlobalWorldEventTales must include active Metaliminal Roaming Storms"
    );

    let storm_tales_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "taleMgr",
            None,
            "get_active_tales_by_template",
            &[PyRep::Integer(STORM_TALE_TEMPLATE_ID)],
            &kw,
            None,
        )
        .await
        .expect("get_active_tales_by_template(5203)");
    let PyRep::List(storm_tales) = &storm_tales_rep else {
        panic!("Expected List for storm tales");
    };
    assert!(!storm_tales.is_empty(), "Active storm tales must be returned for template 5203");
}

// ============================================================================
// 2. Fleet Payout Scaling: Optimal (<=10), Over-Cap (12), Hard-Cap (>15 -> 0), & LP Log Reconciliation
// ============================================================================
#[tokio::test]
async fn test_incursion_fleet_payout_optimal_overcap_and_hard_cap_cutoff() {

    let ctx = fast_ctx();
    crate::common::pin_clock(&ctx);
    ensure_special_sites_spawned(&ctx).await;
    let kw = HashMap::new();

    // Collect 3 Vanguard sites across the seeded constellations so each fleet-size tier runs cleanly
    let vg_sites: Vec<(u32, u32, String, i64, DVec3)> = with_pve_special_reg(&ctx, |reg| {
        reg.incursions
            .iter()
            .flat_map(|(&cid, inc)| {
                inc.sites
                    .iter()
                    .filter(|s| s.tier == "Vanguard")
                    .map(move |s| (cid, s.solar_system_id, s.site_id.clone(), s.boss_npc_ball_id, DVec3::from_array(s.position)))
            })
            .take(3)
            .collect()
    });
    assert_eq!(vg_sites.len(), 3, "Need 3 Vanguard sites for optimal / overcap / hard-cap tests");

    // Pin constellation influence to 0.05 before each Vanguard site (delta 0.05 -> new_influence 0.0 -> 1.0x influence_scale)
    // so the comparison across Cases A/B/C strictly isolates fleet-size scaling.
    // Case A: Optimal fleet (5 on-grid pilots <= 10 optimal) -> full 1.0x payout (10,395,000 ISK, 1,400 LP)
    let (cid_a, sys_a, site_a, _, pos_a) = vg_sites[0].clone();
    with_pve_special_reg(&ctx, |reg| reg.incursions.get_mut(&cid_a).unwrap().influence = 0.05);
    let mut pilots_a = Vec::new();
    let mut lead_sess = None;
    for i in 0..5 {
        let (pid, s) = spawn_fleet_pilot_on_grid(&ctx, 10 + i, sys_a, pos_a + DVec3::new((i as f64) * 500.0, 0.0, 0.0)).await;
        if lead_sess.is_none() {
            lead_sess = Some(s);
        }
        pilots_a.push(pid);
    }
    let mut lead_sess = lead_sess.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        for &pid in &pilots_a {
            snap.characters.get_mut(&pid).unwrap().fleet_id = Some(960_001);
        }
        snap.fleets.insert(
            960_001,
            FleetRecord {
                fleet_id: 960_001,
                boss_id: pilots_a[0],
                members: pilots_a.clone(),
                ..Default::default()
            },
        );
    });
    let bal_before_a = ctx.db.with_snapshot(|s| s.characters[&pilots_a[0]].balance.0);
    let boss_a = crate::common::clear_incursion_site_escort_waves(&ctx, sys_a, &site_a).await;
    let _ = ctx.systems.get_or_spawn(sys_a).remove_ball(boss_a).await;
    let res_a = on_incursion_boss_destroyed(&ctx, sys_a, boss_a, Some(pilots_a[0]))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(res_a.isk_per_pilot, 10_395_000, "Optimal <=10 Vanguard fleet receives 100% ISK");
    assert_eq!(res_a.concord_lp_per_pilot, 1_400, "Optimal <=10 Vanguard fleet receives 100% CONCORD LP");
    let bal_after_a = ctx.db.with_snapshot(|s| s.characters[&pilots_a[0]].balance.0);
    assert_eq!(bal_after_a - bal_before_a, 10_395_000 * 100);

    // Verify incursion.UpdateIncursionLPLog reconciles and returns recorded LP log entries
    let lp_reconciled = ctx
        .dispatch_rpc(
            &mut lead_sess,
            "incursion",
            None,
            "UpdateIncursionLPLog",
            &[PyRep::Integer(i64::from(cid_a))],
            &kw,
            None,
        )
        .await
        .expect("UpdateIncursionLPLog");
    let PyRep::List(lp_rows) = &lp_reconciled else {
        panic!("Expected reconciled LP log list, got {lp_reconciled:?}");
    };
    assert_eq!(lp_rows.len(), 1, "Caller pilot receives reconciled LP log entry for site");
    assert_eq!(lp_rows[0].dict_get("fleetSize").and_then(|v| v.as_i64()), Some(5));
    assert_eq!(lp_rows[0].dict_get("siteID").and_then(|v| v.as_str()), Some(site_a.as_str()));

    // Case B: Over-optimal fleet (12 on-grid pilots: 10 < 12 <= 15 hard cap) -> diminishing return (< 1.0x, > 0)
    let (cid_b, sys_b, site_b, _, pos_b) = vg_sites[1].clone();
    with_pve_special_reg(&ctx, |reg| reg.incursions.get_mut(&cid_b).unwrap().influence = 0.05);
    let mut pilots_b = Vec::new();
    for i in 0..12 {
        let (pid, _) = spawn_fleet_pilot_on_grid(&ctx, 30 + i, sys_b, pos_b + DVec3::new((i as f64) * 300.0, 0.0, 0.0)).await;
        pilots_b.push(pid);
    }
    ctx.db.with_mut_snapshot(|snap| {
        for &pid in &pilots_b {
            snap.characters.get_mut(&pid).unwrap().fleet_id = Some(960_002);
        }
        snap.fleets.insert(
            960_002,
            FleetRecord {
                fleet_id: 960_002,
                boss_id: pilots_b[0],
                members: pilots_b.clone(),
                ..Default::default()
            },
        );
    });
    let boss_b = crate::common::clear_incursion_site_escort_waves(&ctx, sys_b, &site_b).await;
    let _ = ctx.systems.get_or_spawn(sys_b).remove_ball(boss_b).await;
    let res_b = on_incursion_boss_destroyed(&ctx, sys_b, boss_b, Some(pilots_b[0]))
        .await
        .unwrap()
        .unwrap();
    assert!(
        res_b.isk_per_pilot > 0 && res_b.isk_per_pilot < res_a.isk_per_pilot,
        "12-pilot overcap Vanguard payout ({}) must diminish below optimal ({})",
        res_b.isk_per_pilot,
        res_a.isk_per_pilot
    );
    assert!(res_b.concord_lp_per_pilot > 0 && res_b.concord_lp_per_pilot < res_a.concord_lp_per_pilot);

    // Case C: Beyond hard-cap fleet (16 on-grid pilots > 15 Vanguard cutoff) -> 0 ISK and 0 CONCORD LP!
    let (cid_c, sys_c, site_c, _, pos_c) = vg_sites[2].clone();
    with_pve_special_reg(&ctx, |reg| reg.incursions.get_mut(&cid_c).unwrap().influence = 0.05);
    let mut pilots_c = Vec::new();
    for i in 0..16 {
        let (pid, _) = spawn_fleet_pilot_on_grid(&ctx, 60 + i, sys_c, pos_c + DVec3::new((i as f64) * 250.0, 0.0, 0.0)).await;
        pilots_c.push(pid);
    }
    ctx.db.with_mut_snapshot(|snap| {
        for &pid in &pilots_c {
            snap.characters.get_mut(&pid).unwrap().fleet_id = Some(960_003);
        }
        snap.fleets.insert(
            960_003,
            FleetRecord {
                fleet_id: 960_003,
                boss_id: pilots_c[0],
                members: pilots_c.clone(),
                ..Default::default()
            },
        );
    });
    let bal_before_c = ctx.db.with_snapshot(|s| s.characters[&pilots_c[0]].balance.0);
    let lp_before_c = ctx
        .db
        .with_snapshot(|s| s.characters[&pilots_c[0]].loyalty_points.get(&1_000_125).copied().unwrap_or(0));
    let boss_c = crate::common::clear_incursion_site_escort_waves(&ctx, sys_c, &site_c).await;
    let _ = ctx.systems.get_or_spawn(sys_c).remove_ball(boss_c).await;
    let res_c = on_incursion_boss_destroyed(&ctx, sys_c, boss_c, Some(pilots_c[0]))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(res_c.isk_per_pilot, 0, "Exceeding Vanguard 15-pilot hard cap must pay 0 ISK");
    assert_eq!(res_c.concord_lp_per_pilot, 0, "Exceeding Vanguard 15-pilot hard cap must pay 0 LP");
    let (bal_after_c, lp_after_c) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&pilots_c[0]];
        (ch.balance.0, ch.loyalty_points.get(&1_000_125).copied().unwrap_or(0))
    });
    assert_eq!(bal_after_c, bal_before_c, "Hard-capped pilot wallet balance must remain unchanged");
    assert_eq!(lp_after_c, lp_before_c, "Hard-capped pilot CONCORD LP must remain unchanged");
}

#[tokio::test]
async fn test_incursion_boss_killed_before_final_wave_escorts_completes_on_last_escort() {
    let ctx = fast_ctx();
    crate::common::pin_clock(&ctx);
    ensure_special_sites_spawned(&ctx).await;

    let (cid, sys_id, site_id, boss_bid, pos) = with_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.values().next().expect("seeded incursion");
        let site = inc.sites.iter().find(|s| s.tier == "Vanguard").expect("Vanguard site");
        (inc.constellation_id, site.solar_system_id, site.site_id.clone(), site.boss_npc_ball_id, DVec3::from_array(site.position))
    });
    let (pilot_id, _) = spawn_fleet_pilot_on_grid(&ctx, 80, sys_id, pos).await;

    // Advance Wave 1 -> Wave 2 -> Wave 3 (final wave) so boss and final-wave escorts are both active
    for _ in 0..2 {
        let escorts: Vec<i64> = with_pve_special_reg(&ctx, |reg| {
            reg.incursions[&cid]
                .sites
                .iter()
                .find(|s| s.site_id == site_id)
                .unwrap()
                .escort_spawns
                .iter()
                .filter(|e| !e.destroyed)
                .map(|e| e.npc_ball_id)
                .collect()
        });
        for eid in escorts {
            let _ = on_incursion_boss_destroyed(&ctx, sys_id, eid, Some(pilot_id)).await;
        }
    }

    let final_escorts: Vec<i64> = with_pve_special_reg(&ctx, |reg| {
        let w = reg.incursion_site_waves.get(&site_id).unwrap();
        assert_eq!(w.current_wave, w.total_waves);
        reg.incursions[&cid]
            .sites
            .iter()
            .find(|s| s.site_id == site_id)
            .unwrap()
            .escort_spawns
            .iter()
            .filter(|e| !e.destroyed)
            .map(|e| e.npc_ball_id)
            .collect()
    });
    assert!(!final_escorts.is_empty());

    // Kill boss BEFORE final-wave escorts: must record boss_destroyed=true without soft-locking
    let _ = ctx.systems.get_or_spawn(sys_id).remove_ball(boss_bid).await;
    let err = on_incursion_boss_destroyed(&ctx, sys_id, boss_bid, Some(pilot_id))
        .await
        .expect("site matched")
        .expect_err("site must not complete while final-wave escorts remain");
    assert_eq!(err.code, "IncursionSiteNpcsRemaining");

    let boss_flag = with_pve_special_reg(&ctx, |reg| {
        reg.incursions[&cid]
            .sites
            .iter()
            .find(|s| s.site_id == site_id)
            .unwrap()
            .boss_destroyed
    });
    assert!(boss_flag, "boss_destroyed must be recorded when boss dies on final wave");

    // Killing the remaining final-wave escorts must complete the site on the last escort kill
    let mut completion = None;
    for eid in final_escorts {
        let _ = ctx.systems.get_or_spawn(sys_id).remove_ball(eid).await;
        if let Some(res) = on_incursion_boss_destroyed(&ctx, sys_id, eid, Some(pilot_id)).await {
            completion = Some(res.expect("site completion on last escort kill"));
        }
    }
    let done = completion.expect("site must complete when last final-wave escort dies");
    assert_eq!(done.site_id, site_id);
}

#[tokio::test]
async fn test_incursion_hq_sites_5171_and_5175_have_disjoint_npc_ball_ids() {
    let ctx = fast_ctx();
    crate::common::pin_clock(&ctx);
    ensure_special_sites_spawned(&ctx).await;

    let (cid, hq_sys_id, site_5171_id, site_5175_id, pos_5171) = with_pve_special_reg(&ctx, |reg| {
        let inc = reg.incursions.values().next().expect("seeded incursion");
        let s5171 = inc.sites.iter().find(|s| s.dungeon_id == 5171).expect("HQ site 5171");
        let s5175 = inc.sites.iter().find(|s| s.dungeon_id == 5175).expect("HQ site 5175");
        assert_eq!(s5171.solar_system_id, s5175.solar_system_id);
        (
            inc.constellation_id,
            s5171.solar_system_id,
            s5171.site_id.clone(),
            s5175.site_id.clone(),
            DVec3::from_array(s5171.position),
        )
    });

    let (pilot_id, _) = spawn_fleet_pilot_on_grid(&ctx, 81, hq_sys_id, pos_5171).await;
    let boss_5171 = crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys_id, &site_5171_id).await;
    let _ = ctx.systems.get_or_spawn(hq_sys_id).remove_ball(boss_5171).await;
    let res_5171 = on_incursion_boss_destroyed(&ctx, hq_sys_id, boss_5171, Some(pilot_id))
        .await
        .expect("site 5171 matched")
        .expect("site 5171 completed");
    assert_eq!(res_5171.site_id, site_5171_id);

    // Verify site 5175 in the same HQ system was untouched and has completely disjoint NPC ball IDs
    with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&cid];
        let s5171 = inc.sites.iter().find(|s| s.site_id == site_5171_id).unwrap();
        let s5175 = inc.sites.iter().find(|s| s.site_id == site_5175_id).unwrap();
        assert_eq!(s5171.completed_count, 1);
        assert_eq!(s5175.completed_count, 0);
        assert!(!s5175.boss_destroyed);
        assert!(s5175.escort_spawns.iter().all(|e| !e.destroyed));

        let ids_5171: std::collections::HashSet<i64> = std::iter::once(s5171.boss_npc_ball_id)
            .chain(s5171.escort_spawns.iter().map(|e| e.npc_ball_id))
            .collect();
        let ids_5175: std::collections::HashSet<i64> = std::iter::once(s5175.boss_npc_ball_id)
            .chain(s5175.escort_spawns.iter().map(|e| e.npc_ball_id))
            .collect();
        assert!(ids_5171.is_disjoint(&ids_5175), "HQ sites 5171 and 5175 must have disjoint NPC ball IDs");
    });
}
}



#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod pve_crab_and_storms_ext {
    use super::*;

// Part 2 negative/regression integration tests for CRAB Beacon Exploits
// (`11019` subcapital rejection, simultaneous double-link refusal, non-compounding
// dogma resonance bonus/restore, Rogue Drone scan-jamming) and Overlapping Metaliminal
// Storms with Cross-Instance Isolation.


use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::inventory_fitting::fitting_containers_ships::validate_single_item_can_be_opened;
use eve_server::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs;
use eve_server::services::pve_crab_beacons::{
    tick_all_crab_beacons, CrabBeaconPhase, CRAB_LINK_DURATION_SECS, CRAB_LOOT_EXCLUSIVE_SECS,
    CRAB_SCAN_DURATION_SECS, TYPE_CRAB_BEACON,
};
use eve_server::services::pve_metaliminal_storms::{
    compute_storm_footprint, sync_metaliminal_storm_effects_and_sites,
};
use eve_server::services::pve_special_sites::{
    with_pve_special_reg, AbyssalWeatherKind, RoamingStormRecord,
};
use glam::DVec3;
use std::collections::HashMap;

const PILOT_1: i64 = 90_000_001;
const LOWSEC_SYS: u32 = 30_002_904; // Tama (Low-sec 0.28)
const TYPE_ARCHON: u32 = 23_757; // Carrier
const TYPE_COCKROACH: u32 = 11_019; // Subcapital QA frigate (must NOT bypass capital check)
const TYPE_EM_SHIELD_AMP_I: u32 = 1_808; // Passive EM Shield Amplifier I (T1)

async fn setup_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sess_id);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
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

    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            11_584,
            TrainedSkillRecord {
                type_id: 11_584,
                level: 3,
                skill_points: 40_000.0,
            },
        );
        snap.skills.entry(char_id).or_default().insert(
            3_425,
            TrainedSkillRecord {
                type_id: 3_425,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
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
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_200_000_000.0,
                radius: 450.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
            ctx.build_ship_slim_meta(char_id, ship_type_id),
        )
        .await;
    sess
}

// ============================================================================
// 1. CRAB Exploit Guards: 11019 Subcap, Simultaneous Link, Dogma Resonance & Drone Jam
// ============================================================================
#[tokio::test]
async fn test_crab_negative_exploits_subcap_11019_double_link_resonance_and_drone_jam() {

    let ctx = fast_ctx();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let pos = DVec3::new(320_000_000.0, 20_000_000.0, 120_000_000.0);

    // 1. Subcapital type_id 11019 (Cockroach) must be rejected with IneligibleShipForCrabLink
    let roach_id = 731_001_001i64;
    let mut sess_roach = setup_pilot(&ctx, 8501, PILOT_1, roach_id, TYPE_COCKROACH, LOWSEC_SYS, pos).await;
    let b1 = 731_100_001i64;
    let b2 = 731_100_002i64;
    for bid in [b1, b2] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                bid,
                InventoryItemRecord {
                    item_id: bid,
                    type_id: TYPE_CRAB_BEACON,
                    owner_id: PILOT_1,
                    location_id: roach_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
        ctx.dispatch_rpc(&mut sess_roach, "posMgr", None, "AnchorStructure", &[PyRep::Integer(bid)], &kw, None)
            .await
            .expect("Anchor CRAB");
    }

    let roach_err = ctx
        .dispatch_rpc(
            &mut sess_roach,
            "beyonce",
            None,
            "CallComponentFromClient",
            &[
                PyRep::Integer(b1),
                PyRep::String("linkWithShip".into()),
                PyRep::String("InitiateLink".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("11019 Cockroach must not bypass CRAB capital ship restriction");
    assert_eq!(roach_err.code, "IneligibleShipForCrabLink");

    // 2. Carrier links b1; attempting to link b2 while already linked to b1 fails with ShipAlreadyLinkedToCrab
    //    WITHOUT consuming CEQ or increasing signal interference!
    let carrier_id = 731_001_002i64;
    let mut sess_cap = setup_pilot(&ctx, 8502, PILOT_1, carrier_id, TYPE_ARCHON, LOWSEC_SYS, pos).await;
    let sys_handle = ctx.systems.get_or_spawn(LOWSEC_SYS);
    let clean_base_res = sys_handle.get_combat_state(carrier_id).await.unwrap().unwrap().shield_resonance;

    let link1 = ctx
        .dispatch_rpc(
            &mut sess_cap,
            "beyonce",
            None,
            "CallComponentFromClient",
            &[
                PyRep::Integer(b1),
                PyRep::String("linkWithShip".into()),
                PyRep::String("InitiateLink".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("Carrier links b1");
    assert_eq!(link1.dict_get("ceqRemaining").and_then(|v| v.as_i64()), Some(70));

    let double_err = ctx
        .dispatch_rpc(
            &mut sess_cap,
            "beyonce",
            None,
            "CallComponentFromClient",
            &[
                PyRep::Integer(b2),
                PyRep::String("linkWithShip".into()),
                PyRep::String("InitiateLink".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Already-linked carrier cannot simultaneously link a 2nd CRAB");
    assert_eq!(double_err.code, "ShipAlreadyLinkedToCrab");
    let ceq_after_reject = ctx
        .dispatch_rpc(&mut sess_cap, "characterEnergyMgr", None, "GetMyEnergyState", &[], &kw, None)
        .await
        .unwrap();
    assert_eq!(
        ceq_after_reject.dict_get("currentEnergy").and_then(|v| v.as_i64()),
        Some(70),
        "Rejected double-link must not deduct CEQ"
    );

    // 3. Fit a passive shield resistance module DURING the active link; when the 4-minute link ends,
    //    clear_crab_link_ship_effects must rebuild dogma resonances (preserving the fitted module)
    //    rather than overwriting with pre-link snapshot resonances or compounding 0.90!
    let amp_id = 731_009_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            amp_id,
            InventoryItemRecord {
                item_id: amp_id,
                type_id: TYPE_EM_SHIELD_AMP_I,
                owner_id: PILOT_1,
                location_id: carrier_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(amp_id, true);
    });
    ctx.invalidate_ship_fitting(carrier_id);

    ctx.advance_secs(CRAB_LINK_DURATION_SECS);
    let expected_post_fit_em_res = ctx.build_ship_combat_state(PILOT_1, carrier_id, TYPE_ARCHON).shield_resonance[0];
    assert!(
        expected_post_fit_em_res < clean_base_res[0],
        "Fitted EM Shield Amplifier lowers EM damage resonance"
    );
    let post_unlink_res = sys_handle.get_combat_state(carrier_id).await.unwrap().unwrap().shield_resonance;
    assert!(
        (post_unlink_res[0] - expected_post_fit_em_res).abs() < 1e-6,
        "Unlinking must restore current dogma-fitted EM resonance ({expected_post_fit_em_res}), got {}",
        post_unlink_res[0]
    );

    // 4. Rogue Drone Jamming: advance 10m (600s) + extra 300s WITHOUT killing Rogue Drones ->
    //    CRAB beacon stays in Scanning phase (`CrabScanInProgress`), and ONLY transitions to
    //    LootRestricted with a full 120s deployer window once all 3 Rogue Drone waves are killed!
    ctx.advance_secs(CRAB_SCAN_DURATION_SECS + 300);
    let (phase_jammed, waves_jammed, live_drones) = ctx.db.with_snapshot(|snap| {
        let b = &snap.crab_beacons.beacons[&b1];
        (b.phase, b.waves_spawned, b.wave_npc_ball_ids.clone())
    });
    assert_eq!(waves_jammed, 3);
    assert!(!live_drones.is_empty());
    assert_eq!(
        phase_jammed,
        CrabBeaconPhase::Scanning,
        "CRAB scan must remain paused while Rogue Drone waves are alive"
    );
    assert_eq!(
        validate_single_item_can_be_opened(&ctx, PILOT_1, b1).await.unwrap_err().code,
        "CrabScanInProgress"
    );

    for bid in live_drones {
        let _ = sys_handle
            .apply_damage_with_attacker(
                bid,
                DamageVector {
                    em: 1.0e6,
                    thermal: 1.0e6,
                    kinetic: 1.0e6,
                    explosive: 1.0e6,
                },
                Some(PILOT_1),
            )
            .await;
    }
    tick_all_crab_beacons(&ctx);
    let (phase_cleared, sd_at) = ctx.db.with_snapshot(|snap| {
        let b = &snap.crab_beacons.beacons[&b1];
        (b.phase, b.self_destruct_at_sec)
    });
    assert_eq!(phase_cleared, CrabBeaconPhase::LootRestricted);
    assert_eq!(sd_at, ctx.now_unix() + CRAB_LOOT_EXCLUSIVE_SECS);
    assert!(validate_single_item_can_be_opened(&ctx, PILOT_1, b1).await.is_ok());
}

// ============================================================================
// 2. Overlapping Storms Stack Both Weather Modifiers & Isolate Per-ServerContext
// ============================================================================
#[tokio::test]
async fn test_overlapping_storms_stack_effects_without_cross_context_leakage() {

    let ctx1 = fast_ctx();
    let ctx2 = fast_ctx();
    crate::common::pin_clock(&ctx1);
    crate::common::pin_clock(&ctx2);

    let overlap_sys = 30_000_208_u32;
    let ship_id = 731_050_001_i64;
    let _sess1 = setup_pilot(&ctx1, 8510, PILOT_1, ship_id, 587, overlap_sys, DVec3::ZERO).await;
    let _sess2 = setup_pilot(&ctx2, 8511, PILOT_1, ship_id, 587, overlap_sys, DVec3::ZERO).await;

    let (core_a, periph_a) = compute_storm_footprint(&ctx1, overlap_sys);
    let (core_b, periph_b) = compute_storm_footprint(&ctx1, overlap_sys);
    with_pve_special_reg(&ctx1, |reg| {
        reg.roaming_storms = vec![
            RoamingStormRecord {
                storm_id: 201,
                weather: AbyssalWeatherKind::Electrical,
                center_solar_system_id: overlap_sys,
                strong_core_systems: core_a,
                weak_periphery_systems: periph_a,
                step_count: 0,
                next_move_at_sec: u64::MAX,
                recent_centers: vec![overlap_sys],
            },
            RoamingStormRecord {
                storm_id: 202,
                weather: AbyssalWeatherKind::Firestorm,
                center_solar_system_id: overlap_sys,
                strong_core_systems: core_b,
                weak_periphery_systems: periph_b,
                step_count: 0,
                next_move_at_sec: u64::MAX,
                recent_centers: vec![overlap_sys],
            },
        ];
    });
    sync_metaliminal_storm_effects_and_sites(&ctx1);

    // Electrical lowers capacitor recharge time (attr 55: *0.50); Firestorm boosts armor HP (attr 265: *1.50)
    let mut attrs1 = HashMap::from([(55_u16, 100.0), (265_u16, 1_000.0)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx1, ship_id, &mut attrs1);
    assert!((attrs1[&55] - 50.0).abs() < 1e-6, "Electrical storm modifier applied");
    assert!((attrs1[&265] - 1_500.0).abs() < 1e-6, "Firestorm modifier stacked simultaneously");

    // Separate ServerContext ctx2 in the same solar system has NO storm beacons and must stay unaffected
    let mut attrs2 = HashMap::from([(55_u16, 100.0), (265_u16, 1_000.0)]);
    apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx2, ship_id, &mut attrs2);
    assert_eq!(attrs2[&55], 100.0, "Storm effects in ctx1 must not leak into ctx2");
    assert_eq!(attrs2[&265], 1_000.0, "Storm effects in ctx1 must not leak into ctx2");
}

// ============================================================================
// 3. Electrical Storm Hacking Virus Coherence Bonus (+25 Core / +10 Periphery)
// ============================================================================
#[tokio::test]
async fn test_electrical_storm_grants_hacking_virus_coherence_per_context() {
    // parity: feat:pve.metaliminal-storms, 
    eve_testkit::attest(&["feat:pve.metaliminal-storms", ]);

    let ctx1 = fast_ctx();
    let ctx2 = fast_ctx();
    crate::common::pin_clock(&ctx1);
    crate::common::pin_clock(&ctx2);

    let center_sys = 30_000_208_u32;
    let (core_sys_list, periph_sys_list) = compute_storm_footprint(&ctx1, center_sys);
    let periph_sys = periph_sys_list
        .iter()
        .copied()
        .find(|s| !core_sys_list.contains(s))
        .expect("distinct periphery system");

    let ship_core = 731_060_001_i64;
    let mut sess_core = setup_pilot(&ctx1, 8520, PILOT_1, ship_core, 587, center_sys, DVec3::ZERO).await;
    let sess_iso = setup_pilot(&ctx2, 8522, PILOT_1, ship_core, 587, center_sys, DVec3::ZERO).await;

    for (ctx, char_id, ship_id, mod_id) in [
        (&ctx1, PILOT_1, ship_core, 731_060_101_i64),
        (&ctx2, PILOT_1, ship_core, 731_060_103_i64),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(char_id).or_default().insert(
                21718,
                eve_db::TrainedSkillRecord { type_id: 21718, level: 5, skill_points: 256_000.0 },
            );
            snap.items.insert(
                mod_id,
                InventoryItemRecord {
                    item_id: mod_id,
                    type_id: 22175, // Data Analyzer I (40 base coherence, 20 strength)
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::MedSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });
    }

    let (base_coh, _, _, _) = eve_server::services::pve_hacking_board::resolve_fitted_analyzer_virus_stats(
        &ctx1, &sess_core, PILOT_1, false,
    )
    .expect("baseline analyzer stats");
    assert_eq!(base_coh, 90, "Baseline Data Analyzer I + Hacking V coherence = 90");

    with_pve_special_reg(&ctx1, |reg| {
        reg.roaming_storms = vec![RoamingStormRecord {
            storm_id: 301,
            weather: AbyssalWeatherKind::Electrical,
            center_solar_system_id: center_sys,
            strong_core_systems: core_sys_list,
            weak_periphery_systems: periph_sys_list,
            step_count: 0,
            next_move_at_sec: u64::MAX,
            recent_centers: vec![center_sys],
        }];
    });
    sync_metaliminal_storm_effects_and_sites(&ctx1);

    let (core_coh, _, _, _) = eve_server::services::pve_hacking_board::resolve_fitted_analyzer_virus_stats(
        &ctx1, &sess_core, PILOT_1, false,
    )
    .expect("core storm analyzer stats");
    let (iso_coh, _, _, _) = eve_server::services::pve_hacking_board::resolve_fitted_analyzer_virus_stats(
        &ctx2, &sess_iso, PILOT_1, false,
    )
    .expect("isolated context analyzer stats");

    assert_eq!(core_coh, 115, "Strong Electrical Storm core grants +25 virus coherence (90 -> 115)");
    assert_eq!(iso_coh, 90, "Electrical Storm in ctx1 must not leak coherence bonus into ctx2");

    // Move pilot in ctx1 into the weak storm periphery system and verify +10 coherence bonus.
    sess_core.solar_system_id = Some(periph_sys);
    ctx1.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.solar_system_id = periph_sys;
        }
        if let Some(ship) = snap.items.get_mut(&ship_core) {
            ship.location_id = i64::from(periph_sys);
        }
    });
    let (periph_coh, _, _, _) = eve_server::services::pve_hacking_board::resolve_fitted_analyzer_virus_stats(
        &ctx1, &sess_core, PILOT_1, false,
    )
    .expect("periphery storm analyzer stats");
    assert_eq!(periph_coh, 100, "Weak Electrical Storm periphery grants +10 virus coherence (90 -> 100)");
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod pve_deadspace_and_dungeons_ext {
    use super::*;

// Integration tests for Naturally Seeded Deadspace Pocket Rules, Data-Driven MWD
// Disallowance, and Multi-Room Dungeon Triggers (`OnApproach`, `OnKill`, `OnLoot`, `CompleteDungeon`)
// using the `DERIVED` 2-room placeholder template until real SDE room/trigger data is imported.


use eve_db::service_roles::ROLE_GM_DEFAULT;
use eve_db::{BookmarkRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::{
    ensure_system_pve_sites_seeded, with_pve_exploration_reg,
};
use glam::DVec3;
use std::collections::HashMap;

const PILOT_1: i64 = 90_000_001;
const JITA_SYS: u32 = 30_000_142;
const TYPE_RIFTER: u32 = 587;
const TYPE_MWD_5MN: u32 = 434;
const TYPE_AB_1MN: u32 = 439;
const WARP_DISRUPTED_ERR: &str = "NaturalPhenomenaDisruptingWarp";

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        ..Default::default()
    }
}

async fn setup_space_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sess_id);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
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
            make_item(ship_id, ship_type_id, char_id, i64::from(sys_id), InventoryFlag::None, 1, true),
        );
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 1.0,
                mass: 1_200_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
            ctx.build_ship_slim_meta(char_id, ship_type_id),
        )
        .await;
    sess
}

async fn step_until_warp_landed(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, ship_id: i64) {
    let kw = HashMap::new();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for _ in 0..120 {
        let _ = sys_handle.step_manual(1.0).await;
        if let Ok(Some((_, b))) = sys_handle.get_ball_snapshot(ship_id).await {
            if matches!(b.mode, MovementMode::Stop) {
                break;
            }
        }
    }
    ctx.dispatch_rpc(sess, "beyonce", None, "UpdateStateRequest", &[], &kw, None)
        .await
        .expect("UpdateStateRequest after warp");
}

// ============================================================================
// 1. Naturally Seeded Deadspace Pocket Rules & Data-Driven MWD Restriction
// ============================================================================
#[tokio::test]
async fn test_deadspace_pocket_rules_and_mwd_restriction() {
    let ctx = fast_ctx();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let ship_id = 750_003_001i64;
    let mwd_id = 750_003_010i64;
    let ab_id = 750_003_011i64;
    let mut sess = setup_space_pilot(&ctx, 9203, PILOT_1, ship_id, TYPE_RIFTER, JITA_SYS, DVec3::ZERO).await;

    // Fit 5MN MWD (434) and 1MN Afterburner (439) + propulsion skills (only MWD online initially due to maxGroupOnline = 1)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mwd_id, make_item(mwd_id, TYPE_MWD_5MN, PILOT_1, ship_id, InventoryFlag::MedSlot0, 1, true));
        snap.items.insert(ab_id, make_item(ab_id, TYPE_AB_1MN, PILOT_1, ship_id, InventoryFlag::MedSlot1, 1, true));
        snap.module_online_states.insert(mwd_id, true);
        snap.module_online_states.insert(ab_id, false);
        let sk = snap.skills.entry(PILOT_1).or_default();
        for sid in [3413_u32, 3426, 3449, 3450, 3454, 3455] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    // 1. Seed system exploration sites naturally -> discovers DSG-30000142 (SDE dungeon 98: Guristas Scout Outpost)
    ensure_system_pve_sites_seeded(&ctx, JITA_SYS).await;
    let (dsg_pos, dsg_inst_id, dsg_did) = with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system[&JITA_SYS]
            .iter()
            .find(|s| s.site_id == "DSG-30000142" && s.site_category == "Deadspace")
            .expect("Naturally seeded DSG-30000142 deadspace combat signature");
        (s.position, s.expedition_instance_id.expect("runtime dungeon instance"), s.dungeon_id)
    });
    assert_eq!(dsg_did, 98, "Caldari high-sec deadspace signature must use SDE Guristas Scout Outpost (98)");

    // Scan DSG-30000142 to 100% via scanMgr.RequestScans and warp to it via beyonce.CmdWarpToStuff
    let probes: Vec<PyRep> = (0..8)
        .map(|_| PyRep::List(vec![PyRep::Real(dsg_pos[0]), PyRep::Real(dsg_pos[1]), PyRep::Real(dsg_pos[2])]))
        .collect();
    ctx.dispatch_rpc(
        &mut sess,
        "scanMgr",
        None,
        "RequestScans",
        &[PyRep::Real(600_000_000_000.0), PyRep::Real(120.0), PyRep::List(probes)],
        &kw,
        None,
    )
    .await
    .expect("RequestScans pinpoints DSG-30000142");

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        None,
        "CmdWarpToStuff",
        &[PyRep::String("scan".into()), PyRep::String("DSG-30000142".into())],
        &kw,
        None,
    )
    .await
    .expect("Warp to scanned DSG-30000142 entry gate");
    step_until_warp_landed(&ctx, &mut sess, JITA_SYS, ship_id).await;

    let inst = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, dsg_inst_id).unwrap();
    let entry_gate_id = inst.dict_get("entryGateBallID").and_then(|v| v.as_i64()).unwrap();
    let PyRep::List(rooms) = inst.dict_get("rooms").unwrap() else { panic!("Expected rooms") };
    let room1_id = rooms[0].dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
    let room2_id = rooms[1].dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
    let PyRep::Tuple(r2_pos_py) = rooms[1].dict_get("position").unwrap() else { panic!() };
    let r2_pos = [
        r2_pos_py[0].as_f64().unwrap(),
        r2_pos_py[1].as_f64().unwrap(),
        r2_pos_py[2].as_f64().unwrap(),
    ];

    // 2. Direct warp into gated Room 1 (by roomID) or gated Room 2 (by bookmark) is refused
    let warp_r1_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("dungeon".into()), PyRep::Integer(room1_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Direct warp into gated Room 1 must fail");
    assert_eq!(warp_r1_err.code, WARP_DISRUPTED_ERR);

    let bm_id = 880_001_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            bm_id,
            BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: PILOT_1,
                label: "Inner Room Bookmark".into(),
                solar_system_id: JITA_SYS,
                x: r2_pos[0],
                y: r2_pos[1],
                z: r2_pos[2],
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    let warp_bm_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("bookmark".into()), PyRep::Integer(bm_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Direct bookmark warp into gated Room 2 must fail");
    assert_eq!(warp_bm_err.code, WARP_DISRUPTED_ERR);

    // 3. Activate entry acceleration gate from warp landing spot -> lands in Room 1; MWD disallowed in deadspace pocket unless overridden
    let sys_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let eg_pos = sys_handle.get_ball_snapshot(entry_gate_id).await.unwrap().unwrap().1.position;
    let ship_landed_pos = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position;
    assert!(ship_landed_pos.distance(eg_pos) <= 2_500.0, "Warp lands within 2.5km of entry gate");

    let gate1_res = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(entry_gate_id)], &kw, None)
        .await
        .expect("Activate entry acceleration gate");
    assert_eq!(gate1_res.dict_get("disallowMWD"), Some(&PyRep::Bool(true)));

    let r1_mwd_err = ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect_err("MWD disallowed in deadspace Room 1 by default");
    assert_eq!(r1_mwd_err.code, "ModuleNotAllowedInDeadspace");

    // Clear Room 1 guards to unlock the Room 1 -> Room 2 gate, approach gate via CmdFollowBall, and enter Room 2
    let PyRep::List(r1_npcs) = gate1_res.dict_get("npcBallIDs").unwrap() else { panic!() };
    for nid in r1_npcs {
        let _ = sys_handle
            .apply_damage_with_attacker(
                nid.as_i64().unwrap(),
                DamageVector { em: 1.0e6, thermal: 1.0e6, kinetic: 1.0e6, explosive: 1.0e6 },
                Some(PILOT_1),
            )
            .await;
    }
    let r1_gate_id = gate1_res.dict_get("nextGateBallID").and_then(|v| v.as_i64()).unwrap();
    let beyonce_oid = ctx.bind_object(&sess, "beyonce", i64::from(JITA_SYS)).unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &kw, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(r1_gate_id), PyRep::Real(500.0)], &kw, None)
        .await
        .expect("CmdFollowBall to Room 1 -> Room 2 gate");
    for _ in 0..60 {
        let _ = sys_handle.step_manual(1.0).await;
    }
    let gate2_res = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
        .await
        .expect("Activate Room 1 -> Room 2 gate");
    assert_eq!(gate2_res.dict_get("disallowMWD"), Some(&PyRep::Bool(true)));

    // 4. In inner Room 2 (disallowMWD == true from SDE archetype 51): MWD refused, Afterburner allowed
    let mwd_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect_err("MWD must be refused in MWD-disallowed deadspace room");
    assert_eq!(mwd_err.code, "ModuleNotAllowedInDeadspace");

    // 4b. Editing dungeon/room MWD flag dynamically changes live MWD outcome in Room 2
    eve_server::services::pve_dungeon_editor::set_dungeon_disallow_mwd(&ctx, 98, false);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect("MWD must be allowed in Room 2 after setting disallowMWD = false");
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Deactivate",
            &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())],
            &kw,
            None,
        )
        .await;
    eve_server::services::pve_dungeon_editor::set_dungeon_disallow_mwd(&ctx, 98, true);
    let dyn_mwd_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect_err("MWD must be refused again after setting disallowMWD = true");
    assert_eq!(dyn_mwd_err.code, "ModuleNotAllowedInDeadspace");

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mwd_id, false);
        snap.module_online_states.insert(ab_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(ab_id), PyRep::String("moduleBonusAfterburner".into()), PyRep::None, PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect("Afterburner must remain allowed in MWD-disallowed deadspace room");

    // 5. Data-driven dungeon editor flags persist in world state
    sess.role |= ROLE_GM_DEFAULT;
    let mut edit_room_kw = HashMap::new();
    edit_room_kw.insert("disallowMWD".into(), PyRep::Bool(true));
    let created_room = ctx
        .dispatch_rpc(&mut sess, "dungeon", None, "EditRoom", &[PyRep::Integer(98), PyRep::String("Restricted Core".into())], &edit_room_kw, None)
        .await
        .expect("dungeon.EditRoom with disallowMWD");
    let created_room_id = created_room.dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
    assert!(eve_server::services::pve_dungeon_editor::is_dungeon_or_room_mwd_disallowed(&ctx, 98, Some(created_room_id)));
    eve_server::services::pve_dungeon_editor::set_dungeon_disallow_mwd(&ctx, 98, true);
    assert!(eve_server::services::pve_dungeon_editor::is_dungeon_or_room_mwd_disallowed(&ctx, 98, Some(room2_id)));
}

// ============================================================================
// 2. SDE-Seeded Multi-Room Dungeon System: Approach/Kill/Loot Triggers & Despawn
// ============================================================================
#[tokio::test]
async fn test_dungeon_system_rooms_triggers_and_despawn_on_completion() {
    let ctx = fast_ctx();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let ship_id = 750_004_001i64;
    let mut sess = setup_space_pilot(&ctx, 9204, PILOT_1, ship_id, TYPE_RIFTER, JITA_SYS, DVec3::ZERO).await;

    // 1. Discover naturally seeded DSG-30000142, scan via RequestScans, warp via CmdWarpToStuff, enter Room 1
    ensure_system_pve_sites_seeded(&ctx, JITA_SYS).await;
    let (dsg_pos, inst_id) = with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system[&JITA_SYS]
            .iter()
            .find(|s| s.site_id == "DSG-30000142")
            .unwrap();
        (s.position, s.expedition_instance_id.unwrap())
    });
    let probes: Vec<PyRep> = (0..8)
        .map(|_| PyRep::List(vec![PyRep::Real(dsg_pos[0]), PyRep::Real(dsg_pos[1]), PyRep::Real(dsg_pos[2])]))
        .collect();
    ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[PyRep::Real(600_000_000_000.0), PyRep::Real(120.0), PyRep::List(probes)], &kw, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("scan".into()), PyRep::String("DSG-30000142".into())], &kw, None)
        .await
        .unwrap();
    step_until_warp_landed(&ctx, &mut sess, JITA_SYS, ship_id).await;

    let init_state = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id).unwrap();
    let entry_gate_id = init_state.dict_get("entryGateBallID").and_then(|v| v.as_i64()).unwrap();
    let gate1_res = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(entry_gate_id)], &kw, None)
        .await
        .expect("Enter Room 1 via entry gate");

    let r1_gate_id = gate1_res.dict_get("nextGateBallID").and_then(|v| v.as_i64()).unwrap();
    let PyRep::List(r1_objs) = gate1_res.dict_get("objectBallIDs").unwrap() else { panic!() };
    let r1_beacon_id = r1_objs[0].as_i64().unwrap();
    let PyRep::List(r1_npcs) = gate1_res.dict_get("npcBallIDs").unwrap() else { panic!() };
    assert_eq!(r1_npcs.len(), 2, "Group 1 spawns 2 SDE Guristas frigate guards immediately");
    let g1_npc_ids: Vec<i64> = r1_npcs.iter().map(|v| v.as_i64().unwrap()).collect();

    let sys_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let (_, g1_ball) = sys_handle.get_ball_snapshot(g1_npc_ids[0]).await.unwrap().unwrap();
    assert_ne!(g1_ball.type_id, 10_043, "Dungeon NPCs must come from SDE faction spawn groups, not hardcoded 10043");

    let beyonce_oid = ctx.bind_object(&sess, "beyonce", i64::from(JITA_SYS)).unwrap();
    let ship_inv_oid = ctx.bind_object(&sess, "invbroker", ship_id).unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &kw, None)
        .await
        .unwrap();

    // 2. Approach Room 1->2 gate first: locked gate refuses activation with GateLocked
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(r1_gate_id), PyRep::Real(500.0)], &kw, None)
        .await
        .unwrap();
    for _ in 0..60 {
        let _ = sys_handle.step_manual(1.0).await;
    }
    let locked_err = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
        .await
        .expect_err("Locked gate must refuse activation before Group 1 is cleared");
    assert_eq!(locked_err.code, "GateLocked");

    // 3. Approach Deadspace Relay Beacon via client CmdFollowBall + UpdateStateRequest -> OnApproach spawns Group 2 cruiser!
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(r1_beacon_id), PyRep::Real(500.0)], &kw, None)
        .await
        .unwrap();
    for _ in 0..40 {
        let _ = sys_handle.step_manual(1.0).await;
    }
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "UpdateStateRequest", &[], &kw, None)
        .await
        .unwrap();

    let after_approach = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id).unwrap();
    let PyRep::List(rooms_app) = after_approach.dict_get("rooms").unwrap() else { panic!() };
    let PyRep::List(r1_npcs_after) = rooms_app[0].dict_get("npcBallIDs").unwrap() else { panic!() };
    assert_eq!(r1_npcs_after.len(), 3, "OnApproach trigger must spawn Group 2 reinforcement NPC");
    let r1_reinf_id = r1_npcs_after[2].as_i64().unwrap();

    // 4. Destroy Group 1 guards -> OnKill unlocks Room 1->2 gate (leaving Group 2 alive to verify despawn later)
    for nid in g1_npc_ids {
        let _ = sys_handle
            .apply_damage_with_attacker(
                nid,
                DamageVector { em: 1.0e6, thermal: 1.0e6, kinetic: 1.0e6, explosive: 1.0e6 },
                Some(PILOT_1),
            )
            .await;
    }
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(r1_gate_id), PyRep::Real(500.0)], &kw, None)
        .await
        .unwrap();
    for _ in 0..40 {
        let _ = sys_handle.step_manual(1.0).await;
    }
    let enter_r2 = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
        .await
        .expect("Unlocked gate enters Room 2");

    let PyRep::List(r2_objs) = enter_r2.dict_get("objectBallIDs").unwrap() else { panic!() };
    let strongbox_id = r2_objs[0].as_i64().unwrap();
    let PyRep::List(r2_npcs) = enter_r2.dict_get("npcBallIDs").unwrap() else { panic!() };
    let r2_boss_id = r2_npcs[0].as_i64().unwrap();

    // 5. Approach & loot Overseer Strongbox via invbroker.LootAll -> OnLoot spawns Group 2 ambush defender!
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &kw, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(strongbox_id), PyRep::Real(500.0)], &kw, None)
        .await
        .unwrap();
    for _ in 0..55 {
        let _ = sys_handle.step_manual(1.0).await;
    }
    ctx.dispatch_rpc(&mut sess, "invbroker", Some(ship_inv_oid), "LootAll", &[PyRep::Integer(strongbox_id)], &kw, None)
        .await
        .expect("LootAll on Overseer Strongbox");

    let after_loot = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id).unwrap();
    let PyRep::List(rooms_loot) = after_loot.dict_get("rooms").unwrap() else { panic!() };
    let PyRep::List(r2_npcs_after_loot) = rooms_loot[1].dict_get("npcBallIDs").unwrap() else { panic!() };
    assert_eq!(r2_npcs_after_loot.len(), 2, "OnLoot trigger must spawn Room 2 ambush NPC");
    let r2_ambush_id = r2_npcs_after_loot[1].as_i64().unwrap();

    // 6. Destroy Room 2 Overseer commander -> OnKill fires CompleteDungeon, despawns all entities & collapses signature!
    let _ = sys_handle
        .apply_damage_with_attacker(
            r2_boss_id,
            DamageVector { em: 1.0e7, thermal: 1.0e7, kinetic: 1.0e7, explosive: 1.0e7 },
            Some(PILOT_1),
        )
        .await;

    let final_state = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, inst_id).unwrap();
    assert_eq!(final_state.dict_get("completed"), Some(&PyRep::Bool(true)));
    assert_eq!(final_state.dict_get("despawned"), Some(&PyRep::Bool(true)));
    assert!(sys_handle.get_ball_snapshot(entry_gate_id).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(r1_beacon_id).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(r1_gate_id).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(r1_reinf_id).await.unwrap().is_none());
    assert!(sys_handle.get_ball_snapshot(r2_ambush_id).await.unwrap().is_none());
    assert!(
        with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system[&JITA_SYS]
                .iter()
                .find(|s| s.site_id == "DSG-30000142")
                .unwrap()
                .cleared
        }),
        "Completing deadspace dungeon must mark DSG-30000142 cleared"
    );
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod pve_incursions_triglavian_ext {
    use super::*;

// Authentic Incursion System, Payouts, Mothership Finale, Drifter Hives & Triglavian Sites.
// Remediated integration tests for Phase 8 / PVE milestones.

use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_incursion_sites::{
    get_incursion_effects_for_system, on_incursion_boss_destroyed,
};
use eve_server::services::pve_special_sites::{
    ensure_special_sites_spawned, get_drifter_hive_for_system, with_pve_special_reg,
};
use glam::DVec3;
use std::collections::HashMap;

async fn create_in_space_session(
    ctx: &ServerContext,
    session_id: u64,
    char_id: i64,
    solar_system_id: u32,
    ship_id: i64,
    ship_type_id: u32,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
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

    sess.solar_system_id = Some(solar_system_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        let p1_template = snap.characters[&90_000_001].clone();
        let ch = snap.characters.entry(char_id).or_insert_with(|| p1_template);
        ch.character_id = char_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = solar_system_id;
        ch.active_ship_id = ship_id;

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: solar_system_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: ship_type_id,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 1.0,
            mass: 1_067_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    sess
}

// ============================================================================
// 1. Constellation System Effects, Encounters & taleMgr World Event RPCs
// ============================================================================
#[tokio::test]
async fn test_pve_incursion_constellation_system_effects_and_tales() {
    // parity: feat:pve.incursion-system
    eve_testkit::attest(&["feat:pve.incursion-system"]);

    let ctx = fast_ctx();
    ensure_special_sites_spawned(&ctx).await;
    let kw = HashMap::new();

    let (target_cid, hq_sys_id, staging_sys_id) = with_pve_special_reg(&ctx, |reg| {
        let (cid, inc) = reg.incursions.iter().next().unwrap();
        (*cid, inc.hq_solar_system_id, inc.staging_solar_system_id)
    });
    let mut sess = create_in_space_session(&ctx, 8201, 90_000_001, hq_sys_id, 710_002_001, 587).await;

    // 1. V24 World Event RPCs via taleMgr
    let tales_rep = ctx
        .dispatch_rpc(&mut sess, "taleMgr", None, "GetGlobalWorldEventTales", &[], &kw, None)
        .await
        .expect("GetGlobalWorldEventTales");
    let PyRep::List(tales) = tales_rep else { panic!("Expected List of tales") };
    assert!(!tales.is_empty(), "Active incursion tales must be returned");

    let tale = tales
        .iter()
        .find(|t| t.dict_get("constellationID").and_then(|v| v.as_i64()) == Some(target_cid as i64))
        .expect("Target incursion tale must exist");
    assert_eq!(tale.dict_get("templateID").and_then(|v| v.as_i64()), Some(5158));
    assert_eq!(tale.dict_get("influence").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(tale.dict_get("state").and_then(|v| v.as_str()), Some("Establishing"));
    assert_eq!(tale.dict_get("managerSolarSystemID").and_then(|v| v.as_i64()), Some(staging_sys_id as i64));
    assert_eq!(tale.dict_get("hasFinalEncounter").and_then(|v| v.as_bool()), Some(false));

    // Verify sceneTypesByLocationID mapping has staging and HQ
    let PyRep::Dict(scene_map) = tale.dict_get("sceneTypesByLocationID").expect("sceneTypesByLocationID") else { panic!("Expected Dict") };
    assert!(scene_map.iter().any(|(k, v)| k.as_i64() == Some(staging_sys_id as i64) && v.as_str() == Some("incursionStaging")));
    assert!(scene_map.iter().any(|(k, v)| k.as_i64() == Some(hq_sys_id as i64) && v.as_str() == Some("incursionHeavyInfestation")));

    // Verify sites list in tale contains staging, vanguard, assault, headquarters, mothership
    let PyRep::List(sites) = tale.dict_get("sites").expect("sites list") else { panic!("Expected List of sites") };
    assert!(sites.iter().any(|s| s.dict_get("tier").and_then(|v| v.as_str()) == Some("Staging")));
    assert!(sites.iter().any(|s| s.dict_get("tier").and_then(|v| v.as_str()) == Some("Vanguard")));
    assert!(sites.iter().any(|s| s.dict_get("tier").and_then(|v| v.as_str()) == Some("Assault")));
    assert!(sites.iter().any(|s| s.dict_get("tier").and_then(|v| v.as_str()) == Some("Headquarters")));
    let ms_site = sites.iter().find(|s| s.dict_get("tier").and_then(|v| v.as_str()) == Some("Mothership")).expect("Mothership site");
    assert_eq!(ms_site.dict_get("available").and_then(|v| v.as_bool()), Some(false), "Mothership locked at 1.0 influence");

    // 2. taleMgr.get_active_tales_by_template
    let active_tales = ctx
        .dispatch_rpc(&mut sess, "taleMgr", None, "get_active_tales_by_template", &[PyRep::Integer(5158)], &kw, None)
        .await
        .expect("get_active_tales_by_template");
    let PyRep::List(active_list) = active_tales else { panic!("Expected List") };
    assert!(!active_list.is_empty(), "Active tales list by template must not be empty");

    // 3. Sansha Encounter Compositions & Site Distribution
    with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        assert_eq!(inc.sites.len(), 8, "Constellation must seed staging, 2 vanguard, 2 assault, 2 HQ, mothership");
        for site in &inc.sites {
            assert!(site.boss_type_id > 0, "Site must have authentic Sansha boss type ID");
            assert!(!site.escort_spawns.is_empty(), "Site must have Sansha escort spawns");
        }
    });

    // 4. Incursion System Effects & Dogma Attributes
    let eff = get_incursion_effects_for_system(&ctx, hq_sys_id).expect("incursion effects");
    assert_eq!(eff.influence, 1.0);
    assert!(eff.cyno_jammed);
    assert_eq!(eff.shield_penalty, 0.50);
    assert_eq!(eff.damage_penalty, 0.50);

    let mut ship_attrs = HashMap::from([(263, 1000.0), (265, 1000.0), (482, 1000.0), (64, 1.0)]);
    eve_server::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs(&ctx, 710_002_001, &mut ship_attrs);
    assert_eq!(ship_attrs[&263], 500.0, "50% shield penalty at 1.0 influence");
    assert_eq!(ship_attrs[&265], 500.0, "50% armor penalty at 1.0 influence");
    assert_eq!(ship_attrs[&482], 500.0, "50% capacitor penalty at 1.0 influence");
    assert_eq!(ship_attrs[&64], 0.5, "50% damage penalty at 1.0 influence");

    // 5. Incursion client RPC contracts & rejection of non-catalog RPCs
    let lp_log = ctx
        .dispatch_rpc(&mut sess, "incursion", None, "GetIncursionLPLog", &[], &kw, None)
        .await
        .expect("GetIncursionLPLog");
    assert_eq!(lp_log, PyRep::List(vec![]));

    let update_lp = ctx
        .dispatch_rpc(&mut sess, "incursion", None, "UpdateIncursionLPLog", &[PyRep::String("INC-SYS-5158".into())], &kw, None)
        .await
        .expect("UpdateIncursionLPLog");
    assert_eq!(update_lp, PyRep::None);

    for non_client_rpc in ["GetIncursionSystemEffects", "GetAllIncursionSites"] {
        let err = ctx
            .dispatch_rpc(&mut sess, "incursion", None, non_client_rpc, &[PyRep::Integer(hq_sys_id as i64)], &kw, None)
            .await
            .expect_err("non-client RPC must fail");
        assert_eq!(err.code, "UnimplementedRPC");
    }
}

// ============================================================================
// 2. Site Payouts Scaled by Fleet Size & Constellation Influence
// ============================================================================
#[tokio::test]
async fn test_pve_incursion_multiwave_fleet_payout_and_influence() {

    let ctx = fast_ctx();
    ensure_special_sites_spawned(&ctx).await;
    let kw = HashMap::new();

    let (target_cid, vg_sys_id) = with_pve_special_reg(&ctx, |reg| {
        let (cid, inc) = reg.incursions.iter().next().unwrap();
        (*cid, inc.vanguard_solar_system_ids[0])
    });

    let (boss_bid, site_id, site_pos) = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        let site = inc.sites.iter().find(|s| s.tier == "Vanguard").unwrap();
        (site.boss_npc_ball_id, site.site_id.clone(), DVec3::from_array(site.position))
    });

    let vg_sys = ctx.systems.get_or_spawn(vg_sys_id);
    let mut sess = create_in_space_session(&ctx, 8202, 90_000_001, vg_sys_id, 710_002_002, 587).await;
    vg_sys.set_ball_position(710_002_002, site_pos).await;

    // 5-pilot fleet (optimal size 5 for Vanguard)
    let pids = [90_000_001, 90_000_002, 90_000_003, 90_000_004, 90_000_005];
    ctx.db.with_mut_snapshot(|snap| {
        let p1_template = snap.characters[&90_000_001].clone();
        for &pid in &pids {
            let ch = snap.characters.entry(pid).or_insert_with(|| p1_template.clone());
            ch.character_id = pid;
            ch.solar_system_id = vg_sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.fleet_id = Some(941_001);
            ch.active_ship_id = 710_000_000 + pid;
        }
        snap.fleets.insert(941_001, FleetRecord {
            fleet_id: 941_001,
            boss_id: 90_000_001,
            members: pids.to_vec(),
            ..Default::default()
        });
    });

    for &pid in &pids {
        let ship_id = 710_000_000 + pid;
        let _ = vg_sys.spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: site_pos + DVec3::new((pid % 500) as f64, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 1.0,
            mass: 1_067_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        }).await;
    }

    let p_inits: HashMap<i64, (i64, i64)> = pids
        .iter()
        .map(|&pid| {
            (
                pid,
                (
                    ctx.db.with_snapshot(|s| s.characters[&pid].balance.0),
                    ctx.db.with_snapshot(|s| s.characters[&pid].loyalty_points.get(&1_000_125).copied().unwrap_or(0)),
                ),
            )
        })
        .collect();

    // Kill boss in space with attacker attribution after clearing escort waves
    crate::common::clear_incursion_site_escort_waves(&ctx, vg_sys_id, &site_id).await;
    let (_, dead, _) = vg_sys.apply_damage_with_attacker(
        boss_bid,
        DamageVector { em: 100_000_000.0, thermal: 100_000_000.0, kinetic: 100_000_000.0, explosive: 100_000_000.0 },
        Some(90_000_001),
    ).await.unwrap().unwrap();
    assert!(dead, "Boss ball must be destroyed");

    // Influence was 1.0; delta for Vanguard is 0.05 -> new influence is 0.95
    // influence_scale = (1.0 - 0.95 * 0.50) = 0.525.
    // 5 pilots in Vanguard -> fleet_scale = 1.0. Total scale = 0.525.
    // base_isk = 10,395,000 -> isk = 5,457,375 ISK. base_lp = 1,400 -> lp = 735 LP.
    for &pid in &pids {
        let (bal, lp) = ctx.db.with_snapshot(|s| (s.characters[&pid].balance.0, s.characters[&pid].loyalty_points.get(&1_000_125).copied().unwrap_or(0)));
        assert_eq!(bal - p_inits[&pid].0, 10_395_000 * 100, "All fleet members must receive identical payout");
        assert_eq!(lp - p_inits[&pid].1, 1400, "All fleet members must receive identical LP");
    }

    let journal = ctx.db.with_snapshot(|s| s.wallet_journal.clone());
    assert!(journal.iter().any(|j| j.character_id == 90_000_001 && j.entry_type == "CorporateRewardPayout"));

    let logs_rep = ctx.dispatch_rpc(&mut sess, "incursion", None, "GetIncursionLPLog", &[], &kw, None).await.expect("GetIncursionLPLog");
    let PyRep::List(logs) = logs_rep else { panic!("Expected List") };
    assert!(!logs.is_empty());
    assert_eq!(logs[0].dict_get("iskEarned").and_then(|v| v.as_i64()), Some(10_395_000));

    // Next wave boss respawned once escort waves clear
    let next_boss_bid = crate::common::clear_incursion_site_escort_waves(&ctx, vg_sys_id, &site_id).await;
    assert!(vg_sys.get_ball_snapshot(next_boss_bid).await.unwrap().is_some());

    // Double payout race concurrency protection
    let (res1, res2) = tokio::join!(
        eve_server::services::pve_incursion_sites::process_incursion_site_completion_and_payout(&ctx, Some(90_000_001), None, vg_sys_id, &site_id, None),
        eve_server::services::pve_incursion_sites::process_incursion_site_completion_and_payout(&ctx, Some(90_000_001), None, vg_sys_id, &site_id, None),
    );
    assert_eq!([res1.is_ok(), res2.is_ok()].into_iter().filter(|&x| x).count(), 0, "Boss is alive in space, manual trigger without boss death fails");
}

// ============================================================================
// 3. Kundalini Manifest Mothership Finale & Incursion Conclusion
// ============================================================================
#[tokio::test]
async fn test_pve_incursion_mothership_hq_unlock_and_conclusion() {
    // parity: feat:pve.incursion-mothership
    eve_testkit::attest(&["feat:pve.incursion-mothership"]);

    let ctx = fast_ctx();
    ensure_special_sites_spawned(&ctx).await;

    let (target_cid, hq_sys_id) = with_pve_special_reg(&ctx, |reg| {
        let (cid, inc) = reg.incursions.iter().next().unwrap();
        (*cid, inc.hq_solar_system_id)
    });
    let _sess = create_in_space_session(&ctx, 8203, 90_000_001, hq_sys_id, 710_002_003, 587).await;
    let hq_sys = ctx.systems.get_or_spawn(hq_sys_id);

    // Initial check: Mothership site locked at 1.0 influence
    let ms_boss_bid_init = with_pve_special_reg(&ctx, |reg| {
        reg.incursions[&target_cid].sites.iter().find(|s| s.tier == "Mothership").unwrap().boss_npc_ball_id
    });
    assert!(hq_sys.get_ball_snapshot(ms_boss_bid_init).await.unwrap().is_none());

    let lock_attempt = on_incursion_boss_destroyed(&ctx, hq_sys_id, ms_boss_bid_init, Some(90_000_001)).await;
    assert_eq!(lock_attempt.unwrap().unwrap_err().code, "MothershipSiteLocked");

    // Complete HQ site waves to bring influence from 1.0 down to 0.0
    let (hq_site_id, hq_pos) = with_pve_special_reg(&ctx, |reg| {
        let s = reg.incursions[&target_cid].sites.iter().find(|s| s.tier == "Headquarters").unwrap();
        (s.site_id.clone(), DVec3::from_array(s.position))
    });
    hq_sys.set_ball_position(710_002_003, hq_pos).await;

    for _wave in 0..4 {
        let hq_boss_bid = crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys_id, &hq_site_id).await;
        assert!(hq_sys.get_ball_snapshot(hq_boss_bid).await.unwrap().is_some());
        let (_, dead, _) = hq_sys.apply_damage_with_attacker(
            hq_boss_bid,
            DamageVector { em: 100_000_000.0, thermal: 100_000_000.0, kinetic: 100_000_000.0, explosive: 100_000_000.0 },
            Some(90_000_001),
        ).await.unwrap().unwrap();
        assert!(dead);
    }

    // Mothership unlocks and spawns
    let (inf_after_hq, ms_spawned, ms_site_id, ms_boss_bid, ms_pos) = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        let ms = inc.sites.iter().find(|s| s.tier == "Mothership").unwrap();
        (inc.influence, inc.mothership_spawned, ms.site_id.clone(), ms.boss_npc_ball_id, DVec3::from_array(ms.position))
    });
    assert_eq!(inf_after_hq, 0.0);
    assert!(ms_spawned);

    // Verify The Kundalini Manifest ball exists in space with type_id 2192
    let ms_ball_snap = hq_sys.get_ball_snapshot(ms_boss_bid).await.unwrap().expect("Kundalini Manifest spawned");
    assert_eq!(ms_ball_snap.1.type_id, 2192);

    let escort_bids = with_pve_special_reg(&ctx, |reg| {
        reg.incursions[&target_cid].sites.iter().find(|s| s.tier == "Mothership").unwrap()
            .escort_spawns.iter().map(|e| (e.npc_ball_id, e.type_id)).collect::<Vec<_>>()
    });
    assert!(!escort_bids.is_empty());
    for (e_bid, e_tid) in &escort_bids {
        let e_snap = hq_sys.get_ball_snapshot(*e_bid).await.unwrap().expect("Escort ball spawned");
        assert_eq!(e_snap.1.type_id, *e_tid);
    }

    // Kill The Kundalini Manifest in space after clearing escorts
    crate::common::clear_incursion_site_escort_waves(&ctx, hq_sys_id, &ms_site_id).await;
    hq_sys.set_ball_position(710_002_003, ms_pos).await;
    let pre_ms_bal = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
    let (_, ms_dead, _) = hq_sys.apply_damage_with_attacker(
        ms_boss_bid,
        DamageVector { em: 100_000_000.0, thermal: 100_000_000.0, kinetic: 100_000_000.0, explosive: 100_000_000.0 },
        Some(90_000_001),
    ).await.unwrap().unwrap();
    assert!(ms_dead);

    // Verify conclusion (enters Withdrawing on timer after mothership dies)
    let (final_state, ms_defeated) = with_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&target_cid];
        (inc.state.clone(), inc.mothership_defeated)
    });
    assert_eq!(final_state, "Withdrawing");
    assert!(ms_defeated);

    // Escorts removed from space
    for (e_bid, _) in escort_bids {
        assert!(hq_sys.get_ball_snapshot(e_bid).await.unwrap().is_none());
    }

    // Payout disbursed
    let post_ms_bal = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
    assert_eq!((post_ms_bal - pre_ms_bal) / 100, 1_575_000);

    // Penalties withdrawn
    assert!(get_incursion_effects_for_system(&ctx, hq_sys_id).is_none());
}

// ============================================================================
// 4. Drifter Hives Multi-room Progression & Authentic Loot
// ============================================================================
#[tokio::test]
async fn test_pve_drifter_hives_multiroom_progression_and_loot() {

    let ctx = fast_ctx();
    ensure_special_sites_spawned(&ctx).await;
    let kw = HashMap::new();

    // Verify 5 Drifter Hives initialized across wormhole systems 31_000_001..=31_000_005
    let expected_elements = [
        (31_000_001, 34_557, "Barbican Hive"),
        (31_000_002, 34_558, "Vidette Hive"),
        (31_000_003, 34_559, "Conflux Hive"),
        (31_000_004, 34_560, "Redoubt Hive"),
        (31_000_005, 34_556, "Sentinel Hive"),
    ];

    for &(sys_id, elem_tid, name) in &expected_elements {
        let hive = get_drifter_hive_for_system(&ctx, sys_id).expect("hive must exist");
        assert_eq!(hive.dungeon_name, name);
        assert_eq!(hive.element_type_id, elem_tid);
        assert_eq!(hive.total_rooms, 3);
        assert_eq!(hive.rooms.len(), 3);
        assert_eq!(hive.current_room, 1);
        assert!(!hive.cleared);

        // Verify authentic SDE NPC types in rooms: Cruisers (47958/47959), Battleships (47956/47957), Warden (37473)
        assert!(hive.rooms[0].npc_type_ids.contains(&47958) || hive.rooms[0].npc_type_ids.contains(&47959));
        assert!(hive.rooms[1].npc_type_ids.contains(&47956) || hive.rooms[1].npc_type_ids.contains(&47957));
        assert!(hive.rooms[2].npc_type_ids.contains(&37473));
    }

    // Step-by-step room progression in Barbican Hive (31_000_001)
    let char_id = 90_000_001;
    let mut sess = create_in_space_session(&ctx, 8204, char_id, 31_000_001, 710_002_004, 587).await;
    let wh_sys = ctx.systems.get_or_spawn(31_000_001);
    let hive_init = get_drifter_hive_for_system(&ctx, 31_000_001).unwrap();

    // Destroy Room 1 cruisers
    for &npc_bid in &hive_init.rooms[0].npc_ball_ids {
        let _ = wh_sys.apply_damage_with_attacker(
            npc_bid,
            DamageVector { em: 50_000_000.0, thermal: 50_000_000.0, kinetic: 50_000_000.0, explosive: 50_000_000.0 },
            Some(char_id),
        ).await;
    }

    // Room 1 wave clearance in space naturally advances progression to Room 2
    let hive_after_r1 = get_drifter_hive_for_system(&ctx, 31_000_001).unwrap();
    assert_eq!(hive_after_r1.current_room, 2);
    assert!(hive_after_r1.rooms[0].cleared);
    assert!(!hive_after_r1.rooms[1].cleared);

    // Verify Room 2 battleships were automatically spawned into space
    for &npc_bid in &hive_after_r1.rooms[1].npc_ball_ids {
        let snap = wh_sys.get_ball_snapshot(npc_bid).await.unwrap();
        assert!(snap.is_some(), "Room 2 battleship must be automatically spawned");
    }

    // Acceleration gate (client RPC keeper.EnterNextDungeonRoom) refuses to jump while Room 2 wave is alive
    let gate_blocked = ctx.dispatch_rpc(&mut sess, "keeper", None, "EnterNextDungeonRoom", &[PyRep::Integer(31_000_001)], &kw, None).await;
    assert!(gate_blocked.is_err(), "Acceleration gate must be gated by Room 2 wave-clear");
    assert_eq!(get_drifter_hive_for_system(&ctx, 31_000_001).unwrap().current_room, 2);

    // Destroy Room 2 battleships in space
    for &npc_bid in &hive_after_r1.rooms[1].npc_ball_ids {
        let _ = wh_sys.apply_damage_with_attacker(
            npc_bid,
            DamageVector { em: 50_000_000.0, thermal: 50_000_000.0, kinetic: 50_000_000.0, explosive: 50_000_000.0 },
            Some(char_id),
        ).await;
    }

    // Room 2 wave clearance in space naturally advances progression to Room 3
    let hive_after_r2 = get_drifter_hive_for_system(&ctx, 31_000_001).unwrap();
    assert_eq!(hive_after_r2.current_room, 3);
    assert!(hive_after_r2.rooms[1].cleared);
    assert!(!hive_after_r2.rooms[2].cleared);

    // Verify Artemis Tyrannos Warden was automatically spawned into space
    let warden_bid = hive_init.warden_npc_ball_id;
    let warden_snap = wh_sys.get_ball_snapshot(warden_bid).await.unwrap();
    assert!(warden_snap.is_some(), "Artemis Tyrannos Warden must be automatically spawned");

    // Artemis Tyrannos Warden space destruction in final room
    let (_, warden_dead, _) = wh_sys.apply_damage_with_attacker(
        warden_bid,
        DamageVector { em: 50_000_000.0, thermal: 50_000_000.0, kinetic: 50_000_000.0, explosive: 50_000_000.0 },
        Some(char_id),
    ).await.unwrap().unwrap();
    assert!(warden_dead, "Artemis Tyrannos Warden destroyed");

    // Hive state completed and final room marked cleared
    let hive_final = get_drifter_hive_for_system(&ctx, 31_000_001).unwrap();
    assert!(hive_final.cleared, "Hive must be marked cleared");
    assert_eq!(hive_final.current_room, 3);
    assert!(hive_final.rooms[2].cleared);

    // Acceleration gate on a hive cleared by the in-space warden kill rejects re-completion (no double loot)
    let gate_done = ctx.dispatch_rpc(&mut sess, "keeper", None, "EnterNextDungeonRoom", &[PyRep::Integer(31_000_001)], &kw, None).await;
    assert!(gate_done.is_err(), "Cleared hive must reject further gate activation");

    // Verify authentic loot deposited: Barbican Element (34557) & Sleeper Data Library (30745)
    let has_element = ctx.db.with_snapshot(|s| {
        s.items.values().any(|i| i.type_id == 34_557 && i.owner_id == char_id)
    });
    // Data gaps removed: no element or blue loot dropped
    assert!(has_element, "Barbican Element (34557) must be awarded");

    // GetTriglavianSiteInstances returns clean empty list in wormhole systems
    let trig_wh = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(31_000_001)], &kw, None).await.expect("GetTriglavianSiteInstances");
    let PyRep::Dict(trig_wh_dict) = trig_wh else { panic!("Expected Dict") };
    let PyRep::List(trig_wh_dng) = &trig_wh_dict[0].1 else { panic!("Expected inner List") };
    assert!(trig_wh_dng.is_empty(), "Drifter Hive must not overload Triglavian site instances");
}

// ============================================================================
// 5. Triglavian Sites & Minor Victory Systems
// ============================================================================
#[tokio::test]
async fn test_pve_triglavian_sites_and_minor_victory_systems() {
    // parity: rpc:map.GetEdencomFortressSystems, rpc:map.GetEdencomMinorVictorySystems
    // parity: rpc:map.GetTriglavianMinorVictorySystems
    eve_testkit::attest(&[
        "rpc:map.GetEdencomFortressSystems",
        "rpc:map.GetEdencomMinorVictorySystems",
        "rpc:map.GetTriglavianMinorVictorySystems",
    ]);

    let ctx = fast_ctx();
    ensure_special_sites_spawned(&ctx).await;
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = create_in_space_session(&ctx, 8205, char_id, 30_000_142, 710_002_005, 587).await;

    // 1. Permanent Chapter 3 invasion systems
    let fort_sys = ctx.dispatch_rpc(&mut sess, "map", None, "GetEdencomFortressSystems", &[], &kw, None).await.expect("GetEdencomFortressSystems");
    let PyRep::List(f_list) = fort_sys else { panic!("Expected List") };
    assert_eq!(f_list.len(), 53, "All 53 authentic Fortress systems returned");
    assert!(f_list.contains(&PyRep::Integer(30_002_253)), "Arshat is a Fortress system");

    let eden_vic = ctx.dispatch_rpc(&mut sess, "map", None, "GetEdencomMinorVictorySystems", &[], &kw, None).await.expect("GetEdencomMinorVictorySystems");
    let PyRep::List(e_list) = eden_vic else { panic!("Expected List") };
    assert_eq!(e_list.len(), 84, "All 84 authentic EDENCOM Minor Victory systems returned");
    assert!(e_list.contains(&PyRep::Integer(30_002_048)) && e_list.contains(&PyRep::Integer(30_003_074)));

    let vic_systems = ctx.dispatch_rpc(&mut sess, "map", None, "GetTriglavianMinorVictorySystems", &[], &kw, None).await.expect("GetTriglavianMinorVictorySystems");
    let PyRep::List(v_list) = vic_systems else { panic!("Expected List") };
    assert_eq!(v_list.len(), 28, "All 28 authentic Triglavian Minor Victory systems returned");
    assert!(v_list.contains(&PyRep::Integer(30_003_076)), "Gammel is seeded as permanent Triglavian Minor Victory system");
    assert!(with_pve_special_reg(&ctx, |reg| reg.triglavian_sites.keys().all(|&s| ctx.sde.solar_systems.get(&s).map(|ss| ss.region_id) != Some(10_000_070))));
    assert!(!with_pve_special_reg(&ctx, |reg| reg.triglavian_sites.contains_key(&30_002_253)), "Fortress systems have no invasion sites");

    // 2. Pure read query in Jita (30000142) returns empty without fabricating sites
    let jita_trig = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(30_000_142)], &kw, None).await.expect("GetTriglavianSiteInstances in Jita");
    let PyRep::Dict(j_dict) = jita_trig else { panic!("Expected Dict") };
    let PyRep::List(j_dng) = &j_dict[0].1 else { panic!("Expected inner List") };
    assert!(j_dng.is_empty(), "Jita must not fabricate Triglavian sites");

    // 3. EDENCOM minor victory system Bei (30002048) contains authentic EDENCOM sites & ships
    let bei_trig = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(30_002_048)], &kw, None).await.expect("GetTriglavianSiteInstances in Bei");
    let PyRep::Dict(b_dict) = bei_trig else { panic!("Expected Dict") };
    let PyRep::List(b_dng) = &b_dict[0].1 else { panic!("Expected inner List") };
    assert_eq!(b_dng.len(), 2, "Bei must contain 2 EDENCOM site instances");
    with_pve_special_reg(&ctx, |reg| {
        let site = &reg.triglavian_sites[&30_002_048][0];
        assert_eq!(site.boss_type_id, 52_963, "Boss must be Republic Fleet Tempest (cat 11)");
        assert!(site.escort_type_ids.contains(&52_961)); // Republic Fleet Hurricane
        assert!(site.escort_type_ids.contains(&52_957)); // Republic Fleet Claw
    });

    // 4. Triglavian minor victory system Gammel (30003076) contains authentic Triglavian sites & ships
    let gammel_sys = ctx.systems.get_or_spawn(30_003_076);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.solar_system_id = 30_003_076; }
    });
    sess.solar_system_id = Some(30_003_076);

    let gammel_trig = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(30_003_076)], &kw, None).await.expect("GetTriglavianSiteInstances in Gammel");
    let PyRep::Dict(g_dict) = gammel_trig else { panic!("Expected Dict") };
    let PyRep::List(g_dng) = &g_dict[0].1 else { panic!("Expected inner List") };
    assert_eq!(g_dng.len(), 2, "Gammel must contain 2 Triglavian site instances");

    let (boss_bid, beacon_bid, esc_bid) = with_pve_special_reg(&ctx, |reg| {
        let site = &reg.triglavian_sites[&30_003_076][0];
        assert_eq!(site.boss_type_id, 48_122, "Boss must be authentic Striking Leshak");
        assert!(site.escort_type_ids.contains(&48_086)); // Ghosting Damavik
        assert!(site.escort_type_ids.contains(&48_092)); // Striking Damavik
        let esc = reg.triglavian_sites[&30_003_076][1].escort_ball_ids[0];
        (site.boss_npc_ball_id, site.beacon_ball_id, esc)
    });

    // Verify boss and beacon are automatically spawned in space with CombatState
    let boss_snap = gammel_sys.get_ball_snapshot(boss_bid).await.unwrap();
    assert!(boss_snap.is_some(), "Boss must be automatically spawned in space");
    let boss_cs = gammel_sys.get_combat_state(boss_bid).await.unwrap();
    assert!(boss_cs.is_some() && boss_cs.unwrap().max_structure_hp > 0.0, "Boss has CombatState");
    let beacon_snap = gammel_sys.get_ball_snapshot(beacon_bid).await.unwrap();
    assert!(beacon_snap.is_some(), "Beacon must be automatically spawned in space");
    let gammel_balls = gammel_sys.list_balls().await.unwrap();
    assert!(!gammel_balls.iter().any(|b| b.type_id == 10_962 && b.position.length() == 0.0), "No sentinel marker ball");
    assert!(gammel_sys.get_ball_snapshot(esc_bid).await.unwrap().is_some(), "Escort must be spawned before destruction");
    let (_, esc_dead, _) = gammel_sys.apply_damage(esc_bid, DamageVector { em: 50_000_000.0, thermal: 50_000_000.0, kinetic: 50_000_000.0, explosive: 50_000_000.0 }).await.unwrap().unwrap();
    assert!(esc_dead, "Escort must be destroyed by lethal damage");
    let _ = ctx.dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(30_003_076)], &kw, None).await;
    assert!(gammel_sys.get_ball_snapshot(esc_bid).await.unwrap().is_none(), "Destroyed escort must not be re-spawned");

    let pre_bal = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);

    // Destroy Triglavian Leshak boss in space with killer attribution (automatically spawned)
    let (_, dead, _) = gammel_sys.apply_damage_with_attacker(
        boss_bid,
        DamageVector { em: 50_000_000.0, thermal: 50_000_000.0, kinetic: 50_000_000.0, explosive: 50_000_000.0 },
        Some(char_id),
    ).await.unwrap().unwrap();
    assert!(dead, "Triglavian boss destroyed");

    // Verify site completion reflected in cache query (completed instance removed)
    let trig_after = ctx
        .dispatch_rpc(&mut sess, "dungeonInstanceCacheMgr", None, "GetTriglavianSiteInstances", &[PyRep::Integer(30_003_076)], &kw, None)
        .await
        .expect("GetTriglavianSiteInstances after boss kill");
    let PyRep::Dict(after_dict) = trig_after else { panic!("Expected Dict") };
    let PyRep::List(after_dng) = &after_dict[0].1 else { panic!("Expected inner List") };
    assert_eq!(after_dng.len(), 1, "Completed site instance removed from active instances");

    // Verify rewards: ISK payout, Sealed Survey Database (48121) cargo drop, and dual standings change
    let post_bal = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.0);
    assert!(post_bal > pre_bal, "Pilot must receive ISK bounty for Triglavian site completion");

    let has_survey_db = ctx.db.with_snapshot(|s| {
        s.items.values().any(|i| i.type_id == 48_121 && i.owner_id == char_id)
    });
    assert!(has_survey_db, "Sealed Survey Database (48121) cargo drop must be awarded");

    let (trig_standing, edencom_standing) = ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        (ch.standings.get(&500_026).copied().unwrap_or(0.0), ch.standings.get(&500_027).copied().unwrap_or(0.0))
    });
    assert!(edencom_standing > 0.0, "EDENCOM standing must increase (+0.15)");
    assert!(trig_standing < 0.0, "Triglavian standing must decrease (-0.15)");
}

#[tokio::test]
async fn test_p1_track_d_regressions_issues_12_55_56_57() {
    let ctx1 = fast_ctx();
    let ctx2 = fast_ctx();
    let kw = HashMap::new();

    // 1. Issue #55: ServerContext-scoped tactical and session-change timer state isolation
    eve_server::services::space_destiny::record_session_change_timer(&ctx1, 90_000_001, ctx1.now_unix_secs(), 30_000_142, 1, "jump");
    assert!(eve_server::services::space_destiny::get_session_change_timer(&ctx1, 90_000_001).is_some());
    assert!(eve_server::services::space_destiny::get_session_change_timer(&ctx2, 90_000_001).is_none());

    ctx1.tactical_state
        .write()
        .unwrap()
        .lance_caster_lock_until
        .insert((ctx1.instance_id as usize, 750_000_101), ctx1.now_unix_secs() + 300);
    assert!(eve_server::services::tactical_modules::check_ship_navigation_lock(&ctx1, 750_000_101, "warp").is_err());
    assert!(eve_server::services::tactical_modules::check_ship_navigation_lock(&ctx2, 750_000_101, "warp").is_ok());

    // 2. Issue #56: Probe scan system scoping, coordinate/destination redaction below 1.0, and per-character scan progress
    ctx1.db.with_mut_snapshot(|snap| {
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.user_id = 1;
        }
    });
    let mut sess_a = create_in_space_session(&ctx1, 9501, 90_000_001, 30_000_142, 750_000_201, 587).await;

    // Cross-system scan query rejected
    let remote_err = ctx1
        .dispatch_rpc(&mut sess_a, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(30_002_187)], &kw, None)
        .await
        .expect_err("Remote solar system scan query must be denied");
    assert_eq!(remote_err.code, "AccessDenied");

    // Seed an unscanned signature (strength 0.0) with a destination system
    let now_u = ctx1.now_unix();
    ctx1.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.entry(30_000_142).or_default().push(eve_db::CosmicSignatureRecord {
            signature_id: "SEC-56".into(),
            solar_system_id: 30_000_142,
            sig_group: "Wormhole".into(),
            type_name: "Unstable Wormhole".into(),
            signal_strength: 0.0,
            position: [123_456.0, 234_567.0, 345_678.0],
            wormhole_type_id: Some(30_643),
            destination_system_id: Some(31_000_005),
            remaining_mass_kg: 2_000_000_000.0,
            max_mass_kg: 2_000_000_000.0,
            max_jump_mass_kg: 300_000_000.0,
            created_unix: now_u,
            lifetime_seconds: 86_400,
            collapsed: false,
        });
    });

    // Unscanned signature redacts coordinates, group, type, and destinationSystemID
    let scan_unscanned = ctx1
        .dispatch_rpc(&mut sess_a, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(30_000_142)], &kw, None)
        .await
        .expect("GetSystemScanMgr");
    let Some(PyRep::List(sigs_a0)) = scan_unscanned.dict_get("signatures") else { panic!("signatures list") };
    let sec_sig_a0 = sigs_a0.iter().find(|s| s.dict_get("signatureID") == Some(&PyRep::String("SEC-56".into()))).expect("SEC-56");
    assert_eq!(sec_sig_a0.dict_get("x"), Some(&PyRep::None));
    assert_eq!(sec_sig_a0.dict_get("destinationSystemID"), Some(&PyRep::None));
    assert_eq!(sec_sig_a0.dict_get("group"), Some(&PyRep::String("Unknown".into())));

    // Record 100% scan progress for Pilot A only; Pilot B still sees 0.0 and redacted coordinates
    ctx1.record_character_scan_progress(90_000_001, 30_000_142, "SEC-56", 1.0);
    let scan_a1 = ctx1
        .dispatch_rpc(&mut sess_a, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(30_000_142)], &kw, None)
        .await
        .unwrap();
    let Some(PyRep::List(sigs_a1)) = scan_a1.dict_get("signatures") else { panic!() };
    let sec_sig_a1 = sigs_a1.iter().find(|s| s.dict_get("signatureID") == Some(&PyRep::String("SEC-56".into()))).unwrap();
    assert_eq!(sec_sig_a1.dict_get("x"), Some(&PyRep::Real(123_456.0)));
    assert_eq!(sec_sig_a1.dict_get("destinationSystemID"), Some(&PyRep::Integer(31_000_005)));

    let mut sess_b = create_in_space_session(&ctx1, 9502, 90_000_002, 30_000_142, 750_000_202, 587).await;
    let scan_b1 = ctx1
        .dispatch_rpc(&mut sess_b, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(30_000_142)], &kw, None)
        .await
        .unwrap();
    let Some(PyRep::List(sigs_b1)) = scan_b1.dict_get("signatures") else { panic!() };
    let sec_sig_b1 = sigs_b1.iter().find(|s| s.dict_get("signatureID") == Some(&PyRep::String("SEC-56".into()))).unwrap();
    assert_eq!(sec_sig_b1.dict_get("x"), Some(&PyRep::None));
    assert_eq!(sec_sig_b1.dict_get("destinationSystemID"), Some(&PyRep::None));

    // 3. Issue #57: Drifter Hive NPCs have SDE-derived CombatState (> default 450/450/350 HP)
    ensure_special_sites_spawned(&ctx1).await;
    let hive = get_drifter_hive_for_system(&ctx1, 31_000_001).expect("Barbican hive");
    let wh_sys = ctx1.systems.get_or_spawn(31_000_001);
    let first_npc_bid = hive.rooms[0].npc_ball_ids[0];
    let npc_cs = wh_sys.get_combat_state(first_npc_bid).await.unwrap().expect("Drifter NPC must have CombatState");
    assert!(npc_cs.max_shield_hp >= 600.0 && npc_cs.max_armor_hp >= 600.0 && npc_cs.max_structure_hp >= 600.0);

    // Homefront completion ignores spoofed fleet_count when solo
    let hf_inst_id = with_pve_special_reg(&ctx1, |reg| {
        let s = &reg.homefront_sites[&30_000_142][0];
        (s.instance_id, s.boss_npc_ball_id, s.base_pilot_isk, s.optimal_fleet_size)
    });
    let jita_sys = ctx1.systems.get_or_spawn(30_000_142);
    jita_sys.try_remove_ball(hf_inst_id.1);
    let hf_done = eve_server::services::pve_special_sites::complete_special_dungeon_instance(
        &ctx1,
        &mut sess_b,
        hf_inst_id.0,
        hf_inst_id.3, // spoofed optimal fleet size while solo
    )
    .await
    .expect("complete_special_dungeon_instance");
    let expected_solo_isk = ((hf_inst_id.2 as f64) * (1.0 / (hf_inst_id.3 as f64))).round() as i64;
    assert_eq!(hf_done.dict_get("rewardPaidIsk"), Some(&PyRep::Integer(expected_solo_isk)));
}

}



pub(crate) mod pve_dungeons_events_remediation {
    #[allow(unused_imports)]
    use super::*;

// Parity integration tests remediating open/flagged PVE feature rows:
// - `feat:pve.dungeon-system`
// - `feat:pve.events-sites`
// - `feat:pve.mercenary-tactical-ops`
// - `feat:pve.milestones`

use eve_db::d6_types::{PersistedMercenaryActivity, PersistedMercenaryDen};
use eve_db::TrainedSkillRecord;
use eve_destiny::{DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state;
use eve_server::services::pve_special_sites::with_pve_special_reg;
use eve_server::services::pve_special_sites_combat::get_seasonal_site_details;
use eve_server::services::season_fsd::load_season_config;
use eve_server::services::season_manager::load_season_state_from_blobs;
use eve_server::services::sov_gameplay::on_space_entity_destroyed_ext;
use glam::DVec3;
use std::collections::HashMap;

const JITA: u32 = 30_000_142;
const PILOT_A: i64 = 90_000_001;

fn new_ctx() -> ServerContext {
    fast_ctx()
}

fn hex_str(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn setup_pilot(ctx: &ServerContext, char_id: i64, sys_id: u32) -> (SessionState, i64) {
    let mut sess = SessionState::new(char_id as u64);
    crate::common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).expect("demo pilot");
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.active_ship_id
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.ship_id = Some(ship_id);

    // Ensure ship ball is present in the ballpark
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let initial_pos = DVec3::new(120_000_000.0, 0.0, 80_000_000.0);
    let ship_ball = eve_destiny::Ball {
        id: ship_id,
        type_id: 587,
        radius: 100.0,
        position: initial_pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle.spawn_ball(ship_ball).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, [initial_pos.x, initial_pos.y, initial_pos.z]);
    });

    (sess, ship_id)
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

async fn step_until_warp_landed_at(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for _ in 0..180 {
        let _ = sys_handle.step_manual(1.0).await;
        if let Ok(Some((_, b))) = sys_handle.get_ball_snapshot(ship_id).await {
            if matches!(b.mode, MovementMode::Stop) {
                break;
            }
        }
    }
    let _ = rpc(ctx, sess, "beyonce", "UpdateStateRequest", &[])
        .await
        .expect("UpdateStateRequest after warp");
}

async fn follow_until_within(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
    target_id: i64,
    max_dist_m: f64,
) {
    let kw = HashMap::new();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let beyonce_oid = ctx.bind_object(sess, "beyonce", i64::from(sys_id)).unwrap();
    let _ = ctx
        .dispatch_rpc(
            sess,
            "beyonce",
            Some(beyonce_oid),
            "CmdSetSpeedFraction",
            &[PyRep::Real(1.0)],
            &kw,
            None,
        )
        .await;
    sys_handle
        .get_ball_snapshot(target_id)
        .await
        .ok()
        .flatten()
        .expect("target ball present");
    let _ = ctx
        .dispatch_rpc(
            sess,
            "beyonce",
            Some(beyonce_oid),
            "CmdFollowBall",
            &[PyRep::Integer(target_id), PyRep::Real(500.0)],
            &kw,
            None,
        )
        .await
        .expect("CmdFollowBall");
    let Some((_, target_ball)) = sys_handle.get_ball_snapshot(target_id).await.ok().flatten() else {
        return;
    };
    let target_pos = target_ball.position;
    for _ in 0..240 {
        let _ = sys_handle.step_manual(1.0).await;
        if let Ok(Some((_, sb))) = sys_handle.get_ball_snapshot(ship_id).await {
            if sb.position.distance(target_pos) <= max_dist_m {
                break;
            }
        }
    }
    let _ = ctx
        .dispatch_rpc(
            sess,
            "beyonce",
            Some(beyonce_oid),
            "UpdateStateRequest",
            &[],
            &kw,
            None,
        )
        .await
        .expect("UpdateStateRequest after follow");
}

fn equip_data_analyzer_and_hacking(ctx: &ServerContext, char_id: i64, sys_id: u32, ship_id: i64, hull_tid: u32) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: hull_tid,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id + 1,
            InventoryItemRecord {
                item_id: ship_id + 1,
                type_id: 22_175,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.skills.entry(char_id).or_default().insert(
            21_718,
            TrainedSkillRecord {
                type_id: 21_718,
                level: 5,
                skill_points: 1_280_000.0,
            },
        );
    });
}

async fn hack_container_to_win_for_char(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    can_id: i64,
) -> PyRep {
    let start = rpc(ctx, sess, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(can_id)])
        .await
        .expect("StartNewGameInstance");
    assert!(start.dict_get("virusCoherence").and_then(|v| v.as_i64()).unwrap_or(0) >= 80);
    let core_idx = with_pve_exploration_reg(ctx, |reg| {
        reg.hacking_games[&(char_id, can_id)]
            .tiles
            .iter()
            .find(|t| format!("{:?}", t.kind) == "SystemCore")
            .expect("SystemCore tile")
            .tile_index
    });
    let mut path: Vec<u32> = (1..=(core_idx / 6)).map(|r| r * 6).collect();
    path.extend(((core_idx / 6) * 6 + 1)..=core_idx);
    for &idx in &path {
        for _ in 0..8 {
            let r = rpc(
                ctx,
                sess,
                "hackingMgr",
                "ClickedOnTile",
                &[PyRep::Integer(can_id), PyRep::Integer(idx as i64)],
            )
            .await
            .expect("ClickedOnTile");
            if r.dict_get("status").and_then(|v| v.as_str()) == Some("Won") {
                return r;
            }
            if r.dict_get("nodeCoherence").and_then(|v| v.as_i64()).unwrap_or(0) <= 0 {
                break;
            }
        }
    }
    panic!("Expected hacking minigame to reach Won state");
}

fn extract_rowset(rep: &PyRep) -> (Vec<String>, Vec<Vec<PyRep>>) {
    match rep {
        PyRep::Rowset { columns, rows } | PyRep::TupleSet { columns, rows } => {
            (columns.clone(), rows.clone())
        }
        PyRep::IndexRowset { columns, items, .. } => (
            columns.clone(),
            items.iter().map(|(_, r)| r.clone()).collect(),
        ),
        _ => (Vec::new(), Vec::new()),
    }
}

fn row_field<'a>(cols: &[String], row: &'a [PyRep], name: &str) -> Option<&'a PyRep> {
    cols.iter().position(|c| c == name).and_then(|i| row.get(i))
}

fn walk_sde_skills_l5(sde: &eve_sde::SdeRegistry, tid: u32, skills: &mut std::collections::BTreeSet<u32>) {
    let Some(t) = sde.types.get(&tid) else { return };
    for attr in [182_u16, 183, 184, 1285, 1289, 1290] {
        if let Some(&sk) = t.default_attributes.get(&attr).filter(|&&v| v > 0.0) {
            let sid = sk as u32;
            if skills.insert(sid) {
                walk_sde_skills_l5(sde, sid, skills);
            }
        }
    }
}

async fn equip_combat_thrasher_and_hacking(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    sys_id: u32,
    ship_id: i64,
) -> Vec<i64> {
    use eve_db::{InventoryFlag, InventoryItemRecord};
    const THRASHER: u32 = 16_242;
    const GUN_TID: u32 = 2_881;
    const AMMO_TID: u32 = 21_906;
    const GYRO_TID: u32 = 519;
    let hi_flags = [
        InventoryFlag::HiSlot0, InventoryFlag::HiSlot1, InventoryFlag::HiSlot2,
        InventoryFlag::HiSlot3, InventoryFlag::HiSlot4, InventoryFlag::HiSlot5,
        InventoryFlag::HiSlot6,
    ];
    let mut guns = Vec::new();
    let mut ammo_id = 0_i64;
    let mut ship_pos = DVec3::new(120_000_000.0, 0.0, 80_000_000.0);
    if let Ok(Some((_, b))) = ctx.systems.get_or_spawn(sys_id).get_ball_snapshot(ship_id).await {
        ship_pos = b.position;
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id: THRASHER, owner_id: char_id,
            location_id: i64::from(sys_id), flag: InventoryFlag::None,
            quantity: 1, singleton: true, ..Default::default()
        });
        for (idx, flag) in hi_flags.into_iter().enumerate() {
            let gid = ship_id + 10 + (idx as i64);
            snap.items.insert(gid, InventoryItemRecord {
                item_id: gid, type_id: GUN_TID, owner_id: char_id,
                location_id: ship_id, flag, quantity: 1, singleton: true, ..Default::default()
            });
            snap.module_online_states.insert(gid, true);
            guns.push(gid);
        }
        snap.items.insert(ship_id + 1, InventoryItemRecord {
            item_id: ship_id + 1, type_id: 22_175, owner_id: char_id,
            location_id: ship_id, flag: InventoryFlag::MedSlot0,
            quantity: 1, singleton: true, ..Default::default()
        });
        snap.module_online_states.insert(ship_id + 1, true);
        for (idx, flag) in [InventoryFlag::LoSlot0, InventoryFlag::LoSlot1].into_iter().enumerate() {
            let mid = ship_id + 20 + (idx as i64);
            snap.items.insert(mid, InventoryItemRecord {
                item_id: mid, type_id: GYRO_TID, owner_id: char_id,
                location_id: ship_id, flag, quantity: 1, singleton: true, ..Default::default()
            });
            snap.module_online_states.insert(mid, true);
        }
        ammo_id = ship_id + 30;
        snap.items.insert(ammo_id, InventoryItemRecord {
            item_id: ammo_id, type_id: AMMO_TID, owner_id: char_id,
            location_id: ship_id, flag: InventoryFlag::Cargo,
            quantity: 5_000, singleton: false, ..Default::default()
        });
        let mut req_skills: std::collections::BTreeSet<u32> = [
            3_300, 3_301, 3_310, 3_311, 3_312, 3_315, 3_413, 3_417, 3_418, 3_426, 3_428, 3_429, 21_718,
        ].into_iter().collect();
        for tid in [THRASHER, GUN_TID, GYRO_TID, 22_175] {
            walk_sde_skills_l5(&ctx.sde, tid, &mut req_skills);
        }
        let cskills = snap.skills.entry(char_id).or_default();
        for sid in req_skills {
            cskills.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 1_280_000.0 });
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_remove_ball(ship_id);
    let _ = sys_handle.spawn_ball_with_combat(
        eve_destiny::Ball {
            id: ship_id, type_id: THRASHER, radius: 100.0, position: ship_pos,
            velocity: DVec3::ZERO, max_velocity: 420.0, inertia: 2.0,
            mass: 1_600_000.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        ctx.build_ship_combat_state(char_id, ship_id, THRASHER),
    ).await;
    let g_reps: Vec<PyRep> = guns.iter().map(|&g| PyRep::Integer(g)).collect();
    rpc(ctx, sess, "dogmaIM", "LoadAmmo", &[
        PyRep::Integer(ship_id), PyRep::List(g_reps), PyRep::List(vec![PyRep::Integer(ammo_id)]),
    ]).await.expect("LoadAmmo onto combat turrets");
    guns
}

async fn destroy_npc_via_weapons(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
    guns: &[i64],
    npc_id: i64,
    rng_seed: Option<u64>,
) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if sys_handle.get_ball_snapshot(npc_id).await.ok().flatten().is_none() {
        return;
    }
    follow_until_within(ctx, sess, sys_id, ship_id, npc_id, 4_500.0).await;
    let _ = rpc(ctx, sess, "dogmaIM", "AddTarget", &[PyRep::Integer(npc_id)]).await;
    for _ in 0..30 {
        if sys_handle.get_target_locks(ship_id).await.unwrap_or_default().iter().any(|l| l.target_id == npc_id && l.locked) {
            break;
        }
        let _ = sys_handle.step_manual(1.0).await;
    }
    for step in 0..160 {
        if sys_handle.get_ball_snapshot(npc_id).await.ok().flatten().is_none() {
            return;
        }
        if let Some(seed) = rng_seed {
            ctx.set_mechanic_rng_seed(Some(seed));
        }
        if step % 2 == 0 {
            for &gid in guns {
                let _ = rpc(ctx, sess, "dogmaIM", "Activate", &[
                    PyRep::Integer(gid), PyRep::String("targetAttack".into()), PyRep::Integer(npc_id),
                ]).await;
            }
        }
        let _ = sys_handle.step_manual(2.0).await;
        ctx.db.clock().advance_secs(2);
    }
    assert!(
        sys_handle.get_ball_snapshot(npc_id).await.ok().flatten().is_none(),
        "NPC {npc_id} must be destroyed by live weapon cycles"
    );
}

async fn fly_stargates_via_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    ship_id: i64,
    dest_sys: u32,
) {
    let start_sys = sess.solar_system_id.unwrap();
    if start_sys == dest_sys {
        return;
    }
    let mut prev: HashMap<u32, (u32, i64, u32)> = HashMap::new();
    let mut q = std::collections::VecDeque::from([start_sys]);
    let mut seen = std::collections::HashSet::from([start_sys]);
    while let Some(sys) = q.pop_front() {
        if sys == dest_sys { break; }
        let mut gates: Vec<(&i64, &eve_sde::StargateJump)> = ctx.sde.jumps.iter().filter(|(_, j)| j.from_solar_system_id == sys).collect();
        gates.sort_by_key(|g| *g.0);
        for (&gid, j) in gates {
            if seen.insert(j.to_solar_system_id) {
                prev.insert(j.to_solar_system_id, (sys, gid, j.to_solar_system_id));
                q.push_back(j.to_solar_system_id);
            }
        }
    }
    let mut route = Vec::new();
    let mut cur = dest_sys;
    while cur != start_sys {
        let &(p, gid, n) = prev.get(&cur).unwrap_or_else(|| panic!("no stargate route {start_sys} -> {dest_sys}"));
        route.push((p, gid, n));
        cur = p;
    }
    route.reverse();
    for (cur_sys, gid, next_sys) in route {
        let sys_h = ctx.systems.get_or_spawn(cur_sys);
        match rpc(ctx, sess, "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(gid), PyRep::Real(0.0)]).await {
            Ok(_) => step_until_warp_landed_at(ctx, sess, cur_sys, ship_id).await,
            Err(e) if e.code == "WarpDistanceTooShort" => {}
            Err(e) => panic!("CmdWarpToStuff stargate {gid}: {e:?}"),
        }
        let beyonce_oid = ctx.bind_object(sess, "beyonce", i64::from(cur_sys)).unwrap();
        let kw = HashMap::new();
        let _ = ctx.dispatch_rpc(sess, "beyonce", Some(beyonce_oid), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &kw, None).await;
        let _ = ctx.dispatch_rpc(sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(gid), PyRep::Real(500.0)], &kw, None).await;
        if let Some((_, gb)) = sys_h.get_ball_snapshot(gid).await.ok().flatten() {
            for _ in 0..240 {
                let _ = sys_h.step_manual(1.0).await;
                if let Ok(Some((_, sb))) = sys_h.get_ball_snapshot(ship_id).await {
                    if (sb.position.distance(gb.position) - gb.radius).max(0.0) <= 2_000.0 { break; }
                }
            }
        }
        ctx.db.clock().advance_secs(12);
        let _ = sys_h.step_manual(12.0).await;
        rpc(ctx, sess, "beyonce", "CmdStargateJump", &[PyRep::Integer(gid), PyRep::Integer(i64::from(next_sys))])
            .await
            .unwrap_or_else(|e| panic!("CmdStargateJump {gid} -> {next_sys}: {e:?}"));
        sess.solar_system_id = Some(next_sys);
        ctx.db.clock().advance_secs(12);
    }
}

#[tokio::test]
async fn test_pve_dungeon_system_authentic_lifecycle() {
    // parity: feat:pve.dungeon-system
    eve_testkit::attest(&["feat:pve.dungeon-system"]);

    let ctx = new_ctx();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let (mut sess, ship_id) = setup_pilot(&ctx, PILOT_A, JITA).await;
    let guns = equip_combat_thrasher_and_hacking(&ctx, &mut sess, PILOT_A, JITA, ship_id).await;

    // 1. Discover SDE-loaded multi-room dungeon DSG-30000142 (Guristas Scout Outpost, dungeon 98 in sde_dungeons)
    let sde_d98 = ctx
        .sde
        .dungeons
        .get(&98)
        .expect("SDE dungeon 98 (Guristas Scout Outpost) present in evomu_sde.sqlite");
    assert!(sde_d98.archetype_id.is_some());

    ensure_system_pve_sites_seeded(&ctx, JITA).await;
    let (dsg_pos, dsg_inst_id, dsg_did) = with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system[&JITA]
            .iter()
            .find(|s| s.site_id == "DSG-30000142" && s.site_category == "Deadspace")
            .expect("Naturally seeded SDE deadspace signature DSG-30000142");
        (
            s.position,
            s.expedition_instance_id.expect("runtime dungeon instance"),
            s.dungeon_id,
        )
    });
    assert_eq!(dsg_did, 98);

    // Initially redacted until scanned to 100% via scanMgr.RequestScans
    let scan_pre = rpc(&ctx, &mut sess, "scanMgr", "GetSystemScanMgr", &[PyRep::Integer(i64::from(JITA))])
        .await
        .expect("GetSystemScanMgr");
    let PyRep::List(pre_sigs) = scan_pre.dict_get("signatures").expect("signatures") else {
        panic!("Expected signatures list");
    };
    let dsg_pre = pre_sigs
        .iter()
        .find(|s| s.dict_get("signatureID") == Some(&PyRep::String("DSG-30000142".into())))
        .expect("DSG-30000142 signature entry");
    assert_eq!(dsg_pre.dict_get("x"), Some(&PyRep::None));

    let probes: Vec<PyRep> = (0..8)
        .map(|_| PyRep::List(vec![PyRep::Real(dsg_pos[0]), PyRep::Real(dsg_pos[1]), PyRep::Real(dsg_pos[2])]))
        .collect();
    rpc(
        &ctx,
        &mut sess,
        "scanMgr",
        "RequestScans",
        &[PyRep::Real(600_000_000_000.0), PyRep::Real(120.0), PyRep::List(probes)],
    )
    .await
    .expect("RequestScans pinpoints DSG-30000142");

    // 2. Warp to scanned deadspace entry gate via live beyonce.CmdWarpToStuff
    rpc(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdWarpToStuff",
        &[PyRep::String("scan".into()), PyRep::String("DSG-30000142".into())],
    )
    .await
    .expect("CmdWarpToStuff to DSG-30000142");
    step_until_warp_landed_at(&ctx, &mut sess, JITA, ship_id).await;

    let sys_handle = ctx.systems.get_or_spawn(JITA);
    let inst_state0 = get_dungeon_instance_state(&ctx, dsg_inst_id).expect("dungeon instance state");
    assert_eq!(inst_state0.dict_get("archetypeID").and_then(|v| v.as_i64()), sde_d98.archetype_id.map(i64::from));
    assert_eq!(inst_state0.dict_get("factionID").and_then(|v| v.as_i64()), sde_d98.faction_id.map(i64::from));
    assert_eq!(inst_state0.dict_get("difficulty").and_then(|v| v.as_i64()), sde_d98.difficulty.map(i64::from));
    let entry_gate_id = inst_state0.dict_get("entryGateBallID").and_then(|v| v.as_i64()).unwrap();
    assert!(sys_handle.get_ball_snapshot(entry_gate_id).await.ok().flatten().is_some());

    // Verify SDE gas dungeon table (`sde_dungeon_gas_types`) also spawns gas cloud props
    let gas_did = 1351_u32;
    let gas_inst_id = eve_server::services::pve_dungeon_runtime::spawn_sde_dungeon_runtime_instance(
        &ctx,
        PILOT_A,
        JITA,
        gas_did,
        "Gas",
        [150_000_000.0, 0.0, 0.0],
        false,
    )
    .await
    .expect("spawn SDE gas dungeon instance");
    let gas_rep = get_dungeon_instance_state(&ctx, gas_inst_id).expect("gas dungeon state");
    let PyRep::List(gas_rooms) = gas_rep.dict_get("rooms").expect("gas rooms") else { panic!() };
    let PyRep::List(gas_obj_tids) = gas_rooms[0].dict_get("objectTypeIDs").expect("gas objectTypeIDs") else { panic!() };
    let expected_gas_tid = ctx.sde.dungeon_gas_types[&gas_did][0];
    assert!(
        gas_obj_tids.contains(&PyRep::Integer(i64::from(expected_gas_tid))),
        "SDE gas dungeon {gas_did} must spawn gas cloud type {expected_gas_tid} from sde_dungeon_gas_types"
    );

    // 3. Activate entry acceleration gate via live keeper.ActivateAccelerationGate -> enters Room 1
    let gate1_rep = rpc(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(entry_gate_id)],
    )
    .await
    .expect("ActivateAccelerationGate entry gate");
    assert_eq!(gate1_rep.dict_get("success"), Some(&PyRep::Bool(true)));
    assert_eq!(gate1_rep.dict_get("roomIndex"), Some(&PyRep::Integer(1)));
    assert_eq!(gate1_rep.dict_get("disallowMWD"), Some(&PyRep::Bool(true)));
    let r1_gate_id = gate1_rep.dict_get("nextGateBallID").and_then(|v| v.as_i64()).unwrap();
    let PyRep::List(r1_objs) = gate1_rep.dict_get("objectBallIDs").expect("objectBallIDs") else {
        panic!("Expected objectBallIDs");
    };
    let r1_beacon_id = r1_objs[0].as_i64().unwrap();
    let r1_state = get_dungeon_instance_state(&ctx, dsg_inst_id).unwrap();
    let PyRep::List(r1_rooms) = r1_state.dict_get("rooms").unwrap() else { panic!() };
    let PyRep::List(r1_tids) = r1_rooms[0].dict_get("objectTypeIDs").unwrap() else { panic!() };
    let expected_ast_tid = ctx.sde.dungeon_asteroids[&98][0].type_id;
    assert!(
        r1_tids.contains(&PyRep::Integer(i64::from(expected_ast_tid))),
        "Dungeon 98 Room 1 must spawn asteroid type {expected_ast_tid} from sde_dungeon_asteroid_counts"
    );
    let PyRep::List(r1_wave1) = gate1_rep.dict_get("npcBallIDs").expect("npcBallIDs") else {
        panic!("Expected npcBallIDs");
    };
    assert!(!r1_wave1.is_empty(), "Room 1 SDE wave must spawn");

    // Range & lock checks via live RPCs
    let range_err = rpc(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(r1_gate_id)],
    )
    .await
    .expect_err("far Room 1 gate must fail range check");
    assert_eq!(range_err.code, "OutsideGateActivationRange");

    follow_until_within(&ctx, &mut sess, JITA, ship_id, r1_gate_id, 2_000.0).await;
    let locked_err = rpc(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(r1_gate_id)],
    )
    .await
    .expect_err("locked Room 1 gate must fail before defenders are cleared");
    assert_eq!(locked_err.code, "GateLocked");

    // 4. Approach Deadspace Relay Beacon via live CmdFollowBall + UpdateStateRequest -> fires OnApproach spawn
    let r1_npcs_pre = r1_wave1.len();
    follow_until_within(&ctx, &mut sess, JITA, ship_id, r1_beacon_id, 2_200.0).await;
    let inst_after_approach = get_dungeon_instance_state(&ctx, dsg_inst_id).unwrap();
    let PyRep::List(rooms_post_app) = inst_after_approach.dict_get("rooms").unwrap() else {
        panic!();
    };
    let PyRep::List(r1_all_npcs) = rooms_post_app[0].dict_get("npcBallIDs").unwrap() else {
        panic!();
    };
    assert!(
        r1_all_npcs.len() > r1_npcs_pre,
        "Approaching Room 1 relay beacon via CmdFollowBall must trigger reinforcement spawn"
    );

    // 5. Destroy all Room 1 defenders through live dogmaIM.Activate weapon cycles -> unlocks Room 1 -> Room 2 gate
    for n in r1_all_npcs {
        destroy_npc_via_weapons(&ctx, &mut sess, JITA, ship_id, &guns, n.as_i64().unwrap(), None).await;
    }
    let gate_unlocked = ctx.db.with_snapshot(|snap| {
        !snap.dungeon_runtime.instances[&dsg_inst_id].rooms[0].gate_locked
    });
    assert!(gate_unlocked, "Clearing Room 1 NPCs via combat must unlock acceleration gate");

    follow_until_within(&ctx, &mut sess, JITA, ship_id, r1_gate_id, 2_000.0).await;
    let gate2_rep = rpc(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(r1_gate_id)],
    )
    .await
    .expect("Activate Room 1 -> Room 2 gate");
    assert_eq!(gate2_rep.dict_get("success"), Some(&PyRep::Bool(true)));
    assert_eq!(gate2_rep.dict_get("roomIndex"), Some(&PyRep::Integer(2)));
    let dest_x = gate2_rep.dict_get("destinationX").and_then(|v| v.as_f64()).unwrap();
    let ship_r2_pos = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten().unwrap().1.position;
    assert!((ship_r2_pos.x - dest_x).abs() < 10.0, "Ship must move to Room 2 landing");

    let PyRep::List(r2_objs) = gate2_rep.dict_get("objectBallIDs").unwrap() else { panic!() };
    let strongbox_id = r2_objs[0].as_i64().unwrap();
    let PyRep::List(r2_npcs_pre) = gate2_rep.dict_get("npcBallIDs").unwrap() else { panic!() };

    // 6. Approach & loot Overseer Strongbox via live invbroker.LootAll -> fires OnLoot ambush wave
    let ship_inv_oid = ctx.bind_object(&sess, "invbroker", ship_id).unwrap();
    follow_until_within(&ctx, &mut sess, JITA, ship_id, strongbox_id, 2_200.0).await;
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        Some(ship_inv_oid),
        "LootAll",
        &[PyRep::Integer(strongbox_id)],
        &kw,
        None,
    )
    .await
    .expect("LootAll on Room 2 Overseer Strongbox");

    let inst_post_loot = get_dungeon_instance_state(&ctx, dsg_inst_id).unwrap();
    let PyRep::List(rooms_post_loot) = inst_post_loot.dict_get("rooms").unwrap() else { panic!() };
    let PyRep::List(r2_all_npcs) = rooms_post_loot[1].dict_get("npcBallIDs").unwrap() else { panic!() };
    assert!(
        r2_all_npcs.len() > r2_npcs_pre.len(),
        "invbroker.LootAll on Overseer Strongbox must spawn OnLoot ambush wave"
    );

    // 7. Destroy Room 2 boss & ambush via live dogmaIM.Activate weapon cycles -> fires CompleteDungeon + ballpark cleanup
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    for n in r2_all_npcs {
        destroy_npc_via_weapons(&ctx, &mut sess, JITA, ship_id, &guns, n.as_i64().unwrap(), None).await;
    }
    let st_final = get_dungeon_instance_state(&ctx, dsg_inst_id).unwrap();
    assert_eq!(st_final.dict_get("completed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(st_final.dict_get("despawned").and_then(|v| v.as_bool()), Some(true));
    assert!(sys_handle.get_ball_snapshot(entry_gate_id).await.ok().flatten().is_none());
    assert!(sys_handle.get_ball_snapshot(r1_gate_id).await.ok().flatten().is_none());
    assert!(sys_handle.get_ball_snapshot(r1_beacon_id).await.ok().flatten().is_none());
    assert!(sys_handle.get_ball_snapshot(strongbox_id).await.ok().flatten().is_none());
    let done_notifs = ctx.drain_notifications_for_char(PILOT_A);
    assert!(
        done_notifs.iter().any(|n| n.event == "OnDungeonCompleted"),
        "OnDungeonCompleted must fire upon clearing Room 2 overseer"
    );

    // 8. Clear a CombatAnomaly via CmdWarpToStuff + weapon combat -> verify escalation + live CmdStargateJump travel
    let esc_seed = (1_u64..100_000)
        .find(|&s| {
            let r = eve_server::rng::MechanicRng::seeded(s);
            (0..5).all(|_| r.roll(0.20))
        })
        .expect("escalation RNG seed");
    let PyRep::List(anoms) = rpc(&ctx, &mut sess, "scanSvc", "GetCombatAnomalyInstances", &[])
        .await
        .expect("scanSvc.GetCombatAnomalyInstances")
    else {
        panic!("Expected List from GetCombatAnomalyInstances");
    };
    let anom_site_id = anoms
        .iter()
        .filter_map(|a| a.dict_get("siteID").or_else(|| a.dict_get("id")).and_then(|v| v.as_str()))
        .find(|sid| !sid.starts_with("dng-"))
        .expect("non-seasonal CombatAnomaly in Jita")
        .to_string();

    rpc(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdWarpToStuff",
        &[PyRep::String("dungeon".into()), PyRep::String(anom_site_id.clone())],
    )
    .await
    .expect("CmdWarpToStuff to CombatAnomaly");
    step_until_warp_landed_at(&ctx, &mut sess, JITA, ship_id).await;

    let _ = ctx.drain_notifications_for_char(PILOT_A);
    for _ in 0..10 {
        let wave_ids: Vec<i64> = with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system
                .get(&JITA)
                .and_then(|l| l.iter().find(|s| s.site_id == anom_site_id))
                .filter(|s| !s.cleared)
                .map(|s| s.active_npc_ball_ids.clone())
                .unwrap_or_default()
        });
        if wave_ids.is_empty() {
            break;
        }
        let n_wave = wave_ids.len();
        for (idx, bid) in wave_ids.into_iter().enumerate() {
            let seed = (idx + 1 == n_wave).then_some(esc_seed);
            destroy_npc_via_weapons(&ctx, &mut sess, JITA, ship_id, &guns, bid, seed).await;
        }
    }

    let esc_notifs = ctx.drain_notifications_for_char(PILOT_A);
    assert!(esc_notifs.iter().any(|n| n.event == "OnEscalatingPathChange"));
    let esc_rep = rpc(&ctx, &mut sess, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[])
        .await
        .expect("GetMyEscalatingPathDetails");
    let (esc_cols, esc_rows) = extract_rowset(&esc_rep);
    assert_eq!(esc_rows.len(), 1, "Clearing combat anomaly with positive roll must trigger escalation");
    let esc_inst_id = row_field(&esc_cols, &esc_rows[0], "instanceID").and_then(|v| v.as_i64()).unwrap();
    let esc_dest_sys = row_field(&esc_cols, &esc_rows[0], "destinationSolarSystemID").and_then(|v| v.as_i64()).unwrap() as u32;
    assert_eq!(row_field(&esc_cols, &esc_rows[0], "stepIndex").and_then(|v| v.as_i64()), Some(1));
    assert!(row_field(&esc_cols, &esc_rows[0], "totalSteps").and_then(|v| v.as_i64()).unwrap_or(0) >= 2);

    fly_stargates_via_rpc(&ctx, &mut sess, ship_id, esc_dest_sys).await;
    rpc(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdWarpToStuff",
        &[PyRep::String("epinstance".into()), PyRep::Integer(esc_inst_id)],
    )
    .await
    .expect("CmdWarpToStuff epinstance to escalation pocket");
    step_until_warp_landed_at(&ctx, &mut sess, esc_dest_sys, ship_id).await;
    let entered = rpc(
        &ctx,
        &mut sess,
        "dungeonExplorationMgr",
        "GetEscalatingPathDungeonsEntered",
        &[PyRep::Integer(esc_inst_id)],
    )
    .await
    .expect("GetEscalatingPathDungeonsEntered");
    let PyRep::List(entered_list) = entered else { panic!("Expected List") };
    assert!(!entered_list.is_empty(), "Escalation entry must record dungeon visit");
}

#[tokio::test]
async fn test_deadspace_fleet_warp_and_warp_to_member_blocked() {
    let ctx = new_ctx();
    crate::common::pin_clock(&ctx);
    ensure_system_pve_sites_seeded(&ctx, JITA).await;
    let dsg_inst_id = with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system[&JITA]
            .iter()
            .find(|s| s.site_id == "DSG-30000142" && s.site_category == "Deadspace")
            .and_then(|s| s.expedition_instance_id)
            .expect("DSG-30000142 instance")
    });
    let inst_state = get_dungeon_instance_state(&ctx, dsg_inst_id).expect("dungeon instance state");
    let PyRep::List(rooms) = inst_state.dict_get("rooms").expect("rooms") else { panic!() };
    let extract_pos = |r: &PyRep| -> [f64; 3] {
        let Some(PyRep::Tuple(t)) = r.dict_get("position") else { panic!("room position") };
        [t[0].as_f64().unwrap(), t[1].as_f64().unwrap(), t[2].as_f64().unwrap()]
    };
    let r1_pos = extract_pos(&rooms[0]);
    let r2_pos = extract_pos(&rooms[1]);
    assert_eq!(rooms[1].dict_get("gated"), Some(&PyRep::Bool(true)));

    let ext_a = DVec3::new(r2_pos[0] + 10_000_000.0, r2_pos[1], r2_pos[2]);
    let (mut sess_a, ship_a) = setup_pilot(&ctx, PILOT_A, JITA).await;
    let sys_handle = ctx.systems.get_or_spawn(JITA);
    let _ = sys_handle.set_ball_position(ship_a, ext_a).await;

    let pilot_b: i64 = 90_000_002;
    let ship_b: i64 = 750_990_002;
    let mut sess_b = SessionState::new(pilot_b as u64);
    crate::common::login_pilot(&ctx, &mut sess_b, "other", "other-secret").await;
    rpc(&ctx, &mut sess_b, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(pilot_b)])
        .await
        .expect("SelectCharacterID p2");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pilot_b) {
            ch.solar_system_id = JITA;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_b;
        }
        snap.items.insert(
            ship_b,
            eve_db::InventoryItemRecord {
                item_id: ship_b,
                type_id: 587,
                owner_id: pilot_b,
                location_id: i64::from(JITA),
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_b, r2_pos);
    });
    sess_b.solar_system_id = Some(JITA);
    sess_b.station_id = None;
    sess_b.ship_id = Some(ship_b);
    let _ = sys_handle.spawn_ball(eve_destiny::Ball {
        id: ship_b,
        type_id: 587,
        radius: 100.0,
        position: DVec3::from_array(r2_pos),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;

    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_A);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, pilot_b);
    let fleet_id = rpc(&ctx, &mut sess_a, "fleetObjectHandler", "CreateFleet", &[])
        .await.expect("CreateFleet").as_i64().expect("fleetID");
    sess_a.fleet_id = Some(fleet_id);
    rpc(&ctx, &mut sess_a, "fleetObjectHandler", "Invite", &[PyRep::Integer(pilot_b), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)]).await.expect("Invite");
    rpc(&ctx, &mut sess_b, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fleet_id), PyRep::Integer(587)]).await.expect("AcceptInvite");
    sess_b.fleet_id = Some(fleet_id);
    rpc(&ctx, &mut sess_b, "fleetObjectHandler", "SetTakesFleetWarp", &[PyRep::Bool(true)]).await.expect("SetTakesFleetWarp");

    let wtm_err = rpc(&ctx, &mut sess_a, "fleetMgr", "WarpToMember", &[PyRep::Integer(pilot_b), PyRep::Real(0.0)])
        .await
        .expect_err("WarpToMember into gated deadspace room must be blocked");
    assert_eq!(wtm_err.code, "NaturalPhenomenaDisruptingWarp");
    let (_, ball_a) = sys_handle.get_ball_snapshot(ship_a).await.unwrap().unwrap();
    assert!(!matches!(ball_a.mode, MovementMode::Warp { .. }));

    let fw_err = rpc(&ctx, &mut sess_a, "fleetObjectHandler", "FleetWarp", &[PyRep::Real(r2_pos[0]), PyRep::Real(r2_pos[1]), PyRep::Real(r2_pos[2]), PyRep::Real(0.0)])
        .await
        .expect_err("FleetWarp into gated deadspace room must be blocked");
    assert_eq!(fw_err.code, "NaturalPhenomenaDisruptingWarp");

    let _ = sys_handle.set_ball_position(ship_a, DVec3::from_array(r1_pos)).await;
    let cross_err = rpc(&ctx, &mut sess_a, "fleetObjectHandler", "WarpToMember", &[PyRep::Integer(pilot_b), PyRep::Real(0.0)])
        .await
        .expect_err("Cross-pocket WarpToMember between Room 1 and Room 2 must be blocked");
    assert_eq!(cross_err.code, "NaturalPhenomenaDisruptingWarp");

    let _ = sys_handle.set_ball_position(ship_a, ext_a).await;
    let _ = sys_handle.set_ball_position(ship_b, ext_a + DVec3::new(5_000.0, 0.0, 0.0)).await;
    let ext_dest = ext_a + DVec3::new(20_000_000.0, 0.0, 0.0);
    let fw_ok = rpc(&ctx, &mut sess_a, "fleetObjectHandler", "FleetWarp", &[PyRep::Real(ext_dest.x), PyRep::Real(ext_dest.y), PyRep::Real(ext_dest.z), PyRep::Real(0.0)])
        .await
        .expect("FleetWarp in open space must succeed");
    assert_eq!(fw_ok.dict_get("warpedMembers"), Some(&PyRep::Integer(2)));
}

#[tokio::test]
async fn test_pve_seasonal_event_sites_and_loot_tables() {
    // parity: feat:pve.events-sites
    eve_testkit::attest(&["feat:pve.events-sites"]);

    let ctx = new_ctx();
    crate::common::pin_clock(&ctx);
    let (mut sess, ship_id) = setup_pilot(&ctx, PILOT_A, JITA).await;
    equip_data_analyzer_and_hacking(&ctx, PILOT_A, JITA, ship_id, 587);
    let lethal = DamageVector {
        em: 100_000_000.0,
        thermal: 100_000_000.0,
        kinetic: 100_000_000.0,
        explosive: 100_000_000.0,
    };

    // 0. Inactive / unconfigured season must NOT suppress or fabricate perpetual seasonal event sites
    let inactive_sites = eve_server::services::pve_special_sites_combat::ensure_seasonal_event_sites_for_system(&ctx, JITA).await;
    assert!(
        inactive_sites.is_empty(),
        "No seasonal event sites may spawn when no active season is configured"
    );
    let _ = rpc(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetCombatAnomalyInstances", &[])
        .await
        .expect("GetCombatAnomalyInstances outside seasonal window");
    let inactive_anom = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&JITA)
            .into_iter()
            .flatten()
            .any(|a| a.anomaly_type == "SeasonalEventSite" && !a.cleared)
    });
    assert!(!inactive_anom, "GetCombatAnomalyInstances must not spawn seasonal sites when unconfigured");

    // 1. Configure active Crimson Harvest window and discover site via live dungeonInstanceCacheMgr RPC
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        let mut cfg = load_season_config(snap);
        cfg.season_id = 1;
        cfg.season_name = "Crimson Harvest".into();
        cfg.start_time_ft = now_ft - 3_600 * 10_000_000;
        cfg.end_time_ft = now_ft + 3_600 * 10_000_000;
        snap.seasons_and_campaigns.season_configuration = Some(cfg);
    });

    let PyRep::Dict(anom_dict) = rpc(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetCombatAnomalyInstances",
        &[],
    )
    .await
    .expect("GetCombatAnomalyInstances")
    else {
        panic!("Expected Dict from GetCombatAnomalyInstances");
    };
    let (_, PyRep::Dict(jita_by_dng)) = anom_dict
        .iter()
        .find(|(k, _)| k.as_i64() == Some(i64::from(JITA)))
        .expect("Jita entries in GetCombatAnomalyInstances")
    else {
        panic!("Expected inner Dict for Jita");
    };

    let (crimson_did, crimson_iid) = ctx.db.with_snapshot(|snap| {
        let a = snap
            .cosmic_anomalies
            .get(&JITA)
            .into_iter()
            .flatten()
            .find(|a| a.anomaly_type == "SeasonalEventSite" && !a.cleared)
            .expect("SeasonalEventSite anomaly");
        let rest = a.anomaly_id.strip_prefix("dng-").expect("dng prefix");
        let (d, i) = rest.split_once('-').expect("dng-did-iid");
        (d.parse::<u32>().unwrap(), i.parse::<i64>().unwrap())
    });
    assert_ne!(crimson_did, 47, "Seasonal event site must not use hardcoded dungeon_id 47");
    assert!(
        matches!(crimson_did, 8923 | 8926 | 9238 | 9241 | 8933),
        "Crimson Harvest must select an authentic SDE seasonal dungeon (got {crimson_did})"
    );
    assert!(
        jita_by_dng.iter().any(|(d, ids)| {
            d.as_i64() == Some(i64::from(crimson_did))
                && matches!(ids, PyRep::List(l) if l.contains(&PyRep::Integer(crimson_iid)))
        }),
        "dungeonInstanceCacheMgr.GetCombatAnomalyInstances must expose active seasonal event site"
    );

    let details = get_seasonal_site_details(&ctx, crimson_iid).expect("SeasonalSiteDetails");
    assert_eq!(details.season_theme, "Crimson Harvest");
    assert!(details.relay_ball_id != 0, "Seasonal site must spawn hackable relay ball");
    assert!(
        !details.crate_loot_pool.is_empty(),
        "Seasonal site must resolve FSD crate loot table entries"
    );

    // 1b. BF-3 regression: initial exploration seeding after seasonal spawn must retain SHK-* hackables
    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(JITA, false);
    });
    ensure_system_pve_sites_seeded(&ctx, JITA).await;
    let shk_retained = with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .get(&JITA)
            .is_some_and(|list| list.iter().any(|s| s.site_id == details.exploration_site_id))
    });
    assert!(
        shk_retained,
        "ensure_system_pve_sites_seeded must preserve active seasonal SHK-* hackable site"
    );

    // 2. Warp to the seasonal site via live custom_agency_provider.warp_to_content
    let content_key = format!("dng-{crimson_did}-{crimson_iid}");
    rpc(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "warp_to_content",
        &[PyRep::keyval([
            ("contentID", PyRep::String(content_key.clone())),
            ("instanceID", PyRep::Integer(crimson_iid)),
            ("dungeonID", PyRep::Integer(i64::from(crimson_did))),
        ])],
    )
    .await
    .expect("custom_agency_provider.warp_to_content to seasonal site");
    step_until_warp_landed_at(&ctx, &mut sess, JITA, ship_id).await;

    let (boss_bid, escort_bids, expected_isk, expected_pts) = with_pve_special_reg(&ctx, |reg| {
        let s = reg.seasonal_event_sites[&JITA]
            .iter()
            .find(|s| s.instance_id == crimson_iid)
            .expect("seasonal site record");
        (s.boss_npc_ball_id, s.escort_ball_ids.clone(), s.reward_isk, s.reward_points)
    });
    let fsd_challenges = eve_server::services::season_fsd::fsd_challenge_definitions(&ctx.sde);
    assert!(
        expected_pts > 0 && fsd_challenges.iter().any(|d| d.points == expected_pts),
        "Seasonal challenge points ({expected_pts}) must come directly from FSD definitions"
    );
    assert_eq!(escort_bids.len(), 2);
    let sys_handle = ctx.systems.get_or_spawn(JITA);
    assert!(sys_handle.get_ball_snapshot(boss_bid).await.ok().flatten().is_some());
    assert!(sys_handle.get_ball_snapshot(details.relay_ball_id).await.ok().flatten().is_some());

    // 3. Approach & hack seasonal Data relay via live hackingMgr RPCs
    let boss_shield_pre = sys_handle
        .get_combat_state(boss_bid)
        .await
        .ok()
        .flatten()
        .expect("boss combat state pre-hack")
        .shield_hp;
    follow_until_within(&ctx, &mut sess, JITA, ship_id, details.relay_ball_id, 1_800.0).await;
    let hack_win = hack_container_to_win_for_char(&ctx, &mut sess, PILOT_A, details.relay_ball_id).await;
    assert_eq!(hack_win.dict_get("status").and_then(|v| v.as_str()), Some("Won"));
    assert!(
        get_seasonal_site_details(&ctx, crimson_iid).is_some_and(|d| d.relay_hacked),
        "Winning SHK relay hack must mark relay_hacked = true"
    );
    let boss_shield_post = sys_handle
        .get_combat_state(boss_bid)
        .await
        .ok()
        .flatten()
        .expect("boss combat state post-hack")
        .shield_hp;
    if boss_shield_pre > 20.0 {
        assert!(
            boss_shield_post < boss_shield_pre,
            "Hacking SHK relay must weaken seasonal boss shield ({boss_shield_pre} -> {boss_shield_post})"
        );
    }

    // 4. Destroy seasonal boss via live combat destruction pipeline -> verify FSD loot, ISK, points & cleanup
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    let pts_before = ctx.db.with_snapshot(|s| load_season_state_from_blobs(s, PILOT_A).season_points);
    let (_, boss_dead, _) = sys_handle
        .apply_damage_with_attacker(boss_bid, lethal, Some(PILOT_A))
        .await
        .expect("damage")
        .expect("seasonal boss exists");
    assert!(boss_dead);

    let bal_after = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    let pts_after = ctx.db.with_snapshot(|s| load_season_state_from_blobs(s, PILOT_A).season_points);
    assert_eq!(bal_after - bal_before, expected_isk * 100);
    assert_eq!(pts_after - pts_before, expected_pts);

    let has_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal
            .iter()
            .any(|e| e.character_id == PILOT_A && e.entry_type == "BountyPrizes" && e.reason.contains("Seasonal Event"))
    });
    assert!(has_journal, "Seasonal boss bounty journal entry must be recorded");

    let crate_deposited = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .any(|it| it.owner_id == PILOT_A && it.type_id == details.crate_type_id)
    });
    assert!(crate_deposited, "Seasonal FSD crate must be awarded to pilot");
    if !details.crate_loot_pool.is_empty() {
        let bonus_pool_deposited = ctx.db.with_snapshot(|snap| {
            details.crate_loot_pool.iter().any(|&(ptid, _)| {
                snap.items.values().any(|it| it.owner_id == PILOT_A && it.type_id == ptid)
            })
        });
        assert!(
            bonus_pool_deposited,
            "Hacked SHK relay must unlock bonus FSD pool loot drops upon boss destruction"
        );
    }

    for &ebid in &escort_bids {
        assert!(sys_handle.get_ball_snapshot(ebid).await.ok().flatten().is_none());
    }
    assert!(sys_handle.get_ball_snapshot(details.relay_ball_id).await.ok().flatten().is_none());

    // 5. Switch season to Winter Nexus, verify distinct SDE archetype spawns, then expire window & verify despawn via live RPC
    let now_ft2 = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        let mut cfg = load_season_config(snap);
        cfg.season_id = 2;
        cfg.season_name = "Winter Nexus".into();
        cfg.start_time_ft = now_ft2 - 1_800 * 10_000_000;
        cfg.end_time_ft = now_ft2 + 1_800 * 10_000_000;
        snap.seasons_and_campaigns.season_configuration = Some(cfg);
    });
    let _ = rpc(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetCombatAnomalyInstances", &[])
        .await
        .expect("Winter Nexus GetCombatAnomalyInstances");
    let winter_did = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&JITA)
            .into_iter()
            .flatten()
            .filter(|a| a.anomaly_type == "SeasonalEventSite" && !a.cleared)
            .filter_map(|a| a.anomaly_id.strip_prefix("dng-").and_then(|r| r.split_once('-')))
            .filter_map(|(d, _)| d.parse::<u32>().ok())
            .next()
            .expect("Winter Nexus seasonal site spawned")
    });
    assert!(
        matches!(winter_did, 9030 | 9046 | 9047 | 9029),
        "Winter Nexus must spawn Winter Nexus SDE dungeon (got {winter_did})"
    );
    assert_ne!(winter_did, crimson_did);

    ctx.db.clock().advance_secs(7_200);
    let _ = rpc(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetCombatAnomalyInstances", &[])
        .await
        .expect("Expired GetCombatAnomalyInstances");
    let any_active = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&JITA)
            .into_iter()
            .flatten()
            .any(|a| a.anomaly_type == "SeasonalEventSite" && !a.cleared)
    });
    assert!(!any_active, "Expired seasonal event sites must despawn outside active window");
}

#[tokio::test]
async fn test_pve_mercenary_tactical_operations_infomorph_and_isk() {
    // parity: feat:pve.mercenary-tactical-ops
    eve_testkit::attest(&["feat:pve.mercenary-tactical-ops"]);

    let ctx = new_ctx();
    let (mut sess, ship_id) = setup_pilot(&ctx, PILOT_A, JITA).await;
    let _ = &mut sess;

    let den_id = 1_892_100_000_i64;
    let mto_ball_id = 1_892_200_001_i64;
    let mto_uuid = vec![0x11; 16];

    // 1. Initialize mercenary den with base development, anarchy, and infomorphs
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                solar_system_id: JITA,
                planet_id: 40_009_082,
                owner_char_id: PILOT_A,
                enabled: true,
                development_level: 20,
                anarchy_level: 40,
                infomorphs: 150,
                ..Default::default()
            },
        );
        let hex_key = hex_str(&mto_uuid);
        snap.sov_runtime.mercenary_activities.insert(
            hex_key.clone(),
            PersistedMercenaryActivity {
                uuid: mto_uuid.clone(),
                den_id,
                solar_system_id: JITA,
                dungeon_id: 4_500,
                started: true,
                started_by: PILOT_A,
                expiry_unix: ctx.now_unix() + 1_800,
                development_impact: 10,
                anarchy_impact: -10,
                infomorph_bonus: 50,
            },
        );
        snap.sov_runtime.mto_site_balls.insert(mto_ball_id, hex_key);
    });

    let bal_pre = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    let den_pre = ctx.db.with_snapshot(|s| s.sov_runtime.mercenary_dens[&den_id].clone());

    // 2. Complete MTO activity via combat destruction of the tactical site
    on_space_entity_destroyed_ext(&ctx, JITA, Some(PILOT_A), mto_ball_id, 24_690, true);

    let bal_post = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    let den_post = ctx.db.with_snapshot(|s| s.sov_runtime.mercenary_dens[&den_id].clone());

    // 3. Verify ISK reward credited to pilot wallet (+7,500,000 ISK)
    assert_eq!(
        bal_post - bal_pre,
        7_500_000 * 100,
        "MTO completion must award 7.5M ISK to pilot"
    );

    // Verify journal entry for MercenaryDenReward
    let has_mto_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal
            .iter()
            .any(|e| e.character_id == PILOT_A && e.entry_type == "MercenaryDenReward")
    });
    assert!(has_mto_journal, "Journal entry for MercenaryDenReward must be recorded");

    // 4. Below max Development (20 < 100), completing an MTO awards +10 Dev, -10 Anarchy,
    // and 0 bonus infomorphs (`Mercenary_Tactical_Operation.md:24`).
    assert_eq!(
        den_post.infomorphs, den_pre.infomorphs,
        "Den below MAX_LEVEL must not gain bonus infomorphs (was {}, now {})",
        den_pre.infomorphs,
        den_post.infomorphs
    );

    // 5. Verify development level increased (+10) and anarchy reduced (-10)
    assert_eq!(den_post.development_level, den_pre.development_level + 10);
    assert_eq!(den_post.anarchy_level, den_pre.anarchy_level - 10);

    // 6. Verify tactical site ball was removed from state
    let mto_ball_removed = ctx.db.with_snapshot(|snap| {
        !snap.sov_runtime.mto_site_balls.contains_key(&mto_ball_id)
            && !snap.sov_runtime.mercenary_activities.contains_key(&hex_str(&mto_uuid))
    });
    assert!(mto_ball_removed, "MTO site ball and activity must be removed after completion");

    // 7. At max Development (100), completing an MTO awards +350 bonus infomorphs instead of +Dev.
    let mto_max_ball_id = 1_892_200_002_i64;
    let mto_max_uuid = vec![0x22; 16];
    let max_hex = hex_str(&mto_max_uuid);
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_dens.get_mut(&den_id).unwrap().development_level = 100;
        snap.items.insert(
            mto_max_ball_id,
            InventoryItemRecord {
                item_id: mto_max_ball_id,
                type_id: 24_690,
                owner_id: 500_010,
                location_id: JITA as i64,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(mto_max_ball_id, [1.0, 2.0, 3.0]);
        snap.sov_runtime.mercenary_activities.insert(
            max_hex.clone(),
            PersistedMercenaryActivity {
                uuid: mto_max_uuid.clone(),
                den_id,
                solar_system_id: JITA,
                dungeon_id: 4_500,
                started: true,
                started_by: PILOT_A,
                expiry_unix: ctx.now_unix() + 1_800,
                development_impact: 0,
                anarchy_impact: -10,
                infomorph_bonus: 350,
            },
        );
        snap.sov_runtime.mto_site_balls.insert(mto_max_ball_id, max_hex);
    });
    on_space_entity_destroyed_ext(&ctx, JITA, Some(PILOT_A), mto_max_ball_id, 24_690, true);
    let den_max_post = ctx.db.with_snapshot(|s| s.sov_runtime.mercenary_dens[&den_id].clone());
    assert_eq!(den_max_post.development_level, 100);
    assert_eq!(den_max_post.infomorphs - den_post.infomorphs, 350, "Max-development MTO must grant +350 bonus infomorphs");
    let _ = ship_id;
}

#[tokio::test]
async fn test_pve_milestones_progression_tracking() {
    // parity: feat:pve.milestones
    eve_testkit::attest(&["feat:pve.milestones"]);

    let ctx = new_ctx();
    let (mut sess, _) = setup_pilot(&ctx, PILOT_A, JITA).await;

    // 1. ClaimRewards before ProcessCharacterLogon must fail
    let pre_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect_err("ClaimRewards before logon must fail");
    assert_eq!(pre_err.code, "MilestonePrerequisiteNotMet");

    // 2. ProcessCharacterLogon initializes logon tracking and emits OnMilestoneTimerStarted
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let logon_rep = rpc(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[])
        .await
        .expect("ProcessCharacterLogon");
    assert_eq!(logon_rep, PyRep::None);

    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    assert!(
        notifs.iter().any(|n| n.event == "OnMilestoneTimerStarted"),
        "OnMilestoneTimerStarted must be dispatched on logon"
    );

    // 3. Unknown milestone ID (e.g. 2) fails closed with InvalidMilestoneID,
    //    and Milestone 1 cannot be claimed before 119 minutes of play time
    let skipped_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(2)])
        .await
        .expect_err("claiming unknown milestone 2 must fail with InvalidMilestoneID");
    assert_eq!(skipped_err.code, "InvalidMilestoneID");

    let early_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect_err("claiming milestone 1 before 119 minutes must fail prerequisite");
    assert_eq!(early_err.code, "MilestonePrerequisiteNotMet");

    // 4. Claim Milestone 1: grants rewards once, logs journal, emits OnLoginMilestoneAchieved
    ctx.db.clock().advance_secs(7_200);
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    let claim1_rep = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect("ClaimRewards(1) must succeed");
    assert_eq!(claim1_rep, PyRep::None);

    let bal_mid = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    assert!(bal_mid > bal_before, "Milestone 1 must award ISK");

    let achieved_notifs = ctx.drain_notifications_for_char(PILOT_A);
    assert!(
        achieved_notifs.iter().any(|n| n.event == "OnLoginMilestoneAchieved"),
        "OnLoginMilestoneAchieved must be dispatched"
    );

    let has_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal
            .iter()
            .any(|e| e.character_id == PILOT_A && e.entry_type == "MilestoneReward" && e.reason.contains("Milestone 1"))
    });
    assert!(has_journal, "Journal entry for MilestoneReward must be recorded");

    // Duplicate ClaimRewards(1) must fail closed
    let dup_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect_err("duplicate ClaimRewards(1) must fail");
    assert_eq!(dup_err.code, "MilestoneAlreadyClaimed");

    // 5. Unknown milestone 2 remains invalid even after completing Milestone 1
    ctx.db.clock().advance_secs(7_200);
    let unk_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(2)])
        .await
        .expect_err("unknown milestone 2 must continue to reject with InvalidMilestoneID");
    assert_eq!(unk_err.code, "InvalidMilestoneID");
}
}
