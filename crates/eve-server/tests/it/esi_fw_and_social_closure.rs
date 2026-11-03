//! Closure tests for Crimewatch Kill Rights, Security Status, and Fleet Boss Handover /
//! Stored Formations & Warp Shapes / Target Tagging (`feat:crimewatch.*`, `feat:fleet.*`).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::crimewatch::{
    compute_sec_status_loss, AggressionEvaluation, CrimeWatchDirector,
};
use eve_server::combat::npc_ai::{NpcAiState, NpcCombatEntity, NpcSpawnManager};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_combat_runtime;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, String> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
        .map_err(|e| format!("{e:?}"))
}

fn field(rep: &PyRep, key: &str) -> PyRep {
    rep.dict_get(key).cloned().unwrap_or(PyRep::None)
}

fn spawn_ship(
    ctx: &ServerContext,
    owner_id: i64,
    sys_id: u32,
    type_id: u32,
    _pos: DVec3,
) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let sid = snap.next_entity_id;
        snap.items.insert(
            sid,
            InventoryItemRecord {
                item_id: sid,
                type_id,
                owner_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if type_id != 670 {
            snap.next_entity_id += 1;
            let gun_id = snap.next_entity_id;
            snap.items.insert(
                gun_id,
                InventoryItemRecord {
                    item_id: gun_id,
                    type_id: 487, // 250mm Light Artillery Cannon I
                    owner_id,
                    location_id: sid,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(gun_id, true);
            snap.next_entity_id += 1;
            let ammo_id = snap.next_entity_id;
            snap.items.insert(
                ammo_id,
                InventoryItemRecord {
                    item_id: ammo_id,
                    type_id: 183, // Fusion S
                    owner_id,
                    location_id: sid,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 100,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.active_ship_id = sid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
        }
        sid
    })
}

#[tokio::test]
async fn test_crimewatch_kill_rights_gain_and_sell_closure() {
    // parity: feat:crimewatch.kill-rights-gain, feat:crimewatch.kill-rights-sell
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();

    let highsec_sys = 30_000_142_u32; // Jita (0.95)
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status > 0.0 && s.security_status < 0.45)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_510);
    let victim_id = 90_000_001_i64;
    let criminal_id = 90_000_002_i64;
    let buyer_id = 90_000_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&buyer_id) {
            if let Some(mut c3) = snap.characters.get(&victim_id).cloned() {
                c3.character_id = buyer_id;
                c3.name = "Buyer Pilot".to_string();
                snap.characters.insert(buyer_id, c3);
            }
        }
        for (cid, uid, bal) in [
            (victim_id, 1, 10_000_000.0),
            (criminal_id, 2, 10_000_000.0),
            (buyer_id, 3, 20_000_000.0),
        ] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.user_id = uid;
                ch.balance = IskCents::from_f64_isk(bal);
                ch.solar_system_id = highsec_sys;
                ch.station_id = None;
            }
        }
    });

    let victim_ship = spawn_ship(&ctx, victim_id, highsec_sys, 587, DVec3::ZERO);
    let crim_ship = spawn_ship(&ctx, criminal_id, highsec_sys, 587, DVec3::new(1_000.0, 0.0, 0.0));
    let _buyer_ship = spawn_ship(&ctx, buyer_id, highsec_sys, 587, DVec3::new(2_000.0, 0.0, 0.0));

    let sys_handle = ctx.systems.get_or_spawn(highsec_sys);
    for (id, pos) in [(victim_ship, DVec3::ZERO), (crim_ship, DVec3::new(1_000.0, 0.0, 0.0))] {
        let mut cs = ctx.build_type_combat_state(587);
        cs.shield_hp = 500.0;
        let _ = sys_handle.spawn_ball_with_combat(Ball {
            id, type_id: 587, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
            inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }, cs).await;
    }

    // 1. Low-sec unprovoked ship aggression grants Suspect flag, NOT a kill right.
    let mut cw = CrimeWatchDirector::new();
    let low_eval = cw.handle_combat_aggression_with_pod(
        criminal_id,
        victim_id,
        crim_ship,
        lowsec_sys,
        0.4,
        false,
        false,
        now,
    );
    assert!(matches!(low_eval, AggressionEvaluation::SuspectFlagTriggered { .. }));
    assert!(ctx.db.with_snapshot(|s| s.kill_rights.is_empty()));

    // 1b. Low-sec criminal aggression against a capsule (pod) grants a 30-day private kill right.
    let low_pod_eval = cw.handle_combat_aggression_with_pod(
        criminal_id,
        victim_id,
        crim_ship,
        lowsec_sys,
        0.4,
        false,
        true,
        now,
    );
    assert!(matches!(low_pod_eval, AggressionEvaluation::CriminalFlagTriggered { .. }));
    let lowsec_handle = ctx.systems.get_or_spawn(lowsec_sys);
    let low_crim_ship = spawn_ship(&ctx, criminal_id, lowsec_sys, 587, DVec3::new(1_000.0, 0.0, 0.0));
    let low_vic_pod = spawn_ship(&ctx, victim_id, lowsec_sys, 670, DVec3::ZERO);
    for (id, tid, pos) in [(low_crim_ship, 587_u32, DVec3::new(1_000.0, 0.0, 0.0)), (low_vic_pod, 670_u32, DVec3::ZERO)] {
        let mut cs = ctx.build_type_combat_state(tid);
        cs.shield_hp = 500.0;
        let _ = lowsec_handle.spawn_ball_with_combat(Ball {
            id, type_id: tid, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
            inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }, cs).await;
    }
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&victim_id) {
            ch.active_ship_id = low_vic_pod;
        }
    });
    eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, criminal_id, low_crim_ship, lowsec_sys, low_vic_pod, true,
    ).expect("evaluate_and_record_pvp_aggression lowsec pod");
    assert!(
        ctx.db.with_snapshot(|s| s.kill_rights.values().any(|kr| kr.owner_id == victim_id && kr.target_id == criminal_id)),
        "evaluate_and_record_pvp_aggression on low-sec pod must grant kill right"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.kill_rights.clear();
        s.crimewatch_aggressions.clear();
    });
    let sec_before_cmd = ctx.db.with_snapshot(|s| s.characters[&criminal_id].security_status);
    let mut sess_crim_low = SessionState::default();
    sess_crim_low.user_id = Some(2);
    sess_crim_low.character_id = Some(criminal_id);
    sess_crim_low.solar_system_id = Some(lowsec_sys);
    sess_crim_low.ship_id = Some(low_crim_ship);
    for i in 0..2 {
        if i > 0 {
            for _ in 0..10 { let _ = lowsec_handle.step_manual(1.0).await; }
        }
        // rpc-lint: allow legacy CmdAttackTarget
        let _ = rpc(&ctx, &mut sess_crim_low, "beyonce", "CmdAttackTarget", &[PyRep::Integer(low_vic_pod)])
            .await
            .expect("CmdAttackTarget lowsec pod");
    }
    let (sec_after_cmd, kr_count_cmd) = ctx.db.with_snapshot(|s| {
        let krs: Vec<_> = s.kill_rights.values().filter(|kr| kr.owner_id == victim_id && kr.target_id == criminal_id).collect();
        (s.characters[&criminal_id].security_status, krs.len())
    });
    assert!(sec_after_cmd < sec_before_cmd, "CmdAttackTarget low-sec pod must deduct sec status");
    assert_eq!(kr_count_cmd, 1, "two CmdAttackTarget cycles must create only 1 kill right");
    // Verify second cycle did not re-deduct security status
    let sec_before_cycle2 = sec_after_cmd;
    for _ in 0..10 { let _ = lowsec_handle.step_manual(1.0).await; }
    // rpc-lint: allow legacy CmdAttackTarget
    let _ = rpc(&ctx, &mut sess_crim_low, "beyonce", "CmdAttackTarget", &[PyRep::Integer(low_vic_pod)]).await.expect("CmdAttackTarget cycle 3");
    assert!((ctx.db.with_snapshot(|s| s.characters[&criminal_id].security_status) - sec_before_cycle2).abs() < 1e-9);

    // 1c. Main weapon path (dogmaIM.Activate) ship-then-pod in low-sec grants pod penalty + kill right
    let low_vic_ship = spawn_ship(&ctx, victim_id, lowsec_sys, 587, DVec3::new(500.0, 0.0, 0.0));
    let mut cs_ship = ctx.build_type_combat_state(587);
    cs_ship.shield_hp = 500.0;
    let _ = lowsec_handle.spawn_ball_with_combat(Ball {
        id: low_vic_ship, type_id: 587, position: DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }, cs_ship).await;
    let gun_id = ctx.db.with_mut_snapshot(|s| {
        s.kill_rights.clear();
        s.crimewatch_aggressions.clear();
        s.characters.get_mut(&victim_id).unwrap().active_ship_id = low_vic_ship;
        s.items.values().find(|i| i.location_id == low_crim_ship && i.type_id == 487).unwrap().item_id
    });
    let _ = lowsec_handle.add_target_lock(low_crim_ship, eve_destiny::TargetLock {
        target_id: low_vic_ship, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None,
    }).await;
    let _ = lowsec_handle.add_target_lock(low_crim_ship, eve_destiny::TargetLock {
        target_id: low_vic_pod, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None,
    }).await;
    let sec_pre_ship = ctx.db.with_snapshot(|s| s.characters[&criminal_id].security_status);
    let _ = rpc(&ctx, &mut sess_crim_low, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("targetAttack".into()), PyRep::Integer(low_vic_ship)]).await.expect("Activate ship");
    let (sec_post_ship, kr_after_ship) = ctx.db.with_snapshot(|s| (s.characters[&criminal_id].security_status, s.kill_rights.len()));
    assert!(sec_post_ship < sec_pre_ship && kr_after_ship == 0, "low-sec ship hit deducts sec without kill right");
    for _ in 0..10 { let _ = lowsec_handle.step_manual(1.0).await; }
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&victim_id).unwrap().active_ship_id = low_vic_pod);
    let _ = rpc(&ctx, &mut sess_crim_low, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("targetAttack".into()), PyRep::Integer(low_vic_pod)]).await.expect("Activate pod");
    let (sec_post_pod, kr_after_pod, dogma_kr_id) = ctx.db.with_snapshot(|s| {
        let krs: Vec<_> = s.kill_rights.values().filter(|kr| kr.owner_id == victim_id && kr.target_id == criminal_id).collect();
        (s.characters[&criminal_id].security_status, krs.len(), krs[0].kill_right_id)
    });
    assert!(sec_post_pod < sec_post_ship && kr_after_pod == 1, "low-sec pod hit after ship hit must deduct pod sec and grant 1 kill right");
    let low_pod_exp = with_combat_runtime(&ctx, |rt| rt.kill_right_expires_unix.get(&dogma_kr_id).copied())
        .expect("30-day expiry recorded for low-sec pod kill right");
    assert_eq!(low_pod_exp, now + 30 * 86_400);
    // Clean up low-sec pod kill right and return criminal_id to highsec_sys before testing high-sec CONCORD kill right
    ctx.db.with_mut_snapshot(|s| {
        s.kill_rights.remove(&dogma_kr_id);
        if let Some(ch) = s.characters.get_mut(&criminal_id) {
            ch.solar_system_id = highsec_sys;
        }
    });

    // 2. High-sec criminal aggression triggers CONCORD arrival and grants a 30-day private kill right.
    let hi_eval = cw.handle_combat_aggression_with_pod(
        criminal_id,
        victim_id,
        crim_ship,
        highsec_sys,
        0.95,
        false,
        false,
        now,
    );
    assert!(matches!(hi_eval, AggressionEvaluation::CriminalFlagTriggered { .. }));
    let reports = cw.step_concord(&ctx, now + 10, 42).await;
    assert_eq!(reports.len(), 1);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&criminal_id) {
            ch.station_id = None;
            ch.solar_system_id = highsec_sys;
        }
    });
    let kr_id = reports[0].kill_right_id;
    assert!(kr_id > 0);

    let (kr_fee, kr_owner, kr_target) = ctx.db.with_snapshot(|s| {
        let kr = s.kill_rights.get(&kr_id).expect("kill right created by CONCORD");
        (kr.activation_fee.0, kr.owner_id, kr.target_id)
    });
    assert_eq!(kr_owner, victim_id);
    assert_eq!(kr_target, criminal_id);
    assert_eq!(kr_fee, 0, "newly earned kill right starts private with 0 fee");
    let exp_unix = with_combat_runtime(&ctx, |rt| rt.kill_right_expires_unix.get(&kr_id).copied())
        .expect("30-day expiry recorded");
    assert_eq!(exp_unix, now + 30 * 86_400);

    // 3. SellKillRight -> CancelSellKillRight -> SellKillRight -> BuyKillRight (15-min suspect + ISK transfer)
    let mut sess_victim = SessionState::default();
    sess_victim.user_id = Some(1);
    sess_victim.character_id = Some(victim_id);
    sess_victim.solar_system_id = Some(highsec_sys);

    let listed = rpc(
        &ctx,
        &mut sess_victim,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(2_500_000.0)],
    )
    .await
    .expect("SellKillRight");
    if let PyRep::List(items) = listed {
        assert_eq!(field(&items[0], "forSale"), PyRep::Bool(true));
        assert_eq!(field(&items[0], "price"), PyRep::Real(2_500_000.0));
    } else {
        panic!("expected List from SellKillRight");
    }

    let cancelled = rpc(
        &ctx,
        &mut sess_victim,
        "bountyProxy",
        "CancelSellKillRight",
        &[PyRep::Integer(kr_id)],
    )
    .await
    .expect("CancelSellKillRight");
    assert_eq!(field(&cancelled, "forSale"), PyRep::Bool(false));

    // Re-list for 2,500,000 ISK and have third-party pilot buy it via killRightMgr.BuyKillRight
    let _ = rpc(
        &ctx,
        &mut sess_victim,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(2_500_000.0)],
    )
    .await
    .expect("SellKillRight again");

    let mut sess_buyer = SessionState::default();
    sess_buyer.user_id = Some(3);
    sess_buyer.character_id = Some(buyer_id);
    sess_buyer.solar_system_id = Some(highsec_sys);

    let bought = rpc(
        &ctx,
        &mut sess_buyer,
        "killRightMgr",
        "BuyKillRight",
        &[PyRep::Integer(kr_id)],
    )
    .await
    .expect("BuyKillRight");
    assert_eq!(field(&bought, "suspectFlagActive"), PyRep::Bool(true));
    assert_eq!(field(&bought, "feePaidISK"), PyRep::Real(2_500_000.0));

    let (victim_bal, buyer_bal) = ctx.db.with_snapshot(|s| {
        (
            s.characters.get(&victim_id).unwrap().balance.to_wire_f64(),
            s.characters.get(&buyer_id).unwrap().balance.to_wire_f64(),
        )
    });
    assert!((victim_bal - 12_500_000.0).abs() < 1e-2);
    assert!((buyer_bal - 17_500_000.0).abs() < 1e-2);

    let suspect_until = ctx
        .crimewatch
        .read()
        .unwrap()
        .states
        .get(&criminal_id)
        .map(|s| s.suspect_flag_until_unix)
        .unwrap_or(0);
    assert_eq!(suspect_until, now + 900, "BuyKillRight sets 15-min suspect flag");
}

#[tokio::test]
async fn test_crimewatch_sec_status_loss_caps_tags_and_transactions_closure() {
    // parity: feat:crimewatch.sec-status-loss, feat:crimewatch.sec-status-gain-caps, feat:crimewatch.sec-status-transactions, feat:crimewatch.sec-tag-tier-bands
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();

    let char_id = 90_000_001_i64;
    let victim_id = 90_000_002_i64;
    // Find a low-sec CONCORD/DED station with Security Office service in SDE for Clone Soldier Tag exchange
    let security_office_svc_id = ctx
        .sde
        .station_services
        .values()
        .find(|s| s.name == "Security Office")
        .map(|s| s.service_id)
        .unwrap_or(26);
    let (lowsec_station_id, lowsec_sys_id) = ctx
        .sde
        .stations
        .values()
        .find(|st| {
            let is_concord = matches!(st.owner_corporation_id, 1_000_125 | 1_000_137)
                || st.owner_faction_id == Some(500_006);
            let is_lowsec = ctx
                .sde
                .solar_systems
                .get(&st.solar_system_id)
                .is_some_and(|s| s.security_status > 0.0 && s.security_status < 0.45);
            let has_office = ctx
                .sde
                .station_operations
                .get(&st.operation_id)
                .is_some_and(|op| op.services.contains(&security_office_svc_id));
            is_concord && is_lowsec && has_office
        })
        .map(|st| (st.station_id, st.solar_system_id))
        .expect("low-sec CONCORD Security Office station in SDE");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = 1;
            ch.security_status = 0.0;
            ch.balance = IskCents::from_f64_isk(50_000_000.0);
            ch.station_id = Some(lowsec_station_id);
            ch.solar_system_id = lowsec_sys_id;
        }
        if let Some(vic) = snap.characters.get_mut(&victim_id) {
            vic.user_id = 2;
            vic.security_status = 5.0;
        }
        for &tag_type in &[33138_u32, 33139, 33140, 33141] {
            snap.next_entity_id += 1;
            let iid = snap.next_entity_id;
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tag_type,
                    owner_id: char_id,
                    location_id: lowsec_station_id as i64,
                    flag: InventoryFlag::Hangar,
                    quantity: 5,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
    });

    // 1. Verify EVE security status loss formula and live character security_status update on illegal high-sec aggression
    let loss_ship = compute_sec_status_loss(0.0, 5.0, false, 0.95);
    let expected_ship = (10.0 - 0.0) * 0.025 * 0.95 * (1.0 + (5.0 - 0.0) / 100.0);
    assert!((loss_ship - expected_ship).abs() < 1e-6);

    let loss_pod = compute_sec_status_loss(-2.0, 5.0, true, 0.95);
    let expected_pod = (10.0 - (-2.0)) * 0.25 * 0.95 * (1.0 + (5.0 - (-2.0)) / 100.0);
    assert!((loss_pod - expected_pod).abs() < 1e-6);

    let jita_sec = ctx.sde.solar_systems[&30_000_142].security_status;
    let expected_jita_loss = compute_sec_status_loss(0.0, 5.0, false, jita_sec);
    eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, char_id, 1, 30_000_142, victim_id, false,
    )
    .expect("evaluate_and_record_pvp_aggression");
    // Repeated cycle on same target must not re-deduct security status; CONCORD arrival must not double-deduct
    eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, char_id, 1, 30_000_142, victim_id, false,
    )
    .expect("evaluate_and_record_pvp_aggression repeat");
    let mut cw_concord = CrimeWatchDirector::new();
    cw_concord.states = ctx.crimewatch.read().unwrap().states.clone();
    let _ = cw_concord.step_concord(&ctx, now + 30, 99).await;
    let sec_after_aggr = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!((sec_after_aggr - (-expected_jita_loss)).abs() < 1e-6, "live character security_status must drop once by exact ship loss formula");

    // 1b. Verify beyonce.CmdAttackTarget legacy high-sec path uses compute_sec_status_loss with real victim sec status (not flat -0.5)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = 0.0;
            ch.solar_system_id = 30_000_142;
            ch.station_id = None;
        }
    });
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    let att_ship = spawn_ship(&ctx, char_id, 30_000_142, 587, DVec3::new(1_000.0, 0.0, 0.0));
    let vic_ship = spawn_ship(&ctx, victim_id, 30_000_142, 587, DVec3::ZERO);
    for (id, pos) in [(att_ship, DVec3::new(1_000.0, 0.0, 0.0)), (vic_ship, DVec3::ZERO)] {
        let mut cs = ctx.build_type_combat_state(587);
        cs.shield_hp = 500.0;
        let _ = jita_handle.spawn_ball_with_combat(Ball {
            id, type_id: 587, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
            inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }, cs).await;
    }
    let mut sess_cmd = SessionState::default();
    sess_cmd.user_id = Some(1);
    sess_cmd.character_id = Some(char_id);
    sess_cmd.solar_system_id = Some(30_000_142);
    sess_cmd.ship_id = Some(att_ship);
    let _ = rpc(
        &ctx,
        &mut sess_cmd,
        "beyonce",
        // rpc-lint: allow legacy CmdAttackTarget
        "CmdAttackTarget",
        &[PyRep::Integer(vic_ship)],
    )
    .await
    .expect("CmdAttackTarget highsec");
    let sec_after_cmd = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!(
        (sec_after_cmd - (-expected_jita_loss)).abs() < 1e-6,
        "CmdAttackTarget high-sec must use compute_sec_status_loss ({}) instead of flat -0.5 (got {})",
        -expected_jita_loss,
        sec_after_cmd
    );

    // 2. Verify NPC kill security status gain caps at +5.0 (`feat:crimewatch.sec-status-gain-caps`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = 4.99;
        }
    });
    let mut npc_dir = NpcSpawnManager::new();
    let make_npc = |ball_id: i64| NpcCombatEntity {
        ball_id,
        type_id: 12000,
        faction_id: 500_010,
        state: NpcAiState::Idle,
        spawn_group_id: 1,
        spawn_pos: DVec3::ZERO,
        target_id: None,
        bounty_isk: 10_000_000.0,
        optimal_range: 10_000.0,
        orbit_range: 5_000.0,
        base_dps: 50.0,
        damage_vector: DamageVector { em: 10.0, thermal: 10.0, kinetic: 15.0, explosive: 15.0 },
        last_attack_tick: 0,
        has_shield_rep: false,
        has_armor_rep: false,
    };
    npc_dir.entities.insert(80_000_001, make_npc(80_000_001));
    let _ = npc_dir.handle_npc_destroyed(&ctx, lowsec_sys_id, 80_000_001, char_id, now).expect("handle_npc_destroyed");
    let sec_after_rat = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().security_status);
    assert!(sec_after_rat > 4.99 && sec_after_rat <= 5.0, "NPC kill sec gain must stop at +5.0");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = 5.0;
        }
    });
    npc_dir.entities.insert(80_000_002, make_npc(80_000_002));
    let _ = npc_dir.handle_npc_destroyed(&ctx, lowsec_sys_id, 80_000_002, char_id, now).expect("handle_npc_destroyed at cap");
    let sec_at_cap = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().security_status);
    assert!((sec_at_cap - 5.0).abs() < 1e-6, "NPC kill sec gain must not exceed +5.0");

    // Pilot starting above +5.0 (e.g. +6.0) must not be clamped down to 5.0 on NPC kill
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = 6.0;
        }
    });
    npc_dir.entities.insert(80_000_003, make_npc(80_000_003));
    let _ = npc_dir.handle_npc_destroyed(&ctx, lowsec_sys_id, 80_000_003, char_id, now).expect("handle_npc_destroyed above +5.0");
    let sec_above_cap = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().security_status);
    assert!((sec_above_cap - 6.0).abs() < 1e-6, "NPC kill must not lower security status above +5.0 (got {sec_above_cap})");

    // 3. Verify all 4 Clone Soldier Tag bands, high-sec station rejection, ISK fee, and out-of-band refusal (`feat:crimewatch.sec-tag-tier-bands`)
    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.station_id = Some(60_003_760); // Jita 4-4 (high-sec non-CONCORD station)
    sess.solar_system_id = Some(30_000_142);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = -9.0;
        }
    });
    let highsec_err = rpc(
        &ctx,
        &mut sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Real(-8.5), PyRep::Dict(vec![(PyRep::Integer(33138), PyRep::Integer(1))])],
    )
    .await;
    assert!(
        highsec_err.unwrap_err().contains("SecurityOfficeRequired"),
        "High-sec non-CONCORD station must reject tag turn-in"
    );

    sess.station_id = Some(lowsec_station_id);
    sess.solar_system_id = Some(lowsec_sys_id);
    let bal_before_tags = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(lowsec_station_id);
        ch.solar_system_id = lowsec_sys_id;
        ch.balance
    });

    for (start_sec, valid_tag, invalid_tag, req_qty, target_sec, expected_sec) in [
        (-9.0_f64, 33138_i64, 33139_i64, 1_i64, -8.5_f64, -8.5_f64), // Trainer: -10 to -8
        (-6.5_f64, 33139_i64, 33140_i64, 1_i64, -6.0_f64, -6.0_f64), // Recruiter: -8 to -5
        (-3.5_f64, 33140_i64, 33141_i64, 1_i64, -3.0_f64, -3.0_f64), // Transporter: -5 to -2
        (-0.2_f64, 33141_i64, 33138_i64, 5_i64, 0.0_f64, 0.0_f64),   // Negotiator: -2 to 0 (capped at 0.0)
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.security_status = start_sec;
            }
        });
        let bad = rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(target_sec), PyRep::Dict(vec![(PyRep::Integer(invalid_tag), PyRep::Integer(1))])],
        )
        .await;
        assert!(bad.unwrap_err().contains("SecurityTagBandMismatch"));

        let ok = rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(target_sec), PyRep::Dict(vec![(PyRep::Integer(valid_tag), PyRep::Integer(req_qty))])],
        )
        .await
        .expect("ApplyTagsForSecStatus");
        let new_sec = match field(&ok, "newSecurityStatus") {
            PyRep::Real(v) => v,
            other => panic!("expected Real newSecurityStatus, got {other:?}"),
        };
        assert!((new_sec - expected_sec).abs() < 1e-6);
    }
    // Reject i64->i32 overflow / negative tag quantities and negative tag type IDs (`R-20261101-P0-01`)
    for bad_qty in [4_294_966_296_i64, 2_147_483_648_i64, 0_i64, -5_i64] {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.security_status = -0.5;
            }
        });
        let err = rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(bad_qty))])],
        )
        .await;
        assert!(err.unwrap_err().contains("InvalidQuantity"));
    }
    assert!(
        rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(-1), PyRep::Integer(1))])],
        )
        .await
        .unwrap_err()
        .contains("InvalidSecurityTagType")
    );

    // Tag at 0.0 is refused (cannot raise above 0.0)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = 0.0;
        }
    });
    assert!(
        rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(1))])],
        )
        .await
        .unwrap_err()
        .contains("InvalidTargetSecurityStatus")
    );

    // Verify derived placeholder ISK fee charged for 4 effective tags (1M + 2M + 5M + 10M = 18M ISK, 4 Negotiator tags preserved, journal logged)
    let (bal_after_tags, rem_neg_tags, has_tag_journal) = ctx.db.with_snapshot(|s| {
        let rem = s.items.values().filter(|i| i.owner_id == char_id && i.type_id == 33141).map(|i| i.quantity).sum::<i32>();
        let j = s.wallet_journal.iter().any(|w| w.character_id == char_id && w.entry_type == "SecurityTagProcessing" && w.amount == IskCents::from_isk(-10_000_000));
        (s.characters[&char_id].balance, rem, j)
    });
    assert_eq!(bal_before_tags.0 - bal_after_tags.0, IskCents::from_isk(18_000_000).0);
    assert_eq!(rem_neg_tags, 4, "excess tags above 0.0 cap must not be consumed");
    assert!(has_tag_journal, "SecurityTagFee wallet journal entry must be recorded");

    // 4. Verify GetSecurityStatusTransactions returns V24 client columns by name
    let txs = rpc(&ctx, &mut sess, "crimewatch", "GetSecurityStatusTransactions", &[])
        .await
        .expect("GetSecurityStatusTransactions");
    let PyRep::Rowset { columns, rows, .. } = txs else {
        panic!("expected Rowset, got {txs:?}");
    };
    assert_eq!(rows.len(), 6, "2 illegal PvP aggressions (no CONCORD duplicate) + 4 tag turn-ins recorded");
    let row0: std::collections::HashMap<&str, &PyRep> = columns
        .iter()
        .map(|s| s.as_str())
        .zip(rows[2].iter())
        .collect();
    let expected_ft = (now as i64 + 11_644_473_600) * 10_000_000;
    assert_eq!(row0.get("eventDate"), Some(&&PyRep::Integer(expected_ft)));
    assert_eq!(row0.get("eventTypeID"), Some(&&PyRep::Integer(385)));
    assert_eq!(row0.get("locationID"), Some(&&PyRep::Integer(lowsec_sys_id as i64)));
    assert_eq!(row0.get("modification"), Some(&&PyRep::Real(0.5)));
    assert_eq!(row0.get("newValue"), Some(&&PyRep::Real(-8.5)));
    assert_eq!(row0.get("otherOwnerID"), Some(&&PyRep::None));
    assert_eq!(row0.get("otherTypeID"), Some(&&PyRep::None));
}
