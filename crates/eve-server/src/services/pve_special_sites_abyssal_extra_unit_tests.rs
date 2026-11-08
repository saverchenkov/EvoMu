//! Extra branch-coverage and mutation-killing unit tests for `pve_special_sites_abyssal`.

#![forbid(unsafe_code)]

use std::sync::{Arc, OnceLock};

use crate::router::ServerContext;
use crate::services::pve_special_sites::with_abyssal_state;
use crate::services::pve_special_sites_abyssal::{
    abyssal_mutaplasmid_for_tier, abyssal_room_gate_type, classify_abyssal_filament,
    ensure_abyssal_room_balls_for_ctx, enter_abyssal_pocket_for_character,
    enter_abyssal_pocket_with_trace, handle_abyssal_entrance_deployment,
    on_abyssal_cache_destroyed_core, on_abyssal_room_npc_destroyed_core,
    on_abyssal_room_npc_destroyed_in_ballpark, pocket_grid_offset, spawn_abyssal_room_contents,
    AbyssalWeatherKind, BIOADAPTIVE_CACHE_WRECK_TYPE_ID,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, Ballpark, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn test_weather_as_str_and_all_filament_tiers_mutaplasmids_and_gates() {
    assert_eq!(AbyssalWeatherKind::Electrical.as_str(), "Electrical");
    assert_eq!(AbyssalWeatherKind::Dark.as_str(), "Dark");
    assert_eq!(AbyssalWeatherKind::Exotic.as_str(), "Exotic");
    assert_eq!(AbyssalWeatherKind::Firestorm.as_str(), "Firestorm");
    assert_eq!(AbyssalWeatherKind::Gamma.as_str(), "Gamma");

    let expected = [
        // Tier 0
        (56131, 0, AbyssalWeatherKind::Electrical),
        (56132, 0, AbyssalWeatherKind::Dark),
        (56133, 0, AbyssalWeatherKind::Exotic),
        (56134, 0, AbyssalWeatherKind::Firestorm),
        (56136, 0, AbyssalWeatherKind::Gamma),
        // Tier 1
        (47761, 1, AbyssalWeatherKind::Exotic),
        (47762, 1, AbyssalWeatherKind::Dark),
        (47763, 1, AbyssalWeatherKind::Firestorm),
        (47764, 1, AbyssalWeatherKind::Gamma),
        (47765, 1, AbyssalWeatherKind::Electrical),
        // Tier 2
        (47888, 2, AbyssalWeatherKind::Exotic),
        (47892, 2, AbyssalWeatherKind::Dark),
        (47896, 2, AbyssalWeatherKind::Firestorm),
        (47900, 2, AbyssalWeatherKind::Gamma),
        (47904, 2, AbyssalWeatherKind::Electrical),
        // Tier 3
        (47889, 3, AbyssalWeatherKind::Exotic),
        (47893, 3, AbyssalWeatherKind::Dark),
        (47897, 3, AbyssalWeatherKind::Firestorm),
        (47901, 3, AbyssalWeatherKind::Gamma),
        (47905, 3, AbyssalWeatherKind::Electrical),
        // Tier 4
        (47890, 4, AbyssalWeatherKind::Exotic),
        (47894, 4, AbyssalWeatherKind::Dark),
        (47898, 4, AbyssalWeatherKind::Firestorm),
        (47902, 4, AbyssalWeatherKind::Gamma),
        (47906, 4, AbyssalWeatherKind::Electrical),
        // Tier 5
        (47891, 5, AbyssalWeatherKind::Exotic),
        (47895, 5, AbyssalWeatherKind::Dark),
        (47899, 5, AbyssalWeatherKind::Firestorm),
        (47903, 5, AbyssalWeatherKind::Gamma),
        (47907, 5, AbyssalWeatherKind::Electrical),
        // Tier 6
        (56139, 6, AbyssalWeatherKind::Electrical),
        (56140, 6, AbyssalWeatherKind::Dark),
        (56141, 6, AbyssalWeatherKind::Exotic),
        (56142, 6, AbyssalWeatherKind::Firestorm),
        (56143, 6, AbyssalWeatherKind::Gamma),
    ];
    for (tid, tier, weather) in expected {
        assert_eq!(classify_abyssal_filament(tid), Some((tier, weather)));
    }
    assert_eq!(classify_abyssal_filament(0), None);
    assert_eq!(classify_abyssal_filament(56135), None);

    // Boundary checks on abyssal_mutaplasmid_for_tier (0..=2 -> 47699, 3..=4 -> 47701, 5+ -> 47700)
    for t in [0, 1, 2] {
        assert_eq!(abyssal_mutaplasmid_for_tier(t), 47_699);
    }
    for t in [3, 4] {
        assert_eq!(abyssal_mutaplasmid_for_tier(t), 47_701);
    }
    for t in [5, 6, 7] {
        assert_eq!(abyssal_mutaplasmid_for_tier(t), 47_700);
    }

    // Boundary checks on abyssal_room_gate_type (< 3 -> 47685, >= 3 -> 47686)
    assert_eq!(abyssal_room_gate_type(0), 47_685);
    assert_eq!(abyssal_room_gate_type(2), 47_685);
    assert_eq!(abyssal_room_gate_type(3), 47_686);
    assert_eq!(abyssal_room_gate_type(4), 47_686);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_abyssal_deployment_errors_trace_fleet_join_ballpark_unlock_and_cache_tiers() {
    let origin_sys: SolarSystemId = 30_000_142;
    let char1: i64 = 90_150_001;
    let char2: i64 = 90_150_002;
    let ship1: i64 = 1_150_000_001;
    let ship2: i64 = 1_150_000_002;
    let filament_item: i64 = 1_150_000_500;

    let sde = shared_sde();
    let frigate_tid: u32 = 587;

    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account("ab_p1", "pw", char1, "AbyssP1");
    db.provision_gm_account("ab_p2", "pw", char2, "AbyssP2");
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_700_000)));
    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid) in [(char1, ship1), (char2, ship2)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = origin_sys;
                ch.station_id = None;
                ch.active_ship_id = sid;
                ch.fleet_id = Some(8801);
            }
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: frigate_tid,
                    owner_id: cid,
                    location_id: origin_sys as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.fleets.insert(
            8801,
            eve_db::types::FleetRecord {
                fleet_id: 8801,
                boss_id: char1,
                members: vec![char1, char2],
                free_move: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            filament_item,
            InventoryItemRecord {
                item_id: filament_item,
                type_id: 56131, // Tier 0 Exotic (allowed in 0.9-1.0 highsec)
                owner_id: char1,
                location_id: ship1,
                flag: InventoryFlag::Cargo,
                quantity: 6,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        let chan_key = format!("solarsystemid2:{origin_sys}");
        let chan = eve_db::ChatChannelRecord {
            channel_id: chan_key.clone(),
            name: "Local".into(),
            motd: String::new(),
            owner_id: Some(0),
            members: std::collections::HashSet::from([char1]),
            is_system_local: true,
            solar_system_id: Some(origin_sys),
            is_private: false,
            operators: Default::default(),
            password: None,
            is_delayed_local: false,
        };
        snap.chat_channels.insert(chan_key, chan);
    });

    // Unknown pocket_grid_offset returns [0, 0, 0]
    assert_eq!(pocket_grid_offset(&ctx, 999_999_999), [0.0, 0.0, 0.0]);
    // ensure_abyssal_room_balls_for_ctx on character without pocket is a no-op
    ensure_abyssal_room_balls_for_ctx(&ctx, 999_999_999).await;

    let mut sess1 = SessionState::new(601);
    // 1. Unauthenticated error
    assert!(handle_abyssal_entrance_deployment(&ctx, &mut sess1, &[PyRep::Integer(filament_item)]).await.is_err());

    sess1.user_id = Some(1);
    sess1.character_id = Some(char1);
    // 2. Docked in station or structure error
    sess1.station_id = Some(60_003_760);
    assert!(handle_abyssal_entrance_deployment(&ctx, &mut sess1, &[PyRep::Integer(filament_item)]).await.is_err());
    sess1.station_id = None;
    sess1.structure_id = Some(1_000_000_000_001);
    assert!(handle_abyssal_entrance_deployment(&ctx, &mut sess1, &[PyRep::Integer(filament_item)]).await.is_err());
    sess1.structure_id = None;

    // 3. No active ship error
    assert!(handle_abyssal_entrance_deployment(&ctx, &mut sess1, &[PyRep::Integer(filament_item)]).await.is_err());
    sess1.ship_id = Some(ship1);

    // 4. No solar system error
    assert!(handle_abyssal_entrance_deployment(&ctx, &mut sess1, &[PyRep::Integer(filament_item)]).await.is_err());
    sess1.solar_system_id = Some(origin_sys);
    sess1.fleet_id = Some(8801);

    // 5. Missing or <= 0 filament argument error
    assert!(handle_abyssal_entrance_deployment(&ctx, &mut sess1, &[]).await.is_err());
    assert!(handle_abyssal_entrance_deployment(&ctx, &mut sess1, &[PyRep::Integer(0)]).await.is_err());

    // Spawn ship1 in origin system and deploy filament trace
    let origin_handle = ctx.systems.get_or_spawn(origin_sys);
    let _ = origin_handle
        .spawn_ball(Ball {
            id: ship1,
            type_id: frigate_tid,
            position: DVec3::new(60_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let dep_res = handle_abyssal_entrance_deployment(&ctx, &mut sess1, &[PyRep::Integer(filament_item)])
        .await
        .expect("deploy succeeds");
    let trace_id = dep_res.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    assert!(trace_id > 0);

    // Pilot 1 enters pocket with trace_id (Tier 5 -> cache_hp_mult = 25.0, subnode spawned)
    let (pid1, r1_npcs) = enter_abyssal_pocket_with_trace(
        &ctx,
        &mut sess1,
        char1,
        47891,
        5,
        origin_sys,
        Some(trace_id),
    )
    .await;
    assert!(pid1 > 0);
    assert_eq!(r1_npcs.len(), 4); // (2 + 5/2).clamp(2, 5) == 4

    // Pilot 2 joins the same pocket via the same trace_id (tests already_spawned == true & fleet join branch)
    let mut sess2 = SessionState::new(602);
    sess2.user_id = Some(2);
    sess2.character_id = Some(char2);
    sess2.solar_system_id = Some(origin_sys);
    sess2.ship_id = Some(ship2);
    sess2.fleet_id = Some(8801);
    let (pid2, r1_npcs_2) = enter_abyssal_pocket_with_trace(
        &ctx,
        &mut sess2,
        char2,
        47891,
        5,
        origin_sys,
        Some(trace_id),
    )
    .await;
    assert_eq!(pid1, pid2);
    assert_eq!(r1_npcs, r1_npcs_2);

    // A third pilot entering a separate pocket gets a distinct grid slot offset (slot 1 -> Y = 10_000_000.0)
    let mut sess3 = SessionState::new(603);
    sess3.user_id = Some(3);
    sess3.character_id = Some(90_150_003);
    sess3.solar_system_id = Some(origin_sys);
    sess3.ship_id = Some(1_150_000_003);
    let (pid3, _) = enter_abyssal_pocket_for_character(&ctx, &mut sess3, 90_150_003, 47889, 3, origin_sys).await;
    assert_ne!(pid1, pid3);
    assert_eq!(pocket_grid_offset(&ctx, pid3), [0.0, 10_000_000.0, 0.0]);

    // Test on_abyssal_room_npc_destroyed_in_ballpark in abyssal solar system (32_000_001)
    let ab_sys: SolarSystemId = 32_000_001;
    let mut bp = Ballpark::new(ab_sys);
    // While r1_npcs[1] is still in ballpark, killing r1_npcs[0] does NOT unlock gate
    bp.balls.insert(
        r1_npcs[1],
        Ball {
            id: r1_npcs[1],
            type_id: 48090,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    );
    assert!(!on_abyssal_room_npc_destroyed_in_ballpark(&mut bp, &ctx.db, ab_sys, r1_npcs[0]));

    // Once all other NPC balls are removed from ballpark, killing the final NPC unlocks the gate
    bp.balls.remove(&r1_npcs[1]);
    assert!(on_abyssal_room_npc_destroyed_in_ballpark(&mut bp, &ctx.db, ab_sys, r1_npcs[1]));
    // Calling again after room is already cleared returns false
    assert!(!on_abyssal_room_npc_destroyed_in_ballpark(&mut bp, &ctx.db, ab_sys, r1_npcs[1]));

    // ensure_abyssal_room_balls_for_ctx on cleared room emits SetBallInteractive(true)
    ensure_abyssal_room_balls_for_ctx(&ctx, char1).await;

    // Test on_abyssal_room_npc_destroyed_core when another NPC is still alive in pid3
    let pid3_npcs = with_abyssal_state(&ctx, |st| {
        st.pockets[&90_150_003].rooms[0].npc_ball_ids.clone()
    });
    assert!(!on_abyssal_room_npc_destroyed_core(&ctx.systems, &ctx.db, ab_sys, pid3_npcs[0]).await);

    // Test spawn_abyssal_room_contents idempotence when cache_item_id already exists
    let r1_cache_id = with_abyssal_state(&ctx, |st| {
        st.pockets[&char1].rooms[0].cache_item_id
    });
    let ab_handle = ctx.systems.get_or_spawn(ab_sys);
    spawn_abyssal_room_contents(
        &ctx, &ab_handle, pid1, 1, &[], r1_cache_id, 5, 47891, 2.0, 0.0, 3,
    )
    .await;

    // Test on_abyssal_cache_destroyed_core on EXTRACTION_SUBNODE_TYPE_ID with killer_char_id = None
    // and non-cache item (no-op)
    on_abyssal_cache_destroyed_core(&ctx.systems, &ctx.db, ab_sys, 999_888_777, None).await;
    let subnode_id = r1_cache_id + 500_000;
    on_abyssal_cache_destroyed_core(&ctx.systems, &ctx.db, ab_sys, subnode_id, None).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&subnode_id));
        assert!(snap
            .items
            .values()
            .any(|i| i.type_id == BIOADAPTIVE_CACHE_WRECK_TYPE_ID && i.owner_id == 500_026));
    });
}
