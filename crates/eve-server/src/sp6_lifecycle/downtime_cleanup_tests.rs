//! Unit tests for downtime wreck purging and mission pocket respawn.

use super::{
    cleanup_npc_wrecks_and_reset_missions_on_downtime,
    spawn_mission_wave_on_pocket_entry_if_needed,
};
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime, ExtendedNpcEntity};
use crate::sp6_lifecycle::{record_wreck_or_container_meta_in_snap, with_sp6_state};
use eve_db::{ActiveMissionRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[tokio::test]
async fn downtime_purges_npc_wrecks_resets_missions_and_respawns_wave() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let npc_wreck_id = 900_100_001;
    let npc_loot_id = 900_100_002;
    let player_wreck_id = 900_100_003;
    let old_mission_npc_id = 900_100_010;

    ctx.db.with_mut_snapshot(|snap| {
        for (wid, owner, is_npc) in [
            (npc_wreck_id, 1_000_125, true),
            (player_wreck_id, char_id, false),
        ] {
            snap.items.insert(
                wid,
                InventoryItemRecord {
                    item_id: wid,
                    type_id: 26557,
                    owner_id: owner,
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
            snap.item_positions.insert(wid, [100.0, 0.0, 0.0]);
            record_wreck_or_container_meta_in_snap(snap, wid, ctx.now_unix(), is_npc);
        }
        snap.items.insert(
            npc_loot_id,
            InventoryItemRecord {
                item_id: npc_loot_id,
                type_id: 212,
                owner_id: 1_000_125,
                location_id: npc_wreck_id,
                flag: InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.active_missions.insert(
            (char_id, 3_000_001),
            ActiveMissionRecord {
                mission_id: 501,
                character_id: char_id,
                agent_id: 3_000_001,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 3,
                npcs_remaining: 2,
                spawned_npc_ball_ids: vec![old_mission_npc_id],
                bookmark_id: None,
            },
        );
        snap.active_missions.insert(
            (90_000_002, 3_000_002),
            ActiveMissionRecord {
                mission_id: 502,
                character_id: 90_000_002,
                agent_id: 3_000_002,
                solar_system_id: sys_id,
                state: "Completed".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        snap.sp6_lifecycle
            .space_object_meta
            .entry(npc_wreck_id)
            .or_default()
            .is_npc_wreck = true;
        snap.sp6_lifecycle
            .mission_initial_npc_counts
            .insert("90000002:502".into(), 3);
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for bid in [npc_wreck_id, player_wreck_id, old_mission_npc_id] {
        sys_handle.try_spawn_ball_with_combat(
            Ball {
                id: bid,
                type_id: 26557,
                position: DVec3::new(100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 20.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        );
    }
    with_mut_combat_runtime(&ctx, |rt| {
        rt.container_lifetimes.insert(
            npc_wreck_id,
            crate::services::combat_crimewatch::ContainerLifetimeEntry {
                item_id: npc_wreck_id,
                solar_system_id: sys_id,
                type_id: 26557,
                owner_id: 1_000_125,
                created_unix: 1_000,
                lifetime_secs: 9_999,
                is_abandoned: false,
            },
        );
        rt.npc_entities.insert(
            old_mission_npc_id,
            ExtendedNpcEntity {
                ball_id: old_mission_npc_id,
                type_id: 10_019,
                faction_id: 500_010,
                archetype: "Pirate".into(),
                solar_system_id: sys_id,
                spawn_group_id: 1,
                target_id: None,
                aggression_threat: HashMap::new(),
                threat_table: HashMap::new(),
                bounty_isk: 10_000.0,
                optimal_range: 10_000.0,
                orbit_range: 5_000.0,
                damage_vector: DamageVector::new(10.0, 0.0, 0.0, 0.0),
                entropic_cycles: 0,
                drifter_overshield_hp: 0.0,
                drifter_doomsday_fired: false,
                web_speed_factor: 1.0,
                web_range: 0.0,
                scram_strength: 0.0,
                scram_range: 0.0,
                neut_amount: 0.0,
                ecm_jammed: false,
                remote_shield_rep: 0.0,
                remote_armor_rep: 0.0,
                remote_rep_range: 0.0,
                tag_drop_type_id: None,
                spawn_position: DVec3::new(100.0, 0.0, 0.0),
                sleeper_wh_class: 0,
                custom_cargo_loot: None,
            },
        );
    });

    let (wrecks_removed, missions_reset) = cleanup_npc_wrecks_and_reset_missions_on_downtime(&ctx);
    assert_eq!(wrecks_removed, 1);
    assert_eq!(missions_reset, 1);

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&npc_wreck_id));
        assert!(!snap.items.contains_key(&npc_loot_id));
        assert!(snap.items.contains_key(&player_wreck_id));
        let m = &snap.active_missions[&(char_id, 3_000_001)];
        assert_eq!(m.current_room_id, 1);
        assert_eq!(m.npcs_remaining, 4);
        assert!(m.spawned_npc_ball_ids.is_empty());
    });
    with_sp6_state(&ctx.db, |st| {
        assert!(!st.mission_initial_npc_counts.contains_key("90000002:502"));
        assert!(st.missions_awaiting_downtime_respawn.contains(&(char_id, 3_000_001)));
    });
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.container_lifetimes.contains_key(&npc_wreck_id));
        assert!(!rt.npc_entities.contains_key(&old_mission_npc_id));
    });

    // Pocket entry respawns the initial mission NPC wave and clears awaiting flag
    let spawned = spawn_mission_wave_on_pocket_entry_if_needed(
        &ctx,
        char_id,
        sys_id,
        DVec3::new(5_000.0, 0.0, 0.0),
    );
    assert_eq!(spawned, 4);
    ctx.db.with_snapshot(|snap| {
        let m = &snap.active_missions[&(char_id, 3_000_001)];
        assert_eq!(m.spawned_npc_ball_ids.len(), 4);
    });
    with_sp6_state(&ctx.db, |st| {
        assert!(!st.missions_awaiting_downtime_respawn.contains(&(char_id, 3_000_001)));
    });
}

#[tokio::test]
async fn populate_space_objects_decays_expired_containers_bubbles_and_deployables() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    ctx.advance_secs(10_000_000);
    let now = ctx.now_unix();

    let expired_pi = 900_200_001;
    let expired_mwd = 900_200_002;
    let expired_depot = 900_200_003;
    let expired_secure = 900_200_004;
    let active_depot = 900_200_005;

    let secure_tid = ctx
        .sde
        .types
        .values()
        .find(|t| {
            t.effect_ids.contains(&649)
                && t.capacity > 0.0
                && !crate::services::mobile_deployables::is_mobile_structure(&ctx.sde, t.type_id)
        })
        .map(|t| t.type_id)
        .unwrap_or(17_366);
    let mwd_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 361)
        .map(|t| t.type_id)
        .unwrap_or(12_202);

    ctx.db.with_mut_snapshot(|snap| {
        for (id, tid, pos) in [
            (expired_pi, 23u32, [10_000.0, 0.0, 0.0]),
            (expired_mwd, mwd_tid, [20_000.0, 0.0, 0.0]),
            (expired_depot, 33_474u32, [30_000.0, 0.0, 0.0]),
            (expired_secure, secure_tid, [40_000.0, 0.0, 0.0]),
            (active_depot, 33_474u32, [50_000.0, 0.0, 0.0]),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: 90_000_001,
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
            snap.item_positions.insert(id, pos);
        }

        let sp6 = &mut snap.sp6_lifecycle;
        sp6.space_object_meta.insert(
            expired_pi,
            crate::sp6_lifecycle::PersistedSpaceObjectMeta {
                created_unix: Some(1),
                planet_id: Some(40_009_049),
                ..Default::default()
            },
        );
        sp6.space_object_meta.insert(
            expired_mwd,
            crate::sp6_lifecycle::PersistedSpaceObjectMeta {
                anchor_start_unix: Some(1),
                ..Default::default()
            },
        );
        sp6.space_object_meta.insert(
            expired_secure,
            crate::sp6_lifecycle::PersistedSpaceObjectMeta {
                anchor_start_unix: Some(1),
                ..Default::default()
            },
        );

        crate::services::mobile_deployables::write_deployable_state_snap(snap, |dep_state| {
            dep_state.deployables.insert(
                expired_depot,
                crate::services::mobile_deployables::MobileDeployableRecord {
                    item_id: expired_depot,
                    type_id: 33_474,
                    group_id: 1246,
                    owner_id: 90_000_001,
                    solar_system_id: sys_id,
                    anchor_start_unix: 10,
                    anchor_duration_secs: 60,
                    anchor_completed: true,
                    is_unanchored: false,
                    expires_at_unix: 100,
                    last_interaction_unix: 10,
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    position: [30_000.0, 0.0, 0.0],
                    deployed_fleet_id: None,
                },
            );
            dep_state.deployables.insert(
                active_depot,
                crate::services::mobile_deployables::MobileDeployableRecord {
                    item_id: active_depot,
                    type_id: 33_474,
                    group_id: 1246,
                    owner_id: 90_000_001,
                    solar_system_id: sys_id,
                    anchor_start_unix: now.saturating_sub(120),
                    anchor_duration_secs: 60,
                    anchor_completed: true,
                    is_unanchored: false,
                    expires_at_unix: now + 86_400,
                    last_interaction_unix: now,
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    position: [50_000.0, 0.0, 0.0],
                    deployed_fleet_id: None,
                },
            );
        });
    });

    let mut ballpark = eve_destiny::Ballpark::new(sys_id);
    crate::sp6_lifecycle::populate_persisted_space_objects(&mut ballpark, sys_id, &ctx.sde, &ctx.db);

    assert!(!ballpark.balls.contains_key(&expired_pi));
    assert!(!ballpark.balls.contains_key(&expired_mwd));
    assert!(!ballpark.balls.contains_key(&expired_depot));
    assert!(!ballpark.balls.contains_key(&expired_secure));
    assert!(ballpark.balls.contains_key(&active_depot));

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&expired_pi));
        assert!(!snap.items.contains_key(&expired_mwd));
        assert!(!snap.items.contains_key(&expired_depot));
        assert!(!snap.items.contains_key(&expired_secure));
        assert!(snap.items.contains_key(&active_depot));
    });
}

#[tokio::test]
async fn cluster_advance_handles_safe_logoff_abort_scrambled_warp_bounty_concord_and_void() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let base_now = ctx.now_unix();
    let ship1 = 900_210_001;
    let ship2 = 900_210_002;
    let void_sys = crate::services::abyssal_void_mgr::VOID_SPACE_SYSTEM_ID;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid, s) in [(90_000_001i64, ship1, sys_id), (90_000_002i64, ship2, sys_id)] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: s as i64,
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
            snap.item_positions.insert(sid, [0.0, 0.0, 0.0]);
            if let Some(c) = snap.characters.get_mut(&cid) {
                c.active_ship_id = sid;
                c.solar_system_id = s;
                c.station_id = None;
                c.structure_id = None;
            }
        }
    });

    let handle = ctx.systems.get_or_spawn(sys_id);
    for (cid, sid) in [(90_000_001i64, ship1), (90_000_002i64, ship2)] {
        handle.try_spawn_ball_with_combat(
            Ball {
                id: sid,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_100_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(cid, sid, 587),
        );
    }
    let _ = handle.set_warp_scramble_strength(ship2, 2).await;

    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, 90_000_001), 50_000.0);
        rt.bounty_tick_next_flush_unix.insert((sys_id, 90_000_001), base_now + 5);
    });
    if let Ok(mut cw) = ctx.crimewatch.write() {
        let st = cw.states.entry(90_000_002).or_default();
        st.pending_concord_arrival = Some((sys_id, base_now + 5, 90_000_001, ship1));
    }

    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _| {
        st.self_destructs.insert(
            ship1,
            eve_db::SelfDestructPersistedEntry {
                ship_id: ship1,
                char_id: 90_000_001,
                solar_system_id: sys_id,
                started_unix: base_now,
                detonate_at_unix: base_now + 60,
            },
        );
        st.safe_logoffs.insert(
            90_000_001,
            crate::sp6_lifecycle::SafeLogoffEntry {
                char_id: 90_000_001,
                ship_id: ship1,
                solar_system_id: sys_id,
                started_unix: base_now,
                complete_at_unix: base_now + 20,
                completed: false,
            },
        );
        st.disconnected_ships.insert(
            90_000_002,
            crate::sp6_lifecycle::DisconnectedShipEntry {
                char_id: 90_000_002,
                ship_id: ship2,
                ship_type_id: 587,
                solar_system_id: sys_id,
                last_position: [0.0, 0.0, 0.0],
                emergency_warp_dest: Some([1_000_000.0, 0.0, 0.0]),
                disconnected_at_unix: base_now,
                remove_at_unix: base_now + 60,
                removed_from_space: false,
                pre_warp_position: None,
            },
        );
    });

    ctx.advance_secs(20);
    crate::sp6_lifecycle::on_advance_secs(&ctx, base_now, 20);

    crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
        assert!(st.safe_logoffs.get(&90_000_001).is_some_and(|e| e.completed));
        assert!(st.disconnected_ships[&90_000_002].emergency_warp_dest.is_none());
    });

    // Stranded pilot in Void Space evacuates back to origin solar system; Abyssal Space collapses ship
    let void_ship = 900_210_003;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            void_ship,
            InventoryItemRecord {
                item_id: void_ship,
                type_id: 587,
                owner_id: 90_000_002,
                location_id: void_sys as i64,
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
        if let Some(c) = snap.characters.get_mut(&90_000_002) {
            c.active_ship_id = void_ship;
            c.solar_system_id = void_sys;
            c.station_id = None;
            c.structure_id = None;
        }
    });
    crate::sp6_lifecycle::cluster_tick::collapse_stranded_abyssal_if_needed_sync(
        &ctx,
        90_000_002,
        ctx.now_unix(),
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&90_000_002].solar_system_id),
        30_000_142
    );

    let abyss_sys = crate::services::abyssal_void_mgr::ABYSSAL_SPACE_SYSTEM_ID;
    let abyss_ship = 900_210_004;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            abyss_ship,
            InventoryItemRecord {
                item_id: abyss_ship,
                type_id: 587,
                owner_id: 90_000_002,
                location_id: abyss_sys as i64,
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
        if let Some(c) = snap.characters.get_mut(&90_000_002) {
            c.active_ship_id = abyss_ship;
            c.solar_system_id = abyss_sys;
            c.station_id = None;
            c.structure_id = None;
        }
    });
    crate::sp6_lifecycle::cluster_tick::collapse_stranded_abyssal_if_needed_sync(
        &ctx,
        90_000_002,
        ctx.now_unix(),
    );
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&abyss_ship)));
}

#[tokio::test]
async fn reload_space_objects_and_advance_disconnected_warp_expiry_and_self_destruct() {
    use crate::sp6_lifecycle::{
        on_advance_secs, populate_persisted_space_objects, with_mut_sp6_state,
        DisconnectedShipEntry, PersistedCombatHp, PersistedSpaceObjectMeta,
        SelfDestructPersistedEntry,
    };
    use eve_destiny::Ballpark;

    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let now = ctx.now_unix();
    let mwd_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 361)
        .map(|t| t.type_id)
        .unwrap_or(12198);

    let custom_hp_ship = 900_220_001;
    let anchoring_mwd_id = 900_220_002;
    let disc_expired_ship = 900_220_003;
    let disc_warping_ship = 900_220_004;
    let self_destruct_ship = 900_220_006;

    ctx.db.with_mut_snapshot(|snap| {
        for (item_id, type_id, owner_id, pos) in [
            (custom_hp_ship, 587, 90_000_001, [1_000.0, 0.0, 0.0]),
            (anchoring_mwd_id, mwd_type_id, 90_000_001, [25_000.0, 0.0, 0.0]),
            (disc_expired_ship, 587, 90_000_002, [1_000.0, 0.0, 0.0]),
            (disc_warping_ship, 587, 90_000_001, [4_000.0, 0.0, 0.0]),
            (self_destruct_ship, 587, 90_000_001, [500_000.0, 1_000.0, 0.0]),
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id, type_id, owner_id, location_id: sys_id as i64,
                    flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(item_id, pos);
        }
        for (cid, sid) in [(90_000_002, disc_expired_ship), (90_000_001, disc_warping_ship)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.active_ship_id = sid;
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
            }
        }
    });

    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.ship_combat_hp.insert(disc_warping_ship, PersistedCombatHp { shield_hp: 123.0, armor_hp: 234.0, hull_hp: 345.0 });
        st.space_object_meta.insert(anchoring_mwd_id, PersistedSpaceObjectMeta {
            created_unix: Some(now), anchor_start_unix: Some(now), anchoring_delay_secs: 90.0,
            anchor_completed: false, bubble_radius_m: Some(18_500.0), ..Default::default()
        });
        st.disconnected_ships.insert(90_000_002, DisconnectedShipEntry {
            char_id: 90_000_002, ship_id: disc_expired_ship, ship_type_id: 587, solar_system_id: sys_id,
            disconnected_at_unix: now.saturating_sub(120), remove_at_unix: now,
            emergency_warp_dest: Some([50_000.0, 0.0, 0.0]), last_position: [1_000.0, 0.0, 0.0],
            removed_from_space: false, pre_warp_position: None,
        });
        st.disconnected_ships.insert(90_000_001, DisconnectedShipEntry {
            char_id: 90_000_001, ship_id: disc_warping_ship, ship_type_id: 587, solar_system_id: sys_id,
            disconnected_at_unix: now, remove_at_unix: now + 90,
            emergency_warp_dest: Some([5_500.0, 0.0, 0.0]), last_position: [4_000.0, 0.0, 0.0],
            removed_from_space: false, pre_warp_position: Some([4_000.0, 0.0, 0.0]),
        });
    });

    let mut ballpark = Ballpark::new(sys_id);
    let pending = populate_persisted_space_objects(&mut ballpark, sys_id, &ctx.sde, &ctx.db);
    assert!(pending.iter().any(|p| p.id == anchoring_mwd_id));
    assert!(ballpark.balls.contains_key(&disc_warping_ship));
    assert!(!ballpark.balls.contains_key(&disc_expired_ship));
    let cs = ballpark.combat_states.get(&disc_warping_ship).expect("ship combat hp");
    assert_eq!(cs.shield_hp, 123.0);
    assert_eq!(cs.armor_hp, 234.0);

    let _sys = ctx.systems.get_or_spawn(sys_id);
    with_mut_sp6_state(&ctx.db, |st, snap| {
        snap.items.insert(
            disc_expired_ship,
            InventoryItemRecord {
                item_id: disc_expired_ship, type_id: 587, owner_id: 90_000_002,
                location_id: sys_id as i64, flag: InventoryFlag::None, quantity: 1,
                singleton: true, version: 1, ..Default::default()
            },
        );
        st.disconnected_ships.insert(90_000_002, DisconnectedShipEntry {
            char_id: 90_000_002, ship_id: disc_expired_ship, ship_type_id: 587, solar_system_id: sys_id,
            disconnected_at_unix: now.saturating_sub(115), remove_at_unix: now + 5,
            emergency_warp_dest: None, last_position: [1_000.0, 0.0, 0.0],
            removed_from_space: false, pre_warp_position: None,
        });
        st.self_destructs.insert(self_destruct_ship, SelfDestructPersistedEntry {
            ship_id: self_destruct_ship, char_id: 90_000_001, solar_system_id: sys_id,
            started_unix: now.saturating_sub(120), detonate_at_unix: now + 5,
        });
    });
    ctx.db.clock().advance_secs(10);
    on_advance_secs(&ctx, now, 10);
    with_sp6_state(&ctx.db, |st| {
        assert!(st.disconnected_ships[&90_000_002].removed_from_space);
        assert!(st.disconnected_ships[&90_000_001].emergency_warp_dest.is_none());
    });
}


