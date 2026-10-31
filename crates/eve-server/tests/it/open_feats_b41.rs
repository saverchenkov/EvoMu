//! Integration tests for batch 41 exclusive `feat:` parity rows:
//! - `feat:pve.insurgency-laundering`
//! - `feat:pve.insurgency-mining-ambush`
//! - `feat:pve.insurgency-outpost-raid`
//! - `feat:pve.proving-grounds-queue-and-rewards`

#![forbid(unsafe_code)]

use crate::fw_warzone_insurgency_closure::setup_warzone_chain;
use crate::sovereignty_equinox_gameplay_closure::{
    lock_target_via_rpc, place_ship_ball, register_online_pilot, rpc_call, set_pilot_location,
};
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_combat_runtime;
use eve_server::services::fw_services::{
    HQ_SYSTEM_AMO, HQ_SYSTEM_INTAKI, HQ_SYSTEM_MEHATOOR, HQ_SYSTEM_ONNAMON,
};
use eve_server::services::insurgency_complex_ops::{
    load_ops_state, OutpostRaidSize, LARGE_ADV_OUTPOST_RESPAWN_SECS, MINING_AMBUSH_BASE_LP,
    MINING_AMBUSH_RANSOM_SECS, MINING_AMBUSH_RESPAWN_SECS, TYPE_ANGEL_INSURGENCY_DATA_SEQ,
    TYPE_FEDERATION_CI_OUTPOST_HUB, TYPE_GURISTAS_INSURGENCY_DATA_SEQ,
    TYPE_GURISTAS_LAUNDERING_FACILITY, TYPE_IMPERIAL_CI_OUTPOST_HUB, TYPE_ORE_RORQUAL,
    TYPE_ORE_RORQUAL_WRECK, TYPE_REPUBLIC_CI_OUTPOST_HUB, TYPE_STATE_CI_OUTPOST_HUB,
    TYPE_UPWELL_CONSORTIUM_DATA_CACHE,
};
use eve_server::services::insurgency_services::{
    add_system_corruption, add_system_suppression, init_insurgency_campaign, FACTION_ANGEL_CARTEL,
    FORECASTING_DURATION_SECS,
};
use eve_server::services::pvp_filament_mgr::{get_pvp_filament_match_spec, load_pvp_store};
use glam::DVec3;
use std::collections::HashMap;

fn fast_forward_secs(ctx: &ServerContext, secs: u64) {
    ctx.clock().advance_secs(secs);
    let cur = ctx.now_unix_secs();
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert("__server_now_unix".into(), cur.to_string());
    });
}

fn fit_small_railgun(ctx: &ServerContext, char_id: i64, ship_id: i64, gun_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            gun_id,
            InventoryItemRecord {
                item_id: gun_id,
                type_id: 10_678, // 125mm Railgun I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            gun_id + 1,
            InventoryItemRecord {
                item_id: gun_id + 1,
                type_id: 222, // Antimatter Charge S
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 200,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(gun_id, true);
    });
}

async fn kill_npc_with_turret_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    ship_id: i64,
    gun_id: i64,
    npc_id: i64,
) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let npc_pos = sys_h.get_ball_snapshot(npc_id).await.unwrap().expect("NPC ball").1.position;
    let _ = sys_h.set_ball_position(ship_id, npc_pos + DVec3::new(1_200.0, 0.0, 0.0)).await;
    let mut cs = sys_h.get_combat_state(npc_id).await.unwrap().unwrap();
    (cs.shield_hp, cs.armor_hp, cs.structure_hp, cs.shield_recharge_time_ms) = (0.0, 0.0, 1.0, 1.0e15);
    let _ = sys_h.set_combat_state(npc_id, cs).await;
    let _ = rpc_call(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gun_id)]).await;
    lock_target_via_rpc(ctx, sess, sys_id, ship_id, npc_id).await;
    rpc_call(
        ctx,
        sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(gun_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_id)],
    )
    .await
    .expect("dogmaIM.Activate on NPC");
    for _ in 0..8 {
        sys_h.step_manual(1.0).await.unwrap();
        if sys_h.get_ball_snapshot(npc_id).await.unwrap().is_none() {
            break;
        }
    }
    assert!(sys_h.get_ball_snapshot(npc_id).await.unwrap().is_none());
    let _ = rpc_call(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gun_id)]).await;
}

async fn warp_ship_via_rpc(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, ship_id: i64, target_id: i64) {
    rpc_call(
        ctx,
        sess,
        "beyonce",
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(target_id), PyRep::Real(0.0)],
    )
    .await
    .expect("beyonce.CmdWarpToStuff");
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for _ in 0..60 {
        sys_h.step_manual(1.0).await.unwrap();
        if sys_h.get_ball_snapshot(ship_id).await.unwrap().is_some_and(|(_, b)| !matches!(b.mode, MovementMode::Warp { .. })) {
            break;
        }
    }
}

async fn bind_invbroker(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32) -> u64 {
    let bind = ctx
        .dispatch_rpc(
            sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Tuple(vec![PyRep::Integer(i64::from(sys_id)), PyRep::Integer(15)]), PyRep::None],
            &HashMap::new(),
            None,
        )
        .await
        .expect("bind invbroker");
    match bind {
        PyRep::Tuple(parts) => parts[0].as_i64().unwrap() as u64,
        PyRep::Integer(i) => i as u64,
        other => panic!("unexpected bind rep {other:?}"),
    }
}

async fn total_lp_via_rpc(ctx: &ServerContext, sess: &mut SessionState) -> i64 {
    let rep = rpc_call(ctx, sess, "LPSvc", "GetAllMyCharacterWalletLPBalances", &[])
        .await
        .expect("LPSvc.GetAllMyCharacterWalletLPBalances");
    let PyRep::List(rows) = rep else { return 0 };
    rows.iter()
        .filter_map(|r| r.dict_get("lp").and_then(PyRep::as_i64))
        .sum()
}

fn find_recipe_for_trader(ctx: &ServerContext, trader_type_id: u32, input_type_id: u32) -> (u32, u32) {
    let (recipes, _) = ctx.sde.item_trader_component_for_type(trader_type_id).unwrap().expect("itemTrader component");
    let table = ctx.sde.load_cfsd_nested("res:/staticdata/itemtraderrecipes.fsdbinary").unwrap();
    for rid in recipes {
        if let Some(rv) = table.get(&rid.to_string()) {
            let has_in = rv.get("inputItems").and_then(|v| v.as_object()).is_some_and(|m| m.contains_key(&input_type_id.to_string()));
            if has_in {
                let out_tid = rv.get("outputItems").and_then(|v| v.as_object()).and_then(|m| m.keys().next()).and_then(|k| k.parse::<u32>().ok()).unwrap();
                return (rid, out_tid);
            }
        }
    }
    panic!("no recipe on trader {trader_type_id} accepting {input_type_id}");
}

#[tokio::test]
async fn test_b41_insurgency_laundering_roamer_drops_fob_and_war_hq_crates() {
    // parity: feat:pve.insurgency-laundering
    eve_testkit::attest(&["feat:pve.insurgency-laundering"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, cmd_sys, rear_sys) = setup_warzone_chain(&ctx);

    let (pir_cid, pir_ship, emp_cid, emp_ship) = (91_100_001, 101_100_001, 91_100_002, 101_100_002);
    let mut s_pir = register_online_pilot(&ctx, 89_101, "b41_pir1", "pw", pir_cid, 98_100_001, None, front_sys, pir_ship, 621, DVec3::ZERO, 0).await;
    let mut s_emp = register_online_pilot(&ctx, 89_102, "b41_emp1", "pw", emp_cid, 98_100_002, None, front_sys, emp_ship, 621, DVec3::new(5_000.0, 0.0, 0.0), 0).await;
    rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)]).await.unwrap();
    fit_small_railgun(&ctx, pir_cid, pir_ship, 210_001);
    fit_small_railgun(&ctx, emp_cid, emp_ship, 210_011);

    let _ = rpc_call(&ctx, &mut s_emp, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let (gur_camp_id, gur_fob_sys) = ctx.db.with_snapshot(|s| {
        let (&cid, c) = s.insurgency_runtime.campaigns.iter().next().unwrap();
        (cid, c.fob_solar_system_id)
    });
    init_insurgency_campaign(&ctx, 7_702, FACTION_ANGEL_CARTEL, 500_003, rear_sys, vec![rear_sys, cmd_sys]);
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);

    // Raise front_sys to Corruption Stage 3 & Suppression Stage 3 so named roamers spawn.
    assert_eq!(add_system_corruption(&ctx, gur_camp_id, front_sys, 600).1, 3);
    assert_eq!(add_system_suppression(&ctx, gur_camp_id, front_sys, 600).1, 3);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let (named_pir_id, named_enf_id) = with_combat_runtime(&ctx, |rt| {
        let p = rt.npc_entities.values().find(|n| n.solar_system_id == front_sys && (79_358..=79_364).contains(&n.type_id)).expect("named Guristas roamer").ball_id;
        let e = rt.npc_entities.values().find(|n| n.solar_system_id == front_sys && matches!(n.type_id, 79_389 | 79_394 | 79_395 | 79_396)).expect("named Enforcer roamer").ball_id;
        (p, e)
    });

    kill_npc_with_turret_rpc(&ctx, &mut s_pir, front_sys, pir_ship, 210_001, named_enf_id).await;
    kill_npc_with_turret_rpc(&ctx, &mut s_emp, front_sys, emp_ship, 210_011, named_pir_id).await;

    let (enf_wreck_id, cache_item_id, pir_wreck_id, seq_item_id) = ctx.db.with_snapshot(|s| {
        let c = s.items.values().find(|i| i.type_id == TYPE_UPWELL_CONSORTIUM_DATA_CACHE).expect("78848 dropped in Enforcer wreck");
        let q = s.items.values().find(|i| i.type_id == TYPE_GURISTAS_INSURGENCY_DATA_SEQ).expect("78857 dropped in Guristas wreck");
        (c.location_id, c.item_id, q.location_id, q.item_id)
    });

    let sys_h = ctx.systems.get_or_spawn(front_sys);
    let ew_pos = sys_h.get_ball_snapshot(enf_wreck_id).await.unwrap().unwrap().1.position;
    let _ = sys_h.set_ball_position(pir_ship, ew_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    let mgr_pir = bind_invbroker(&ctx, &mut s_pir, front_sys).await;
    let pir_ship_inv = ctx.dispatch_rpc(&mut s_pir, "invbroker", Some(mgr_pir), "GetInventoryFromId", &[PyRep::Integer(pir_ship), PyRep::Integer(0)], &HashMap::new(), None).await.unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(&mut s_pir, "invbroker", Some(pir_ship_inv), "Add", &[PyRep::Integer(cache_item_id), PyRep::Integer(enf_wreck_id), PyRep::Integer(5)], &HashMap::new(), None).await.expect("loot 78848");

    let pw_pos = sys_h.get_ball_snapshot(pir_wreck_id).await.unwrap().unwrap().1.position;
    let _ = sys_h.set_ball_position(emp_ship, pw_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    let mgr_emp = bind_invbroker(&ctx, &mut s_emp, front_sys).await;
    let emp_ship_inv = ctx.dispatch_rpc(&mut s_emp, "invbroker", Some(mgr_emp), "GetInventoryFromId", &[PyRep::Integer(emp_ship), PyRep::Integer(0)], &HashMap::new(), None).await.unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(&mut s_emp, "invbroker", Some(emp_ship_inv), "Add", &[PyRep::Integer(seq_item_id), PyRep::Integer(pir_wreck_id), PyRep::Integer(5)], &HashMap::new(), None).await.expect("loot 78857");

    // Move Pirate & Empire pilots to Guristas FOB system and test >= 3 negative itemTrader RPCs.
    set_pilot_location(&ctx, &mut s_pir, gur_fob_sys, None);
    place_ship_ball(&ctx, gur_fob_sys, pir_cid, pir_ship, 621, DVec3::ZERO).await;
    set_pilot_location(&ctx, &mut s_emp, gur_fob_sys, None);
    place_ship_ball(&ctx, gur_fob_sys, emp_cid, emp_ship, 621, DVec3::new(1_150_500.0, 0.0, 45_000.0)).await;

    let lf_id = 1_891_100_000 + gur_camp_id;
    let (lf_recipe, lf_crate_tid) = find_recipe_for_trader(&ctx, TYPE_GURISTAS_LAUNDERING_FACILITY, TYPE_UPWELL_CONSORTIUM_DATA_CACHE);
    let trade_args = |tid: i64, rec: u32| [PyRep::Integer(tid), PyRep::String("itemTrader".into()), PyRep::String("ProcessTrade".into()), PyRep::Integer(rec as i64), PyRep::Integer(1)];

    assert_eq!(rpc_call(&ctx, &mut s_pir, "beyonce", "CallComponentFromClient", &trade_args(lf_id, lf_recipe)).await.unwrap_err().code, "ItemTraderOutOfRange");
    assert_eq!(rpc_call(&ctx, &mut s_emp, "beyonce", "CallComponentFromClient", &trade_args(lf_id, lf_recipe)).await.unwrap_err().code, "FactionNotAligned");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(1_899_999_001, InventoryItemRecord { item_id: 1_899_999_001, type_id: TYPE_STATE_CI_OUTPOST_HUB, owner_id: 500_001, location_id: i64::from(gur_fob_sys), flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default() });
        snap.item_positions.insert(1_899_999_001, [1_150_500.0, 0.0, 45_000.0]);
    });
    let (onnamon_recipe, onnamon_crate_tid) = find_recipe_for_trader(&ctx, TYPE_STATE_CI_OUTPOST_HUB, TYPE_GURISTAS_INSURGENCY_DATA_SEQ);
    assert_eq!(rpc_call(&ctx, &mut s_emp, "beyonce", "CallComponentFromClient", &trade_args(1_899_999_001, onnamon_recipe)).await.unwrap_err().code, "InvalidWarHQLocation");

    // Positive FOB Laundering hand-in: Pirate warps to FOB Laundering Facility and exchanges 78848 -> crate + LP + corruption.
    warp_ship_via_rpc(&ctx, &mut s_pir, gur_fob_sys, pir_ship, lf_id).await;
    let fob_corr_before = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&gur_camp_id].corruption_points.get(&gur_fob_sys).copied().unwrap_or(0));
    rpc_call(&ctx, &mut s_pir, "beyonce", "CallComponentFromClient", &trade_args(lf_id, lf_recipe)).await.expect("Pirate FOB Laundering hand-in");
    assert!(ctx.db.with_snapshot(|s| s.items.values().any(|i| i.location_id == pir_ship && i.flag == InventoryFlag::Cargo && i.type_id == lf_crate_tid)));
    assert!(total_lp_via_rpc(&ctx, &mut s_pir).await >= 2_500);
    assert!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&gur_camp_id].corruption_points.get(&gur_fob_sys).copied().unwrap_or(0)) > fob_corr_before);
    assert_eq!(rpc_call(&ctx, &mut s_pir, "beyonce", "CallComponentFromClient", &trade_args(lf_id, lf_recipe)).await.unwrap_err().code, "InsufficientInputItems");

    // Positive Empire War HQ Counter-Insurgency Outpost hand-ins in all 4 War HQs (Onnamon, Intaki, Mehatoor, Amo).
    set_pilot_location(&ctx, &mut s_emp, HQ_SYSTEM_ONNAMON, None);
    place_ship_ball(&ctx, HQ_SYSTEM_ONNAMON, emp_cid, emp_ship, 621, DVec3::ZERO).await;
    warp_ship_via_rpc(&ctx, &mut s_emp, HQ_SYSTEM_ONNAMON, emp_ship, 1_891_200_003).await;
    let fob_supp_before = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&gur_camp_id].suppression_points.get(&gur_fob_sys).copied().unwrap_or(0));
    rpc_call(&ctx, &mut s_emp, "beyonce", "CallComponentFromClient", &trade_args(1_891_200_003, onnamon_recipe)).await.expect("Onnamon Counter-Insurgency hand-in");
    assert!(ctx.db.with_snapshot(|s| s.items.values().any(|i| i.location_id == emp_ship && i.type_id == onnamon_crate_tid)));
    assert!(total_lp_via_rpc(&ctx, &mut s_emp).await >= 2_500);
    assert!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&gur_camp_id].suppression_points.get(&gur_fob_sys).copied().unwrap_or(0)) > fob_supp_before);

    for (idx, (hq_sys, hub_id, hub_tid, emp_fac, data_tid)) in [
        (HQ_SYSTEM_INTAKI, 1_891_200_001_i64, TYPE_FEDERATION_CI_OUTPOST_HUB, 500_004_i64, TYPE_GURISTAS_INSURGENCY_DATA_SEQ),
        (HQ_SYSTEM_MEHATOOR, 1_891_200_002, TYPE_IMPERIAL_CI_OUTPOST_HUB, 500_003, TYPE_ANGEL_INSURGENCY_DATA_SEQ),
        (HQ_SYSTEM_AMO, 1_891_200_004, TYPE_REPUBLIC_CI_OUTPOST_HUB, 500_002, TYPE_ANGEL_INSURGENCY_DATA_SEQ),
    ]
    .into_iter()
    .enumerate()
    {
        let (cid, sid) = (91_100_010 + idx as i64, 101_100_010 + idx as i64);
        let mut s = register_online_pilot(&ctx, 89_110 + idx as u64, &format!("b41_hq_{idx}"), "pw", cid, 98_100_010 + idx as i64, None, hq_sys, sid, 621, DVec3::new(1_150_500.0, 0.0, 35_000.0), 0).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&cid).unwrap().war_faction_id = Some(emp_fac);
            snap.items.insert(880_100 + idx as i64, InventoryItemRecord { item_id: 880_100 + idx as i64, type_id: data_tid, owner_id: cid, location_id: sid, flag: InventoryFlag::Cargo, quantity: 1, ..Default::default() });
        });
        let (rid, crate_tid) = find_recipe_for_trader(&ctx, hub_tid, data_tid);
        rpc_call(&ctx, &mut s, "beyonce", "CallComponentFromClient", &trade_args(hub_id, rid)).await.expect("War HQ Counter-Insurgency hand-in");
        assert!(ctx.db.with_snapshot(|sn| sn.items.values().any(|i| i.location_id == sid && i.type_id == crate_tid)));
    }
}

#[tokio::test]
async fn test_b41_insurgency_mining_ambush_ransom_rescue_destroy_and_lp_split() {
    // parity: feat:pve.insurgency-mining-ambush
    eve_testkit::attest(&["feat:pve.insurgency-mining-ambush"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let mut pir_pilots = Vec::new();
    for i in 0..6_i64 {
        let (cid, sid) = (91_200_001 + i, 101_200_001 + i);
        let mut s = register_online_pilot(&ctx, 89_201 + i as u64, &format!("b41_amb_p{i}"), "pw", cid, 98_200_001 + i, None, front_sys, sid, 621, DVec3::ZERO, 0).await;
        rpc_call(&ctx, &mut s, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
        fit_small_railgun(&ctx, cid, sid, 220_001 + i * 10);
        pir_pilots.push((cid, sid, s));
    }
    let (emp_cid, emp_ship) = (91_200_099, 101_200_099);
    let mut s_emp = register_online_pilot(&ctx, 89_299, "b41_amb_emp", "pw", emp_cid, 98_200_099, None, front_sys, emp_ship, 621, DVec3::ZERO, 0).await;
    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)]).await.unwrap();
    fit_small_railgun(&ctx, emp_cid, emp_ship, 220_901);

    let _ = rpc_call(&ctx, &mut s_emp, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let camp_id = ctx.db.with_snapshot(|s| *s.insurgency_runtime.campaigns.keys().next().unwrap());
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    add_system_corruption(&ctx, camp_id, front_sys, 200);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    // 1. Subcase A: 6 Pirate pilots complete ransom -> ORE Rorqual warps off, corruption +50, LP shared across >5 players (25,000 each).
    let amb1 = ctx.db.with_snapshot(|s| load_ops_state(s).mining_ambushes[&front_sys].clone());
    let sys_h = ctx.systems.get_or_spawn(front_sys);
    assert!(sys_h.is_ball_scrambled_sync(amb1.rorqual_ball_id));
    let mut pir_lp0 = Vec::new();
    for (_, sid, s) in &mut pir_pilots {
        pir_lp0.push(total_lp_via_rpc(&ctx, s).await);
        warp_ship_via_rpc(&ctx, s, front_sys, *sid, amb1.beacon_id).await;
    }
    let p0_sid = pir_pilots[0].1;
    for def_id in amb1.ore_defender_ids {
        kill_npc_with_turret_rpc(&ctx, &mut pir_pilots[0].2, front_sys, p0_sid, 220_001, def_id).await;
    }
    let corr0 = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].corruption_points[&front_sys]);
    fast_forward_secs(&ctx, MINING_AMBUSH_RANSOM_SECS + 2);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    assert!(!sys_h.is_ball_scrambled_sync(amb1.rorqual_ball_id));
    assert!(matches!(sys_h.get_ball_snapshot(amb1.rorqual_ball_id).await.unwrap().unwrap().1.mode, MovementMode::Warp { .. }));
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].corruption_points[&front_sys]), corr0 + 50);
    for (idx, (_, _, s)) in pir_pilots.iter_mut().enumerate() {
        assert_eq!(total_lp_via_rpc(&ctx, s).await - pir_lp0[idx], (MINING_AMBUSH_BASE_LP * 5) / 6);
    }

    // 2. Subcase B: Empire rescue -> kill all pirate tacklers, Rorqual warps off, suppression +50, full 30,000 LP for 1 pilot (<=5).
    for (_, sid, _) in &pir_pilots {
        let _ = sys_h.set_ball_position(*sid, DVec3::ZERO).await;
    }
    fast_forward_secs(&ctx, MINING_AMBUSH_RESPAWN_SECS + 20);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let amb2 = ctx.db.with_snapshot(|s| load_ops_state(s).mining_ambushes[&front_sys].clone());
    assert!(!amb2.resolved && sys_h.is_ball_scrambled_sync(amb2.rorqual_ball_id));
    let emp_lp0 = total_lp_via_rpc(&ctx, &mut s_emp).await;
    warp_ship_via_rpc(&ctx, &mut s_emp, front_sys, emp_ship, amb2.beacon_id).await;
    let supp0 = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].suppression_points.get(&front_sys).copied().unwrap_or(0));
    for pt_id in amb2.pirate_tackler_ids {
        kill_npc_with_turret_rpc(&ctx, &mut s_emp, front_sys, emp_ship, 220_901, pt_id).await;
    }
    assert!(!sys_h.is_ball_scrambled_sync(amb2.rorqual_ball_id));
    assert!(matches!(sys_h.get_ball_snapshot(amb2.rorqual_ball_id).await.unwrap().unwrap().1.mode, MovementMode::Warp { .. }));
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].suppression_points[&front_sys]), supp0 + 50);
    assert_eq!(total_lp_via_rpc(&ctx, &mut s_emp).await - emp_lp0, (MINING_AMBUSH_BASE_LP * 105) / 100);

    // 3. Subcase C: Destroying ORE Rorqual spawns lootable 79586 wreck, forfeits LP (0 delta), and keeps corruption (+50).
    let _ = sys_h.set_ball_position(emp_ship, DVec3::ZERO).await;
    fast_forward_secs(&ctx, MINING_AMBUSH_RESPAWN_SECS + 20);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let amb3 = ctx.db.with_snapshot(|s| load_ops_state(s).mining_ambushes[&front_sys].clone());
    assert_eq!(sys_h.get_ball_snapshot(amb3.rorqual_ball_id).await.unwrap().unwrap().1.type_id, TYPE_ORE_RORQUAL);

    let lp_before_kill = total_lp_via_rpc(&ctx, &mut pir_pilots[0].2).await;
    let corr_before_kill = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].corruption_points[&front_sys]);
    kill_npc_with_turret_rpc(&ctx, &mut pir_pilots[0].2, front_sys, p0_sid, 220_001, amb3.rorqual_ball_id).await;

    assert_eq!(total_lp_via_rpc(&ctx, &mut pir_pilots[0].2).await, lp_before_kill, "Destroying ORE Rorqual forfeits LP");
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].corruption_points[&front_sys]), corr_before_kill + 50, "Destroying ORE Rorqual keeps corruption");

    let (rorq_wreck_id, rorq_loot_id) = ctx.db.with_snapshot(|s| {
        let w = s.items.values().find(|i| i.type_id == TYPE_ORE_RORQUAL_WRECK).expect("ORE Rorqual Wreck 79586");
        let l = s.items.values().find(|i| i.location_id == w.item_id && i.type_id == TYPE_UPWELL_CONSORTIUM_DATA_CACHE).expect("Rorqual wreck loot");
        (w.item_id, l.item_id)
    });
    let mgr = bind_invbroker(&ctx, &mut pir_pilots[0].2, front_sys).await;
    let _ = sys_h.set_ball_position(p0_sid, DVec3::ZERO).await;
    assert_eq!(ctx.dispatch_rpc(&mut pir_pilots[0].2, "invbroker", Some(mgr), "GetInventoryFromId", &[PyRep::Integer(rorq_wreck_id), PyRep::Integer(0)], &HashMap::new(), None).await.unwrap_err().code, "InventoryNotAccessible");

    let wpos = sys_h.get_ball_snapshot(rorq_wreck_id).await.unwrap().unwrap().1.position;
    let _ = sys_h.set_ball_position(p0_sid, wpos + DVec3::new(800.0, 0.0, 0.0)).await;
    let ship_inv = ctx.dispatch_rpc(&mut pir_pilots[0].2, "invbroker", Some(mgr), "GetInventoryFromId", &[PyRep::Integer(p0_sid), PyRep::Integer(0)], &HashMap::new(), None).await.unwrap().as_i64().unwrap() as u64;
    assert_eq!(ctx.dispatch_rpc(&mut pir_pilots[0].2, "invbroker", Some(ship_inv), "Add", &[PyRep::Integer(rorq_loot_id), PyRep::Integer(rorq_wreck_id), PyRep::Integer(5), PyRep::Integer(0)], &HashMap::new(), None).await.unwrap_err().code, "InvalidQuantity");
    ctx.dispatch_rpc(&mut pir_pilots[0].2, "invbroker", Some(ship_inv), "Add", &[PyRep::Integer(rorq_loot_id), PyRep::Integer(rorq_wreck_id), PyRep::Integer(5)], &HashMap::new(), None).await.expect("Loot ORE Rorqual Wreck");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&rorq_loot_id).unwrap().location_id), p0_sid);
}

#[tokio::test]
async fn test_b41_insurgency_outpost_raid_sizes_timers_defenders_bc_and_respawn() {
    // parity: feat:pve.insurgency-outpost-raid
    eve_testkit::attest(&["feat:pve.insurgency-outpost-raid"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let mut s_neu = register_online_pilot(&ctx, 89_301, "b41_raid_neu", "pw", 91_300_001, 98_300_001, None, front_sys, 101_300_001, 587, DVec3::ZERO, 0).await;
    let mut s_pir = register_online_pilot(&ctx, 89_302, "b41_raid_pir", "pw", 91_300_002, 98_300_002, None, front_sys, 101_300_002, 587, DVec3::ZERO, 0).await;
    let mut s_emp = register_online_pilot(&ctx, 89_303, "b41_raid_emp", "pw", 91_300_003, 98_300_003, None, front_sys, 101_300_003, 621, DVec3::ZERO, 0).await;
    rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)]).await.unwrap();
    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)]).await.unwrap();
    fit_small_railgun(&ctx, 91_300_002, 101_300_002, 230_001);
    fit_small_railgun(&ctx, 91_300_003, 101_300_003, 230_011);

    let _ = rpc_call(&ctx, &mut s_emp, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let camp_id = ctx.db.with_snapshot(|s| *s.insurgency_runtime.campaigns.keys().next().unwrap());
    fast_forward_secs(&ctx, FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let gate_for = |sz: OutpostRaidSize| 1_893_100_000 + i64::from(front_sys) * 10 + sz.slot_index();
    let (g_small, g_med, g_mod, g_ladv) = (gate_for(OutpostRaidSize::Small), gate_for(OutpostRaidSize::Medium), gate_for(OutpostRaidSize::Moderate), gate_for(OutpostRaidSize::LargeAdv));
    let sys_h = ctx.systems.get_or_spawn(front_sys);

    // >= 4 negative RPC tests on keeper.ActivateAccelerationGate.
    let _ = sys_h.set_ball_position(101_300_001, OutpostRaidSize::Small.gate_position()).await;
    assert_eq!(rpc_call(&ctx, &mut s_neu, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)]).await.unwrap_err().code, "NotEnlistedInMilitia");
    assert_eq!(rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)]).await.unwrap_err().code, "TooFarFromGate");
    let _ = sys_h.set_ball_position(101_300_003, OutpostRaidSize::Small.gate_position()).await;
    assert_eq!(rpc_call(&ctx, &mut s_emp, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)]).await.unwrap_err().code, "ShipRestrictedForComplex");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(230_099, InventoryItemRecord { item_id: 230_099, type_id: 10_998, owner_id: 91_300_002, location_id: 101_300_002, flag: InventoryFlag::LoSlot0, quantity: 1, singleton: true, ..Default::default() });
    });
    let _ = sys_h.set_ball_position(101_300_002, OutpostRaidSize::Small.gate_position()).await;
    assert_eq!(rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)]).await.unwrap_err().code, "WarpCoreStabilizerRestricted");
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&230_099); });

    // 1. Small Outpost Raid (Pirate Frigate 587): Mordu's Legion defender (79545), 10-min (600s) capture, 0.25% contribution.
    rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_small)]).await.expect("enter Small raid");
    let mordus_small = ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_small].mordus_defender_ball_id.unwrap());
    assert_eq!(sys_h.get_ball_snapshot(mordus_small).await.unwrap().unwrap().1.type_id, 79_545);
    fast_forward_secs(&ctx, 120);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_small].weighted_elapsed_secs), 0.0);
    kill_npc_with_turret_rpc(&ctx, &mut s_pir, front_sys, 101_300_002, 230_001, mordus_small).await;
    fast_forward_secs(&ctx, 590);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_small].completed_on_cooldown));
    fast_forward_secs(&ctx, 15);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_small].completed_on_cooldown));
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].corruption_points[&front_sys]), 2);

    // 2. Medium Outpost Raid (Empire Cruiser 621): Pirate defender (79533), 15-min (900s) capture, 0.5% (+5 suppression pts).
    let _ = sys_h.set_ball_position(101_300_003, OutpostRaidSize::Medium.gate_position()).await;
    rpc_call(&ctx, &mut s_emp, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_med)]).await.expect("enter Medium raid");
    let pir_def_med = ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_med].pirate_defender_ball_id.unwrap());
    assert_eq!(sys_h.get_ball_snapshot(pir_def_med).await.unwrap().unwrap().1.type_id, 79_533);
    kill_npc_with_turret_rpc(&ctx, &mut s_emp, front_sys, 101_300_003, 230_011, pir_def_med).await;
    fast_forward_secs(&ctx, 890);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(!ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_med].completed_on_cooldown));
    fast_forward_secs(&ctx, 15);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].suppression_points[&front_sys]), 5);

    // 3. Moderate Outpost Raid: Battleship rejected; Battlecruiser (24698 Drake) gets 1.25x speed (720s vs 900s) & 1.25x LP (33,750) + 1.5% (+15 pts).
    ctx.db.with_mut_snapshot(|s| s.items.get_mut(&101_300_002).unwrap().type_id = 638);
    let _ = sys_h.set_ball_position(101_300_002, OutpostRaidSize::Moderate.gate_position()).await;
    assert_eq!(rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_mod)]).await.unwrap_err().code, "ShipRestrictedForComplex");

    ctx.db.with_mut_snapshot(|s| s.items.get_mut(&101_300_002).unwrap().type_id = 24_698);
    let lp_before_mod = total_lp_via_rpc(&ctx, &mut s_pir).await;
    rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_mod)]).await.expect("enter Moderate raid in Battlecruiser");
    let mordus_mod = ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_mod].mordus_defender_ball_id.unwrap());
    kill_npc_with_turret_rpc(&ctx, &mut s_pir, front_sys, 101_300_002, 230_001, mordus_mod).await;
    fast_forward_secs(&ctx, 725);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_mod].completed_on_cooldown));
    assert_eq!(total_lp_via_rpc(&ctx, &mut s_pir).await - lp_before_mod, 33_750);
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].corruption_points[&front_sys]), 17);

    // 4. Static Large ADV Outpost Raid (Battleship 638): 15-min (900s) capture, 1.0% (+10 pts), and static 40-min (2,400s) respawn.
    ctx.db.with_mut_snapshot(|s| s.items.get_mut(&101_300_002).unwrap().type_id = 638);
    let _ = sys_h.set_ball_position(101_300_002, OutpostRaidSize::LargeAdv.gate_position()).await;
    rpc_call(&ctx, &mut s_pir, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(g_ladv)]).await.expect("enter Large ADV raid");
    let mordus_ladv = ctx.db.with_snapshot(|s| load_ops_state(s).outpost_raids[&g_ladv].mordus_defender_ball_id.unwrap());
    kill_npc_with_turret_rpc(&ctx, &mut s_pir, front_sys, 101_300_002, 230_001, mordus_ladv).await;
    fast_forward_secs(&ctx, 905);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(ctx.db.with_snapshot(|s| { let r = &load_ops_state(s).outpost_raids[&g_ladv]; r.is_static && r.completed_on_cooldown }));
    assert_eq!(ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].corruption_points[&front_sys]), 27);
    assert!(sys_h.get_ball_snapshot(g_ladv).await.unwrap().is_none());

    fast_forward_secs(&ctx, LARGE_ADV_OUTPOST_RESPAWN_SECS - 15);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(sys_h.get_ball_snapshot(g_ladv).await.unwrap().is_none());

    fast_forward_secs(&ctx, 20);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(sys_h.get_ball_snapshot(g_ladv).await.unwrap().is_some());
}

#[tokio::test]
async fn test_b41_proving_grounds_fleet_queue_restrictions_boundary_timeout_and_rewards() {
    // parity: feat:pve.proving-grounds-queue-and-rewards
    eve_testkit::attest(&["feat:pve.proving-grounds-queue-and-rewards"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let jita = 30_000_142_u32;
    let fil_tid = 72_984_u32; // 2v2 Battleships Proving Filament
    let spec = get_pvp_filament_match_spec(&ctx.sde, fil_tid);
    assert_eq!((spec.fleet_size, spec.number_of_fleets), (2, 2));

    let high_meta_mod_tid = ctx.sde.types.values().find(|t| t.category_id == 7 && t.default_attributes.get(&633).copied().unwrap_or(0.0) > f64::from(spec.max_meta_level)).unwrap().type_id;
    let high_meta_imp_tid = ctx.sde.types.values().find(|t| t.category_id == 20 && t.default_attributes.get(&633).copied().unwrap_or(0.0) > f64::from(spec.max_meta_level)).unwrap().type_id;

    let mut sessions = Vec::new();
    let mut ids = Vec::new();
    for i in 0..5_i64 {
        let (cid, sid, fid) = (91_400_001 + i, 101_400_001 + i, 884_001 + i);
        let s = register_online_pilot(&ctx, 89_401 + i as u64, &format!("b41_pg_{i}"), "pw", cid, 98_400_001 + i, None, jita, sid, 638, DVec3::new(50_000_000.0 + (i as f64) * 2_000.0, 0.0, 0.0), 0).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(fid, InventoryItemRecord { item_id: fid, type_id: fil_tid, owner_id: cid, location_id: sid, flag: InventoryFlag::Cargo, quantity: 1, ..Default::default() });
        });
        ids.push((cid, sid, fid));
        sessions.push(s);
    }

    // >= 5 negative RPC tests on pvpFilamentMgr.JoinPVPQueue (verifying filament is NOT consumed).
    set_pilot_location(&ctx, &mut sessions[0], jita, Some(60_003_760));
    assert_eq!(rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_tid as i64)]).await.unwrap_err().code, "MustBeInSpace");
    set_pilot_location(&ctx, &mut sessions[0], jita, None);

    ctx.db.with_mut_snapshot(|s| s.items.get_mut(&ids[0].1).unwrap().type_id = 606);
    assert_eq!(rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_tid as i64)]).await.unwrap_err().code, "InvalidShipForFilament");
    ctx.db.with_mut_snapshot(|s| s.items.get_mut(&ids[0].1).unwrap().type_id = 638);

    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(884_101, InventoryItemRecord { item_id: 884_101, type_id: high_meta_mod_tid, owner_id: ids[0].0, location_id: ids[0].1, flag: InventoryFlag::LoSlot0, quantity: 1, singleton: true, ..Default::default() });
    });
    assert_eq!(rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_tid as i64)]).await.unwrap_err().code, "InvalidModuleTierForFilament");
    ctx.db.with_mut_snapshot(|s| {
        s.items.remove(&884_101);
        s.active_implants.insert(ids[0].0, vec![high_meta_imp_tid]);
    });
    assert_eq!(rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_tid as i64)]).await.unwrap_err().code, "InvalidImplantsForFilament");
    ctx.db.with_mut_snapshot(|s| {
        s.active_implants.remove(&ids[0].0);
        s.fleets.insert(501, FleetRecord { fleet_id: 501, boss_id: ids[0].0, members: vec![ids[0].0, ids[1].0, ids[4].0], ..Default::default() });
    });
    assert_eq!(rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_tid as i64)]).await.unwrap_err().code, "FleetTooLargeForFilament");
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&ids[0].2)));

    // Configure two valid 2-pilot fleets: Fleet 501 = [p0, p1], Fleet 502 = [p2, p3].
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(501, FleetRecord { fleet_id: 501, boss_id: ids[0].0, members: vec![ids[0].0, ids[1].0], ..Default::default() });
        s.fleets.insert(502, FleetRecord { fleet_id: 502, boss_id: ids[2].0, members: vec![ids[2].0, ids[3].0], ..Default::default() });
    });

    assert_eq!(rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[0].2), PyRep::Integer(fil_tid as i64)]).await.unwrap(), PyRep::Integer(30));
    assert!(ctx.db.with_snapshot(|s| { let q = &load_pvp_store(s).queue; q.contains_key(&ids[0].0) && q.contains_key(&ids[1].0) }));
    rpc_call(&ctx, &mut sessions[1], "pvpFilamentMgr", "LeavePVPQueue", &[PyRep::Integer(ids[0].2)]).await.expect("fleet LeavePVPQueue");
    assert!(ctx.db.with_snapshot(|s| load_pvp_store(s).queue.is_empty()));
    let refunded_fid = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.owner_id == ids[0].0 && i.location_id == ids[0].1 && i.type_id == fil_tid).unwrap().item_id);
    assert_eq!(rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(refunded_fid), PyRep::Integer(fil_tid as i64)]).await.unwrap(), PyRep::Integer(30));

    // Fleet 502 boss activates 1 filament -> matches with Fleet 501 and spawns isolated arena pocket in 32000001!
    assert_eq!(rpc_call(&ctx, &mut sessions[2], "pvpFilamentMgr", "JoinPVPQueue", &[PyRep::Integer(ids[2].2), PyRep::Integer(fil_tid as i64)]).await.unwrap(), PyRep::Integer(5));
    let (mid, center) = ctx.db.with_snapshot(|s| {
        let st = load_pvp_store(s);
        let (&m_id, off) = st.match_arena_offsets.iter().next().unwrap();
        (m_id, DVec3::from_array(*off))
    });
    let arena_h = ctx.systems.get_or_spawn(32_000_001);
    let total_hp = |cs: &eve_destiny::CombatState| cs.shield_hp + cs.armor_hp + cs.structure_hp;

    // Inside 75 km boundary sphere before 900s timer -> zero environmental damage.
    let hp_inside_0 = total_hp(&arena_h.get_combat_state(ids[0].1).await.unwrap().unwrap());
    arena_h.step_manual(1.0).await.unwrap();
    let hp_inside_1 = total_hp(&arena_h.get_combat_state(ids[0].1).await.unwrap().unwrap());
    assert!((hp_inside_1 - hp_inside_0).abs() < 1e-6);

    // 1. Exiting 75 km boundary sphere inflicts escalating environmental damage until destruction!
    let _ = arena_h.set_ball_position(ids[2].1, center + DVec3::new(80_000.0, 0.0, 0.0)).await;
    let mut cs_p2 = arena_h.get_combat_state(ids[2].1).await.unwrap().unwrap();
    (cs_p2.shield_hp, cs_p2.armor_hp, cs_p2.structure_hp, cs_p2.shield_recharge_time_ms) = (0.0, 0.0, 900.0, 1.0e15);
    let _ = arena_h.set_combat_state(ids[2].1, cs_p2).await;

    arena_h.step_manual(1.0).await.unwrap();
    let hp_b1 = total_hp(&arena_h.get_combat_state(ids[2].1).await.unwrap().unwrap());
    let dmg_b1 = 900.0 - hp_b1;
    assert!(dmg_b1 > 0.0);

    arena_h.step_manual(1.0).await.unwrap();
    let hp_b2 = total_hp(&arena_h.get_combat_state(ids[2].1).await.unwrap().unwrap());
    let dmg_b2 = hp_b1 - hp_b2;
    assert!(dmg_b2 > dmg_b1, "Boundary damage escalates ({dmg_b2} > {dmg_b1})");

    arena_h.step_manual(1.0).await.unwrap();
    assert!(arena_h.get_ball_snapshot(ids[2].1).await.unwrap().is_none());
    assert!(ctx.db.with_snapshot(|s| load_pvp_store(s).active_matches[&mid].active));

    // 2. Exceeding 900s arena timer inflicts escalating environmental damage until destruction & awards Proving Grounds victory rewards!
    let mut cs_p3 = arena_h.get_combat_state(ids[3].1).await.unwrap().unwrap();
    (cs_p3.shield_hp, cs_p3.armor_hp, cs_p3.structure_hp, cs_p3.shield_recharge_time_ms) = (0.0, 0.0, 500.0, 1.0e15);
    let _ = arena_h.set_combat_state(ids[3].1, cs_p3).await;
    let bal_p0_before = ctx.db.with_snapshot(|s| s.characters[&ids[0].0].balance);
    let items_p0_before = ctx.db.with_snapshot(|s| s.items.values().filter(|i| i.owner_id == ids[0].0).count());

    fast_forward_secs(&ctx, 905);
    arena_h.step_manual(1.0).await.unwrap();
    let hp_t1 = total_hp(&arena_h.get_combat_state(ids[3].1).await.unwrap().unwrap());
    assert!(500.0 - hp_t1 > 0.0);

    arena_h.step_manual(1.0).await.unwrap();
    assert!(arena_h.get_ball_snapshot(ids[3].1).await.unwrap().is_none());

    let (m_active, m_winner, ev_id, got_reward) = ctx.db.with_snapshot(|s| {
        let st = load_pvp_store(s);
        let m = &st.active_matches[&mid];
        let bal_after = s.characters[&ids[0].0].balance;
        let items_after = s.items.values().filter(|i| i.owner_id == ids[0].0).count();
        (m.active, m.winner_char_id, m.event_id, bal_after > bal_p0_before || items_after > items_p0_before)
    });
    assert!(!m_active && m_winner == Some(ids[0].0) && got_reward);

    ctx.sync_session(&mut sessions[0]);
    let stats_rep = rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "GetCharacterStatistics", &[PyRep::Integer(ev_id)]).await.unwrap();
    assert_eq!(stats_rep.dict_get("wins").and_then(PyRep::as_i64), Some(1));
    let exit_rep = rpc_call(&ctx, &mut sessions[0], "pvpFilamentMgr", "AbyssalPVPEndGateActivation", &[PyRep::Integer(70_000 + mid)]).await.expect("exit arena via end gate");
    assert_eq!(exit_rep.dict_get("originSystemID").and_then(PyRep::as_i64), Some(i64::from(jita)));
}
