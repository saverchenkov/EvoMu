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

pub(crate) mod crimewatch_drones_lifecycle {
//! End-to-end integration tests closing Crimewatch parity rows:
//! - `feat:crimewatch.concord-response-scaling`
//! - `feat:crimewatch.criminal-wreck-auto-abandon`
//! - `feat:crimewatch.limited-engagement-duel`
//! - `feat:crimewatch.suspect-timer-looting-ess`
//! - `feat:crimewatch.safety-setting-action-block`

use eve_db::{AccountRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes, TrainedSkillRecord};
use eve_destiny::{Ball, DamageVector, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_combat_runtime;
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::collections::HashMap;

pub(crate) fn new_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx
}

pub(crate) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    bound: Option<u64>,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, bound, method, args, &HashMap::new(), None).await
}

pub(crate) fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(item_id, InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() });
    });
}

pub(crate) async fn spawn_pilot(
    ctx: &ServerContext, sid: u64, char_id: i64, corp_id: i64, ship_id: i64, ship_type_id: u32, sys_id: u32, pos: DVec3,
) -> SessionState {
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(char_id, CharacterRecord {
            character_id: char_id, user_id: sid, name: format!("Pilot {char_id}"), corporation_id: corp_id,
            alliance_id: None, war_faction_id: None, station_id: None, structure_id: None, solar_system_id: sys_id,
            constellation_id: 20_000_020, region_id: 10_000_002, active_ship_id: ship_id,
            balance: IskCents::from_isk(500_000_000), plex_balance: 0, loyalty_points: HashMap::new(),
            standings: HashMap::new(), security_status: 2.0, clone_grade: 2, home_station_id: 60_003_760,
            race_id: 1, bloodline_id: 1, ancestry_id: 1, school_id: 1, neural: NeuralAttributes::default(),
            skill_points: 10_000.0 * 1_000.0, activity_status: None, free_skill_points: 0.0,
            last_clone_jump_unix: 0, jump_fatigue_minutes: 0.0, jump_cooldown_until_unix: 0, jump_fatigue_until_unix: 0,
            description: String::new(), title: String::new(), gender: 1, create_date_unix: 1_700_000_000,
            fleet_id: None, autopilot_waypoints: Vec::new(), version: 1, system_epoch: 0,
        });
        snap.accounts.entry(format!("u_{sid}")).or_insert(AccountRecord {
            user_id: sid, username: format!("u_{sid}"), password_token: String::new(), role: 0, banned_until_unix: None, ban_reason: None,
        });
        let sk = snap.skills.entry(char_id).or_default();
        for tid in [3429, 3436, 11579] {
            sk.insert(tid, TrainedSkillRecord { type_id: tid, level: 5, skill_points: 256_000.0 });
        }
    });
    seed_item(ctx, ship_id, ship_type_id, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    let _ = ctx.db.acquire_online_lock(sid, 10_000 + sid, Some(char_id), true, ctx.now_unix());
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let grp = ctx.sde.types.get(&ship_type_id).map(|t| t.group_id).unwrap_or(25);
    let _ = sys_h.spawn_ball_with_meta(
        Ball { id: ship_id, type_id: ship_type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 320.0, inertia: 2.0, mass: 1_200_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_type_combat_state(ship_type_id),
        SlimItemMeta { group_id: grp, category_id: 6, owner_id: char_id, char_id: Some(char_id), corp_id, alliance_id: -1, security_status: 2.0, is_cloaked: false , ..Default::default()},
    ).await;
    let mut sess = SessionState::new(10_000 + sid);
    sess.user_id = Some(sid);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess
}

pub(crate) fn fit_online(ctx: &ServerContext, char_id: i64, ship_id: i64, mod_id: i64, mod_tid: u32, flag: InventoryFlag, ammo: Option<(i64, u32, i32)>) {
    seed_item(ctx, mod_id, mod_tid, char_id, ship_id, flag, 1, true);
    if let Some((aid, atid, qty)) = ammo {
        seed_item(ctx, aid, atid, char_id, ship_id, flag, qty, false);
    }
    let req_skills: Vec<u32> = ctx.sde.types.get(&mod_tid).map(|t| {
        [182u16, 183, 184, 1285, 1289, 1290].into_iter()
            .filter_map(|a| t.default_attributes.get(&a).copied())
            .filter(|&v| v > 0.0).map(|v| v as u32).collect()
    }).unwrap_or_default();
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mod_id, true);
        let sk = snap.skills.entry(char_id).or_default();
        for tid in req_skills.into_iter().chain([3413, 3426]) {
            sk.insert(tid, TrainedSkillRecord { type_id: tid, level: 5, skill_points: 256_000.0 });
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
}

pub(crate) async fn lock_target(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, target_id: i64) {
    rpc(ctx, sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(target_id)]).await.expect("dogmaIM.AddTarget");
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for _ in 0..60 {
        let _ = sys_h.step_manual(1.0).await;
    }
}

fn find_wreck_of_owner(ctx: &ServerContext, owner_id: i64, sys_id: u32) -> i64 {
    ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|i| {
                i.owner_id == owner_id
                    && i.location_id == sys_id as i64
                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186)
            })
            .map(|i| i.item_id)
            .expect("Expected real wreck spawned by death_pipeline")
    })
}

#[tokio::test]
async fn test_crimewatch_concord_response_scaling_and_invulnerable_enforcement() {
    // parity: feat:crimewatch.concord-response-scaling
    eve_testkit::attest(&["feat:crimewatch.concord-response-scaling"]);

    // 1. Live SDE high-sec solar systems across all 6 response tiers via real router `dogmaIM.Activate`
    let tiers: [(u32, u64); 6] = [
        (30_000_010, 19), // Chidah (~0.5)
        (30_000_009, 14), // Sooma (~0.6)
        (30_000_026, 11), // Ordize (~0.7)
        (30_000_002, 8),  // Lashesih (~0.8)
        (30_000_142, 6),  // Jita (~0.9)
        (30_100_032, 6),  // Manifest (1.0)
    ];
    for (idx, (sys_id, expected_delay_s)) in tiers.into_iter().enumerate() {
        let ctx = new_ctx();
        let atk_c = 91_000_100 + idx as i64;
        let vic_c = 91_000_200 + idx as i64;
        let atk_s = 81_000_100 + idx as i64;
        let vic_s = 81_000_200 + idx as i64;
        let gun_id = 81_000_300 + idx as i64;
        let ammo_id = 81_000_400 + idx as i64;
        let mut atk_sess = spawn_pilot(&ctx, 100 + idx as u64, atk_c, 1_000_009, atk_s, 587, sys_id, DVec3::ZERO).await;
        let _vic_sess = spawn_pilot(&ctx, 200 + idx as u64, vic_c, 1_000_010, vic_s, 587, sys_id, DVec3::new(1_000.0, 0.0, 0.0)).await;
        fit_online(&ctx, atk_c, atk_s, gun_id, 486, InventoryFlag::HiSlot0, Some((ammo_id, 185, 50)));
        lock_target(&ctx, &mut atk_sess, sys_id, vic_s).await;
        rpc(&ctx, &mut atk_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
        let t0 = ctx.now_unix();
        rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(gun_id), PyRep::Integer(vic_s)]).await.expect("Unprovoked high-sec attack on Safety Red");
        let (_, arrival, _, _) = ctx.crimewatch.read().unwrap().states[&atk_c].pending_concord_arrival.expect("CONCORD schedule");
        assert_eq!(arrival, t0 + expected_delay_s, "Mismatch for system {sys_id} (expected {expected_delay_s}s)");
    }

    // 2. Low-sec (0.4, Onsooh 30_000_013) triggers Suspect Flag with zero CONCORD schedule
    {
        let ctx = new_ctx();
        let (atk_c, vic_c, atk_s, vic_s) = (91_000_501, 91_000_502, 81_000_501, 81_000_502);
        let mut atk_sess = spawn_pilot(&ctx, 301, atk_c, 1_000_009, atk_s, 587, 30_000_013, DVec3::ZERO).await;
        let _vic_sess = spawn_pilot(&ctx, 302, vic_c, 1_000_010, vic_s, 587, 30_000_013, DVec3::new(1_000.0, 0.0, 0.0)).await;
        fit_online(&ctx, atk_c, atk_s, 81_000_510, 486, InventoryFlag::HiSlot0, Some((81_000_511, 185, 50)));
        lock_target(&ctx, &mut atk_sess, 30_000_013, vic_s).await;
        rpc(&ctx, &mut atk_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
        rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(81_000_510), PyRep::Integer(vic_s)]).await.unwrap();
        let cw = ctx.crimewatch.read().unwrap();
        assert!(cw.states[&atk_c].pending_concord_arrival.is_none(), "Low-sec must never schedule CONCORD");
        assert!(cw.states[&atk_c].is_suspect(ctx.now_unix()));
    }

    // 3. Full CONCORD execution suite in Jita (0.9 -> 6s) driven by production `tick_background_cluster`
    let ctx = new_ctx();
    let jita = 30_000_142u32;
    let (atk_c, vic_c, atk_s, vic_s) = (91_000_901, 91_000_902, 81_000_901, 81_000_902);
    let mut atk_sess = spawn_pilot(&ctx, 401, atk_c, 1_000_009, atk_s, 621, jita, DVec3::new(5_000.0, 0.0, 0.0)).await;
    let _vic_sess = spawn_pilot(&ctx, 402, vic_c, 1_000_010, vic_s, 587, jita, DVec3::new(6_000.0, 0.0, 0.0)).await;
    fit_online(&ctx, atk_c, atk_s, 81_000_910, 486, InventoryFlag::HiSlot0, Some((81_000_911, 185, 50)));
    let sys_h = ctx.systems.get_or_spawn(jita);
    lock_target(&ctx, &mut atk_sess, jita, vic_s).await;
    let jita_b = ctx.bind_object(&atk_sess, "beyonce", jita as i64).unwrap();
    rpc(&ctx, &mut atk_sess, "beyonce", Some(jita_b), "CmdOrbit", &[PyRep::Integer(vic_s), PyRep::Real(1_000.0)]).await.unwrap();

    rpc(&ctx, &mut atk_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(81_000_910), PyRep::Integer(vic_s)]).await.unwrap();

    // At t0 + 3s (< 6s): CONCORD has not arrived yet; at t0 + 6s: `tick_background_cluster` executes CONCORD
    ctx.advance_virtual_clock(3);
    tick_background_cluster(&ctx).await;
    assert!(ctx.crimewatch.concord_reports().is_empty());
    assert!(sys_h.get_ball_snapshot(atk_s).await.unwrap().is_some(), "Criminal hull intact before 6s arrival");

    ctx.advance_virtual_clock(3);
    tick_background_cluster(&ctx).await;
    let reports = ctx.crimewatch.concord_reports();
    assert_eq!(reports.len(), 1);
    let rep = &reports[0];
    assert_eq!(rep.criminal_char_id, atk_c);
    assert_eq!(rep.webbed_speed_fraction, 0.0, "CONCORD must apply 100% stasis web");
    assert!(rep.warp_scramble_strength >= 100, "CONCORD must warp scramble criminal hull");
    assert_eq!(rep.capacitor_after_neut, 0.0, "CONCORD must completely drain capacitor");
    assert!(rep.ecm_jammed, "CONCORD must apply ECM jam");
    assert!(rep.locks_cleared >= 1, "CONCORD ECM jam must clear criminal target locks");

    // Verify CONCORD battleship on grid is completely invulnerable to incoming damage
    let concord_cs_before = sys_h.get_combat_state(rep.concord_ball_id).await.unwrap().expect("CONCORD ship on grid");
    assert_eq!(concord_cs_before.shield_resonance, [0.0, 0.0, 0.0, 0.0]);
    let (applied, destroyed, _) = sys_h
        .apply_damage(rep.concord_ball_id, DamageVector::new(250_000.0, 250_000.0, 250_000.0, 250_000.0))
        .await
        .unwrap()
        .expect("CONCORD ball present");
    assert_eq!(applied, 0.0, "CONCORD hulls must take 0.0 damage");
    assert!(!destroyed);

    // Criminal ship was destroyed and pilot is now in an intact Capsule (670)
    assert!(sys_h.get_ball_snapshot(atk_s).await.unwrap().is_none(), "Criminal hull must be destroyed");
    let pod_id = ctx.db.get_character(atk_c).unwrap().active_ship_id;
    assert_eq!(ctx.db.get_item(pod_id).unwrap().type_id, 670, "Criminal pilot must be ejected into an intact capsule");
}

#[tokio::test]
async fn test_crimewatch_criminal_wreck_and_jetcan_auto_abandon_all_sec_bands() {
    // parity: feat:crimewatch.criminal-wreck-auto-abandon
    eve_testkit::attest(&["feat:crimewatch.criminal-wreck-auto-abandon"]);

    let ctx = new_ctx();

    // 1. High-sec (`Jita` 30_000_142): pilot commits real criminal aggression (`dogmaIM.Activate`), jettisons a jetcan (`ship.Jettison`),
    //    and is destroyed by CONCORD via `tick_background_cluster` -> real wreck & jetcan are auto-abandoned even after Criminal Timer expires!
    let jita = 30_000_142u32;
    let (hs_crim, hs_vic, hs_loot) = (92_100_001, 92_100_002, 92_100_003);
    let (hs_crim_s, hs_vic_s, hs_loot_s) = (82_100_001, 82_100_002, 82_100_003);
    let mut hs_crim_sess = spawn_pilot(&ctx, 501, hs_crim, 1_000_009, hs_crim_s, 587, jita, DVec3::ZERO).await;
    let _hs_vic_sess = spawn_pilot(&ctx, 502, hs_vic, 1_000_010, hs_vic_s, 587, jita, DVec3::new(800.0, 0.0, 0.0)).await;
    let mut hs_loot_sess = spawn_pilot(&ctx, 503, hs_loot, 1_000_011, hs_loot_s, 587, jita, DVec3::new(300.0, 0.0, 0.0)).await;
    fit_online(&ctx, hs_crim, hs_crim_s, 82_100_101, 486, InventoryFlag::HiSlot0, Some((82_100_102, 185, 20)));
    seed_item(&ctx, 82_100_200, 34, hs_crim, hs_crim_s, InventoryFlag::Cargo, 40, false);
    for k in 1..=16 {
        seed_item(&ctx, 82_100_200 + k, 34, hs_crim, hs_crim_s, InventoryFlag::Cargo, 5, true);
    }
    lock_target(&ctx, &mut hs_crim_sess, jita, hs_vic_s).await;
    rpc(&ctx, &mut hs_crim_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut hs_crim_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(82_100_101), PyRep::Integer(hs_vic_s)]).await.unwrap();
    assert!(ctx.crimewatch.read().unwrap().states[&hs_crim].is_criminal(ctx.now_unix()));

    let PyRep::Tuple(hs_jet_t) = rpc(&ctx, &mut hs_crim_sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(82_100_200)])]).await.unwrap() else { panic!() };
    let PyRep::List(hs_jet_ids) = &hs_jet_t[0] else { panic!() };
    let hs_jetcan = hs_jet_ids[0].as_i64().unwrap();
    let hs_jet_item = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == hs_jetcan).unwrap().item_id);

    ctx.advance_virtual_clock(6);
    tick_background_cluster(&ctx).await;
    let hs_wreck = find_wreck_of_owner(&ctx, hs_crim, jita);

    // Advance 901s so `hs_crim`'s Criminal Timer expires: wreck & jetcan created while criminal remain permanently abandoned!
    ctx.advance_virtual_clock(901);
    assert!(!ctx.crimewatch.read().unwrap().states[&hs_crim].is_criminal(ctx.now_unix()));
    rpc(&ctx, &mut hs_loot_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    let hs_inv = ctx.bind_object(&hs_loot_sess, "invbroker", hs_loot_s).unwrap();
    let hs_wrep = rpc(&ctx, &mut hs_loot_sess, "invbroker", Some(hs_inv), "LootAll", &[PyRep::Integer(hs_wreck)]).await.expect("High-sec criminal wreck auto-abandoned");
    assert_eq!(hs_wrep.dict_get("abandoned"), Some(&PyRep::Bool(true)));
    assert_eq!(hs_wrep.dict_get("suspectFlagTriggered"), Some(&PyRep::Bool(false)));
    rpc(&ctx, &mut hs_loot_sess, "invbroker", Some(hs_inv), "Add", &[PyRep::Integer(hs_jet_item), PyRep::Integer(hs_jetcan), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect("High-sec criminal jetcan auto-abandoned");
    assert!(!ctx.crimewatch.read().unwrap().states.get(&hs_loot).is_some_and(|st| st.is_suspect(ctx.now_unix())));

    // 2. Low-sec (`Amamake` 30_002_537): pilot EWAR-attacks a Capsule (`670`) via `dogmaIM.Activate` -> acquires live Criminal Timer,
    //    jettisons a jetcan, and has their Impairor (`596`) destroyed via bounded Smartbomb (`14550`) cycles -> wreck & jetcan auto-abandoned!
    let lowsec = 30_002_537u32;
    let (ls_crim, ls_pod_c, ls_loot) = (92_200_001, 92_200_002, 92_200_003);
    let (ls_crim_s, ls_pod_s, ls_loot_s) = (82_200_001, 82_200_002, 82_200_003);
    let mut ls_crim_sess = spawn_pilot(&ctx, 511, ls_crim, 1_000_009, ls_crim_s, 596, lowsec, DVec3::ZERO).await;
    let _ls_pod_sess = spawn_pilot(&ctx, 512, ls_pod_c, 1_000_010, ls_pod_s, 670, lowsec, DVec3::new(9_000.0, 0.0, 0.0)).await;
    let mut ls_loot_sess = spawn_pilot(&ctx, 513, ls_loot, 1_000_011, ls_loot_s, 24692, lowsec, DVec3::new(400.0, 0.0, 0.0)).await;
    fit_online(&ctx, ls_crim, ls_crim_s, 82_200_101, 3242, InventoryFlag::MedSlot0, None);
    fit_online(&ctx, ls_loot, ls_loot_s, 82_200_102, 14550, InventoryFlag::HiSlot0, None);
    seed_item(&ctx, 82_200_200, 34, ls_crim, ls_crim_s, InventoryFlag::Cargo, 40, false);
    for k in 1..=16 {
        seed_item(&ctx, 82_200_200 + k, 34, ls_crim, ls_crim_s, InventoryFlag::Cargo, 5, true);
    }
    lock_target(&ctx, &mut ls_crim_sess, lowsec, ls_pod_s).await;
    rpc(&ctx, &mut ls_crim_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut ls_crim_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(82_200_101), PyRep::Integer(ls_pod_s)]).await.unwrap();
    assert!(ctx.crimewatch.read().unwrap().states[&ls_crim].is_criminal(ctx.now_unix()));

    let PyRep::Tuple(ls_jet_t) = rpc(&ctx, &mut ls_crim_sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(82_200_200)])]).await.unwrap() else { panic!() };
    let PyRep::List(ls_jet_ids) = &ls_jet_t[0] else { panic!() };
    let ls_jetcan = ls_jet_ids[0].as_i64().unwrap();
    let ls_jet_item = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == ls_jetcan).unwrap().item_id);

    rpc(&ctx, &mut ls_loot_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    for _ in 0..3 {
        if ctx.db.get_item(ls_crim_s).is_none() {
            break;
        }
        rpc(&ctx, &mut ls_loot_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(82_200_102)]).await.unwrap();
        ctx.advance_virtual_clock(11);
    }
    assert!(ctx.db.get_item(ls_crim_s).is_none(), "Low-sec criminal ship must be destroyed");
    let ls_wreck = find_wreck_of_owner(&ctx, ls_crim, lowsec);

    rpc(&ctx, &mut ls_loot_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    let ls_inv = ctx.bind_object(&ls_loot_sess, "invbroker", ls_loot_s).unwrap();
    let ls_wrep = rpc(&ctx, &mut ls_loot_sess, "invbroker", Some(ls_inv), "LootAll", &[PyRep::Integer(ls_wreck)]).await.expect("Low-sec criminal wreck auto-abandoned");
    assert_eq!(ls_wrep.dict_get("abandoned"), Some(&PyRep::Bool(true)));
    assert_eq!(ls_wrep.dict_get("suspectFlagTriggered"), Some(&PyRep::Bool(false)));
    rpc(&ctx, &mut ls_loot_sess, "invbroker", Some(ls_inv), "Add", &[PyRep::Integer(ls_jet_item), PyRep::Integer(ls_jetcan), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect("Low-sec criminal jetcan auto-abandoned");
    assert!(!ctx.crimewatch.read().unwrap().states.get(&ls_loot).is_some_and(|st| st.is_suspect(ctx.now_unix())));

    // 3. Null-sec (`Kaira` 30_003_887 -> stargate 50_007_698 -> `A2-V27` 30_003_942): criminal pilot jumps stargate into null-sec
    //    holding a live Criminal Timer, jettisons a jetcan, and is destroyed in null-sec -> wreck & jetcan auto-abandoned!
    let (kaira, a2_v27, gate_id) = (30_003_887u32, 30_003_942u32, 50_007_698i64);
    let gate_pos = DVec3::new(975_370_690_560.0, -4_693_133_598_720.0, -389_298_954_240.0);
    let (ns_crim, ns_pod_c, ns_loot) = (92_300_001, 92_300_002, 92_300_003);
    let (ns_crim_s, ns_pod_s, ns_loot_s) = (82_300_001, 82_300_002, 82_300_003);
    let mut ns_crim_sess = spawn_pilot(&ctx, 521, ns_crim, 1_000_009, ns_crim_s, 596, kaira, gate_pos + DVec3::new(100.0, 0.0, 0.0)).await;
    let _ns_pod_sess = spawn_pilot(&ctx, 522, ns_pod_c, 1_000_010, ns_pod_s, 670, kaira, gate_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    fit_online(&ctx, ns_crim, ns_crim_s, 82_300_101, 447, InventoryFlag::MedSlot0, None);
    seed_item(&ctx, 82_300_200, 34, ns_crim, ns_crim_s, InventoryFlag::Cargo, 40, false);
    for k in 1..=16 {
        seed_item(&ctx, 82_300_200 + k, 34, ns_crim, ns_crim_s, InventoryFlag::Cargo, 5, true);
    }
    lock_target(&ctx, &mut ns_crim_sess, kaira, ns_pod_s).await;
    rpc(&ctx, &mut ns_crim_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut ns_crim_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(82_300_101), PyRep::Integer(ns_pod_s)]).await.unwrap();
    ctx.advance_virtual_clock(61); // 60s Weapons Timer expires; 839s of live Criminal Timer remains
    let kaira_b = ctx.bind_object(&ns_crim_sess, "beyonce", kaira as i64).unwrap();
    rpc(&ctx, &mut ns_crim_sess, "beyonce", Some(kaira_b), "CmdStargateJump", &[PyRep::Integer(gate_id)]).await.expect("Low-sec criminal jumps gate into null-sec");
    assert_eq!(ns_crim_sess.solar_system_id, Some(a2_v27));
    assert!(ctx.crimewatch.read().unwrap().states[&ns_crim].is_criminal(ctx.now_unix()));

    let a2_h = ctx.systems.get_or_spawn(a2_v27);
    let (_, ns_crim_ball) = a2_h.get_ball_snapshot(ns_crim_s).await.unwrap().unwrap();
    let mut ns_loot_sess = spawn_pilot(&ctx, 523, ns_loot, 1_000_011, ns_loot_s, 24692, a2_v27, ns_crim_ball.position + DVec3::new(400.0, 0.0, 0.0)).await;
    fit_online(&ctx, ns_loot, ns_loot_s, 82_300_102, 14550, InventoryFlag::HiSlot0, None);
    let PyRep::Tuple(ns_jet_t) = rpc(&ctx, &mut ns_crim_sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(82_300_200)])]).await.unwrap() else { panic!() };
    let PyRep::List(ns_jet_ids) = &ns_jet_t[0] else { panic!() };
    let ns_jetcan = ns_jet_ids[0].as_i64().unwrap();
    let ns_jet_item = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == ns_jetcan).unwrap().item_id);

    for _ in 0..61 {
        let _ = a2_h.step_manual(1.0).await; // Expire 60-tick post-jump gate cloak/invulnerability naturally
    }
    ctx.advance_virtual_clock(65);
    assert!(ctx.crimewatch.read().unwrap().states[&ns_crim].is_criminal(ctx.now_unix()));
    for _ in 0..3 {
        if ctx.db.get_item(ns_crim_s).is_none() {
            break;
        }
        rpc(&ctx, &mut ns_loot_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(82_300_102)]).await.unwrap();
        ctx.advance_virtual_clock(11);
    }
    assert!(ctx.db.get_item(ns_crim_s).is_none(), "Null-sec criminal ship must be destroyed");
    let ns_wreck = find_wreck_of_owner(&ctx, ns_crim, a2_v27);

    rpc(&ctx, &mut ns_loot_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    let ns_inv = ctx.bind_object(&ns_loot_sess, "invbroker", ns_loot_s).unwrap();
    let ns_wrep = rpc(&ctx, &mut ns_loot_sess, "invbroker", Some(ns_inv), "LootAll", &[PyRep::Integer(ns_wreck)]).await.expect("Null-sec criminal wreck auto-abandoned");
    assert_eq!(ns_wrep.dict_get("abandoned"), Some(&PyRep::Bool(true)));
    assert_eq!(ns_wrep.dict_get("suspectFlagTriggered"), Some(&PyRep::Bool(false)));
    rpc(&ctx, &mut ns_loot_sess, "invbroker", Some(ns_inv), "Add", &[PyRep::Integer(ns_jet_item), PyRep::Integer(ns_jetcan), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect("Null-sec criminal jetcan auto-abandoned");

    // 4. Negative control: non-criminal pilot's jetcan and duel wreck are NOT auto-abandoned (`CantDoInCurrentSafetyLevel`)
    let (cl_a, cl_b, cl_sa, cl_sb) = (92_400_001, 92_400_002, 82_400_001, 82_400_002);
    let mut cl_sess_a = spawn_pilot(&ctx, 531, cl_a, 1_000_009, cl_sa, 596, lowsec, DVec3::new(50_000.0, 0.0, 0.0)).await;
    let mut cl_sess_b = spawn_pilot(&ctx, 532, cl_b, 1_000_010, cl_sb, 24692, lowsec, DVec3::new(50_400.0, 0.0, 0.0)).await;
    fit_online(&ctx, cl_b, cl_sb, 82_400_101, 14550, InventoryFlag::HiSlot0, None);
    seed_item(&ctx, 82_400_200, 34, cl_a, cl_sa, InventoryFlag::Cargo, 20, false);
    for k in 1..=16 {
        seed_item(&ctx, 82_400_200 + k, 34, cl_a, cl_sa, InventoryFlag::Cargo, 5, true);
    }
    let PyRep::Tuple(cl_jet_t) = rpc(&ctx, &mut cl_sess_a, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(82_400_200)])]).await.unwrap() else { panic!() };
    let PyRep::List(cl_jet_ids) = &cl_jet_t[0] else { panic!() };
    let cl_jetcan = cl_jet_ids[0].as_i64().unwrap();
    let cl_jet_item = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == cl_jetcan).unwrap().item_id);
    rpc(&ctx, &mut cl_sess_a, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(cl_b)]).await.unwrap();
    rpc(&ctx, &mut cl_sess_b, "crimewatch", None, "RespondToDuelChallenge", &[PyRep::Integer(cl_a), PyRep::Bool(true)]).await.unwrap();
    rpc(&ctx, &mut cl_sess_b, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    for _ in 0..3 {
        if ctx.db.get_item(cl_sa).is_none() {
            break;
        }
        rpc(&ctx, &mut cl_sess_b, "dogmaIM", None, "Activate", &[PyRep::Integer(82_400_101)]).await.unwrap();
        ctx.advance_virtual_clock(11);
    }
    assert!(ctx.db.get_item(cl_sa).is_none(), "Duel ship must be destroyed");
    let cl_wreck = find_wreck_of_owner(&ctx, cl_a, lowsec);
    let _ = ctx.systems.get_or_spawn(lowsec).set_ball_position(ls_loot_s, DVec3::new(50_200.0, 0.0, 0.0)).await;
    assert_eq!(rpc(&ctx, &mut ls_loot_sess, "invbroker", Some(ls_inv), "LootAll", &[PyRep::Integer(cl_wreck)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    assert_eq!(rpc(&ctx, &mut ls_loot_sess, "invbroker", Some(ls_inv), "Add", &[PyRep::Integer(cl_jet_item), PyRep::Integer(cl_jetcan), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
}

#[tokio::test]
async fn test_crimewatch_duel_limited_engagement_and_suspect_looting_ess() {
    // parity: feat:crimewatch.limited-engagement-duel, feat:crimewatch.suspect-timer-looting-ess, feat:crimewatch.safety-setting-action-block
    eve_testkit::attest(&[
        "feat:crimewatch.limited-engagement-duel",
        "feat:crimewatch.suspect-timer-looting-ess",
        "feat:crimewatch.safety-setting-action-block",
    ]);

    let ctx = new_ctx();
    let jita = 30_000_142u32;
    let lowsec = 30_002_537u32;
    let nullsec = 30_004_759u32;
    let (c1, c2, c3) = (93_000_001, 93_000_002, 93_000_003);
    let (s1, s2, s3) = (83_000_001, 83_000_002, 83_000_003);
    let mut sess1 = spawn_pilot(&ctx, 701, c1, 1_000_009, s1, 587, jita, DVec3::ZERO).await;
    let mut sess2 = spawn_pilot(&ctx, 702, c2, 1_000_010, s2, 587, jita, DVec3::new(1_500.0, 0.0, 0.0)).await;
    let mut sess3 = spawn_pilot(&ctx, 703, c3, 1_000_011, s3, 587, jita, DVec3::new(2_000.0, 0.0, 0.0)).await;

    fit_online(&ctx, c1, s1, 83_000_101, 486, InventoryFlag::HiSlot0, Some((83_000_102, 185, 200)));
    fit_online(&ctx, c1, s1, 83_000_103, 447, InventoryFlag::MedSlot0, None);
    fit_online(&ctx, c2, s2, 83_000_201, 486, InventoryFlag::HiSlot0, Some((83_000_202, 185, 200)));
    fit_online(&ctx, c3, s3, 83_000_301, 486, InventoryFlag::HiSlot0, Some((83_000_302, 185, 200)));

    let sys_h = ctx.systems.get_or_spawn(jita);
    lock_target(&ctx, &mut sess1, jita, s2).await;
    lock_target(&ctx, &mut sess2, jita, s1).await;
    lock_target(&ctx, &mut sess3, jita, s1).await;

    // 1. Duel lifecycle: reject self/nonexistent/offline targets; challenge -> decline -> challenge -> accept -> 300s bidirectional LE
    let c_offline = 93_000_050i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&c1].clone();
        ch.character_id = c_offline;
        ch.user_id = 750;
        ch.active_ship_id = 0;
        snap.characters.insert(c_offline, ch);
    });
    assert_eq!(rpc(&ctx, &mut sess1, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(c1)]).await.unwrap_err().code, "InvalidDuelTarget");
    assert_eq!(rpc(&ctx, &mut sess1, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(99_999_999)]).await.unwrap_err().code, "InvalidDuelTarget");
    assert_eq!(rpc(&ctx, &mut sess1, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(c_offline)]).await.unwrap_err().code, "DuelChallengeFailNoSesssionForTarget");

    let _ = ctx.drain_notifications_for_char(c1);
    let _ = ctx.drain_notifications_for_char(c2);
    rpc(&ctx, &mut sess1, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(c2)]).await.unwrap();
    let duel_notifs = ctx.drain_notifications_for_char(c2);
    let duel_note = duel_notifs
        .iter()
        .find(|n| n.event == "OnDuelChallenge")
        .expect("OnDuelChallenge");
    let expiry_ft = duel_note
        .args
        .get(3)
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| ctx.now_blueos_filetime_ticks() + 600_000_000);
    // V24.01 3-arg decline: (fromCharID, expiryTime, false) must NOT interpret expiryTime as accept=true
    let dec_3arg = rpc(
        &ctx,
        &mut sess2,
        "crimewatch",
        None,
        "RespondToDuelChallenge",
        &[PyRep::Integer(c1), PyRep::Integer(expiry_ft), PyRep::Bool(false)],
    )
    .await
    .unwrap();
    assert_eq!(dec_3arg.dict_get("accepted").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(dec_3arg.dict_get("limitedEngagementSeconds").and_then(|v| v.as_i64()), Some(0));
    assert!(!with_combat_runtime(&ctx, |rt| {
        rt.has_limited_engagement_with(c1, c2, ctx.now_unix())
            || rt.has_limited_engagement_with(c2, c1, ctx.now_unix())
    }));

    // 2-arg decline still works
    rpc(&ctx, &mut sess1, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(c2)]).await.unwrap();
    assert!(ctx.drain_notifications_for_char(c2).iter().any(|n| n.event == "OnDuelChallenge"));
    rpc(&ctx, &mut sess2, "crimewatch", None, "RespondToDuelChallenge", &[PyRep::Integer(c1), PyRep::Bool(false)]).await.unwrap();
    assert!(!with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(c1, c2, ctx.now_unix())));

    // V24.01 3-arg accept: (fromCharID, expiryTime, true) creates bidirectional 300s Limited Engagement
    rpc(&ctx, &mut sess1, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(c2)]).await.unwrap();
    assert!(ctx.drain_notifications_for_char(c2).iter().any(|n| n.event == "OnDuelChallenge"));
    let acc = rpc(
        &ctx,
        &mut sess2,
        "crimewatch",
        None,
        "RespondToDuelChallenge",
        &[PyRep::Integer(c1), PyRep::Integer(expiry_ft), PyRep::Bool(true)],
    )
    .await
    .unwrap();
    assert_eq!(acc.dict_get("accepted").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(acc.dict_get("limitedEngagementSeconds").and_then(|v| v.as_i64()), Some(300));
    assert!(ctx.drain_notifications_for_char(c1).iter().any(|n| n.event == "OnCrimewatchEngagementCreated"));

    // Both duelists fight in high-sec on Safety Green (`2`) with zero CONCORD and zero sentry gun interference
    rpc(&ctx, &mut sess1, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    rpc(&ctx, &mut sess2, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    rpc(&ctx, &mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(83_000_101), PyRep::Integer(s2)]).await.expect("Duelist 1 turret hit");
    rpc(&ctx, &mut sess2, "dogmaIM", None, "Activate", &[PyRep::Integer(83_000_201), PyRep::Integer(s1)]).await.expect("Duelist 2 return fire");
    tick_background_cluster(&ctx).await;
    assert!(ctx.crimewatch.read().unwrap().states.values().all(|st| st.pending_concord_arrival.is_none()), "CONCORD must ignore Limited Engagement");
    assert!(!with_combat_runtime(&ctx, |rt| rt.sentry_gun_aggression.values().any(|s| s.active_on_grid)), "Sentry guns must ignore Limited Engagement");

    // Subsequent offensive action after 120s resets LE back to `now + 300s`; after 301s idle, LE expires naturally
    ctx.advance_virtual_clock(120);
    let t_ref = ctx.now_unix();
    rpc(&ctx, &mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(83_000_103), PyRep::Integer(s2)]).await.expect("EWAR refreshes LE");
    let (e1, e2) = with_combat_runtime(&ctx, |rt| (rt.limited_engagements[&c1][&c2], rt.limited_engagements[&c2][&c1]));
    assert!(e1 >= t_ref + 300 && e2 >= t_ref + 300);

    ctx.advance_virtual_clock(301);
    let _ = rpc(&ctx, &mut sess1, "dogmaIM", None, "Deactivate", &[PyRep::Integer(83_000_101)]).await;
    ctx.advance_secs(10);
    let _ = sys_h.step_manual(1.0).await;
    let post_le_err = rpc(&ctx, &mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(83_000_101), PyRep::Integer(s2)]).await.unwrap_err();
    assert_eq!(post_le_err.code, "CantDoInCurrentSafetyLevel");

    // Unprovoked low-sec attack (Smartbomb requires Safety Red `0`) flags attacker Suspect WITHOUT creating an LE; victim returning fire creates 300s LE
    let (ls_a, ls_b, ls_sa, ls_sb) = (93_000_011, 93_000_012, 83_000_011, 83_000_012);
    let mut ls_sess_a = spawn_pilot(&ctx, 711, ls_a, 1_000_009, ls_sa, 626, lowsec, DVec3::ZERO).await;
    let mut ls_sess_b = spawn_pilot(&ctx, 712, ls_b, 1_000_010, ls_sb, 587, lowsec, DVec3::new(1_000.0, 0.0, 0.0)).await;
    fit_online(&ctx, ls_a, ls_sa, 83_000_401, 3953, InventoryFlag::HiSlot0, None);
    fit_online(&ctx, ls_b, ls_sb, 83_000_402, 486, InventoryFlag::HiSlot0, Some((83_000_403, 185, 100)));
    lock_target(&ctx, &mut ls_sess_b, lowsec, ls_sa).await;
    rpc(&ctx, &mut ls_sess_a, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut ls_sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(83_000_401)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    rpc(&ctx, &mut ls_sess_a, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut ls_sess_a, "dogmaIM", None, "Activate", &[PyRep::Integer(83_000_401)]).await.expect("Low-sec Smartbomb on Red");
    assert!(ctx.crimewatch.read().unwrap().states[&ls_a].is_suspect(ctx.now_unix()));
    assert!(!with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(ls_a, ls_b, ctx.now_unix())), "Unprovoked illegal hit must NOT create LE");
    rpc(&ctx, &mut ls_sess_b, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    rpc(&ctx, &mut ls_sess_b, "dogmaIM", None, "Activate", &[PyRep::Integer(83_000_402), PyRep::Integer(ls_sa)]).await.expect("Victim returns fire against Suspect");
    assert!(with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(ls_a, ls_b, ctx.now_unix())), "Returning fire against Suspect must create 300s LE");

    // 2. Suspect timer (900s) from foreign Cargo Container (`23` via `ship.Jettison`), foreign Wreck (`26561`), foreign Mobile Tractor Unit (`33475`, group 1250), and ESS Main Bank (`32882`)
    seed_item(&ctx, 83_099_001, 34, c2, s2, InventoryFlag::Cargo, 15, false);
    let _ = sys_h.set_ball_position(s2, DVec3::new(400.0, 0.0, 0.0)).await;
    let PyRep::Tuple(j_tup) = rpc(&ctx, &mut sess2, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(83_099_001)])]).await.unwrap() else { panic!() };
    let PyRep::List(j_ids) = &j_tup[0] else { panic!() };
    let foreign_jetcan = j_ids[0].as_i64().unwrap();
    assert_eq!(ctx.db.get_item(foreign_jetcan).unwrap().type_id, 23, "Jettison spawns Cargo Container (23)");
    let foreign_jet_item = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == foreign_jetcan).unwrap().item_id);

    let inv1 = ctx.bind_object(&sess1, "invbroker", s1).unwrap();
    rpc(&ctx, &mut sess1, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut sess1, "invbroker", Some(inv1), "Add", &[PyRep::Integer(foreign_jet_item), PyRep::Integer(foreign_jetcan), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    rpc(&ctx, &mut sess1, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
    let now_j = ctx.now_unix();
    rpc(&ctx, &mut sess1, "invbroker", Some(inv1), "Add", &[PyRep::Integer(foreign_jet_item), PyRep::Integer(foreign_jetcan), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap();
    assert_eq!(ctx.crimewatch.read().unwrap().states[&c1].suspect_flag_until_unix, now_j + 900);

    for (offset, container_tid) in [(0i64, 26561u32), (20, 33475u32)] {
        ctx.advance_virtual_clock(901); // Let previous 900s Suspect Timer expire naturally
        let cid = 83_100_000 + offset;
        let lid = 83_100_001 + offset;
        seed_item(&ctx, cid, container_tid, c2, jita as i64, InventoryFlag::None, 1, true);
        seed_item(&ctx, lid, 34, c2, cid, InventoryFlag::Cargo, 10, false);
        let _ = sys_h.spawn_ball(Ball {
            id: cid, type_id: container_tid, position: DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 0.0, inertia: 1.0, mass: 5_000.0, radius: 30.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
        rpc(&ctx, &mut sess1, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
        assert_eq!(rpc(&ctx, &mut sess1, "invbroker", Some(inv1), "Add", &[PyRep::Integer(lid), PyRep::Integer(cid), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
        rpc(&ctx, &mut sess1, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
        let now_l = ctx.now_unix();
        rpc(&ctx, &mut sess1, "invbroker", Some(inv1), "Add", &[PyRep::Integer(lid), PyRep::Integer(cid), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap();
        assert_eq!(ctx.crimewatch.read().unwrap().states[&c1].suspect_flag_until_unix, now_l + 900);
    }

    // Any other player (`c3`) in high-sec on Safety Green (`2`) freely attacks Suspect `c1` without CONCORD or security-status penalty
    let sec3_before = ctx.db.get_character(c3).unwrap().security_status;
    rpc(&ctx, &mut sess3, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    rpc(&ctx, &mut sess3, "dogmaIM", None, "Activate", &[PyRep::Integer(83_000_301), PyRep::Integer(s1)]).await.expect("Free high-sec attack on Suspect");
    assert!(ctx.crimewatch.read().unwrap().states.values().all(|st| st.pending_concord_arrival.is_none()));
    assert_eq!(ctx.db.get_character(c3).unwrap().security_status, sec3_before);
    assert!(with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(c1, c3, ctx.now_unix())));

    // ESS Main Bank hack in null-sec grants 900s Suspect Timer (blocked on Green with CantDoInCurrentSafetyLevel, allowed on Yellow)
    let (ess_c, ess_s, ess_ball) = (93_000_099, 83_000_099, 820_000_000 + nullsec as i64);
    let mut ess_sess = spawn_pilot(&ctx, 799, ess_c, 1_000_009, ess_s, 587, nullsec, DVec3::ZERO).await;
    ctx.db.with_mut_snapshot(|snap| { snap.system_states.entry(nullsec).or_default().ess_main_bank = IskCents::from_isk(25_000_000); });
    let _ = ctx.systems.get_or_spawn(nullsec).spawn_ball(Ball {
        id: ess_ball, type_id: 32882, position: DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 0.0, inertia: 1.0, mass: 50_000.0, radius: 100.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    rpc(&ctx, &mut ess_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    let ess_err = rpc(&ctx, &mut ess_sess, "essMgr", None, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball)]).await.unwrap_err();
    assert_eq!(ess_err.code, "CantDoInCurrentSafetyLevel");
    rpc(&ctx, &mut ess_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
    let now_ess = ctx.now_unix();
    rpc(&ctx, &mut ess_sess, "essMgr", None, "AttemptLinkToMainBank", &[PyRep::Integer(ess_ball)]).await.expect("ESS link on Yellow");
    assert_eq!(ctx.crimewatch.read().unwrap().states[&ess_c].suspect_flag_until_unix, now_ess + 900);
}

#[tokio::test]
async fn test_crimewatch_safety_setting_green_yellow_red_enforcement() {
    // parity: feat:crimewatch.safety-setting-action-block
    eve_testkit::attest(&["feat:crimewatch.safety-setting-action-block"]);

    let ctx = new_ctx();
    let jita = 30_000_142u32;
    let lowsec = 30_002_537u32;
    let (atk_c, vic_c, ally_c) = (94_000_001, 94_000_002, 94_000_003);
    let (atk_s, vic_s, ally_s) = (84_000_001, 84_000_002, 84_000_003);
    let mut atk_sess = spawn_pilot(&ctx, 801, atk_c, 1_000_009, atk_s, 626, jita, DVec3::ZERO).await;
    let mut vic_sess = spawn_pilot(&ctx, 802, vic_c, 1_000_010, vic_s, 587, jita, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let mut ally_sess = spawn_pilot(&ctx, 803, ally_c, 1_000_011, ally_s, 587, jita, DVec3::new(1_200.0, 0.0, 0.0)).await;

    let burst_tid = 580u32; // Burst Jammer I (group 80)
    fit_online(&ctx, atk_c, atk_s, 84_000_101, 486, InventoryFlag::HiSlot0, Some((84_000_102, 185, 100)));
    fit_online(&ctx, atk_c, atk_s, 84_000_103, 3953, InventoryFlag::HiSlot1, None);
    fit_online(&ctx, atk_c, atk_s, 84_000_104, burst_tid, InventoryFlag::MedSlot0, None);
    fit_online(&ctx, atk_c, atk_s, 84_000_105, 11355, InventoryFlag::HiSlot2, None); // Small Remote Armor Repairer I
    assert!(ctx.get_or_rebuild_ship_fitting(atk_c, atk_s).unwrap().modules.contains_key(&84_000_105), "Remote repairer must reside in ShipFittingEngine.modules");
    fit_online(&ctx, ally_c, ally_s, 84_000_107, 486, InventoryFlag::HiSlot0, Some((84_000_108, 185, 50)));
    seed_item(&ctx, 84_000_106, 2454, atk_c, atk_s, InventoryFlag::DroneBay, 1, true);
    seed_item(&ctx, 84_000_109, 34, vic_c, vic_s, InventoryFlag::Cargo, 20, false);

    lock_target(&ctx, &mut atk_sess, jita, vic_s).await;
    lock_target(&ctx, &mut atk_sess, jita, ally_s).await;
    lock_target(&ctx, &mut ally_sess, jita, vic_s).await;
    rpc(&ctx, &mut atk_sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(84_000_106)])]).await.unwrap();
    let hob_ball = with_combat_runtime(&ctx, |rt| rt.active_drones_in_space.keys().copied().next().unwrap());

    // 1. Safety Green (`2`) in high-sec blocks turret, Smartbomb, Burst Jammer, offensive drone, and remote repair onto LE/Suspect (`CantDoInCurrentSafetyLevel`)
    let _ = ctx.drain_notifications_for_char(atk_c);
    rpc(&ctx, &mut atk_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    let notifs = ctx.drain_notifications_for_char(atk_c);
    assert_eq!(notifs.iter().filter(|n| n.event == "OnSafetyLevelChanged" && n.args == vec![PyRep::Integer(2)]).count(), 1);

    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_101), PyRep::Integer(vic_s)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_103)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_104)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    assert_eq!(rpc(&ctx, &mut atk_sess, "entity", None, "CmdEngage", &[PyRep::List(vec![PyRep::Integer(hob_ball)]), PyRep::Integer(vic_s)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");

    // Live duel creates a real Limited Engagement on `ally_c` -> Green blocks remote repair (`CantDoInCurrentSafetyLevel`)
    rpc(&ctx, &mut ally_sess, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(vic_c)]).await.unwrap();
    rpc(&ctx, &mut vic_sess, "crimewatch", None, "RespondToDuelChallenge", &[PyRep::Integer(ally_c), PyRep::Bool(true)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_105), PyRep::Integer(ally_s)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    ctx.advance_virtual_clock(301); // Duel LE expires naturally

    // Live theft gives `ally_c` a real Suspect Timer -> Green blocks remote repair (`CantDoInCurrentSafetyLevel`)
    let PyRep::Tuple(vj_t) = rpc(&ctx, &mut vic_sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(84_000_109)])]).await.unwrap() else { panic!() };
    let PyRep::List(vj_ids) = &vj_t[0] else { panic!() };
    let v_can = vj_ids[0].as_i64().unwrap();
    let v_item = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == v_can).unwrap().item_id);
    let atk_inv = ctx.bind_object(&atk_sess, "invbroker", atk_s).unwrap();
    assert_eq!(rpc(&ctx, &mut atk_sess, "invbroker", Some(atk_inv), "Add", &[PyRep::Integer(v_item), PyRep::Integer(v_can), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    assert_eq!(rpc(&ctx, &mut atk_sess, "invbroker", Some(atk_inv), "LootAll", &[PyRep::Integer(v_can)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    rpc(&ctx, &mut ally_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
    let ally_inv = ctx.bind_object(&ally_sess, "invbroker", ally_s).unwrap();
    rpc(&ctx, &mut ally_sess, "invbroker", Some(ally_inv), "Add", &[PyRep::Integer(v_item), PyRep::Integer(v_can), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_105), PyRep::Integer(ally_s)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");

    // 2. Safety Yellow (`1`) blocks Criminal actions (high-sec PvP, Smartbomb/Burst Jammer, repping Criminal, low-sec Capsule/AoE) while allowing Suspect actions
    rpc(&ctx, &mut atk_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_101), PyRep::Integer(vic_s)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_103)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_104)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_105), PyRep::Integer(ally_s)]).await.expect("Safety Yellow allows repping Suspect");
    assert!(ctx.crimewatch.read().unwrap().states[&atk_c].is_suspect(ctx.now_unix()), "Repping Suspect must inherit Suspect flag");
    rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(84_000_105)]).await.unwrap();

    // `ally_sess` switches to Red and shoots `vic_s` -> Criminal Timer -> `atk_sess` on Yellow blocked (`CantDoInCurrentSafetyLevel`), on Red inherits Criminal + CONCORD!
    rpc(&ctx, &mut ally_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut ally_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_107), PyRep::Integer(vic_s)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_105), PyRep::Integer(ally_s)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");

    // Low-sec Green & Yellow vs Capsule (`670`), Smartbomb (`3953`), and Burst Jammer (`burst_tid`): all blocked with `CantDoInCurrentSafetyLevel`; Red (`0`) permits!
    let (ls_c, ls_vc, ls_s, ls_pod) = (94_000_021, 94_000_022, 84_000_021, 84_000_022);
    let mut ls_sess = spawn_pilot(&ctx, 821, ls_c, 1_000_009, ls_s, 626, lowsec, DVec3::ZERO).await;
    let _ls_pod_sess = spawn_pilot(&ctx, 822, ls_vc, 1_000_010, ls_pod, 670, lowsec, DVec3::new(1_000.0, 0.0, 0.0)).await;
    fit_online(&ctx, ls_c, ls_s, 84_000_201, 447, InventoryFlag::MedSlot0, None);
    fit_online(&ctx, ls_c, ls_s, 84_000_202, 3953, InventoryFlag::HiSlot0, None);
    fit_online(&ctx, ls_c, ls_s, 84_000_203, burst_tid, InventoryFlag::MedSlot1, None);
    lock_target(&ctx, &mut ls_sess, lowsec, ls_pod).await;
    for lvl in [2i64, 1i64] {
        rpc(&ctx, &mut ls_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(lvl)]).await.unwrap();
        assert_eq!(rpc(&ctx, &mut ls_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_201), PyRep::Integer(ls_pod)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
        assert_eq!(rpc(&ctx, &mut ls_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_202)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
        assert_eq!(rpc(&ctx, &mut ls_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_203)]).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    }
    let pod_cs = ctx.systems.get_or_spawn(lowsec).get_combat_state(ls_pod).await.unwrap().unwrap();
    assert_eq!(pod_cs.shield_hp, pod_cs.max_shield_hp, "Low-sec pod must take 0 Smartbomb damage under Safety Yellow");
    assert_eq!(pod_cs.ecm_jammed_remaining_s, 0.0, "Low-sec pod must not be ECM-burst jammed under Safety Yellow");
    assert!(!ctx.crimewatch.read().unwrap().states.get(&ls_c).is_some_and(|st| st.is_criminal(ctx.now_unix())));

    ctx.advance_virtual_clock(35);
    let ls_sec_before = ctx.db.get_character(ls_c).unwrap().security_status;
    rpc(&ctx, &mut ls_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut ls_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_203)]).await.expect("Safety Red permits low-sec Burst Jammer near pod");
    rpc(&ctx, &mut ls_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_202)]).await.expect("Safety Red permits low-sec Smartbomb near pod");
    assert!(ctx.crimewatch.read().unwrap().states[&ls_c].is_criminal(ctx.now_unix()));
    assert!(ctx.db.get_character(ls_c).unwrap().security_status < ls_sec_before);

    rpc(&ctx, &mut atk_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_105), PyRep::Integer(ally_s)]).await.expect("Safety Red permits repping Criminal in high-sec");
    let atk_cw = ctx.crimewatch.read().unwrap().states[&atk_c].clone();
    assert!(atk_cw.is_criminal(ctx.now_unix()) && atk_cw.pending_concord_arrival.is_some(), "Repping Criminal in high-sec on Red must schedule CONCORD");
    rpc(&ctx, &mut atk_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_101), PyRep::Integer(vic_s)]).await.expect("Safety Red permits high-sec PvP");

    // 3. R1 negative test: out-of-range or no-charge activation on a neutral leaves crimewatch flags and security status unchanged
    let (neg_c, neg_vic, neg_s, neg_vs) = (94_000_031, 94_000_032, 84_000_031, 84_000_032);
    let mut neg_sess = spawn_pilot(&ctx, 831, neg_c, 1_000_009, neg_s, 626, jita, DVec3::ZERO).await;
    let _neg_vic_sess = spawn_pilot(&ctx, 832, neg_vic, 1_000_010, neg_vs, 587, jita, DVec3::new(30_000.0, 0.0, 0.0)).await;
    fit_online(&ctx, neg_c, neg_s, 84_000_301, 447, InventoryFlag::MedSlot0, None); // Warp Scrambler I
    fit_online(&ctx, neg_c, neg_s, 84_000_302, 529, InventoryFlag::HiSlot1, None); // Small Remote Capacitor Transmitter I (falloff 0)
    lock_target(&ctx, &mut neg_sess, jita, neg_vs).await;
    rpc(&ctx, &mut neg_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    let neg_sec_before = ctx.db.get_character(neg_c).unwrap().security_status;

    // Out of range offensive module (30 km >> 9 km) -> fails with TargetOutOfRange; crimewatch flags and security status untouched
    let oor_atk = rpc(&ctx, &mut neg_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_301), PyRep::Integer(neg_vs)]).await;
    assert_eq!(oor_atk.unwrap_err().code, "TargetOutOfRange");
    assert_eq!(ctx.db.get_character(neg_c).unwrap().security_status, neg_sec_before);
    assert!(!ctx.crimewatch.read().unwrap().states.get(&neg_c).is_some_and(|st| st.is_criminal(ctx.now_unix())));

    // Out of range remote assistance module (30 km >> 6 km, falloff 0) -> fails with TargetOutOfRange; crimewatch flags and sec status untouched
    let oor_res = rpc(&ctx, &mut neg_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_302), PyRep::Integer(neg_vs)]).await;
    assert_eq!(oor_res.unwrap_err().code, "TargetOutOfRange");
    assert_eq!(ctx.db.get_character(neg_c).unwrap().security_status, neg_sec_before);
    assert!(!ctx.crimewatch.read().unwrap().states.get(&neg_c).is_some_and(|st| st.is_criminal(ctx.now_unix())));

    // In-range but drained capacitor -> fails with NotEnoughCapacitor; crimewatch flags and sec status untouched
    let _ = ctx.systems.get_or_spawn(jita).set_ball_position(neg_vs, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let _ = ctx.systems.get_or_spawn(jita).set_capacitor(neg_s, 0.0).await;
    let cap_res = rpc(&ctx, &mut neg_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(84_000_301), PyRep::Integer(neg_vs)]).await;
    assert_eq!(cap_res.unwrap_err().code, "NotEnoughCapacitor");
    assert_eq!(ctx.db.get_character(neg_c).unwrap().security_status, neg_sec_before);
    assert!(!ctx.crimewatch.read().unwrap().states.get(&neg_c).is_some_and(|st| st.is_criminal(ctx.now_unix())));
}

}

pub(crate) mod weapons_timer_drone_bandwidth_and_corp_structures {
//! End-to-end integration tests closing Crimewatch movement, Drones, Fighters, and Corporation Structure rows:
//! - `feat:crimewatch.weapons-timer-dock-jump-gate`
//! - `feat:lifecycle.crimewatch-highsec-criminal-movement`
//! - `feat:drones.bandwidth-and-bay-constraints`
//! - `feat:drones.sentry-drone-stationary-tracking`
//! - `feat:drones.drone-npc-aggro-switching`
//! - `feat:drones.carrier-launch-tubes-and-flight-control`
//! - `feat:corporation.corp-structure-ownership`

use super::crimewatch_drones_lifecycle::{fit_online, lock_target, new_ctx, rpc, seed_item, spawn_pilot};
use eve_db::{CorporationRecord, InventoryFlag, TrainedSkillRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_server::services::combat_crimewatch::{spawn_specialized_npc_entity, with_combat_runtime};
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_crimewatch_weapons_timer_and_highsec_criminal_movement_restrictions() {
    // parity: feat:crimewatch.weapons-timer-dock-jump-gate, feat:lifecycle.crimewatch-highsec-criminal-movement
    eve_testkit::attest(&[
        "feat:crimewatch.weapons-timer-dock-jump-gate",
        "feat:lifecycle.crimewatch-highsec-criminal-movement",
    ]);

    let ctx = new_ctx();
    let jita = 30_000_142u32;
    let lowsec = 30_002_537u32;

    // 1. Verify turret, missile launcher, EWAR/neut, smartbomb, bomb launcher, and drone `CmdEngage` each initiate a 60s Weapons Timer
    {
        let nullsec = 30_004_759u32;
        let (a_id, b_id, sa_id, sb_id) = (95_000_001, 95_000_002, 85_000_001, 85_000_002);
        let mut sa_sess = spawn_pilot(&ctx, 901, a_id, 1_000_009, sa_id, 626, nullsec, DVec3::ZERO).await;
        let _sb_sess = spawn_pilot(&ctx, 902, b_id, 1_000_010, sb_id, 587, nullsec, DVec3::new(1_000.0, 0.0, 0.0)).await;
        fit_online(&ctx, a_id, sa_id, 85_000_101, 486, InventoryFlag::HiSlot0, Some((85_000_102, 185, 50)));
        fit_online(&ctx, a_id, sa_id, 85_000_103, 501, InventoryFlag::HiSlot1, Some((85_000_104, 209, 50)));
        fit_online(&ctx, a_id, sa_id, 85_000_105, 447, InventoryFlag::MedSlot0, None);
        fit_online(&ctx, a_id, sa_id, 85_000_106, 3953, InventoryFlag::HiSlot2, None);
        fit_online(&ctx, a_id, sa_id, 85_000_107, 27914, InventoryFlag::HiSlot3, Some((85_000_108, 27920, 5)));
        seed_item(&ctx, 85_000_109, 2454, a_id, sa_id, InventoryFlag::DroneBay, 1, true);
        lock_target(&ctx, &mut sa_sess, nullsec, sb_id).await;
        rpc(&ctx, &mut sa_sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(85_000_109)])]).await.unwrap();
        let drone_ball = with_combat_runtime(&ctx, |rt| rt.active_drones_in_space.keys().copied().next().unwrap());

        for (label, svc, method, args) in [
            ("turret", "dogmaIM", "Activate", vec![PyRep::Integer(85_000_101), PyRep::Integer(sb_id)]),
            ("missile", "dogmaIM", "Activate", vec![PyRep::Integer(85_000_103), PyRep::Integer(sb_id)]),
            ("ewar", "dogmaIM", "Activate", vec![PyRep::Integer(85_000_105), PyRep::Integer(sb_id)]),
            ("smartbomb", "dogmaIM", "Activate", vec![PyRep::Integer(85_000_106)]),
            ("bomb", "dogmaIM", "Activate", vec![PyRep::Integer(85_000_107)]),
            ("drone", "entity", "CmdEngage", vec![PyRep::List(vec![PyRep::Integer(drone_ball)]), PyRep::Integer(sb_id)]),
        ] {
            ctx.advance_virtual_clock(61); // Let previous 60s Weapons Timer expire naturally
            let now = ctx.now_unix();
            rpc(&ctx, &mut sa_sess, svc, None, method, &args).await.unwrap_or_else(|e| panic!("{label} failed: {e:?}"));
            let wt = ctx.crimewatch.read().unwrap().states[&a_id].weapon_timer_until_unix;
            assert_eq!(wt, now + 60, "{label} must initiate a 60-second Weapons Timer");
        }

        let (npc_hunter_c, npc_hunter_s) = (95_000_003, 85_000_003);
        let mut hunter_sess = spawn_pilot(&ctx, 903, npc_hunter_c, 1_000_009, npc_hunter_s, 626, nullsec, DVec3::ZERO).await;
        fit_online(&ctx, npc_hunter_c, npc_hunter_s, 85_000_120, 486, InventoryFlag::HiSlot0, Some((85_000_121, 185, 50)));
        fit_online(&ctx, npc_hunter_c, npc_hunter_s, 85_000_122, 501, InventoryFlag::HiSlot1, Some((85_000_123, 209, 50)));
        let npc_bid = spawn_specialized_npc_entity(&ctx, nullsec, 10004, "Pirate", 1, DVec3::new(2_000.0, 0.0, 0.0)).await;
        lock_target(&ctx, &mut hunter_sess, nullsec, npc_bid).await;
        rpc(&ctx, &mut hunter_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(85_000_120), PyRep::Integer(npc_bid)]).await.unwrap();
        rpc(&ctx, &mut hunter_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(85_000_122), PyRep::Integer(npc_bid)]).await.unwrap();
        let wt_npc = ctx.crimewatch.read().unwrap().states.get(&npc_hunter_c).map(|s| s.weapon_timer_until_unix).unwrap_or(0);
        assert_eq!(wt_npc, 0, "Attacking an NPC with turrets or missiles must NOT initiate a Weapons Timer");
    }

    // 2a. High-sec Criminal Timer (acquired organically via pod EWAR in low-sec Agoze 30_003_787 -> stargate 50_004_205 -> Intaki 30_003_788):
    //     non-capsule hull blocked from warp, stargate, wormhole, jump drive/bridge/portal, station/structure dock, eject, and board!
    let (agoze, intaki, agoze_gate, intaki_gate, intaki_station) = (30_003_787u32, 30_003_788u32, 50_004_205i64, 50_004_430i64, 60_009_385i64);
    let agoze_gate_pos = DVec3::new(-2_769_606_942_720.0, 116_952_145_920.0, 2_253_235_445_760.0);
    let (c_id, pod_vic_id, hs_vic_id) = (95_000_010, 95_000_011, 95_000_012);
    let (ship_id, pod_vic_s, hs_vic_s, spare_ship_id, intaki_struct) = (85_000_010, 85_000_011, 85_000_012, 85_000_013, 1_040_000_000_950i64);
    let mut sess = spawn_pilot(&ctx, 910, c_id, 1_000_009, ship_id, 587, agoze, agoze_gate_pos).await;
    let _pod_vic = spawn_pilot(&ctx, 911, pod_vic_id, 1_000_010, pod_vic_s, 670, agoze, agoze_gate_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    fit_online(&ctx, c_id, ship_id, 85_000_201, 447, InventoryFlag::MedSlot0, None);
    fit_online(&ctx, c_id, ship_id, 85_000_202, 486, InventoryFlag::HiSlot0, Some((85_000_203, 185, 50)));
    lock_target(&ctx, &mut sess, agoze, pod_vic_s).await;
    rpc(&ctx, &mut sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    rpc(&ctx, &mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(85_000_201), PyRep::Integer(pod_vic_s)]).await.unwrap();
    ctx.advance_virtual_clock(61); // 60s Weapons Timer expires naturally; 839s of live Criminal Timer remains
    let agoze_b = ctx.bind_object(&sess, "beyonce", agoze as i64).unwrap();
    rpc(&ctx, &mut sess, "beyonce", Some(agoze_b), "CmdStargateJump", &[PyRep::Integer(agoze_gate)]).await.expect("Low-sec criminal jumps into Intaki");
    assert_eq!(sess.solar_system_id, Some(intaki));
    ctx.advance_virtual_clock(12); // Clear 10s post-jump session change timer while 827s of Criminal Timer remains

    let _ = rpc(&ctx, &mut sess, "scanMgr", None, "GetSystemScanMgr", &[]).await.unwrap();
    let (intaki_wh_sig, intaki_wh_pos) = ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&intaki].iter().find(|s| s.sig_group == "Wormhole").unwrap();
        (sig.signature_id.clone(), sig.position)
    });
    let intaki_wh_id = eve_server::services::space_destiny::wormhole_item_id_for_signature(intaki, &intaki_wh_sig);
    let intaki_pos = DVec3::new(intaki_wh_pos[0] + 400.0, intaki_wh_pos[1], intaki_wh_pos[2]);
    let intaki_h = ctx.systems.get_or_spawn(intaki);
    let _ = intaki_h.set_ball_position(ship_id, intaki_pos).await;
    seed_item(&ctx, spare_ship_id, 587, c_id, intaki as i64, InventoryFlag::None, 1, true);
    let _ = intaki_h.spawn_ball_with_combat(
        Ball { id: spare_ship_id, type_id: 587, position: intaki_pos + DVec3::new(300.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 2.0, mass: 1_200_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_type_combat_state(587),
    ).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(intaki_struct, UpwellStructureRecord {
            structure_id: intaki_struct, type_id: 35832, solar_system_id: intaki, owner_corp_id: 1_000_009,
            name: "Intaki Astrahus".into(), position: intaki_pos.to_array(), state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 100, shield_hp: 100_000.0, armor_hp: 100_000.0, hull_hp: 100_000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
    });

    let intaki_b = ctx.bind_object(&sess, "beyonce", intaki as i64).unwrap();
    assert_eq!(rpc(&ctx, &mut sess, "beyonce", Some(intaki_b), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(intaki_station), PyRep::Real(0.0)]).await.unwrap_err().code, "CantWarpAwayGlobalCriminalFlag");
    assert_eq!(rpc(&ctx, &mut sess, "beyonce", Some(intaki_b), "CmdStargateJump", &[PyRep::Integer(intaki_gate)]).await.unwrap_err().code, "CantJumpGlobalCriminalFlag");
    assert_eq!(rpc(&ctx, &mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(intaki_wh_id)]).await.unwrap_err().code, "CantEnterWormholeGlobalCriminalFlag");
    assert_eq!(rpc(&ctx, &mut sess, "beyonce", Some(intaki_b), "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(c_id), PyRep::Integer(lowsec as i64), PyRep::Integer(1)]).await.unwrap_err().code, "CantBeaconJumpGlobalCriminalFlag");
    assert_eq!(rpc(&ctx, &mut sess, "beyonce", Some(intaki_b), "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(c_id), PyRep::Integer(lowsec as i64), PyRep::Integer(1)]).await.unwrap_err().code, "CantBeaconJumpGlobalCriminalFlag");
    assert_eq!(rpc(&ctx, &mut sess, "beyonce", Some(intaki_b), "CmdJumpThroughFleet", &[PyRep::Integer(c_id), PyRep::Integer(ship_id)]).await.unwrap_err().code, "CantBeaconJumpGlobalCriminalFlag");
    assert_eq!(rpc(&ctx, &mut sess, "stationSvc", None, "Dock", &[PyRep::Integer(intaki_station)]).await.unwrap_err().code, "CantDockGlobalCriminalFlag");
    assert_eq!(rpc(&ctx, &mut sess, "structureDocking", None, "Dock", &[PyRep::Integer(intaki_struct)]).await.unwrap_err().code, "CantDockGlobalCriminalFlag");
    assert_eq!(rpc(&ctx, &mut sess, "ship", None, "Eject", &[]).await.unwrap_err().code, "CannotEjectInvolvedInCrime");
    assert_eq!(rpc(&ctx, &mut sess, "ship", None, "Board", &[PyRep::Integer(spare_ship_id)]).await.unwrap_err().code, "CannotBoardShipInHighSecAsCriminal");

    // CONCORD executes `ship_id` in Intaki (`0.6 -> 14s`) via `tick_background_cluster`, placing `c_id` into a real Capsule (`670`)!
    for _ in 0..61 {
        let _ = intaki_h.step_manual(1.0).await;
    }
    let _hs_vic = spawn_pilot(&ctx, 912, hs_vic_id, 1_000_010, hs_vic_s, 587, intaki, intaki_pos + DVec3::new(800.0, 0.0, 0.0)).await;
    lock_target(&ctx, &mut sess, intaki, hs_vic_s).await;
    rpc(&ctx, &mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(85_000_202), PyRep::Integer(hs_vic_s)]).await.unwrap();
    ctx.advance_virtual_clock(14);
    tick_background_cluster(&ctx).await;
    let pod_id = ctx.db.get_character(c_id).unwrap().active_ship_id;
    assert_eq!(ctx.db.get_item(pod_id).unwrap().type_id, 670);
    sess.ship_id = Some(pod_id);
    ctx.advance_virtual_clock(47); // Let 60s Weapons Timer expire while >800s of Criminal Timer remains on the real Capsule (`670`)
    assert!(ctx.crimewatch.read().unwrap().states[&c_id].is_criminal(ctx.now_unix()));
    assert_eq!(rpc(&ctx, &mut sess, "ship", None, "Board", &[PyRep::Integer(spare_ship_id)]).await.unwrap_err().code, "CannotBoardShipInHighSecAsCriminal");
    rpc(&ctx, &mut sess, "beyonce", Some(intaki_b), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(intaki_station), PyRep::Real(0.0)]).await.expect("Criminal capsule CAN warp in high-sec");

    // 2b. Non-criminal 60s Weapons Timer (acquired organically via live duel turret fire in Jita):
    let (w_id, d_id, ws_id, ds_id, w_spare, jita_struct) = (95_000_020, 95_000_021, 85_000_020, 85_000_021, 85_000_022, 1_040_000_000_951i64);
    let mut w_sess = spawn_pilot(&ctx, 920, w_id, 1_000_009, ws_id, 587, jita, DVec3::ZERO).await;
    let mut d_sess = spawn_pilot(&ctx, 921, d_id, 1_000_010, ds_id, 587, jita, DVec3::ZERO).await;
    let _ = rpc(&ctx, &mut w_sess, "scanMgr", None, "GetSystemScanMgr", &[]).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let sigs = snap.cosmic_signatures.entry(jita).or_default();
        if !sigs.iter().any(|s| s.sig_group == "Wormhole") {
            sigs.push(eve_db::CosmicSignatureRecord {
                signature_id: "JMM-142".into(),
                solar_system_id: jita,
                sig_group: "Wormhole".into(),
                type_name: "Unstable Wormhole (K162)".into(),
                position: [149_597_870_700.0, 0.0, 0.0],
                signal_strength: 1.0,
                wormhole_type_id: Some(26_272),
                destination_system_id: Some(31_000_005),
                created_unix: ctx.now_unix(),
                lifetime_seconds: 86_400,
                max_mass_kg: 2_000_000_000.0,
                remaining_mass_kg: 2_000_000_000.0,
                max_jump_mass_kg: 375_000_000.0,
                collapsed: false,
            });
        }
    });
    let (wh_sig, wh_pos) = ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&jita].iter().find(|s| s.sig_group == "Wormhole").unwrap();
        (sig.signature_id.clone(), sig.position)
    });
    let wh_item_id = eve_server::services::space_destiny::wormhole_item_id_for_signature(jita, &wh_sig);
    let jita_gate = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == jita).cloned().unwrap();
    let station_id = 60_003_760i64;
    let pos = DVec3::new(wh_pos[0] + 400.0, wh_pos[1], wh_pos[2]);
    let sys_h = ctx.systems.get_or_spawn(jita);
    let _ = sys_h.set_ball_position(ws_id, pos).await;
    let _ = sys_h.set_ball_position(ds_id, pos + DVec3::new(800.0, 0.0, 0.0)).await;
    fit_online(&ctx, w_id, ws_id, 85_000_301, 486, InventoryFlag::HiSlot0, Some((85_000_302, 185, 50)));
    lock_target(&ctx, &mut w_sess, jita, ds_id).await;
    seed_item(&ctx, w_spare, 587, w_id, jita as i64, InventoryFlag::None, 1, true);
    let _ = sys_h.spawn_ball_with_combat(
        Ball { id: w_spare, type_id: 587, position: pos + DVec3::new(300.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 2.0, mass: 1_200_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        ctx.build_type_combat_state(587),
    ).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(jita_struct, UpwellStructureRecord {
            structure_id: jita_struct, type_id: 35832, solar_system_id: jita, owner_corp_id: 1_000_009,
            name: "Gatehouse Astrahus".into(), position: pos.to_array(), state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 100, shield_hp: 100_000.0, armor_hp: 100_000.0, hull_hp: 100_000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
    });

    rpc(&ctx, &mut w_sess, "crimewatch", None, "StartDuelChallenge", &[PyRep::Integer(d_id)]).await.unwrap();
    rpc(&ctx, &mut d_sess, "crimewatch", None, "RespondToDuelChallenge", &[PyRep::Integer(w_id), PyRep::Bool(true)]).await.unwrap();
    rpc(&ctx, &mut w_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(85_000_301), PyRep::Integer(ds_id)]).await.unwrap();

    let beyonce = ctx.bind_object(&w_sess, "beyonce", jita as i64).unwrap();
    assert_eq!(rpc(&ctx, &mut w_sess, "stationSvc", None, "Dock", &[PyRep::Integer(station_id)]).await.unwrap_err().code, "WeaponsTimerActive");
    assert_eq!(rpc(&ctx, &mut w_sess, "structureDocking", None, "Dock", &[PyRep::Integer(jita_struct)]).await.unwrap_err().code, "WeaponsTimerActive");
    assert_eq!(rpc(&ctx, &mut w_sess, "beyonce", Some(beyonce), "CmdStargateJump", &[PyRep::Integer(jita_gate.stargate_id)]).await.unwrap_err().code, "CannotJumpWithWeaponsTimer");
    assert_eq!(rpc(&ctx, &mut w_sess, "beyonce", Some(beyonce), "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(w_id), PyRep::Integer(lowsec as i64), PyRep::Integer(1)]).await.unwrap_err().code, "CannotJumpWithWeaponsTimer");
    assert_eq!(rpc(&ctx, &mut w_sess, "beyonce", Some(beyonce), "CmdBridgeToFleetModuleBeacon", &[PyRep::Integer(w_id), PyRep::Integer(lowsec as i64), PyRep::Integer(1)]).await.unwrap_err().code, "CannotJumpWithWeaponsTimer");
    assert_eq!(rpc(&ctx, &mut w_sess, "beyonce", Some(beyonce), "CmdJumpThroughFleet", &[PyRep::Integer(w_id), PyRep::Integer(ws_id)]).await.unwrap_err().code, "CannotJumpWithWeaponsTimer");
    assert_eq!(rpc(&ctx, &mut w_sess, "structureCynoBeaconMgr", None, "CmdJumpToStructureBeacon", &[PyRep::Integer(lowsec as i64), PyRep::Integer(jita_struct)]).await.unwrap_err().code, "CannotJumpWithWeaponsTimer");
    assert_eq!(rpc(&ctx, &mut w_sess, "ship", None, "Eject", &[]).await.unwrap_err().code, "CannotEjectActiveWeaponsTimer");
    assert_eq!(rpc(&ctx, &mut w_sess, "ship", None, "Board", &[PyRep::Integer(w_spare)]).await.unwrap_err().code, "CannotStoreShipActiveWeaponsTimer");

    // Once 60s Weapons Timer expires, docking succeeds; undock, re-trigger 60s Weapons Timer via live turret fire, and verify WormholeJump
    ctx.advance_virtual_clock(61);
    rpc(&ctx, &mut w_sess, "structureDocking", None, "Dock", &[PyRep::Integer(jita_struct)]).await.expect("Dock succeeds after 60s Weapons Timer expires");
    ctx.advance_virtual_clock(12);
    rpc(&ctx, &mut w_sess, "structureDocking", None, "Undock", &[]).await.expect("Undock");
    let beyonce = ctx.bind_object(&w_sess, "beyonce", jita as i64).unwrap();
    rpc(&ctx, &mut w_sess, "beyonce", Some(beyonce), "CmdStop", &[]).await.expect("Stop after undock");
    let _ = sys_h.set_ball_position(ws_id, pos).await;
    lock_target(&ctx, &mut w_sess, jita, ds_id).await;
    rpc(&ctx, &mut w_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(85_000_301), PyRep::Integer(ds_id)]).await.unwrap();

    // Per live EVE V24.01 ground truth (`eve-wiki/articles/Wormholes.md:68`: "Weapon timers do not affect
    // travel through wormholes"), `feat:navigation.wormhole-aggression-exemption`, and `reviewer_brief.md`
    // ground-truth precedence, a non-criminal 60s Weapons Timer permits `wormholeMgr.WormholeJump`
    // (whereas high-sec Criminal Timer above rejected `WormholeJump` with `CantEnterWormholeGlobalCriminalFlag`).
    rpc(&ctx, &mut w_sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)]).await.expect("WormholeJump allowed under non-criminal Weapons Timer");
    assert_eq!(w_sess.solar_system_id, Some(31_000_005));
}

#[tokio::test]
async fn test_drones_bandwidth_sentry_and_npc_aggro_switching_end_to_end() {
    // parity: feat:drones.bandwidth-and-bay-constraints
    // parity: feat:drones.sentry-drone-stationary-tracking
    // parity: feat:drones.drone-npc-aggro-switching
    eve_testkit::attest(&[
        "feat:drones.bandwidth-and-bay-constraints",
        "feat:drones.sentry-drone-stationary-tracking",
        "feat:drones.drone-npc-aggro-switching",
    ]);

    let ctx = new_ctx();
    let sys_id = 30_000_142u32;
    let c1 = 96_000_001i64;
    let vexor_id = 86_000_001i64;
    let mut sess = spawn_pilot(&ctx, 1001, c1, 1_000_009, vexor_id, 626, sys_id, DVec3::ZERO).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);

    // 1. Bandwidth, bay capacity (125 m3 on Vexor), and 5-drone skill cap
    let (ogre_1, ogre_2, ogre_3, garde_id, extra_ogre, cargo_ogre) = (86_000_010, 86_000_011, 86_000_012, 86_000_013, 86_000_014, 86_000_015);
    for id in [ogre_1, ogre_2, ogre_3, extra_ogre] {
        seed_item(&ctx, id, 2444, c1, vexor_id, InventoryFlag::DroneBay, 1, true);
    }
    seed_item(&ctx, garde_id, 23561, c1, vexor_id, InventoryFlag::DroneBay, 1, true);
    seed_item(&ctx, cargo_ogre, 2444, c1, vexor_id, InventoryFlag::Cargo, 1, true);
    let inv_bound = ctx.bind_object(&sess, "invbroker", vexor_id).unwrap();
    assert_eq!(rpc(&ctx, &mut sess, "invbroker", Some(inv_bound), "Add", &[PyRep::Integer(cargo_ogre), PyRep::Integer(vexor_id), PyRep::Integer(87)]).await.unwrap_err().code, "CapacityExceeded");

    ctx.db.with_mut_snapshot(|s| { s.skills.entry(c1).or_default().remove(&3436); });
    ctx.invalidate_ship_fitting(vexor_id);
    assert_eq!(rpc(&ctx, &mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(ogre_1)])]).await.unwrap_err().code, "NoDroneManagementAbilities");

    ctx.db.with_mut_snapshot(|s| { s.skills.entry(c1).or_default().insert(3436, TrainedSkillRecord { type_id: 3436, level: 1, skill_points: 250.0 }); });
    ctx.invalidate_ship_fitting(vexor_id);
    assert_eq!(rpc(&ctx, &mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(ogre_1), PyRep::Integer(ogre_2)])]).await.unwrap_err().code, "NoDroneManagementAbilitiesLeft");

    ctx.db.with_mut_snapshot(|s| { s.skills.entry(c1).or_default().insert(3436, TrainedSkillRecord { type_id: 3436, level: 5, skill_points: 256_000.0 }); });
    ctx.invalidate_ship_fitting(vexor_id);
    rpc(&ctx, &mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(garde_id)])]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(ogre_1), PyRep::Integer(ogre_2), PyRep::Integer(ogre_3)])]).await.unwrap_err().code, "MaxBandwidthExceeded");

    // 2. Sentry drone stationary tracking (`Garde I` 23561 stays at 0 m/s; >2500m scoop/return rejected)
    let garde_ball = with_combat_runtime(&ctx, |rt| rt.active_drones_in_space.values().find(|d| d.type_id == 23561).unwrap().ball_id);
    let (_, garde_ball_snap) = sys_h.get_ball_snapshot(garde_ball).await.unwrap().unwrap();
    assert_eq!(garde_ball_snap.max_velocity, 0.0, "Sentry drone must have max_velocity == 0");

    let _ = rpc(&ctx, &mut sess, "entity", None, "CmdReturnAndOrbit", &[PyRep::List(vec![PyRep::Integer(garde_ball)])]).await.expect("CmdReturnAndOrbit on sentry drone succeeds");
    let (_, garde_after_snap) = sys_h.get_ball_snapshot(garde_ball).await.unwrap().unwrap();
    assert_eq!(garde_after_snap.max_velocity, 0.0, "Sentry drone remains stationary at 0 m/s");

    // Sentry drone optimal, falloff, and tracking turret formula hit-chance evaluation
    let (hit_opt, hit_falloff, hit_transversal) = with_combat_runtime(&ctx, |rt| {
        let d = rt.active_drones_in_space.get(&garde_ball).unwrap();
        let d_ball = Ball { id: garde_ball, type_id: 23561, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1000.0, radius: 10.0, speed_fraction: 0.0, mode: MovementMode::Stop };
        let falloff = d.effective_falloff_range(&ctx);
        let t_opt = Ball { id: 10004, type_id: 10004, position: DVec3::new(d.optimal_range, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
        let t_falloff = Ball { id: 10004, type_id: 10004, position: DVec3::new(d.optimal_range + falloff, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
        let t_trans = Ball { id: 10004, type_id: 10004, position: DVec3::new(d.optimal_range, 0.0, 0.0), velocity: DVec3::new(0.0, 5000.0, 0.0), max_velocity: 5000.0, inertia: 1.0, mass: 1000.0, radius: 25.0, speed_fraction: 1.0, mode: MovementMode::Stop };
        (
            d.compute_combat_hit_multiplier(&ctx, &d_ball, &t_opt, 50.0),
            d.compute_combat_hit_multiplier(&ctx, &d_ball, &t_falloff, 50.0),
            d.compute_combat_hit_multiplier(&ctx, &d_ball, &t_trans, 25.0),
        )
    });
    assert!((hit_opt - 1.0).abs() < 1e-3, "Stationary target at optimal has 1.0 hit chance");
    assert!((hit_falloff - 0.5).abs() < 0.05, "Target at optimal + falloff has ~0.5 hit chance");
    assert!(hit_transversal < 0.25, "High transversal tracking penalty drops hit chance");

    let _ = sys_h.set_ball_position(vexor_id, DVec3::new(4_000.0, 0.0, 0.0)).await;
    assert_eq!(rpc(&ctx, &mut sess, "entity", None, "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(garde_ball)])]).await.unwrap_err().code, "SentryDroneOutOfScoopRange");
    let _ = sys_h.set_ball_position(vexor_id, DVec3::ZERO).await;
    rpc(&ctx, &mut sess, "ship", None, "ScoopToDroneBay", &[PyRep::Integer(garde_ball)]).await.unwrap();

    // 5-drone cap check at Drones V: 6x Hobgoblin I (30 Mbit/s <= 75 Mbit/s) rejected with NoDroneManagementAbilitiesLeft
    ctx.db.with_mut_snapshot(|s| { for id in [ogre_1, ogre_2, ogre_3, extra_ogre, garde_id] { s.items.remove(&id); } });
    let hobs: Vec<i64> = (0..6).map(|i| 86_000_100 + i).collect();
    for &hid in &hobs {
        seed_item(&ctx, hid, 2454, c1, vexor_id, InventoryFlag::DroneBay, 1, true);
    }
    assert_eq!(
        rpc(&ctx, &mut sess, "ship", None, "LaunchDrones", &[PyRep::List(hobs.iter().map(|&id| PyRep::Integer(id)).collect())]).await.unwrap_err().code,
        "NoDroneManagementAbilitiesLeft"
    );

    // 3a. Frigate (`593` Tristan, sig_radius 36m -> base threat ~109.0, 25 Mbit/s bandwidth) vs idle drone (~58.0): idle drone NEVER steals aggro from a Frigate!
    let frig_sys = 30_000_144u32; // Perimeter
    let (frig_c, tristan_id, frig_hob_item) = (96_000_050i64, 86_000_050i64, 86_000_051i64);
    let mut frig_sess = spawn_pilot(&ctx, 1002, frig_c, 1_000_009, tristan_id, 593, frig_sys, DVec3::ZERO).await;
    let frig_h = ctx.systems.get_or_spawn(frig_sys);
    seed_item(&ctx, frig_hob_item, 2454, frig_c, tristan_id, InventoryFlag::DroneBay, 1, true);
    let frig_npc = spawn_specialized_npc_entity(&ctx, frig_sys, 10004, "Pirate", 1, DVec3::new(1_200.0, 0.0, 0.0)).await;
    lock_target(&ctx, &mut frig_sess, frig_sys, frig_npc).await;
    ctx.advance_virtual_clock(1);
    tick_background_cluster(&ctx).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&frig_npc].target_id), Some(tristan_id));

    rpc(&ctx, &mut frig_sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(frig_hob_item)])]).await.unwrap();
    let frig_hob_ball = with_combat_runtime(&ctx, |rt| rt.active_drones_in_space.values().find(|d| d.owner_char_id == frig_c).unwrap().ball_id);
    let _ = frig_h.set_ball_position(frig_hob_ball, DVec3::new(1_000.0, 0.0, 0.0)).await;
    ctx.advance_virtual_clock(1);
    tick_background_cluster(&ctx).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&frig_npc].target_id), Some(tristan_id), "Idle drone must NOT steal aggro from a Frigate hull");

    rpc(&ctx, &mut frig_sess, "entity", None, "CmdEngage", &[PyRep::List(vec![PyRep::Integer(frig_hob_ball)]), PyRep::Integer(frig_npc)]).await.unwrap();
    ctx.advance_virtual_clock(1);
    tick_background_cluster(&ctx).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&frig_npc].target_id), Some(frig_hob_ball), "Engaging drone switches NPC aggro off Frigate");
    rpc(&ctx, &mut frig_sess, "entity", None, "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(frig_hob_ball)])]).await.unwrap();
    ctx.advance_virtual_clock(1);
    tick_background_cluster(&ctx).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&frig_npc].target_id), Some(tristan_id), "NPC returns aggro to Frigate after drone recall");

    // 3b. Cruiser (`626` Vexor) multi-drone threat ranking (Heavy `Ogre I` 2444 vs Light `Hobgoblin I` 2454) & real NPC damage application
    seed_item(&ctx, ogre_1, 2444, c1, vexor_id, InventoryFlag::DroneBay, 1, true);
    let npc_id = spawn_specialized_npc_entity(&ctx, sys_id, 10004, "Pirate", 1, DVec3::new(1_200.0, 0.0, 0.0)).await;
    lock_target(&ctx, &mut sess, sys_id, npc_id).await;

    ctx.advance_virtual_clock(1);
    tick_background_cluster(&ctx).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&npc_id].target_id), Some(vexor_id));

    rpc(&ctx, &mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(ogre_1), PyRep::Integer(hobs[0])])]).await.unwrap();
    let (ogre_ball, hob_ball) = with_combat_runtime(&ctx, |rt| {
        let ob = rt.active_drones_in_space.values().find(|d| d.type_id == 2444).unwrap().ball_id;
        let hb = rt.active_drones_in_space.values().find(|d| d.type_id == 2454).unwrap().ball_id;
        (ob, hb)
    });
    let _ = sys_h.set_ball_position(ogre_ball, DVec3::new(1_000.0, 0.0, 0.0)).await;
    let _ = sys_h.set_ball_position(hob_ball, DVec3::new(22_000.0, 0.0, 0.0)).await;
    ctx.advance_virtual_clock(1);
    tick_background_cluster(&ctx).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&npc_id].target_id), Some(vexor_id), "Idle drones must not steal NPC aggro");

    let ogre_cs_before = sys_h.get_combat_state(ogre_ball).await.unwrap().unwrap();
    rpc(&ctx, &mut sess, "entity", None, "CmdEngage", &[PyRep::List(vec![PyRep::Integer(ogre_ball), PyRep::Integer(hob_ball)]), PyRep::Integer(npc_id)]).await.unwrap();
    ctx.advance_virtual_clock(1);
    tick_background_cluster(&ctx).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&npc_id].target_id), Some(ogre_ball), "NPC must switch aggro onto highest-threat active drone");
    let ogre_cs_after = sys_h.get_combat_state(ogre_ball).await.unwrap().unwrap();
    assert!(ogre_cs_after.shield_hp < ogre_cs_before.shield_hp, "Attacking drone must take real NPC weapon damage");

    let _ = sys_h.set_ball_position(hob_ball, DVec3::new(500.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut sess, "entity", None, "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(ogre_ball), PyRep::Integer(hob_ball)])]).await.unwrap();
    assert_eq!(ctx.db.get_item(ogre_1).unwrap().flag, InventoryFlag::DroneBay);
    ctx.advance_virtual_clock(1);
    tick_background_cluster(&ctx).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&npc_id].target_id), Some(vexor_id), "NPC must switch aggro back to player ship when drones recall");
}

#[tokio::test]
async fn test_carrier_supercarrier_launch_tubes_and_flight_control() {
    // parity: feat:drones.carrier-launch-tubes-and-flight-control
    eve_testkit::attest(&["feat:drones.carrier-launch-tubes-and-flight-control"]);

    let ctx = new_ctx();
    let sys_id = 30_002_537u32; // Amamake (low-sec)
    let c1 = 97_000_001i64;
    let vanguard_id = 87_000_001i64;
    let archon_id = 87_000_002i64;
    let aeon_id = 87_000_003i64;

    // 1. 3-tube hull (`Vanguard` 42132: tubes 0..=2), 4-tube Carrier (`Archon` 23757: max 3 light), 5-tube Supercarrier (`Aeon` 23919: 0 support)
    let mut sess = spawn_pilot(&ctx, 1101, c1, 1_000_009, vanguard_id, 42132, sys_id, DVec3::ZERO).await;
    seed_item(&ctx, 87_000_101, 23057, c1, vanguard_id, InventoryFlag::FighterBay, 6, true);
    rpc(&ctx, &mut sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(2), PyRep::Integer(87_000_101)]).await.expect("3-tube hull tube 2 valid");
    assert_eq!(rpc(&ctx, &mut sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(3), PyRep::Integer(87_000_101)]).await.unwrap_err().code, "InvalidFighterTube");

    let mut aeon_sess = spawn_pilot(&ctx, 1102, c1, 1_000_009, aeon_id, 23919, sys_id, DVec3::ZERO).await;
    seed_item(&ctx, 87_000_201, 23057, c1, aeon_id, InventoryFlag::FighterBay, 6, true);
    seed_item(&ctx, 87_000_202, 40346, c1, aeon_id, InventoryFlag::FighterBay, 3, true);
    rpc(&ctx, &mut aeon_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(4), PyRep::Integer(87_000_201)]).await.expect("Supercarrier tube 4 valid");
    assert_eq!(rpc(&ctx, &mut aeon_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(5), PyRep::Integer(87_000_201)]).await.unwrap_err().code, "InvalidFighterTube");
    assert_eq!(rpc(&ctx, &mut aeon_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(87_000_202)]).await.unwrap_err().code, "CannotLoadFighterTubeNoTubesExistForClass");

    let mut archon_sess = spawn_pilot(&ctx, 1103, c1, 1_000_009, archon_id, 23757, sys_id, DVec3::ZERO).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    fit_online(&ctx, c1, archon_id, 87_000_399, 11370, InventoryFlag::HiSlot0, None); // Prototype Cloaking Device I
    seed_item(&ctx, 87_000_301, 23057, c1, archon_id, InventoryFlag::FighterBay, 15, false);
    for i in 1..4 {
        seed_item(&ctx, 87_000_301 + i, 23057, c1, archon_id, InventoryFlag::FighterBay, 6, true);
    }

    assert_eq!(rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await.unwrap_err().code, "EmptyFighterTube");
    rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(87_000_301)]).await.unwrap();
    assert_eq!(ctx.db.get_item(87_000_301).unwrap().quantity, 6, "Oversized fighter stack must clamp to maxSquadronSize (6)");
    rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(1), PyRep::Integer(87_000_302)]).await.unwrap();
    rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(2), PyRep::Integer(87_000_303)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(3), PyRep::Integer(87_000_304)]).await.unwrap_err().code, "CannotLoadFighterTubeNoTubesAvailableForClass");

    // 2. Launch forbidden while cloaked (`dogmaIM.Activate` 11370), tethered (`sync_tether` near friendly Fortizar), or in warp (`CmdWarpToStuff`)
    rpc(&ctx, &mut archon_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(87_000_399)]).await.expect("Activate Cloaking Device");
    assert_eq!(rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await.unwrap_err().code, "CannotOpenFighterTubesWhileCloaked");
    rpc(&ctx, &mut archon_sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(87_000_399)]).await.expect("Deactivate Cloaking Device");

    let tether_fort = 1_040_000_000_970i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(tether_fort, UpwellStructureRecord {
            structure_id: tether_fort, type_id: 35833, solar_system_id: sys_id, owner_corp_id: 1_000_009,
            name: "Carrier Fortizar".into(), position: [0.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 100, shield_hp: 100_000.0, armor_hp: 100_000.0, hull_hp: 100_000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
    });
    assert_eq!(rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await.unwrap_err().code, "DeniedTethered");
    assert!(sys_h.get_combat_state(archon_id).await.unwrap().unwrap().is_tethered, "Archon must be actively tethered to friendly Fortizar");

    let ama_beyonce = ctx.bind_object(&archon_sess, "beyonce", sys_id as i64).unwrap();
    rpc(&ctx, &mut archon_sess, "beyonce", Some(ama_beyonce), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(60_002_590), PyRep::Real(0.0)]).await.expect("Archon enters warp");
    assert_eq!(rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await.unwrap_err().code, "CannotOpenFighterTubesWhileWarping");
    for _ in 0..120 {
        let _ = sys_h.step_manual(5.0).await;
        if matches!(sys_h.get_ball_snapshot(archon_id).await.unwrap().unwrap().1.mode, MovementMode::Stop) {
            break;
        }
    }
    let (_, archon_landed) = sys_h.get_ball_snapshot(archon_id).await.unwrap().unwrap();
    assert!(matches!(archon_landed.mode, MovementMode::Stop));

    // 3. Launch tube 0, expend a Slot 2 missile charge, suffer 1 fighter unit loss via live hostile Smartbomb (`dogmaIM.Activate`), then recall -> refills to 6 & restocks charges
    let npc_id = spawn_specialized_npc_entity(&ctx, sys_id, 10004, "Pirate", 1, archon_landed.position + DVec3::new(1_000.0, 0.0, 0.0)).await;
    lock_target(&ctx, &mut archon_sess, sys_id, npc_id).await;
    let PyRep::List(launched) = rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await.unwrap() else { panic!() };
    let f_ball = launched[0].as_i64().unwrap();
    let ab = rpc(&ctx, &mut archon_sess, "fighterMgr", None, "CmdActivateAbilitySlots", &[PyRep::List(vec![PyRep::Integer(f_ball)]), PyRep::Integer(2), PyRep::Integer(npc_id)]).await.unwrap();
    let max_ch = ab.dict_get("chargeCount").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(ab.dict_get("remainingCharges").and_then(|v| v.as_i64()), Some(max_ch - 1));
    assert!(!ctx.fighter_charge_state.read().unwrap().is_empty());

    let (_, fb_snap) = sys_h.get_ball_snapshot(f_ball).await.unwrap().unwrap();
    let mut sb_sess = spawn_pilot(&ctx, 1104, 97_000_099, 1_000_010, 87_000_099, 24692, sys_id, fb_snap.position + DVec3::new(200.0, 0.0, 0.0)).await;
    fit_online(&ctx, 97_000_099, 87_000_099, 87_000_499, 14550, InventoryFlag::HiSlot0, None);
    rpc(&ctx, &mut sb_sess, "crimewatch", None, "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    for _ in 0..6 {
        rpc(&ctx, &mut sb_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(87_000_499)]).await.unwrap();
        ctx.advance_virtual_clock(11);
    }
    assert_eq!(ctx.active_fighters.read().unwrap()[&f_ball].squadron_size, 5, "Live Smartbomb damage must destroy 1 fighter unit (5 remaining)");

    rpc(&ctx, &mut archon_sess, "fighterMgr", None, "RecallFightersToTubes", &[PyRep::List(vec![PyRep::Integer(f_ball)])]).await.unwrap();
    assert_eq!(ctx.fighter_tube_loads.read().unwrap().get(&(archon_id, 0)).copied(), Some(87_000_301), "Squadron must land back in tube 0");
    assert_eq!(ctx.db.get_item(87_000_301).unwrap().quantity, 6, "Recalled squadron must refill lost unit from FighterBay reserve");
    assert!(ctx.fighter_charge_state.read().unwrap().is_empty(), "Recalled squadron must restock ability charges");
    assert_eq!(rpc(&ctx, &mut archon_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await.unwrap_err().code, "TubeRearming");
}

#[tokio::test]
async fn test_corporation_structure_ownership_and_station_manager_roles() {
    // parity: feat:corporation.corp-structure-ownership
    eve_testkit::attest(&["feat:corporation.corp-structure-ownership"]);

    let ctx = new_ctx();
    let sys_id = 30_000_144u32;
    let corp_id = 98_500_001i64;
    let other_corp = 98_500_002i64;
    let (ceo_id, mgr_id, outsider_id) = (98_000_001i64, 98_000_002i64, 98_000_003i64);
    let (ceo_ship, mgr_ship, out_ship) = (88_000_001i64, 88_000_002i64, 88_000_003i64);

    let mut ceo_sess = spawn_pilot(&ctx, 1201, ceo_id, corp_id, ceo_ship, 648, sys_id, DVec3::new(0.0, 5.0e9, 0.0)).await;
    let mut mgr_sess = spawn_pilot(&ctx, 1202, mgr_id, corp_id, mgr_ship, 648, sys_id, DVec3::new(500.0, 5.0e9, 0.0)).await;
    let mut out_sess = spawn_pilot(&ctx, 1203, outsider_id, other_corp, out_ship, 648, sys_id, DVec3::new(800.0, 5.0e9, 0.0)).await;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ceo_roles = HashMap::new();
        ceo_roles.insert(ceo_id, eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER);
        snap.corporations.insert(corp_id, CorporationRecord {
            corporation_id: corp_id, name: "Citadel Owners Corp".into(), ticker: "OWNR".into(), ceo_id,
            members: std::collections::HashSet::from([ceo_id, mgr_id]), member_roles: ceo_roles,
            ..Default::default()
        });
        snap.corporations.insert(other_corp, CorporationRecord {
            corporation_id: other_corp, name: "Outsider Corp".into(), ticker: "OUTS".into(), ceo_id: outsider_id,
            members: std::collections::HashSet::from([outsider_id]),
            ..Default::default()
        });
    });
    ceo_sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;

    // 1. Anchoring: unprivileged member (`roles == 0`) is denied; CEO anchors Astrahus (`35832`) -> `owner_corp_id == corp_id`
    seed_item(&ctx, 88_000_101, 35832, mgr_id, mgr_ship, InventoryFlag::Cargo, 1, false);
    assert_eq!(rpc(&ctx, &mut mgr_sess, "structureDeployment", None, "Anchor", &[PyRep::Integer(88_000_101), PyRep::String("Denied Citadel".into())]).await.unwrap_err().code, "InsufficientCorpRoles");

    seed_item(&ctx, 88_000_102, 35832, ceo_id, ceo_ship, InventoryFlag::Cargo, 1, false);
    let anchor_rep = rpc(&ctx, &mut ceo_sess, "structureDeployment", None, "Anchor", &[PyRep::Integer(88_000_102), PyRep::String("Corp Astrahus".into())]).await.expect("CEO anchors structure");
    let struct_id = anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&struct_id].owner_corp_id), corp_id, "Corporation must own the anchored structure");

    // Complete anchoring + quantum core onlining so fuel, profiles, and service modules can be managed
    use eve_server::services::station_structure::structure_lifecycle as life;
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS);
    rpc(&ctx, &mut ceo_sess, "structureGuests", None, "GetGuests", &[PyRep::Integer(struct_id)]).await.unwrap();
    let st_pos = DVec3::from_array(ctx.db.with_snapshot(|s| s.structures[&struct_id].position));
    for sh in [ceo_ship, mgr_ship, out_ship] { assert!(ctx.systems.get_or_spawn(sys_id).set_ball_position(sh, st_pos).await); }
    rpc(&ctx, &mut ceo_sess, "structureDocking", None, "Dock", &[PyRep::Integer(struct_id)]).await.unwrap();
    let core_tid = ctx.sde.types[&35832].default_attributes[&life::ATTR_REQUIRES_DEED_TYPE] as u32;
    seed_item(&ctx, 88_000_103, core_tid, ceo_id, ceo_ship, InventoryFlag::Cargo, 1, true);
    let ceo_inv = ctx.bind_object(&ceo_sess, "invbroker", struct_id).unwrap();
    rpc(&ctx, &mut ceo_sess, "invbroker", Some(ceo_inv), "Add", &[PyRep::Integer(88_000_103), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::StructureDeedBay as i64)]).await.unwrap();
    rpc(&ctx, &mut ceo_sess, "structureGuests", None, "GetGuests", &[PyRep::Integer(struct_id)]).await.unwrap();
    ctx.advance_virtual_clock(life::ONLINING_SECS);
    rpc(&ctx, &mut ceo_sess, "structureGuests", None, "GetGuests", &[PyRep::Integer(struct_id)]).await.unwrap();

    rpc(&ctx, &mut mgr_sess, "structureDocking", None, "Dock", &[PyRep::Integer(struct_id)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut out_sess, "structureDocking", None, "Dock", &[PyRep::Integer(struct_id)]).await.unwrap_err().code, "StructureDockingAccessDenied");

    // 2. CEO loads initial fuel & fits a corp service module (`88_000_212` in `ServiceSlot1`); unprivileged corp member (`roles == 0`) is denied profiles, SpecializedFuelBay (`Add` & `MultiAdd` in/out), and service module fit/unfit/destroy (`Add`, `MultiAdd`, `DestroyFitting`)
    seed_item(&ctx, 88_000_199, 4247, ceo_id, struct_id, InventoryFlag::Hangar, 1_500, false);
    seed_item(&ctx, 88_000_212, 35894, corp_id, struct_id, InventoryFlag::Hangar, 1, true);
    rpc(&ctx, &mut ceo_sess, "invbroker", Some(ceo_inv), "Add", &[PyRep::Integer(88_000_199), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)]).await.unwrap();
    rpc(&ctx, &mut ceo_sess, "invbroker", Some(ceo_inv), "Add", &[PyRep::Integer(88_000_212), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::ServiceSlot1 as i64)]).await.unwrap();
    rpc(&ctx, &mut ceo_sess, "dogmaIM", None, "SetModuleOnline", &[PyRep::Integer(struct_id), PyRep::Integer(88_000_212)]).await.unwrap();
    seed_item(&ctx, 88_000_201, 4247, mgr_id, struct_id, InventoryFlag::Hangar, 1_200, false);
    seed_item(&ctx, 88_000_202, 35894, mgr_id, struct_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, 88_000_203, 4247, outsider_id, struct_id, InventoryFlag::Hangar, 500, false);
    seed_item(&ctx, 88_000_204, 35894, outsider_id, struct_id, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, 88_000_205, 34, mgr_id, struct_id, InventoryFlag::Hangar, 100, false); // Tritanium (non-fuel)
    seed_item(&ctx, 88_000_206, 4247, mgr_id, struct_id, InventoryFlag::Hangar, 400, false);

    assert_eq!(rpc(&ctx, &mut mgr_sess, "structureDeployment", None, "RenameStructure", &[PyRep::Integer(struct_id), PyRep::String("NoRole".into())]).await.unwrap_err().code, "InsufficientCorpRoles");
    assert_eq!(rpc(&ctx, &mut mgr_sess, "structureVulnerability", None, "UpdateReinforceTimesFromClient", &[PyRep::Integer(struct_id), PyRep::Integer(18), PyRep::Integer(2)]).await.unwrap_err().code, "InsufficientCorpRoles");
    assert_eq!(rpc(&ctx, &mut mgr_sess, "structureSettings", None, "CreateProfile", &[PyRep::String("Ops".into()), PyRep::String("Desc".into())]).await.unwrap_err().code, "InsufficientCorpRoles");
    // Seed fuel stack after structure sync RPCs (`burn_fuel` absorbs `SpecializedFuelBay` stacks into `st.fuel_blocks` on sync)
    seed_item(&ctx, 88_000_210, 4247, corp_id, struct_id, InventoryFlag::SpecializedFuelBay, 300, false);
    let mgr_inv = ctx.bind_object(&mgr_sess, "invbroker", struct_id).unwrap();
    assert_eq!(rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "Add", &[PyRep::Integer(88_000_201), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)]).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(88_000_201)]), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)]).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "Add", &[PyRep::Integer(88_000_210), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::Hangar as i64)]).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(88_000_210)]), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::Hangar as i64)]).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(ctx.db.get_item(88_000_210).unwrap().flag, InventoryFlag::SpecializedFuelBay);
    assert_eq!(rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "Add", &[PyRep::Integer(88_000_202), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await.unwrap_err().code, "InsufficientCorpRoles");
    assert_eq!(rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "Add", &[PyRep::Integer(88_000_212), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::Hangar as i64)]).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(88_000_212)]), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::Hangar as i64)]).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(rpc(&ctx, &mut mgr_sess, "dogmaIM", None, "DestroyFitting", &[PyRep::Integer(88_000_212)]).await.unwrap_err().code, "InsufficientCorpRoles");
    assert_eq!(ctx.db.get_item(88_000_212).unwrap().flag, InventoryFlag::ServiceSlot1);

    // 3. CEO grants `ROLE_STATION_MANAGER` (`1024`, without Director) to `mgr_id` via `corpRegistry.UpdateMember`
    rpc(
        &ctx, &mut ceo_sess, "corpRegistry", None, "UpdateMember",
        &[PyRep::Integer(mgr_id), PyRep::Integer(eve_db::corp_roles::ROLE_STATION_MANAGER as i64)],
    ).await.expect("Grant ROLE_STATION_MANAGER");
    rpc(&ctx, &mut mgr_sess, "structureDeployment", None, "RenameStructure", &[PyRep::Integer(struct_id), PyRep::String("StationMgr Citadel".into())]).await.expect("Station Manager RenameStructure");
    rpc(&ctx, &mut mgr_sess, "structureVulnerability", None, "UpdateReinforceTimesFromClient", &[PyRep::Integer(struct_id), PyRep::Integer(18), PyRep::Integer(2)]).await.expect("Station Manager UpdateReinforceTimesFromClient");

    // Note: V24.01 `code.ccp` `eveProto/generated/eve/structure/profile_pb2.pyj` defines only `Identifier` (0 Quasar RPCs);
    // V24 structure profile CRUD runs via MachoNet `structureProfiles` (`structureProfileController.pyj` / `allProfilesController.pyj`)
    // paired with catalog `structureSettings` (`CharacterHasService` / `CharacterHasSetting`).
    let prof_id = rpc(&ctx, &mut mgr_sess, "structureSettings", None, "CreateProfile", &[PyRep::String("Industrial ACL".into()), PyRep::String("Managed by Station Manager".into())]).await.expect("Station Manager CreateProfile").as_i64().unwrap();
    rpc(&ctx, &mut mgr_sess, "structureSettings", None, "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(struct_id)]), PyRep::Integer(prof_id)]).await.expect("Station Manager ChangeProfiles");
    assert_eq!(rpc(&ctx, &mut out_sess, "structureSettings", None, "CharacterHasService", &[PyRep::Integer(struct_id), PyRep::Integer(35894)]).await.unwrap(), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut out_sess, "structureSettings", None, "CharacterHasSetting", &[PyRep::Integer(struct_id), PyRep::Integer(1)]).await.unwrap(), PyRep::Bool(true));

    rpc(&ctx, &mut out_sess, "structureDocking", None, "Dock", &[PyRep::Integer(struct_id)]).await.expect("Public profile assigned by Station Manager allows outsider docking");
    seed_item(&ctx, 88_000_211, 4247, corp_id, struct_id, InventoryFlag::SpecializedFuelBay, 300, false);
    let out_inv = ctx.bind_object(&out_sess, "invbroker", struct_id).unwrap();
    assert_eq!(rpc(&ctx, &mut out_sess, "invbroker", Some(out_inv), "Add", &[PyRep::Integer(88_000_203), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)]).await.unwrap_err().code, "StructureOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut out_sess, "invbroker", Some(out_inv), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(88_000_203)]), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)]).await.unwrap_err().code, "StructureOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut out_sess, "invbroker", Some(out_inv), "Add", &[PyRep::Integer(88_000_211), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::Hangar as i64)]).await.unwrap_err().code, "StructureOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut out_sess, "invbroker", Some(out_inv), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(88_000_211)]), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::Hangar as i64)]).await.unwrap_err().code, "StructureOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut out_sess, "invbroker", Some(out_inv), "Add", &[PyRep::Integer(88_000_204), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await.unwrap_err().code, "StructureOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut out_sess, "invbroker", Some(out_inv), "Add", &[PyRep::Integer(88_000_212), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::Hangar as i64)]).await.unwrap_err().code, "StructureOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut out_sess, "invbroker", Some(out_inv), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(88_000_212)]), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::Hangar as i64)]).await.unwrap_err().code, "ItemOwnershipViolation");
    assert_eq!(rpc(&ctx, &mut out_sess, "dogmaIM", None, "DestroyFitting", &[PyRep::Integer(88_000_212)]).await.unwrap_err().code, "StructureOwnershipViolation");

    // Negative tests for R-DF-01: Director cannot destroy personal hangar items; outsider can destroy their own
    seed_item(&ctx, 88_000_250, 34, outsider_id, struct_id, InventoryFlag::Hangar, 10, false);
    seed_item(&ctx, 88_000_251, 34, mgr_id, struct_id, InventoryFlag::Hangar, 10, false);
    assert_eq!(rpc(&ctx, &mut ceo_sess, "dogmaIM", None, "DestroyFitting", &[PyRep::Integer(88_000_250)]).await.unwrap_err().code, "ItemNotOwnedByCharacter");
    assert!(ctx.db.get_item(88_000_250).is_some(), "Outsider hangar item remains intact");
    assert_eq!(rpc(&ctx, &mut ceo_sess, "dogmaIM", None, "DestroyFitting", &[PyRep::Integer(88_000_251)]).await.unwrap_err().code, "ItemNotOwnedByCharacter");
    assert!(ctx.db.get_item(88_000_251).is_some(), "Corp member hangar item remains intact");
    rpc(&ctx, &mut out_sess, "dogmaIM", None, "DestroyFitting", &[PyRep::Integer(88_000_250)]).await.expect("Outsider destroys own hangar item");
    assert!(ctx.db.get_item(88_000_250).is_none(), "Outsider hangar item destroyed by owner");

    // Non-fuel item rejected in SpecializedFuelBay on both `Add` and `MultiAdd`
    assert_eq!(rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(88_000_205)]), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)]).await.unwrap_err().code, "InvalidInventoryFlag");

    // Station Manager loads personal fuel via `Add` (partial split + full move) and `MultiAdd`: `owner_id` transfers to `corp_id`!
    let mut split_kw = HashMap::new();
    split_kw.insert("qty".into(), PyRep::Integer(500));
    ctx.dispatch_rpc(&mut mgr_sess, "invbroker", Some(mgr_inv), "Add", &[PyRep::Integer(88_000_201), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)], &split_kw, None).await.expect("Station Manager split-loads fuel");
    rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "Add", &[PyRep::Integer(88_000_201), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)]).await.expect("Station Manager load SpecializedFuelBay");
    rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "MultiAdd", &[PyRep::List(vec![PyRep::Integer(88_000_206)]), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)]).await.expect("Station Manager MultiAdd SpecializedFuelBay");
    ctx.db.with_snapshot(|s| {
        let fuel_items: Vec<_> = s.items.values().filter(|i| i.location_id == struct_id && i.flag == InventoryFlag::SpecializedFuelBay).collect();
        assert!(!fuel_items.is_empty());
        assert!(fuel_items.iter().all(|i| i.owner_id == corp_id), "All fuel in SpecializedFuelBay must retain owning corp_id");
    });

    let _ = ctx.drain_notifications_for_char(mgr_id);
    rpc(&ctx, &mut mgr_sess, "invbroker", Some(mgr_inv), "Add", &[PyRep::Integer(88_000_202), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await.expect("Station Manager fits service module");
    rpc(&ctx, &mut mgr_sess, "dogmaIM", None, "TakeModuleOffline", &[PyRep::Integer(struct_id), PyRep::Integer(88_000_202)]).await.expect("Station Manager offlines service module");
    assert_eq!(ctx.db.with_snapshot(|s| s.module_online_states.get(&88_000_202).copied()), Some(false));
    rpc(&ctx, &mut mgr_sess, "dogmaIM", None, "SetModuleOnline", &[PyRep::Integer(struct_id), PyRep::Integer(88_000_202)]).await.expect("Station Manager onlines service module");
    assert_eq!(ctx.db.with_snapshot(|s| s.module_online_states.get(&88_000_202).copied()), Some(true));
    assert!(ctx.drain_notifications_for_char(mgr_id).iter().any(|n| n.event == "OnStructureServiceChanged"));
}

}
