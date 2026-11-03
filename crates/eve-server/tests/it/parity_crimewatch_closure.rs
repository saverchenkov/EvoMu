//! Closure tests for CrimeWatch Security Status Loss/Gains, Transactions,
//! Kill Rights Gain/Sell, and Clone Soldier Tag Tier Bands (`C-8`).
//!
//! Covers:
//! - `feat:crimewatch.sec-status-loss`
//! - `feat:crimewatch.sec-status-transactions`
//! - `feat:crimewatch.kill-rights-gain`
//! - `feat:crimewatch.kill-rights-sell`
//! - `feat:crimewatch.sec-status-gain-caps`
//! - `feat:crimewatch.sec-tag-tier-bands`

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::crimewatch::{
    compute_sec_status_loss, CrimeWatchDirector,
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
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.active_ship_id = sid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
        }
        sid
    })
}

#[tokio::test]
async fn test_crimewatch_sec_status_loss_and_kill_rights_e2e() {
    // parity: feat:crimewatch.sec-status-loss, feat:crimewatch.sec-status-transactions, feat:crimewatch.kill-rights-gain, feat:crimewatch.kill-rights-sell
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();

    let highsec_sys = 30_000_142_u32; // Jita (0.95)
    let lowsec_sys = 30_000_012_u32;  // Asabona (0.32)
    let jita_sec = ctx.sde.solar_systems[&highsec_sys].security_status;
    let lowsec_sec = ctx.sde.solar_systems[&lowsec_sys].security_status;

    let criminal_id = 90_000_101_i64;
    let victim_id = 90_000_102_i64;
    let buyer_id = 90_000_103_i64;
    let victim2_id = 90_000_104_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for &(cid, uid, name, sec, bal) in &[
            (criminal_id, 101, "Criminal Pilot", 0.0, 50_000_000.0),
            (victim_id, 102, "Victim Pilot", 5.0, 10_000_000.0),
            (buyer_id, 103, "Buyer Pilot", 0.0, 20_000_000.0),
            (victim2_id, 104, "Victim2 Pilot", 5.0, 10_000_000.0),
        ] {
            if let Some(mut ch) = snap.characters.get(&90_000_001).cloned() {
                ch.character_id = cid;
                ch.user_id = uid;
                ch.name = name.to_string();
                ch.security_status = sec;
                ch.balance = IskCents::from_f64_isk(bal);
                ch.solar_system_id = highsec_sys;
                ch.station_id = None;
                snap.characters.insert(cid, ch);
            }
        }
    });

    // 1. Exact mathematical formula verification for security status loss
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

    // Nullsec yields zero sec loss
    assert_eq!(compute_sec_status_loss(0.0, 5.0, false, 0.0), 0.0);
    assert_eq!(compute_sec_status_loss(0.0, 5.0, true, -0.5), 0.0);
    // At or below -10.0, no further loss
    assert_eq!(compute_sec_status_loss(-10.0, 5.0, true, 0.95), 0.0);

    // 2. Highsec illegal aggression triggers sec-status-loss, sec-status-transactions, and kill-rights-gain
    let crim_ship = spawn_ship(&ctx, criminal_id, highsec_sys, 587, DVec3::ZERO);
    let vic_ship = spawn_ship(&ctx, victim_id, highsec_sys, 587, DVec3::new(1_000.0, 0.0, 0.0));
    let buyer_ship = spawn_ship(&ctx, buyer_id, highsec_sys, 587, DVec3::new(2_000.0, 0.0, 0.0));

    let jita_handle = ctx.systems.get_or_spawn(highsec_sys);
    for (id, pos) in [(crim_ship, DVec3::ZERO), (vic_ship, DVec3::new(1_000.0, 0.0, 0.0)), (buyer_ship, DVec3::new(2_000.0, 0.0, 0.0))] {
        let mut cs = ctx.build_type_combat_state(587);
        cs.shield_hp = 500.0;
        let _ = jita_handle.spawn_ball_with_combat(Ball {
            id, type_id: 587, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
            inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }, cs).await;
    }

    let aggr_res = eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, criminal_id, crim_ship, highsec_sys, victim_id, false,
    );
    assert!(aggr_res.is_ok(), "evaluate_and_record_pvp_aggression in highsec must succeed");

    // Re-attack within same engagement must not double-penalize
    let aggr_repeat = eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, criminal_id, crim_ship, highsec_sys, victim_id, false,
    );
    assert!(aggr_repeat.is_ok());

    let mut cw = CrimeWatchDirector::new();
    cw.states = ctx.crimewatch.read().unwrap().states.clone();
    let concord_reports = cw.step_concord(&ctx, now + 10, 42).await;
    assert_eq!(concord_reports.len(), 1, "CONCORD must respond to criminal aggression in highsec");
    let hs_kr_id = concord_reports[0].kill_right_id;
    assert!(hs_kr_id > 0, "CONCORD response must award a kill right against highsec criminal");

    let sec_after = ctx.db.with_snapshot(|s| s.characters[&criminal_id].security_status);
    let expected_sec = 0.0 - expected_ship_loss;
    assert!(
        (sec_after - expected_sec).abs() < 1e-6,
        "Security status must decrease once by exact loss: got {sec_after}, expected {expected_sec}"
    );

    // Verify kill right attributes earned on highsec unprovoked attack
    let (kr_owner, kr_target, kr_fee, kr_exp) = ctx.db.with_snapshot(|s| {
        let kr = s.kill_rights.get(&hs_kr_id).expect("kill right in db");
        let exp = s.combat_crimewatch_state.kill_right_expires_unix.get(&hs_kr_id).copied().unwrap_or(0);
        (kr.owner_id, kr.target_id, kr.activation_fee.0, exp)
    });
    assert_eq!(kr_owner, victim_id, "Kill right owner must be the victim");
    assert_eq!(kr_target, criminal_id, "Kill right target must be the aggressor");
    assert_eq!(kr_fee, 0, "Earned kill right starts private with zero activation fee");
    assert_eq!(kr_exp, now + 30 * 86_400, "Kill right must carry exactly 30-day expiration");

    // 3. Lowsec Rules: ship aggression grants Suspect flag & NO kill right; pod aggression awards kill right
    let lowsec_handle = ctx.systems.get_or_spawn(lowsec_sys);
    let low_crim_ship = spawn_ship(&ctx, criminal_id, lowsec_sys, 587, DVec3::ZERO);
    let low_vic_ship = spawn_ship(&ctx, victim_id, lowsec_sys, 587, DVec3::new(500.0, 0.0, 0.0));
    let low_vic_pod = spawn_ship(&ctx, victim2_id, lowsec_sys, 670, DVec3::new(600.0, 0.0, 0.0));

    for (id, tid, pos) in [(low_crim_ship, 587, DVec3::ZERO), (low_vic_ship, 587, DVec3::new(500.0, 0.0, 0.0)), (low_vic_pod, 670, DVec3::new(600.0, 0.0, 0.0))] {
        let cs = ctx.build_type_combat_state(tid);
        let _ = lowsec_handle.spawn_ball_with_combat(Ball {
            id, type_id: tid, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
            inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }, cs).await;
    }

    // Unprovoked ship aggression in lowsec: gives suspect flag, deducts sec, NO kill right
    let kr_count_pre = ctx.db.with_snapshot(|s| s.kill_rights.len());
    let _ = eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, criminal_id, low_crim_ship, lowsec_sys, victim_id, false,
    );
    let kr_count_after_ship = ctx.db.with_snapshot(|s| s.kill_rights.len());
    assert_eq!(kr_count_after_ship, kr_count_pre, "Lowsec ship aggression must not award a kill right");

    // Unprovoked pod aggression in lowsec against victim2: awards a 30-day kill right
    let _ = eve_server::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
        &ctx, criminal_id, low_crim_ship, lowsec_sys, victim2_id, true,
    );
    let (kr_count_after_pod, low_pod_kr_id) = ctx.db.with_snapshot(|s| {
        let kr = s.kill_rights.values().find(|k| k.owner_id == victim2_id && k.target_id == criminal_id).unwrap();
        (s.kill_rights.len(), kr.kill_right_id)
    });
    assert_eq!(kr_count_after_pod, kr_count_pre + 1, "Lowsec pod aggression must award a kill right");
    let low_kr_exp = with_combat_runtime(&ctx, |rt| rt.kill_right_expires_unix.get(&low_pod_kr_id).copied()).unwrap();
    assert_eq!(low_kr_exp, now + 30 * 86_400, "Lowsec pod kill right carries 30-day expiration");

    // 4. Verify GetSecurityStatusTransactions audit log
    let mut sess_crim = SessionState::default();
    sess_crim.user_id = Some(101);
    sess_crim.character_id = Some(criminal_id);
    sess_crim.solar_system_id = Some(highsec_sys);

    let tx_res = rpc(&ctx, &mut sess_crim, "crimewatch", "GetSecurityStatusTransactions", &[])
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
    assert_eq!(row0.get("otherOwnerID"), Some(&&PyRep::Integer(victim_id)), "otherOwnerID must identify the victim");

    // 5. Kill rights sell, cancel, buy, and activate lifecycle
    let mut sess_vic = SessionState::default();
    sess_vic.user_id = Some(102);
    sess_vic.character_id = Some(victim_id);
    sess_vic.solar_system_id = Some(highsec_sys);

    // Victim lists kill right for 2,000,000 ISK
    let list_res = rpc(
        &ctx,
        &mut sess_vic,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(hs_kr_id), PyRep::Real(2_000_000.0)],
    )
    .await
    .expect("SellKillRight");
    if let PyRep::List(items) = list_res {
        assert_eq!(field(&items[0], "forSale"), PyRep::Bool(true));
        assert_eq!(field(&items[0], "price"), PyRep::Real(2_000_000.0));
    } else {
        panic!("expected List from SellKillRight");
    }

    // Victim cancels sale
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

    // Re-list for 3,000,000 ISK
    let _ = rpc(
        &ctx,
        &mut sess_vic,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(hs_kr_id), PyRep::Real(3_000_000.0)],
    )
    .await
    .expect("SellKillRight re-list");

    // Error checks on purchase
    ctx.db.with_mut_snapshot(|s| {
        let crim = s.characters.get_mut(&criminal_id).unwrap();
        crim.solar_system_id = highsec_sys;
        crim.active_ship_id = crim_ship;
        crim.station_id = None;
    });

    let mut sess_crim_buy = SessionState::default();
    sess_crim_buy.user_id = Some(101);
    sess_crim_buy.character_id = Some(criminal_id);
    sess_crim_buy.solar_system_id = Some(highsec_sys);
    let self_buy_err = rpc(&ctx, &mut sess_crim_buy, "killRightMgr", "BuyKillRight", &[PyRep::Integer(hs_kr_id)]).await;
    assert!(self_buy_err.unwrap_err().contains("CannotBuyKillRightAgainstSelf"));

    let owner_buy_err = rpc(&ctx, &mut sess_vic, "killRightMgr", "BuyKillRight", &[PyRep::Integer(hs_kr_id)]).await;
    assert!(owner_buy_err.unwrap_err().contains("AlreadyKillRightOwner"));

    // Third-party pilot purchases and activates kill right in space
    let mut sess_buyer = SessionState::default();
    sess_buyer.user_id = Some(103);
    sess_buyer.character_id = Some(buyer_id);
    sess_buyer.solar_system_id = Some(highsec_sys);

    let buy_res = rpc(&ctx, &mut sess_buyer, "killRightMgr", "BuyKillRight", &[PyRep::Integer(hs_kr_id)])
        .await
        .expect("BuyKillRight");
    assert_eq!(field(&buy_res, "suspectFlagActive"), PyRep::Bool(true));
    assert_eq!(field(&buy_res, "feePaidISK"), PyRep::Real(3_000_000.0));

    // Verify ISK transfer: buyer debited 3M, victim credited 3M
    let (vic_bal, buyer_bal) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&victim_id].balance.to_wire_f64(),
            s.characters[&buyer_id].balance.to_wire_f64(),
        )
    });
    assert!((vic_bal - 13_000_000.0).abs() < 1e-2, "Victim balance must receive 3M ISK fee");
    assert!((buyer_bal - 17_000_000.0).abs() < 1e-2, "Buyer balance must be charged 3M ISK fee");

    // Target pilot has active 15-minute Suspect flag
    let suspect_exp = ctx.crimewatch.read().unwrap().states[&criminal_id].suspect_flag_until_unix;
    assert_eq!(suspect_exp, now + 900, "BuyKillRight must apply a 15-minute suspect flag to target");

    // Owner direct activation of lowsec pod kill right in space
    ctx.db.with_mut_snapshot(|s| {
        let vic2 = s.characters.get_mut(&victim2_id).unwrap();
        vic2.solar_system_id = lowsec_sys;
        vic2.active_ship_id = low_vic_pod;
        vic2.station_id = None;

        let crim = s.characters.get_mut(&criminal_id).unwrap();
        crim.solar_system_id = lowsec_sys;
        crim.active_ship_id = low_crim_ship;
        crim.station_id = None;
    });
    let mut sess_vic2 = SessionState::default();
    sess_vic2.user_id = Some(104);
    sess_vic2.character_id = Some(victim2_id);
    sess_vic2.solar_system_id = Some(lowsec_sys);
    let act_res = rpc(&ctx, &mut sess_vic2, "killRightMgr", "ActivateKillRight", &[PyRep::Integer(low_pod_kr_id)])
        .await
        .expect("ActivateKillRight");
    assert_eq!(field(&act_res, "suspectFlagActive"), PyRep::Bool(true));
    assert_eq!(field(&act_res, "suspectFlagged"), PyRep::Bool(true));
}

#[tokio::test]
async fn test_crimewatch_sec_status_gain_caps_and_tag_tier_bands_e2e() {
    // parity: feat:crimewatch.sec-status-gain-caps, feat:crimewatch.sec-tag-tier-bands, feat:crimewatch.sec-status-transactions
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let now = ctx.now_unix();

    let char_id = 90_000_201_i64;

    // Find a lowsec station offering Security Office (service 27)
    let (lowsec_station_id, lowsec_sys_id) = ctx
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
            ch.character_id = char_id;
            ch.user_id = 201;
            ch.name = "SecTag Pilot".to_string();
            ch.security_status = -9.5;
            ch.balance = IskCents::from_f64_isk(100_000_000.0);
            ch.station_id = Some(lowsec_station_id);
            ch.solar_system_id = lowsec_sys_id;
            snap.characters.insert(char_id, ch);
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

    // 1. NPC Kill Security Status Gain Caps at +5.0
    let make_npc = |ball_id: i64| NpcCombatEntity {
        ball_id, type_id: 12000, faction_id: 500_010, state: NpcAiState::Idle,
        spawn_group_id: 1, spawn_pos: DVec3::ZERO, target_id: None, bounty_isk: 10_000_000.0,
        optimal_range: 10_000.0, orbit_range: 5_000.0, base_dps: 50.0,
        damage_vector: DamageVector { em: 10.0, thermal: 10.0, kinetic: 15.0, explosive: 15.0 },
        last_attack_tick: 0, has_shield_rep: false, has_armor_rep: false,
    };
    let mut npc_mgr = NpcSpawnManager::new();

    // Gain below +5.0 increases towards +5.0 cap
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().security_status = 4.95);
    npc_mgr.entities.insert(85_000_001, make_npc(85_000_001));
    let _ = npc_mgr.handle_npc_destroyed(&ctx, lowsec_sys_id, 85_000_001, char_id, now);
    let sec_gain = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!(sec_gain > 4.95 && sec_gain <= 5.0, "NPC kill sec gain must increase towards +5.0 cap (got {sec_gain})");

    // Gain that would exceed +5.0 caps exactly at +5.0
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().security_status = 4.9999);
    npc_mgr.entities.insert(85_000_002, make_npc(85_000_002));
    let _ = npc_mgr.handle_npc_destroyed(&ctx, lowsec_sys_id, 85_000_002, char_id, now);
    let sec_at_cap = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!((sec_at_cap - 5.0).abs() < 1e-6, "NPC kill sec gain must cap at +5.0 (got {sec_at_cap})");

    // Rat kill while already at +5.0 does not exceed +5.0
    npc_mgr.entities.insert(85_000_003, make_npc(85_000_003));
    let _ = npc_mgr.handle_npc_destroyed(&ctx, lowsec_sys_id, 85_000_003, char_id, now);
    let sec_still_cap = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!((sec_still_cap - 5.0).abs() < 1e-6, "NPC kill sec gain must not exceed +5.0");

    // Rat kill when sec > +5.0 (e.g. +6.0) does not clamp down to +5.0
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().security_status = 6.0);
    npc_mgr.entities.insert(85_000_004, make_npc(85_000_004));
    let _ = npc_mgr.handle_npc_destroyed(&ctx, lowsec_sys_id, 85_000_004, char_id, now);
    let sec_above = ctx.db.with_snapshot(|s| s.characters[&char_id].security_status);
    assert!((sec_above - 6.0).abs() < 1e-6, "NPC kill must preserve sec status above +5.0");

    // 2. Highsec station rejection
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(60_003_760);
        ch.solar_system_id = 30_000_142;
        ch.security_status = -9.0;
    });
    let mut sess = SessionState::default();
    sess.user_id = Some(201);
    sess.character_id = Some(char_id);
    sess.station_id = Some(60_003_760); // Jita 4-4 (highsec station)
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

    // 3. Lowsec Station: All 4 Clone Soldier Tag Bands, Fees, and Out-of-Band Refusal
    let bal_before = ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(lowsec_station_id);
        ch.solar_system_id = lowsec_sys_id;
        ch.balance
    });
    sess.station_id = Some(lowsec_station_id);
    sess.solar_system_id = Some(lowsec_sys_id);

    // Test matrix: (start_sec, valid_tag, invalid_tag, target_sec, expected_sec, fee_isk)
    // Tier 1 Trainer (33138): -10 to -8, 1M ISK
    // Tier 2 Recruiter (33139): -8 to -5, 2M ISK
    // Tier 3 Transporter/Ranger (33140): -5 to -2, 5M ISK
    // Tier 4 Negotiator (33141): -2 to 0, 10M ISK
    for (start_sec, valid_tag, invalid_tag, target_sec, expected_sec) in [
        (-9.0_f64, 33138_i64, 33139_i64, -8.5_f64, -8.5_f64),
        (-6.5_f64, 33139_i64, 33140_i64, -6.0_f64, -6.0_f64),
        (-3.5_f64, 33140_i64, 33141_i64, -3.0_f64, -3.0_f64),
        (-0.5_f64, 33141_i64, 33138_i64, 0.0_f64, 0.0_f64),
    ] {
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().security_status = start_sec);

        // Out-of-band refusal
        let bad_res = rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(target_sec), PyRep::Dict(vec![(PyRep::Integer(invalid_tag), PyRep::Integer(1))])],
        )
        .await;
        assert!(bad_res.unwrap_err().contains("SecurityTagBandMismatch"), "Out-of-band tag must be refused");

        // Valid in-band turn-in
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

    // 4. Tags cannot raise security status above 0.0 and excess tags are preserved
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().security_status = 0.0);
    let at_zero_err = rpc(
        &ctx,
        &mut sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(1))])],
    )
    .await;
    assert!(at_zero_err.unwrap_err().contains("InvalidTargetSecurityStatus"));

    // Partial consumption: starting at -0.2 with 5 Negotiator tags requested -> consumes only 1 tag to reach 0.0
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().security_status = -0.2);
    let excess_res = rpc(
        &ctx,
        &mut sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Real(0.0), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(5))])],
    )
    .await
    .expect("ApplyTagsForSecStatus with excess tags");
    assert_eq!(field(&excess_res, "newSecurityStatus"), PyRep::Real(0.0));

    // Verify 3 Negotiator tags remain (5 initial - 1 in band 4 - 1 in excess test = 3)
    let rem_neg_tags = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.owner_id == char_id && i.type_id == 33141).map(|i| i.quantity).sum::<i32>()
    });
    assert_eq!(rem_neg_tags, 3, "Excess tags beyond 0.0 cap must not be consumed");

    // Total fee: 1M + 2M + 5M + 10M + 10M = 28M ISK
    let bal_after = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    assert_eq!(bal_before.0 - bal_after.0, IskCents::from_isk(28_000_000).0, "Exact ISK fees must be charged");

    // Wallet journal entry for SecurityTagProcessing
    let has_journal = ctx.db.with_snapshot(|s| {
        s.wallet_journal.iter().any(|j| j.character_id == char_id && j.entry_type == "SecurityTagProcessing" && j.amount == IskCents::from_isk(-10_000_000))
    });
    assert!(has_journal, "Wallet journal SecurityTagProcessing entry must exist");

    // 5. Argument validation checks
    for bad_q in [0_i64, -1_i64, 4_294_966_296_i64] {
        let err = rpc(
            &ctx,
            &mut sess,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(-0.1), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(bad_q))])],
        )
        .await;
        assert!(err.unwrap_err().contains("InvalidQuantity"));
    }
    let bad_type_err = rpc(
        &ctx,
        &mut sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Real(-0.1), PyRep::Dict(vec![(PyRep::Integer(587), PyRep::Integer(1))])],
    )
    .await;
    assert!(bad_type_err.unwrap_err().contains("InvalidSecurityTagType"));

    // 6. Security status transaction log records tag turn-in
    let sec_tx = rpc(&ctx, &mut sess, "crimewatch", "GetSecurityStatusTransactions", &[])
        .await
        .expect("GetSecurityStatusTransactions");
    let PyRep::Rowset { columns, rows, .. } = sec_tx else {
        panic!("expected Rowset from GetSecurityStatusTransactions");
    };
    assert!(!rows.is_empty(), "Tag turn-in transactions must be recorded");
    let row0: HashMap<&str, &PyRep> = columns.iter().map(|s| s.as_str()).zip(rows[0].iter()).collect();
    assert_eq!(row0.get("eventTypeID"), Some(&&PyRep::Integer(385)), "Tag turn-in eventTypeID must be 385");
    assert_eq!(row0.get("locationID"), Some(&&PyRep::Integer(lowsec_sys_id as i64)));
    assert_eq!(row0.get("modification"), Some(&&PyRep::Real(0.5)));
    assert_eq!(row0.get("otherOwnerID"), Some(&&PyRep::None), "Tag turn-in otherOwnerID must be None");
}
