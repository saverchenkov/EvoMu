//! Exploit and security regression tests for Round 6 Track 1 P1 issues (#366-#369).
#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_missiles_aoe::step_missiles_and_breachers;
use crate::services::combat_crimewatch::{with_mut_combat_runtime, ActiveBreacherDot, SafetyLevel};
use crate::services::space_destiny::space_cyno_filaments;
use crate::services::tactical_modules::apply_projected_remote_assistance;
use crate::system_actor::ActiveModuleCycle;
use eve_db::{InventoryFlag, InventoryItemRecord, ServerClock, VirtualServerClock, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_dogma::DogmaItemState;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        for p in [
            SdeRegistry::default_db_path(),
            std::path::PathBuf::from("../../data/evomu_sde.sqlite"),
            std::path::PathBuf::from("/Users/sergeya/ai/EvoMu/data/evomu_sde.sqlite"),
        ] {
            if let Ok(sde) = SdeRegistry::open(&p) {
                return Arc::new(sde);
            }
        }
        Arc::new(SdeRegistry::default())
    }))
}

fn test_ctx_at(base_unix: u64) -> (Arc<ServerContext>, Arc<VirtualServerClock>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(shared_sde(), db));
    let clock = Arc::new(VirtualServerClock::with_fixed_base(base_unix));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);
    (ctx, clock)
}

fn make_session(char_id: i64, corp_id: i64, sys_id: u32, ship_id: Option<i64>) -> SessionState {
    let mut sess = SessionState::new(char_id as u64);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = ship_id;
    sess
}

fn make_item(id: i64, owner: i64, loc: i64, flag: InventoryFlag, type_id: u32, qty: i32) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id: id,
        owner_id: owner,
        location_id: loc,
        flag,
        type_id,
        quantity: qty,
        singleton: false,
        ..InventoryItemRecord::default()
    }
}

fn make_ship_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

fn make_char(
    character_id: i64,
    corporation_id: i64,
    active_ship_id: i64,
    solar_system_id: u32,
) -> eve_db::CharacterRecord {
    eve_db::CharacterRecord {
        activity_status: None,
        character_id,
        user_id: 1,
        name: format!("Pilot {character_id}"),
        corporation_id,
        alliance_id: None,
        war_faction_id: None,
        station_id: None,
        structure_id: None,
        solar_system_id,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        active_ship_id,
        balance: eve_db::IskCents(0),
        plex_balance: 0,
        loyalty_points: HashMap::new(),
        standings: HashMap::new(),
        security_status: 0.0,
        clone_grade: 2,
        home_station_id: 60_003_760,
        race_id: 1,
        bloodline_id: 1,
        ancestry_id: 1,
        school_id: 11,
        neural: eve_db::NeuralAttributes::default(),
        skill_points: 0.0,
        free_skill_points: 0.0,
        last_clone_jump_unix: 0,
        jump_fatigue_minutes: 0.0,
        jump_cooldown_until_unix: 0,
        jump_fatigue_until_unix: 0,
        description: String::new(),
        title: String::new(),
        gender: 1,
        create_date_unix: 0,
        fleet_id: None,
        autopilot_waypoints: Vec::new(),
        version: 1,
        system_epoch: 1,
    }
}

#[tokio::test]
async fn issue_366_breacher_dot_respects_tether_and_records_killmail_assist_and_notifies() {
    let (ctx, _) = test_ctx_at(1_700_004_000);
    let attacker_char = 91_004_001;
    let victim_char = 91_004_002;
    let attacker_ship = 1_100_004_001;
    let victim_ship = 1_100_004_002;
    let sys_id = 30_000_842;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(attacker_char, make_char(attacker_char, 98_001, attacker_ship, sys_id));
        snap.characters.insert(victim_char, make_char(victim_char, 98_002, victim_ship, sys_id));
        snap.items.insert(
            attacker_ship,
            make_item(attacker_ship, attacker_char, sys_id as i64, InventoryFlag::None, 81_272, 1),
        );
        snap.items.insert(
            victim_ship,
            make_item(victim_ship, victim_char, sys_id as i64, InventoryFlag::None, 587, 1),
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.spawn_ball(make_ship_ball(victim_ship, 587, DVec3::ZERO)).await;
    let mut cs = CombatState::default();
    cs.is_tethered = true;
    cs.shield_hp = 1_000.0;
    cs.max_shield_hp = 1_000.0;
    let _ = sys.set_combat_state(victim_ship, cs).await;

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_breacher_dots.insert(
            victim_ship,
            ActiveBreacherDot {
                target_ball_id: victim_ship,
                source_char_id: attacker_char,
                source_ship_id: attacker_ship,
                weapon_type_id: 81_280,
                charge_type_id: 81_282,
                flat_cap_dps: 100.0,
                pct_cap_fraction: 1.0,
                remaining_s: 10.0,
                tick_damage: 100.0,
            },
        );
    });

    step_missiles_and_breachers(&ctx, sys_id, 1.0).await;
    let after_tether = sys.get_combat_state(victim_ship).await.unwrap().unwrap();
    assert!((after_tether.shield_hp - 1_000.0).abs() < 1e-6);

    let mut untethered = after_tether;
    untethered.is_tethered = false;
    let _ = sys.set_combat_state(victim_ship, untethered).await;

    let _ = ctx.db.acquire_online_lock(1, 301, Some(attacker_char), false, ctx.now_unix());
    let mut notify_rx = ctx.notify_tx.subscribe();
    step_missiles_and_breachers(&ctx, sys_id, 1.0).await;
    let after_tick = sys.get_combat_state(victim_ship).await.unwrap().unwrap();
    assert!(after_tick.shield_hp < 1_000.0);

    let assist_entries = ctx
        .combat_ephemeral
        .read()
        .unwrap()
        .aggression_assist_log
        .get(&victim_ship)
        .cloned()
        .unwrap_or_default();
    assert!(assist_entries.iter().any(|e| e.attacker_char_id == attacker_char && e.damage_done > 0.0));

    let (recipient_id, notif) = notify_rx.try_recv().expect("Breacher DoT tick must emit damage notification");
    assert_eq!(recipient_id, attacker_char);
    assert_eq!(notif.event, "OnDamageMessage");
}

#[tokio::test]
async fn issue_367_remote_assistance_enforces_safety_and_inherits_criminal_flag() {
    let (ctx, _) = test_ctx_at(1_700_005_000);
    let now_unix = ctx.now_unix();
    let logi_char = 91_005_001;
    let crim_char = 91_005_002;
    let logi_ship = 1_100_005_001;
    let crim_ship = 1_100_005_002;
    let highsec_sys = 30_000_142;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(logi_char, make_char(logi_char, 98_001, logi_ship, highsec_sys));
        snap.characters.insert(crim_char, make_char(crim_char, 98_002, crim_ship, highsec_sys));
        snap.items.insert(
            logi_ship,
            make_item(logi_ship, logi_char, highsec_sys as i64, InventoryFlag::None, 11_987, 1),
        );
        snap.items.insert(
            crim_ship,
            make_item(crim_ship, crim_char, highsec_sys as i64, InventoryFlag::None, 587, 1),
        );
    });

    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(crim_char).criminal_flag_until_unix = now_unix + 900;
    }

    let mod_id = 1_100_005_099;
    let mod_type = ctx.sde.types.get(&1964).expect("Remote Sensor Booster I in SDE").clone();
    let mod_state = DogmaItemState::from_sde(mod_id, 1964, &ctx.sde);

    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(logi_char, SafetyLevel::Green);
    });
    let blocked = apply_projected_remote_assistance(
        &ctx, logi_char, logi_ship, mod_id, &mod_type, &mod_state, Some(crim_ship), 1_000.0,
    );
    assert!(blocked.is_err(), "Green safety in highsec must block assisting a Criminal");

    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(logi_char, SafetyLevel::Red);
    });
    let allowed = apply_projected_remote_assistance(
        &ctx, logi_char, logi_ship, mod_id, &mod_type, &mod_state, Some(crim_ship), 1_000.0,
    );
    assert!(allowed.is_ok(), "Red safety in highsec should permit remote assist while flagging: {allowed:?}");

    let cw = ctx.crimewatch.read().unwrap();
    let logi_state = cw.states.get(&logi_char).expect("logi crimewatch state");
    assert!(logi_state.is_criminal(now_unix));
}

#[tokio::test]
async fn issue_368_wormhole_jump_preserves_combat_state_and_spawns_distinct_exit_wh() {
    let (ctx, _) = test_ctx_at(1_700_006_000);
    let char_id = 91_006_001;
    let corp_id = 98_006_001;
    let ship_id = 1_100_006_001;
    let old_sys = 30_000_142;
    let dest_sys = 31_000_005;
    let entry_wh_id = 850_006_001;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(char_id, make_char(char_id, corp_id, ship_id, old_sys));
        snap.items.insert(
            ship_id,
            make_item(ship_id, char_id, old_sys as i64, InventoryFlag::None, 587, 1),
        );
        snap.system_states.entry(old_sys).or_default().wormhole_destination_system_id = Some(dest_sys);
        snap.system_states.entry(old_sys).or_default().wormhole_remaining_mass_kg = 100_000_000.0;
        snap.system_states.entry(dest_sys).or_default().wormhole_remaining_mass_kg = 100_000_000.0;
    });

    let old_handle = ctx.systems.get_or_spawn(old_sys);
    let _ = old_handle.spawn_ball(make_ship_ball(ship_id, 587, DVec3::new(500.0, 0.0, 0.0))).await;
    let _ = old_handle
        .spawn_ball(Ball {
            id: entry_wh_id,
            type_id: 26272,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000_000.0,
            radius: 1500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;

    let mut damaged_cs = CombatState::default();
    damaged_cs.shield_hp = 250.0;
    damaged_cs.max_shield_hp = 1_000.0;
    damaged_cs.capacitor = 75.0;
    damaged_cs.max_capacitor = 300.0;
    damaged_cs.active_breacher_dot = Some((50.0, 0.0, 12));
    let _ = old_handle.set_combat_state(ship_id, damaged_cs).await;

    let mut sess = make_session(char_id, corp_id, old_sys, Some(ship_id));
    let kwargs = HashMap::new();
    let res = space_cyno_filaments::handle(
        &ctx,
        &mut sess,
        None,
        "wormholeMgr",
        "WormholeJump",
        &[PyRep::Integer(entry_wh_id)],
        &kwargs,
    )
    .await
    .expect("handled")
    .expect("wormhole jump must succeed");

    let exit_wh_id = match &res {
        PyRep::KeyVal(entries) => entries
            .get("exitWormholeBallID")
            .and_then(|v| v.as_i64())
            .expect("exitWormholeBallID present"),
        other => panic!("expected PyRep::KeyVal response, got {other:?}"),
    };
    assert_ne!(exit_wh_id, entry_wh_id);

    let dest_handle = ctx.systems.get_or_spawn(dest_sys);
    assert!(dest_handle.get_ball_snapshot(exit_wh_id).await.unwrap().is_some());

    let dest_cs = dest_handle.get_combat_state(ship_id).await.unwrap().expect("destination combat state");
    assert!(((dest_cs.shield_hp / dest_cs.max_shield_hp) - 0.25).abs() < 1e-4);
    assert!(((dest_cs.capacitor / dest_cs.max_capacitor) - 0.25).abs() < 1e-4);
    assert_eq!(dest_cs.active_breacher_dot, Some((50.0, 0.0, 12)));
}

#[tokio::test]
async fn issue_369_sleeper_and_drifter_npcs_are_capacitor_immune_to_neut_and_nos() {
    let (ctx, _) = test_ctx_at(1_700_007_000);
    let sys_id = 31_000_005;
    let attacker_ship = 1_100_007_001;
    let sleeper_npc = 1_100_007_002;
    let drifter_npc = 1_100_007_003;

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.spawn_ball(make_ship_ball(attacker_ship, 622, DVec3::ZERO)).await;
    let _ = sys.spawn_ball(make_ship_ball(sleeper_npc, 30_190, DVec3::new(1_000.0, 0.0, 0.0))).await;
    let _ = sys.spawn_ball(make_ship_ball(drifter_npc, 37_472, DVec3::new(1_200.0, 0.0, 0.0))).await;

    let mut atk_cs = CombatState::default();
    atk_cs.capacitor = 100.0;
    atk_cs.max_capacitor = 1_000.0;
    let _ = sys.set_combat_state(attacker_ship, atk_cs).await;

    let mut sleeper_cs = CombatState::default();
    sleeper_cs.capacitor = 2_500.0;
    sleeper_cs.max_capacitor = 2_500.0;
    let _ = sys.set_combat_state(sleeper_npc, sleeper_cs).await;

    let mut drifter_cs = CombatState::default();
    drifter_cs.capacitor = 5_000.0;
    drifter_cs.max_capacitor = 5_000.0;
    let _ = sys.set_combat_state(drifter_npc, drifter_cs).await;

    sys.add_module_cycle(ActiveModuleCycle {
        ship_id: attacker_ship,
        module_id: 99_001,
        target_id: Some(drifter_npc),
        cap_drain_target: 300.0,
        duration_s: 1.0,
        repeat: 1,
        ..ActiveModuleCycle::default()
    })
    .await
    .expect("neut cycle added");

    sys.add_module_cycle(ActiveModuleCycle {
        ship_id: attacker_ship,
        module_id: 99_002,
        target_id: Some(sleeper_npc),
        cap_transfer_target: 150.0,
        duration_s: 1.0,
        repeat: 1,
        ..ActiveModuleCycle::default()
    })
    .await
    .expect("nos cycle added");

    let _ = sys.step_manual(1.2).await;

    let sleeper_after = sys.get_combat_state(sleeper_npc).await.unwrap().unwrap();
    let drifter_after = sys.get_combat_state(drifter_npc).await.unwrap().unwrap();
    assert!((sleeper_after.capacitor - 2_500.0).abs() < 1e-6);
    assert!((drifter_after.capacitor - 5_000.0).abs() < 1e-6);
}

#[tokio::test]
async fn arch_585_breacher_dot_respects_invuln_guards_and_preserves_destruction_events() {
    let (ctx, _) = test_ctx_at(1_700_008_000);
    let sys_id = 30_000_142;
    let attacker_char = 91_008_001;
    let victim_char = 91_008_002;
    let attacker_ship = 1_100_008_001;
    let victim_ship = 1_100_008_002;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters
            .insert(attacker_char, make_char(attacker_char, 98_001, attacker_ship, sys_id));
        snap.characters
            .insert(victim_char, make_char(victim_char, 98_002, victim_ship, sys_id));
        snap.items.insert(
            attacker_ship,
            make_item(attacker_ship, attacker_char, sys_id as i64, InventoryFlag::None, 587, 1),
        );
        snap.items.insert(
            victim_ship,
            make_item(victim_ship, victim_char, sys_id as i64, InventoryFlag::None, 587, 1),
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.spawn_ball(make_ship_ball(attacker_ship, 587, DVec3::ZERO)).await;
    let _ = sys
        .spawn_ball(make_ship_ball(victim_ship, 587, DVec3::new(500.0, 0.0, 0.0)))
        .await;

    let mut vcs = CombatState::default();
    vcs.shield_hp = 100.0;
    vcs.max_shield_hp = 100.0;
    vcs.armor_hp = 100.0;
    vcs.max_armor_hp = 100.0;
    vcs.structure_hp = 1.0;
    vcs.max_structure_hp = 100.0;
    vcs.is_tethered = true;
    let _ = sys.set_combat_state(victim_ship, vcs.clone()).await;

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_breacher_dots.insert(
            victim_ship,
            ActiveBreacherDot {
                target_ball_id: victim_ship,
                source_char_id: attacker_char,
                source_ship_id: attacker_ship,
                weapon_type_id: 81_280,
                charge_type_id: 81_282,
                flat_cap_dps: 1_000.0,
                pct_cap_fraction: 1.0,
                remaining_s: 3.0,
                tick_damage: 1_000.0,
            },
        );
    });

    // Tethered victim must take zero true damage from Breacher DoT.
    step_missiles_and_breachers(&ctx, sys_id, 1.0).await;
    let guarded = sys.get_combat_state(victim_ship).await.unwrap().unwrap();
    assert!((guarded.shield_hp - 100.0).abs() < 1e-6);
    assert!((guarded.structure_hp - 1.0).abs() < 1e-6);

    // Clear tether and apply lethal tick: victim should be destroyed while preserving TerminalExplosion.
    vcs.is_tethered = false;
    vcs.shield_hp = 0.0;
    vcs.armor_hp = 0.0;
    vcs.structure_hp = 1.0;
    let _ = sys.set_combat_state(victim_ship, vcs).await;
    step_missiles_and_breachers(&ctx, sys_id, 1.0).await;

    assert!(sys.get_combat_state(victim_ship).await.unwrap().is_none());
    let (_, events, _) = sys.drain_observer_updates(victim_ship).await.unwrap();
    assert!(events
        .iter()
        .any(|ev| matches!(ev, &eve_destiny::DestinyEvent::TerminalExplosion { ball_id, .. } if ball_id == victim_ship)));
    assert!(events
        .iter()
        .any(|ev| matches!(ev, &eve_destiny::DestinyEvent::OnDamageStateChange { ball_id, .. } if ball_id == victim_ship)));
}
