use crate::router::ServerContext;
use crate::services::pve_dungeon_runtime::{
    check_dungeon_runtime_gate_ship_allowed, with_dungeon_runtime_store,
    DungeonRuntimeInstance, DungeonRuntimeRoom,
};
use crate::services::pve_dungeon_runtime_rpc::check_warp_target_blocked_by_gated_dungeon_room;
use crate::services::pve_expeditions::{
    advance_expedition_step_core, create_escalation_expedition_core,
    validate_and_advance_pve_site_room,
};
use crate::services::pve_exploration::{with_pve_exploration_reg, PveSiteInstance};
use crate::services::pve_incursion_lifecycle::{
    advance_incursion_lifecycles, IncursionLifecycleMeta,
};
use crate::services::pve_incursion_sites::{
    process_incursion_site_completion_and_payout_core, IncursionNpcSpawn, IncursionSiteEntry,
    IncursionTaleRecord,
};
use crate::services::pve_special_sites::{read_pve_special_reg, with_pve_special_reg};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::SessionState;

fn make_ship_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test]
async fn r7_tc_p1_454_incursion_mothership_despawn_rotates_ball_ids_and_rejects_empty_site_id() {
    let ctx = ServerContext::new();
    let const_id = 20_999_454_u32;
    let sys_id = 30_000_142_u32;
    let initial_boss_id = 1_880_454_001_i64;

    with_pve_special_reg(&ctx, |reg| {
        reg.incursions.insert(
            const_id,
            IncursionTaleRecord {
                tale_id: 900_454,
                constellation_id: const_id,
                constellation_name: "TestConstellation".into(),
                staging_solar_system_id: sys_id,
                hq_solar_system_id: sys_id,
                vanguard_solar_system_ids: vec![sys_id],
                assault_solar_system_ids: Vec::new(),
                influence: 0.09,
                state: "Mobilizing".into(),
                mothership_spawned: true,
                mothership_defeated: false,
                tale_stop_published: false,
                lockout_until_filetime: None,
                sites: vec![IncursionSiteEntry {
                    site_id: "INC-900454-MS".into(),
                    dungeon_id: 2440,
                    dungeon_name: "Kundalini Manifest".into(),
                    tier: "Mothership".into(),
                    solar_system_id: sys_id,
                    base_isk: 31_500_000,
                    base_concord_lp: 7_000,
                    min_pilots: 40,
                    max_optimal_pilots: 80,
                    influence_delta: 0.15,
                    boss_npc_ball_id: initial_boss_id,
                    boss_type_id: 30_852,
                    escort_spawns: vec![IncursionNpcSpawn {
                        npc_ball_id: initial_boss_id + 1,
                        type_id: 30_845,
                        offset: [5_000.0, 0.0, 0.0],
                        destroyed: false,
                    }],
                    boss_destroyed: false,
                    killer_char_id: None,
                    killer_fleet_id: None,
                    completed_count: 0,
                    locked_until_low_influence: true,
                    position: DVec3::new(1_000_000.0, 0.0, 0.0),
                }],
            },
        );
        reg.incursion_lifecycle.insert(
            const_id,
            IncursionLifecycleMeta {
                spawned_at_sec: 100,
                last_site_completed_sec: 100,
                last_regen_sec: 100,
                withdraw_until_sec: None,
                ended_at_sec: None,
            },
        );
    });

    let empty_err = process_incursion_site_completion_and_payout_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        None,
        None,
        sys_id,
        "   ",
        None,
    )
    .await;
    assert!(empty_err.is_err(), "empty site_id must return IncursionSiteNotFound");

    ctx.db.clock().advance_secs(3_600);
    advance_incursion_lifecycles(&ctx);

    read_pve_special_reg(&ctx, |reg| {
        let inc = &reg.incursions[&const_id];
        assert!(!inc.mothership_spawned);
        let ms = inc.sites.iter().find(|s| s.tier == "Mothership").unwrap();
        assert_eq!(
            ms.boss_npc_ball_id,
            initial_boss_id + 1_000,
            "despawning Mothership on regen must rotate boss_npc_ball_id to prevent stale ball collisions"
        );
        assert!(
            ms.escort_spawns.iter().all(|e| e.npc_ball_id > initial_boss_id + 1_000),
            "escort_spawns must also rotate to unclaimed ball IDs"
        );
    });
}

#[tokio::test]
async fn r7_tc_p1_455_dungeon_gate_hull_restrictions_and_unconditional_gated_warp_block() {
    let ctx = ServerContext::new();
    let sys_id = 30_000_142_u32;

    assert!(
        check_dungeon_runtime_gate_ship_allowed(&ctx.sde, 1420, 670, 500_000.0).is_err(),
        "capsule must be restricted from dungeon acceleration gates"
    );
    let dread_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.published && t.group_id == 485)
        .map(|t| t.type_id)
        .unwrap_or(19_720);
    assert!(
        check_dungeon_runtime_gate_ship_allowed(&ctx.sde, 1420, dread_tid, 1_200_000_000.0).is_err(),
        "dreadnought must be restricted from dungeon acceleration gates"
    );

    let room2_pos = [2_500_000.0, 0.0, 0.0];
    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(
            55_455,
            DungeonRuntimeInstance {
                instance_id: 55_455,
                dungeon_id: 1420,
                dungeon_name: "Test Multiroom Complex".into(),
                category: "Combat".into(),
                archetype: String::new(),
                solar_system_id: sys_id,
                base_position: [1_000_000.0, 0.0, 0.0],
                owner_char_id: 91_455_001,
                disallow_mwd: false,
                entry_gate_pos: None,
                entry_gate_ball_id: None,
                entry_gate_locked: false,
                current_room_index: 3,
                rooms: vec![
                    DungeonRuntimeRoom {
                        room_id: 1,
                        room_index: 1,
                        room_name: "Room 1".into(),
                        position: [1_000_000.0, 0.0, 0.0],
                        gate_destination_pos: [990_000.0, 0.0, 0.0],
                        gated: false,
                        disallow_mwd: None,
                        gate_ball_id: Some(77_455_001),
                        gate_locked: false,
                        objects: Vec::new(),
                        npc_groups: Vec::new(),
                        triggers: Vec::new(),
                        spawned: true,
                        cleared: true,
                    },
                    DungeonRuntimeRoom {
                        room_id: 2,
                        room_index: 2,
                        room_name: "Room 2".into(),
                        position: room2_pos,
                        gate_destination_pos: [2_490_000.0, 0.0, 0.0],
                        gated: true,
                        disallow_mwd: None,
                        gate_ball_id: Some(77_455_002),
                        gate_locked: false,
                        objects: Vec::new(),
                        npc_groups: Vec::new(),
                        triggers: Vec::new(),
                        spawned: true,
                        cleared: true,
                    },
                ],
                completed: false,
                despawned: false,
                active_objective_id: 0,
                last_completed_task_id: None,
                completed_task_ids: Vec::new(),
                created_unix: 1_000,
            },
        );
    });

    let warp_res = check_warp_target_blocked_by_gated_dungeon_room(
        &ctx,
        sys_id,
        DVec3::from_array(room2_pos),
        Some(77_455_002),
    );
    assert!(
        warp_res.is_err(),
        "gated room (r_idx > 0) must unconditionally reject direct warp regardless of current_room_index"
    );
}

#[tokio::test]
async fn r7_tc_p1_456_expedition_step_coordinates_loot_and_non_ded_gate_range() {
    let ctx = ServerContext::new();
    let sys_id = 30_000_142_u32;
    let char_id = 91_456_001_i64;
    let ship_id = 92_456_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.name = "Explorer".into();
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let entry = create_escalation_expedition_core(
        &ctx.db,
        &ctx.sde,
        &ctx.systems,
        char_id,
        sys_id,
        1420,
        "Guristas Scout Outpost",
        [0.0; 3],
    )
    .await
    .expect("escalation creation");
    let inst_id = entry.instance_id;

    let _ = advance_expedition_step_core(&ctx.db, &ctx.sde, &ctx.systems, char_id, inst_id).await;
    let _ = advance_expedition_step_core(&ctx.db, &ctx.sde, &ctx.systems, char_id, inst_id).await;
    let done = advance_expedition_step_core(&ctx.db, &ctx.sde, &ctx.systems, char_id, inst_id)
        .await
        .unwrap()
        .unwrap();

    let expected_x = -45_000_000.0 + (done.step_index as f64) * 5_000_000.0;
    ctx.db.with_snapshot(|snap| {
        let drop_pos = snap
            .items
            .values()
            .find(|it| it.type_id == 19419 && it.owner_id == char_id)
            .and_then(|it| snap.item_positions.get(&it.location_id).copied())
            .expect("final expedition step reward container must be placed in snap.item_positions");
        assert!(
            (drop_pos[0] - expected_x).abs() < 1.0,
            "final expedition loot container X={} must match step {} expected X={}",
            drop_pos[0],
            done.step_index,
            expected_x
        );
    });

    let site_pos = [5_000_000.0, 0.0, 0.0];
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(sys_id).or_default().push(PveSiteInstance {
            site_id: "NON-DED-456".into(),
            dungeon_id: 2132,
            dungeon_name: "Angel Hideout".into(),
            solar_system_id: sys_id,
            site_category: "Combat".into(),
            ded_rating: None,
            position: site_pos,
            current_room: 1,
            total_rooms: 2,
            ..Default::default()
        });
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(make_ship_ball(
            ship_id,
            587,
            DVec3::new(site_pos[0] + 10_000.0, site_pos[1], site_pos[2]),
        ))
        .await;
    let mut sess = SessionState {
        session_id: 4561,
        user_id: Some(51),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let gate_err = validate_and_advance_pve_site_room(&ctx, &mut sess, sys_id, "NON-DED-456").await;
    assert!(gate_err.is_err(), "non-DED acceleration gate must reject activation beyond 2,500m");
}
