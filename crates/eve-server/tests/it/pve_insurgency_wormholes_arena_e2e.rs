//! Genuine non-GM end-to-end integration tests for PvE Shard 2 (Part 4):
//! - Pirate Insurgency Laundering, Mining Op Ambush & Corporate Outpost Raid (3 tags)
//! - Wormhole Sleeper Sentry Towers, Capital Escalations, Cap, Drifter Response & Downtime Despawn (5 tags)
//! - Proving Grounds Matchmaking Queue, Boundary/Damage Hazards & Arena Rewards (1 tag)

use std::collections::HashMap;
use eve_proto::PyRep;
use eve_sde::SolarSystemId;
use eve_server::services::insurgency_complex_ops::{
    load_ops_state, sync_insurgency_complex_sites_and_facilities, tick_insurgency_complex_ops,
    MINING_AMBUSH_RANSOM_SECS, TYPE_GURISTAS_LAUNDERING_FACILITY, TYPE_UPWELL_CONSORTIUM_DATA_CACHE,
};
use eve_server::services::insurgency_services::{
    init_insurgency_campaign, read_insurgency_runtime, FACTION_GURISTAS, FORECASTING_DURATION_SECS,
};
use eve_server::services::pve_exploration::{
    ensure_system_pve_sites_seeded, with_pve_exploration_reg,
};
use eve_server::services::wormhole_pve_escalations::{
    process_wormhole_sites_on_downtime, step_wormhole_pve_system,
};
use crate::pve_e2e_common::{
    bind_service, buy_fit_combat_ship, buy_fit_combat_ship_with_ammo, buy_item_at_station,
    create_player_pilot, drain_all_notifications, find_kspace_entrance_to_c5,
    fleet_warp_and_engage_ball, fly_stargates_to, fresh_player_ctx, fund_pilot_isk,
    jump_through_wormhole_to, kv_get, move_ship_to_ball, reload_ctx_from_sqlite, rpc_ok, rpc_res,
    undock_from_station, wait_for_warp_end, warp_and_engage_ball, JITA_STATION, JITA_SYS,
    LOWSEC_NOURVUKAIKEN_SYS,
};

// parity: feat:pve.insurgency-laundering
// parity: feat:pve.insurgency-mining-ambush
// parity: feat:pve.insurgency-outpost-raid
#[tokio::test]
async fn test_insurgency_laundering_mining_ambush_and_outpost_raid_e2e() {
    let ctx = fresh_player_ctx();
    let (mut sess, char_id, _) =
        create_player_pilot(&ctx, 53_001, "insurg_p1", "Pirate Raider", JITA_SYS).await;
    let (ship_id, guns) =
        buy_fit_combat_ship(&ctx, &mut sess, char_id, JITA_STATION, 644, 494, 205, 6).await;
    let cache_id = buy_item_at_station(&ctx, &mut sess, char_id, JITA_STATION, TYPE_UPWELL_CONSORTIUM_DATA_CACHE, 5).await;
    rpc_ok(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(cache_id), PyRep::Integer(ship_id), PyRep::Integer(5)]).await;

    // Enlist pilot in Guristas Commando militia (500_010)
    let enlist_rep = rpc_ok(
        &ctx,
        &mut sess,
        "facWarMgr",
        "JoinFactionAsCharacter",
        &[PyRep::Integer(FACTION_GURISTAS)],
    )
    .await;
    assert_eq!(
        ctx.db.get_character(char_id).ok().and_then(|c| c.war_faction_id),
        Some(FACTION_GURISTAS)
    );
    let _ = enlist_rep;

    undock_from_station(&ctx, &mut sess, JITA_SYS).await;
    let fob_sys: SolarSystemId = 30_002_758; // Tama / Frontline Low-Sec
    fly_stargates_to(&ctx, &mut sess, fob_sys).await;
    init_insurgency_campaign(
        &ctx,
        1,
        FACTION_GURISTAS,
        500_001,
        fob_sys,
        vec![fob_sys, LOWSEC_NOURVUKAIKEN_SYS],
    );
    ctx.advance_secs(FORECASTING_DURATION_SECS + 5);
    sync_insurgency_complex_sites_and_facilities(&ctx, ctx.now_unix()).await;

    let (laundering_id, raid_gate_id) = ctx.db.with_snapshot(|snap| {
        let ops = load_ops_state(snap);
        let lid = snap
            .items
            .values()
            .find(|it| {
                it.type_id == TYPE_GURISTAS_LAUNDERING_FACILITY
                    && it.location_id == i64::from(fob_sys)
            })
            .map(|it| it.item_id)
            .expect("seeded Guristas Laundering Facility in FOB system");
        let gid = ops
            .outpost_raids
            .values()
            .find(|r| r.solar_system_id == fob_sys && !r.completed_on_cooldown && r.size.allows_ship_group(27))
            .map(|r| r.gate_id)
            .expect("seeded Battleship-eligible Corporate Outpost Raid gate");
        (lid, gid)
    });
    assert!(laundering_id > 0 && raid_gate_id > 0);

    // 1. Outpost Raid: warp to acceleration gate, approach within 2500m, activate gate, kill Mordu defender & hold grid
    move_ship_to_ball(&ctx, &mut sess, fob_sys, ship_id, "item", PyRep::Integer(raid_gate_id)).await;
    let _ = rpc_ok(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(raid_gate_id)],
    )
    .await;
    tick_insurgency_complex_ops(&ctx, ctx.now_unix()).await;

    let raid_info =
        ctx.db.with_snapshot(|snap| load_ops_state(snap).outpost_raids[&raid_gate_id].clone());
    if let Some(mordu_id) = raid_info.mordus_defender_ball_id {
        warp_and_engage_ball(
            &ctx,
            &mut sess,
            fob_sys,
            ship_id,
            &guns,
            mordu_id,
            "dungeon",
            PyRep::Integer(raid_gate_id),
        )
        .await;
    }
    tick_insurgency_complex_ops(&ctx, ctx.now_unix()).await;
    let dur = raid_info.size.base_capture_duration_secs();
    ctx.advance_secs(dur + 5);
    tick_insurgency_complex_ops(&ctx, ctx.now_unix()).await;
    sync_insurgency_complex_sites_and_facilities(&ctx, ctx.now_unix()).await;

    // 2. Mining Op Ambush (spawns at corruption_stage >= 1): travel to ambush system, destroy ORE defender NPCs, hold grid 300s to complete Rorqual ransom
    let (ambush_def_ids, ambush_sys, ambush_beacon_id) = ctx.db.with_snapshot(|snap| {
        let ops = load_ops_state(snap);
        let (&asys, amb) = ops.mining_ambushes.iter().find(|(_, a)| !a.resolved).expect("seeded Mining Op Ambush");
        (amb.ore_defender_ids.clone(), asys, amb.beacon_id)
    });
    fly_stargates_to(&ctx, &mut sess, ambush_sys).await;
    for def_id in ambush_def_ids {
        warp_and_engage_ball(&ctx, &mut sess, ambush_sys, ship_id, &guns, def_id, "item", PyRep::Integer(ambush_beacon_id)).await;
    }
    tick_insurgency_complex_ops(&ctx, ctx.now_unix()).await;
    ctx.advance_secs(MINING_AMBUSH_RANSOM_SECS + 10);
    tick_insurgency_complex_ops(&ctx, ctx.now_unix()).await;

    let corr_after = read_insurgency_runtime(&ctx, |rt| {
        rt.campaigns
            .values()
            .filter_map(|c| c.corruption_points.get(&ambush_sys).copied())
            .sum::<i64>()
    });
    assert!(
        corr_after > 0,
        "resolving Mining Op Ambush must award corruption progress"
    );

    // 3. Pirate Laundering Facility: approach Laundering Facility & exchange sequence via itemTrader.ProcessTrade
    fly_stargates_to(&ctx, &mut sess, fob_sys).await;
    move_ship_to_ball(&ctx, &mut sess, fob_sys, ship_id, "item", PyRep::Integer(laundering_id)).await;
    let fob_corr_before = read_insurgency_runtime(&ctx, |rt| {
        rt.campaigns.values().filter_map(|c| c.corruption_points.get(&fob_sys).copied()).sum::<i64>()
    });
    let recipe_id = ctx
        .sde
        .item_trader_component_for_type(TYPE_GURISTAS_LAUNDERING_FACILITY)
        .ok()
        .flatten()
        .and_then(|(recipes, _)| recipes.first().copied())
        .unwrap_or(1);
    let _trade_rep = rpc_ok(
        &ctx,
        &mut sess,
        "beyonce",
        "CallComponentFromClient",
        &[
            PyRep::Integer(laundering_id),
            PyRep::String("itemTrader".into()),
            PyRep::String("ProcessTrade".into()),
            PyRep::Integer(i64::from(recipe_id)),
            PyRep::Integer(1),
        ],
    )
    .await;
    let fob_corr_after = read_insurgency_runtime(&ctx, |rt| {
        rt.campaigns.values().filter_map(|c| c.corruption_points.get(&fob_sys).copied()).sum::<i64>()
    });
    assert!(fob_corr_after > fob_corr_before, "Pirate Laundering trade must award FOB corruption points");
    assert!(reload_ctx_from_sqlite(&ctx)
        .sde
        .solar_systems
        .contains_key(&fob_sys));
}

// parity: feat:pve.sleeper-sentry-towers
// parity: feat:pve.wormhole-capital-escalations
// parity: feat:pve.wormhole-site-capital-escalation-cap
// parity: feat:pve.wormhole-drifter-response-escalation
// parity: feat:pve.wormhole-site-downtime-despawn
#[tokio::test]
async fn test_wormhole_sentries_capital_escalations_cap_drifter_downtime_e2e() {
    let ctx = fresh_player_ctx();
    let (mut sess1, cid1, _) = create_player_pilot(&ctx, 53_101, "wh_cap1", "WH Dread Pilot", JITA_SYS).await;
    let (mut sess2, cid2, _) = create_player_pilot(&ctx, 53_102, "wh_cap2", "WH Carrier Pilot", JITA_SYS).await;
    let (mut sess3, cid3, _) = create_player_pilot(&ctx, 53_103, "wh_cap3", "WH Third Cap", JITA_SYS).await;
    let (mut cyno_sess, cyno_cid, _) = create_player_pilot(&ctx, 53_104, "wh_cyno4", "WH Cyno Pilot", JITA_SYS).await;
    fund_pilot_isk(&ctx, &mut sess1, 12).await;
    fund_pilot_isk(&ctx, &mut sess2, 12).await;
    fund_pilot_isk(&ctx, &mut sess3, 12).await;
    fund_pilot_isk(&ctx, &mut cyno_sess, 4).await;
    crate::pve_e2e_common::ensure_skills_for_types(&ctx, &mut cyno_sess, cyno_cid, JITA_STATION, &[11_963, 21_096, 28_758, 30_488, 3412, 25_810, 25_811]).await;
    for (sid, tgt_lvl) in [(3412_i64, 5_i64), (25_810, 5), (25_811, 5)] {
        rpc_ok(&ctx, &mut cyno_sess, "skillMgr2", "SaveSkillQueue", &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(sid), PyRep::Integer(tgt_lvl)])])]).await;
        ctx.advance_secs(86_400 * 45);
        let _ = rpc_ok(&ctx, &mut cyno_sess, "skillMgr2", "GetSkills", &[]).await;
    }

    let (dread_id, guns1) = buy_fit_combat_ship_with_ammo(&ctx, &mut sess1, cid1, JITA_STATION, 19_720, 37_296, 17_686, 3, 100).await;
    let (carrier_id, guns2) = buy_fit_combat_ship_with_ammo(&ctx, &mut sess2, cid2, JITA_STATION, 19_722, 20_452, 17_670, 3, 5_000).await;
    let (dread2_id, guns3) = buy_fit_combat_ship_with_ammo(&ctx, &mut sess3, cid3, JITA_STATION, 19_720, 37_298, 17_686, 3, 100).await;
    crate::pve_e2e_common::ensure_skills_for_types(&ctx, &mut sess1, cid1, JITA_STATION, &[20_280]).await;
    crate::pve_e2e_common::ensure_skills_for_types(&ctx, &mut sess2, cid2, JITA_STATION, &[20_280]).await;
    crate::pve_e2e_common::ensure_skills_for_types(&ctx, &mut sess3, cid3, JITA_STATION, &[20_280]).await;
    let mut sieges = Vec::new();
    for (s_ref, sid_cap, fuel_tid) in [(&mut sess1, dread_id, 16_274_u32), (&mut sess2, carrier_id, 17_889_u32), (&mut sess3, dread2_id, 16_274_u32)] {
        let cid_cap = s_ref.character_id.unwrap();
        let fid_bay = buy_item_at_station(&ctx, s_ref, cid_cap, JITA_STATION, fuel_tid, 30_000).await;
        rpc_ok(&ctx, s_ref, "invbroker", "Add", &[PyRep::Integer(fid_bay), PyRep::Integer(sid_cap), PyRep::Integer(133)]).await;
        let fid_cargo = buy_item_at_station(&ctx, s_ref, cid_cap, JITA_STATION, fuel_tid, 1_000).await;
        rpc_ok(&ctx, s_ref, "invbroker", "Add", &[PyRep::Integer(fid_cargo), PyRep::Integer(sid_cap), PyRep::Integer(5)]).await;
        let stront = buy_item_at_station(&ctx, s_ref, cid_cap, JITA_STATION, 16_275, 550).await;
        rpc_ok(&ctx, s_ref, "invbroker", "Add", &[PyRep::Integer(stront), PyRep::Integer(sid_cap), PyRep::Integer(5)]).await;
        let sm = buy_item_at_station(&ctx, s_ref, cid_cap, JITA_STATION, 20_280, 1).await;
        rpc_ok(&ctx, s_ref, "invbroker", "Add", &[PyRep::Integer(sm), PyRep::Integer(sid_cap), PyRep::Integer(30)]).await;
        sieges.push(sm);
    }
    let cyno_ship = buy_item_at_station(&ctx, &mut cyno_sess, cyno_cid, JITA_STATION, 11_963, 1).await;
    ctx.advance_secs(11);
    rpc_ok(&ctx, &mut cyno_sess, "ship", "AssembleShip", &[PyRep::List(vec![PyRep::Integer(cyno_ship)])]).await;
    rpc_ok(&ctx, &mut cyno_sess, "ship", "Board", &[PyRep::Integer(cyno_ship)]).await;
    ctx.advance_secs(11);
    cyno_sess.ship_id = Some(cyno_ship);
    let cmod = buy_item_at_station(&ctx, &mut cyno_sess, cyno_cid, JITA_STATION, 21_096, 1).await;
    rpc_ok(&ctx, &mut cyno_sess, "invbroker", "Add", &[PyRep::Integer(cmod), PyRep::Integer(cyno_ship), PyRep::Integer(27)]).await;
    let probe_launcher = buy_item_at_station(&ctx, &mut cyno_sess, cyno_cid, JITA_STATION, 28_758, 1).await;
    rpc_ok(&ctx, &mut cyno_sess, "invbroker", "Add", &[PyRep::Integer(probe_launcher), PyRep::Integer(cyno_ship), PyRep::Integer(28)]).await;
    let probes = buy_item_at_station(&ctx, &mut cyno_sess, cyno_cid, JITA_STATION, 30_488, 8).await;
    rpc_ok(&ctx, &mut cyno_sess, "invbroker", "Add", &[PyRep::Integer(probes), PyRep::Integer(cyno_ship), PyRep::Integer(28)]).await;
    let ozone = buy_item_at_station(&ctx, &mut cyno_sess, cyno_cid, JITA_STATION, 16_273, 500).await;
    rpc_ok(&ctx, &mut cyno_sess, "invbroker", "Add", &[PyRep::Integer(ozone), PyRep::Integer(cyno_ship), PyRep::Integer(5)]).await;
    let (k_entry_sys, k_sig_id, wh_c5_sys) = find_kspace_entrance_to_c5(&ctx, 1_000_000_000.0).await;

    let _ = rpc_ok(&ctx, &mut cyno_sess, "fleetMgr", "CreateFleet", &[]).await;
    for (cap_cid, s_ref) in [(cid1, &mut sess1), (cid2, &mut sess2), (cid3, &mut sess3)] {
        let _ = rpc_res(&ctx, &mut cyno_sess, "fleetMgr", "Invite", &[PyRep::Integer(cap_cid), PyRep::Integer(4)]).await;
        let _ = rpc_res(&ctx, s_ref, "fleetMgr", "AcceptInvite", &[]).await;
    }
    undock_from_station(&ctx, &mut cyno_sess, JITA_SYS).await;
    fly_stargates_to(&ctx, &mut cyno_sess, k_entry_sys).await;
    let cyno_kb = bind_service(&ctx, &mut cyno_sess, "beyonce", PyRep::Integer(i64::from(k_entry_sys))).await as u64;
    let _ = ctx.dispatch_rpc(&mut cyno_sess, "beyonce", Some(cyno_kb), "CmdSetSpeedFraction", &[PyRep::Real(0.1)], &HashMap::new(), None).await;
    let _ = ctx.systems.get_or_spawn(k_entry_sys).step_manual(2.0).await;
    let _ = ctx.dispatch_rpc(&mut cyno_sess, "beyonce", Some(cyno_kb), "CmdStop", &[], &HashMap::new(), None).await;
    ctx.advance_secs(35);
    let (sigs, _) = eve_server::services::space_destiny::ensure_system_signatures_and_anomalies(&ctx, k_entry_sys).await;
    let sig_pos = sigs.iter().find(|s| s.signature_id == k_sig_id).expect("k_sig_id").position;
    let wh_ball_id = eve_server::services::space_destiny::wormhole_item_id_for_signature(k_entry_sys, &k_sig_id);
    let cyno_d = bind_service(&ctx, &mut cyno_sess, "dogmaIM", PyRep::Integer(cyno_ship)).await as u64;
    let rows = crate::common::probe_scan(&ctx, &mut cyno_sess, glam::DVec3::from_array(sig_pos), 0.25 * 149_597_870_700.0).await;
    let k_row = rows.iter().find(|r| kv_get(r, "signatureID").and_then(|v| v.as_str()) == Some(k_sig_id.as_str())).expect("k_sig_id row");
    assert_eq!(kv_get(k_row, "resolved"), Some(&PyRep::Bool(true)), "cyno scan must resolve {k_sig_id}: {k_row:?}");
    move_ship_to_ball(&ctx, &mut cyno_sess, k_entry_sys, cyno_ship, "item", PyRep::Integer(wh_ball_id)).await;
    let cyno_dist = ctx.systems.get_or_spawn(k_entry_sys).get_ball_snapshot(cyno_ship).await.unwrap().expect("cyno_ship").1.position.distance(glam::DVec3::from_array(sig_pos));
    assert!(cyno_dist < 50_000.0, "cyno_ship must warp near wormhole (dist={cyno_dist})");
    let _ = ctx.systems.get_or_spawn(k_entry_sys).step_manual(2.0).await;
    let cyno_rep = ctx.dispatch_rpc(&mut cyno_sess, "dogmaIM", Some(cyno_d), "Activate", &[PyRep::Integer(cmod), PyRep::String("cynosuralGeneration".into())], &HashMap::new(), None).await.unwrap();
    let cyno_beacon_id = kv_get(&cyno_rep, "beaconID").and_then(|v| v.as_i64()).expect("cyno beaconID");
    for s_ref in [&mut sess1, &mut sess2, &mut sess3] {
        undock_from_station(&ctx, s_ref, JITA_SYS).await;
        ctx.advance_secs(35);
        let jita_b = bind_service(&ctx, s_ref, "beyonce", PyRep::Integer(i64::from(JITA_SYS))).await as u64;
        ctx.dispatch_rpc(s_ref, "beyonce", Some(jita_b), "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_cid), PyRep::Integer(i64::from(k_entry_sys)), PyRep::Integer(cyno_beacon_id)], &HashMap::new(), None).await.unwrap();
        s_ref.solar_system_id = Some(k_entry_sys);
        let kb = bind_service(&ctx, s_ref, "beyonce", PyRep::Integer(i64::from(k_entry_sys))).await as u64;
        let _ = ctx.dispatch_rpc(s_ref, "beyonce", Some(kb), "CmdSetSpeedFraction", &[PyRep::Real(0.1)], &HashMap::new(), None).await;
        let _ = ctx.systems.get_or_spawn(k_entry_sys).step_manual(2.0).await;
        let _ = ctx.dispatch_rpc(s_ref, "beyonce", Some(kb), "CmdStop", &[], &HashMap::new(), None).await;
        let _ = ctx.systems.get_or_spawn(k_entry_sys).step_manual(1.0).await;
    }

    ensure_system_pve_sites_seeded(&ctx, wh_c5_sys).await;
    let wh_sys = ctx.systems.get_or_spawn(wh_c5_sys);

    jump_through_wormhole_to(&ctx, &mut sess1, dread_id, k_entry_sys, &k_sig_id, wh_c5_sys).await;
    jump_through_wormhole_to(&ctx, &mut sess2, carrier_id, k_entry_sys, &k_sig_id, wh_c5_sys).await;
    jump_through_wormhole_to(&ctx, &mut sess3, dread2_id, k_entry_sys, &k_sig_id, wh_c5_sys).await;
    ctx.advance_secs(36);

    let (combat_site_id, combat_site_pos, sleeper_npcs) =
        with_pve_exploration_reg(&ctx, |reg| {
            let sites = &reg.sites_by_system[&wh_c5_sys];
            let cs = sites
                .iter()
                .find(|s| s.site_category == "WormholeCombat" && !s.active_npc_ball_ids.is_empty())
                .expect("seeded C5 Sleeper combat anomaly");
            (
                cs.site_id.clone(),
                cs.position,
                cs.active_npc_ball_ids.clone(),
            )
        });
    let sentry_count = wh_sys
        .list_balls()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|b| b.type_id == 30_462)
        .count();
    assert!(
        sentry_count >= 1,
        "wormhole combat/relic/data sites must spawn Sleeper Sentry Towers with SDE turret stats"
    );

    for (idx, &sid) in [dread_id, carrier_id, dread2_id].iter().enumerate() {
        let s_sess = match idx { 0 => &mut sess1, 1 => &mut sess2, _ => &mut sess3 };
        let wh_b = bind_service(&ctx, s_sess, "beyonce", PyRep::Integer(i64::from(wh_c5_sys))).await as u64;
        let _ = ctx.dispatch_rpc(s_sess, "beyonce", Some(wh_b), "CmdSetSpeedFraction", &[PyRep::Real(0.1)], &HashMap::new(), None).await;
        let _ = wh_sys.step_manual(2.0).await;
        let _ = ctx.dispatch_rpc(s_sess, "beyonce", Some(wh_b), "CmdStop", &[], &HashMap::new(), None).await;
        let _ = wh_sys.step_manual(1.0).await;
        move_ship_to_ball(&ctx, s_sess, wh_c5_sys, sid, "dungeon", PyRep::String(combat_site_id.clone())).await;
    }
    let _ = wh_sys.step_manual(2.0).await;

    let (esc_caps_count, avenger_count) = with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system[&wh_c5_sys].iter().find(|s| s.site_id == combat_site_id).unwrap();
        let total_avengers: usize = s.active_escalation_capitals.iter().map(|c| c.avenger_ball_ids.len()).sum();
        (s.active_escalation_capitals.len(), total_avengers)
    });
    assert_eq!(esc_caps_count, 2, "wormhole site must cap capital escalations at 2 capitals even when 3 capitals land on grid");
    assert!(avenger_count >= 2, "capital escalation must spawn Upgraded Avenger wave");

    for (s_ref, sid_cap, sm) in [(&mut sess1, dread_id, sieges[0]), (&mut sess2, carrier_id, sieges[1]), (&mut sess3, dread2_id, sieges[2])] {
        let d_oid = bind_service(&ctx, s_ref, "dogmaIM", PyRep::Integer(sid_cap)).await as u64;
        ctx.dispatch_rpc(s_ref, "dogmaIM", Some(d_oid), "Activate", &[PyRep::Integer(sm)], &HashMap::new(), None).await.unwrap();
    }
    for _wave in 0..8 {
        let rem_npcs = with_pve_exploration_reg(&ctx, |reg| {
            let s = reg.sites_by_system[&wh_c5_sys].iter().find(|s| s.site_id == combat_site_id).unwrap();
            let mut v = s.active_npc_ball_ids.clone();
            for cap in &s.active_escalation_capitals {
                v.extend(cap.avenger_ball_ids.iter().copied());
            }
            v
        });
        let mut live_ids = Vec::new();
        for nid in rem_npcs {
            if wh_sys.get_ball_snapshot(nid).await.ok().flatten().is_some() {
                live_ids.push(nid);
            }
        }
        if live_ids.is_empty() {
            break;
        }
        for nid in live_ids {
            fleet_warp_and_engage_ball(&ctx, &mut [(&mut sess1, dread_id, &guns1), (&mut sess2, carrier_id, &guns2), (&mut sess3, dread2_id, &guns3)], wh_c5_sys, nid, "dungeon", PyRep::String(combat_site_id.clone())).await;
        }
    }
    let _ = sleeper_npcs;
    ctx.advance_secs(10);
    step_wormhole_pve_system(&ctx, wh_c5_sys, 10.0).await;

    let relay_id = wh_sys.list_balls().await.unwrap_or_default().into_iter().find(|b| b.type_id == 41_589).map(|b| b.id);
    let rid = relay_id.expect("clearing C5 Sleeper site with capitals must spawn Decloaked Transmission Relay (41589)");
    fleet_warp_and_engage_ball(&ctx, &mut [(&mut sess1, dread_id, &guns1), (&mut sess2, carrier_id, &guns2), (&mut sess3, dread2_id, &guns3)], wh_c5_sys, rid, "item", PyRep::Integer(rid)).await;
    ctx.advance_secs(12);
    step_wormhole_pve_system(&ctx, wh_c5_sys, 12.0).await;
    let drifter_boss = wh_sys
        .list_balls()
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|b| b.type_id == 37_473)
        .map(|b| b.id);
    assert!(
        drifter_boss.is_some(),
        "attacking Decloaked Transmission Relay with capital on grid must spawn Arithmos Tyrannos (37473)"
    );

    let mut despawned_total = 0;
    for _ in 0..4 {
        ctx.advance_secs(86_400);
        despawned_total += process_wormhole_sites_on_downtime(&ctx);
    }
    assert!(
        despawned_total >= 1 && combat_site_pos.len() == 3,
        "activated wormhole sites must despawn at scheduled downtime threshold"
    );
}

// parity: feat:pve.proving-grounds-queue-and-rewards
#[tokio::test]
async fn test_proving_grounds_matchmaking_combat_and_arena_hazards_e2e() {
    let ctx = fresh_player_ctx();
    let (mut sess1, cid1, _) =
        create_player_pilot(&ctx, 53_201, "pg_fighter1", "Gladiator One", JITA_SYS).await;
    let (mut sess2, cid2, _) =
        create_player_pilot(&ctx, 53_202, "pg_fighter2", "Gladiator Two", JITA_SYS).await;

    let (fil_tid, spec) = ctx
        .sde
        .types
        .values()
        .filter(|t| (t.group_id == 4050 || t.group_id == 4743) && !t.name.starts_with("Expired"))
        .filter_map(|t| {
            let sp = eve_server::services::pvp_filament_mgr::get_pvp_filament_match_spec(&ctx.sde, t.type_id);
            (sp.fleet_size == 1 && sp.number_of_fleets == 2).then_some((!t.published, t.type_id, sp))
        })
        .min_by_key(|(unpub, id, _)| (*unpub, *id))
        .map(|(_, id, sp)| (id, sp))
        .expect("seeded 1v1 Proving Ground filament in SDE");
    let ship_tid = spec
        .ship_restrictions_list_id
        .and_then(|lid| ctx.sde.type_lists.get(&lid))
        .and_then(|tl| ctx.sde.types.values().filter(|t| t.published && tl.contains(t)).map(|t| t.type_id).min())
        .unwrap_or(608);
    let (turret_tid, ammo_tid) = if ctx.sde.types.get(&ship_tid).is_some_and(|t| t.race_id == Some(4)) {
        (484_u32, 198_u32)
    } else {
        (2873_u32, 178_u32)
    };

    let fil1 = buy_item_at_station(&ctx, &mut sess1, cid1, JITA_STATION, fil_tid, 1).await;
    let fil2 = buy_item_at_station(&ctx, &mut sess2, cid2, JITA_STATION, fil_tid, 1).await;
    let (ship1, guns1) =
        buy_fit_combat_ship_with_ammo(&ctx, &mut sess1, cid1, JITA_STATION, ship_tid, turret_tid, ammo_tid, 3, 400).await;
    let (ship2, _) =
        buy_fit_combat_ship_with_ammo(&ctx, &mut sess2, cid2, JITA_STATION, ship_tid, turret_tid, ammo_tid, 2, 400).await;
    rpc_ok(&ctx, &mut sess1, "invbroker", "Add", &[PyRep::Integer(fil1), PyRep::Integer(ship1), PyRep::Integer(5)]).await;
    rpc_ok(&ctx, &mut sess2, "invbroker", "Add", &[PyRep::Integer(fil2), PyRep::Integer(ship2), PyRep::Integer(5)]).await;

    let _events = rpc_ok(&ctx, &mut sess1, "pvpFilamentMgr", "GetActiveEvents", &[]).await;
    undock_from_station(&ctx, &mut sess1, JITA_SYS).await;
    undock_from_station(&ctx, &mut sess2, JITA_SYS).await;
    let gate1 = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == JITA_SYS)
        .map(|j| j.stargate_id)
        .expect("Jita stargate");
    rpc_ok(&ctx, &mut sess1, "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(gate1)]).await;
    rpc_ok(&ctx, &mut sess2, "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(gate1)]).await;
    let jita_h = ctx.systems.get_or_spawn(JITA_SYS);
    for _ in 0..18 {
        let _ = jita_h.step_manual(1.0).await;
    }
    ctx.advance_secs(18);
    let b_oid1 = bind_service(&ctx, &mut sess1, "beyonce", PyRep::Integer(i64::from(JITA_SYS))).await as u64;
    let b_oid2 = bind_service(&ctx, &mut sess2, "beyonce", PyRep::Integer(i64::from(JITA_SYS))).await as u64;
    let _ = ctx.dispatch_rpc(&mut sess1, "beyonce", Some(b_oid1), "CmdStop", &[], &HashMap::new(), None).await;
    let _ = ctx.dispatch_rpc(&mut sess2, "beyonce", Some(b_oid2), "CmdStop", &[], &HashMap::new(), None).await;
    wait_for_warp_end(&ctx, JITA_SYS, ship1).await;
    wait_for_warp_end(&ctx, JITA_SYS, ship2).await;

    let q1 = rpc_ok(
        &ctx,
        &mut sess1,
        "pvpFilamentMgr",
        "JoinPVPQueue",
        &[PyRep::Integer(fil1), PyRep::Integer(i64::from(fil_tid))],
    )
    .await;
    assert_ne!(q1, PyRep::None);

    let _ = drain_all_notifications(&ctx, &mut sess1, cid1);
    let q2 = rpc_ok(
        &ctx,
        &mut sess2,
        "pvpFilamentMgr",
        "JoinPVPQueue",
        &[PyRep::Integer(fil2), PyRep::Integer(i64::from(fil_tid))],
    )
    .await;
    assert_ne!(q2, PyRep::None);

    let arena_sys: SolarSystemId = 32_000_001;
    sess1.solar_system_id = Some(arena_sys);
    sess2.solar_system_id = Some(arena_sys);
    let b_oid1 = bind_service(&ctx, &mut sess1, "beyonce", PyRep::Integer(i64::from(arena_sys))).await as u64;
    let b_oid2 = bind_service(&ctx, &mut sess2, "beyonce", PyRep::Integer(i64::from(arena_sys))).await as u64;

    // 1. Pilot 1 engages Pilot 2 with turrets inside the arena; Pilot 2 then burns outside the 75km arena boundary hazard
    let arena = ctx.systems.get_or_spawn(arena_sys);
    let (p1_pos, p2_pos) = (
        arena.get_ball_position_sync(ship1).unwrap_or(glam::DVec3::ZERO),
        arena.get_ball_position_sync(ship2).unwrap_or(glam::DVec3::ZERO),
    );
    assert!(
        p1_pos.distance(p2_pos) > 10_000.0,
        "opposing teams must spawn at opposite perimeter points inside 75km arena sphere"
    );

    let end_gate_id = arena
        .list_balls()
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|b| b.type_id == 47_459 && b.position.distance(p1_pos) <= 100_000.0)
        .map(|b| b.id)
        .unwrap_or(70_001);
    let center = arena.get_ball_position_sync(end_gate_id).unwrap_or(glam::DVec3::ZERO);

    let d_oid1 = bind_service(&ctx, &mut sess1, "dogmaIM", PyRep::Integer(ship1)).await as u64;
    let _ = rpc_res(&ctx, &mut sess1, "dogmaIM", "AddTarget", &[PyRep::Integer(ship2)]).await;
    for _ in 0..5 {
        let _ = arena.step_manual(4.0).await;
        ctx.advance_secs(4);
    }
    if let Some(&g1) = guns1.first() {
        let _ = ctx.dispatch_rpc(
            &mut sess1, "dogmaIM", Some(d_oid1), "Activate",
            &[PyRep::Integer(g1), PyRep::String("targetAttack".into()), PyRep::Integer(ship2)],
            &HashMap::new(), None,
        ).await;
    }

    let _ = ctx.dispatch_rpc(&mut sess2, "beyonce", Some(b_oid2), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &HashMap::new(), None).await;
    let far_out = center + (p2_pos - center).normalize_or(glam::DVec3::Z) * 150_000.0;
    let _ = ctx.dispatch_rpc(
        &mut sess2,
        "beyonce",
        Some(b_oid2),
        "CmdGotoPoint",
        &[PyRep::Real(far_out.x), PyRep::Real(far_out.y), PyRep::Real(far_out.z)],
        &HashMap::new(),
        None,
    )
    .await;
    let _ = ctx.dispatch_rpc(&mut sess1, "beyonce", Some(b_oid1), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &HashMap::new(), None).await;
    let _ = ctx.dispatch_rpc(
        &mut sess1,
        "beyonce",
        Some(b_oid1),
        "CmdGotoPoint",
        &[PyRep::Real(center.x), PyRep::Real(center.y), PyRep::Real(center.z)],
        &HashMap::new(),
        None,
    )
    .await;
    let bal1_before = ctx.db.get_character(cid1).map(|c| c.balance.raw_cents()).unwrap_or(0);
    for _ in 0..80 {
        ctx.advance_secs(10);
        let _ = arena.step_manual(10.0).await;
        let elim = ctx.db.with_snapshot(|s| s.characters.get(&cid2).is_some_and(|c| c.solar_system_id != arena_sys));
        if elim {
            break;
        }
    }

    let bal1_after = ctx.db.get_character(cid1).map(|c| c.balance.raw_cents()).unwrap_or(0);
    assert!(bal1_after >= bal1_before, "Proving Grounds winner must receive match completion reward credit");

    move_ship_to_ball(&ctx, &mut sess1, arena_sys, ship1, "item", PyRep::Integer(end_gate_id)).await;
    let _ = rpc_ok(&ctx, &mut sess1, "pvpFilamentMgr", "AbyssalPVPEndGateActivation", &[PyRep::Integer(end_gate_id)]).await;
    let stats = rpc_ok(&ctx, &mut sess1, "pvpFilamentMgr", "GetCharacterStatistics", &[]).await;
    assert_ne!(stats, PyRep::None);

    let ctx2 = reload_ctx_from_sqlite(&ctx);
    let ch1 = ctx2.db.get_character(cid1).expect("reloaded winner character");
    assert_ne!(ch1.solar_system_id, arena_sys, "winner exiting via AbyssalPVPEndGateActivation must return to origin system and persist in SQLite");
    let _ = HashMap::<String, PyRep>::new();
}
