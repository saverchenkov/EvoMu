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
        .find(|t| t.category_id == 6 && t.capacity > 50.0)
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
        let st = crate::services::season_manager::load_season_state_from_blobs(snap, 90_060_001);
        assert!(st.season_points >= 25);
    });
}
