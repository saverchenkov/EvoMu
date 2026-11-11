//! Unit tests for `pve_special_sites_combat`.

use super::*;
use crate::router::ServerContext;
use crate::services::pve_special_sites::{
    ensure_special_sites_initialized, HomefrontSiteRecord, TriglavianSiteRecord,
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

    let ctx = ServerContext::with_sde_and_repository(sde, db);
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

    // 2. Homefront boss kill distributes full optimal 5-pilot payout to all 5 fleet members
    let hf_boss_bid: i64 = 9_910_001;
    with_pve_special_reg(&ctx, |reg| {
        reg.homefront_sites.entry(sys_id).or_default().push(HomefrontSiteRecord {
            instance_id: 991_001,
            dungeon_id: 1,
            dungeon_name: "Fleet Raid".into(),
            solar_system_id: sys_id,
            optimal_fleet_size: 5,
            base_pilot_isk: 17_230_000,
            completed: false,
            reward_paid_isk: 0,
            boss_npc_ball_id: hf_boss_bid,
            boss_type_id: 48_090,
        });
    });
    let hf_changes = on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, sys_id, hf_boss_bid, Some(90_060_001)).await;
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
    let s_boss = seasonal[0].boss_npc_ball_id;
    let s_changes = on_special_site_npc_destroyed_core(&ctx.systems, &ctx.db, &ctx.sde, sys_id, s_boss, Some(90_060_001)).await;
    assert_eq!(s_changes.len(), 1);
    ctx.db.with_snapshot(|snap| {
        let st = crate::services::season_manager::load_season_state(snap, 90_060_001);
        assert!(st.season_points >= 25);
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

    let ctx = ServerContext::with_sde_and_repository(sde, db);
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

    // 3. Homefront site: killer_char_id = None does NOT complete; solo killer excludes docked & remote fleet mates
    let hf_bid = 9_930_001_i64;
    with_pve_special_reg(&ctx, |reg| {
        reg.homefront_sites.entry(sys_id).or_default().push(HomefrontSiteRecord {
            instance_id: 993_001,
            dungeon_id: 2,
            dungeon_name: "Solo HF".into(),
            solar_system_id: sys_id,
            optimal_fleet_size: 1,
            base_pilot_isk: 2_000_000,
            completed: false,
            reward_paid_isk: 0,
            boss_npc_ball_id: hf_bid,
            boss_type_id: 48_090,
        });
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
