//! Edge-case behavioral tests for `pve_exploration`.

use super::*;
use crate::router::{NotificationTarget, ServerContext};
use eve_db::clock::VirtualServerClock;
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn make_ball(id: i64, type_id: u32, position: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn edge_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_750_000_000)));
    ctx
}

fn sample_site(
    site_id: &str,
    dungeon_id: u32,
    sys_id: SolarSystemId,
    cat: &str,
    ded_rating: Option<u8>,
) -> PveSiteInstance {
    PveSiteInstance {
        site_id: site_id.to_string(),
        dungeon_id,
        dungeon_name: format!("Site {site_id}"),
        solar_system_id: sys_id,
        site_category: cat.to_string(),
        ded_rating,
        wormhole_class: None,
        position: [1_000_000.0, 0.0, 0.0],
        probe_required: false,
        signal_strength: 100.0,
        current_room: 1,
        total_rooms: 1,
        current_wave: 1,
        total_waves: 2,
        active_npc_ball_ids: Vec::new(),
        hackable_can_ids: Vec::new(),
        npc_guards_spawned: false,
        ghost_timer_started_at: None,
        ghost_detonate_after_sec: 120,
        ghost_detonated: false,
        sleeper_rift_stabilized: false,
        sleeper_sentry_disabled: false,
        sleeper_gas_cloud_active: false,
        cleared: false,
        created_unix: 1_750_000_000,
        lifetime_seconds: 86_400,
        expedition_instance_id: None,
        gate_ball_id: None,
        activated_at_downtime: None,
        dread_escalation_triggers: 0,
        carrier_escalation_triggers: 0,
        active_escalation_capitals: Vec::new(),
        is_shattered_resource_site: false,
        delayed_wave_triggered: false,
        relay_spawned: false,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exploration_unauth_guards_dungeon_restrictions_info_and_gate_routing() {
    let ctx = edge_ctx();
    let kw = HashMap::new();
    let mut unauth = SessionState::default();

    for (svc, meth) in [
        ("scanSvc", "GetCombatAnomalyInstances"),
        ("scanSvc", "EnterCombatAnomaly"),
        ("keeper", "GetDungeonShipRestrictions"),
        ("keeper", "ActivateAccelerationGate"),
        ("keeper", "GetDungeonInfo"),
        ("dungeonExplorationMgr", "GetMyEscalatingPathDetails"),
        ("dungeonExplorationMgr", "GetEscalatingPathDungeonsEntered"),
    ] {
        let res = handle_pve_exploration(&ctx, &mut unauth, svc, meth, &[], &kw)
            .await
            .expect("route must handle method");
        assert!(res.is_err(), "{svc}.{meth} must reject unauthenticated caller");
    }

    let char_id = 90_000_001;
    let sys_id = 30_000_142;
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // GetMyEscalatingPathDetails rejects non-empty args
    assert_eq!(
        handle_pve_exploration(
            &ctx,
            &mut sess,
            "dungeonExplorationMgr",
            "GetMyEscalatingPathDetails",
            &[PyRep::Integer(1)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArguments"
    );

    // GetDungeonInfo missing -> DungeonNotFound
    assert_eq!(
        handle_pve_exploration(
            &ctx,
            &mut sess,
            "keeper",
            "GetDungeonInfo",
            &[PyRep::String("NOPE-SITE".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "DungeonNotFound"
    );

    // Sleeper Cache site vs DED site lookup by string site_id & numeric dungeon_id, plus gate activation
    let gate_id = 9_876_543;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    if sys_h.get_ball_snapshot(ship_id).await.ok().flatten().is_none() {
        sys_h.try_spawn_ball_with_combat(make_ball(ship_id, 587, DVec3::ZERO), CombatState::default());
    }
    let ship_pos = sys_h
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position)
        .unwrap_or(DVec3::ZERO);
    sys_h.try_spawn_ball_with_combat(
        make_ball(gate_id, 17831, ship_pos + DVec3::new(100.0, 0.0, 0.0)),
        CombatState::default(),
    );

    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);
        let mut sc = sample_site("SC-EDGE-1", 400_100, sys_id, "SleeperCache", None);
        sc.sleeper_rift_stabilized = true;
        sc.sleeper_sentry_disabled = true;
        reg.sites_by_system.entry(sys_id).or_default().push(sc);

        let mut ded = sample_site("DED-EDGE-1", 400_200, sys_id, "DED", Some(4));
        ded.gate_ball_id = Some(gate_id);
        ded.position = [ship_pos.x, ship_pos.y, ship_pos.z];
        ded.total_rooms = 2;
        reg.sites_by_system.entry(sys_id).or_default().push(ded);
    });

    // GetDungeonInfo returns sleeper flags for SC-EDGE-1
    let info = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "GetDungeonInfo",
        &[PyRep::String("SC-EDGE-1".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(info.dict_get("sleeperRiftStabilized"), Some(&PyRep::Bool(true)));
    assert_eq!(info.dict_get("sleeperSentryDisabled"), Some(&PyRep::Bool(true)));
    assert_eq!(info.dict_get("cleared"), Some(&PyRep::Bool(false)));

    // GetDungeonShipRestrictions clamps <=10 DED ratings and matches SleeperCache by site_id
    let sc_rest = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "GetDungeonShipRestrictions",
        &[PyRep::String("SC-EDGE-1".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(sc_rest.dict_get("allowedGroupIDs").is_some());

    // CanEnterDungeonInCurrentShip rejects a capital ship on DED 1
    let big_ship = 200_000_999;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            big_ship,
            InventoryItemRecord {
                item_id: big_ship,
                type_id: 23757, // Archon / carrier group 547 or heavy mass if present, fallback uses type_id
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let ded_rest = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "GetDungeonShipRestrictions",
        &[PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ded_rest.dict_get("maxShipMass").is_some());

    // Unrecognized numeric gate ball returns None so non-PvE gate handlers can take it
    let unknown_gate = handle_pve_exploration(
        &ctx,
        &mut sess,
        "beyonce",
        "ActivateAccelerationGate",
        &[PyRep::Integer(777_777_777)],
        &kw,
    )
    .await;
    assert!(unknown_gate.is_none());

    // Known DED gate_ball_id advances room
    let ok_gate = handle_pve_exploration(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(gate_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ok_gate.dict_get("currentWave"), Some(&PyRep::Integer(2)));
    with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system[&sys_id]
            .iter()
            .find(|s| s.site_id == "DED-EDGE-1")
            .unwrap();
        assert_eq!(s.current_wave, 2);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn advance_site_wave_or_complete_guards_sleeper_stabilized_and_completion() {
    let ctx = edge_ctx();
    let char_id = 90_000_001;
    let sys_id = 30_000_142;

    // Docked caller fails with ShipDocked
    let mut docked = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        station_id: Some(60_003_760),
        ..Default::default()
    };
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut docked, sys_id, "ANY")
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );

    // Wrong solar system fails with NotInSiteSystem
    let mut wrong_sys = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(30_000_144),
        station_id: None,
        ..Default::default()
    };
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut wrong_sys, sys_id, "ANY")
            .await
            .unwrap_err()
            .code,
        "NotInSiteSystem"
    );

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        station_id: None,
        ..Default::default()
    };

    // Missing site -> SiteNotFound
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "DOES-NOT-EXIST")
            .await
            .unwrap_err()
            .code,
        "SiteNotFound"
    );

    // Seed cleared site, unhacked Relic site, wave-blocked Combat site, and stabilized SleeperCache
    let live_npc_id = 9_111_001;
    let sc_npc_id = 9_111_002;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for npc_id in [live_npc_id, sc_npc_id] {
        sys_h.try_spawn_ball_with_combat(
            make_ball(npc_id, 23787, DVec3::new(5000.0, 0.0, 0.0)),
            CombatState::default(),
        );
    }

    with_pve_exploration_reg(&ctx, |reg| {
        reg.seeded_systems.insert(sys_id, true);

        let mut cleared = sample_site("CLR-1", 401_001, sys_id, "Combat", None);
        cleared.cleared = true;

        let mut relic = sample_site("REL-1", 401_002, sys_id, "Relic", None);
        relic.hackable_can_ids = vec![555_001];
        relic.total_waves = 1;

        let mut combat = sample_site("CMB-1", 401_003, sys_id, "Combat", None);
        combat.active_npc_ball_ids = vec![live_npc_id];
        combat.total_waves = 2;

        let mut sc = sample_site("SLC-1", 401_004, sys_id, "SleeperCache", None);
        sc.active_npc_ball_ids = vec![sc_npc_id];
        sc.sleeper_rift_stabilized = true;
        sc.total_waves = 1;

        reg.sites_by_system
            .entry(sys_id)
            .or_default()
            .extend([cleared, relic, combat, sc]);
    });

    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "CLR-1")
            .await
            .unwrap_err()
            .code,
        "SiteAlreadyCleared"
    );
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "REL-1")
            .await
            .unwrap_err()
            .code,
        "SiteContainersUnhacked"
    );
    assert_eq!(
        advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "CMB-1")
            .await
            .unwrap_err()
            .code,
        "WaveNpcsRemaining"
    );

    // Stabilized SleeperCache bypasses live NPC check and completes immediately
    let sc_done = advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "SLC-1")
        .await
        .expect("stabilized SleeperCache must advance/complete despite sentry ball");
    assert_eq!(sc_done.dict_get("completed"), Some(&PyRep::Bool(true)));
    with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system[&sys_id]
            .iter()
            .find(|s| s.site_id == "SLC-1")
            .unwrap();
        assert!(s.cleared);
    });

    // Hacking Relic container unblocks completion
    with_pve_exploration_reg(&ctx, |reg| {
        reg.hacked_containers.insert((sys_id, 555_001));
    });
    let rel_done = advance_site_wave_or_complete(&ctx, &mut sess, sys_id, "REL-1")
        .await
        .expect("hacked Relic site must complete");
    assert_eq!(rel_done.dict_get("completed"), Some(&PyRep::Bool(true)));
}

#[test]
fn notify_pve_destruction_events_routes_fleet_and_caps_queue_at_256() {
    let ctx = edge_ctx();
    let killer = 90_000_001;
    let fleet_mate = 90_000_002;
    let fleet_id = 555_001;

    ctx.db.with_mut_snapshot(|snap| {
        let mut mate = snap.characters[&killer].clone();
        mate.character_id = fleet_mate;
        mate.fleet_id = Some(fleet_id);
        snap.characters.insert(fleet_mate, mate);
        snap.characters.get_mut(&killer).unwrap().fleet_id = Some(fleet_id);
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: killer,
                members: vec![killer, fleet_mate],
                wings: Vec::new(),
                squads: Vec::new(),
                free_move: true,
                is_registered: false,
                wing_details: HashMap::new(),
                squad_details: HashMap::new(),
                member_details: HashMap::new(),
                invites: HashSet::new(),
                motd: String::new(),
            },
        );
    });

    // Pre-fill killer's queue with 254 items so pushing 3+ events tests 256-cap truncation cleanly
    ctx.pending_notifications.write().unwrap().clear();
    for _ in 0..254 {
        ctx.notify(
            NotificationTarget::Character(killer),
            "OnSkillTrained",
            "charid",
            vec![],
        );
    }

    let esc = EscalatingPathEntry {
        instance_id: 7001,
        character_id: killer,
        source_dungeon_id: 300_000,
        expedition_dungeon_id: 300_001,
        expedition_name: "Escalation Path".into(),
        destination_solar_system_id: 30_000_144,
        step_index: 1,
        total_steps: 4,
        journal_text: "Follow the trail".into(),
        created_at_sec: 1_750_000_000,
        expires_at_sec: 1_750_086_400,
        dungeons_entered: Vec::new(),
        journal_entry_template_id: 0,
    };
    let mut esc_adv = esc.clone();
    esc_adv.instance_id = 7002;
    esc_adv.step_index = 2;

    // Register SITE-X in PvE exploration registry so cleared_did_opt resolves for OnDungeonCompleted
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .entry(30_000_142)
            .or_default()
            .push(sample_site(
                "SITE-X",
                300_000,
                30_000_142,
                "CombatAnomaly",
                None,
            ));
    });

    notify_pve_destruction_events(
        &ctx.db,
        &ctx.pending_notifications,
        &ctx.notify_tx,
        killer,
        Some("SITE-X".to_string()),
        Some(esc),
        Some(esc_adv),
    );

    let q = ctx.pending_notifications.read().unwrap();
    assert!(q[&killer].len() <= 256, "len={}", q[&killer].len());
    let killer_events: Vec<&str> = q[&killer].iter().map(|n| n.event.as_str()).collect();
    assert!(killer_events.contains(&"OnDungeonCompleted"));
    assert!(killer_events.contains(&"OnEscalatingPathChange"));
    assert!(killer_events.contains(&"OnEscalatingPathMessage"));

    let mate_events: Vec<&str> = q[&fleet_mate].iter().map(|n| n.event.as_str()).collect();
    assert!(mate_events.contains(&"OnFleetEscalatingPathMessage"));
}
