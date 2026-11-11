//! Edge-case unit tests for `pve_exploration`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::cache_notifications::PendingMachoNotification;
use crate::services::pve_exploration_extra_unit_tests::{make_ctx_and_session, shared_sde};
use eve_db::{FleetRecord, OwnerNoteRecord};
use eve_proto::PyRep;
use std::collections::HashMap;
use std::sync::RwLock;
use tokio::sync::broadcast;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_reg_clock_clamp_fleet_notifications_and_queue_eviction() {
    let (ctx, mut sess) = make_ctx_and_session(30_000_142, 90_200_001, 100_200_001, 587);
    let cid = 90_200_001_i64;
    let wingman = 90_200_002_i64;

    // Corrupt JSON in PVE_EXPLORATION_NOTE_ID falls back gracefully to default registry.
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(
            PVE_EXPLORATION_NOTE_ID,
            OwnerNoteRecord {
                note_id: PVE_EXPLORATION_NOTE_ID,
                character_id: 0,
                item_id: 0,
                label: "pve_reg".into(),
                note: "{corrupt_json".into(),
                timestamp_unix: 0,
            },
        );
    });
    with_pve_exploration_reg(&ctx, |reg| {
        assert!(reg.sites_by_system.is_empty());
    });

    // advance_pve_exploration_clock when sim_clock_sec >= cur_now exercises the saturating max branch
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sim_clock_sec = 2_000_000_000;
    });
    advance_pve_exploration_clock(&ctx, 120);
    with_pve_exploration_reg(&ctx, |reg| {
        assert_eq!(reg.sim_clock_sec, 2_000_000_120);
    });

    // notify_pve_destruction_events: fleet notifications + 256-entry queue cap eviction
    ctx.db.with_mut_snapshot(|snap| {
        let members = vec![cid, wingman];
        snap.characters.get_mut(&cid).unwrap().fleet_id = Some(800_001);
        snap.fleets.insert(
            800_001,
            FleetRecord {
                fleet_id: 800_001,
                boss_id: cid,
                members,
                ..Default::default()
            },
        );
    });

    let pend: RwLock<HashMap<i64, Vec<PendingMachoNotification>>> = RwLock::new(HashMap::new());
    {
        let mut q = pend.write().unwrap();
        let entry = q.entry(cid).or_default();
        for i in 0..256 {
            let (pkt, mpkt) = crate::router::ServerContext::build_macho_notification_packet(
                cid,
                None,
                "OnCosmicAnomalyRemoved",
                "charid",
                vec![PyRep::Integer(i)],
            );
            entry.push(PendingMachoNotification { seq_id: 0,
                recipient_char_id: cid,
                event: "OnCosmicAnomalyRemoved".into(),
                id_type: "charid".into(),
                args: vec![PyRep::Integer(i)],
                packet: pkt,
                marshaled_packet: mpkt,
                created_filetime: 1,
            });
        }
    }
    let (tx, _rx) = broadcast::channel(64);
    let esc = EscalatingPathEntry {
        instance_id: 77_001,
        character_id: cid,
        source_dungeon_id: 1001,
        expedition_dungeon_id: 1002,
        expedition_name: "Escalation Path".into(),
        destination_solar_system_id: 30_000_144,
        step_index: 1,
        total_steps: 4,
        journal_text: "Follow".into(),
        created_at_sec: 1_776_000_000,
        expires_at_sec: 1_776_086_400,
        dungeons_entered: vec![],
        journal_entry_template_id: 0,
    };

    notify_pve_destruction_events(
        &ctx.db,
        &pend,
        &tx,
        cid,
        None,
        Some(esc),
        None,
    );
    let q = pend.read().unwrap();
    assert_eq!(q.get(&cid).unwrap().len(), 256);
    assert!(q
        .get(&wingman)
        .unwrap()
        .iter()
        .any(|n| n.event == "OnFleetEscalatingPathMessage"));
    drop(q);
    let _ = &mut sess;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn npc_destruction_core_wormhole_combat_multiwave_and_advance_guards() {
    let sys_id = 30_000_142_u32;
    let wh_sys = 31_000_200_u32;
    let (ctx, mut sess) = make_ctx_and_session(sys_id, 90_200_003, 100_200_003, 587);
    let cid = 90_200_003_i64;
    let kw = HashMap::new();

    // Docked errors (station_id & structure_id) + NotInSiteSystem error
    sess.station_id = Some(60_003_760);
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "ANY")
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    sess.station_id = None;
    sess.structure_id = Some(1_000_000_001);
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "ANY")
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    sess.structure_id = None;
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, 30_000_144, "ANY")
            .await
            .unwrap_err()
            .code,
        "NotInSiteSystem"
    );

    // SiteNotFound, SiteAlreadyCleared, SiteContainersUnhacked, WaveNpcsRemaining, SleeperCache stabilized bypass
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let dummy_ball = eve_destiny::Ball {
        id: 890_000_001,
        type_id: 587,
        position: glam::DVec3::ZERO,
        velocity: glam::DVec3::ZERO,
        max_velocity: 100.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let _ = sys_h
        .spawn_ball_with_combat(
            dummy_ball,
            crate::services::pve_exploration_seeding::build_type_combat_state_from_sde(&ctx.sde, 587),
        )
        .await;

    let now_sites = ctx.now_unix_secs();
    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        let sites = reg.sites_by_system.entry(sys_id).or_default();
        sites.push(PveSiteInstance {
            site_id: "CLEARED-1".into(),
            solar_system_id: sys_id,
            dungeon_id: 90_001,
            dungeon_name: "Cleared".into(),
            site_category: "CombatAnomaly".into(),
            ded_rating: None,
            wormhole_class: None,
            position: [0.0; 3],
            probe_required: false,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 1,
            active_npc_ball_ids: vec![],
            hackable_can_ids: vec![],
            npc_guards_spawned: false,
            ghost_timer_started_at: None,
            ghost_detonate_after_sec: 0,
            ghost_detonated: false,
            cleared: true,
            sleeper_rift_stabilized: false,
            sleeper_sentry_disabled: false,
            sleeper_gas_cloud_active: false,
            created_unix: now_sites,
            lifetime_seconds: 86_400,
            expedition_instance_id: None,
            gate_ball_id: None,
            activated_at_downtime: None,
            dread_escalation_triggers: 0,
            carrier_escalation_triggers: 0,
            active_escalation_capitals: vec![],
            is_shattered_resource_site: false,
            delayed_wave_triggered: false,
            relay_spawned: false,
        });
        sites.push(PveSiteInstance {
            site_id: "UNHACKED-1".into(),
            solar_system_id: sys_id,
            dungeon_id: 90_002,
            dungeon_name: "Unhacked".into(),
            site_category: "Data".into(),
            ded_rating: None,
            wormhole_class: None,
            position: [0.0; 3],
            probe_required: true,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 1,
            active_npc_ball_ids: vec![],
            hackable_can_ids: vec![555_001],
            npc_guards_spawned: false,
            ghost_timer_started_at: None,
            ghost_detonate_after_sec: 0,
            ghost_detonated: false,
            cleared: false,
            sleeper_rift_stabilized: false,
            sleeper_sentry_disabled: false,
            sleeper_gas_cloud_active: false,
            created_unix: now_sites,
            lifetime_seconds: 86_400,
            expedition_instance_id: None,
            gate_ball_id: None,
            activated_at_downtime: None,
            dread_escalation_triggers: 0,
            carrier_escalation_triggers: 0,
            active_escalation_capitals: vec![],
            is_shattered_resource_site: false,
            delayed_wave_triggered: false,
            relay_spawned: false,
        });
        sites.push(PveSiteInstance {
            site_id: "LIVE-WAVE-1".into(),
            solar_system_id: sys_id,
            dungeon_id: 90_003,
            dungeon_name: "Live Wave".into(),
            site_category: "CombatAnomaly".into(),
            ded_rating: None,
            wormhole_class: None,
            position: [0.0; 3],
            probe_required: false,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 2,
            active_npc_ball_ids: vec![890_000_001],
            hackable_can_ids: vec![],
            npc_guards_spawned: false,
            ghost_timer_started_at: None,
            ghost_detonate_after_sec: 0,
            ghost_detonated: false,
            cleared: false,
            sleeper_rift_stabilized: false,
            sleeper_sentry_disabled: false,
            sleeper_gas_cloud_active: false,
            created_unix: now_sites,
            lifetime_seconds: 86_400,
            expedition_instance_id: None,
            gate_ball_id: None,
            activated_at_downtime: None,
            dread_escalation_triggers: 0,
            carrier_escalation_triggers: 0,
            active_escalation_capitals: vec![],
            is_shattered_resource_site: false,
            delayed_wave_triggered: false,
            relay_spawned: false,
        });
        sites.push(PveSiteInstance {
            site_id: "SC-STAB-1".into(),
            solar_system_id: sys_id,
            dungeon_id: 300_001,
            dungeon_name: "Sleeper Cache".into(),
            site_category: "SleeperCache".into(),
            ded_rating: None,
            wormhole_class: None,
            position: [0.0; 3],
            probe_required: true,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 1,
            active_npc_ball_ids: vec![890_000_001], // bypassed because sleeper_rift_stabilized == true
            hackable_can_ids: vec![],
            npc_guards_spawned: false,
            ghost_timer_started_at: None,
            ghost_detonate_after_sec: 0,
            ghost_detonated: false,
            cleared: false,
            sleeper_rift_stabilized: true,
            sleeper_sentry_disabled: false,
            sleeper_gas_cloud_active: false,
            created_unix: now_sites,
            lifetime_seconds: 86_400,
            expedition_instance_id: None,
            gate_ball_id: None,
            activated_at_downtime: None,
            dread_escalation_triggers: 0,
            carrier_escalation_triggers: 0,
            active_escalation_capitals: vec![],
            is_shattered_resource_site: false,
            delayed_wave_triggered: false,
            relay_spawned: false,
        });
    });

    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "NOPE")
            .await
            .unwrap_err()
            .code,
        "SiteNotFound"
    );
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "CLEARED-1")
            .await
            .unwrap_err()
            .code,
        "SiteAlreadyCleared"
    );
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "UNHACKED-1")
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

    // SleeperCache with sleeper_rift_stabilized bypasses WaveNpcsRemaining and completes
    let sc_done = advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "SC-STAB-1")
        .await
        .expect("sleeper cache stabilized completes");
    assert_eq!(sc_done.dict_get("completed"), Some(&PyRep::Bool(true)));

    // Remove dummy ball so LIVE-WAVE-1 advances from wave 1 -> wave 2, then test handle_pve_site_npc_destruction_core
    let _ = sys_h.remove_ball(890_000_001).await;
    let adv_res = advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "LIVE-WAVE-1")
        .await
        .expect("wave advances to 2");
    assert_eq!(adv_res.dict_get("completed"), Some(&PyRep::Bool(false)));
    assert_eq!(adv_res.dict_get("currentWave"), Some(&PyRep::Integer(2)));

    // WormholeCombat C5 site: non-trigger kill leaves !wave_cleared; final kill drops Blue Loot & spawns relay
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(wh_sys).or_default().push(PveSiteInstance {
            site_id: "WH-C5-EDGE".into(),
            solar_system_id: wh_sys,
            dungeon_id: 95_001,
            dungeon_name: "Core Garrison".into(),
            site_category: "WormholeCombat".into(),
            ded_rating: None,
            wormhole_class: Some(5),
            position: [10_000.0, 0.0, 0.0],
            probe_required: false,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 1,
            active_npc_ball_ids: vec![891_000_001, 891_000_002],
            hackable_can_ids: vec![],
            npc_guards_spawned: false,
            ghost_timer_started_at: None,
            ghost_detonate_after_sec: 0,
            ghost_detonated: false,
            cleared: false,
            sleeper_rift_stabilized: false,
            sleeper_sentry_disabled: false,
            sleeper_gas_cloud_active: false,
            created_unix: now_sites,
            lifetime_seconds: 86_400,
            expedition_instance_id: None,
            gate_ball_id: None,
            activated_at_downtime: None,
            dread_escalation_triggers: 0,
            carrier_escalation_triggers: 0,
            active_escalation_capitals: vec![],
            is_shattered_resource_site: false,
            delayed_wave_triggered: false,
            relay_spawned: false,
        });
    });

    // Kill non-trigger NPC (index 1) -> returns (None, None, None) because trigger 891_000_001 remains
    let res1 = handle_pve_site_npc_destruction_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.mechanic_rng,
        &ctx.combat_ephemeral,
        wh_sys,
        891_000_002,
        cid,
        true,
    )
    .await;
    assert_eq!(res1.0, None);

    // Kill final NPC (index 0) -> wave_cleared & final wave -> clears site, drops blue loot & spawns C5 relay
    let res2 = handle_pve_site_npc_destruction_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        &ctx.mechanic_rng,
        &ctx.combat_ephemeral,
        wh_sys,
        891_000_001,
        cid,
        false,
    )
    .await;
    assert_eq!(res2.0.as_deref(), Some("WH-C5-EDGE"));

    // RPC dispatch edges: NotInSpace fallback, GetDungeonShipRestrictions variants, GetCombatAnomalyInstances filter,
    // GetDungeonInfo not found, GetMyEscalatingPathDetails invalid args, ActivateAccelerationGate unknown int -> None
    let _ = &kw;
    let _ = shared_sde();
    assert_eq!(
        handle_pve_exploration(&ctx, &mut sess, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[PyRep::Integer(1)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    assert_eq!(
        handle_pve_exploration(&ctx, &mut sess, "keeper", "GetDungeonInfo", &[PyRep::String("MISSING".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DungeonNotFound"
    );
    assert!(
        handle_pve_exploration(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(999_888_777)], &kw)
            .await
            .is_none()
    );
    assert!(
        handle_pve_exploration(&ctx, &mut sess, "scanSvc", "GetCombatAnomalyInstances", &[PyRep::Integer(sys_id as i64)], &kw)
            .await
            .unwrap()
            .is_ok()
    );
    let mut sess_no_ship = sess.clone();
    sess_no_ship.ship_id = None;
    let rest_ded = handle_pve_exploration(
        &ctx,
        &mut sess_no_ship,
        "keeper",
        "GetDungeonShipRestrictions",
        &[PyRep::Integer(4)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rest_ded.dict_get("dedRating"), Some(&PyRep::Integer(4)));

    // Sleeper cache dungeon ID > 10 & string lookup for UNHACKED-1
    let rest_str = handle_pve_exploration(
        &ctx,
        &mut sess_no_ship,
        "keeper",
        "GetDungeonShipRestrictions",
        &[PyRep::String("SC-STAB-1".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(rest_str.dict_get("dungeonID").is_some());

    // EnterCombatAnomaly with advance_wave = true -> hits validate_and_advance_pve_site_room
    assert!(
        handle_pve_exploration(
            &ctx,
            &mut sess,
            "scanSvc",
            "EnterCombatAnomaly",
            &[PyRep::String("MISSING-ANOM".into()), PyRep::Bool(true)],
            &kw,
        )
        .await
        .unwrap()
        .is_err()
    );

    // resolve_session_system_id NotInSpace when both session and character absent
    let mut sess_no_sys = eve_machonet::SessionState::new(999);
    sess_no_sys.user_id = Some(999);
    sess_no_sys.character_id = Some(999_777_666);
    assert_eq!(
        handle_pve_exploration(&ctx, &mut sess_no_sys, "keeper", "GetDungeonInfo", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInSpace"
    );
    let _ = sys_h.remove_ball(890_000_001).await;
}
