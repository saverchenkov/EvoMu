//! Semantic unit tests split from `pve_exploration_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::pve_exploration_extra_unit_tests::*;

use super::pve_exploration::*;
use eve_db::FleetRecord;
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::HashMap;

#[test]
fn notify_pve_destruction_events_dispatches_fleet_and_escalation_packets() {
    let sys_id: SolarSystemId = 30_000_142;
    let killer_id = 90_100_010;
    let wingman_id = 90_100_011;
    let fleet_id = 77_001;
    let (ctx, _) = make_ctx_and_session(sys_id, killer_id, 100_100_010, 587);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&killer_id) {
            ch.fleet_id = Some(fleet_id);
        }
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: killer_id,
                members: vec![killer_id, wingman_id],
                ..Default::default()
            },
        );
    });
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            sys_id,
            vec![PveSiteInstance {
                site_id: "ANOM-NOTIF-1".into(),
                dungeon_id: 4321,
                solar_system_id: sys_id,
                site_category: "CombatAnomaly".into(),
                ..Default::default()
            }],
        );
    });

    let esc = EscalatingPathEntry {
        instance_id: 55_001,
        character_id: killer_id,
        source_dungeon_id: 4321,
        expedition_dungeon_id: 4322,
        expedition_name: "Escalated Outpost".into(),
        destination_solar_system_id: sys_id,
        step_index: 1,
        total_steps: 4,
        journal_text: "Follow the trail".into(),
        created_at_sec: 1_776_000_000,
        expires_at_sec: 1_800_000_000,
        dungeons_entered: Vec::new(),
        journal_entry_template_id: 99,
    };

    notify_pve_destruction_events(
        &ctx.db,
        &ctx.pending_notifications,
        &ctx.notify_tx,
        killer_id,
        Some("ANOM-NOTIF-1".into()),
        Some(esc.clone()),
        Some(esc),
    );

    let queues = ctx.pending_notifications.read().unwrap();
    let killer_events: Vec<&str> = queues
        .get(&killer_id)
        .unwrap()
        .iter()
        .map(|n| n.event.as_str())
        .collect();
    assert!(killer_events.contains(&"OnCosmicAnomalyRemoved"));
    assert!(killer_events.contains(&"OnDungeonCompleted"));
    assert!(killer_events.contains(&"OnEscalatingPathChange"));
    assert!(killer_events.contains(&"OnEscalatingPathMessage"));
    assert!(killer_events.contains(&"OnFleetEscalatingPathMessage"));

    let wingman_events: Vec<&str> = queues
        .get(&wingman_id)
        .unwrap()
        .iter()
        .map(|n| n.event.as_str())
        .collect();
    assert!(wingman_events.contains(&"OnFleetEscalatingPathMessage"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn advance_site_wave_or_complete_and_exploration_rpc_edge_cases() {
    let sys_id: SolarSystemId = 30_000_142;
    let char_id = 90_100_020;
    let ship_id = 100_100_020;
    let (ctx, mut sess) = make_ctx_and_session(sys_id, char_id, ship_id, 587);
    let kw = HashMap::new();

    // 1. Docked guard
    sess.station_id = Some(60_003_760);
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "ANY")
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    sess.station_id = None;

    // 2. Wrong system guard
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, 30_000_144, "ANY")
            .await
            .unwrap_err()
            .code,
        "NotInSiteSystem"
    );

    // 3. Site not found
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "NONEXISTENT-SITE")
            .await
            .unwrap_err()
            .code,
        "SiteNotFound"
    );

    // Seed custom sites for wave/completion & RPC tests
    let live_ball_id = 888_101;
    let gate_ball_id = 888_202;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: live_ball_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 20.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    with_pve_exploration_reg(&ctx, |reg| {
        reg.escalations_by_char.insert(
            char_id,
            vec![EscalatingPathEntry {
                instance_id: 701,
                character_id: char_id,
                source_dungeon_id: 100,
                expedition_dungeon_id: 5604,
                expedition_name: "Owned Expedition".into(),
                destination_solar_system_id: sys_id,
                step_index: 1,
                total_steps: 3,
                journal_text: "Step 1".into(),
                created_at_sec: 1_776_000_000,
                expires_at_sec: 1_900_000_000,
                dungeons_entered: Vec::new(),
                journal_entry_template_id: 1,
            }],
        );
        let sites = reg.sites_by_system.entry(sys_id).or_default();
        sites.push(PveSiteInstance {
            site_id: "CLEARED-1".into(),
            dungeon_id: 501,
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            cleared: true,
            ..Default::default()
        });
        sites.push(PveSiteInstance {
            site_id: "UNHACKED-GHOST".into(),
            dungeon_id: 502,
            solar_system_id: sys_id,
            site_category: "Ghost".into(),
            hackable_can_ids: vec![910_001],
            ..Default::default()
        });
        sites.push(PveSiteInstance {
            site_id: "LIVE-WAVE-1".into(),
            dungeon_id: 503,
            solar_system_id: sys_id,
            site_category: "CombatAnomaly".into(),
            current_wave: 1,
            total_waves: 2,
            active_npc_ball_ids: vec![live_ball_id],
            ..Default::default()
        });
        sites.push(PveSiteInstance {
            site_id: "SLEEPER-CACHE-1".into(),
            dungeon_id: 5604,
            dungeon_name: "Limited Sleeper Cache".into(),
            solar_system_id: sys_id,
            site_category: "SleeperCache".into(),
            sleeper_rift_stabilized: true,
            current_wave: 1,
            total_waves: 1,
            active_npc_ball_ids: vec![live_ball_id],
            ..Default::default()
        });
        sites.push(PveSiteInstance {
            site_id: "DED-GATE-SITE".into(),
            dungeon_id: 504,
            dungeon_name: "DED Complex".into(),
            solar_system_id: sys_id,
            site_category: "DED".into(),
            ded_rating: Some(5),
            gate_ball_id: Some(gate_ball_id),
            current_room: 1,
            total_rooms: 2,
            current_wave: 1,
            total_waves: 2,
            ..Default::default()
        });
        sites.push(PveSiteInstance {
            site_id: "EXP-OWNED".into(),
            dungeon_id: 505,
            dungeon_name: "Owned Expedition".into(),
            solar_system_id: sys_id,
            site_category: "Expedition".into(),
            expedition_instance_id: Some(701),
            probe_required: false,
            ..Default::default()
        });
        sites.push(PveSiteInstance {
            site_id: "EXP-FOREIGN".into(),
            dungeon_id: 506,
            dungeon_name: "Foreign Expedition".into(),
            solar_system_id: sys_id,
            site_category: "Expedition".into(),
            expedition_instance_id: Some(999),
            probe_required: false,
            ..Default::default()
        });
    });

    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "CLEARED-1")
            .await
            .unwrap_err()
            .code,
        "SiteAlreadyCleared"
    );
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "UNHACKED-GHOST")
            .await
            .unwrap_err()
            .code,
        "SiteContainersUnhacked"
    );
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "LIVE-WAVE-1")
            .await
            .unwrap_err()
            .code,
        "WaveNpcsRemaining"
    );

    // Stabilized SleeperCache ignores live_ball_id and completes
    let sc_done = advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "SLEEPER-CACHE-1")
        .await
        .expect("stabilized sleeper cache completes");
    assert_eq!(sc_done.dict_get("completed"), Some(&PyRep::Bool(true)));

    // Remove live_ball_id -> LIVE-WAVE-1 advances to wave 2 (completed = false)
    let _ = sys_handle.remove_ball(live_ball_id).await;
    let w2 = advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "LIVE-WAVE-1")
        .await
        .expect("advances to wave 2");
    assert_eq!(w2.dict_get("currentWave"), Some(&PyRep::Integer(2)));
    assert_eq!(w2.dict_get("completed"), Some(&PyRep::Bool(false)));

    // GetCombatAnomalyInstances includes EXP-OWNED and excludes EXP-FOREIGN & CLEARED-1
    let PyRep::List(anom_list) = handle_pve_exploration(
        &ctx,
        &mut sess,
        "scanSvc",
        "GetCombatAnomalyInstances",
        &[PyRep::Integer(sys_id as i64)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap() else {
        panic!("expected list");
    };
    let site_ids: Vec<String> = anom_list
        .iter()
        .filter_map(|r| r.dict_get("siteID")?.as_str().map(str::to_string))
        .collect();
    assert!(site_ids.contains(&"EXP-OWNED".to_string()));
    assert!(!site_ids.contains(&"EXP-FOREIGN".to_string()));
    assert!(!site_ids.contains(&"CLEARED-1".to_string()));

    // GetDungeonShipRestrictions for SleeperCache dungeon IDs (Limited=5604, Standard=5609, Superior=5632) and by site_id
    for (did, expected_name) in [
        (5604, "Limited Sleeper Cache"),
        (5609, "Standard Sleeper Cache"),
        (5632, "Superior Sleeper Cache"),
    ] {
        let rep = handle_pve_exploration(&ctx, &mut sess, "keeper", "GetDungeonShipRestrictions", &[PyRep::Integer(did)], &kw)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rep.dict_get("dungeonName").and_then(|v| v.as_str()), Some(expected_name));
    }
    let by_site = handle_pve_exploration(&ctx, &mut sess, "keeper", "GetDungeonShipRestrictions", &[PyRep::String("DED-GATE-SITE".into())], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_site.dict_get("dedRating"), Some(&PyRep::Integer(5)));

    // ActivateAccelerationGate with unknown integer gate returns None; with DED gate_ball_id resolves site
    assert!(handle_pve_exploration(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(999_888_777)], &kw)
        .await
        .is_none());

    let gate_res = handle_pve_exploration(&ctx, &mut sess, "beyonce", "ActivateAccelerationGate", &[PyRep::Integer(gate_ball_id)], &kw)
        .await;
    assert!(gate_res.is_some());

    // GetDungeonObjectivesData missing -> DungeonNotFound
    assert_eq!(
        handle_pve_exploration(&ctx, &mut sess, "keeper", "GetDungeonObjectivesData", &[PyRep::String("NO-SUCH-DUNGEON".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DungeonNotFound"
    );

    // GetMyEscalatingPathDetails with extra args -> InvalidArguments
    assert!(handle_pve_exploration(&ctx, &mut sess, "scanSvc", "GetMyEscalatingPathDetails", &[PyRep::Integer(1)], &kw)
        .await
        .unwrap()
        .is_err());
}
