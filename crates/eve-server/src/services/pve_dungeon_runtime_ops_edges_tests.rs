//! Edge-case unit tests for `pve_dungeon_runtime_ops`:
//! `resolve_fw_or_special_warp_target` (FW complexes, Insurgency sites, Homefront ops,
//! Triglavian sites, active vs ended Incursions, Drifter Hives) and
//! `find_active_dungeon_id_at_position` (runtime entry gate, runtime rooms,
//! completed/despawned filtering, and exploration site fallback).

use super::*;
use crate::services::pve_exploration::{with_pve_exploration_reg, PveSiteInstance};
use crate::services::pve_incursion_sites::{IncursionSiteEntry, IncursionTaleRecord};
use crate::services::pve_special_sites::{
    with_pve_special_reg, DrifterHiveRecord, HomefrontSiteRecord, TriglavianSiteRecord,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{PersistedInsurgencySiteRecord, SystemDynamicState, WorldRepository};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_200_000)));
    ctx
}

#[test]
fn resolve_fw_or_special_warp_target_all_site_types_and_filters() {
    let ctx = make_ctx();
    let sys_id = 30_000_142u32;

    // Nothing seeded -> None
    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_id, 999, None),
        None
    );

    // 1. FW complex and Insurgency instance in snapshot
    let fw_pos = [123_000.0, -45_000.0, 67_000.0];
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            sys_id,
            SystemDynamicState {
                solar_system_id: sys_id,
                fw_complex_position: Some(fw_pos),
                ..Default::default()
            },
        );
        snap.fw_runtime
            .fw_complexes
            .insert(700_001, (sys_id, "Novice".into(), 500_001, 0.0, false));
        snap.fw_runtime
            .fw_complexes
            .insert(700_002, (sys_id, "Novice".into(), 500_001, 100.0, true)); // done -> ignored
        snap.insurgency_runtime
            .insurgency_instances
            .push(PersistedInsurgencySiteRecord {
                instance_id: 700_003,
                solar_system_id: sys_id,
                site_type: "CorporateOutpostRaid".into(),
                name: "Raid".into(),
                corruption_reward: 100.0,
                completed: false,
            });
    });

    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_id, 700_001, None),
        Some(DVec3::from_array(fw_pos))
    );
    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_id, 700_002, None),
        None
    );
    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_id, 700_003, None),
        Some(DVec3::new(123_000.0, -45_000.0, 67_000.0))
    );

    // 2. Homefront, Triglavian, Incursions (Active vs Ended), Drifter Hives
    with_pve_special_reg(&ctx, |reg| {
        reg.homefront_sites.insert(
            sys_id,
            vec![
                HomefrontSiteRecord {
                    instance_id: 800_012,
                    dungeon_id: 5901,
                    dungeon_name: "HF Active".into(),
                    solar_system_id: sys_id,
                    optimal_fleet_size: 5,
                    base_pilot_isk: 1_000_000,
                    completed: false,
                    reward_paid_isk: 0,
                    boss_npc_ball_id: 1,
                    boss_type_id: 0,
                },
                HomefrontSiteRecord {
                    instance_id: 800_013,
                    dungeon_id: 5902,
                    dungeon_name: "HF Done".into(),
                    solar_system_id: sys_id,
                    optimal_fleet_size: 5,
                    base_pilot_isk: 1_000_000,
                    completed: true,
                    reward_paid_isk: 0,
                    boss_npc_ball_id: 2,
                    boss_type_id: 0,
                },
            ],
        );
        reg.triglavian_sites.insert(
            sys_id,
            vec![TriglavianSiteRecord {
                instance_id: 810_005,
                dungeon_id: 6100,
                dungeon_name: "Trig Active".into(),
                solar_system_id: sys_id,
                faction_owner_id: 500_026,
                base_isk: 1_000_000,
                survey_db_drop: 0,
                completed: false,
                boss_npc_ball_id: 3,
                boss_type_id: 0,
                escort_ball_ids: vec![],
                escort_type_ids: vec![],
                beacon_ball_id: 0,
            }],
        );
        reg.incursions.insert(
            20_000_020,
            IncursionTaleRecord {
                tale_id: 1,
                constellation_id: 20_000_020,
                constellation_name: "Kimotoro".into(),
                staging_solar_system_id: sys_id,
                vanguard_solar_system_ids: vec![sys_id],
                assault_solar_system_ids: vec![],
                hq_solar_system_id: sys_id,
                influence: 0.5,
                state: "Mobilizing".into(),
                mothership_spawned: false,
                mothership_defeated: false,
                sites: vec![IncursionSiteEntry {
                    site_id: "INC-SITE-1".into(),
                    dungeon_id: 6200,
                    dungeon_name: "Vanguard".into(),
                    solar_system_id: sys_id,
                    tier: "Vanguard".into(),
                    min_pilots: 5,
                    max_optimal_pilots: 10,
                    base_isk: 10_000_000,
                    base_concord_lp: 1_400,
                    influence_delta: 0.01,
                    locked_until_low_influence: false,
                    completed_count: 0,
                    boss_npc_ball_id: 4,
                    boss_type_id: 2933,
                    position: [50_000.0, 60_000.0, 70_000.0],
                    escort_spawns: vec![],
                    boss_destroyed: false,
                    killer_char_id: None,
                    killer_fleet_id: None,
                }],
                lockout_until_filetime: None,
                tale_stop_published: false,
            },
        );
        reg.incursions.insert(
            20_000_021,
            IncursionTaleRecord {
                tale_id: 2,
                constellation_id: 20_000_021,
                constellation_name: "EndedCon".into(),
                staging_solar_system_id: sys_id,
                vanguard_solar_system_ids: vec![sys_id],
                assault_solar_system_ids: vec![],
                hq_solar_system_id: sys_id,
                influence: 0.0,
                state: "Ended".into(),
                mothership_spawned: false,
                mothership_defeated: true,
                sites: vec![IncursionSiteEntry {
                    site_id: "INC-ENDED".into(),
                    dungeon_id: 6299,
                    dungeon_name: "Ended".into(),
                    solar_system_id: sys_id,
                    tier: "Vanguard".into(),
                    min_pilots: 5,
                    max_optimal_pilots: 10,
                    base_isk: 0,
                    base_concord_lp: 0,
                    influence_delta: 0.0,
                    locked_until_low_influence: false,
                    completed_count: 0,
                    boss_npc_ball_id: 5,
                    boss_type_id: 2933,
                    position: [1.0, 2.0, 3.0],
                    escort_spawns: vec![],
                    boss_destroyed: true,
                    killer_char_id: None,
                    killer_fleet_id: None,
                }],
                lockout_until_filetime: None,
                tale_stop_published: true,
            },
        );
    });

    // Homefront match by instance_id and dungeon_id
    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_id, 800_012, None),
        Some(DVec3::new(70_000.0, 2_000.0, 0.0))
    );
    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_id, 5901, None),
        Some(DVec3::new(70_000.0, 2_000.0, 0.0))
    );
    // Completed homefront ignored
    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_id, 800_013, None),
        None
    );

    // Triglavian match by instance_id and dungeon_id
    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_id, 810_005, None),
        Some(DVec3::new(22_500.0, 0.0, 0.0))
    );
    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_id, 6100, None),
        Some(DVec3::new(22_500.0, 0.0, 0.0))
    );

    // Incursion match by site_id string and dungeon_id; Ended incursion ignored
    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_id, 0, Some("INC-SITE-1")),
        Some(DVec3::new(50_000.0, 60_000.0, 70_000.0))
    );
    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_id, 6200, None),
        Some(DVec3::new(50_000.0, 60_000.0, 70_000.0))
    );
    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_id, 6299, Some("INC-ENDED")),
        None
    );

    // Drifter hive active -> (10_000, 0, 0), cleared -> None
    with_pve_special_reg(&ctx, |reg| {
        reg.drifter_hives.insert(
            sys_id,
            DrifterHiveRecord {
                site_id: "HIVE-1".into(),
                dungeon_id: 7001,
                dungeon_name: "Redoubt".into(),
                solar_system_id: sys_id,
                element_type_id: 34_575,
                element_name: "Antikythera".into(),
                current_room: 1,
                total_rooms: 3,
                warden_npc_ball_id: 9,
                cleared: false,
                rooms: vec![],
            },
        );
    });
    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_id, 999_999, None),
        Some(DVec3::new(10_000.0, 0.0, 0.0))
    );
    with_pve_special_reg(&ctx, |reg| {
        reg.drifter_hives.get_mut(&sys_id).unwrap().cleared = true;
    });
    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_id, 999_999, None),
        None
    );
}

#[test]
fn find_active_dungeon_id_at_position_covers_entry_room_filters_and_exploration_fallback() {
    let ctx = make_ctx();
    let sys_id = 30_000_142u32;

    assert_eq!(
        find_active_dungeon_id_at_position(&ctx, sys_id, [0.0, 0.0, 0.0]),
        None
    );

    let entry_pos = [1_000_000.0, 0.0, 0.0];
    let room_pos = [5_000_000.0, 0.0, 0.0];
    with_dungeon_runtime_store(&ctx, |store| {
        // Completed instance should be skipped
        store.instances.insert(
            1,
            DungeonRuntimeInstance {
                instance_id: 1,
                dungeon_id: 111,
                dungeon_name: "Done".into(),
                category: "Deadspace".into(),
                archetype: "Deadspace".into(),
                solar_system_id: sys_id,
                owner_char_id: 90_000_001,
                base_position: entry_pos,
                disallow_mwd: false,
                entry_gate_ball_id: None,
                entry_gate_pos: Some(entry_pos),
                entry_gate_locked: false,
                current_room_index: 1,
                rooms: vec![],
                completed: true,
                despawned: false,
                active_objective_id: 0,
                last_completed_task_id: None,
                completed_task_ids: vec![],
                created_unix: 1,
            },
        );
        // Active instance matching entry_gate_pos & room_pos
        store.instances.insert(
            2,
            DungeonRuntimeInstance {
                instance_id: 2,
                dungeon_id: 222,
                dungeon_name: "Active".into(),
                category: "Deadspace".into(),
                archetype: "Deadspace".into(),
                solar_system_id: sys_id,
                owner_char_id: 90_000_001,
                base_position: entry_pos,
                disallow_mwd: false,
                entry_gate_ball_id: None,
                entry_gate_pos: Some(entry_pos),
                entry_gate_locked: false,
                current_room_index: 1,
                rooms: vec![DungeonRuntimeRoom {
                    room_index: 1,
                    room_id: 500,
                    room_name: "R1".into(),
                    position: room_pos,
                    gate_destination_pos: room_pos,
                    gated: false,
                    disallow_mwd: None,
                    gate_ball_id: None,
                    gate_locked: false,
                    objects: vec![],
                    npc_groups: vec![],
                    triggers: vec![],
                    spawned: true,
                    cleared: false,
                }],
                completed: false,
                despawned: false,
                active_objective_id: 0,
                last_completed_task_id: None,
                completed_task_ids: vec![],
                created_unix: 1,
            },
        );
    });

    assert_eq!(
        find_active_dungeon_id_at_position(&ctx, sys_id, [1_050_000.0, 0.0, 0.0]),
        Some(222)
    );
    assert_eq!(
        find_active_dungeon_id_at_position(&ctx, sys_id, [5_100_000.0, 0.0, 0.0]),
        Some(222)
    );

    // Exploration site fallback when no runtime instance is nearby
    let exp_pos = [9_000_000.0, 0.0, 0.0];
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            sys_id,
            vec![
                PveSiteInstance {
                    site_id: "CLEARED".into(),
                    dungeon_id: 333,
                    position: exp_pos,
                    cleared: true,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "ACTIVE".into(),
                    dungeon_id: 444,
                    position: exp_pos,
                    cleared: false,
                    ..Default::default()
                },
            ],
        );
    });
    assert_eq!(
        find_active_dungeon_id_at_position(&ctx, sys_id, [9_010_000.0, 0.0, 0.0]),
        Some(444)
    );
    assert_eq!(
        find_active_dungeon_id_at_position(&ctx, sys_id, [99_000_000.0, 0.0, 0.0]),
        None
    );
}

#[test]
fn resolve_fw_or_special_warp_target_withdrawing_incursion_and_system_isolation() {
    let ctx = make_ctx();
    let sys_a = 30_000_142_u32;
    let sys_b = 30_000_144_u32;
    let fw_pos = [42_000.0, 12_000.0, -8_000.0];
    let inc_pos = [91_000.0, 22_000.0, 15_000.0];

    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            sys_a,
            SystemDynamicState {
                solar_system_id: sys_a,
                fw_complex_position: Some(fw_pos),
                ..Default::default()
            },
        );
        snap.fw_runtime
            .fw_complexes
            .insert(710_001, (sys_a, "Small".into(), 500_001, 0.0, false));
    });

    with_pve_special_reg(&ctx, |reg| {
        reg.incursions.insert(
            20_000_030,
            IncursionTaleRecord {
                tale_id: 3,
                constellation_id: 20_000_030,
                constellation_name: "WithdrawCon".into(),
                staging_solar_system_id: sys_a,
                vanguard_solar_system_ids: vec![sys_a],
                assault_solar_system_ids: vec![],
                hq_solar_system_id: sys_a,
                influence: 0.2,
                state: "Withdrawing".into(),
                mothership_spawned: false,
                mothership_defeated: false,
                sites: vec![IncursionSiteEntry {
                    site_id: "INC-WITHDRAW".into(),
                    dungeon_id: 6301,
                    dungeon_name: "Withdrawing Vanguard".into(),
                    solar_system_id: sys_a,
                    tier: "Vanguard".into(),
                    min_pilots: 5,
                    max_optimal_pilots: 10,
                    base_isk: 1_000_000,
                    base_concord_lp: 1_000,
                    influence_delta: 0.01,
                    locked_until_low_influence: false,
                    completed_count: 0,
                    boss_npc_ball_id: 10,
                    boss_type_id: 2933,
                    position: [5.0, 6.0, 7.0],
                    escort_spawns: vec![],
                    boss_destroyed: false,
                    killer_char_id: None,
                    killer_fleet_id: None,
                }],
                lockout_until_filetime: None,
                tale_stop_published: false,
            },
        );
        reg.incursions.insert(
            20_000_031,
            IncursionTaleRecord {
                tale_id: 4,
                constellation_id: 20_000_031,
                constellation_name: "ActiveCon".into(),
                staging_solar_system_id: sys_a,
                vanguard_solar_system_ids: vec![sys_a],
                assault_solar_system_ids: vec![],
                hq_solar_system_id: sys_a,
                influence: 0.5,
                state: "Established".into(),
                mothership_spawned: false,
                mothership_defeated: false,
                sites: vec![IncursionSiteEntry {
                    site_id: "INC-ACTIVE".into(),
                    dungeon_id: 6302,
                    dungeon_name: "Active Vanguard".into(),
                    solar_system_id: sys_a,
                    tier: "Vanguard".into(),
                    min_pilots: 5,
                    max_optimal_pilots: 10,
                    base_isk: 10_000_000,
                    base_concord_lp: 1_400,
                    influence_delta: 0.01,
                    locked_until_low_influence: false,
                    completed_count: 0,
                    boss_npc_ball_id: 11,
                    boss_type_id: 2933,
                    position: inc_pos,
                    escort_spawns: vec![],
                    boss_destroyed: false,
                    killer_char_id: None,
                    killer_fleet_id: None,
                }],
                lockout_until_filetime: None,
                tale_stop_published: false,
            },
        );
    });

    // Wrong system does not resolve FW complex or Incursion site.
    assert_eq!(resolve_fw_or_special_warp_target(&ctx, sys_b, 710_001, None), None);
    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_b, 6302, Some("INC-ACTIVE")),
        None
    );
    // Withdrawing incursion is filtered out.
    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_a, 6301, Some("INC-WITHDRAW")),
        None
    );
    // Active same-system FW complex & Established incursion return exact non-zero coordinates.
    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_a, 710_001, None),
        Some(DVec3::from_array(fw_pos))
    );
    assert_eq!(
        resolve_fw_or_special_warp_target(&ctx, sys_a, 0, Some("INC-ACTIVE")),
        Some(DVec3::from_array(inc_pos))
    );
}
