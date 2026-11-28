use super::*;
use crate::combat::npc_ai::npc_test_helpers::{
    get_npc_entity, has_npc_entity, spawn_decloaked_transmission_relay_for_ctx,
    spawn_upgraded_avengers_for_ctx,
};
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use crate::services::pve_exploration::{with_pve_exploration_reg, PveSiteInstance};

#[cfg(test)]
#[path = "wormhole_pve_escalations_edges_tests.rs"]
mod wormhole_pve_escalations_edges_tests;

#[tokio::test]
async fn test_wormhole_pve_escalations_avengers_relays_and_downtime() {
    let (ctx, _session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let wh_sys: SolarSystemId = 31_000_005;

    // 1. Spawn Upgraded Avengers (3 in C5, 4 in C6)
    let c5_ids = spawn_upgraded_avengers_for_ctx(&ctx, wh_sys, 5, DVec3::ZERO, 0).await;
    assert_eq!(c5_ids.len(), 3);

    let c6_ids = spawn_upgraded_avengers_for_ctx(&ctx, wh_sys, 6, DVec3::ZERO, 1).await;
    assert_eq!(c6_ids.len(), 4);

    // 2. Seed a C6 combat site and a shattered resource site in PvE exploration registry
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            wh_sys,
            vec![
                PveSiteInstance {
                    site_id: "C6-CORE-1".into(),
                    dungeon_id: 3716,
                    dungeon_name: "Core Garrison".into(),
                    solar_system_id: wh_sys,
                    site_category: "WormholeCombat".into(),
                    wormhole_class: Some(6),
                    position: [1_000.0, 0.0, 0.0],
                    cleared: true,
                    activated_at_downtime: Some(0),
                    dread_escalation_triggers: 2,
                    carrier_escalation_triggers: 1,
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "SHD-RES-1".into(),
                    dungeon_id: 3790,
                    dungeon_name: "Shattered Debris Field".into(),
                    solar_system_id: wh_sys,
                    site_category: "ShatteredDebris".into(),
                    wormhole_class: Some(3),
                    position: [50_000.0, 0.0, 0.0],
                    is_shattered_resource_site: true,
                    delayed_wave_triggered: false,
                    cleared: false,
                    ..Default::default()
                },
            ],
        );
    });

    // 3. Spawn Decloaked Transmission Relay and trigger Drifter response escalation
    let relay_bid = spawn_decloaked_transmission_relay_for_ctx(
        &ctx,
        wh_sys,
        "C6-CORE-1",
        [1_000.0, 0.0, 0.0],
    )
    .await
    .expect("relay should spawn for C6 combat site");

    let (drifter_bid, drifter_tid) =
        on_decloaked_relay_attacked_sync(&ctx, wh_sys, relay_bid).expect("drifter should spawn");
    assert!(drifter_bid > 0);
    assert!(matches!(drifter_tid, 37_473 | 86_498));
    // Attacking already-triggered relay is idempotent
    assert!(on_decloaked_relay_attacked_sync(&ctx, wh_sys, relay_bid).is_none());

    // 4. Test on_asteroid_mined_in_system triggers delayed Sleeper wave
    on_asteroid_mined_in_system(&ctx, wh_sys, char_id);
    let delayed_triggered = with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .get(&wh_sys)
            .and_then(|s| s.iter().find(|x| x.site_id == "SHD-RES-1"))
            .map(|x| x.delayed_wave_triggered && !x.active_npc_ball_ids.is_empty())
            .unwrap_or(false)
    });
    assert!(delayed_triggered);

    // 5. Step wormhole PvE system tick
    step_wormhole_pve_system(&ctx, wh_sys, 1.0).await;

    // 6. Test process_wormhole_sites_on_downtime (cleared site despawns at 3rd downtime, unfinished at 4th)
    assert_eq!(process_wormhole_sites_on_downtime(&ctx), 0); // dt = 1
    assert_eq!(process_wormhole_sites_on_downtime(&ctx), 0); // dt = 2
    assert_eq!(process_wormhole_sites_on_downtime(&ctx), 1); // dt = 3 -> cleared site despawns
    assert_eq!(process_wormhole_sites_on_downtime(&ctx), 1); // dt = 4 -> unfinished site despawns
}

#[tokio::test]
async fn r11_k12_escalation_slim_meta_sleeper_sde_stats_and_downtime_relay_cleanup() {
    let (ctx, _session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let wh_sys: SolarSystemId = 31_000_005;

    let avenger_ids = spawn_upgraded_avengers_for_ctx(&ctx, wh_sys, 5, DVec3::ZERO, 0).await;
    assert_eq!(avenger_ids.len(), 3);

    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            wh_sys,
            vec![
                PveSiteInstance {
                    site_id: "C6-RELAY-CLEAN".into(),
                    dungeon_id: 3716,
                    dungeon_name: "Core Garrison".into(),
                    solar_system_id: wh_sys,
                    site_category: "WormholeCombat".into(),
                    wormhole_class: Some(6),
                    position: [2_000.0, 0.0, 0.0],
                    cleared: true,
                    activated_at_downtime: Some(0),
                    ..Default::default()
                },
                PveSiteInstance {
                    site_id: "SHD-RES-CLEAN".into(),
                    dungeon_id: 3790,
                    dungeon_name: "Shattered Debris Field".into(),
                    solar_system_id: wh_sys,
                    site_category: "ShatteredDebris".into(),
                    wormhole_class: Some(3),
                    position: [60_000.0, 0.0, 0.0],
                    is_shattered_resource_site: true,
                    delayed_wave_triggered: false,
                    cleared: false,
                    ..Default::default()
                },
            ],
        );
    });

    let relay_bid = spawn_decloaked_transmission_relay_for_ctx(
        &ctx,
        wh_sys,
        "C6-RELAY-CLEAN",
        [2_000.0, 0.0, 0.0],
    )
    .await
    .expect("relay spawns");
    let (drifter_bid, _) =
        on_decloaked_relay_attacked_sync(&ctx, wh_sys, relay_bid).expect("drifter spawns");

    on_asteroid_mined_in_system(&ctx, wh_sys, char_id);

    let wave_bids = with_pve_exploration_reg(&ctx, |reg| {
        reg.sentry_last_fire_unix.insert(relay_bid, 100.0);
        reg.sentry_last_fire_sim_s.insert(relay_bid, 10.0);
        reg.sites_by_system[&wh_sys]
            .iter()
            .find(|s| s.site_id == "SHD-RES-CLEAN")
            .unwrap()
            .active_npc_ball_ids
            .clone()
    });
    assert!(!wave_bids.is_empty());

    let first_sleeper = get_npc_entity(&ctx, wave_bids[0]).expect("sleeper entity");
    assert!(first_sleeper.custom_cargo_loot.is_some());

    let sys_handle = ctx.systems.get_or_spawn(wh_sys);
    let av0 = avenger_ids[0];
    let w0 = wave_bids[0];
    let (
        av_owner,
        av_cat,
        av_has_name,
        rel_owner,
        rel_gid,
        rel_has_name,
        dr_owner,
        dr_has_name,
        sl_owner,
        sl_has_name,
    ) = sys_handle
        .test_with_ballpark_async(move |bp| {
            let av_meta = bp.ball_slim_meta.get(&av0).expect("avenger slim meta");
            let rel_meta = bp.ball_slim_meta.get(&relay_bid).expect("relay slim meta");
            let dr_meta = bp.ball_slim_meta.get(&drifter_bid).expect("drifter slim meta");
            let sl_meta = bp.ball_slim_meta.get(&w0).expect("sleeper slim meta");
            (
                av_meta.owner_id,
                av_meta.category_id,
                av_meta.name.is_some(),
                rel_meta.owner_id,
                rel_meta.group_id,
                rel_meta.name.is_some(),
                dr_meta.owner_id,
                dr_meta.name.is_some(),
                sl_meta.owner_id,
                sl_meta.name.is_some(),
            )
        })
        .await;
    assert_eq!(av_owner, 500_005);
    assert_eq!(av_cat, 11);
    assert!(av_has_name);
    assert_eq!(rel_owner, 500_024);
    assert_eq!(rel_gid, 494);
    assert!(rel_has_name);
    assert_eq!(dr_owner, 500_024);
    assert!(dr_has_name);
    assert_eq!(sl_owner, 500_005);
    assert!(sl_has_name);

    // Downtime 3 despawns cleared C6 site along with its relay, spawned Drifter, and fire timers
    assert_eq!(process_wormhole_sites_on_downtime(&ctx), 0);
    assert_eq!(process_wormhole_sites_on_downtime(&ctx), 0);
    assert_eq!(process_wormhole_sites_on_downtime(&ctx), 1);

    with_pve_exploration_reg(&ctx, |reg| {
        assert!(!reg.active_relays.contains_key(&relay_bid));
        assert!(!reg.sentry_last_fire_unix.contains_key(&relay_bid));
        assert!(!reg.sentry_last_fire_sim_s.contains_key(&relay_bid));
    });
    let (has_relay, has_drifter) = sys_handle
        .test_with_ballpark_async(move |bp| {
            (
                bp.balls.contains_key(&relay_bid),
                bp.balls.contains_key(&drifter_bid),
            )
        })
        .await;
    assert!(!has_relay);
    assert!(!has_drifter);
    assert!(!has_npc_entity(&ctx, relay_bid));
    assert!(!has_npc_entity(&ctx, drifter_bid));
}
