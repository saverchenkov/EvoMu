//! Integration tests for Batch 36 open `feat:pve.*` parity rows (part 5: ESS Main Bank autopayout,
//! 1 Hz ballpark NPC ESS pooling, & SQLite-durable server-tick Reserve Bank key tiers).

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

fn count_char_event(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    let all = ctx.drain_notifications_for_char(char_id);
    let mut matched = Vec::new();
    let mut keep = Vec::new();
    for n in all {
        if n.event == event {
            matched.push(n.args);
        } else {
            keep.push(n);
        }
    }
    if !keep.is_empty() {
        if let Ok(mut guard) = ctx.pending_notifications.write() {
            guard.entry(char_id).or_default().extend(keep);
        }
    }
    matched
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
        .filter(|s| {
            s.security_status <= -0.1
                && s.solar_system_id < 31_000_000
                && ctx
                    .sde
                    .celestials_in_system(s.solar_system_id)
                    .iter()
                    .any(|c| c.group_id == 9)
        })
        .min_by_key(|s| s.solar_system_id)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);

    let pilot_a = 100_001_i64;
    let pilot_b = 100_002_i64;
    let pilot_c = 100_003_i64;
    let ship_a = 965_001_i64;
    let ship_b = 965_002_i64;
    let ship_c = 965_003_i64;
    let wep_a = 965_015_i64;
    let ammo_a = 965_016_i64;
    let wep_b = 965_017_i64;
    let ammo_b = 965_018_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.values().next().cloned().expect("base char");
        for (pid, uid, sid) in [(pilot_a, 1_u64, ship_a), (pilot_b, 2, ship_b), (pilot_c, 3, ship_c)] {
            let mut ch = base.clone();
            ch.character_id = pid;
            ch.user_id = uid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.active_ship_id = sid;
            ch.balance = IskCents(0);
            snap.characters.insert(pid, ch);
            snap.items.insert(sid, make_item(sid, 639, pid, i64::from(sys_id), InventoryFlag::None, 1, true));
        }

        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.sov_alliance_id = Some(99_000_001);
        st.dbs_multiplier = 1.35;
        st.ess_main_bank = IskCents(0);
        st.ess_reserve_bank = IskCents(0);

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

    for (sid, pos) in [
        (ship_a, ess_pos + DVec3::new(1_500.0, 0.0, 0.0)),
        (ship_b, rat_spot),
        (ship_c, rat_spot + DVec3::new(50_000.0, 0.0, 0.0)),
    ] {
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

    // 1a (`F-04` regression): 1 Hz `NpcSpawnManager::handle_npc_destroyed` in null-sec splits bounties into
    // `ess_main_bank` + `ess_reserve_bank` and records `pilot_c` in `ess_main_bank_contributors`!
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let first_belt_rat = ctx.npc_spawns.read().unwrap()[&sys_id]
        .entities
        .values()
        .filter(|e| e.bounty_isk > 0.0)
        .min_by_key(|e| e.ball_id)
        .expect("spawned null-sec belt rat with positive bounty")
        .ball_id;
    let belt_rat_bounty = ctx
        .npc_spawns
        .write()
        .unwrap()
        .get_mut(&sys_id)
        .unwrap()
        .handle_npc_destroyed(&ctx, sys_id, first_belt_rat, pilot_c, ctx.now_unix())
        .expect("1 Hz handle_npc_destroyed in null-sec");
    assert!(belt_rat_bounty > 0.0);
    let (c_bal_direct, main_from_1hz, res_from_1hz, c_contrib) = ctx.db.with_snapshot(|s| {
        let st = &s.system_states[&sys_id];
        (
            s.characters[&pilot_c].balance.0,
            st.ess_main_bank.0,
            st.ess_reserve_bank.0,
            st.ess_main_bank_contributors.get(&pilot_c).copied().unwrap_or(0),
        )
    });
    assert!(main_from_1hz > 0 && res_from_1hz > 0, "1 Hz null-sec NPC kill must pool into ESS Main & Reserve Banks");
    assert_eq!(c_contrib, main_from_1hz, "1 Hz null-sec NPC kill must record contributor share in ess_main_bank_contributors");
    assert!(c_bal_direct > 0 && (c_bal_direct as f64 / 100.0) < belt_rat_bounty * 1.35);

    // 1b. ESS Main Bank 2h45m autopayout via live NPC bounty combat (pilot_a 2 kills : pilot_b 1 kill) + active-link deferral
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

    let (main_pooled, bal_a_pre, bal_b_pre, bal_c_pre) = ctx.db.with_snapshot(|s| {
        (
            s.system_states[&sys_id].ess_main_bank.0,
            s.characters[&pilot_a].balance.0,
            s.characters[&pilot_b].balance.0,
            s.characters[&pilot_c].balance.0,
        )
    });
    assert!(main_pooled > main_from_1hz);
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

    let (main_after, gain_a, gain_b, gain_c, has_j_a, has_j_b, has_j_c) = ctx.db.with_snapshot(|s| {
        (
            s.system_states[&sys_id].ess_main_bank.0,
            s.characters[&pilot_a].balance.0 - bal_a_pre,
            s.characters[&pilot_b].balance.0 - bal_b_pre,
            s.characters[&pilot_c].balance.0 - bal_c_pre,
            s.wallet_journal.iter().any(|j| j.character_id == pilot_a && j.entry_type == "ESSMainBankPayout"),
            s.wallet_journal.iter().any(|j| j.character_id == pilot_b && j.entry_type == "ESSMainBankPayout"),
            s.wallet_journal.iter().any(|j| j.character_id == pilot_c && j.entry_type == "ESSMainBankPayout"),
        )
    });
    assert_eq!(main_after, 0);
    assert_eq!(gain_a + gain_b + gain_c, main_pooled);
    assert!((gain_a - 2 * gain_b).abs() * 1_000 <= gain_a);
    assert!((gain_c - main_from_1hz).abs() <= 2);
    assert!(has_j_a && has_j_b && has_j_c);

    // 2. ESS Reserve Bank key tiers, duplicate-unlock guard, SQLite durability, & server-tick progressive streaming (`F-03`)!
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
    let (mut wrong_quad_15m, mut matched_15m_tid) = (false, 0_u32);
    for (idx, &tid) in keys_15m.iter().enumerate() {
        let iid = 965_610 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(iid, make_item(iid, tid, pilot_a, ship_a, InventoryFlag::Cargo, 1, false));
        });
        if matched_15m_tid > 0 {
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.remove(&iid);
            });
            continue;
        }
        match ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(iid)], &kw, None).await {
            Ok(rep) => {
                assert_eq!(kv_map(&rep)["durationMinutes"].as_i64(), Some(15));
                assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&iid)));
                let char_notifs = count_char_event(&ctx, pilot_a, "OnReserveBankUnlock");
                let sess_notifs: Vec<Vec<PyRep>> = sess_a
                    .drain_notifications()
                    .into_iter()
                    .filter(|(n, _)| n == "OnReserveBankUnlock")
                    .map(|(_, a)| a)
                    .collect();
                assert_eq!(char_notifs.len(), 1);
                assert_eq!(sess_notifs.len(), 1);
                assert!(sess_notifs[0][0].dict_get("regionID").is_some());
                matched_15m_tid = tid;
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
    if !wrong_quad_15m {
        let wrong_tid = keys_15m.iter().copied().find(|&t| t != matched_15m_tid).unwrap();
        let wrong_iid = 965_619_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(wrong_iid, make_item(wrong_iid, wrong_tid, pilot_a, ship_a, InventoryFlag::Cargo, 1, false));
        });
        // While already unlocked, duplicate RequestUnlockReserveBank rejects BEFORE consuming any key!
        assert_eq!(
            ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(wrong_iid)], &kw, None)
                .await
                .unwrap_err()
                .code,
            "EssReserveBankAlreadyUnlocked"
        );
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&wrong_iid);
        });
    }
    assert!(matched_15m_tid > 0);

    // Duplicate `RequestUnlockReserveBank` while unlocked is rejected without consuming a second valid key (`F-03`)!
    let spare_key_iid = 965_650_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(spare_key_iid, make_item(spare_key_iid, matched_15m_tid, pilot_a, ship_a, InventoryFlag::Cargo, 1, false));
    });
    assert_eq!(
        ctx.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(spare_key_iid)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssReserveBankAlreadyUnlocked"
    );
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&spare_key_iid)), "Duplicate unlock attempt must NOT consume spare key");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&spare_key_iid);
    });

    // SQLite restart durability (`B36-F06`): reopen SQLite into a fresh ServerContext and stream pulses from server ticks!
    let saved_now = ctx.now_unix();
    let ctx2 = Arc::new(crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "b36_ess_reserve_unlock"));
    ctx2.set_clock(Arc::new(VirtualServerClock::with_fixed_base(saved_now)));
    crate::common::login_pilot(&ctx2, &mut sess_a, "demo", "demo-secret").await;
    ctx2.dispatch_rpc(
        &mut sess_a,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(pilot_a)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID pilot_a on reopened ctx2");
    let _ = ctx2.drain_notifications_for_char(pilot_a);
    let _ = sess_a.drain_notifications();
    sess_a.character_id = Some(pilot_a);
    sess_a.solar_system_id = Some(sys_id);
    sess_a.ship_id = Some(ship_a);

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
        .expect("AttemptLinkToReserveBank initiates link after SQLite restart without consuming another key");
    {
        let now_u = ctx2.now_unix();
        let cw = ctx2.crimewatch.read().unwrap();
        let st = cw.states.get(&pilot_a).expect("crimewatch state");
        assert!(st.suspect_flag_until_unix >= now_u + 900);
        assert!(st.weapon_timer_until_unix >= now_u + 60);
    }

    // Duplicate unlock while actively linked is ALSO rejected without resetting `reserve_linked_character_id` (`F-03`)!
    ctx2.db.with_mut_snapshot(|snap| {
        snap.items.insert(spare_key_iid, make_item(spare_key_iid, matched_15m_tid, pilot_a, ship_a, InventoryFlag::Cargo, 1, false));
    });
    assert_eq!(
        ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(spare_key_iid)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "EssReserveBankAlreadyUnlocked"
    );
    assert_eq!(
        ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "IsClientLinkedToReserveBank", &[], &kw, None).await.unwrap(),
        PyRep::Bool(true)
    );
    ctx2.db.with_mut_snapshot(|snap| {
        snap.items.remove(&spare_key_iid);
    });
    let _ = count_char_event(&ctx2, pilot_a, "OnReserveBankPlayerLinkDisconnected");

    // Stream Pulse 1 & Pulse 2 purely from `tick_background_cluster` (NO manual `AttemptLinkToReserveBank` polling!)
    ctx2.db.clock().advance_secs(60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx2).await;
    let pulse1 = ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0) - bal_before_r15;
    assert!(pulse1 > 0, "Server tick must stream Reserve Bank pulse 1 automatically");
    assert!(ctx2.db.with_snapshot(|s| s.wallet_journal.iter().any(|j| j.character_id == pilot_a && j.entry_type == "ESSReserveBankPayout")));
    assert!(
        count_char_event(&ctx2, pilot_a, "OnReserveBankPlayerLinkDisconnected").is_empty(),
        "Intermediate pulse 1 must NOT emit OnReserveBankPlayerLinkDisconnected"
    );
    assert_eq!(
        ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "IsClientLinkedToReserveBank", &[], &kw, None).await.unwrap(),
        PyRep::Bool(true)
    );

    ctx2.db.clock().advance_secs(60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx2).await;
    let pulse2 = (ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0) - bal_before_r15) - pulse1;
    assert!(pulse2 > pulse1, "Reserve Bank payouts must progressively spool up (pulse2={pulse2} > pulse1={pulse1})");
    assert!(
        count_char_event(&ctx2, pilot_a, "OnReserveBankPlayerLinkDisconnected").is_empty(),
        "Intermediate pulse 2 must NOT emit OnReserveBankPlayerLinkDisconnected"
    );

    // Moving beyond 2,500 m severs the Reserve Bank link on the next server tick and emits exactly 1 disconnect (`reason == 1`)!
    let _ = sys_h2.set_ball_position(ship_a, ess_pos2 + DVec3::new(3_200.0, 0.0, 0.0)).await;
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx2).await;
    let range_disc = count_char_event(&ctx2, pilot_a, "OnReserveBankPlayerLinkDisconnected");
    assert_eq!(range_disc.len(), 1, "Range break must emit exactly 1 OnReserveBankPlayerLinkDisconnected");
    assert_eq!(range_disc[0][0].dict_get("reason"), Some(&PyRep::Integer(1)));
    assert_eq!(
        ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "IsClientLinkedToReserveBank", &[], &kw, None).await.unwrap(),
        PyRep::Bool(false)
    );

    // Move back within 2,500 m, re-link during remaining window, and stream remaining 13 pulses via `tick_background_cluster`!
    let _ = sys_h2.set_ball_position(ship_a, ess_pos2 + DVec3::new(500.0, 0.0, 0.0)).await;
    ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_id)], &kw, None)
        .await
        .expect("Re-link during remaining 15m Reserve Bank window");
    let mut per_pulse = vec![pulse1, pulse2];
    for step in 0..13 {
        let before = ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0);
        ctx2.db.clock().advance_secs(60);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx2).await;
        let gained = ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0) - before;
        per_pulse.push(gained);
        if step < 12 {
            assert!(count_char_event(&ctx2, pilot_a, "OnReserveBankPlayerLinkDisconnected").is_empty());
        }
    }
    assert!(per_pulse[11] > per_pulse[0] && per_pulse[11] > per_pulse[14], "Reserve Bank curve must spool up to peak and drop down");
    assert_eq!(ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0) - bal_before_r15, 4_500_000);
    assert_eq!(ctx2.db.with_snapshot(|s| s.system_states[&sys_id].ess_reserve_bank.0), 0);
    let final_disc = count_char_event(&ctx2, pilot_a, "OnReserveBankPlayerLinkDisconnected");
    assert_eq!(final_disc.len(), 1, "Terminal pulse 15 must emit exactly 1 OnReserveBankPlayerLinkDisconnected");
    assert_eq!(final_disc[0][0].dict_get("reason"), Some(&PyRep::Integer(0)));

    // 45m Reserve Security Key tier unlocks cleanly once prior 15m window has terminated, and streams progressive pulse 1!
    ctx2.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&sys_id).unwrap().ess_reserve_bank = IskCents(4_500_000);
    });
    let mut matched_45m = false;
    for (idx, &tid) in keys_45m.iter().enumerate() {
        let iid = 965_620 + idx as i64;
        ctx2.db.with_mut_snapshot(|snap| {
            snap.items.insert(iid, make_item(iid, tid, pilot_a, ship_a, InventoryFlag::Cargo, 1, false));
        });
        if matched_45m {
            ctx2.db.with_mut_snapshot(|snap| {
                snap.items.remove(&iid);
            });
            continue;
        }
        if let Ok(rep) = ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "RequestUnlockReserveBank", &[PyRep::Integer(iid)], &kw, None).await {
            assert_eq!(kv_map(&rep)["durationMinutes"].as_i64(), Some(45));
            matched_45m = true;
        } else {
            ctx2.db.with_mut_snapshot(|snap| {
                snap.items.remove(&iid);
            });
        }
    }
    assert!(matched_45m);

    let bal_before_r45 = ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0);
    ctx2.dispatch_rpc(&mut sess_a, "essMgr", None, "AttemptLinkToReserveBank", &[PyRep::Integer(ess_id)], &kw, None).await.unwrap();
    ctx2.db.clock().advance_secs(60);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx2).await;
    let expected_45_p1 = eve_server::services::pve_open_feats_b36::compute_ess_reserve_pulse_payout_cents(4_500_000, 45, 45);
    assert_eq!(ctx2.db.with_snapshot(|s| s.characters[&pilot_a].balance.0) - bal_before_r45, expected_45_p1);
}
