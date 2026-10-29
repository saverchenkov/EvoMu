//! Part 2 of FW & Insurgency closure tests: FW I-Hub Siege/Regen, System Tiers, War HQs, Leaderboards, FW Agents & Corp/Alliance Enlistment (`SO-5`, `Q-8`).

#![forbid(unsafe_code)]

use crate::fw_warzone_insurgency_closure::setup_warzone_chain;
use crate::sovereignty_equinox_gameplay_closure::{
    drain_notif_events, fit_siege_artillery, lock_target_via_rpc, make_ball, open_sqlite_ctx,
    register_online_pilot, rpc_call, set_pilot_location, step_weapon_ticks, temp_sqlite_path,
};
use eve_db::{corp_roles, IskCents};
use eve_proto::PyRep;
use eve_server::services::fw_gameplay::TYPE_FW_IHUB;
use eve_server::services::fw_services::STATUS_FRONTLINE;
use glam::DVec3;

const FW_ENLISTMENT_COOLDOWN_SECS: u64 = 24 * 3_600;
const IHUB_UNATTACKED_REGEN_SECS: u64 = 2 * 3_600;

#[tokio::test]
async fn test_fw_ihub_siege_tiers_war_hqs_leaderboards_missions_and_corp_enlistment() {
    // parity: feat:fw.infrastructure-hub
    // parity: feat:fw.ihub-capture-and-tier-scaling
    // parity: feat:fw.war-hq-systems
    // parity: feat:fw.fw-leaderboards
    // parity: feat:fw.fw-missions
    // parity: notify:OnWarzoneOccupationStateUpdated
    eve_testkit::attest(&[
        "feat:fw.infrastructure-hub",
        "feat:fw.ihub-capture-and-tier-scaling",
        "feat:fw.war-hq-systems",
        "feat:fw.fw-leaderboards",
        "feat:fw.fw-missions",
        "notify:OnWarzoneOccupationStateUpdated",
    ]);

    let db_path = temp_sqlite_path("fw_warzone_f2");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (_enemy_sys, front_sys, cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let cal_cid = 90_000_021i64;
    let cal_corp = 98_000_021i64;
    let cal_ally = 99_000_021i64;
    let cal_ship = 100_000_021i64;

    let gal_cid = 90_000_022i64;
    let gal_corp = 98_000_022i64;
    let gal_ship = 100_000_022i64;

    let mut s_cal = register_online_pilot(
        &ctx, 82_001, "demo", "demo-secret", cal_cid, cal_corp, Some(cal_ally),
        front_sys, cal_ship, 639, DVec3::new(202_000.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;
    let mut s_gal = register_online_pilot(
        &ctx, 82_002, "demo2", "demo2-secret", gal_cid, gal_corp, None,
        front_sys, gal_ship, 639, DVec3::new(202_500.0, 0.0, 0.0), corp_roles::ROLE_DIRECTOR,
    ).await;

    // 1. War HQ systems (`fwWarzoneSolarsystem.GetHQSystemIDs`) & unrestricted War HQ station access (`stationSvc.GetStation`).
    let hqs = rpc_call(&ctx, &mut s_cal, "fwWarzoneSolarsystem", "GetHQSystemIDs", &[]).await.unwrap();
    let hq_str = format!("{hqs:?}");
    for hq_sys in [30_002_974u32, 30_045_324, 30_003_788, 30_002_055] {
        assert!(hq_str.contains(&hq_sys.to_string()));
        let war_hq_sta = ctx.sde.solar_systems[&hq_sys].stations[0].station_id;
        let sta_rep = rpc_call(&ctx, &mut s_cal, "stationSvc", "GetStation", &[PyRep::Integer(war_hq_sta)]).await.unwrap();
        assert_eq!(sta_rep.dict_get("isWarHQ"), Some(&PyRep::Bool(true)));
        assert!(matches!(sta_rep.dict_get("warHQServices"), Some(PyRep::List(v)) if !v.is_empty()));
    }
    let jita_rep = rpc_call(&ctx, &mut s_cal, "stationSvc", "GetStation", &[PyRep::Integer(60_003_760)]).await.unwrap();
    assert_eq!(jita_rep.dict_get("isWarHQ"), Some(&PyRep::Bool(false)));

    // 2. Corporate & Alliance Militia Enlistment (`facWarMgr`), 24h cooldown, and individual pilot preservation on corp leave!
    let mut s_norole = register_online_pilot(
        &ctx, 82_003, "demo3", "demo3-secret", 90_000_023, cal_corp, Some(cal_ally),
        front_sys, 100_000_023, 587, DVec3::new(10_000.0, 0.0, 0.0), 0,
    ).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_norole, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(cal_corp), PyRep::Integer(500_001)])
            .await.unwrap_err().code,
        "AccessDenied"
    );
    // Individual direct enlistment for pilot `90_000_023` BEFORE corp joins/leaves.
    rpc_call(&ctx, &mut s_norole, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
        .await.expect("direct enlistment");

    rpc_call(&ctx, &mut s_cal, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(cal_corp), PyRep::Integer(500_001)])
        .await.expect("JoinFactionAsCorporation");
    assert_eq!(
        rpc_call(&ctx, &mut s_cal, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(cal_corp), PyRep::Integer(500_001)])
            .await.unwrap_err().code,
        "CorporationAlreadyEnlisted"
    );
    assert_eq!(
        rpc_call(&ctx, &mut s_cal, "facWarMgr", "LeaveFactionAsCorporation", &[PyRep::Integer(cal_corp), PyRep::Integer(500_001)])
            .await.unwrap_err().code,
        "MilitiaCooldownActive"
    );
    ctx.advance_secs(FW_ENLISTMENT_COOLDOWN_SECS + 10);
    rpc_call(&ctx, &mut s_cal, "facWarMgr", "LeaveFactionAsCorporation", &[PyRep::Integer(cal_corp), PyRep::Integer(500_001)])
        .await.expect("LeaveFactionAsCorporation after 24h");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&90_000_023].war_faction_id),
        Some(500_001),
        "leaving corporate militia preserves individually direct-enlisted member"
    );

    // Alliance enlistment & re-enlistment for Caldari (`500001`) and Gallente (`500004`).
    ctx.advance_secs(FW_ENLISTMENT_COOLDOWN_SECS + 10);
    rpc_call(&ctx, &mut s_cal, "facWarMgr", "JoinFactionAsAlliance", &[PyRep::Integer(cal_ally), PyRep::Integer(500_001)])
        .await.expect("JoinFactionAsAlliance");
    rpc_call(&ctx, &mut s_gal, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(gal_corp), PyRep::Integer(500_004)])
        .await.expect("Gallente corp enlist");

    // 3. I-Hub LP Donations (`facWarMgr.DonateLPsToSolarSystem`) & live `marketProxy.PlaceBuyOrder` broker fee reduction!
    assert_eq!(
        rpc_call(&ctx, &mut s_cal, "facWarMgr", "DonateLPsToSolarSystem", &[PyRep::Integer(30_000_142), PyRep::Integer(1_000)])
            .await.unwrap_err().code,
        "NotInWarzone"
    );
    // Find a warzone system occupied by Caldari (or seed one) that has an NPC station so we can compare real broker fees!
    let (mkt_sys, mkt_sta, mkt_reg) = ctx.sde.solar_systems.values()
        .filter(|s| s.security_status > 0.0 && s.security_status < 0.45 && !s.stations.is_empty())
        .map(|s| (s.solar_system_id, s.stations[0].station_id, s.region_id))
        .next()
        .expect("low-sec system with NPC station");
    ctx.db.with_mut_snapshot(|s| {
        s.fw_system_occupations.entry(mkt_sys).or_insert(eve_db::snapshot_ext::FwSystemOccupationRecord {
            solar_system_id: mkt_sys,
            owner_faction_id: 500_001,
            occupier_faction_id: 500_001,
            victory_points: 0,
            victory_points_threshold: 3_000,
        }).occupier_faction_id = 500_001;
        let ch = s.characters.get_mut(&cal_cid).unwrap();
        ch.solar_system_id = mkt_sys;
        ch.station_id = Some(mkt_sta);
        ch.region_id = mkt_reg;
        ch.constellation_id = ctx.sde.solar_systems[&mkt_sys].constellation_id;
        ch.balance = IskCents::from_isk(500_000_000);
        ch.loyalty_points.insert(1_000_180, 500_000);
    });
    s_cal.solar_system_id = Some(mkt_sys);
    s_cal.station_id = Some(mkt_sta);
    s_cal.region_id = Some(mkt_reg);

    let bp_id = *ctx.sde.blueprints.keys().next().unwrap() as i64;
    let bal0 = ctx.db.with_snapshot(|s| s.characters[&cal_cid].balance.0);
    rpc_call(
        &ctx, &mut s_cal, "marketProxy", "PlaceBuyOrder",
        &[PyRep::Integer(mkt_sta), PyRep::Integer(34), PyRep::Real(1_000.0), PyRep::Integer(1_000), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90)],
    ).await.expect("PlaceBuyOrder at Tier 0");
    let fee_tier0 = (bal0 - ctx.db.with_snapshot(|s| s.characters[&cal_cid].balance.0)) - 100_000_000;
    let bp_data0 = rpc_call(&ctx, &mut s_cal, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(bp_id)]).await.unwrap();
    let tax_tier0 = bp_data0.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
    assert!(tax_tier0 > 0.0);
    assert!(fee_tier0 > 0);

    for (tier, donate_amt) in [(1u8, 40_000), (2, 20_000), (3, 30_000), (4, 50_000), (5, 60_000)] {
        let don = rpc_call(&ctx, &mut s_cal, "facWarMgr", "DonateLPsToSolarSystem", &[PyRep::Integer(mkt_sys as i64), PyRep::Integer(donate_amt)]).await.unwrap();
        assert_eq!(don.dict_get("tier").and_then(|v| v.as_i64()), Some(tier as i64));

        let expected_mult = 1.0 - 0.10 * (tier as f64);

        ctx.db.with_mut_snapshot(|s| s.market_orders.clear());
        let bal_before = ctx.db.with_snapshot(|s| s.characters[&cal_cid].balance.0);
        rpc_call(
            &ctx, &mut s_cal, "marketProxy", "PlaceBuyOrder",
            &[PyRep::Integer(mkt_sta), PyRep::Integer(34), PyRep::Real(1_000.0), PyRep::Integer(1_000), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90)],
        ).await.expect("PlaceBuyOrder");
        let fee_t = (bal_before - ctx.db.with_snapshot(|s| s.characters[&cal_cid].balance.0)) - 100_000_000;
        let expected_fee = ((fee_tier0 as f64) * expected_mult).round() as i64;
        assert!((fee_t - expected_fee).abs() <= 100, "broker fee at Tier {tier}: {fee_t} expected ~{expected_fee}");

        let bp_data = rpc_call(&ctx, &mut s_cal, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(bp_id)]).await.unwrap();
        let tax_t = bp_data.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
        let expected_tax = ((tax_tier0 * expected_mult) * 100.0).round() / 100.0;
        assert!((tax_t - expected_tax).abs() <= 0.05, "facility tax at Tier {tier}: {tax_t} expected ~{expected_tax}");
    }

    // Move Caldari pilot back to `front_sys` in space.
    set_pilot_location(&ctx, &mut s_cal, front_sys, None);

    // 4. FW I-Hub (`32226`) invulnerability before 100% contested, 2h unattacked regeneration to 80%, and live siege flip!
    let _ = rpc_call(&ctx, &mut s_gal, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    let ihub_id = ctx.db.with_snapshot(|s| {
        s.structures.values().find(|st| st.solar_system_id == front_sys && st.type_id == TYPE_FW_IHUB).unwrap().structure_id
    });
    fit_siege_artillery(&ctx, gal_cid, gal_ship, 998_000);
    lock_target_via_rpc(&ctx, &mut s_gal, front_sys, gal_ship, ihub_id).await;
    assert_eq!(
        rpc_call(&ctx, &mut s_gal, "dogmaIM", "Activate", &[
            PyRep::Integer(998_000),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(ihub_id),
        ]).await.unwrap_err().code,
        "IHubInvulnerable"
    );

    let vulnerable_ts = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        s.fw_system_occupations.get_mut(&front_sys).unwrap().victory_points = 3_000;
        s.fw_runtime.system_victory_points.insert(front_sys, 3_000);
        s.fw_runtime.ihub_vulnerable.insert(front_sys, true);
        s.fw_runtime.last_ihub_attacked_unix.insert(front_sys, vulnerable_ts);
        s.system_states.entry(front_sys).or_default().fw_contested_pct = 100.0;
    });
    ctx.advance_secs(IHUB_UNATTACKED_REGEN_SECS + 5);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.system_states[&front_sys].fw_contested_pct),
        80.0,
        "unattacked 100%-contested FW I-Hub regenerates back to 80% after 2 hours"
    );

    // Re-contest to 100% and destroy FW I-Hub via `dogmaIM.Activate`: flips occupancy, emits `OnWarzoneOccupationStateUpdated`, and updates `cmd_sys` to Frontline!
    let attack_ts = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        s.fw_system_occupations.get_mut(&front_sys).unwrap().victory_points = 3_000;
        s.fw_runtime.system_victory_points.insert(front_sys, 3_000);
        s.fw_runtime.ihub_vulnerable.insert(front_sys, true);
        s.fw_runtime.last_ihub_attacked_unix.insert(front_sys, attack_ts);
        s.system_states.entry(front_sys).or_default().fw_contested_pct = 100.0;
        if let Some(st) = s.structures.get_mut(&ihub_id) {
            st.shield_hp = 0.0;
            st.armor_hp = 0.0;
            st.hull_hp = 50.0;
        }
    });
    lock_target_via_rpc(&ctx, &mut s_gal, front_sys, gal_ship, ihub_id).await;
    let _ = drain_notif_events(&ctx, gal_cid);
    for _ in 0..8 {
        let _ = rpc_call(&ctx, &mut s_gal, "dogmaIM", "Activate", &[
            PyRep::Integer(998_000),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(ihub_id),
        ]).await;
        step_weapon_ticks(&ctx, front_sys).await;
        if ctx.db.with_snapshot(|s| s.fw_system_occupations[&front_sys].occupier_faction_id == 500_004) {
            break;
        }
        ctx.advance_secs(25);
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.fw_system_occupations[&front_sys].occupier_faction_id), 500_004);
    assert!(drain_notif_events(&ctx, gal_cid).iter().any(|e| e == "OnWarzoneOccupationStateUpdated"));
    set_pilot_location(&ctx, &mut s_gal, cmd_sys, None);
    let cmd_after = rpc_call(&ctx, &mut s_gal, "fwWarzoneSolarsystem", "GetLocalOccupationState", &[]).await.unwrap();
    let PyRep::Tuple(cmd_tup) = &cmd_after else { panic!("expected Tuple, got {cmd_after:?}") };
    assert_eq!(cmd_tup[1].dict_get("adjacencyState").and_then(|v| v.as_i64()), Some(STATUS_FRONTLINE));
    set_pilot_location(&ctx, &mut s_gal, front_sys, None);

    // 5. Live militia PvP ship kill updates `facWarMgr.GetStats_TopAndAllKillsAndVPs`!
    let mut victim_cs = ctx.build_ship_combat_state(cal_cid, cal_ship, 639);
    victim_cs.shield_hp = 100.0;
    victim_cs.armor_hp = 100.0;
    victim_cs.structure_hp = 100.0;
    let _ = ctx.systems.get_or_spawn(front_sys).spawn_ball_with_combat(make_ball(cal_ship, 639, DVec3::new(202_500.0, 0.0, 0.0)), victim_cs).await;
    let atk_cs = ctx.build_ship_combat_state(gal_cid, gal_ship, 19_722);
    let _ = ctx.systems.get_or_spawn(front_sys).spawn_ball_with_combat(make_ball(gal_ship, 19_722, DVec3::new(200_000.0, 0.0, 0.0)), atk_cs).await;
    lock_target_via_rpc(&ctx, &mut s_gal, front_sys, gal_ship, cal_ship).await;
    ctx.set_invention_roll_override(Some(0.05));
    rpc_call(&ctx, &mut s_gal, "dogmaIM", "Activate", &[PyRep::Integer(998_000), PyRep::Integer(cal_ship)])
        .await.expect("destroy enemy militia ship");

    // Add historical FW stat events at distinct rolling window boundaries:
    // Event 1 (just occurred): 1 kill, 20 VP (within yesterday < 24h, lastWeek < 7d, total)
    // Event 2 (2 days ago): 2 kills, 40 VP (outside yesterday, within lastWeek < 7d, total)
    // Event 3 (10 days ago): 3 kills, 60 VP (outside 7d, folded into total and pruned from stat_events)
    let now_sec = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.stat_events.push(eve_db::PersistedFwStatEvent {
            timestamp_unix: now_sec - 2 * 86_400,
            character_id: gal_cid,
            corporation_id: gal_corp,
            alliance_id: None,
            faction_id: 500_004,
            kills: 2,
            victory_points: 40,
        });
        snap.fw_runtime.stat_events.push(eve_db::PersistedFwStatEvent {
            timestamp_unix: now_sec - 10 * 86_400,
            character_id: gal_cid,
            corporation_id: gal_corp,
            alliance_id: None,
            faction_id: 500_004,
            kills: 3,
            victory_points: 60,
        });
        eve_server::services::fw_services::prune_and_fold_fw_stat_events_in_snap(snap, now_sec);
    });

    let lb = rpc_call(&ctx, &mut s_gal, "facWarMgr", "GetStats_TopAndAllKillsAndVPs", &[]).await.unwrap();
    let find_stat = |cat_name: &str, field_key: &str, id_key: &str, expected_id: i64| -> i64 {
        let sub = lb.dict_get(cat_name).unwrap();
        let list = sub.dict_get(field_key).and_then(|v| match v { PyRep::List(l) => Some(l), _ => None }).unwrap();
        list.iter()
            .find(|row| row.dict_get(id_key).and_then(|v| v.as_i64()) == Some(expected_id))
            .and_then(|row| row.dict_get("amount").and_then(|v| v.as_i64()))
            .unwrap_or(0)
    };

    // Verify rolling window progression: yesterday (1) < lastWeek (1+2=3) < total (1+2+3=6)
    assert_eq!(find_stat("characters", "killsY", "characterID", gal_cid), 1);
    assert_eq!(find_stat("characters", "killsLW", "characterID", gal_cid), 3);
    assert_eq!(find_stat("characters", "killsTotal", "characterID", gal_cid), 6);
    assert!(find_stat("characters", "killsY", "characterID", gal_cid) < find_stat("characters", "killsLW", "characterID", gal_cid));
    assert!(find_stat("characters", "killsLW", "characterID", gal_cid) < find_stat("characters", "killsTotal", "characterID", gal_cid));

    assert_eq!(find_stat("corporations", "killsY", "corporationID", gal_corp), 1);
    assert_eq!(find_stat("corporations", "killsLW", "corporationID", gal_corp), 3);
    assert_eq!(find_stat("corporations", "killsTotal", "corporationID", gal_corp), 6);

    assert_eq!(find_stat("factions", "killsY", "factionID", 500_004), 1);
    assert_eq!(find_stat("factions", "killsLW", "factionID", 500_004), 3);
    assert_eq!(find_stat("factions", "killsTotal", "factionID", 500_004), 6);

    // Verify rolling window progression for victory points: yesterday (20) < lastWeek (60) < total (120)
    assert_eq!(find_stat("characters", "vpY", "characterID", gal_cid), 20);
    assert_eq!(find_stat("characters", "vpLW", "characterID", gal_cid), 60);
    assert_eq!(find_stat("characters", "vpTotal", "characterID", gal_cid), 120);
    assert!(find_stat("characters", "vpY", "characterID", gal_cid) < find_stat("characters", "vpLW", "characterID", gal_cid));
    assert!(find_stat("characters", "vpLW", "characterID", gal_cid) < find_stat("characters", "vpTotal", "characterID", gal_cid));

    assert_eq!(find_stat("corporations", "vpY", "corporationID", gal_corp), 20);
    assert_eq!(find_stat("corporations", "vpLW", "corporationID", gal_corp), 60);
    assert_eq!(find_stat("corporations", "vpTotal", "corporationID", gal_corp), 120);

    assert_eq!(find_stat("factions", "vpY", "factionID", 500_004), 20);
    assert_eq!(find_stat("factions", "vpLW", "factionID", 500_004), 60);
    assert_eq!(find_stat("factions", "vpTotal", "factionID", 500_004), 120);

    // Verify bounded log: events older than 7 days were folded and pruned from active stat_events
    ctx.db.with_snapshot(|snap| {
        assert!(snap.fw_runtime.stat_events.iter().all(|e| now_sec.saturating_sub(e.timestamp_unix) <= 7 * 86_400));
        assert_eq!(snap.fw_runtime.historical_char_totals.get(&gal_cid), Some(&(3, 60)));
    });

    // 6. SDE FW Agent (`agent_type_id == 9`) mission gating (`NotEnlistedInMilitia`) & repeatable acceptance!
    let (fw_agent_id, fw_sys, fw_sta, fw_fac) = ctx.sde.agents.values()
        .filter(|a| a.agent_type_id == 9 && a.solar_system_id.is_some() && a.station_id.is_some())
        .find_map(|a| {
            let fac = ctx.sde.factions.values().find(|f| f.militia_corporation_id == Some(a.corporation_id))?.faction_id as i64;
            Some((a.agent_id, a.solar_system_id.unwrap(), a.station_id.unwrap(), fac))
        })
        .expect("SDE FW agent");
    s_norole.solar_system_id = Some(fw_sys);
    s_norole.station_id = Some(fw_sta);
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&90_000_023).unwrap();
        ch.solar_system_id = fw_sys;
        ch.station_id = Some(fw_sta);
        ch.war_faction_id = None;
    });
    assert_eq!(
        rpc_call(&ctx, &mut s_norole, "agentMgr", "DoAction", &[PyRep::Integer(fw_agent_id), PyRep::String("RequestMission".into())])
            .await.unwrap_err().code,
        "NotEnlistedInMilitia"
    );
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&90_000_023).unwrap();
        ch.war_faction_id = Some(fw_fac);
        ch.standings.insert(fw_fac, 8.0);
    });
    s_norole.war_faction_id = Some(fw_fac);
    rpc_call(&ctx, &mut s_norole, "agentMgr", "DoAction", &[PyRep::Integer(fw_agent_id), PyRep::String("RequestMission".into())])
        .await.expect("Request FW mission");
    rpc_call(&ctx, &mut s_norole, "agentMgr", "DoAction", &[PyRep::Integer(fw_agent_id), PyRep::String("AcceptMission".into())])
        .await.expect("Accept FW mission");
    let journal = rpc_call(&ctx, &mut s_norole, "agentMgr", "GetMyJournalDetails", &[]).await.unwrap();
    assert!(format!("{journal:?}").contains(&fw_agent_id.to_string()));

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}
