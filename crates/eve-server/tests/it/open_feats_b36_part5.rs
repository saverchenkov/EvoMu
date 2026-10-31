//! Integration tests for Batch 36 open `feat:pve.*` parity rows (part 5: ESS Main Bank autopayout & SQLite-durable Reserve Bank key tiers).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, VirtualServerClock};
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

#[tokio::test]
async fn test_b36_ess_main_autopayout_and_reserve_bank_key_tiers() {
    // parity: feat:pve.ess-main-bank-autopayout, feat:pve.ess-reserve-bank-key-tiers
    eve_testkit::attest(&[
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
        .find(|s| s.security_status <= -0.1 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);

    let pilot_a = 100_001_i64;
    let pilot_b = 100_002_i64;
    let ship_a = 965_001_i64;
    let ship_b = 965_002_i64;
    let wep_a = 965_015_i64;
    let ammo_a = 965_016_i64;
    let wep_b = 965_017_i64;
    let ammo_b = 965_018_i64;

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
        snap.items.insert(wep_a, make_item(wep_a, 2961, pilot_a, ship_a, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(ammo_a, make_item(ammo_a, 21894, pilot_a, ship_a, InventoryFlag::HiSlot0, 5_000, false));
        snap.items.insert(wep_b, make_item(wep_b, 2961, pilot_b, ship_b, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(ammo_b, make_item(ammo_b, 21894, pilot_b, ship_b, InventoryFlag::HiSlot0, 5_000, false));
        snap.module_online_states.insert(wep_a, true);
        snap.module_online_states.insert(wep_b, true);
        for pid in [pilot_a, pilot_b] {
            let sk = snap.skills.entry(pid).or_default();
            for sid in [3300_u32, 3302, 3305, 3308, 3310, 3311, 3312, 3315, 3316, 3318, 3334, 3413, 3418, 3426, 20327, 21667] {
                sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 1_280_000.0 });
            }
            for tid in [639_u32, 2961, 21894] {
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

    let mut sess_a = SessionState::new(3614);
    crate::common::login_pilot(&ctx, &mut sess_a, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess_a,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(pilot_a)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID pilot_a");
    let _ = ctx.drain_notifications_for_char(pilot_a);
    let _ = sess_a.drain_notifications();
    sess_a.character_id = Some(pilot_a);
    sess_a.solar_system_id = Some(sys_id);
    sess_a.ship_id = Some(ship_a);

    let mut sess_b = SessionState::new(3615);
    sess_b.user_id = Some(2);
    sess_b.character_id = Some(pilot_b);
    sess_b.solar_system_id = Some(sys_id);
    sess_b.ship_id = Some(ship_b);

    let ess_info = kv_map(
        &ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "GetDataForClientSolarSystem", &[], &kw, None)
            .await
            .unwrap(),
    );
    let ess_id = ess_info["essID"].as_i64().unwrap();
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let ess_pos = sys_h.get_ball_snapshot(ess_id).await.unwrap().unwrap().1.position;
    let rat_spot = DVec3::new(-5.0e9, 0.0, 0.0);

    for (sid, pos) in [(ship_a, ess_pos + DVec3::new(1_500.0, 0.0, 0.0)), (ship_b, rat_spot)] {
        let _ = sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: sid,
                    type_id: 639,
                    position: pos,
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
    }

    // 1. ESS Main Bank 2h45m autopayout via live NPC bounty combat (2:1 ratio) + active-link deferral
    let bounty_npc_tid = ctx
        .sde
        .types
        .values()
        .filter(|t| {
            t.category_id == 11
                && (100_000.0..=250_000.0)
                    .contains(&t.default_attributes.get(&481).copied().unwrap_or(0.0))
        })
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap();

    for (idx, (is_a, npc_bid)) in [(true, 965_501_i64), (true, 965_502), (false, 965_503)].into_iter().enumerate() {
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
                &ctx.dispatch_rpc(s_ref, "dogmaIM", None, "Activate", &[PyRep::Integer(w_id), PyRep::Integer(npc_bid)], &kw, None)
                    .await
                    .unwrap(),
            );
            if r.get("destroyed").and_then(|v| v.as_bool()) == Some(true) {
                break;
            }
            let _ = sys_h.step_manual(1.0).await;
        }
        assert!(sys_h.get_ball_snapshot(npc_bid).await.unwrap().is_none());
    }

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
        .dispatch_rpc(&mut sess_a, "essMgr", None, "GetDataForClientSolarSystem", &[], &kw, None)
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
    assert!((gain_a - 2 * gain_b).abs() * 1_000 <= gain_a);
    assert!(has_j_a && has_j_b);

    // 2. ESS Reserve Bank key tiers (15m vs 45m quadrant keys), single OnReserveBankUnlock delivery, and SQLite restart durability!
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&sys_id).unwrap().ess_reserve_bank = IskCents(4_500_000);
        snap.item_positions.insert(ship_a, (ess_pos + DVec3::new(500.0, 0.0, 0.0)).to_array());
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
    let _ = ctx.drain_notifications_for_char(pilot_a);
    let _ = sess_a.drain_notifications();

    let keys_15m = [57714_u32, 57716, 57717, 57720];
    let keys_45m = [57715_u32, 57719, 57718, 57721];
    let (mut wrong_quad_15m, mut matched_15m) = (false, false);
    for (idx, &tid) in keys_15m.iter().enumerate() {
        let iid = 965_610 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(iid, make_item(iid, tid, pilot_a, ship_a, InventoryFlag::Cargo, 1, false));
        });
        match ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(iid)], &kw, None).await {
            Ok(rep) => {
                assert_eq!(kv_map(&rep)["durationMinutes"].as_i64(), Some(15));
                assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&iid)));
                let char_notifs: Vec<Vec<PyRep>> = ctx
                    .drain_notifications_for_char(pilot_a)
                    .into_iter()
                    .filter(|n| n.event == "OnReserveBankUnlock")
                    .map(|n| n.args)
                    .collect();
                let sess_notifs: Vec<Vec<PyRep>> = sess_a
                    .drain_notifications()
                    .into_iter()
                    .filter(|(n, _)| n == "OnReserveBankUnlock")
                    .map(|(_, a)| a)
                    .collect();
                assert_eq!(char_notifs.len(), 1);
                assert_eq!(
                    sess_notifs.len(),
                    1,
                    "OnReserveBankUnlock must be delivered once to session (no duplicate push)"
                );
                assert!(sess_notifs[0][0].dict_get("regionID").is_some());
                matched_15m = true;
            }
            Err(e) => {
                assert_eq!(e.code, "EssReserveKeyWrongQuadrant");
                ctx.db.with_mut_snapshot(|snap| {
                    snap.items.remove(&iid);
                });
                wrong_quad_15m = true;
            }
        }
    }
    assert!(wrong_quad_15m && matched_15m);

    // SQLite restart durability (`B36-F06`): reopen SQLite into a fresh ServerContext (empty CombatRuntimeState)
    // and verify Reserve Bank unlock window + pulses survive and allow AttemptLinkToReserveBank with zero keys in cargo!
    let saved_now = ctx.now_unix();
    let ctx2 = Arc::new(crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "b36_ess_reserve_unlock"));
    ctx2.set_clock(Arc::new(VirtualServerClock::with_fixed_base(saved_now)));

    let ess_info_re = kv_map(
        &ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "GetDataForClientSolarSystem", &[], &kw, None)
            .await
            .unwrap(),
    );
    assert_eq!(ess_info_re["reserveBankPulsesRemaining"].as_i64(), Some(15));
    let details_re = kv_map(
        &ctx2.dispatch_rpc(&mut sess_a, "dynamicResourceCacheMgr", None, "GetESSSystemDetails", &[], &kw, None)
            .await
            .unwrap(),
    );
    assert_eq!(details_re["reserveTimeWindowState"].as_i64(), Some(2));

    let sys_h2 = ctx2.systems.get_or_spawn(sys_id);
    let ess_pos2 = sys_h2.get_ball_snapshot(ess_id).await.unwrap().unwrap().1.position;
    let _ = sys_h2
        .spawn_ball_with_combat(
            Ball {
                id: ship_a,
                type_id: 639,
                position: ess_pos2 + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx2.build_type_combat_state(639),
        )
        .await;

    let bal_before_r15 = ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0);
    ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_id)], &kw, None)
        .await
        .expect("AttemptLinkToReserveBank succeeds after SQLite restart without consuming another key");
    {
        let now_u = ctx2.now_unix();
        let cw = ctx2.crimewatch.read().unwrap();
        let st = cw.states.get(&pilot_a).expect("crimewatch state");
        assert!(st.suspect_flag_until_unix >= now_u + 900);
        assert!(st.weapon_timer_until_unix >= now_u + 60);
    }
    for _ in 0..6 {
        let _ = sys_h2.step_manual(1.0).await;
    }
    ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_id)], &kw, None)
        .await
        .unwrap();
    assert_eq!(ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0) - bal_before_r15, 300_000);
    assert!(ctx2.db.with_snapshot(|s| s.wallet_journal.iter().any(|j| j.character_id == pilot_a && j.entry_type == "ESSReserveBankPayout")));
    assert_eq!(
        ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "IsClientLinkedToReserveBank", &[], &kw, None).await.unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_id)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssReservePulseCooldown"
    );

    ctx2.db.clock().advance_secs(61);
    let pulse2_rep = kv_map(
        &ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_id)], &kw, None)
            .await
            .expect("Second Reserve Bank pulse on persistent link"),
    );
    assert_eq!(pulse2_rep.get("linkStartedOnly"), Some(&PyRep::Bool(false)));
    assert_eq!(ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0) - bal_before_r15, 600_000);

    // 45m Reserve Security Key tier -> 100,000 IskCents per pulse (4_500_000 / 45)
    ctx2.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&sys_id).unwrap().ess_reserve_bank = IskCents(4_500_000);
    });
    let mut matched_45m = false;
    for (idx, &tid) in keys_45m.iter().enumerate() {
        let iid = 965_620 + idx as i64;
        ctx2.db.with_mut_snapshot(|snap| {
            snap.items.insert(iid, make_item(iid, tid, pilot_a, ship_a, InventoryFlag::Cargo, 1, false));
        });
        if let Ok(rep) = ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(iid)], &kw, None).await {
            assert_eq!(kv_map(&rep)["durationMinutes"].as_i64(), Some(45));
            matched_45m = true;
        }
    }
    assert!(matched_45m);

    let bal_before_r45 = ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0);
    ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_id)], &kw, None).await.unwrap();
    for _ in 0..6 {
        let _ = sys_h2.step_manual(1.0).await;
    }
    ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_id)], &kw, None).await.unwrap();
    assert_eq!(ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0) - bal_before_r45, 100_000);
}
