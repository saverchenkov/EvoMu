use super::*;
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
    let c5_ids = spawn_upgraded_avengers_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        wh_sys,
        5,
        DVec3::ZERO,
        0,
    )
    .await;
    assert_eq!(c5_ids.len(), 3);

    let c6_ids = spawn_upgraded_avengers_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
        wh_sys,
        6,
        DVec3::ZERO,
        1,
    )
    .await;
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
    let relay_bid = spawn_decloaked_transmission_relay_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.combat_ephemeral,
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
