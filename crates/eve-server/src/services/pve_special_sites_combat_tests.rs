//! Unit tests for `pve_special_sites_combat`.

use super::*;
use crate::router::ServerContext;
use crate::services::pve_special_sites::{
    ensure_special_sites_initialized, TriglavianSiteRecord,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
use eve_sde::{SdeRegistry, SolarSystemId};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn special_site_npc_destruction_and_seasonal_sites() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let sys_id: SolarSystemId = 30_000_142;

    // Seed a 5-pilot fleet in space plus a solo pilot, with ship cargo capacity.
    let ship_tid = sde
        .types
        .values()
        .filter(|t| t.category_id == 6 && t.group_id == 25 && t.capacity > 50.0)
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(587);

    for idx in 1..=5i64 {
        let cid = 90_060_000 + idx;
        db.provision_gm_account(&format!("hf_pilot_{idx}"), "pw", cid, &format!("HfPilot{idx}"));
    }
    db.with_mut_snapshot(|snap| {
        for idx in 1..=5i64 {
            let cid = 90_060_000 + idx;
            let sid = 1_060_000_000 + idx;
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.fleet_id = Some(700);
                ch.active_ship_id = sid;
                ch.balance = IskCents::from_isk(100_000);
            }
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: ship_tid,
                    owner_id: cid,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_600_000)));
    ensure_special_sites_initialized(&ctx);

    // 1. Drifter Hive read helpers & room progression on NPC destruction
    assert!(get_drifter_hive_for_system(&ctx, 31_000_001).is_some());
    assert_eq!(get_all_drifter_hives(&ctx).len(), 5);

    let (r1_npcs, r2_npcs, r3_npcs, warden_bid) = {
        let h = get_drifter_hive_for_system(&ctx, 31_000_001).unwrap();
        (
            h.rooms[0].npc_ball_ids.clone(),
            h.rooms[1].npc_ball_ids.clone(),
            h.rooms[2].npc_ball_ids.clone(),
            h.warden_npc_ball_id,
        )
    };
    for bid in r1_npcs {
        let _ = on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, 31_000_001, bid, Some(90_060_001)).await;
    }
    assert_eq!(get_drifter_hive_for_system(&ctx, 31_000_001).unwrap().current_room, 2);

    for bid in r2_npcs {
        let _ = on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, 31_000_001, bid, Some(90_060_001)).await;
    }
    for bid in r3_npcs {
        let _ = on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, 31_000_001, bid, Some(90_060_001)).await;
    }
    let _ = on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, 31_000_001, warden_bid, Some(90_060_001)).await;
    assert!(get_drifter_hive_for_system(&ctx, 31_000_001).unwrap().cleared);

    // 2. Homefront Suspicious Signal relay kill settles full 5-pilot optimal payout after 3 arrays hacked
    let ss = crate::services::pve_homefront_sites::spawn_homefront_site_for_dungeon(
        &ctx,
        sys_id,
        10_367,
    )
    .expect("spawn Homefront Suspicious Signal");
    let relay_bid = match ss.progress {
        crate::services::pve_homefront_sites::HomefrontArchetypeProgress::SuspiciousSignal {
            relay_ball_id,
            ..
        } => relay_ball_id,
        ref other => panic!("expected SuspiciousSignal progress, got {other:?}"),
    };
    for idx in 1..=5i64 {
        let mut sess = eve_machonet::SessionState::new(idx as u64);
        sess.user_id = Some(idx as u64);
        sess.character_id = Some(90_060_000 + idx);
        sess.solar_system_id = Some(sys_id);
        let sid = 1_060_000_000 + idx;
        sess.ship_id = Some(sid);
        let _ = ctx
            .systems
            .get_or_spawn(sys_id)
            .spawn_ball(make_site_npc_ball(
                sid,
                587,
                ss.site_position + DVec3::new(400.0, 0.0, 0.0),
            ))
            .await;
        crate::services::pve_homefront_sites::activate_homefront_acceleration_gate(
            &ctx,
            &sess,
            ss.instance_id,
        )
        .await
        .expect("fleet pilot enters Suspicious Signal gate");
    }
    // Premature relay kill before arrays hacked must NOT settle site.
    assert!(on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        relay_bid,
        Some(90_060_001),
    )
    .await
    .is_empty());

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(rec) = snap.dungeon_runtime.homefront_runtimes.get_mut(&ss.instance_id) {
            for c in rec.contributions.values_mut() {
                c.damage_dealt = 2_000.0;
                c.arrays_hacked = 1;
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
        }
    });
    let p1_uid = ctx.db.with_snapshot(|s| s.characters[&90_060_001].user_id);
    ctx.db
        .acquire_online_lock(p1_uid, 9_001, Some(90_060_001), true, ctx.now_unix())
        .expect("p1 online");
    let _ = ctx.drain_notifications_for_char(90_060_001);
    let hf_changes = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        relay_bid,
        Some(90_060_001),
    )
    .await;
    assert_eq!(hf_changes.len(), 5);
    let p1_notifs = ctx.drain_notifications_for_char(90_060_001);
    assert!(p1_notifs.iter().any(|n| n.event == "OnAccountChange"));

    // 3. Triglavian boss kill removes escorts, adjusts standings, pays ISK & deposits Survey DBs
    let trig_boss_bid: i64 = 9_920_001;
    let trig_esc_bid: i64 = 9_920_002;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball(make_site_npc_ball(trig_esc_bid, 48_090, DVec3::ZERO)).await;
    with_pve_special_reg(&ctx, |reg| {
        reg.triglavian_sites.entry(sys_id).or_default().push(TriglavianSiteRecord {
            instance_id: 992_001,
            dungeon_id: 1,
            dungeon_name: "Emerging Conduit".into(),
            solar_system_id: sys_id,
            faction_owner_id: 500_027,
            base_isk: 3_000_000,
            survey_db_drop: 3,
            completed: false,
            boss_npc_ball_id: trig_boss_bid,
            boss_type_id: 48_122,
            escort_ball_ids: vec![trig_esc_bid],
            escort_type_ids: vec![48_090],
            beacon_ball_id: 0,
        });
    });
    let trig_changes = on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, sys_id, trig_boss_bid, Some(90_060_001)).await;
    assert_eq!(trig_changes.len(), 1);
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&90_060_001).unwrap();
        assert!((*ch.standings.get(&500_026).unwrap_or(&0.0) - 0.15).abs() < 1e-6);
    });

    // 4. Seasonal event site spawning & boss destruction & expiration
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        let mut cfg = crate::services::season_fsd::load_season_config(snap);
        cfg.start_time_ft = now_ft - 1_000_000;
        cfg.end_time_ft = now_ft + 10_000_000_000;
        snap.seasons_and_campaigns.season_configuration = Some(cfg);
    });
    let seasonal = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert_eq!(seasonal.len(), 1);
    assert_ne!(seasonal[0].dungeon_id, 47, "must select authentic SDE seasonal dungeon rather than stub 47");
    let details = crate::services::pve_special_sites_combat::get_seasonal_site_details(&ctx, seasonal[0].instance_id)
        .expect("companion seasonal details stored");
    assert_ne!(details.relay_ball_id, 0);
    assert_eq!(seasonal[0].escort_ball_ids.len(), 2);
    let s_boss = seasonal[0].boss_npc_ball_id;
    let s_escorts = seasonal[0].escort_ball_ids.clone();
    let boss_only = on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, sys_id, s_boss, Some(90_060_001)).await;
    assert!(boss_only.is_empty(), "boss kill alone must not pay out while escorts survive");
    for (idx, &ebid) in s_escorts.iter().enumerate() {
        let s_changes = on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, sys_id, ebid, Some(90_060_001)).await;
        if idx + 1 < s_escorts.len() {
            assert!(s_changes.is_empty());
        } else {
            assert_eq!(s_changes.len(), 1);
        }
    }
    ctx.db.with_snapshot(|snap| {
        let st = crate::services::season_manager::load_season_state(snap, 90_060_001);
        assert!(st.season_points >= seasonal[0].reward_points);
        assert!(seasonal[0].reward_points > 0);
        assert!(
            sde.types.len() <= 100
                || crate::services::season_fsd::fsd_challenge_definitions(&sde)
                    .iter()
                    .any(|d| d.points == seasonal[0].reward_points),
            "seasonal reward_points ({}) must match an FSD challenge definition",
            seasonal[0].reward_points
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn special_site_edge_cases_no_killer_edencom_overflow_and_seasonal_expiry() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let sys_id: SolarSystemId = 30_000_142;

    // Pilot 1: solo in space with NO ship (active_ship_id = 0) so loot overflows to container ball!
    // Pilot 2: docked in same fleet (excluded from Homefront payout!)
    // Pilot 3: in different solar system in same fleet (excluded from Homefront payout!)
    let solo_cid = 90_060_101_i64;
    let docked_cid = 90_060_102_i64;
    let remote_cid = 90_060_103_i64;
    db.provision_gm_account("solo_p", "pw", solo_cid, "SoloP");
    db.provision_gm_account("docked_p", "pw", docked_cid, "DockedP");
    db.provision_gm_account("remote_p", "pw", remote_cid, "RemoteP");

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&solo_cid) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.fleet_id = Some(701);
            ch.active_ship_id = 0;
            ch.balance = IskCents::from_isk(10_000);
        }
        if let Some(ch) = snap.characters.get_mut(&docked_cid) {
            ch.solar_system_id = sys_id;
            ch.station_id = Some(60_003_760);
            ch.fleet_id = Some(701);
            ch.balance = IskCents::from_isk(10_000);
        }
        if let Some(ch) = snap.characters.get_mut(&remote_cid) {
            ch.solar_system_id = 30_000_144;
            ch.station_id = None;
            ch.fleet_id = Some(701);
            ch.balance = IskCents::from_isk(10_000);
        }
    });

    let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_600_000)));
    ensure_special_sites_initialized(&ctx);

    // 1. get_drifter_hive_for_system on non-hive system -> None
    assert!(get_drifter_hive_for_system(&ctx, sys_id).is_none());

    // 2. Unknown ball ID returns empty vec
    assert!(on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        999_999_999,
        Some(solo_cid),
    )
    .await
    .is_empty());

    // 3. Homefront Suspicious Signal: killer_char_id = None with no entered/contributing pilot
    //    does NOT complete; docked & remote fleet mates cannot enter gate; solo killer settles cleanly.
    let ss2 = crate::services::pve_homefront_sites::spawn_homefront_site_for_dungeon(
        &ctx,
        sys_id,
        10_367,
    )
    .expect("spawn Homefront Suspicious Signal edge case");
    let hf_bid = match ss2.progress {
        crate::services::pve_homefront_sites::HomefrontArchetypeProgress::SuspiciousSignal {
            relay_ball_id,
            ..
        } => relay_ball_id,
        ref other => panic!("expected SuspiciousSignal progress, got {other:?}"),
    };

    // Docked & off-system fleet mates are blocked at the Homefront acceleration gate
    let mut docked_sess = eve_machonet::SessionState::new(202);
    docked_sess.user_id = Some(202);
    docked_sess.character_id = Some(docked_cid);
    docked_sess.solar_system_id = Some(sys_id);
    docked_sess.station_id = Some(60_003_760);
    assert_eq!(
        crate::services::pve_homefront_sites::activate_homefront_acceleration_gate(
            &ctx,
            &docked_sess,
            ss2.instance_id,
        )
        .await
        .expect_err("docked fleet mate blocked")
        .code,
        "ShipDocked"
    );

    let mut remote_sess = eve_machonet::SessionState::new(203);
    remote_sess.user_id = Some(203);
    remote_sess.character_id = Some(remote_cid);
    remote_sess.solar_system_id = Some(30_000_144);
    remote_sess.ship_id = Some(1_060_103);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            1_060_103,
            InventoryItemRecord {
                item_id: 1_060_103,
                type_id: 587,
                owner_id: remote_cid,
                location_id: 30_000_144,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        crate::services::pve_homefront_sites::activate_homefront_acceleration_gate(
            &ctx,
            &remote_sess,
            ss2.instance_id,
        )
        .await
        .expect_err("off-system fleet mate blocked")
        .code,
        "WrongSolarSystem"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(rec) = snap.dungeon_runtime.homefront_runtimes.get_mut(&ss2.instance_id) {
            if let eve_db::service_domains::pve_sites::HomefrontProgressRecord::SuspiciousSignal {
                arrays_hacked,
                vulnerability_window_secs,
                ..
            } = &mut rec.progress
            {
                *arrays_hacked = [true; 3];
                *vulnerability_window_secs = 90;
            }
        }
    });
    assert!(on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        hf_bid,
        None,
    )
    .await
    .is_empty());

    // Enter solo pilot via temporary Rifter ship item, then restore active_ship_id = 0 for section 4
    let temp_ship = 1_060_101_i64;
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball(make_site_npc_ball(
            temp_ship,
            587,
            ss2.site_position + DVec3::new(400.0, 0.0, 0.0),
        ))
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&solo_cid).unwrap().active_ship_id = temp_ship;
        snap.items.insert(
            temp_ship,
            InventoryItemRecord {
                item_id: temp_ship,
                type_id: 587,
                owner_id: solo_cid,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut solo_sess = eve_machonet::SessionState::new(201);
    solo_sess.user_id = Some(201);
    solo_sess.character_id = Some(solo_cid);
    solo_sess.solar_system_id = Some(sys_id);
    solo_sess.ship_id = Some(temp_ship);
    crate::services::pve_homefront_sites::activate_homefront_acceleration_gate(
        &ctx,
        &solo_sess,
        ss2.instance_id,
    )
    .await
    .expect("solo pilot enters Suspicious Signal");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&solo_cid).unwrap().active_ship_id = 0;
        if let Some(rec) = snap.dungeon_runtime.homefront_runtimes.get_mut(&ss2.instance_id) {
            if let Some(c) = rec.contributions.get_mut(&solo_cid) {
                c.damage_dealt = 2_000.0;
            }
        }
    });

    let hf_paid = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        hf_bid,
        Some(solo_cid),
    )
    .await;
    assert_eq!(hf_paid.len(), 1);
    assert_eq!(hf_paid[0].0, solo_cid);
    // Already completed Homefront site does not pay twice
    assert!(on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        hf_bid,
        Some(solo_cid),
    )
    .await
    .is_empty());

    // 4. Triglavian site with faction_owner_id == 500_026 (grants standing to 500_027, loses 500_026)
    //    and active_ship_id == 0 spawns container ball in ballpark!
    let trig_eden_bid = 9_940_001_i64;
    with_pve_special_reg(&ctx, |reg| {
        reg.triglavian_sites.entry(sys_id).or_default().push(TriglavianSiteRecord {
            instance_id: 994_001,
            dungeon_id: 2,
            dungeon_name: "EDENCOM Minor".into(),
            solar_system_id: sys_id,
            faction_owner_id: 500_026,
            base_isk: 1_500_000,
            survey_db_drop: 2,
            completed: false,
            boss_npc_ball_id: trig_eden_bid,
            boss_type_id: 48_122,
            escort_ball_ids: Vec::new(),
            escort_type_ids: Vec::new(),
            beacon_ball_id: 0,
        });
    });
    let trig_res = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        trig_eden_bid,
        Some(solo_cid),
    )
    .await;
    assert_eq!(trig_res.len(), 1);
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&solo_cid).unwrap();
        assert!((*ch.standings.get(&500_027).unwrap_or(&0.0) - 0.15).abs() < 1e-6);
    });
    // Also verify Triglavian with killer_char_id = None removes escorts & completes without payout
    let trig_nokill_bid = 9_940_002_i64;
    let trig_nokill_esc = 9_940_003_i64;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(make_site_npc_ball(trig_nokill_esc, 48_090, DVec3::ZERO))
        .await;
    with_pve_special_reg(&ctx, |reg| {
        reg.triglavian_sites.entry(sys_id).or_default().push(TriglavianSiteRecord {
            instance_id: 994_002,
            dungeon_id: 3,
            dungeon_name: "No Killer".into(),
            solar_system_id: sys_id,
            faction_owner_id: 500_027,
            base_isk: 1_000_000,
            survey_db_drop: 1,
            completed: false,
            boss_npc_ball_id: trig_nokill_bid,
            boss_type_id: 48_122,
            escort_ball_ids: vec![trig_nokill_esc],
            escort_type_ids: vec![48_090],
            beacon_ball_id: 0,
        });
    });
    assert!(on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        trig_nokill_bid,
        None,
    )
    .await
    .is_empty());

    // 5. Drifter Hive: duplicate destruction of same room NPC & warden with None killer
    let hive_sys = 31_000_002_u32;
    let (h_r1_first, h_warden) = {
        let h = get_drifter_hive_for_system(&ctx, hive_sys).unwrap();
        (h.rooms[0].npc_ball_ids[0], h.warden_npc_ball_id)
    };
    let _ = on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, hive_sys, h_r1_first, None).await;
    let _ = on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, hive_sys, h_r1_first, None).await;
    assert_eq!(get_drifter_hive_for_system(&ctx, hive_sys).unwrap().current_room, 1);
    let _ = on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, hive_sys, h_warden, None).await;
    assert!(get_drifter_hive_for_system(&ctx, hive_sys).unwrap().cleared);

    // 6. Seasonal event site idempotency, killer_char_id = None, and expiration cleanup
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        let mut cfg = crate::services::season_fsd::load_season_config(snap);
        cfg.start_time_ft = now_ft - 1_000_000;
        cfg.end_time_ft = now_ft + 10_000_000_000;
        snap.seasons_and_campaigns.season_configuration = serde_json::from_str(&String::from(serde_json::to_string(&cfg).unwrap())).ok();
    });
    let s1 = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert_eq!(s1.len(), 1);
    // Idempotent second call returns same site without duplicating
    let s2 = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert_eq!(s2.len(), 1);
    assert_eq!(s1[0].instance_id, s2[0].instance_id);

    // Expire window by advancing clock past s1[0].window_end_ft -> ensure_seasonal_event_sites_for_system removes expired site, balls, and anomaly
    let expired_unix = 1_775_600_000_u64 + 10_000_000;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(expired_unix)));
    let future_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        let mut cfg = crate::services::season_fsd::load_season_config(snap);
        cfg.start_time_ft = future_ft + 10_000_000;
        cfg.end_time_ft = future_ft + 20_000_000;
        snap.seasons_and_campaigns.season_configuration = serde_json::from_str(&String::from(serde_json::to_string(&cfg).unwrap())).ok();
    });
    let after_expiry = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert!(after_expiry.is_empty());
    let anom_key = format!("dng-{}-{}", s1[0].dungeon_id, s1[0].instance_id);
    assert!(ctx.db.with_snapshot(|snap| {
        !snap
            .cosmic_anomalies
            .get(&sys_id)
            .is_some_and(|l| l.iter().any(|a| a.anomaly_id == anom_key))
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seasonal_boss_consecutive_kills_allocate_disjoint_item_ids_for_both_pilots() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let sys_1: SolarSystemId = 30_000_142;
    let sys_2: SolarSystemId = 30_000_144;
    let pilot_1: i64 = 90_060_201;
    let pilot_2: i64 = 90_060_202;
    let ship_1: i64 = 1_060_201;
    let ship_2: i64 = 1_060_202;

    let ship_tid = sde
        .types
        .values()
        .filter(|t| t.category_id == 6 && t.group_id == 25 && t.capacity > 50.0)
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(587);

    let extra_pool: Vec<u32> = sde
        .types
        .keys()
        .copied()
        .filter(|tid| *tid != 9950 && *tid != 15457 && *tid != 23 && *tid != ship_tid)
        .take(3)
        .collect();

    db.provision_gm_account("season_p1", "pw", pilot_1, "SeasonP1");
    db.provision_gm_account("season_p2", "pw", pilot_2, "SeasonP2");

    let ctx = ServerContext::with_sde_and_repository(sde, db);
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid, sys) in [(pilot_1, ship_1, sys_1), (pilot_2, ship_2, sys_2)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = sys;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = sid;
            }
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: ship_tid,
                    owner_id: cid,
                    location_id: i64::from(sys),
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        let mut cfg = crate::services::season_fsd::load_season_config(snap);
        cfg.start_time_ft = now_ft - 1_000_000;
        cfg.end_time_ft = now_ft + 10_000_000_000;
        snap.seasons_and_campaigns.season_configuration = Some(cfg);
    });

    let sites_1 = ensure_seasonal_event_sites_for_system(&ctx, sys_1).await;
    let sites_2 = ensure_seasonal_event_sites_for_system(&ctx, sys_2).await;
    assert_eq!(sites_1.len(), 1);
    assert_eq!(sites_2.len(), 1);

    // Enable hacked relay crate pool on both sites so each boss kill awards multiple distinct items
    ctx.db.with_mut_snapshot(|snap| {
        let mut dmap = load_seasonal_site_details_map(snap);
        for (iid, ltid) in [
            (sites_1[0].instance_id, sites_1[0].loot_type_id),
            (sites_2[0].instance_id, sites_2[0].loot_type_id),
        ] {
            if let Some(d) = dmap.get_mut(&iid) {
                d.relay_hacked = true;
                if d.crate_loot_pool.is_empty() {
                    for &ptid in extra_pool.iter().filter(|t| **t != ltid).take(2) {
                        d.crate_loot_pool.push((ptid, 5));
                    }
                }
            }
        }
        save_seasonal_site_details_map(snap, &dmap);
    });

    assert!(
        on_special_site_npc_destroyed_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            sys_1,
            sites_1[0].boss_npc_ball_id,
            Some(pilot_1),
        )
        .await
        .is_empty(),
        "site 1 boss kill alone must not complete before escorts die"
    );
    assert!(
        on_special_site_npc_destroyed_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            sys_1,
            sites_1[0].escort_ball_ids[0],
            Some(pilot_1),
        )
        .await
        .is_empty()
    );
    let ch1 = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_1,
        sites_1[0].escort_ball_ids[1],
        Some(pilot_1),
    )
    .await;
    assert_eq!(ch1.len(), 1);

    let p1_items_after_kill1: Vec<(i64, u32, i32)> = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == ship_1 && i.flag == InventoryFlag::Cargo)
            .map(|i| (i.item_id, i.type_id, i.quantity))
            .collect()
    });
    assert!(
        p1_items_after_kill1.len() >= 2,
        "First seasonal site completion must deposit multiple items: {p1_items_after_kill1:?}"
    );

    assert!(
        on_special_site_npc_destroyed_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            sys_2,
            sites_2[0].boss_npc_ball_id,
            Some(pilot_2),
        )
        .await
        .is_empty()
    );
    assert!(
        on_special_site_npc_destroyed_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            sys_2,
            sites_2[0].escort_ball_ids[0],
            Some(pilot_2),
        )
        .await
        .is_empty()
    );
    let ch2 = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_2,
        sites_2[0].escort_ball_ids[1],
        Some(pilot_2),
    )
    .await;
    assert_eq!(ch2.len(), 1);

    ctx.db.with_snapshot(|snap| {
        for &(iid, tid, qty) in &p1_items_after_kill1 {
            let kept = snap
                .items
                .get(&iid)
                .expect("Second seasonal boss kill must not overwrite pilot 1's loot item ID");
            assert_eq!(kept.owner_id, pilot_1);
            assert_eq!(kept.location_id, ship_1);
            assert_eq!(kept.type_id, tid);
            assert_eq!(kept.quantity, qty);
        }
        let p2_items: Vec<(i64, u32, i32)> = snap
            .items
            .values()
            .filter(|i| i.location_id == ship_2 && i.flag == InventoryFlag::Cargo)
            .map(|i| (i.item_id, i.type_id, i.quantity))
            .collect();
        assert!(
            p2_items.len() >= 2,
            "Second seasonal site completion must deposit multiple items: {p2_items:?}"
        );
        let p1_ids: std::collections::HashSet<i64> =
            p1_items_after_kill1.iter().map(|&(id, _, _)| id).collect();
        for &(p2_id, _, _) in &p2_items {
            assert!(
                !p1_ids.contains(&p2_id),
                "Consecutive seasonal boss loot item IDs must be strictly disjoint"
            );
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seasonal_npcs_registered_in_rt_npc_entities_fight_back_and_despawn_clears_ai() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let sys_id: SolarSystemId = 30_000_142;
    let pilot: i64 = 90_060_301;
    let ship_id: i64 = 1_060_301;
    db.provision_gm_account("season_ai", "pw", pilot, "SeasonAi");

    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_600_000)));
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pilot) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: pilot,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut cfg = crate::services::season_fsd::load_season_config(snap);
        cfg.season_id = 1;
        cfg.season_name = "Crimson Harvest".into();
        cfg.start_time_ft = now_ft - 3_600 * 10_000_000;
        cfg.end_time_ft = now_ft + 3_600 * 10_000_000;
        snap.seasons_and_campaigns.season_configuration = Some(cfg);
    });

    let sites = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert_eq!(sites.len(), 1);
    let site = &sites[0];
    let details = get_seasonal_site_details(&ctx, site.instance_id).expect("details");
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    let mut npc_ids = vec![site.boss_npc_ball_id];
    npc_ids.extend_from_slice(&site.escort_ball_ids);
    crate::services::combat_crimewatch::with_combat_ephemeral(&ctx, |rt| {
        for &nid in &npc_ids {
            let ent = rt
                .npc_entities
                .get(&nid)
                .expect("seasonal NPC must be registered in rt.npc_entities");
            assert_eq!(ent.archetype, "Pirate");
            assert_eq!(ent.bounty_isk, 0.0);
            let dmg_sum = ent.damage_vector.em
                + ent.damage_vector.thermal
                + ent.damage_vector.kinetic
                + ent.damage_vector.explosive;
            assert!(dmg_sum > 0.0, "seasonal NPC {nid} must have weapon damage");
        }
    });

    // Place a player ship within weapon range of the site and step NPC AI -> seasonal NPCs engage.
    let player_pos = DVec3::from_array(details.site_position) + DVec3::new(1_000.0, 0.0, 0.0);
    let mut player_ball = make_site_npc_ball(ship_id, 587, player_pos);
    player_ball.radius = 40.0;
    let player_cs = eve_destiny::CombatState {
        shield_hp: 5_000.0,
        max_shield_hp: 5_000.0,
        armor_hp: 5_000.0,
        max_armor_hp: 5_000.0,
        structure_hp: 5_000.0,
        max_structure_hp: 5_000.0,
        signature_radius: 40.0,
        ..Default::default()
    };
    let _ = sys_handle.spawn_ball_with_combat(player_ball, player_cs).await;
    crate::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai(&ctx, sys_id).await;

    let targeted = crate::services::combat_crimewatch::with_combat_ephemeral(&ctx, |rt| {
        npc_ids
            .iter()
            .any(|nid| rt.npc_entities.get(nid).and_then(|e| e.target_id) == Some(ship_id))
    });
    assert!(
        targeted,
        "seasonal NPCs in rt.npc_entities must acquire and target nearby pilots"
    );
    let ship_shield_after = sys_handle
        .get_combat_state(ship_id)
        .await
        .expect("combat state")
        .expect("player ball")
        .shield_hp;
    assert!(
        ship_shield_after < 5_000.0,
        "seasonal NPCs must actively shoot back and damage player ship ({ship_shield_after})"
    );

    // Expiring the seasonal window removes NPCs from both ballpark and rt.npc_entities.
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_610_000)));
    let remaining = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert!(remaining.is_empty());
    crate::services::combat_crimewatch::with_combat_ephemeral(&ctx, |rt| {
        for &nid in &npc_ids {
            assert!(
                !rt.npc_entities.contains_key(&nid),
                "expired seasonal NPC {nid} must be removed from rt.npc_entities"
            );
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seasonal_completion_atomic_rollback_and_no_double_pay_and_sde_fallback() {
    // 1. Minimal synthetic SDE fallback (PVE-055): no seasonal dungeons/FSD tables must not panic.
    let empty_sde = Arc::new(SdeRegistry::default());
    let fallback_spec = resolve_seasonal_event_spec(
        &empty_sde,
        &eve_db::SeasonConfig {
            season_id: 1,
            season_name: "Crimson Harvest".into(),
            ..Default::default()
        },
        30_000_142,
    );
    assert!(fallback_spec.reward_isk > 0);
    assert!(fallback_spec.reward_points > 0);

    // 2. Atomic completion: rollback on mid-flight error + no double-pay after commit.
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let sys_id: SolarSystemId = 30_000_142;
    let pilot: i64 = 90_060_401;
    let ship_id: i64 = 1_060_401;
    db.provision_gm_account("season_atomic", "pw", pilot, "SeasonAtomic");

    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_700_000)));
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pilot) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents(0);
        }
        let mut cfg = crate::services::season_fsd::load_season_config(snap);
        cfg.start_time_ft = now_ft - 3_600 * 10_000_000;
        cfg.end_time_ft = now_ft + 3_600 * 10_000_000;
        snap.seasons_and_campaigns.season_configuration = Some(cfg);
    });

    let sites = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert_eq!(sites.len(), 1);
    let site = sites[0].clone();
    let details = get_seasonal_site_details(&ctx, site.instance_id).expect("details");
    let anom_key = format!("dng-{}-{}", site.dungeon_id, site.instance_id);

    // Destroy boss and first escort -> partial progress only.
    assert!(
        on_special_site_npc_destroyed_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            sys_id,
            site.boss_npc_ball_id,
            Some(pilot),
        )
        .await
        .is_empty()
    );
    assert!(
        on_special_site_npc_destroyed_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            sys_id,
            site.escort_ball_ids[0],
            Some(pilot),
        )
        .await
        .is_empty()
    );

    // Trigger genuine mid-transaction failure (wallet IskCents::checked_add overflow AFTER
    // site.completed / cooldown / details / exploration / anomaly writes) -> atomic rollback.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pilot) {
            ch.balance = IskCents(i64::MAX - 10);
        }
    });
    let bad_kill = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        site.escort_ball_ids[1],
        Some(pilot),
    )
    .await;
    assert!(bad_kill.is_empty());
    ctx.db.with_snapshot(|snap| {
        let rec = snap.pve_special_sites.seasonal_event_sites[&sys_id]
            .iter()
            .find(|s| s.instance_id == site.instance_id)
            .expect("site still present after rollback");
        assert!(!rec.completed, "site.completed must remain false after rollback");
        assert!(
            load_seasonal_site_details_map(snap).contains_key(&site.instance_id),
            "details must not be deleted on aborted completion"
        );
        assert!(
            load_seasonal_cooldown_store(snap)
                .until_by_system
                .get(&sys_id)
                .is_none(),
            "cooldown must not be armed on aborted completion"
        );
        assert!(
            snap.cosmic_anomalies[&sys_id]
                .iter()
                .any(|a| a.anomaly_id == anom_key && !a.cleared),
            "anomaly must remain uncleared after rollback"
        );
        assert!(
            snap.transient_items.contains_key(&details.relay_ball_id),
            "relay item must remain after rollback"
        );
    });
    let exp_still_open = crate::services::pve_exploration::read_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .get(&sys_id)
            .is_some_and(|l| l.iter().any(|s| s.site_id == details.exploration_site_id && !s.cleared))
    });
    assert!(exp_still_open, "exploration site must stay uncleared after rollback");

    // Restore normal wallet, then complete via active ship ball ID (ship -> char resolution P2 #1);
    // replay cannot double-pay.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pilot) {
            ch.balance = IskCents(0);
        }
    });
    let ok_kill = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        site.escort_ball_ids[1],
        Some(ship_id),
    )
    .await;
    assert_eq!(ok_kill.len(), 1);
    let replay_kill = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        site.escort_ball_ids[1],
        Some(pilot),
    )
    .await;
    assert!(replay_kill.is_empty(), "replay after completion must never double-pay");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.characters[&pilot].balance.0,
            site.reward_isk * 100,
            "wallet must receive exactly one completion payout via ship-ID killer resolution"
        );
        let st = crate::services::season_manager::load_season_state(snap, pilot);
        assert_eq!(st.season_points, site.reward_points);
    });

    // Non-character / absent killer ID on final kill still completes site, arms cooldown, and
    // cleans exploration/anomaly/relay without wedging or rolling back (P2 #1).
    let sys_nonchar: SolarSystemId = 30_000_144;
    let nonchar_sites = ensure_seasonal_event_sites_for_system(&ctx, sys_nonchar).await;
    assert_eq!(nonchar_sites.len(), 1);
    let ncs = &nonchar_sites[0];
    let ncs_details = get_seasonal_site_details(&ctx, ncs.instance_id).expect("ncs details");
    let _ = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_nonchar,
        ncs.boss_npc_ball_id,
        Some(999_999_999),
    )
    .await;
    let _ = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_nonchar,
        ncs.escort_ball_ids[0],
        Some(999_999_999),
    )
    .await;
    let nonchar_final = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_nonchar,
        ncs.escort_ball_ids[1],
        Some(999_999_999),
    )
    .await;
    assert!(nonchar_final.is_empty());
    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.pve_special_sites.seasonal_event_sites[&sys_nonchar]
                .iter()
                .any(|s| s.instance_id == ncs.instance_id && s.completed),
            "non-character killer must still complete seasonal site cleanly"
        );
        assert!(
            load_seasonal_cooldown_store(snap)
                .until_by_system
                .contains_key(&sys_nonchar),
            "cooldown must still be armed after non-character killer completion"
        );
        assert!(!snap.transient_items.contains_key(&ncs_details.relay_ball_id));
    });
}

fn setup_seasonal_test_ctx(
    sde: Arc<SdeRegistry>,
    base_unix: u64,
    season_id: i64,
    season_name: &str,
    window_days: u64,
    pilot: i64,
    ship_id: i64,
    sys_id: SolarSystemId,
) -> ServerContext {
    let db = Arc::new(WorldRepository::new_empty());
    db.provision_gm_account(&format!("season_u_{pilot}"), "pw", pilot, "SeasonPilot");
    let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
    let now_ft = ctx.now_blueos_filetime_ticks();
    let ship_tid = sde
        .types
        .values()
        .find(|t| t.category_id == 6 && t.group_id == 25 && t.capacity > 50.0)
        .map(|t| t.type_id)
        .unwrap_or(587);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pilot) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents(0);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_tid,
                owner_id: pilot,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut cfg = crate::services::season_fsd::load_season_config(snap);
        cfg.season_id = season_id;
        cfg.season_name = season_name.into();
        let day_ticks = 86_400_i64 * 10_000_000;
        cfg.start_time_ft = now_ft - day_ticks;
        cfg.end_time_ft = now_ft + (window_days as i64) * day_ticks;
        snap.seasons_and_campaigns.season_configuration = Some(cfg);
    });
    ctx
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seasonal_24h_anomaly_expiry_prunes_stale_site_and_respawns_in_multiday_season() {
    let sde = shared_sde();
    let sys_id: SolarSystemId = 30_000_142;
    let base_unix = 1_775_800_000_u64;
    let ctx = setup_seasonal_test_ctx(
        sde,
        base_unix,
        1,
        "Crimson Harvest",
        14,
        90_060_501,
        1_060_501,
        sys_id,
    );

    let first = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert_eq!(first.len(), 1);
    let site1 = &first[0];
    let details1 = get_seasonal_site_details(&ctx, site1.instance_id).expect("details1");
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Advance past the 24h cosmic anomaly lifetime while staying well inside the 14-day window.
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix + 86_405)));
    assert!(
        active_seasonal_anomaly_instances_readonly(&ctx, sys_id).is_empty(),
        "readonly anomaly view must hide 24h-expired seasonal anomalies"
    );

    let second = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert_eq!(
        second.len(),
        1,
        "24h-expired seasonal site inside active multi-day season must prune and respawn"
    );
    assert_ne!(
        second[0].instance_id, site1.instance_id,
        "respawned site must get a fresh instance_id"
    );
    assert!(
        sys_handle.get_ball_snapshot(site1.boss_npc_ball_id).await.unwrap().is_none()
            && sys_handle.get_ball_snapshot(details1.relay_ball_id).await.unwrap().is_none(),
        "stale boss and relay balls from 24h-expired site must be despawned"
    );
    crate::services::combat_crimewatch::with_combat_ephemeral(&ctx, |rt| {
        assert!(!rt.npc_entities.contains_key(&site1.boss_npc_ball_id));
        for ebid in &site1.escort_ball_ids {
            assert!(!rt.npc_entities.contains_key(ebid));
        }
    });
    let exp_cleared = crate::services::pve_exploration::read_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .get(&sys_id)
            .is_some_and(|l| l.iter().any(|s| s.site_id == details1.exploration_site_id))
    });
    assert!(!exp_cleared, "stale exploration site must be removed on 24h expiry");
    assert!(
        get_seasonal_site_details(&ctx, site1.instance_id).is_none(),
        "stale seasonal details entry must be removed on 24h expiry"
    );
    let ro = active_seasonal_anomaly_instances_readonly(&ctx, sys_id);
    assert_eq!(
        ro.get(&second[0].dungeon_id),
        Some(&vec![second[0].instance_id]),
        "newly respawned site must appear in GetCombatAnomalyInstances"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seasonal_restart_rematerializes_relay_preserves_faction_and_live_ball_state() {
    let sde = shared_sde();
    let sys_id: SolarSystemId = 30_000_142;
    let base_unix = 1_775_900_000_u64;
    let pilot: i64 = 90_060_601;
    let ship_id: i64 = 1_060_601;

    let tmp_db = std::env::temp_dir().join(format!(
        "evomu_seasonal_restart_{}_{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let cleanup_sqlite = |p: &std::path::Path| {
        let _ = std::fs::remove_file(p);
        let _ = std::fs::remove_file(format!("{}-wal", p.display()));
        let _ = std::fs::remove_file(format!("{}-shm", p.display()));
    };
    cleanup_sqlite(&tmp_db);

    let mem_ctx = setup_seasonal_test_ctx(
        Arc::clone(&sde),
        base_unix,
        2,
        "Winter Nexus",
        7,
        pilot,
        ship_id,
        sys_id,
    );
    let initial_snap = mem_ctx.db.with_snapshot(|s| s.clone());
    drop(mem_ctx);

    let repo1 = Arc::new(WorldRepository::open_sqlite(&tmp_db).expect("open sqlite repo1"));
    repo1.with_mut_snapshot(|snap| *snap = initial_snap);
    let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&repo1));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));

    let initial = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert_eq!(initial.len(), 1);
    let site = initial[0].clone();
    let details = get_seasonal_site_details(&ctx, site.instance_id).expect("details");
    assert_ne!(details.faction_id, 0);
    assert_ne!(
        details.faction_id, 500_012,
        "Winter Nexus seasonal site must not carry Crimson Harvest faction fallback"
    );

    // R1482-R2-P1-3: both exploration entry & rt.npc_entities must be persisted atomically by ensure.
    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.owner_notes
                .contains_key(&crate::services::pve_exploration::PVE_EXPLORATION_NOTE_ID),
            "seasonal exploration site must be written inside atomic spawn snapshot"
        );
        assert!(
            snap.get_any_item(details.relay_ball_id).is_some(),
            "relay transient item must exist right after seasonal spawn"
        );
    });
    let (initial_boss_fid, initial_escort_fid) =
        crate::services::combat_crimewatch::with_combat_ephemeral(&ctx, |rt| {
            (
                rt.npc_entities[&site.boss_npc_ball_id].faction_id,
                rt.npc_entities[&site.escort_ball_ids[1]].faction_id,
            )
        });

    // Defeat one escort via live combat hook (`npc_death_hook` -> `on_special_site_npc_destroyed_core`
    // + `handle_npc_destroyed_in_space_with_ctx`) before simulated server restart.
    let sys_initial = ctx.systems.get_or_spawn(sys_id);
    let (_, esc0_dead, _) = sys_initial
        .apply_damage_with_attacker(
            site.escort_ball_ids[0],
            eve_destiny::DamageVector::new(1_000_000.0, 1_000_000.0, 1_000_000.0, 1_000_000.0),
            Some(pilot),
        )
        .await
        .expect("damage escort 0")
        .expect("escort 0 exists");
    assert!(esc0_dead, "escort 0 must die from lethal live damage");

    drop(sys_initial);
    drop(ctx);
    drop(repo1);

    // Reopen SQLite repository from disk (where #[serde(skip)] `transient_items` has been dropped).
    let repo2 = Arc::new(WorldRepository::open_sqlite(&tmp_db).expect("reopen sqlite repo2"));
    repo2.with_snapshot(|snap| {
        assert!(
            snap.transient_items.get(&details.relay_ball_id).is_none(),
            "#[serde(skip)] transient_items must be empty immediately after SQLite reopen"
        );
    });
    let restarted = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&repo2));
    restarted.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix + 60)));
    crate::services::combat_crimewatch::with_combat_ephemeral(&restarted, |rt| {
        assert!(
            !rt.npc_entities.contains_key(&site.escort_ball_ids[0]),
            "defeated escort must be cleared from persisted combat runtime"
        );
        assert!(
            rt.bounty_credited_balls.contains(&site.escort_ball_ids[0]),
            "defeated seasonal escort must mark bounty_credited_balls to block SDE double-bounty"
        );
    });
    // Wipe ephemeral NPC AI entries so ensure_seasonal_event_sites_for_system must rebuild both
    // the Ballpark-present boss (!in_ai && in_ballpark) and Ballpark-absent escort (!in_ai && !in_ballpark).
    crate::services::combat_crimewatch::with_mut_combat_ephemeral(&restarted, |rt| {
        rt.npc_entities.clear();
    });
    let sys_restarted = restarted.systems.get_or_spawn(sys_id);

    // Simulate an already-present Ballpark boss with moved coordinates and weakened shields (!in_ai).
    let moved_pos = DVec3::from_array(details.site_position) + DVec3::new(3_750.0, -450.0, 120.0);
    let boss_ball = make_site_npc_ball(site.boss_npc_ball_id, site.boss_type_id, moved_pos);
    let damaged_cs = eve_destiny::CombatState {
        shield_hp: 42.0,
        max_shield_hp: 500.0,
        armor_hp: 300.0,
        max_armor_hp: 300.0,
        structure_hp: 300.0,
        max_structure_hp: 300.0,
        ..Default::default()
    };
    let _ = sys_restarted.spawn_ball_with_combat(boss_ball, damaged_cs).await;

    let resumed = ensure_seasonal_event_sites_for_system(&restarted, sys_id).await;
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].instance_id, site.instance_id);

    // 1. Relay ball AND transient_items + item_positions rehydrated after SQLite restart (`R1482-R2-P1-2`).
    restarted.db.with_snapshot(|snap| {
        let relay_item = snap
            .get_any_item(details.relay_ball_id)
            .expect("SHK relay transient item must be restored into snap on restart recovery");
        assert_eq!(relay_item.type_id, details.relay_type_id);
        assert!(
            snap.item_positions.contains_key(&details.relay_ball_id),
            "SHK relay item_positions must be restored on restart recovery"
        );
    });
    assert!(
        sys_restarted
            .get_ball_snapshot(details.relay_ball_id)
            .await
            .unwrap()
            .is_some(),
        "SHK relay ball must be re-materialized into Ballpark after restart"
    );
    // 2. Defeated escort stays dead.
    assert!(
        sys_restarted
            .get_ball_snapshot(site.escort_ball_ids[0])
            .await
            .unwrap()
            .is_none(),
        "already-defeated escort must stay absent after restart"
    );
    // 3. Surviving escort re-materialized into Ballpark with persisted theme faction & spawn group.
    assert!(
        sys_restarted
            .get_ball_snapshot(site.escort_ball_ids[1])
            .await
            .unwrap()
            .is_some(),
        "surviving escort missing from Ballpark must be re-materialized"
    );
    crate::services::combat_crimewatch::with_combat_ephemeral(&restarted, |rt| {
        assert!(!rt.npc_entities.contains_key(&site.escort_ball_ids[0]));
        let boss_ent = rt
            .npc_entities
            .get(&site.boss_npc_ball_id)
            .expect("living seasonal boss must be re-registered in rt.npc_entities");
        assert_eq!(boss_ent.faction_id, initial_boss_fid);
        assert_eq!(boss_ent.spawn_group_id, details.faction_id);
        let escort_ent = rt
            .npc_entities
            .get(&site.escort_ball_ids[1])
            .expect("living seasonal escort must be re-registered in rt.npc_entities");
        assert_eq!(escort_ent.faction_id, initial_escort_fid);
        assert_eq!(escort_ent.spawn_group_id, details.faction_id);
    });
    // 4. Existing Ballpark boss ball retains live position and damaged shield HP.
    let (_, boss_after) = sys_restarted
        .get_ball_snapshot(site.boss_npc_ball_id)
        .await
        .unwrap()
        .expect("boss ball still present");
    assert_eq!(boss_after.position, moved_pos);
    let cs_after = sys_restarted
        .get_combat_state(site.boss_npc_ball_id)
        .await
        .unwrap()
        .expect("boss combat state");
    assert!(
        (cs_after.shield_hp - 42.0).abs() < 1e-6,
        "re-materialization must preserve existing Ballpark HP ({})",
        cs_after.shield_hp
    );
    drop(sys_restarted);
    drop(restarted);
    drop(repo2);
    cleanup_sqlite(&tmp_db);

    // 5. Fallback faction test: when an NPC InvType has faction_id == None in Jita (regional
    // Guristas 500_010), Winter Nexus restart must preserve 500_019 (not 0, 500_010, or 500_012).
    let mut no_fid_sde = SdeRegistry::default();
    if let Some(jita) = shared_sde().solar_systems.get(&sys_id).cloned() {
        no_fid_sde.solar_systems.insert(sys_id, jita);
    }
    let no_fid_sde = Arc::new(no_fid_sde);
    let ctx_nf = setup_seasonal_test_ctx(
        Arc::clone(&no_fid_sde),
        base_unix,
        2,
        "Winter Nexus",
        7,
        90_060_602,
        1_060_602,
        sys_id,
    );
    let nf_sites = ensure_seasonal_event_sites_for_system(&ctx_nf, sys_id).await;
    assert_eq!(nf_sites.len(), 1);
    let restarted_nf =
        ServerContext::with_sde_and_repository(no_fid_sde, Arc::clone(&ctx_nf.db));
    restarted_nf.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix + 60)));
    crate::services::combat_crimewatch::with_mut_combat_ephemeral(&restarted_nf, |rt| {
        rt.npc_entities.clear();
    });
    let nf_resumed = ensure_seasonal_event_sites_for_system(&restarted_nf, sys_id).await;
    assert_eq!(nf_resumed.len(), 1);
    crate::services::combat_crimewatch::with_combat_ephemeral(&restarted_nf, |rt| {
        let ent = rt
            .npc_entities
            .get(&nf_resumed[0].boss_npc_ball_id)
            .expect("fallback NPC rematerialized");
        assert_eq!(ent.faction_id, 500_019);
        assert_eq!(ent.spawn_group_id, 500_019);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seasonal_boss_first_relay_hack_awards_crate_pool_and_booster_priority() {
    // Part A: boss killed before escorts -> SHK relay hack still sets relay_hacked and grants pool.
    let sde = shared_sde();
    let sys_id: SolarSystemId = 30_000_142;
    let pilot: i64 = 90_060_701;
    let ship_id: i64 = 1_060_701;
    let ctx = setup_seasonal_test_ctx(
        Arc::clone(&sde),
        1_775_950_000,
        1,
        "Crimson Harvest",
        7,
        pilot,
        ship_id,
        sys_id,
    );

    let sites = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert_eq!(sites.len(), 1);
    let site = sites[0].clone();

    // Ensure at least one valid SDE pool item exists even if canonical unit SDE omits FSD tables.
    let mut details = get_seasonal_site_details(&ctx, site.instance_id).expect("details");
    if details.crate_loot_pool.is_empty() {
        let fallback_pool_tid = sde
            .types
            .keys()
            .copied()
            .find(|&tid| tid != site.loot_type_id)
            .expect("canonical SDE contains at least two item types");
        ctx.db.with_mut_snapshot(|snap| {
            let mut map = load_seasonal_site_details_map(snap);
            if let Some(d) = map.get_mut(&site.instance_id) {
                d.crate_loot_pool.push((fallback_pool_tid, 3));
            }
            save_seasonal_site_details_map(snap, &map);
        });
        details = get_seasonal_site_details(&ctx, site.instance_id).expect("updated details");
    }
    let expected_pool: Vec<(u32, i64)> = details
        .crate_loot_pool
        .iter()
        .copied()
        .filter(|&(ptid, pqty)| ptid != site.loot_type_id && pqty > 0 && sde.types.contains_key(&ptid))
        .collect();
    assert!(!expected_pool.is_empty());

    // Kill boss FIRST while both escorts survive.
    assert!(
        on_special_site_npc_destroyed_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            sys_id,
            site.boss_npc_ball_id,
            Some(pilot),
        )
        .await
        .is_empty()
    );

    // Hack relay AFTER boss death but BEFORE escorts die -> relay_hacked must flip to true.
    on_seasonal_relay_hacked(&ctx, sys_id, &details.exploration_site_id, pilot).await;
    assert!(
        get_seasonal_site_details(&ctx, site.instance_id)
            .expect("details still active before escorts die")
            .relay_hacked,
        "hacking SHK relay after boss-first kill must record relay_hacked = true"
    );

    for ebid in &site.escort_ball_ids {
        let _ = on_special_site_npc_destroyed_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            sys_id,
            *ebid,
            Some(pilot),
        )
        .await;
    }

    ctx.db.with_snapshot(|snap| {
        for &(ptid, pqty) in &expected_pool {
            let found_qty: i64 = snap
                .items
                .values()
                .chain(snap.transient_items.values())
                .filter(|it| it.owner_id == pilot && it.type_id == ptid)
                .map(|it| i64::from(it.quantity))
                .sum();
            assert!(
                found_qty >= pqty,
                "bonus crate_loot_pool type {ptid} x{pqty} must be awarded after boss-first relay hack (got {found_qty})"
            );
        }
    });

    // Part B: Booster priority tiers & integer security-band ISK scaling.
    let mut synth = SdeRegistry::default();
    let base_type = sde.types.values().next().expect("non-empty canonical SDE types");
    let make_booster = |type_id: u32, name: &str| {
        let mut t = base_type.clone();
        t.type_id = type_id;
        t.group_id = 303;
        t.category_id = 20;
        t.name = name.into();
        t.published = true;
        t
    };
    synth
        .types
        .insert(4_001, make_booster(4_001, "Agency 'Overclocker' Dose I"));
    synth
        .types
        .insert(9_950, make_booster(9_950, "Standard Blue Pill Booster"));
    synth.types.insert(
        88_001,
        make_booster(88_001, "Crimson Harvest Cerebral Accelerator"),
    );
    assert_eq!(
        resolve_seasonal_booster_type_id(&synth, "Crimson Harvest"),
        Some(88_001),
        "theme/cerebral booster must outrank lower-ID Standard Blue Pill (9950)"
    );
    synth.types.remove(&88_001);
    assert_eq!(
        resolve_seasonal_booster_type_id(&synth, "Crimson Harvest"),
        Some(9_950),
        "combat booster must outrank generic group_id=303 booster when no theme booster exists"
    );
    synth.types.remove(&9_950);
    assert_eq!(
        resolve_seasonal_booster_type_id(&synth, "Crimson Harvest"),
        Some(4_001)
    );

    let base_sys = sde
        .solar_systems
        .values()
        .next()
        .expect("non-empty canonical SDE solar_systems");
    let add_sys = |reg: &mut SdeRegistry, id: SolarSystemId, sec: f64| {
        let mut s = base_sys.clone();
        s.solar_system_id = id;
        s.security_status = sec;
        reg.solar_systems.insert(id, s);
    };
    add_sys(&mut synth, 30_000_001, 0.9);
    add_sys(&mut synth, 30_000_002, 0.3);
    add_sys(&mut synth, 30_000_003, -0.5);
    let season_cfg = eve_db::SeasonConfig {
        season_id: 1,
        season_name: "Crimson Harvest".into(),
        ..Default::default()
    };
    let hs = resolve_seasonal_event_spec(&synth, &season_cfg, 30_000_001).reward_isk;
    let ls = resolve_seasonal_event_spec(&synth, &season_cfg, 30_000_002).reward_isk;
    let ns = resolve_seasonal_event_spec(&synth, &season_cfg, 30_000_003).reward_isk;
    assert_eq!(ls, hs * 3 / 2);
    assert_eq!(ns, hs * 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seasonal_relay_hack_idempotent_expiry_and_overflow_container_consistency() {
    let sde = shared_sde();
    let sys_id: SolarSystemId = 30_000_142;
    let pilot: i64 = 90_060_801;
    let ship_id: i64 = 1_060_801;
    let base_unix = 1_776_050_000_u64;
    let ctx = setup_seasonal_test_ctx(
        Arc::clone(&sde),
        base_unix,
        1,
        "Crimson Harvest",
        7,
        pilot,
        ship_id,
        sys_id,
    );

    let sites = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert_eq!(sites.len(), 1);
    let site = sites[0].clone();
    let details = get_seasonal_site_details(&ctx, site.instance_id).expect("details");
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // 1. Repeat relay hack is strictly idempotent (`R1482-R2-P1-4`).
    let shield_0 = sys_handle
        .get_combat_state(site.boss_npc_ball_id)
        .await
        .unwrap()
        .expect("boss combat state")
        .shield_hp;
    on_seasonal_relay_hacked(&ctx, sys_id, &details.exploration_site_id, pilot).await;
    assert!(
        get_seasonal_site_details(&ctx, site.instance_id)
            .expect("details")
            .relay_hacked
    );
    let shield_1 = sys_handle
        .get_combat_state(site.boss_npc_ball_id)
        .await
        .unwrap()
        .expect("boss combat state")
        .shield_hp;
    if shield_0 > 20.0 {
        assert!(shield_1 < shield_0);
    }
    on_seasonal_relay_hacked(&ctx, sys_id, &details.exploration_site_id, pilot).await;
    let shield_2 = sys_handle
        .get_combat_state(site.boss_npc_ball_id)
        .await
        .unwrap()
        .expect("boss combat state")
        .shield_hp;
    assert!(
        (shield_2 - shield_1).abs() < 1e-6,
        "repeat on_seasonal_relay_hacked must not drain boss shield a second time"
    );

    // Verify stationary SHK relay ball is pinned (!BALL_FLAG_IS_FREE) in SetState (`P2 #3`)
    // and seed PVE_EXPLORATION_NOTE_ID hacking state for R1482-R4-P1-1 cleanup verification.
    let expected_pos = DVec3::from_array(details.site_position) + DVec3::new(500.0, 0.0, 0.0);
    let obs_id = 1_060_899_i64;
    let _ = sys_handle
        .spawn_ball(make_site_npc_ball(
            obs_id,
            587,
            expected_pos + DVec3::new(10.0, 0.0, 0.0),
        ))
        .await;
    let (_, pre_set_state, _) = sys_handle
        .get_set_state(obs_id)
        .await
        .expect("pre-completion get_set_state");
    let Some(eve_proto::PyRep::Bytes(pre_bytes)) = pre_set_state.dict_get("state") else {
        panic!("pre-completion SetState missing bytes state");
    };
    let (_, _, pre_balls) =
        eve_destiny::wire::decode_add_balls(pre_bytes).expect("decode pre SetState");
    let wire_relay = pre_balls
        .iter()
        .find(|b| b.id == details.relay_ball_id)
        .expect("relay ball present in SetState wire buffer");
    assert_eq!(
        wire_relay.flags & eve_destiny::wire::BALL_FLAG_IS_FREE,
        0,
        "stationary SHK relay ball must omit BALL_FLAG_IS_FREE"
    );

    crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        assert!(
            reg.sites_by_system
                .get(&sys_id)
                .is_some_and(|l| l.iter().any(|s| s.site_id == details.exploration_site_id)),
            "SHK site must be registered in PVE_EXPLORATION_NOTE_ID prior to completion"
        );
        reg.hacked_containers.insert((sys_id, details.relay_ball_id));
        reg.failure_counts.insert((sys_id, details.relay_ball_id), 2);
        reg.self_repair_turns
            .insert((pilot, details.relay_ball_id), 3);
        reg.hacking_games.insert(
            (pilot, details.relay_ball_id),
            crate::services::pve_exploration::HackingGameSession {
                game_id: 99_001,
                character_id: pilot,
                solar_system_id: sys_id,
                container_id: details.relay_ball_id,
                site_id: details.exploration_site_id.clone(),
                is_relic: false,
                is_ghost_site: false,
                is_sleeper_cache_puzzle: false,
                virus_coherence: 50,
                max_virus_coherence: 50,
                base_virus_strength: 20,
                polymorphic_shield_charges: 0,
                secondary_vector_target: None,
                utilities: Vec::new(),
                tiles: Vec::new(),
                status: "Won".into(),
            },
        );
    });

    // 2. Overflow cargo container DB metadata matches Ballpark ball (`P2 #1`).
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pilot) {
            ch.active_ship_id = 0;
        }
    });
    let expected_cont_type = resolve_loot_container_type_id(&sde);
    let mut kill_ids = vec![site.boss_npc_ball_id];
    kill_ids.extend_from_slice(&site.escort_ball_ids);
    for &npc_bid in &kill_ids {
        let (_, dead, _) = sys_handle
            .apply_damage_with_attacker(
                npc_bid,
                eve_destiny::DamageVector::new(1_000_000.0, 1_000_000.0, 1_000_000.0, 1_000_000.0),
                Some(pilot),
            )
            .await
            .expect("damage npc")
            .expect("npc ball exists");
        assert!(dead);
    }
    crate::services::combat_crimewatch::with_combat_ephemeral(&ctx, |rt| {
        for &npc_bid in &kill_ids {
            assert!(
                rt.is_recently_destroyed_item_id(npc_bid),
                "destroyed seasonal NPC {npc_bid} must be recorded in #1032 recently_destroyed_item_ids"
            );
        }
    });
    crate::services::pve_exploration::read_pve_exploration_reg(&ctx, |reg| {
        assert!(
            !reg.sites_by_system
                .get(&sys_id)
                .is_some_and(|l| l.iter().any(|s| s.site_id == details.exploration_site_id)),
            "completed seasonal SHK-* site must be removed from PVE_EXPLORATION_NOTE_ID (`R1482-R4-P1-1`)"
        );
        assert!(
            !reg.hacked_containers
                .contains(&(sys_id, details.relay_ball_id)),
            "completed seasonal relay must be removed from reg.hacked_containers"
        );
        assert!(
            !reg.failure_counts
                .contains_key(&(sys_id, details.relay_ball_id)),
            "completed seasonal relay must be removed from reg.failure_counts"
        );
        assert!(
            !reg.hacking_games
                .contains_key(&(pilot, details.relay_ball_id)),
            "completed seasonal relay must be removed from reg.hacking_games"
        );
        assert!(
            !reg.self_repair_turns
                .contains_key(&(pilot, details.relay_ball_id)),
            "completed seasonal relay must be removed from reg.self_repair_turns"
        );
    });
    let _ = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    crate::services::pve_exploration::read_pve_exploration_reg(&ctx, |reg| {
        assert!(
            !reg.sites_by_system
                .get(&sys_id)
                .is_some_and(|l| l.iter().any(|s| s.site_id == details.exploration_site_id)),
            "subsequent ensure must keep PVE_EXPLORATION_NOTE_ID free of completed SHK-* entries"
        );
    });
    let (cont_id, cont_type, cont_flag, cont_pos, has_meta) = ctx.db.with_snapshot(|snap| {
        let (&cid, item) = snap
            .items
            .iter()
            .find(|(_, it)| it.owner_id == pilot && it.location_id == i64::from(sys_id) && it.type_id == expected_cont_type)
            .expect("seasonal overflow cargo container must be persisted with resolved type_id");
        let pos = snap.item_positions.get(&cid).copied();
        let meta = snap.sp6_lifecycle.space_object_meta.contains_key(&cid);
        (cid, item.type_id, item.flag, pos, meta)
    });
    assert_eq!(cont_type, expected_cont_type);
    assert_eq!(cont_flag, InventoryFlag::None, "overflow container DB flag must be InventoryFlag::None");
    assert_eq!(cont_pos, Some([expected_pos.x, expected_pos.y, expected_pos.z]));
    assert!(has_meta, "overflow container must record sp6_lifecycle space_object_meta");
    let (_, cont_ball) = sys_handle
        .get_ball_snapshot(cont_id)
        .await
        .unwrap()
        .expect("overflow container ball spawned in Ballpark");
    assert_eq!(cont_ball.type_id, expected_cont_type);
    assert_eq!(cont_ball.position, expected_pos);
    assert_eq!(cont_ball.max_velocity, 0.0, "overflow container must be stationary (max_velocity == 0)");
    assert_eq!(cont_ball.speed_fraction, 0.0, "overflow container must have zero speed_fraction");
    assert!(
        sys_handle.get_combat_state(cont_id).await.unwrap().is_some(),
        "overflow container must have Ballpark CombatState"
    );
    let cont_slim = sys_handle
        .get_ball_slim_item(cont_id)
        .await
        .unwrap()
        .expect("overflow container slim item");
    assert_eq!(
        cont_slim.dict_get("ownerID"),
        Some(&eve_proto::PyRep::Integer(pilot)),
        "overflow container SlimItemMeta must use pilot as ownerID"
    );
    let (_, set_state_rep, _) = sys_handle.get_set_state(obs_id).await.expect("get_set_state");
    let Some(eve_proto::PyRep::Bytes(state_bytes)) = set_state_rep.dict_get("state") else {
        panic!("SetState missing bytes state");
    };
    let (_, _, decoded_balls) = eve_destiny::wire::decode_add_balls(state_bytes).expect("decode SetState balls");
    let wire_cont = decoded_balls
        .iter()
        .find(|b| b.id == cont_id)
        .expect("overflow container present in SetState wire buffer");
    assert_eq!(
        wire_cont.flags & eve_destiny::wire::BALL_FLAG_IS_FREE,
        0,
        "overflow container must omit BALL_FLAG_IS_FREE"
    );

    // 3. Expired (>24h) or out-of-window seasonal site rejects `on_seasonal_relay_hacked` (`R1482-R2-P1-4`).
    let other_sys: SolarSystemId = 30_000_144;
    let sites2 = ensure_seasonal_event_sites_for_system(&ctx, other_sys).await;
    assert_eq!(sites2.len(), 1);
    let site2 = sites2[0].clone();
    let details2 = get_seasonal_site_details(&ctx, site2.instance_id).expect("details2");
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix + 90_000)));
    on_seasonal_relay_hacked(&ctx, other_sys, &details2.exploration_site_id, pilot).await;
    assert!(
        !get_seasonal_site_details(&ctx, site2.instance_id)
            .expect("details2 unpruned")
            .relay_hacked,
        "expired >24h seasonal site must reject on_seasonal_relay_hacked"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seasonal_expired_and_out_of_window_npc_kills_prune_without_rewards_or_cooldown() {
    let sde = shared_sde();
    let sys_1: SolarSystemId = 30_000_142;
    let sys_2: SolarSystemId = 30_000_144;
    let pilot: i64 = 90_060_821;
    let ship_id: i64 = 1_060_821;
    let base_unix = 1_776_100_000_u64;
    let ctx = setup_seasonal_test_ctx(
        Arc::clone(&sde),
        base_unix,
        1,
        "Crimson Harvest",
        7,
        pilot,
        ship_id,
        sys_1,
    );

    // Part A: >24h anomaly expiration inside multi-day active season.
    let sites_1 = ensure_seasonal_event_sites_for_system(&ctx, sys_1).await;
    assert_eq!(sites_1.len(), 1);
    let site_1 = sites_1[0].clone();
    let details_1 = get_seasonal_site_details(&ctx, site_1.instance_id).expect("details_1");
    let h1 = ctx.systems.get_or_spawn(sys_1);

    // Seed second system while still inside season window too.
    let sites_2 = ensure_seasonal_event_sites_for_system(&ctx, sys_2).await;
    assert_eq!(sites_2.len(), 1);
    let site_2 = sites_2[0].clone();
    let details_2 = get_seasonal_site_details(&ctx, site_2.instance_id).expect("details_2");
    let h2 = ctx.systems.get_or_spawn(sys_2);

    // Advance +90,000s (>86,400s anomaly lifetime, still inside 7-day season).
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix + 90_000)));
    let exp_ch = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_1,
        site_1.boss_npc_ball_id,
        Some(pilot),
    )
    .await;
    assert!(exp_ch.is_empty(), "expired seasonal site kill must pay zero rewards");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&pilot].balance.0, 0);
        let st = crate::services::season_manager::load_season_state(snap, pilot);
        assert_eq!(st.season_points, 0);
        assert!(
            snap.pve_special_sites
                .seasonal_event_sites
                .get(&sys_1)
                .is_none_or(Vec::is_empty),
            "expired seasonal site must be pruned on kill"
        );
        assert!(!load_seasonal_site_details_map(snap).contains_key(&site_1.instance_id));
        assert!(
            !load_seasonal_cooldown_store(snap)
                .until_by_system
                .contains_key(&sys_1),
            "expired seasonal cleanup must NOT arm respawn cooldown"
        );
        let anom_key = format!("dng-{}-{}", site_1.dungeon_id, site_1.instance_id);
        assert!(
            !snap
                .cosmic_anomalies
                .get(&sys_1)
                .is_some_and(|l| l.iter().any(|a| a.anomaly_id == anom_key))
        );
        assert!(!snap.transient_items.contains_key(&details_1.relay_ball_id));
    });
    assert!(h1.get_ball_snapshot(site_1.boss_npc_ball_id).await.unwrap().is_none());
    assert!(h1.get_ball_snapshot(site_1.escort_ball_ids[0]).await.unwrap().is_none());
    assert!(h1.get_ball_snapshot(details_1.relay_ball_id).await.unwrap().is_none());
    crate::services::combat_crimewatch::with_combat_ephemeral(&ctx, |rt| {
        assert!(rt.bounty_credited_balls.contains(&site_1.boss_npc_ball_id));
        assert!(rt.is_recently_destroyed_item_id(site_1.boss_npc_ball_id));
        assert!(!rt.npc_entities.contains_key(&site_1.escort_ball_ids[0]));
    });
    let exp_pruned = crate::services::pve_exploration::read_pve_exploration_reg(&ctx, |reg| {
        !reg.sites_by_system
            .get(&sys_1)
            .is_some_and(|l| l.iter().any(|s| s.site_id == details_1.exploration_site_id))
    });
    assert!(exp_pruned);

    // Since cooldown was NOT armed, ensure_seasonal_event_sites_for_system spawns fresh replacement.
    let respawned = ensure_seasonal_event_sites_for_system(&ctx, sys_1).await;
    assert_eq!(respawned.len(), 1);
    assert_ne!(respawned[0].instance_id, site_1.instance_id);

    // Part B: Past season end window via live apply_damage_with_attacker death hook.
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix + 8 * 86_400)));
    let (_, dead, _) = h2
        .apply_damage_with_attacker(
            site_2.boss_npc_ball_id,
            eve_destiny::DamageVector::new(1_000_000.0, 1_000_000.0, 1_000_000.0, 1_000_000.0),
            Some(pilot),
        )
        .await
        .expect("damage npc")
        .expect("npc ball exists");
    assert!(dead);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&pilot].balance.0, 0);
        assert!(
            !load_seasonal_cooldown_store(snap)
                .until_by_system
                .contains_key(&sys_2),
            "out-of-season cleanup must NOT arm respawn cooldown"
        );
        assert!(!snap.transient_items.contains_key(&details_2.relay_ball_id));
    });
    assert!(h2.get_ball_snapshot(details_2.relay_ball_id).await.unwrap().is_none());
    crate::services::combat_crimewatch::with_combat_ephemeral(&ctx, |rt| {
        assert!(rt.is_recently_destroyed_item_id(site_2.boss_npc_ball_id));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seasonal_concurrent_ensure_and_npc_destruction_never_resurrects_or_duplicates() {
    let sde = shared_sde();
    let sys_1: SolarSystemId = 30_000_142;
    let sys_2: SolarSystemId = 30_000_144;
    let pilot: i64 = 90_060_831;
    let ship_id: i64 = 1_060_831;
    let ctx = Arc::new(setup_seasonal_test_ctx(
        Arc::clone(&sde),
        1_776_200_000,
        1,
        "Crimson Harvest",
        7,
        pilot,
        ship_id,
        sys_1,
    ));

    // 1. Concurrent creation callers serialize cleanly without duplicate sites or overwrites.
    let c1 = Arc::clone(&ctx);
    let c2 = Arc::clone(&ctx);
    let (a1_res, a2_res) = tokio::join!(
        tokio::spawn(async move { ensure_seasonal_event_sites_for_system(&c1, sys_1).await }),
        tokio::spawn(async move { ensure_seasonal_event_sites_for_system(&c2, sys_1).await }),
    );
    let a1 = a1_res.expect("task a1");
    let a2 = a2_res.expect("task a2");
    assert_eq!(a1.len(), 1);
    assert_eq!(a2.len(), 1);
    assert_eq!(a1[0].instance_id, a2[0].instance_id);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.pve_special_sites.seasonal_event_sites[&sys_1].len(), 1);
    });

    // 2. Concurrent recovery re-materialization vs NPC destruction & completion never resurrects.
    let s2 = ensure_seasonal_event_sites_for_system(&ctx, sys_2).await;
    assert_eq!(s2.len(), 1);
    let site = s2[0].clone();
    let details = get_seasonal_site_details(&ctx, site.instance_id).expect("details");
    let h2 = ctx.systems.get_or_spawn(sys_2);

    // Simulate missing relay, missing escort ball, & missing AI before racing recovery against live kills.
    h2.try_remove_ball(details.relay_ball_id);
    h2.try_remove_ball(site.escort_ball_ids[0]);
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_entities.remove(&site.boss_npc_ball_id);
        for &ebid in &site.escort_ball_ids {
            rt.npc_entities.remove(&ebid);
        }
    });

    let ctx_a = Arc::clone(&ctx);
    let ctx_b = Arc::clone(&ctx);
    let boss_id = site.boss_npc_ball_id;
    let esc_0 = site.escort_ball_ids[0];
    let esc_1 = site.escort_ball_ids[1];
    let (race_res, kill_res) = tokio::join!(
        tokio::spawn(async move {
            let r1 = ensure_seasonal_event_sites_for_system(&ctx_a, sys_2).await;
            tokio::task::yield_now().await;
            let r2 = ensure_seasonal_event_sites_for_system(&ctx_a, sys_2).await;
            (r1, r2)
        }),
        tokio::spawn(async move {
            let mut sum = 0usize;
            for &bid in &[esc_0, boss_id, esc_1] {
                tokio::task::yield_now().await;
                sum += on_special_site_npc_destroyed_core(
                    &ctx_b.systems,
                    &ctx_b.db,
                    &ctx_b.sde,
                    sys_2,
                    bid,
                    Some(pilot),
                )
                .await
                .len();
                h2.try_remove_ball(bid);
            }
            sum
        })
    );
    let rem_after_race = race_res.expect("race task");
    let payouts = kill_res.expect("kill task");
    assert!(
        rem_after_race.0.len() <= 1 && rem_after_race.1.len() <= 1,
        "racing ensure must never create more than one site"
    );
    assert_eq!(payouts, 1, "site completion must pay out exactly once");

    let final_sites = ensure_seasonal_event_sites_for_system(&ctx, sys_2).await;
    assert!(final_sites.is_empty());
    let h2_check = ctx.systems.get_or_spawn(sys_2);
    for &bid in &[boss_id, esc_0, esc_1, details.relay_ball_id] {
        assert!(
            h2_check.get_ball_snapshot(bid).await.unwrap().is_none(),
            "ball {bid} must never resurrect after completion during concurrent ensure"
        );
    }
    crate::services::combat_crimewatch::with_combat_ephemeral(&ctx, |rt| {
        for &bid in &[boss_id, esc_0, esc_1] {
            assert!(
                !rt.npc_entities.contains_key(&bid),
                "defeated seasonal NPC {bid} must never resurrect into rt.npc_entities"
            );
        }
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&pilot].balance.0, site.reward_isk * 100);
        assert!(
            load_seasonal_cooldown_store(snap)
                .until_by_system
                .contains_key(&sys_2)
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seasonal_npc_spawn_racing_step_extended_npc_ai_sync_preserves_ai_and_empty_loot_skips_can() {
    let sde = shared_sde();
    let sys_id: SolarSystemId = 30_000_142;
    let pilot: i64 = 90_060_841;
    let ship_id: i64 = 1_060_841;
    let ctx = Arc::new(setup_seasonal_test_ctx(
        Arc::clone(&sde),
        1_776_250_000,
        1,
        "Crimson Harvest",
        7,
        pilot,
        ship_id,
        sys_id,
    ));

    // 1. Fresh spawn raced against concurrent 1 Hz `step_extended_npc_ai_sync` loop (`R1482-R4-P1-2`).
    let stop_ai = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ctx_ai = Arc::clone(&ctx);
    let stop_flag = Arc::clone(&stop_ai);
    let ai_loop = tokio::spawn(async move {
        while !stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
            crate::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai_sync(
                &ctx_ai,
                sys_id,
            );
            tokio::task::yield_now().await;
        }
    });
    let sites = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    stop_ai.store(true, std::sync::atomic::Ordering::Relaxed);
    ai_loop.await.expect("fresh ai loop");
    assert_eq!(sites.len(), 1);
    let site = sites[0].clone();
    let details = get_seasonal_site_details(&ctx, site.instance_id).expect("details");
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut all_npcs = vec![site.boss_npc_ball_id];
    all_npcs.extend_from_slice(&site.escort_ball_ids);
    for &bid in &all_npcs {
        assert!(
            sys_handle.get_ball_snapshot(bid).await.unwrap().is_some(),
            "fresh spawned NPC {bid} must exist in Ballpark"
        );
    }
    crate::services::combat_crimewatch::with_combat_ephemeral(&ctx, |rt| {
        for &bid in &all_npcs {
            assert!(
                rt.npc_entities.contains_key(&bid),
                "fresh spawned NPC {bid} must remain registered in rt.npc_entities after AI race"
            );
        }
    });

    // 2. Recovery re-spawn deterministically interrupted mid-`spawn_ball_with_meta` by
    //    `step_extended_npc_ai_sync` hooks must still publish all living AI entities (`R1482-R4-P1-2`).
    let boss_bid = site.boss_npc_ball_id;
    sys_handle.try_remove_ball(boss_bid);
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_entities.remove(&boss_bid);
    });
    let ctx_after = Arc::clone(&ctx);
    crate::system_actor::set_spawn_ball_with_meta_after_send_hook_for_test(
        sys_id,
        boss_bid,
        Some(Box::new(move || {
            Box::pin(async move {
                crate::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai_sync(
                    &ctx_after,
                    sys_id,
                );
            })
        })),
    );
    let ctx_pre = Arc::clone(&ctx);
    crate::system_actor::set_spawn_ball_with_meta_pre_reply_hook_for_test(
        sys_id,
        boss_bid,
        Some(Box::new(move || {
            Box::pin(async move {
                crate::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai_sync(
                    &ctx_pre,
                    sys_id,
                );
            })
        })),
    );
    let recovered = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert_eq!(recovered.len(), 1);

    // Spawn pilot ship at the site and step AI once more; NPCs must stay alive and engage.
    sys_handle
        .spawn_ball_with_meta(
            make_site_npc_ball(ship_id, 587, DVec3::from_array(details.site_position)),
            eve_destiny::CombatState::default(),
            eve_destiny::SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: pilot,
                char_id: Some(pilot),
                ..Default::default()
            },
        )
        .await
        .expect("spawn pilot ship");
    crate::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai_sync(&ctx, sys_id);
    for &bid in &all_npcs {
        assert!(
            sys_handle.get_ball_snapshot(bid).await.unwrap().is_some(),
            "recovered NPC {bid} must exist in Ballpark"
        );
    }
    crate::services::combat_crimewatch::with_combat_ephemeral(&ctx, |rt| {
        for &bid in &all_npcs {
            let ent = rt
                .npc_entities
                .get(&bid)
                .unwrap_or_else(|| panic!("recovered NPC {bid} evicted from rt.npc_entities"));
            assert_eq!(
                ent.target_id,
                Some(ship_id),
                "seasonal NPC {bid} must remain active and acquire pilot ship"
            );
        }
    });

    // 3. Synthetic empty-loot completion with undocked ship_id == 0 must not spawn an empty
    //    overflow cargo container (`P2 #1`).
    let min_sde = Arc::new(SdeRegistry::default());
    let ctx_empty = Arc::new(setup_seasonal_test_ctx(
        min_sde,
        1_776_260_000,
        1,
        "Crimson Harvest",
        7,
        pilot,
        ship_id,
        sys_id,
    ));
    let min_sites = ensure_seasonal_event_sites_for_system(&ctx_empty, sys_id).await;
    assert_eq!(min_sites.len(), 1);
    let min_site = &min_sites[0];
    ctx_empty.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&pilot) {
            ch.active_ship_id = 0;
        }
        if let Some(list) = snap.pve_special_sites.seasonal_event_sites.get_mut(&sys_id) {
            for s in list.iter_mut() {
                s.loot_type_id = 0;
            }
        }
    });
    let mut empty_kills = vec![min_site.boss_npc_ball_id];
    empty_kills.extend_from_slice(&min_site.escort_ball_ids);
    for bid in empty_kills {
        let _ = on_special_site_npc_destroyed_core(
            &ctx_empty.systems,
            &ctx_empty.db,
            &ctx_empty.sde,
            sys_id,
            bid,
            Some(pilot),
        )
        .await;
    }
    let expected_cont_type = resolve_loot_container_type_id(&ctx_empty.sde);
    ctx_empty.db.with_snapshot(|snap| {
        let stray_cont = snap.items.values().any(|it| {
            it.owner_id == pilot
                && it.location_id == i64::from(sys_id)
                && it.type_id == expected_cont_type
        });
        assert!(
            !stray_cont,
            "empty loot_specs must not create an empty overflow Cargo Container"
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seasonal_sentinel_zero_killer_does_not_hijack_shipless_character() {
    let sde = shared_sde();
    let sys_id: SolarSystemId = 30_000_142;
    let shipless_pilot: i64 = 90_060_941;
    let ship_id: i64 = 1_060_941;
    let ctx = setup_seasonal_test_ctx(
        sde,
        1_776_270_000,
        1,
        "Crimson Harvest",
        7,
        shipless_pilot,
        ship_id,
        sys_id,
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&shipless_pilot) {
            ch.active_ship_id = 0;
            ch.balance = IskCents(0);
        }
    });

    let sites = ensure_seasonal_event_sites_for_system(&ctx, sys_id).await;
    assert_eq!(sites.len(), 1);
    let site = &sites[0];
    assert!(site.loot_type_id > 0);
    let details = get_seasonal_site_details(&ctx, site.instance_id).expect("details");
    let anom_key = format!("dng-{}-{}", site.dungeon_id, site.instance_id);
    let cont_type = resolve_loot_container_type_id(&ctx.sde);

    let mut kill_order = vec![site.boss_npc_ball_id];
    kill_order.extend_from_slice(&site.escort_ball_ids);
    for bid in kill_order {
        let acct_changes = on_special_site_npc_destroyed_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            sys_id,
            bid,
            Some(0),
        )
        .await;
        assert!(
            acct_changes.is_empty(),
            "sentinel Some(0) killer must never emit ISK account changes (ball {bid})"
        );
    }

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&shipless_pilot].balance.0, 0);
        let season_st = crate::services::season_manager::load_season_state(snap, shipless_pilot);
        assert_eq!(season_st.season_points, 0);
        assert!(
            !snap.items.values().any(|it| {
                it.owner_id == shipless_pilot
                    && it.item_id != ship_id
                    && (it.type_id == cont_type || it.type_id == site.loot_type_id)
            }),
            "shipless character must receive zero loot items or overflow containers"
        );
        assert!(
            !snap
                .items
                .values()
                .any(|it| it.type_id == cont_type && it.location_id == i64::from(sys_id)),
            "sentinel Some(0) killer must not spawn an overflow cargo container in space"
        );
        assert!(snap.pve_special_sites.seasonal_event_sites[&sys_id]
            .iter()
            .any(|s| s.instance_id == site.instance_id && s.completed));
        assert!(!load_seasonal_site_details_map(snap).contains_key(&site.instance_id));
        assert!(load_seasonal_cooldown_store(snap)
            .until_by_system
            .contains_key(&sys_id));
        assert!(snap.cosmic_anomalies[&sys_id]
            .iter()
            .any(|a| a.anomaly_id == anom_key && a.cleared));
        assert!(!snap.transient_items.contains_key(&details.relay_ball_id));
        assert!(!snap.item_positions.contains_key(&details.relay_ball_id));
    });
    let exp_still_present = crate::services::pve_exploration::read_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .get(&sys_id)
            .is_some_and(|l| l.iter().any(|s| s.site_id == details.exploration_site_id))
    });
    assert!(!exp_still_present);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert!(
        sys_handle
            .get_ball_snapshot(details.relay_ball_id)
            .await
            .unwrap()
            .is_none(),
        "relay ball must be removed from Ballpark after sentinel Some(0) completion"
    );
}
