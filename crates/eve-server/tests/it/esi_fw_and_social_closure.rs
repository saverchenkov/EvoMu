//! Closure tests for Crimewatch Kill Rights, Security Status, and Fleet Boss Handover /
//! Stored Formations & Warp Shapes / Target Tagging (`feat:crimewatch.*`, `feat:fleet.*`).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, DamageVector, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::crimewatch::{
    compute_sec_status_loss, AggressionEvaluation, CrimeWatchDirector,
};
use eve_server::combat::death_pipeline::execute_death_pipeline;
use eve_server::combat::npc_ai::{NpcAiState, NpcCombatEntity, NpcSpawnManager};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_combat_runtime;
use eve_server::services::fleet_social::fleet_social_notify::remove_character_from_all_fleets;
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
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
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
    let lowsec_sys = 30_000_119_u32; // Low-sec system
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
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id,
                    type_id: 587,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
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
    // Find a low-sec station in SDE for Clone Soldier Tag exchange
    let (lowsec_station_id, lowsec_sys_id) = ctx
        .sde
        .stations
        .values()
        .find(|st| {
            ctx.sde
                .solar_systems
                .get(&st.solar_system_id)
                .is_some_and(|s| s.security_status > 0.0 && s.security_status < 0.45)
        })
        .map(|st| (st.station_id, st.solar_system_id))
        .unwrap_or((60_003_760, 30_000_119));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = 1;
            ch.security_status = 0.0;
            ch.station_id = Some(lowsec_station_id);
            ch.solar_system_id = lowsec_sys_id;
        }
        if let Some(vic) = snap.characters.get_mut(&victim_id) {
            vic.user_id = 2;
            vic.security_status = 5.0;
        }
        for &tag_type in &[35661_u32, 35662, 35663, 35664] {
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

    // 1. Verify EVE security status loss formula (ship & pod, accounting for target_sec and aggressor_sec)
    let loss_ship = compute_sec_status_loss(0.0, 5.0, false, 0.95);
    let expected_ship = (10.0 - 0.0) * 0.025 * 0.95 * (1.0 + (5.0 - 0.0) / 100.0);
    assert!((loss_ship - expected_ship).abs() < 1e-6);

    let loss_pod = compute_sec_status_loss(-2.0, 5.0, true, 0.95);
    let expected_pod = (10.0 - (-2.0)) * 0.25 * 0.95 * (1.0 + (5.0 - (-2.0)) / 100.0);
    assert!((loss_pod - expected_pod).abs() < 1e-6);

    // 2. Verify NPC kill security status gain caps at +5.0 (`feat:crimewatch.sec-status-gain-caps`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = 4.99;
        }
    });
    let mut npc_dir = NpcSpawnManager::new();
    npc_dir.entities.insert(
        80_000_001,
        NpcCombatEntity {
            ball_id: 80_000_001,
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
            damage_vector: DamageVector {
                em: 10.0,
                thermal: 10.0,
                kinetic: 15.0,
                explosive: 15.0,
            },
            last_attack_tick: 0,
            has_shield_rep: false,
            has_armor_rep: false,
        },
    );
    let _ = npc_dir
        .handle_npc_destroyed(&ctx, lowsec_sys_id, 80_000_001, char_id, now)
        .expect("handle_npc_destroyed");
    let sec_after_rat = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().security_status);
    assert!(sec_after_rat > 4.99 && sec_after_rat <= 5.0, "NPC kill sec gain must stop at +5.0");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = 5.0;
        }
    });
    npc_dir.entities.insert(
        80_000_002,
        NpcCombatEntity {
            ball_id: 80_000_002,
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
            damage_vector: DamageVector {
                em: 10.0,
                thermal: 10.0,
                kinetic: 15.0,
                explosive: 15.0,
            },
            last_attack_tick: 0,
            has_shield_rep: false,
            has_armor_rep: false,
        },
    );
    let _ = npc_dir
        .handle_npc_destroyed(&ctx, lowsec_sys_id, 80_000_002, char_id, now)
        .expect("handle_npc_destroyed at cap");
    let sec_at_cap = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().security_status);
    assert!((sec_at_cap - 5.0).abs() < 1e-6, "NPC kill sec gain must not exceed +5.0");

    // 3. Verify all 4 Clone Soldier Tag bands and out-of-band refusal (`feat:crimewatch.sec-tag-tier-bands`)
    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.station_id = Some(lowsec_station_id);
    sess.solar_system_id = Some(lowsec_sys_id);

    for (start_sec, valid_tag, invalid_tag, expected_sec) in [
        (-9.0_f64, 35661_i64, 35662_i64, -8.5_f64), // Trainer: -10 to -8
        (-6.5_f64, 35662_i64, 35663_i64, -6.0_f64), // Recruiter: -8 to -5
        (-3.5_f64, 35663_i64, 35664_i64, -3.0_f64), // Transporter: -5 to -2
        (-0.2_f64, 35664_i64, 35661_i64, 0.0_f64),  // Negotiator: -2 to 0 (capped at 0.0!)
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
            &[PyRep::Integer(invalid_tag), PyRep::Integer(1)],
        )
        .await;
        assert!(bad.unwrap_err().contains("SecurityTagBandMismatch"));

        let ok = rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Integer(valid_tag), PyRep::Integer(1)],
        )
        .await
        .expect("ApplyTagsForSecStatus");
        let new_sec = match field(&ok, "newSecurityStatus") {
            PyRep::Real(v) => v,
            other => panic!("expected Real newSecurityStatus, got {other:?}"),
        };
        assert!((new_sec - expected_sec).abs() < 1e-6);
    }

    // Tag at 0.0 is refused (cannot raise above 0.0)
    let at_zero = rpc(
        &ctx,
        &mut sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Integer(35664), PyRep::Integer(1)],
    )
    .await;
    assert!(at_zero.unwrap_err().contains("SecurityTagBandMismatch"));

    // 4. Verify GetSecurityStatusTransactions returns exact transaction rows
    let txs = rpc(&ctx, &mut sess, "crimewatch", "GetSecurityStatusTransactions", &[])
        .await
        .expect("GetSecurityStatusTransactions");
    let PyRep::Rowset { rows, .. } = txs else {
        panic!("expected Rowset, got {txs:?}");
    };
    assert_eq!(rows.len(), 4, "4 tag turn-ins recorded");
    assert_eq!(rows[0][1], PyRep::Real(-9.0));
    assert_eq!(rows[0][2], PyRep::Real(-8.5));
    assert_eq!(rows[0][3], PyRep::Real(0.5));
    assert_eq!(rows[0][4], PyRep::String("Clone Soldier Security Tag".into()));
}

#[tokio::test]
async fn test_fleet_boss_handover_formations_and_target_tagging_closure() {
    // parity: feat:fleet.boss-handover, feat:fleet.stored-formations-and-warp-shapes, feat:fleet.target-tagging
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);

    let sys_id = 30_000_142_u32;
    let boss_id = 90_000_001_i64;
    let fc_id = 90_000_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, uid) in [(boss_id, 1_u64), (fc_id, 2_u64)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.user_id = uid;
                ch.solar_system_id = sys_id;
                ch.station_id = None;
            }
        }
    });

    let ship1 = spawn_ship(&ctx, boss_id, sys_id, 587, DVec3::new(0.0, 0.0, 0.0));
    let ship2 = spawn_ship(&ctx, fc_id, sys_id, 587, DVec3::new(1_000.0, 0.0, 0.0));
    let target_ship = spawn_ship(&ctx, fc_id, sys_id, 587, DVec3::new(2_000.0, 0.0, 0.0));

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (sid, pos) in [
        (ship1, DVec3::ZERO),
        (ship2, DVec3::new(1_000.0, 0.0, 0.0)),
        (target_ship, DVec3::new(2_000.0, 0.0, 0.0)),
    ] {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: sid,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    let mut sess1 = SessionState::new(64_001);
    crate::common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    let mut sess2 = SessionState::new(64_002);
    crate::common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;

    for (s, cid) in [(&mut sess1, boss_id), (&mut sess2, fc_id)] {
        rpc(&ctx, s, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(cid)])
            .await
            .expect("SelectCharacterID");
    }
    ctx.advance_secs(15);
    let ship1 = sess1.ship_id.expect("sess1 ship_id");

    let created = rpc(&ctx, &mut sess1, "fleetObjectHandler", "CreateFleet", &[])
        .await
        .expect("CreateFleet");
    let fid = match created {
        PyRep::Integer(v) => v,
        _ => match field(&created, "fleetID") {
            PyRep::Integer(v) => v,
            other => panic!("expected fleetID, got {other:?}"),
        },
    };
    sess1.fleet_id = Some(fid);

    let _ = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        "Invite",
        &[PyRep::Integer(fc_id), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)],
    )
    .await
    .expect("Invite");
    let _ = rpc(&ctx, &mut sess2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fid)])
        .await
        .expect("AcceptInvite");
    sess2.fleet_id = Some(fid);

    // 2. Target tagging (`feat:fleet.target-tagging`): A-Z / 1-9 validation, on-grid lock check, and destruction cleanup
    let unlocked_err = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
        "SetTargetTag",
        &[PyRep::Integer(target_ship), PyRep::String("A".into())],
    )
    .await;
    assert!(unlocked_err.unwrap_err().contains("TargetNotLocked"));

    let _ = sys_handle
        .add_target_lock(
            ship1,
            TargetLock {
                target_id: target_ship,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;
    let bad_tag = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
        "SetTargetTag",
        &[PyRep::Integer(target_ship), PyRep::String("0".into())],
    )
    .await;
    assert!(bad_tag.unwrap_err().contains("InvalidTargetTag"));

    let tagged = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
        "SetTargetTag",
        &[PyRep::Integer(target_ship), PyRep::String("A".into())],
    )
    .await
    .expect("SetTargetTag A");
    assert_eq!(field(&tagged, "tag"), PyRep::String("A".into()));

    // Destroy target_ship -> target tag must be cleared automatically
    let _ = execute_death_pipeline(&ctx, sys_id, fc_id, target_ship, boss_id, DVec3::ZERO, 99)
        .await
        .expect("destroy tagged ship");
    let tags_after = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
        "GetTargetTags",
        &[],
    )
    .await
    .expect("GetTargetTags");
    assert_eq!(tags_after, PyRep::Dict(Vec::new()));

    // 3. Stored formations & FleetWarp deterministic landing offsets (`feat:fleet.stored-formations-and-warp-shapes`)
    let _ = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        // rpc-lint: allow auditor acceptance text feat:fleet.stored-formations-and-warp-shapes
        "SetFleetFormation",
        &[PyRep::Integer(3), PyRep::Real(5_000.0), PyRep::Real(10_000.0)], // Arrow
    )
    .await
    .expect("SetFleetFormation");

    let warp_res = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        "FleetWarp",
        &[PyRep::Integer(40_000_001), PyRep::Real(0.0)],
    )
    .await
    .expect("FleetWarp");
    assert_eq!(field(&warp_res, "formationID"), PyRep::Integer(3));
    if let PyRep::List(positions) = field(&warp_res, "landingPositions") {
        assert_eq!(positions.len(), 2);
        assert_ne!(positions[0], positions[1], "Arrow formation separates member landing positions");
    } else {
        panic!("expected landingPositions list");
    }

    // 4. Boss handover (`feat:fleet.boss-handover`): MakeLeader, LeaveFleet, and disconnect handover
    let _ = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        "MakeLeader",
        &[PyRep::Integer(fc_id)],
    )
    .await
    .expect("MakeLeader");
    assert_eq!(ctx.db.with_snapshot(|s| s.fleets.get(&fid).unwrap().boss_id), fc_id);

    // Boss (fc_id) leaves -> boss passes automatically to boss_id
    let left = rpc(&ctx, &mut sess2, "fleetObjectHandler", "LeaveFleet", &[])
        .await
        .expect("LeaveFleet");
    assert_eq!(field(&left, "disbanded"), PyRep::Bool(false));
    assert_eq!(ctx.db.with_snapshot(|s| s.fleets.get(&fid).unwrap().boss_id), boss_id);

    // Re-invite fc_id, then disconnect boss_id -> boss passes automatically to fc_id
    let _ = rpc(
        &ctx,
        &mut sess1,
        "fleetObjectHandler",
        "Invite",
        &[PyRep::Integer(fc_id), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)],
    )
    .await
    .expect("Re-invite fc_id");
    ctx.advance_secs(15);
    let _ = rpc(&ctx, &mut sess2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fid)])
        .await
        .expect("AcceptInvite 2");
    let _ = remove_character_from_all_fleets(&ctx, boss_id);
    assert_eq!(ctx.db.with_snapshot(|s| s.fleets.get(&fid).unwrap().boss_id), fc_id);
}
