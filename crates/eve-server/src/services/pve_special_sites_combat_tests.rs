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
    notify_special_site_account_changes(&ctx.db, &ctx.pending_notifications, &ctx.notify_tx, &hf_changes);
    notify_special_site_account_changes(&ctx.db, &ctx.pending_notifications, &ctx.notify_tx, &[]);
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

    // Trigger final kill with nonexistent character ID -> atomic transaction rolls back ALL writes.
    let bad_kill = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        site.escort_ball_ids[1],
        Some(999_999_999),
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

    // Valid final kill commits atomically, then replay cannot double-pay.
    let ok_kill = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        sys_id,
        site.escort_ball_ids[1],
        Some(pilot),
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
            "wallet must receive exactly one completion payout"
        );
        let st = crate::services::season_manager::load_season_state(snap, pilot);
        assert_eq!(st.season_points, site.reward_points);
    });
}
