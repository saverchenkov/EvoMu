//! Additional unit and mutation-killing tests for `cluster_tick_downtime`.

use super::{
    cleanup_npc_wrecks_and_reset_missions_on_downtime,
    spawn_mission_wave_on_pocket_entry_if_needed,
};
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, ExtendedNpcEntity,
};
use crate::sp6_lifecycle::{
    with_mut_sp6_state, with_sp6_state, PersistedCombatHp, PersistedSpaceObjectMeta,
};
use eve_db::{ActiveMissionRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::DamageVector;
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
async fn downtime_wreck_filter_system_bounds_group186_and_mission_target_counts() {
    let ctx = test_ctx();

    // Empty run returns (0, 0) when no wrecks or Accepted missions exist.
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.clear();
    });
    assert_eq!(cleanup_npc_wrecks_and_reset_missions_on_downtime(&ctx), (0, 0));

    // Find an SDE wreck type with group_id == 186 and type_id != 26557.
    let sde_wreck_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 186 && t.type_id != 26557)
        .map(|t| t.type_id)
        .unwrap_or(26557);

    let w_low_bound = 910_000_001; // location_id == 30_000_000 (inclusive lower bound -> removed)
    let w_high_bound = 910_000_002; // location_id == 34_000_000 (inclusive upper bound -> removed)
    let w_below_bound = 910_000_003; // location_id == 29_999_999 (excluded -> kept)
    let w_above_bound = 910_000_004; // location_id == 34_000_001 (excluded -> kept)
    let w_non_wreck_type = 910_000_005; // group != 186, type != 26557, is_npc_wreck = true (kept)
    let w_other_child = 910_000_006; // child inside w_below_bound (kept)

    let char_id = 90_000_001;
    let m1_agent = 3_010_001u32; // Uses explicit mission_initial_npc_counts = 7
    let m2_agent = 3_010_002u32; // Uses spawned_npc_ball_ids.len() = 6 (> npcs_remaining=2, > 4)
    let m3_agent = 3_010_003u32; // Uses npcs_remaining = 5 (> spawned_npc_ball_ids.len()=1, > 4)

    with_mut_sp6_state(&ctx.db, |st, snap| {
        for (wid, tid, loc, is_npc) in [
            (w_low_bound, sde_wreck_tid, 30_000_000i64, true),
            (w_high_bound, 26557u32, 34_000_000i64, true),
            (w_below_bound, 26557u32, 29_999_999i64, true),
            (w_above_bound, 26557u32, 34_000_001i64, true),
            (w_non_wreck_type, 587u32, 30_000_142i64, true),
        ] {
            snap.items.insert(
                wid,
                InventoryItemRecord {
                    item_id: wid,
                    type_id: tid,
                    owner_id: 1_000_125,
                    location_id: loc,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(wid, [10.0, 20.0, 30.0]);
            st.space_object_meta.insert(
                wid,
                PersistedSpaceObjectMeta {
                    is_npc_wreck: is_npc,
                    ..Default::default()
                },
            );
            st.ship_combat_hp.insert(
                wid,
                PersistedCombatHp {
                    shield_hp: 0.0,
                    armor_hp: 0.0,
                    hull_hp: 100.0,
                },
            );
        }
        snap.items.insert(
            w_other_child,
            InventoryItemRecord {
                item_id: w_other_child,
                type_id: 34,
                owner_id: 1_000_125,
                location_id: w_below_bound,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );

        // Missions with distinct target_count branches and pre-existing awaiting key.
        snap.active_missions.insert(
            (char_id, m1_agent),
            ActiveMissionRecord {
                mission_id: 601,
                character_id: char_id,
                agent_id: m1_agent as i64,
                solar_system_id: 30_000_142,
                state: "Accepted".into(),
                current_room_id: 2,
                npcs_remaining: 1,
                spawned_npc_ball_ids: vec![910_100_001],
                bookmark_id: None,
            },
        );
        st.mission_initial_npc_counts
            .insert(format!("{char_id}:601"), 7);
        st.mission_initial_npc_types
            .insert(format!("{char_id}:601"), vec![10_019, 10_020]);
        st.mission_initial_npc_types
            .insert("stale:999".into(), vec![10_019]);
        // Pre-populate awaiting key to verify no duplicate is pushed.
        st.missions_awaiting_downtime_respawn
            .push((char_id, m1_agent));

        snap.active_missions.insert(
            (char_id, m2_agent),
            ActiveMissionRecord {
                mission_id: 602,
                character_id: char_id,
                agent_id: m2_agent as i64,
                solar_system_id: 30_000_142,
                state: "Accepted".into(),
                current_room_id: 3,
                npcs_remaining: 2,
                spawned_npc_ball_ids: vec![1, 2, 3, 4, 5, 6],
                bookmark_id: None,
            },
        );
        snap.active_missions.insert(
            (char_id, m3_agent),
            ActiveMissionRecord {
                mission_id: 603,
                character_id: char_id,
                agent_id: m3_agent as i64,
                solar_system_id: 30_000_142,
                state: "Accepted".into(),
                current_room_id: 4,
                npcs_remaining: 5,
                spawned_npc_ball_ids: vec![7],
                bookmark_id: None,
            },
        );
    });

    with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_entities.insert(
            w_low_bound,
            ExtendedNpcEntity {
                ball_id: w_low_bound,
                type_id: sde_wreck_tid,
                faction_id: 500_010,
                archetype: "Pirate".into(),
                solar_system_id: 30_000_000,
                spawn_group_id: 1,
                target_id: None,
                aggression_threat: HashMap::new(),
                threat_table: HashMap::new(),
                bounty_isk: 0.0,
                optimal_range: 1_000.0,
                orbit_range: 500.0,
                damage_vector: DamageVector::default(),
                entropic_cycles: 0,
                drifter_overshield_hp: 0.0,
                drifter_doomsday_fired: false,
                web_speed_factor: 0.0,
                web_range: 0.0,
                scram_strength: 0.0,
                scram_range: 0.0,
                neut_amount: 0.0,
                ecm_jammed: false,
                remote_shield_rep: 0.0,
                remote_armor_rep: 0.0,
                remote_rep_range: 0.0,
                tag_drop_type_id: None,
                custom_cargo_loot: None,
                spawn_position: DVec3::ZERO,
                sleeper_wh_class: 0,
            },
        );
    });

    let (wrecks_removed, missions_reset) = cleanup_npc_wrecks_and_reset_missions_on_downtime(&ctx);
    assert_eq!(wrecks_removed, 2);
    assert_eq!(missions_reset, 3);

    with_sp6_state(&ctx.db, |st| {
        assert!(!st.ship_combat_hp.contains_key(&w_low_bound));
        assert!(!st.ship_combat_hp.contains_key(&w_high_bound));
        assert!(st.ship_combat_hp.contains_key(&w_below_bound));
        assert!(st.ship_combat_hp.contains_key(&w_above_bound));
        assert!(st.ship_combat_hp.contains_key(&w_non_wreck_type));

        assert_eq!(
            st.mission_initial_npc_counts.get(&format!("{char_id}:601")),
            Some(&7)
        );
        assert!(st
            .mission_initial_npc_types
            .contains_key(&format!("{char_id}:601")));
        assert!(!st.mission_initial_npc_types.contains_key("stale:999"));

        let m1_await_count = st
            .missions_awaiting_downtime_respawn
            .iter()
            .filter(|&&k| k == (char_id, m1_agent))
            .count();
        assert_eq!(m1_await_count, 1);
    });

    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.npc_entities.contains_key(&w_low_bound));
    });

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&w_low_bound));
        assert!(!snap.items.contains_key(&w_high_bound));
        assert!(snap.items.contains_key(&w_below_bound));
        assert!(snap.items.contains_key(&w_above_bound));
        assert!(snap.items.contains_key(&w_non_wreck_type));
        assert!(snap.items.contains_key(&w_other_child));

        assert_eq!(snap.active_missions[&(char_id, m1_agent)].npcs_remaining, 7);
        assert_eq!(snap.active_missions[&(char_id, m2_agent)].npcs_remaining, 6);
        assert_eq!(snap.active_missions[&(char_id, m3_agent)].npcs_remaining, 5);
    });
}

#[tokio::test]
async fn spawn_mission_wave_handles_custom_types_fallback_stats_and_geometry() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let agent_id = 3_020_001u32;

    // Pick one valid SDE NPC type and one synthetic unmapped type ID (999_888_777) to hit the fallback tuple.
    let valid_npc_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 11 && t.faction_id.is_some())
        .map(|t| t.type_id)
        .unwrap_or(10_019);
    let fallback_npc_tid = 999_888_777u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.clear();
    });

    with_mut_sp6_state(&ctx.db, |st, snap| {
        st.missions_awaiting_downtime_respawn.clear();
        // Mission needs respawn via (spawned_npc_ball_ids.is_empty() && npcs_remaining > 0) even without awaiting flag!
        snap.active_missions.insert(
            (char_id, agent_id),
            ActiveMissionRecord {
                mission_id: 701,
                character_id: char_id,
                agent_id: agent_id as i64,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 2,
                npcs_remaining: 2,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        st.mission_initial_npc_counts
            .insert(format!("{char_id}:701"), 4);
        st.mission_initial_npc_types.insert(
            format!("{char_id}:701"),
            vec![valid_npc_tid, fallback_npc_tid],
        );
    });

    // Wrong character or wrong solar system spawns 0 NPCs.
    assert_eq!(
        spawn_mission_wave_on_pocket_entry_if_needed(&ctx, char_id + 1, sys_id, DVec3::ZERO),
        0
    );
    assert_eq!(
        spawn_mission_wave_on_pocket_entry_if_needed(&ctx, char_id, sys_id + 1, DVec3::ZERO),
        0
    );

    let origin = DVec3::new(10_000.0, 200.0, -5_000.0);
    let spawned = spawn_mission_wave_on_pocket_entry_if_needed(&ctx, char_id, sys_id, origin);
    assert_eq!(spawned, 4);

    // Calling again immediately spawns 0 because spawned_npc_ball_ids is now non-empty and not awaiting.
    assert_eq!(
        spawn_mission_wave_on_pocket_entry_if_needed(&ctx, char_id, sys_id, origin),
        0
    );

    let bids = ctx.db.with_snapshot(|snap| {
        let m = &snap.active_missions[&(char_id, agent_id)];
        assert_eq!(m.current_room_id, 1);
        assert_eq!(m.npcs_remaining, 4);
        m.spawned_npc_ball_ids.clone()
    });
    assert_eq!(bids.len(), 4);

    with_combat_runtime(&ctx, |rt| {
        // idx 0 -> valid_npc_tid at angle 0: origin + (4500, 0, 0)
        let e0 = &rt.npc_entities[&bids[0]];
        assert_eq!(e0.type_id, valid_npc_tid);
        assert!((e0.spawn_position.x - (origin.x + 4_500.0)).abs() < 1e-6);
        assert!((e0.spawn_position.y - origin.y).abs() < 1e-6);
        assert!((e0.spawn_position.z - origin.z).abs() < 1e-6);
        assert!(e0.orbit_range >= 2_500.0 && e0.orbit_range <= 15_000.0);
        assert!(e0.bounty_isk >= 10_000.0);

        // idx 1 -> fallback_npc_tid at angle PI/2: origin + (0, 0, 4500)
        let e1 = &rt.npc_entities[&bids[1]];
        assert_eq!(e1.type_id, fallback_npc_tid);
        assert_eq!(e1.faction_id, 500_010);
        assert_eq!(e1.bounty_isk, 25_000.0);
        assert_eq!(e1.optimal_range, 10_000.0);
        assert_eq!(e1.orbit_range, 6_000.0);
        assert_eq!(e1.damage_vector, DamageVector::new(12.0, 15.0, 15.0, 12.0));
        assert!((e1.spawn_position.x - origin.x).abs() < 1e-6);
        assert!((e1.spawn_position.z - (origin.z + 4_500.0)).abs() < 1e-6);

        // idx 2 -> valid_npc_tid (cycles modulo 2) at angle PI: origin + (-4500, 0, 0)
        let e2 = &rt.npc_entities[&bids[2]];
        assert_eq!(e2.type_id, valid_npc_tid);
        assert!((e2.spawn_position.x - (origin.x - 4_500.0)).abs() < 1e-6);
    });
}
