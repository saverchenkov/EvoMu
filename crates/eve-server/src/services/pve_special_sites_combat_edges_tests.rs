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
    let u1 = 910_001_u64;
    let u2 = 910_002_u64;
    seed_char(&ctx, c1, "Account One", 30_000_142);
    seed_char(&ctx, c2, "Account Two", 30_000_142);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&c1).unwrap().user_id = u1;
        snap.characters.get_mut(&c2).unwrap().user_id = u2;
        for uid in [u1, u2] {
            snap.accounts.insert(
                format!("user_{uid}"),
                eve_db::AccountRecord {
                    user_id: uid,
                    username: format!("user_{uid}"),
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
    });
    ctx.db
        .acquire_online_lock(u1, 9_101, Some(c1), true, ctx.now_unix())
        .expect("c1 online");
    ctx.db
        .acquire_online_lock(u2, 9_102, Some(c2), true, ctx.now_unix())
        .expect("c2 online");
    let _ = ctx.drain_notifications_for_char(c1);
    let _ = ctx.drain_notifications_for_char(c2);

    // Trigger queue truncation branch on c1 before committing balance updates
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
                scene_scope: None,
            });
        }
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&c1).unwrap().balance = IskCents::from_isk(500_000);
        snap.characters.get_mut(&c2).unwrap().balance = IskCents::from_isk(250_000);
    });
    let q1 = ctx.drain_notifications_for_char(c1);
    let q2 = ctx.drain_notifications_for_char(c2);
    assert_eq!(
        q1.len(),
        crate::router::cache_notifications::MAX_PENDING_NOTIFICATIONS_PER_CHAR
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
    let member_entered_then_docked = 91_001_006;
    let fleet_wingman = 91_001_007;

    for cid in [
        killer_solo,
        fleet_leader,
        member_docked,
        member_offsys,
        member_struct,
        member_entered_then_docked,
        fleet_wingman,
    ] {
        seed_char(&ctx, cid, &format!("Pilot {cid}"), sys_id);
    }

    let fleet_id = 77_001;
    ctx.db.with_mut_snapshot(|snap| {
        for (idx, &cid) in [
            killer_solo,
            fleet_leader,
            member_docked,
            member_offsys,
            member_struct,
            member_entered_then_docked,
            fleet_wingman,
        ]
        .iter()
        .enumerate()
        {
            let sid = 1_090_001 + (idx as i64);
            snap.characters.get_mut(&cid).unwrap().active_ship_id = sid;
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: i64::from(sys_id),
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.characters.get_mut(&member_docked).unwrap().station_id = Some(60_003_760);
        snap.characters.get_mut(&member_offsys).unwrap().solar_system_id = 30_002_187;
        snap.characters.get_mut(&member_struct).unwrap().structure_id = Some(1_000_000_123);
        for &cid in &[
            fleet_leader,
            member_docked,
            member_offsys,
            member_struct,
            member_entered_then_docked,
            fleet_wingman,
        ] {
            snap.characters.get_mut(&cid).unwrap().fleet_id = Some(fleet_id);
        }
    });

    let make_sess = |cid: i64, sid: i64, s_sys: u32, st: Option<i64>, sr: Option<i64>| {
        let mut s = eve_machonet::SessionState::new(cid as u64);
        s.user_id = Some(cid as u64);
        s.character_id = Some(cid);
        s.solar_system_id = Some(s_sys);
        s.ship_id = Some(sid);
        s.station_id = st;
        s.structure_id = sr;
        s
    };

    let relay_id_of = |st: &crate::services::pve_homefront_sites::HomefrontRuntimeState| match st
        .progress
    {
        crate::services::pve_homefront_sites::HomefrontArchetypeProgress::SuspiciousSignal {
            relay_ball_id,
            ..
        } => relay_ball_id,
        ref other => panic!("expected SuspiciousSignal, got {other:?}"),
    };

    // 1. Already completed Homefront site -> skipped
    let ss_done =
        crate::services::pve_homefront_sites::spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_367)
            .expect("spawn ss_done");
    let relay_done = relay_id_of(&ss_done);
    ctx.db.with_mut_snapshot(|snap| {
        snap.dungeon_runtime
            .homefront_runtimes
            .get_mut(&ss_done.instance_id)
            .unwrap()
            .completed = true;
    });
    assert!(on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        relay_done,
        Some(killer_solo),
    )
    .await
    .is_empty());

    // 2. Unentered site with no killer/contributors -> returns empty
    let ss_empty =
        crate::services::pve_homefront_sites::spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_367)
            .expect("spawn ss_empty");
    let relay_empty = relay_id_of(&ss_empty);
    assert!(on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        relay_empty,
        None,
    )
    .await
    .is_empty());

    // 3. Solo entered + arrays hacked -> settles solo pilot with (4/5)^(5-1) scaling
    let ss_solo =
        crate::services::pve_homefront_sites::spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_367)
            .expect("spawn ss_solo");
    let relay_solo = relay_id_of(&ss_solo);
    let solo_sess = make_sess(killer_solo, 1_090_001, sys_id, None, None);
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball(make_ball(1_090_001, 587, ss_solo.site_position + DVec3::new(400.0, 0.0, 0.0)))
        .await;
    crate::services::pve_homefront_sites::activate_homefront_acceleration_gate(
        &ctx,
        &solo_sess,
        ss_solo.instance_id,
    )
    .await
    .expect("solo enters gate");
    ctx.db.with_mut_snapshot(|snap| {
        let rec = snap
            .dungeon_runtime
            .homefront_runtimes
            .get_mut(&ss_solo.instance_id)
            .unwrap();
        if let Some(c) = rec.contributions.get_mut(&killer_solo) {
            c.damage_dealt = 2_000.0;
        }
        if let eve_db::service_domains::pve_sites::HomefrontProgressRecord::SuspiciousSignal {
            arrays_hacked,
            vulnerability_window_secs,
            ..
        } = &mut rec.progress
        {
            *arrays_hacked = [true; 3];
            *vulnerability_window_secs = 90;
        }
    });
    let expected_solo = crate::services::pve_special_sites_ops::compute_homefront_combat_pilot_isk(
        ss_solo.blueprint.base_pilot_isk,
        ss_solo.blueprint.optimal_fleet_size,
        1,
    );
    let solo_changes = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        relay_solo,
        Some(killer_solo),
    )
    .await;
    assert_eq!(solo_changes.len(), 1);
    let solo_isk = ctx.db.with_snapshot(|s| s.characters[&killer_solo].balance.0);
    assert_eq!(solo_isk, expected_solo * 100);

    // 4. Fleet: docked, off-system, and structure members rejected at gate;
    //    member who entered then docked before completion is ALSO excluded from payout!
    let ss_fleet =
        crate::services::pve_homefront_sites::spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_367)
            .expect("spawn ss_fleet");
    let relay_fleet = relay_id_of(&ss_fleet);
    assert!(
        crate::services::pve_homefront_sites::activate_homefront_acceleration_gate(
            &ctx,
            &make_sess(member_docked, 1_090_003, sys_id, Some(60_003_760), None),
            ss_fleet.instance_id,
        )
        .await
        .is_err()
    );
    assert!(
        crate::services::pve_homefront_sites::activate_homefront_acceleration_gate(
            &ctx,
            &make_sess(member_offsys, 1_090_004, 30_002_187, None, None),
            ss_fleet.instance_id,
        )
        .await
        .is_err()
    );
    assert!(
        crate::services::pve_homefront_sites::activate_homefront_acceleration_gate(
            &ctx,
            &make_sess(member_struct, 1_090_005, sys_id, None, Some(1_000_000_123)),
            ss_fleet.instance_id,
        )
        .await
        .is_err()
    );

    for (cid, sid) in [
        (fleet_leader, 1_090_002_i64),
        (member_entered_then_docked, 1_090_006),
        (fleet_wingman, 1_090_007),
    ] {
        let _ = ctx
            .systems
            .get_or_spawn(sys_id)
            .spawn_ball(make_ball(sid, 587, ss_fleet.site_position + DVec3::new(400.0, 0.0, 0.0)))
            .await;
        crate::services::pve_homefront_sites::activate_homefront_acceleration_gate(
            &ctx,
            &make_sess(cid, sid, sys_id, None, None),
            ss_fleet.instance_id,
        )
        .await
        .expect("in-space fleet mate enters gate");
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .get_mut(&member_entered_then_docked)
            .unwrap()
            .station_id = Some(60_003_760);
        let rec = snap
            .dungeon_runtime
            .homefront_runtimes
            .get_mut(&ss_fleet.instance_id)
            .unwrap();
        for cid in [fleet_leader, member_entered_then_docked, fleet_wingman] {
            if let Some(c) = rec.contributions.get_mut(&cid) {
                c.damage_dealt = 2_000.0;
            }
        }
        if let eve_db::service_domains::pve_sites::HomefrontProgressRecord::SuspiciousSignal {
            arrays_hacked,
            vulnerability_window_secs,
            ..
        } = &mut rec.progress
        {
            *arrays_hacked = [true; 3];
            *vulnerability_window_secs = 90;
        }
    });

    let expected_fleet_two =
        crate::services::pve_special_sites_ops::compute_homefront_combat_pilot_isk(
            ss_fleet.blueprint.base_pilot_isk,
            ss_fleet.blueprint.optimal_fleet_size,
            2,
        );
    let fleet_changes = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        relay_fleet,
        Some(fleet_leader),
    )
    .await;
    assert_eq!(fleet_changes.len(), 2);
    let (leader_isk, wingman_isk, later_docked_isk, docked_isk, offsys_isk, struct_isk) =
        ctx.db.with_snapshot(|s| {
            (
                s.characters[&fleet_leader].balance.0,
                s.characters[&fleet_wingman].balance.0,
                s.characters[&member_entered_then_docked].balance.0,
                s.characters[&member_docked].balance.0,
                s.characters[&member_offsys].balance.0,
                s.characters[&member_struct].balance.0,
            )
        });
    assert_eq!(leader_isk, expected_fleet_two * 100);
    assert_eq!(wingman_isk, expected_fleet_two * 100);
    assert_eq!(later_docked_isk, 0);
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
    ctx.set_clock(Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(
        1_775_900_000,
    )));
    let sys_id = 30_000_142;
    let killer = 91_004_001;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    seed_char(&ctx, killer, "Seasonal Pilot", sys_id);
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        snap.seasons_and_campaigns.season_configuration = Some(SeasonConfig {
            season_id: 77,
            season_name: "Crimson Harvest".into(),
            start_time_ft: now_ft - 1_000_000_000,
            end_time_ft: now_ft + 1_000_000_000,
            ..SeasonConfig::default()
        });
        snap.cosmic_anomalies
            .entry(sys_id)
            .or_default()
            .push(CosmicAnomalyRecord {
                anomaly_id: "dng-900001-501".into(),
                solar_system_id: sys_id,
                name: "Crimson Gauntlet".into(),
                anomaly_type: "SeasonalEventSite".into(),
                position: [100.0, 200.0, 300.0],
                signal_strength: 1.0,
                ore_types: vec![],
                remaining_volume: 0.0,
                created_unix: 1_775_900_000,
                lifetime_seconds: 86_400,
                cleared: false,
            });
        snap.cosmic_anomalies
            .entry(sys_id)
            .or_default()
            .push(CosmicAnomalyRecord {
                anomaly_id: "dng-900002-502".into(),
                solar_system_id: sys_id,
                name: "Expired Site".into(),
                anomaly_type: "SeasonalEventSite".into(),
                position: [0.0, 0.0, 0.0],
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
                anomaly_id: "dng-900003-503".into(),
                solar_system_id: sys_id,
                name: "No Killer Site".into(),
                anomaly_type: "SeasonalEventSite".into(),
                position: [400.0, 500.0, 600.0],
                signal_strength: 1.0,
                ore_types: vec![],
                remaining_volume: 0.0,
                created_unix: 1_775_900_000,
                lifetime_seconds: 86_400,
                cleared: false,
            });
    });

    for bid in [95_001_i64, 95_002, 95_011, 95_012, 95_021] {
        let _ = sys_handle
            .spawn_ball(make_ball(bid, 60_000, DVec3::new(10.0, 20.0, 30.0)))
            .await;
    }

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

    let c_boss = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        95_001,
        Some(killer),
    )
    .await;
    assert!(c_boss.is_empty(), "boss kill must not pay out before escorts are destroyed");
    assert!(
        sys_handle.get_ball_snapshot(95_002).await.unwrap().is_some(),
        "live seasonal escort must not auto-despawn when boss dies"
    );
    let c_sea = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        95_002,
        Some(killer),
    )
    .await;
    assert_eq!(c_sea.len(), 1);
    let cleared_501 = ctx.db.with_snapshot(|s| {
        s.cosmic_anomalies
            .get(&sys_id)
            .is_some_and(|v| v.iter().any(|a| a.anomaly_id == "dng-900001-501" && a.cleared))
    });
    assert!(cleared_501, "seasonal anomaly should be marked cleared once full objectives complete");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&killer].balance.0), 500_000 * 100);

    assert!(
        on_special_site_npc_destroyed_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            sys_id,
            95_011,
            None,
        )
        .await
        .is_empty()
    );
    assert!(sys_handle.get_ball_snapshot(95_012).await.unwrap().is_some());
    let c_nok = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        95_012,
        None,
    )
    .await;
    assert!(c_nok.is_empty());

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

    // Cooldown must still block immediate respawn even if an active season is configured.
    let active_cfg = SeasonConfig {
        season_id: 89,
        season_name: "Winter Nexus".into(),
        start_time_ft: now_ft - 10_000_000_000,
        end_time_ft: now_ft + 100_000_000_000,
        ..SeasonConfig::default()
    };
    ctx.db.with_mut_snapshot(|s| {
        s.seasons_and_campaigns.season_configuration = serde_json::from_str(&String::from(serde_json::to_string(&active_cfg).unwrap())).ok();
    });
    let blocked_by_cooldown = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert!(
        blocked_by_cooldown.is_empty(),
        "per-system cooldown must prevent immediate respawn after seasonal site completion"
    );

    ctx.db
        .clock()
        .advance_secs(SEASONAL_RESPAWN_COOLDOWN_SECS + 5);
    let active1 = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert_eq!(active1.len(), 1);
    let active2 = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert_eq!(active2.len(), 1, "active seasonal site should not duplicate");

    ctx.db.with_mut_snapshot(|s| {
        s.seasons_and_campaigns.season_configuration = None;
    });
    let _ = ensure_seasonal_event_sites_for_system(&ctx, 30_002_187).await;
}

#[test]
fn seasonal_system_lock_isolated_per_repository_and_freed_on_drop() {
    let pair = eve_testkit::process_global_ratchet::TwoContextPair::from_factory(build_combat_test_ctx);
    let sys_id: SolarSystemId = 30_000_142;

    let left_lock = seasonal_system_lock(&pair.left().db, sys_id);
    let right_lock = seasonal_system_lock(&pair.right().db, sys_id);
    assert!(!Arc::ptr_eq(&left_lock, &right_lock));

    let left_guard = left_lock.try_lock().expect("left seasonal lock unlocked");
    assert!(
        left_lock.try_lock().is_err(),
        "same repo + system seasonal lock must serialize callers"
    );
    assert!(Arc::ptr_eq(
        &seasonal_system_lock(&pair.left().db, sys_id),
        &left_lock
    ));
    let right_guard = right_lock
        .try_lock()
        .expect("left seasonal lock must not block independent repository");

    drop(left_guard);
    drop(right_guard);
    drop(left_lock);
    drop(right_lock);

    let weak_slot = pair
        .left()
        .db
        .get_transient_runtime_slot::<SeasonalSystemLocksSlot>(SEASONAL_SYSTEM_LOCKS_SLOT_KEY)
        .flatten()
        .map(|slot| Arc::downgrade(&slot))
        .expect("SeasonalSystemLocksSlot initialized");
    assert!(weak_slot.upgrade().is_some());
    drop(pair);
    assert!(
        weak_slot.upgrade().is_none(),
        "dropping WorldRepository must free SeasonalSystemLocksSlot"
    );
}
