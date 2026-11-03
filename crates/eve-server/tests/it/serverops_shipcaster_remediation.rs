//! Regression tests for blocking findings B-01 through B-05:
//! - B-01: Atomic `contribute_fw_beacon_material_inner` (no TOCTOU panic/cargo-loss/uncapped-LP)
//! - B-02: Dilated `DoSimClockRebase` progression & `SystemHandle::step_manual` TiDi scaling
//! - B-03: FW beacon deployment security-status, warzone, and per-system uniqueness gates
//! - B-04: Scheduled 11:00 UTC downtime without pre-11:00 countdown arm & real ESS rollover
//! - B-05: Unified `OnShipcasterTargetStateChanged` `solarsystemid` scope & online-only delivery

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, SystemDynamicState};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::cluster_macho_misc_services::with_misc_state;
use eve_server::services::serverops_cluster::{
    execute_daily_maintenance_jobs, set_scheduled_downtime_enabled, set_system_tidi_factor,
};
use eve_server::services::shipcaster_ops::{
    contribute_fw_beacon_material, deploy_fw_beacon_constructor, link_fw_landing_pad,
    on_landing_pad_ball_destroyed,
};
use eve_server::services::space_destiny::{
    get_ess_reserve_pulses, set_ess_reserve_pulses, EssReservePulseState,
};
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

fn place_pilot(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, ship_id: i64, sys_id: u32) {
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.war_faction_id = Some(500_001);
        }
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = sys_id as i64;
        }
    });
}

async fn spawn_ship(ctx: &ServerContext, sys_id: u32, char_id: i64, ship_id: i64, pos: DVec3) {
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle.remove_ball(ship_id).await;
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 601,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 1000.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 601),
        )
        .await;
}

async fn setup_pilot_session() -> (ServerContext, SessionState, i64, i64) {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx.advance_secs(4_200);
    let mut sess = SessionState::new(30_199);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    let ship_id = sess.ship_id.unwrap_or(100_000_001);
    (ctx, sess, char_id, ship_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b01_concurrent_deposit_and_destroyed_pad_atomicity() {
    let (ctx, mut sess, char_id, ship_id) = setup_pilot_session().await;
    let fw_sys = 30002811_u32; // Onatoh (Caldari FW lowsec)
    let pos = DVec3::new(100_000.0, 0.0, 0.0);

    place_pilot(&ctx, &mut sess, char_id, ship_id, fw_sys);
    spawn_ship(&ctx, fw_sys, char_id, ship_id, pos + DVec3::new(500.0, 0.0, 0.0)).await;

    let comps = ctx.sde.shipcaster_sde_components().expect("shipcaster SDE");
    let spec = &comps.under_construction[&75933];
    let (&mat_tid, entry) = spec.input_items.iter().next().expect("input_items");
    let req_qty = entry.required_qty;

    let (ctor_id, stack_a, stack_b) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let ctor_id = snap.next_entity_id;
        snap.items.insert(
            ctor_id,
            InventoryItemRecord {
                item_id: ctor_id,
                type_id: 75933,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let sa = snap.next_entity_id;
        snap.items.insert(
            sa,
            InventoryItemRecord {
                item_id: sa,
                type_id: mat_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: (req_qty as i32) * 2,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let sb = snap.next_entity_id;
        snap.items.insert(
            sb,
            InventoryItemRecord {
                item_id: sb,
                type_id: mat_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: (req_qty as i32) * 2,
                ..Default::default()
            },
        );
        (ctor_id, sa, sb)
    });

    let pad = deploy_fw_beacon_constructor(&ctx, &sess, ctor_id, pos)
        .await
        .expect("deploy beacon in FW lowsec");

    // Concurrent deposits must never over-consume cargo or mint uncapped LP
    let (res_a, res_b) = tokio::join!(
        contribute_fw_beacon_material(&ctx, &sess, pad.item_id, stack_a, req_qty),
        contribute_fw_beacon_material(&ctx, &sess, pad.item_id, stack_b, req_qty),
    );
    assert!(res_a.is_ok() || res_b.is_ok(), "at least one concurrent deposit must succeed");

    let (total_remaining, lp_after_concurrent) = ctx.db.with_snapshot(|snap| {
        let qa = snap.items.get(&stack_a).map(|i| i.quantity).unwrap_or(0);
        let qb = snap.items.get(&stack_b).map(|i| i.quantity).unwrap_or(0);
        let lp = snap
            .characters
            .get(&char_id)
            .and_then(|c| entry.lp_corp_id.and_then(|corp| c.loyalty_points.get(&corp).copied()))
            .unwrap_or(0);
        (qa + qb, lp)
    });
    let total_consumed = (req_qty as i32) * 4 - total_remaining;
    assert!(
        total_consumed <= req_qty as i32,
        "concurrent deposits must never consume more than req_qty ({total_consumed} > {req_qty})"
    );
    let max_lp = i64::from(entry.lp_per_unit) * i64::from(req_qty);
    assert!(
        lp_after_concurrent <= max_lp,
        "concurrent deposits must never mint uncapped LP ({lp_after_concurrent} > {max_lp})"
    );

    // Destroying a pad before deposit must fail cleanly without cargo loss or panic
    let ctor_id2 = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 75933,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
        id
    });
    let fw_sys2 = 30002813_u32; // Tama
    place_pilot(&ctx, &mut sess, char_id, ship_id, fw_sys2);
    spawn_ship(&ctx, fw_sys2, char_id, ship_id, pos + DVec3::new(500.0, 0.0, 0.0)).await;
    let pad2 = deploy_fw_beacon_constructor(&ctx, &sess, ctor_id2, pos)
        .await
        .expect("deploy second beacon");
    on_landing_pad_ball_destroyed(&ctx, pad2.item_id).await;

    let before_qty = ctx.db.with_snapshot(|s| s.items.get(&stack_a).map(|i| i.quantity).unwrap_or(0));
    let err = contribute_fw_beacon_material(&ctx, &sess, pad2.item_id, stack_a, 1)
        .await
        .expect_err("deposit into destroyed pad must fail");
    assert_eq!(err.code, "LandingPadNotFound");
    let after_qty = ctx.db.with_snapshot(|s| s.items.get(&stack_a).map(|i| i.quantity).unwrap_or(0));
    assert_eq!(before_qty, after_qty, "failed deposit on destroyed pad must not delete cargo");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b02_sim_clock_rebase_dilated_progression_and_step_manual_tidi() {
    let (ctx, mut sess, char_id, ship_id) = setup_pilot_session().await;
    let jita = 30000142_u32;
    place_pilot(&ctx, &mut sess, char_id, ship_id, jita);
    let _ = ctx.drain_notifications_for_char(char_id);

    ctx.db.clock().set_paused(true);
    let _ = set_system_tidi_factor(&ctx, jita, 0.50);
    let _ = ctx.drain_notifications_for_char(char_id);

    // Advance 10 wall seconds under 0.50 TiDi -> sim clock must advance 5.0s (50_000_000 ticks), NOT 10.0s
    ctx.db.clock().advance_secs(10);
    let _ = set_system_tidi_factor(&ctx, jita, 0.25);
    let notes1 = ctx.drain_notifications_for_char(char_id);
    let rebase1 = notes1.iter().find(|n| n.event == "DoSimClockRebase").expect("DoSimClockRebase #1");
    let PyRep::Tuple(pair1) = &rebase1.args[0] else { panic!("expected 2-tuple"); };
    let old1 = pair1[0].as_i64().unwrap();
    let new1 = pair1[1].as_i64().unwrap();
    assert_eq!(new1 - old1, 50_000_000, "0.50 TiDi over 10s must advance sim clock by 5.0s");
    assert!(new1 < ctx.now_blueos_filetime_ticks(), "sim clock must stay dilated behind wall clock");

    // Advance 20 wall seconds under 0.25 TiDi -> sim clock must advance 5.0s (50_000_000 ticks)
    ctx.db.clock().advance_secs(20);
    let _ = set_system_tidi_factor(&ctx, jita, 1.0);
    let notes2 = ctx.drain_notifications_for_char(char_id);
    let rebase2 = notes2.iter().find(|n| n.event == "DoSimClockRebase").expect("DoSimClockRebase #2");
    let PyRep::Tuple(pair2) = &rebase2.args[0] else { panic!("expected 2-tuple"); };
    let old2 = pair2[0].as_i64().unwrap();
    let new2 = pair2[1].as_i64().unwrap();
    assert_eq!(old2, new1);
    assert_eq!(new2 - old2, 50_000_000, "0.25 TiDi over 20s must advance sim clock by 5.0s");

    // Verify step_manual scales dt by tidi_factor for system_step_hook
    let observed_dts: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let obs_clone = Arc::clone(&observed_dts);
    ctx.systems.set_system_step_hook(Arc::new(move |_sys_id, dt, _destroyed| {
        let obs = Arc::clone(&obs_clone);
        Box::pin(async move {
            obs.lock().unwrap().push(dt);
        })
    }));
    let _ = set_system_tidi_factor(&ctx, jita, 0.25);
    let jita_handle = ctx.systems.get_or_spawn(jita);
    let _ = jita_handle.step_manual(4.0).await.expect("step_manual");
    let dts = observed_dts.lock().unwrap().clone();
    assert_eq!(dts.len(), 1);
    assert!((dts[0] - 1.0).abs() < 1e-6, "step_manual(4.0) at 0.25 TiDi must pass eff_dt=1.0, got {}", dts[0]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b03_beacon_deployment_rejects_nullsec_non_fw_lowsec_and_duplicates() {
    let (ctx, mut sess, char_id, ship_id) = setup_pilot_session().await;
    let pos = DVec3::new(100_000.0, 0.0, 0.0);

    let ctor_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 75933,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                ..Default::default()
            },
        );
        id
    });

    // 1. Nullsec system (1DQ1-A: 30004759, security_status <= 0.0) must be rejected
    let nullsec_sys = 30004759_u32;
    place_pilot(&ctx, &mut sess, char_id, ship_id, nullsec_sys);
    let err_null = deploy_fw_beacon_constructor(&ctx, &sess, ctor_id, pos)
        .await
        .expect_err("nullsec deployment must fail");
    assert_eq!(err_null.code, "InvalidBeaconDeploymentSystem");

    // 2. Non-FW lowsec system (remove a lowsec system from fw_system_occupations to simulate non-FW lowsec)
    let non_fw_lowsec = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            let r = (s.security_status * 10.0).round() / 10.0;
            s.solar_system_id < 31_000_000
                && s.solar_system_id != 30_100_000
                && r > 0.0
                && r < 0.45
                && !ctx.db.with_snapshot(|snap| snap.fw_system_occupations.contains_key(&s.solar_system_id))
        })
        .map(|s| s.solar_system_id)
        .expect("at least one non-FW lowsec system in SDE");
    place_pilot(&ctx, &mut sess, char_id, ship_id, non_fw_lowsec);
    let err_non_fw = deploy_fw_beacon_constructor(&ctx, &sess, ctor_id, pos)
        .await
        .expect_err("non-FW lowsec deployment must fail");
    assert_eq!(err_non_fw.code, "InvalidBeaconDeploymentSystem");

    // 3. Valid FW lowsec system (Onatoh 30002811) succeeds once, then rejects duplicate
    let fw_sys = 30002811_u32;
    place_pilot(&ctx, &mut sess, char_id, ship_id, fw_sys);
    let _first = deploy_fw_beacon_constructor(&ctx, &sess, ctor_id, pos)
        .await
        .expect("first FW lowsec deployment must succeed");
    let err_dup = deploy_fw_beacon_constructor(&ctx, &sess, ctor_id, pos + DVec3::new(50_000.0, 0.0, 0.0))
        .await
        .expect_err("duplicate same-system beacon deployment must fail");
    assert_eq!(err_dup.code, "BeaconAlreadyExistsInSystem");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b04_scheduled_downtime_without_pre_arm_and_ess_reserve_reset() {
    let (ctx, _sess, _char_id, _ship_id) = setup_pilot_session().await;
    ctx.db.clock().set_paused(true);

    // Seed active ESS main-bank link AND ESS reserve-bank pulse state
    let ess_sys = 30004759_u32;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            ess_sys,
            SystemDynamicState {
                solar_system_id: ess_sys,
                dbs_multiplier: 1.0,
                ess_main_bank: IskCents::from_isk(500_000),
                ess_reserve_bank: IskCents::from_isk(250_000),
                ess_linked_character_id: Some(90_000_001),
                ess_link_started_tick: Some(10),
                ess_cycle_started_unix: 1_700_000_000,
                ess_main_bank_contributors: HashMap::new(),
                ..Default::default()
            },
        );
        set_ess_reserve_pulses(
            snap,
            ess_sys,
            &EssReservePulseState {
                pulses_total: 10,
                pulses_remaining: 7,
                last_pulse_sec: Some(1_700_000_100),
                reserve_linked_character_id: Some(90_000_001),
                reserve_link_started_tick: Some(12),
                link_started_unix: Some(1_700_000_050),
                unlocked_until_unix: 1_700_002_000,
            },
        );
    });

    // Opt-out flag prevents automatic shutdown when disabled
    set_scheduled_downtime_enabled(&ctx, false);
    let now = ctx.now_unix();
    let day_start = now - (now % 86_400);
    let in_dt_window = day_start + 86_400 + 11 * 3_600 + 120; // Next day 11:02 UTC (no pre-arm tick!)
    ctx.db.clock().advance_secs(in_dt_window.saturating_sub(now));
    tick_background_cluster(&ctx).await;
    assert!(!ctx.is_shutting_down(), "opt-out flag must skip automatic downtime shutdown");

    // Re-enable scheduled downtime and advance to the following day 11:03 UTC without any 10:xx countdown tick
    set_scheduled_downtime_enabled(&ctx, true);
    let next_dt_window = in_dt_window + 86_400 + 60;
    ctx.db.clock().advance_secs(next_dt_window - in_dt_window);
    tick_background_cluster(&ctx).await;
    assert!(
        ctx.is_shutting_down(),
        "entering 11:00..11:15 UTC must trigger scheduled downtime even without a prior 10:xx countdown tick"
    );

    // Verify ESS main-bank link & reserve-bank pulse state were reset while balances remain intact
    ctx.db.with_snapshot(|snap| {
        let sov = &snap.system_states[&ess_sys];
        assert_eq!(sov.ess_main_bank, IskCents::from_isk(500_000));
        assert_eq!(sov.ess_reserve_bank, IskCents::from_isk(250_000));
        assert_eq!(sov.ess_linked_character_id, None);
        assert_eq!(sov.ess_link_started_tick, None);
        assert_eq!(sov.ess_cycle_started_unix, 0);
        let pulses = get_ess_reserve_pulses(snap, ess_sys);
        assert_eq!(pulses.pulses_remaining, 0);
        assert_eq!(pulses.reserve_linked_character_id, None);
    });

    // Also verify execute_daily_maintenance_jobs reports essBanksRolledOver >= 1 when ESS state is active
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sov) = snap.system_states.get_mut(&ess_sys) {
            sov.ess_linked_character_id = Some(90_000_001);
        }
    });
    let report = execute_daily_maintenance_jobs(&ctx);
    assert!(report.get("essBanksRolledOver").copied().unwrap_or(0) >= 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn b05_shipcaster_target_state_changed_unified_scope_and_online_only() {
    let (ctx, mut sess, online_char, ship_id) = setup_pilot_session().await;
    let fw_sys = 30002811_u32;
    let pos = DVec3::new(100_000.0, 0.0, 0.0);

    place_pilot(&ctx, &mut sess, online_char, ship_id, fw_sys);
    spawn_ship(&ctx, fw_sys, online_char, ship_id, pos + DVec3::new(500.0, 0.0, 0.0)).await;

    // Seed an offline character in the same solar system (no online session)
    let offline_char = 90_000_777_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut off_ch = snap.characters[&online_char].clone();
        off_ch.character_id = offline_char;
        off_ch.user_id = 999_777;
        off_ch.solar_system_id = fw_sys;
        snap.characters.insert(offline_char, off_ch);
    });

    // Seed a built pad in fw_sys and link it -> emits OnShipcasterTargetStateChanged with id_type == "solarsystemid"
    let pad_id = 109_888_001_i64;
    with_misc_state(&ctx, |st| {
        st.landing_pads.push(eve_server::services::cluster_macho_misc_services::LandingPadRecord {
            item_id: pad_id,
            type_id: 75929,
            built_type_id: 75929,
            solar_system_id: fw_sys,
            faction_id: 500_001,
            is_built: true,
            link_timestamp: 0,
            is_disrupted: false,
            position: pos,
            required_materials: Default::default(),
            contributed_materials: Default::default(),
        });
    });
    let _ = ctx.drain_notifications_for_char(online_char);
    let _ = ctx.drain_notifications_for_char(offline_char);

    let _ = link_fw_landing_pad(&ctx, pad_id).expect("link pad");
    let link_notes = ctx.drain_notifications_for_char(online_char);
    let link_ev = link_notes
        .iter()
        .find(|n| n.event == "OnShipcasterTargetStateChanged")
        .expect("link OnShipcasterTargetStateChanged");
    assert_eq!(link_ev.id_type, "solarsystemid");
    assert!(ctx.drain_notifications_for_char(offline_char).is_empty(), "offline char must not receive notification");

    // Destroy pad -> emits OnShipcasterTargetStateChanged with identical id_type == "solarsystemid" and online-only
    on_landing_pad_ball_destroyed(&ctx, pad_id).await;
    let destroy_notes = ctx.drain_notifications_for_char(online_char);
    let destroy_ev = destroy_notes
        .iter()
        .find(|n| n.event == "OnShipcasterTargetStateChanged")
        .expect("destroy OnShipcasterTargetStateChanged");
    assert_eq!(destroy_ev.id_type, "solarsystemid");
    assert!(
        ctx.drain_notifications_for_char(offline_char).is_empty(),
        "offline char must not receive pad destruction notification"
    );
}
