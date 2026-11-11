#![allow(unused_imports)]
use super::*;
use crate::router::{PendingMachoNotification, ServerContext};
use crate::services::pve_special_sites::{
    DrifterHiveRecord, DrifterHiveRoom, HomefrontSiteRecord, PveSpecialSitesRegistry,
    SeasonalEventSiteRecord, TriglavianSiteRecord, with_pve_special_reg,
};
use crate::services::season_fsd::{SEASON_CONFIG_KEY, SeasonConfig};
use eve_db::{CosmicAnomalyRecord, IskCents, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_sde::{SolarSystem, SdeRegistry};
use glam::DVec3;
use std::sync::Arc;

fn build_combat_test_ctx() -> ServerContext {
    let mut sde = SdeRegistry::with_canonical_seed();
    for (sid, name) in [(30_000_142_u32, "Jita"), (30_002_187_u32, "Amarr")] {
        sde.solar_systems.entry(sid).or_insert_with(|| SolarSystem {
            solar_system_id: sid,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            name: name.into(),
            security_status: 0.9459,
            radius: 1.0e13,
            galactic_pos_ly: [1.0, 0.0, 0.0],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        });
    }
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

fn seed_char(ctx: &ServerContext, cid: i64, name: &str, sys_id: u32) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut template = snap.characters.values().next().unwrap().clone();
        template.character_id = cid;
        template.name = name.into();
        template.solar_system_id = sys_id;
        template.station_id = None;
        template.structure_id = None;
        template.fleet_id = None;
        template.active_ship_id = 0;
        template.balance = IskCents(0);
        template.loyalty_points.clear();
        snap.characters.insert(cid, template);
    });
}

fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.0,
        mass: 1.0e6,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drifter_hive_lookup_and_notify_account_changes() {
    let ctx = build_combat_test_ctx();
    crate::services::pve_special_sites::ensure_special_sites_spawned(&ctx).await;
    let all = get_all_drifter_hives(&ctx);
    assert!(!all.is_empty());
    let first = &all[0];
    let found = get_drifter_hive_for_system(&ctx, first.solar_system_id).expect("hive lookup matches");
    assert_eq!(found.solar_system_id, first.solar_system_id);
    assert!(get_drifter_hive_for_system(&ctx, 30_000_142).is_none());

    let c1 = 91_000_001;
    let c2 = 91_000_002;
    seed_char(&ctx, c1, "Account One", 30_000_142);
    seed_char(&ctx, c2, "Account Two", 30_000_142);

    // Trigger queue truncation branch on c1 before notifying
    {
        let mut map = ctx.pending_notifications.write().unwrap();
        let q = map.entry(c1).or_default();
        let (pkt, marsh) = ServerContext::build_macho_notification_packet(
            c1,
            None,
            "OnSlimItemChange",
            "solarsystemid",
            vec![],
        );
        for _ in 0..crate::router::cache_notifications::MAX_PENDING_NOTIFICATIONS_PER_CHAR {
            q.push(PendingMachoNotification { seq_id: 0,
                recipient_char_id: c1,
                event: "OnSlimItemChange".into(),
                id_type: "solarsystemid".into(),
                args: vec![],
                packet: pkt.clone(),
                marshaled_packet: marsh.clone(),
                created_filetime: 0,
            });
        }
    }

    notify_special_site_account_changes(
        &ctx.db,
        &ctx.pending_notifications,
        &ctx.notify_tx,
        &[(c1, 500_000.0), (c2, 250_000.0)],
    );
    let q1 = ctx.drain_notifications_for_char(c1);
    let q2 = ctx.drain_notifications_for_char(c2);
    assert_eq!(
        q1.len(),
        crate::router::cache_notifications::MAX_PENDING_NOTIFICATIONS_PER_CHAR + 1
    );
    assert_eq!(q1.last().unwrap().event, "OnAccountChange");
    assert_eq!(q2.len(), 1);
    assert_eq!(q2[0].event, "OnAccountChange");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn homefront_npc_destroyed_covers_solo_fleet_filters_and_skip_cases() {
    let ctx = build_combat_test_ctx();
    let sys_id = 30_000_142;
    let killer_solo = 91_001_001;
    let fleet_leader = 91_001_002;
    let member_docked = 91_001_003;
    let member_offsys = 91_001_004;
    let member_struct = 91_001_005;
    let killer_docked_in_fleet = 91_001_006;

    for cid in [
        killer_solo,
        fleet_leader,
        member_docked,
        member_offsys,
        member_struct,
        killer_docked_in_fleet,
    ] {
        seed_char(&ctx, cid, &format!("Pilot {cid}"), sys_id);
    }

    let fleet_id = 77_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&member_docked).unwrap().station_id = Some(60_003_760);
        snap.characters.get_mut(&member_offsys).unwrap().solar_system_id = 30_002_187;
        snap.characters.get_mut(&member_struct).unwrap().structure_id = Some(1_000_000_123);
        snap.characters.get_mut(&killer_docked_in_fleet).unwrap().station_id = Some(60_003_760);
        for &cid in &[
            fleet_leader,
            member_docked,
            member_offsys,
            member_struct,
            killer_docked_in_fleet,
        ] {
            snap.characters.get_mut(&cid).unwrap().fleet_id = Some(fleet_id);
        }
    });

    with_pve_special_reg(&ctx, |reg: &mut PveSpecialSitesRegistry| {
        reg.homefront_sites.insert(
            sys_id,
            vec![
                HomefrontSiteRecord {
                    instance_id: 1,
                    dungeon_id: 10,
                    dungeon_name: "HF Done".into(),
                    solar_system_id: sys_id,
                    optimal_fleet_size: 5,
                    base_pilot_isk: 1_000_000,
                    completed: true,
                    reward_paid_isk: 1_000_000,
                    boss_npc_ball_id: 88_001,
                    boss_type_id: 1,
                },
                HomefrontSiteRecord {
                    instance_id: 2,
                    dungeon_id: 10,
                    dungeon_name: "HF NoKiller".into(),
                    solar_system_id: sys_id,
                    optimal_fleet_size: 5,
                    base_pilot_isk: 1_000_000,
                    completed: false,
                    reward_paid_isk: 0,
                    boss_npc_ball_id: 88_002,
                    boss_type_id: 1,
                },
                HomefrontSiteRecord {
                    instance_id: 3,
                    dungeon_id: 10,
                    dungeon_name: "HF Solo".into(),
                    solar_system_id: sys_id,
                    optimal_fleet_size: 5,
                    base_pilot_isk: 2_000_000,
                    completed: false,
                    reward_paid_isk: 0,
                    boss_npc_ball_id: 88_003,
                    boss_type_id: 1,
                },
                HomefrontSiteRecord {
                    instance_id: 4,
                    dungeon_id: 10,
                    dungeon_name: "HF Fleet".into(),
                    solar_system_id: sys_id,
                    optimal_fleet_size: 2,
                    base_pilot_isk: 3_000_000,
                    completed: false,
                    reward_paid_isk: 0,
                    boss_npc_ball_id: 88_004,
                    boss_type_id: 1,
                },
            ],
        );
    });

    assert!(
        on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, sys_id, 88_001, Some(killer_solo))
            .await
            .is_empty()
    );
    assert!(
        on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, sys_id, 88_002, None)
            .await
            .is_empty()
    );

    let solo_changes = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        88_003,
        Some(killer_solo),
    )
    .await;
    assert_eq!(solo_changes.len(), 1);
    let solo_isk = ctx.db.with_snapshot(|s| s.characters[&killer_solo].balance.0);
    assert_eq!(solo_isk, 2_000_000 * 100);

    let fleet_changes = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        88_004,
        Some(killer_docked_in_fleet),
    )
    .await;
    assert_eq!(fleet_changes.len(), 2);
    let (leader_isk, killer_isk, docked_isk, offsys_isk, struct_isk) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&fleet_leader].balance.0,
            s.characters[&killer_docked_in_fleet].balance.0,
            s.characters[&member_docked].balance.0,
            s.characters[&member_offsys].balance.0,
            s.characters[&member_struct].balance.0,
        )
    });
    assert_eq!(leader_isk, 3_000_000 * 100);
    assert_eq!(killer_isk, 3_000_000 * 100);
    assert_eq!(docked_isk, 0);
    assert_eq!(offsys_isk, 0);
    assert_eq!(struct_isk, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn triglavian_npc_destroyed_spawns_container_when_no_ship_and_handles_no_killer() {
    let ctx = build_combat_test_ctx();
    let sys_id = 30_000_142;
    let killer = 91_002_001;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    seed_char(&ctx, killer, "Trig Pilot", sys_id);

    for bid in [90_101_i64, 90_102, 90_103, 90_201, 90_202] {
        let _ = sys_handle
            .spawn_ball(make_ball(bid, 52_000, DVec3::new(1000.0, 0.0, 0.0)))
            .await;
    }

    with_pve_special_reg(&ctx, |reg| {
        reg.triglavian_sites.insert(
            sys_id,
            vec![
                TriglavianSiteRecord {
                    instance_id: 101,
                    dungeon_id: 201,
                    dungeon_name: "Trig Non-Trig Owner".into(),
                    solar_system_id: sys_id,
                    faction_owner_id: 500_026,
                    base_isk: 1_500_000,
                    survey_db_drop: 3,
                    completed: false,
                    boss_npc_ball_id: 90_101,
                    boss_type_id: 52_000,
                    escort_ball_ids: vec![90_102],
                    escort_type_ids: vec![52_001],
                    beacon_ball_id: 90_103,
                },
                TriglavianSiteRecord {
                    instance_id: 102,
                    dungeon_id: 202,
                    dungeon_name: "Trig No Killer".into(),
                    solar_system_id: sys_id,
                    faction_owner_id: 500_027,
                    base_isk: 1_500_000,
                    survey_db_drop: 2,
                    completed: false,
                    boss_npc_ball_id: 90_201,
                    boss_type_id: 52_000,
                    escort_ball_ids: vec![90_202],
                    escort_type_ids: vec![52_001],
                    beacon_ball_id: 0,
                },
            ],
        );
    });

    let changes = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        90_101,
        Some(killer),
    )
    .await;
    assert_eq!(changes.len(), 1);
    assert!(sys_handle.get_ball_snapshot(90_102).await.unwrap().is_none());
    let (balance, has_container) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&killer];
        let has_c = s.items.values().any(|i| i.type_id == 23 && i.owner_id == killer);
        (ch.balance.0, has_c)
    });
    assert_eq!(balance, 1_500_000 * 100);
    assert!(has_container, "survey db should drop into a spawned cargo container");

    let no_killer_changes = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        90_201,
        None,
    )
    .await;
    assert!(no_killer_changes.is_empty());
    assert!(sys_handle.get_ball_snapshot(90_202).await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drifter_hive_multi_room_partial_wave_beacon_and_warden_without_ship_or_killer() {
    let ctx = build_combat_test_ctx();
    let sys_id = 30_000_142;
    let killer = 91_003_001;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    seed_char(&ctx, killer, "Hive Runner", sys_id);

    for bid in [91_101_i64, 91_102, 91_201, 91_300] {
        let _ = sys_handle
            .spawn_ball(make_ball(bid, 34_000, DVec3::new(500.0, 0.0, 0.0)))
            .await;
    }

    with_pve_special_reg(&ctx, |reg| {
        reg.drifter_hives.insert(
            sys_id,
            DrifterHiveRecord {
                site_id: "hive-1".into(),
                dungeon_id: 901,
                dungeon_name: "Conflux Hive".into(),
                solar_system_id: sys_id,
                element_type_id: 34_400,
                element_name: "Conflux Element".into(),
                current_room: 0,
                total_rooms: 2,
                warden_npc_ball_id: 91_300,
                cleared: false,
                rooms: vec![
                    DrifterHiveRoom {
                        room_index: 0,
                        room_name: "Entry".into(),
                        npc_ball_ids: vec![91_101, 91_102],
                        npc_type_ids: vec![34_001, 34_001],
                        beacon_ball_id: 91_901,
                        cleared: false,
                        destroyed_npc_ball_ids: vec![],
                    },
                    DrifterHiveRoom {
                        room_index: 1,
                        room_name: "Core".into(),
                        npc_ball_ids: vec![91_201],
                        npc_type_ids: vec![34_002],
                        beacon_ball_id: 91_902,
                        cleared: false,
                        destroyed_npc_ball_ids: vec![],
                    },
                ],
            },
        );
    });

    // Partial kill in room 0 -> room not cleared
    on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, sys_id, 91_101, Some(killer)).await;
    let room0_cleared = crate::services::pve_special_sites::read_pve_special_reg(&ctx, |r| {
        r.drifter_hives[&sys_id].rooms[0].cleared
    });
    assert!(!room0_cleared);

    // Second kill in room 0 -> clears room 0, spawns beacon 91_902 & room 1 NPC
    on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, sys_id, 91_102, Some(killer)).await;
    assert!(sys_handle.get_ball_snapshot(91_902).await.unwrap().is_some());

    // Warden killed with ship_id == 0 -> clears hive & spawns element container in space
    let changes = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        91_300,
        Some(killer),
    )
    .await;
    assert!(changes.is_empty());
    let (hive_cleared, has_elem) = ctx.db.with_snapshot(|s| {
        let has_e = s.items.values().any(|i| i.type_id == 34_400 && i.owner_id == killer);
        (
            crate::services::pve_special_sites::read_pve_special_reg(&ctx, |r| {
                r.drifter_hives[&sys_id].cleared
            }),
            has_e,
        )
    });
    assert!(hive_cleared);
    assert!(has_elem);

    // Warden killed with killer == None
    with_pve_special_reg(&ctx, |reg| {
        reg.drifter_hives.insert(
            30_002_187,
            DrifterHiveRecord {
                site_id: "hive-2".into(),
                dungeon_id: 902,
                dungeon_name: "Barbican".into(),
                solar_system_id: 30_002_187,
                element_type_id: 34_401,
                element_name: "Barbican Element".into(),
                current_room: 0,
                total_rooms: 1,
                warden_npc_ball_id: 92_300,
                cleared: false,
                rooms: vec![],
            },
        );
    });
    let no_k = on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, 30_002_187, 92_300, None).await;
    assert!(no_k.is_empty());
    assert!(crate::services::pve_special_sites::read_pve_special_reg(&ctx, |r| {
        r.drifter_hives[&30_002_187].cleared
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seasonal_site_npc_destroyed_and_ensure_seasonal_sites_edges() {
    let ctx = build_combat_test_ctx();
    let sys_id = 30_000_142;
    let killer = 91_004_001;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    seed_char(&ctx, killer, "Seasonal Pilot", sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies
            .entry(sys_id)
            .or_default()
            .push(CosmicAnomalyRecord {
                anomaly_id: "dng-900001-501".into(),
                solar_system_id: sys_id,
                name: "Crimson Gauntlet".into(),
                anomaly_type: "Combat".into(),
                position: [100.0, 200.0, 300.0],
                signal_strength: 1.0,
                ore_types: vec![],
                remaining_volume: 0.0,
                created_unix: 10,
                lifetime_seconds: 3600,
                cleared: false,
            });
        snap.cosmic_anomalies
            .entry(sys_id)
            .or_default()
            .push(CosmicAnomalyRecord {
                anomaly_id: "dng-900002-502".into(),
                solar_system_id: sys_id,
                name: "Expired Site".into(),
                anomaly_type: "Combat".into(),
                position: [0.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: vec![],
                remaining_volume: 0.0,
                created_unix: 10,
                lifetime_seconds: 3600,
                cleared: false,
            });
    });

    for bid in [95_001_i64, 95_002, 95_011, 95_012, 95_021] {
        let _ = sys_handle
            .spawn_ball(make_ball(bid, 60_000, DVec3::new(10.0, 20.0, 30.0)))
            .await;
    }

    let now_ft = ctx.now_blueos_filetime_ticks();
    with_pve_special_reg(&ctx, |reg| {
        reg.seasonal_event_sites.insert(
            sys_id,
            vec![
                SeasonalEventSiteRecord {
                    instance_id: 501,
                    season_id: 77,
                    dungeon_id: 900_001,
                    dungeon_name: "Crimson Gauntlet".into(),
                    solar_system_id: sys_id,
                    window_start_ft: now_ft - 1_000_000_000,
                    window_end_ft: now_ft + 1_000_000_000,
                    boss_npc_ball_id: 95_001,
                    boss_type_id: 60_001,
                    escort_ball_ids: vec![95_002],
                    reward_isk: 500_000,
                    reward_points: 20,
                    loot_type_id: 0,
                    completed: false,
                },
                SeasonalEventSiteRecord {
                    instance_id: 502,
                    season_id: 77,
                    dungeon_id: 900_002,
                    dungeon_name: "Expired Site".into(),
                    solar_system_id: sys_id,
                    window_start_ft: 100,
                    window_end_ft: now_ft - 1000,
                    boss_npc_ball_id: 95_021,
                    boss_type_id: 60_002,
                    escort_ball_ids: vec![],
                    reward_isk: 100_000,
                    reward_points: 5,
                    loot_type_id: 0,
                    completed: false,
                },
                SeasonalEventSiteRecord {
                    instance_id: 503,
                    season_id: 77,
                    dungeon_id: 900_003,
                    dungeon_name: "No Killer Site".into(),
                    solar_system_id: sys_id,
                    window_start_ft: now_ft - 1000,
                    window_end_ft: now_ft + 1_000_000_000,
                    boss_npc_ball_id: 95_011,
                    boss_type_id: 60_003,
                    escort_ball_ids: vec![95_012],
                    reward_isk: 200_000,
                    reward_points: 10,
                    loot_type_id: 0,
                    completed: false,
                },
            ],
        );
    });

    let c_sea = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        95_001,
        Some(killer),
    )
    .await;
    assert_eq!(c_sea.len(), 1);
    assert!(sys_handle.get_ball_snapshot(95_002).await.unwrap().is_none());
    let cleared_501 = ctx.db.with_snapshot(|s| {
        s.cosmic_anomalies
            .get(&sys_id)
            .is_some_and(|v| v.iter().any(|a| a.anomaly_id == "dng-900001-501" && a.cleared))
    });
    assert!(cleared_501, "seasonal anomaly should be marked cleared when boss dies");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&killer].balance.0), 500_000 * 100);

    let c_nok = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        95_011,
        None,
    )
    .await;
    assert!(c_nok.is_empty());
    assert!(sys_handle.get_ball_snapshot(95_012).await.unwrap().is_none());

    let future_cfg = SeasonConfig {
        season_id: 88,
        season_name: "Future Harvest".into(),
        start_time_ft: now_ft + 10_000_000_000,
        end_time_ft: now_ft + 20_000_000_000,
        ..SeasonConfig::default()
    };
    ctx.db.with_mut_snapshot(|s| {
        s.seasons_and_campaigns.season_configuration = serde_json::from_str(&String::from(serde_json::to_string(&future_cfg).unwrap())).ok();
    });
    let rem_future = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert!(rem_future.is_empty(), "completed and expired seasonal sites must be pruned");
    assert!(sys_handle.get_ball_snapshot(95_021).await.unwrap().is_none());

    let active_cfg = SeasonConfig {
        season_id: 89,
        season_name: "Winter Nexus".into(),
        start_time_ft: now_ft - 10_000_000_000,
        end_time_ft: now_ft + 10_000_000_000,
        ..SeasonConfig::default()
    };
    ctx.db.with_mut_snapshot(|s| {
        s.seasons_and_campaigns.season_configuration = serde_json::from_str(&String::from(serde_json::to_string(&active_cfg).unwrap())).ok();
    });
    let active1 = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert_eq!(active1.len(), 1);
    let active2 = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert_eq!(active2.len(), 1, "active seasonal site should not duplicate");

    ctx.db.with_mut_snapshot(|s| {
        s.seasons_and_campaigns.season_configuration = None;
    });
    let _ = ensure_seasonal_event_sites_for_system(&ctx, 30_002_187).await;
}
