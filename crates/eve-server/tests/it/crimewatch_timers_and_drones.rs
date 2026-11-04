//! Integration tests for CrimeWatch timers, authentic security status loss and gain caps,
//! 15-minute interval system bands, kill rights lifecycle, and Clone Soldier tag tier bands (`C-8`).
//!
//! Remediates the 6 CrimeWatch parity features:
//! - `feat:crimewatch.sec-status-loss`
//! - `feat:crimewatch.sec-status-transactions`
//! - `feat:crimewatch.kill-rights-gain`
//! - `feat:crimewatch.kill-rights-sell`
//! - `feat:crimewatch.sec-status-gain-caps`
//! - `feat:crimewatch.sec-tag-tier-bands`

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::crimewatch::{
    compute_sec_status_loss, credit_specialized_npc_bounty_and_sec_status,
};
use eve_server::router::ServerContext;
use glam::DVec3;

fn field(rep: &PyRep, key: &str) -> PyRep {
    rep.dict_get(key).cloned().unwrap_or(PyRep::None)
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, String> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
        .map_err(|e| format!("{}: {}", e.code, e.message))
}

fn spawn_ship(ctx: &ServerContext, owner_id: i64, sys_id: u32, type_id: u32, _pos: DVec3) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let sid = snap.next_entity_id;
        snap.items.insert(sid, InventoryItemRecord {
            item_id: sid, type_id, owner_id, location_id: sys_id as i64, flag: InventoryFlag::None,
            quantity: 1, singleton: true, ..Default::default()
        });
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.active_ship_id = sid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
        }
        sid
    })
}

#[tokio::test]
async fn test_crimewatch_sec_status_loss_and_kill_rights_lifecycle_e2e() {
    // parity: feat:crimewatch.sec-status-loss, feat:crimewatch.kill-rights-gain, feat:crimewatch.kill-rights-sell, feat:crimewatch.sec-status-transactions
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();

    let highsec_sys = 30_000_142_u32; // Jita (0.95)
    let lowsec_sys = 30_000_012_u32;  // Asabona (0.32)
    let jita_sec = ctx.sde.solar_systems[&highsec_sys].security_status;
    let lowsec_sec = ctx.sde.solar_systems[&lowsec_sys].security_status;

    let pirate_id = 90_000_501_i64;
    let victim_id = 90_000_502_i64;
    let corp_mate_id = 90_000_503_i64;
    let outsider_id = 90_000_504_i64;
    let victim2_id = 90_000_505_i64;
    let target_corp_id = 2_000_101_i64;
    let other_corp_id = 2_000_102_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for &(cid, uid, name, sec, bal, corp) in &[
            (pirate_id, 501, "Pirate Pilot", 0.0, 50_000_000.0, other_corp_id),
            (victim_id, 502, "Victim Pilot", 5.0, 10_000_000.0, target_corp_id),
            (corp_mate_id, 503, "Corp Mate", 0.0, 30_000_000.0, target_corp_id),
            (outsider_id, 504, "Outsider Pilot", 0.0, 30_000_000.0, other_corp_id),
            (victim2_id, 505, "Victim Pod", 5.0, 10_000_000.0, target_corp_id),
        ] {
            if let Some(mut ch) = snap.characters.get(&90_000_001).cloned() {
                ch.character_id = cid;
                ch.user_id = uid;
                ch.name = name.to_string();
                ch.security_status = sec;
                ch.balance = IskCents::from_f64_isk(bal);
                ch.corporation_id = corp;
                ch.solar_system_id = highsec_sys;
                ch.station_id = None;
                snap.characters.insert(cid, ch);
            }
        }
        for (corp_id, name, ticker, mems) in [
            (target_corp_id, "Target Corp", "TGT", &[victim_id, corp_mate_id, victim2_id][..]),
            (other_corp_id, "Other Corp", "OTH", &[pirate_id, outsider_id][..]),
        ] {
            snap.corporations.insert(corp_id, CorporationRecord {
                corporation_id: corp_id, name: name.into(), ticker: ticker.into(),
                members: mems.iter().copied().collect(), ..Default::default()
            });
        }
    });

    // 1. Exact mathematical formula verification for security status loss (`feat:crimewatch.sec-status-loss`)
    // Formula: Mod% = -2.5% (ship) or -25% (pod) * System sec * (1 + (Target sec - Current sec) / 100)
    let loss_ship = compute_sec_status_loss(0.0, 5.0, false, jita_sec);
    let expected_ship_loss = (10.0 - 0.0) * 0.025 * jita_sec * (1.0 + (5.0 - 0.0) / 100.0);
    assert!(
        (loss_ship - expected_ship_loss).abs() < 1e-6,
        "Ship loss formula must match canonical EVE calculation: got {loss_ship}, expected {expected_ship_loss}"
    );

    let loss_pod = compute_sec_status_loss(-2.0, 5.0, true, lowsec_sec);
    let expected_pod_loss = (10.0 - (-2.0)) * 0.25 * lowsec_sec * (1.0 + (5.0 - (-2.0)) / 100.0);
    assert!(
        (loss_pod - expected_pod_loss).abs() < 1e-6,
        "Pod loss formula must match canonical EVE calculation: got {loss_pod}, expected {expected_pod_loss}"
    );

    // Nullsec yields zero sec loss, and at/below -10.0 yields zero further loss
    assert_eq!(compute_sec_status_loss(0.0, 5.0, false, 0.0), 0.0);
    assert_eq!(compute_sec_status_loss(0.0, 5.0, true, -0.5), 0.0);
    assert_eq!(compute_sec_status_loss(-10.0, 5.0, true, 0.95), 0.0);

    // 2. Highsec unprovoked attack immediately awards a 30-day Kill Right (`feat:crimewatch.kill-rights-gain`)
    let crim_ship = spawn_ship(&ctx, pirate_id, highsec_sys, 587, DVec3::ZERO);
    let vic_ship = spawn_ship(&ctx, victim_id, highsec_sys, 587, DVec3::new(1_000.0, 0.0, 0.0));
    let mate_ship = spawn_ship(&ctx, corp_mate_id, highsec_sys, 587, DVec3::new(2_000.0, 0.0, 0.0));
    let out_ship = spawn_ship(&ctx, outsider_id, highsec_sys, 587, DVec3::new(3_000.0, 0.0, 0.0));

    let jita_handle = ctx.systems.get_or_spawn(highsec_sys);
    for (id, pos) in [
        (crim_ship, DVec3::ZERO),
        (vic_ship, DVec3::new(1_000.0, 0.0, 0.0)),
        (mate_ship, DVec3::new(2_000.0, 0.0, 0.0)),
        (out_ship, DVec3::new(3_000.0, 0.0, 0.0)),
    ] {
        let mut cs = ctx.build_type_combat_state(587);
        cs.shield_hp = 500.0;
        let _ = jita_handle
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

    let kr_count_pre = ctx.db.with_snapshot(|s| s.kill_rights.len());
    let aggr_res = eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, pirate_id, crim_ship, highsec_sys, victim_id, false,
    );
    assert!(aggr_res.is_ok(), "evaluate_and_record_pvp_aggression in highsec must succeed");

    // Re-attack within same engagement must not double-penalize
    let aggr_repeat = eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, pirate_id, crim_ship, highsec_sys, victim_id, false,
    );
    assert!(aggr_repeat.is_ok());

    let (kr_count_post_hs, hs_kr_id) = ctx.db.with_snapshot(|s| {
        let kr = s
            .kill_rights
            .values()
            .find(|k| k.owner_id == victim_id && k.target_id == pirate_id)
            .expect("Kill right must exist immediately upon high-sec aggression");
        (s.kill_rights.len(), kr.kill_right_id)
    });
    assert_eq!(
        kr_count_post_hs,
        kr_count_pre + 1,
        "Highsec unprovoked aggression must immediately grant a valid kill right"
    );

    let sec_after_agg = ctx.db.with_snapshot(|s| s.characters[&pirate_id].security_status);
    let expected_sec = 0.0 - expected_ship_loss;
    assert!(
        (sec_after_agg - expected_sec).abs() < 1e-6,
        "Character security status must decrease by exact formula loss: got {sec_after_agg}, expected {expected_sec}"
    );

    let kr_exp = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.kill_right_expires_unix.get(&hs_kr_id).copied()
    })
    .expect("Kill right expiry recorded");
    assert_eq!(kr_exp, now + 30 * 86_400, "Kill right must have exactly 30-day expiration");

    // 3. Lowsec Rules: ship aggression gives Suspect & NO kill right; pod aggression awards kill right
    let lowsec_handle = ctx.systems.get_or_spawn(lowsec_sys);
    let low_crim_ship = spawn_ship(&ctx, pirate_id, lowsec_sys, 587, DVec3::ZERO);
    let low_vic_ship = spawn_ship(&ctx, victim_id, lowsec_sys, 587, DVec3::new(500.0, 0.0, 0.0));
    let low_vic_pod = spawn_ship(&ctx, victim2_id, lowsec_sys, 670, DVec3::new(600.0, 0.0, 0.0));

    for (id, tid, pos) in [
        (low_crim_ship, 587, DVec3::ZERO),
        (low_vic_ship, 587, DVec3::new(500.0, 0.0, 0.0)),
        (low_vic_pod, 670, DVec3::new(600.0, 0.0, 0.0)),
    ] {
        let cs = ctx.build_type_combat_state(tid);
        let _ = lowsec_handle
            .spawn_ball_with_combat(
                Ball {
                    id,
                    type_id: tid,
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

    let kr_count_pre_ls = ctx.db.with_snapshot(|s| s.kill_rights.len());
    let _ = eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, pirate_id, low_crim_ship, lowsec_sys, victim_id, false,
    );
    let kr_count_after_ls_ship = ctx.db.with_snapshot(|s| s.kill_rights.len());
    assert_eq!(
        kr_count_after_ls_ship, kr_count_pre_ls,
        "Lowsec ship aggression must not award a kill right"
    );

    let _ = eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, pirate_id, low_crim_ship, lowsec_sys, victim2_id, true,
    );
    let (kr_count_after_ls_pod, low_pod_kr_id) = ctx.db.with_snapshot(|s| {
        let kr = s
            .kill_rights
            .values()
            .find(|k| k.owner_id == victim2_id && k.target_id == pirate_id)
            .unwrap();
        (s.kill_rights.len(), kr.kill_right_id)
    });
    assert_eq!(
        kr_count_after_ls_pod,
        kr_count_pre_ls + 1,
        "Lowsec pod aggression must award a kill right"
    );
    let low_kr_exp = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.kill_right_expires_unix.get(&low_pod_kr_id).copied()
    })
    .unwrap();
    assert_eq!(low_kr_exp, now + 30 * 86_400, "Lowsec pod kill right carries 30-day expiration");

    // 4. Kill Rights Selling, Canceling, Restricting to Corporation, and Buying (`feat:crimewatch.kill-rights-sell`)
    let mut sess_vic = SessionState::default();
    sess_vic.user_id = Some(502);
    sess_vic.character_id = Some(victim_id);
    sess_vic.solar_system_id = Some(highsec_sys);

    // Victim lists kill right publicly for 2,000,000 ISK
    let list_res = rpc(
        &ctx,
        &mut sess_vic,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(hs_kr_id), PyRep::Real(2_000_000.0)],
    )
    .await
    .expect("SellKillRight public");
    if let PyRep::List(items) = list_res {
        assert_eq!(field(&items[0], "forSale"), PyRep::Bool(true));
        assert_eq!(field(&items[0], "price"), PyRep::Real(2_000_000.0));
    } else {
        panic!("expected List from SellKillRight");
    }

    // Cancel sale
    let cancel_res = rpc(
        &ctx,
        &mut sess_vic,
        "bountyProxy",
        "CancelSellKillRight",
        &[PyRep::Integer(hs_kr_id)],
    )
    .await
    .expect("CancelSellKillRight");
    assert_eq!(field(&cancel_res, "forSale"), PyRep::Bool(false));
    assert_eq!(ctx.db.with_snapshot(|s| s.kill_rights[&hs_kr_id].activation_fee.0), 0);

    // Re-list with restriction to corporation ID with 3,000,000 ISK fee
    let restr_res = rpc(
        &ctx,
        &mut sess_vic,
        "bountyProxy",
        "SellKillRight",
        &[
            PyRep::Integer(hs_kr_id),
            PyRep::Real(3_000_000.0),
            PyRep::Integer(target_corp_id),
        ],
    )
    .await
    .expect("SellKillRight corp-restricted");
    if let PyRep::List(items) = restr_res {
        assert_eq!(field(&items[0], "forSale"), PyRep::Bool(true));
        assert_eq!(field(&items[0], "price"), PyRep::Real(3_000_000.0));
        assert_eq!(field(&items[0], "restrictedTo"), PyRep::Integer(target_corp_id));
    }

    // Outsider pilot is refused purchase with KillRightRestricted
    let mut sess_out = SessionState::default();
    sess_out.user_id = Some(504);
    sess_out.character_id = Some(outsider_id);
    sess_out.solar_system_id = Some(highsec_sys);

    let out_err = rpc(&ctx, &mut sess_out, "killRightMgr", "BuyKillRight", &[PyRep::Integer(hs_kr_id)])
        .await
        .unwrap_err();
    assert!(
        out_err.contains("KillRightRestricted"),
        "Outsider pilot must receive KillRightRestricted error: got {out_err}"
    );

    // Ensure pirate pilot is undocked in highsec for highsec kill right purchase
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&pirate_id).unwrap().solar_system_id = highsec_sys;
    });

    // Corporation member buys and activates the kill right
    let mut sess_mate = SessionState::default();
    sess_mate.user_id = Some(503);
    sess_mate.character_id = Some(corp_mate_id);
    sess_mate.solar_system_id = Some(highsec_sys);

    let buy_res = rpc(
        &ctx,
        &mut sess_mate,
        "killRightMgr",
        "BuyKillRight",
        &[PyRep::Integer(hs_kr_id)],
    )
    .await
    .expect("BuyKillRight by corp member");
    assert_eq!(field(&buy_res, "suspectFlagActive"), PyRep::Bool(true));
    assert_eq!(field(&buy_res, "feePaidISK"), PyRep::Real(3_000_000.0));

    // ISK balance transferred: buyer debited 3M, victim credited 3M
    let (vic_bal, mate_bal) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&victim_id].balance.to_wire_f64(),
            s.characters[&corp_mate_id].balance.to_wire_f64(),
        )
    });
    assert!((vic_bal - 13_000_000.0).abs() < 1e-2, "Victim balance credited 3M ISK");
    assert!((mate_bal - 27_000_000.0).abs() < 1e-2, "Buyer balance debited 3M ISK");

    // Target pilot has active 15-minute Suspect flag
    let suspect_exp = ctx.crimewatch.read().unwrap().states[&pirate_id].suspect_flag_until_unix;
    assert_eq!(
        suspect_exp,
        now + 900,
        "BuyKillRight must apply a 15-minute suspect flag to target"
    );

    // Owner direct activation of lowsec pod kill right
    ctx.db.with_mut_snapshot(|s| {
        let v2 = s.characters.get_mut(&victim2_id).unwrap();
        v2.solar_system_id = lowsec_sys;
        v2.active_ship_id = low_vic_pod;
        v2.station_id = None;

        let p = s.characters.get_mut(&pirate_id).unwrap();
        p.solar_system_id = lowsec_sys;
        p.active_ship_id = low_crim_ship;
        p.station_id = None;
    });

    let mut sess_vic2 = SessionState::default();
    sess_vic2.user_id = Some(505);
    sess_vic2.character_id = Some(victim2_id);
    sess_vic2.solar_system_id = Some(lowsec_sys);

    let act_res = rpc(&ctx, &mut sess_vic2, "killRightMgr", "ActivateKillRight", &[PyRep::Integer(low_pod_kr_id)])
        .await
        .expect("ActivateKillRight");
    assert_eq!(field(&act_res, "suspectFlagActive"), PyRep::Bool(true));
    assert_eq!(field(&act_res, "suspectFlagged"), PyRep::Bool(true));

    // 5. Audit Log: GetSecurityStatusTransactions records aggression entries (`feat:crimewatch.sec-status-transactions`)
    let mut sess_pirate = SessionState::default();
    sess_pirate.user_id = Some(501);
    sess_pirate.character_id = Some(pirate_id);
    sess_pirate.solar_system_id = Some(highsec_sys);

    let tx_res = rpc(&ctx, &mut sess_pirate, "crimewatch", "GetSecurityStatusTransactions", &[])
        .await
        .expect("GetSecurityStatusTransactions");
    let PyRep::Rowset { columns, rows, .. } = tx_res else {
        panic!("expected Rowset from GetSecurityStatusTransactions");
    };
    for col in &["eventDate", "eventTypeID", "locationID", "modification", "newValue", "otherOwnerID", "otherTypeID"] {
        assert!(columns.iter().any(|c| c == col), "missing column {col}");
    }
    assert!(!rows.is_empty(), "Transactions log must record aggression entries");
    let row0: HashMap<&str, &PyRep> = columns.iter().map(|s| s.as_str()).zip(rows[0].iter()).collect();
    assert_eq!(row0.get("eventTypeID"), Some(&&PyRep::Integer(379)), "Aggression must have eventTypeID = 379");
    assert_eq!(row0.get("locationID"), Some(&&PyRep::Integer(highsec_sys as i64)));
    let modif = match row0.get("modification") {
        Some(PyRep::Real(v)) => *v,
        other => panic!("expected Real modification, got {other:?}"),
    };
    assert!((modif - (-expected_ship_loss)).abs() < 1e-6);
    assert_eq!(row0.get("otherOwnerID"), Some(&&PyRep::Integer(victim_id)), "otherOwnerID must identify victim");
}

#[tokio::test]
async fn test_crimewatch_npc_sec_gain_caps_intervals_and_tag_bands_e2e() {
    // parity: feat:crimewatch.sec-status-gain-caps, feat:crimewatch.sec-tag-tier-bands, feat:crimewatch.sec-status-transactions
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let _now = ctx.now_unix();

    let pilot_id = 90_000_601_i64;
    let sys_low_a = 30_000_012_u32; // Asabona (sec 0.32 -> band 3)
    let sys_low_b = 30_000_134_u32; // Rancer (sec 0.16 -> band 2)

    let sec_a = ctx.sde.solar_systems[&sys_low_a].security_status;
    let sec_b = ctx.sde.solar_systems[&sys_low_b].security_status;
    let band_a = (sec_a * 10.0).round() as i32;
    let band_b = (sec_b * 10.0).round() as i32;
    assert_ne!(band_a, band_b, "Test systems must belong to different security bands");

    // Find a lowsec station offering Security Office (service 27)
    let (lowsec_station_id, lowsec_st_sys_id) = ctx
        .sde
        .stations
        .values()
        .find(|st| {
            let sec = ctx.sde.solar_systems.get(&st.solar_system_id).map(|s| s.security_status).unwrap_or(0.0);
            0.0 < sec && sec < 0.45 && eve_server::services::station_structure::npc_station_offers_service(&ctx.sde, st.station_id, 27)
        })
        .map(|st| (st.station_id, st.solar_system_id))
        .expect("low-sec Security Office station in SDE");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut ch) = snap.characters.get(&90_000_001).cloned() {
            ch.character_id = pilot_id;
            ch.user_id = 601;
            ch.name = "SecTag Pilot".to_string();
            ch.security_status = -9.5;
            ch.balance = IskCents::from_f64_isk(100_000_000.0);
            ch.station_id = Some(lowsec_station_id);
            ch.solar_system_id = lowsec_st_sys_id;
            snap.characters.insert(pilot_id, ch);
        }
        // Seed 5x of each Clone Soldier Tag type in lowsec station hangar
        for &tag_type in &[33138_u32, 33139, 33140, 33141] {
            snap.next_entity_id += 1;
            let iid = snap.next_entity_id;
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tag_type,
                    owner_id: pilot_id,
                    location_id: lowsec_station_id as i64,
                    flag: InventoryFlag::Hangar,
                    quantity: 5,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
    });

    // 1. NPC Kill Security Gain 15-Minute Interval Capping per System Band (`feat:crimewatch.sec-status-gain-caps`)
    // Pilot at 0.0 kills rat in band A -> gains sec status
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&pilot_id).unwrap().security_status = 0.0);
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_low_a, 99_000_101, pilot_id, 1_000_000.0);
    let sec_kill1 = ctx.db.with_snapshot(|s| s.characters[&pilot_id].security_status);
    assert!(sec_kill1 > 0.0, "First NPC kill in band A must award sec status increase: got {sec_kill1}");

    // Second kill in SAME system band within 15 minutes with equal/lower bounty is capped (0 gain)
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_low_a, 99_000_102, pilot_id, 500_000.0);
    let sec_kill2 = ctx.db.with_snapshot(|s| s.characters[&pilot_id].security_status);
    assert_eq!(
        sec_kill2, sec_kill1,
        "Subsequent kill within 15-minute interval in same system band must be capped"
    );

    // Kill in DIFFERENT system band B immediately awards sec status gain (independent interval)
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_low_b, 99_000_103, pilot_id, 1_000_000.0);
    let sec_band_b = ctx.db.with_snapshot(|s| s.characters[&pilot_id].security_status);
    assert!(
        sec_band_b > sec_kill2,
        "NPC kill in different system band must award gain in its own interval: got {sec_band_b}"
    );

    // Advance virtual clock past 15 minutes (900s) -> band A interval resets and awards gain again
    ctx.advance_virtual_clock(901);
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_low_a, 99_000_104, pilot_id, 1_000_000.0);
    let sec_reset = ctx.db.with_snapshot(|s| s.characters[&pilot_id].security_status);
    assert!(
        sec_reset > sec_band_b,
        "NPC kill after 15-minute interval reset must award gain: got {sec_reset}"
    );

    // NPC kill gains cap strictly at +5.0
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&pilot_id).unwrap().security_status = 4.999);
    ctx.advance_virtual_clock(901);
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_low_a, 99_000_105, pilot_id, 20_000_000.0);
    let sec_at_cap = ctx.db.with_snapshot(|s| s.characters[&pilot_id].security_status);
    assert!(
        (sec_at_cap - 5.0).abs() < 1e-6,
        "NPC pirate kills must cap at +5.0: got {sec_at_cap}"
    );

    // 2. Highsec Station Rejection for Security Tags (`feat:crimewatch.sec-tag-tier-bands`)
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&pilot_id).unwrap();
        ch.station_id = Some(60_003_760); // Jita 4-4
        ch.solar_system_id = 30_000_142;
        ch.security_status = -9.0;
    });
    let mut sess = SessionState::default();
    sess.user_id = Some(601);
    sess.character_id = Some(pilot_id);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);

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
        "Highsec station without Security Office must reject tag turn-in"
    );

    // 3. All 4 Clone Soldier Tag Bands, Fees, and Out-of-Band Refusal
    let bal_before = ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&pilot_id).unwrap();
        ch.station_id = Some(lowsec_station_id);
        ch.solar_system_id = lowsec_st_sys_id;
        ch.balance
    });
    sess.station_id = Some(lowsec_station_id);
    sess.solar_system_id = Some(lowsec_st_sys_id);

    // Tier 1 Trainer (33138): -10 to -8, 1M ISK
    // Tier 2 Recruiter (33139): -8 to -5, 2M ISK
    // Tier 3 Transporter (33140): -5 to -2, 5M ISK
    // Tier 4 Negotiator (33141): -2 to 0, 10M ISK
    for (start_sec, valid_tag, invalid_tag, target_sec, expected_sec) in [
        (-9.0_f64, 33138_i64, 33139_i64, -8.5_f64, -8.5_f64),
        (-6.5_f64, 33139_i64, 33140_i64, -6.0_f64, -6.0_f64),
        (-3.5_f64, 33140_i64, 33141_i64, -3.0_f64, -3.0_f64),
        (-0.5_f64, 33141_i64, 33138_i64, 0.0_f64, 0.0_f64),
    ] {
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&pilot_id).unwrap().security_status = start_sec);

        // Out-of-band refusal
        let bad_res = rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(target_sec), PyRep::Dict(vec![(PyRep::Integer(invalid_tag), PyRep::Integer(1))])],
        )
        .await;
        assert!(bad_res.unwrap_err().contains("SecurityTagBandMismatch"), "Out-of-band tag refused");

        // In-band turn-in
        let ok_res = rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(target_sec), PyRep::Dict(vec![(PyRep::Integer(valid_tag), PyRep::Integer(1))])],
        )
        .await
        .expect("ApplyTagsForSecStatus in-band");
        let new_s = match field(&ok_res, "newSecurityStatus") {
            PyRep::Real(v) => v,
            other => panic!("expected Real newSecurityStatus, got {other:?}"),
        };
        assert!((new_s - expected_sec).abs() < 1e-6);
    }

    // 4. Tags cannot raise security status above 0.0, excess tags preserved
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&pilot_id).unwrap().security_status = 0.0);
    let at_zero_err = rpc(
        &ctx,
        &mut sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(1))])],
    )
    .await;
    assert!(at_zero_err.unwrap_err().contains("InvalidTargetSecurityStatus"));

    // Partial consumption: -0.2 with 5 Negotiator tags requested -> consumes only 1 tag to reach 0.0
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&pilot_id).unwrap().security_status = -0.2);
    let excess_res = rpc(
        &ctx,
        &mut sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(5))])],
    )
    .await
    .expect("ApplyTagsForSecStatus partial consumption");
    assert_eq!(field(&excess_res, "newSecurityStatus"), PyRep::Real(0.0));

    // Remaining Negotiator tags: 5 initial - 1 in band 4 - 1 in partial = 3
    let rem_neg_tags = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.owner_id == pilot_id && i.type_id == 33141).map(|i| i.quantity).sum::<i32>()
    });
    assert_eq!(rem_neg_tags, 3, "Excess tags beyond 0.0 cap must not be consumed");

    // Total fees: 1M + 2M + 5M + 10M + 10M = 28M ISK
    let bal_after = ctx.db.with_snapshot(|s| s.characters[&pilot_id].balance);
    assert_eq!(bal_before.0 - bal_after.0, IskCents::from_isk(28_000_000).0, "Exact ISK fees charged");

    // 5. GetSecurityStatusTransactions audit trail (`feat:crimewatch.sec-status-transactions`)
    let sec_tx = rpc(&ctx, &mut sess, "crimewatch", "GetSecurityStatusTransactions", &[])
        .await
        .expect("GetSecurityStatusTransactions");
    let PyRep::Rowset { columns, rows, .. } = sec_tx else {
        panic!("expected Rowset from GetSecurityStatusTransactions");
    };

    let col_indices: HashMap<&str, usize> = columns
        .iter()
        .enumerate()
        .map(|(i, col)| (col.as_str(), i))
        .collect();

    let tag_rows: Vec<&Vec<PyRep>> = rows
        .iter()
        .filter(|r| r[col_indices["eventTypeID"]] == PyRep::Integer(385))
        .collect();
    assert!(!tag_rows.is_empty(), "Transactions log must record eventTypeID 385 for tag turn-in");
    let last_tag_row = tag_rows.last().unwrap();
    assert_eq!(last_tag_row[col_indices["locationID"]], PyRep::Integer(lowsec_st_sys_id as i64));
    assert_eq!(last_tag_row[col_indices["otherOwnerID"]], PyRep::None);

    let npc_rows: Vec<&Vec<PyRep>> = rows
        .iter()
        .filter(|r| r[col_indices["eventTypeID"]] == PyRep::Integer(378))
        .collect();
    assert!(!npc_rows.is_empty(), "Transactions log must record eventTypeID 378 for NPC kills");
}
