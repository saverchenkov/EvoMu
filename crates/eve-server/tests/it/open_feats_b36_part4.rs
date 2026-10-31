//! Integration tests for Batch 36 open `feat:pve.*` parity rows (part 4: ESS grid, Main Bank autopayout, Reserve keys).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

fn kv_map(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.clone(),
        PyRep::Dict(entries) => entries
            .iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(s) | PyRep::Token(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect(),
        _ => HashMap::new(),
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

async fn step_until_stopped(ctx: &ServerContext, sys_id: u32, ship_id: i64) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for _ in 0..120 {
        let _ = sys_h.step_manual(1.0).await;
        if let Ok(Some((_, b))) = sys_h.get_ball_snapshot(ship_id).await {
            if matches!(b.mode, MovementMode::Stop) && b.velocity.length() < 0.5 {
                break;
            }
        }
    }
}

#[tokio::test]
async fn test_b36_ess_grid_restrictions_main_autopayout_and_reserve_tiers() {
    // parity: feat:pve.ess-grid-warp-mwd-restrictions, feat:pve.ess-main-bank-autopayout, feat:pve.ess-reserve-bank-key-tiers
    eve_testkit::attest(&[
        "feat:pve.ess-grid-warp-mwd-restrictions",
        "feat:pve.ess-main-bank-autopayout",
        "feat:pve.ess-reserve-bank-key-tiers",
    ]);
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| {
            s.security_status <= -0.1
                && s.solar_system_id < 31_000_000
                && ctx
                    .sde
                    .jumps
                    .values()
                    .any(|j| j.from_solar_system_id == s.solar_system_id)
        })
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);
    let stargate_id = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id)
        .unwrap()
        .stargate_id as i64;

    let pilot_a = 100_001_i64;
    let pilot_b = 100_002_i64;
    let ship_a = 960_001_i64;
    let ship_b = 960_002_i64;
    let frig_a = 960_003_i64;
    let mwd_id = 960_010_i64;
    let ab_id = 960_011_i64;
    let mjd_id = 960_012_i64;
    let cloak_id = 960_013_i64;
    let cyno_id = 960_014_i64;
    let wep_a = 960_015_i64;
    let ammo_a = 960_016_i64;
    let wep_b = 960_017_i64;
    let ammo_b = 960_018_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.values().next().cloned().expect("base char");
        let mut ca = base.clone();
        ca.character_id = pilot_a;
        ca.user_id = 1;
        ca.solar_system_id = sys_id;
        ca.station_id = None;
        ca.active_ship_id = ship_a;
        ca.balance = IskCents(0);
        let mut cb = base;
        cb.character_id = pilot_b;
        cb.user_id = 2;
        cb.solar_system_id = sys_id;
        cb.station_id = None;
        cb.active_ship_id = ship_b;
        cb.balance = IskCents(0);
        snap.characters.insert(pilot_a, ca);
        snap.characters.insert(pilot_b, cb);

        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.sov_alliance_id = Some(99_000_001);
        st.dbs_multiplier = 1.35;
        st.ess_main_bank = IskCents(0);
        st.ess_reserve_bank = IskCents(0);

        snap.items.insert(ship_a, make_item(ship_a, 639, pilot_a, i64::from(sys_id), InventoryFlag::None, 1, true));
        snap.items.insert(ship_b, make_item(ship_b, 639, pilot_b, i64::from(sys_id), InventoryFlag::None, 1, true));
        snap.items.insert(frig_a, make_item(frig_a, 587, pilot_a, i64::from(sys_id), InventoryFlag::None, 1, true));
        snap.items.insert(mwd_id, make_item(mwd_id, 12052, pilot_a, ship_a, InventoryFlag::MedSlot0, 1, true));
        snap.items.insert(ab_id, make_item(ab_id, 12056, pilot_a, ship_a, InventoryFlag::MedSlot1, 1, true));
        snap.items.insert(mjd_id, make_item(mjd_id, 33915, pilot_a, ship_a, InventoryFlag::MedSlot2, 1, true));
        snap.items.insert(cloak_id, make_item(cloak_id, 11370, pilot_a, ship_a, InventoryFlag::HiSlot1, 1, true));
        snap.items.insert(cyno_id, make_item(cyno_id, 21096, pilot_a, ship_a, InventoryFlag::HiSlot2, 1, true));
        snap.items.insert(960_019, make_item(960_019, 16273, pilot_a, ship_a, InventoryFlag::Cargo, 2_000, false));
        snap.items.insert(wep_a, make_item(wep_a, 2961, pilot_a, ship_a, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(ammo_a, make_item(ammo_a, 21894, pilot_a, ship_a, InventoryFlag::HiSlot0, 5_000, false));
        snap.items.insert(wep_b, make_item(wep_b, 2961, pilot_b, ship_b, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(ammo_b, make_item(ammo_b, 21894, pilot_b, ship_b, InventoryFlag::HiSlot0, 5_000, false));
        for mid in [mwd_id, mjd_id, cloak_id, cyno_id, wep_a, wep_b] {
            snap.module_online_states.insert(mid, true);
        }
        snap.module_online_states.insert(ab_id, false);
        for pid in [pilot_a, pilot_b] {
            let sk = snap.skills.entry(pid).or_default();
            for sid in [3300_u32, 3302, 3305, 3308, 3310, 3311, 3312, 3315, 3316, 3318, 3334, 3413, 3418, 3426, 3432, 3449, 3450, 3454, 3455, 4385, 11579, 20327, 21603, 21667] {
                sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 1_280_000.0 });
            }
            for tid in [639_u32, 2961, 21894, 12052, 12056, 33915, 11370, 21096] {
                if let Some(t) = ctx.sde.types.get(&tid) {
                    for sa in [182_u16, 183, 184] {
                        if let Some(&req_f) = t.default_attributes.get(&sa) {
                            let rsid = req_f as u32;
                            if rsid > 0 {
                                sk.insert(rsid, TrainedSkillRecord { type_id: rsid, level: 5, skill_points: 1_280_000.0 });
                            }
                        }
                    }
                }
            }
        }
    });
    ctx.invalidate_ship_fitting(ship_a);
    ctx.invalidate_ship_fitting(ship_b);

    let mut sess_a = SessionState::new(3612);
    sess_a.user_id = Some(1);
    sess_a.character_id = Some(pilot_a);
    sess_a.solar_system_id = Some(sys_id);
    sess_a.ship_id = Some(ship_a);

    let mut sess_b = SessionState::new(3613);
    sess_b.user_id = Some(2);
    sess_b.character_id = Some(pilot_b);
    sess_b.solar_system_id = Some(sys_id);
    sess_b.ship_id = Some(ship_b);

    // 1. ESS intrusion site & acceleration gate (essMgr.GetDataForClientSolarSystem -> essID + essGateID)
    let ess_info = kv_map(
        &ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "GetDataForClientSolarSystem", &[PyRep::Integer(i64::from(sys_id))], &kw, None)
            .await
            .unwrap(),
    );
    let ess_id = ess_info["essID"].as_i64().unwrap();
    let ess_gate_id = ess_info["essGateID"].as_i64().unwrap();

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let gate_pos = sys_h.get_ball_snapshot(ess_gate_id).await.unwrap().unwrap().1.position;
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_a,
                type_id: 639,
                position: gate_pos + DVec3::new(300_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(639),
        )
        .await;

    // Outside 75 km bubble: MWD allowed, direct warp onto ESS grid blocked, warp to exterior ESS gate allowed!
    ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
        .await
        .expect("MWD allowed beyond 75 km");
    let _ = ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Deactivate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())], &kw, None).await;

    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(ess_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleWarpBlocked"
    );
    ctx.dispatch_rpc(&mut sess_a, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(ess_gate_id)], &kw, None)
        .await
        .expect("Warp to exterior ESS gate");
    step_until_stopped(&ctx, sys_id, ship_a).await;

    // Frigate rejected at ESS acceleration gate; Cruiser admitted onto ESS grid!
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: frig_a,
                type_id: 587,
                position: gate_pos + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 1.0,
                mass: 1_067_000.0,
                radius: 38.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await;
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&pilot_a).unwrap().active_ship_id = frig_a);
    sess_a.ship_id = Some(frig_a);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(ess_gate_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "ShipRestrictedByEssGate"
    );
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&pilot_a).unwrap().active_ship_id = ship_a);
    sess_a.ship_id = Some(ship_a);

    let _ = sys_h.set_ball_position(ship_a, gate_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    ctx.dispatch_rpc(&mut sess_a, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(ess_gate_id)], &kw, None)
        .await
        .expect("Cruiser enters ESS grid");
    step_until_stopped(&ctx, sys_id, ship_a).await;

    // Inside 75 km ESS bubble: Warp, MWD, MJD, Cloak, Cyno ALL refused; Afterburner allowed!
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(stargate_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleWarpBlocked"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleMwdBlocked"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(mjd_id), PyRep::String("microJumpDrive".into()), PyRep::None, PyRep::Integer(0)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleMjdBlocked"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(cloak_id), PyRep::String("cloaking".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleCloakBlocked"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_id), PyRep::String("cynosuralGeneration".into()), PyRep::None, PyRep::Integer(0)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssIntrusionBubbleCynoBlocked"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mwd_id, false);
        snap.module_online_states.insert(ab_id, true);
    });
    ctx.invalidate_ship_fitting(ship_a);
    ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(ab_id), PyRep::String("moduleBonusAfterburner".into()), PyRep::None, PyRep::Integer(1)], &kw, None)
        .await
        .expect("Afterburner allowed inside 75 km ESS bubble");
    let _ = ctx.dispatch_rpc(&mut sess_a, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ab_id), PyRep::String("moduleBonusAfterburner".into())], &kw, None).await;

    // 2. ESS Main Bank 2h45m autopayout via live NPC bounty combat (2:1 ratio) + tick_background_cluster
    let bounty_npc_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 11 && t.default_attributes.get(&481).copied().unwrap_or(0.0) >= 100_000.0)
        .map(|t| t.type_id)
        .unwrap();
    let rat_spot = DVec3::new(-5.0e9, 0.0, 0.0);
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_b,
                type_id: 639,
                position: rat_spot,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(639),
        )
        .await;
    let ess_pos = sys_h.get_ball_snapshot(ess_id).await.unwrap().unwrap().1.position;

    for (idx, (is_a, npc_bid)) in [(true, 960_501_i64), (true, 960_502), (false, 960_503)].into_iter().enumerate() {
        let origin = if is_a { ess_pos + DVec3::new(1_500.0, 0.0, 0.0) } else { rat_spot };
        let mut cs = ctx.build_type_combat_state(bounty_npc_tid);
        cs.shield_hp = 10.0;
        cs.armor_hp = 10.0;
        cs.structure_hp = 10.0;
        cs.signature_radius = 400.0;
        let _ = sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: npc_bid,
                    type_id: bounty_npc_tid,
                    position: origin + DVec3::new(5_000.0 + (idx as f64) * 100.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 100.0,
                    inertia: 1.0,
                    mass: 100_000.0,
                    radius: 400.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                cs,
            )
            .await;
        let s_ref = if is_a { &mut sess_a } else { &mut sess_b };
        let w_id = if is_a { wep_a } else { wep_b };
        let attacker_sid = if is_a { ship_a } else { ship_b };
        ctx.dispatch_rpc(s_ref, "dogmaIM", None, "AddTarget", &[PyRep::Integer(npc_bid)], &kw, None)
            .await
            .expect("AddTarget bounty NPC");
        for _ in 0..30 {
            if sys_h.get_target_locks(attacker_sid).await.unwrap().iter().any(|l| l.target_id == npc_bid && l.locked) {
                break;
            }
            let _ = sys_h.step_manual(1.0).await;
        }
        for _ in 0..20 {
            if sys_h.get_ball_snapshot(npc_bid).await.unwrap().is_none() {
                break;
            }
            let r = kv_map(
                &ctx.dispatch_rpc(
                    s_ref,
                    "dogmaIM",
                    None,
                    "Activate",
                    &[PyRep::Integer(w_id), PyRep::Integer(npc_bid)],
                    &kw,
                    None,
                )
                .await
                .unwrap(),
            );
            if r.get("destroyed").and_then(|v| v.as_bool()) == Some(true) {
                break;
            }
            let _ = sys_h.step_manual(1.0).await;
        }
        assert!(
            sys_h.get_ball_snapshot(npc_bid).await.unwrap().is_none(),
            "Bounty NPC {npc_bid} must be destroyed"
        );
    }

    // Advance 20m bounty tick (1,200 s) so ratting bounties pool into ESS Main Bank via tick_background_cluster
    ctx.db.clock().advance_secs(1_200);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let (main_pooled, bal_a_pre, bal_b_pre) = ctx.db.with_snapshot(|s| {
        (
            s.system_states[&sys_id].ess_main_bank.0,
            s.characters[&pilot_a].balance.0,
            s.characters[&pilot_b].balance.0,
        )
    });
    assert!(main_pooled > 0);
    let _ = sys_h.set_ball_position(ship_a, ess_pos + DVec3::new(500.0, 0.0, 0.0)).await;

    ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToMainBank", &[PyRep::Integer(ess_id)], &kw, None)
        .await
        .expect("AttemptLinkToMainBank start");
    ctx.db.clock().advance_secs(9_900);
    let _ = ctx
        .dispatch_rpc(&mut sess_a, "essMgr", None, "GetDataForClientSolarSystem", &[PyRep::Integer(i64::from(sys_id))], &kw, None)
        .await
        .unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&sys_id].ess_main_bank.0), main_pooled);

    ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestMainBankUnlink", &[], &kw, None)
        .await
        .expect("RequestMainBankUnlink");
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let (main_after, gain_a, gain_b, has_j_a, has_j_b) = ctx.db.with_snapshot(|s| {
        (
            s.system_states[&sys_id].ess_main_bank.0,
            s.characters[&pilot_a].balance.0 - bal_a_pre,
            s.characters[&pilot_b].balance.0 - bal_b_pre,
            s.wallet_journal.iter().any(|j| j.character_id == pilot_a && j.entry_type == "ESSMainBankPayout"),
            s.wallet_journal.iter().any(|j| j.character_id == pilot_b && j.entry_type == "ESSMainBankPayout"),
        )
    });
    assert_eq!(main_after, 0);
    assert_eq!(gain_a + gain_b, main_pooled);
    assert!((gain_a - 2 * gain_b).abs() <= 2);
    assert!(has_j_a && has_j_b);

    // 3. ESS Reserve Bank key tiers (15m vs 45m quadrant keys) + live ISK pulse streaming via AttemptLinkToReserveBank
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&sys_id).unwrap().ess_reserve_bank = IskCents(4_500_000);
    });
    let _ = sys_h.set_ball_position(ship_a, ess_pos + DVec3::new(5_000.0, 0.0, 0.0)).await;
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[], &kw, None)
            .await
            .unwrap_err()
            .code,
        "OutsideEssLinkRange"
    );
    let _ = sys_h.set_ball_position(ship_a, ess_pos + DVec3::new(500.0, 0.0, 0.0)).await;

    let keys_15m = [57714_u32, 57716, 57717, 57720];
    let keys_45m = [57715_u32, 57719, 57718, 57721];
    let (mut wrong_quad_15m, mut matched_15m) = (false, false);
    for (idx, &tid) in keys_15m.iter().enumerate() {
        let iid = 960_610 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(iid, make_item(iid, tid, pilot_a, ship_a, InventoryFlag::Cargo, 1, false));
        });
        match ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(iid)], &kw, None).await {
            Ok(rep) => {
                assert_eq!(kv_map(&rep)["durationMinutes"].as_i64(), Some(15));
                assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&iid)));
                assert!(sess_a.drain_notifications().iter().any(|(n, _)| n == "OnReserveBankUnlock"));
                matched_15m = true;
            }
            Err(e) => {
                assert_eq!(e.code, "EssReserveKeyWrongQuadrant");
                wrong_quad_15m = true;
            }
        }
    }
    assert!(wrong_quad_15m && matched_15m);

    let bal_before_r15 = ctx.db.with_snapshot(|s| s.characters[&pilot_a].balance.0);
    ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_id)], &kw, None).await.unwrap();
    for _ in 0..6 {
        let _ = sys_h.step_manual(1.0).await;
    }
    ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_id)], &kw, None).await.unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&pilot_a].balance.0) - bal_before_r15, 300_000);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssReservePulseCooldown"
    );

    ctx.db.clock().advance_secs(61);
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&sys_id).unwrap().ess_reserve_bank = IskCents(4_500_000);
    });
    let mut matched_45m = false;
    for (idx, &tid) in keys_45m.iter().enumerate() {
        let iid = 960_620 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(iid, make_item(iid, tid, pilot_a, ship_a, InventoryFlag::Cargo, 1, false));
        });
        if let Ok(rep) = ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(iid)], &kw, None).await {
            assert_eq!(kv_map(&rep)["durationMinutes"].as_i64(), Some(45));
            matched_45m = true;
        }
    }
    assert!(matched_45m);

    let bal_before_r45 = ctx.db.with_snapshot(|s| s.characters[&pilot_a].balance.0);
    ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_id)], &kw, None).await.unwrap();
    for _ in 0..6 {
        let _ = sys_h.step_manual(1.0).await;
    }
    ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_id)], &kw, None).await.unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&pilot_a].balance.0) - bal_before_r45, 100_000);
}
