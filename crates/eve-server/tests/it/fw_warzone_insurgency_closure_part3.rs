//! Part 3 of FW & Insurgency closure tests: Pirate Enlistment, 48h Forecasting -> 7d Active -> WindDown Lifecycle, Spread & Stage 5 Corruption Effects (`SO-5`, `Q-8`).

#![forbid(unsafe_code)]

use crate::fw_warzone_insurgency_closure::setup_warzone_chain;
use crate::sovereignty_equinox_gameplay_closure::{
    drain_notif_events, fit_siege_artillery, lock_target_via_rpc, make_ball,
    register_online_pilot, rpc_call,
};
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_npc_police::{
    is_concord_suppressed_in_system, is_sentry_disabled_by_corruption,
};
use eve_server::services::fw_gameplay::PIRATE_FOB_STRUCTURE_BASE_ID;
use eve_server::services::insurgency_services::{
    ACTIVE_DURATION_SECS, FORECASTING_DURATION_SECS, WINDDOWN_DURATION_SECS,
};
use glam::DVec3;

#[tokio::test]
async fn test_pirate_insurgency_lifecycle_spread_and_stage5_highsec_lowsec_effects() {
    // parity: feat:fw.fw-pirate
    // parity: feat:fw.insurgency-lifecycle
    // parity: feat:fw.insurgency-spread
    // parity: feat:fw.corruption-suppression-effects
    // parity: feat:fw.corruption-stage5-highsec-bubbles-concord
    // parity: notify:OnInsurgencyCampaignStartedForLocation
    // parity: notify:OnInsurgencyCampaignUpdatedForLocation
    // parity: notify:OnInsurgencyStateChangedForFaction
    eve_testkit::attest(&[
        "feat:fw.fw-pirate",
        "feat:fw.insurgency-lifecycle",
        "feat:fw.insurgency-spread",
        "feat:fw.corruption-suppression-effects",
        "feat:fw.corruption-stage5-highsec-bubbles-concord",
        "notify:OnInsurgencyCampaignStartedForLocation",
        "notify:OnInsurgencyCampaignUpdatedForLocation",
        "notify:OnInsurgencyStateChangedForFaction",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (_enemy_sys, front_sys, _cmd_sys, _rear_sys) = setup_warzone_chain(&ctx);

    let pir_cid = 90_000_031i64;
    let pir_ship = 100_000_031i64;
    let mut s_pir = register_online_pilot(
        &ctx, 83_001, "demo", "demo-secret", pir_cid, 98_000_031, None,
        front_sys, pir_ship, 22_456, DVec3::ZERO, 0, // Sabre (Interdictor) at complex grid [0,0,0]
    ).await;

    // 1. Pirate militia enlistment (`500010` Guristas) + auto-seeding Frontline Insurgency in `48h Forecasting`!
    rpc_call(&ctx, &mut s_pir, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_010)])
        .await.expect("enlist Guristas");
    let _ = drain_notif_events(&ctx, pir_cid);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(drain_notif_events(&ctx, pir_cid).iter().any(|e| e == "OnInsurgencyCampaignStartedForLocation"));

    let (camp_id, start_systems) = ctx.db.with_snapshot(|s| {
        let c = s.insurgency_runtime.campaigns.values().next().expect("auto-seeded campaign");
        (c.campaign_id, c.systems.clone())
    });
    assert!(start_systems.contains(&front_sys) && !start_systems.is_empty());

    let vis = rpc_call(&ctx, &mut s_pir, "insurgencySolarsystem", "GetAllVisibleCampaigns", &[]).await.unwrap();
    let snap_fc = rpc_call(&ctx, &mut s_pir, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[]).await.unwrap();
    assert!(format!("{vis:?}").contains(&camp_id.to_string()));
    let PyRep::Tuple(fc_tup) = &snap_fc else { panic!("expected Tuple, got {snap_fc:?}") };
    assert_eq!(fc_tup[1].dict_get("phase").and_then(|v| v.as_str()), Some("Forecasting"));

    // During 48h Forecasting: zero non-FOB insurgency sites exist and Pirate FOB (`1_897_000_000 + camp_id`) is invulnerable!
    let non_fob_fc = ctx.db.with_snapshot(|s| {
        s.insurgency_runtime.insurgency_instances.iter().filter(|i| i.site_type != "PirateStronghold").count()
    });
    assert_eq!(non_fob_fc, 0);
    let fob_id = PIRATE_FOB_STRUCTURE_BASE_ID + camp_id;
    let emp_cid = 90_000_032i64;
    let emp_ship = 100_000_032i64;
    let mut s_emp = register_online_pilot(
        &ctx, 83_002, "demo2", "demo2-secret", emp_cid, 98_000_032, None,
        front_sys, emp_ship, 639, DVec3::new(97_000.0, 0.0, 45_000.0), 0,
    ).await;
    rpc_call(&ctx, &mut s_emp, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)])
        .await.expect("enlist Caldari");
    fit_siege_artillery(&ctx, emp_cid, emp_ship, 999_000);
    assert_eq!(
        // rpc-lint: allow canonical action_plan S-7 / feat:structures structureControl.AttackStructure live combat RPC
        rpc_call(&ctx, &mut s_emp, "structureControl", "AttackStructure", &[PyRep::Integer(fob_id)]).await.unwrap_err().code,
        "FobInvulnerableInPhase"
    );

    // 2. Advance 48h into `7d Active`: combat insurgency sites spawn, and 60s destiny pulses at `[0,0,0]`
    //    advance Corruption stages, emitting `OnInsurgencyCampaignUpdatedForLocation` and spreading at Stage 3!
    ctx.advance_secs(FORECASTING_DURATION_SECS + 10);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let snap_act = rpc_call(&ctx, &mut s_pir, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[]).await.unwrap();
    let PyRep::Tuple(act_tup) = &snap_act else { panic!("expected Tuple, got {snap_act:?}") };
    assert_eq!(act_tup[1].dict_get("phase").and_then(|v| v.as_str()), Some("Active"));

    let _ = drain_notif_events(&ctx, pir_cid);
    let sys_front_h = ctx.systems.get_or_spawn(front_sys);
    for _ in 0..3 {
        sys_front_h.step_manual(60.0).await.unwrap();
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    }
    let stage3_notifs = drain_notif_events(&ctx, pir_cid);
    assert!(stage3_notifs.iter().any(|e| e == "OnInsurgencyCampaignUpdatedForLocation"));
    let after_spread = ctx.db.with_snapshot(|s| s.insurgency_runtime.campaigns[&camp_id].systems.clone());
    assert!(after_spread.len() > start_systems.len(), "Corruption Stage 3 spreads insurgency to adjacent 0.0..=0.8 system");
    assert!(is_sentry_disabled_by_corruption(&ctx, front_sys), "Stage 3 corruption disables gate/station sentry guns");

    // Pulse 2 more times on `front_sys` to reach Corruption Stage 5 (`1,000` pts)!
    for _ in 0..2 {
        sys_front_h.step_manual(60.0).await.unwrap();
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&front_sys].insurgency_corruption_stage), 5);
    let mod_sys = rpc_call(&ctx, &mut s_pir, "securityMgr", "get_modified_systems", &[]).await.unwrap();
    assert!(format!("{mod_sys:?}").contains(&front_sys.to_string()));

    // 3. Low-Sec Stage 5 Corruption: `Interdiction Sphere Launcher I` (`22782`), `Mobile Small Warp Disruptor I` (`12198`),
    //    and `Bomb Launcher I` (`27914`) succeed without `LowSecDisallowed`!
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(pir_cid).or_default();
        for sid in [3_435u32, 12_098, 28_073, 3_319, 11_584] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        for (iid, tid, loc, flg, qty) in [
            (999_100, 22_782, pir_ship, InventoryFlag::HiSlot0, 1), // Interdiction Sphere Launcher I
            (999_101, 22_778, pir_ship, InventoryFlag::HiSlot0, 5), // Warp Disrupt Probe
            (999_102, 12_198, pir_ship, InventoryFlag::Cargo, 1),   // Mobile Small Warp Disruptor I
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: pir_cid,
                    location_id: loc,
                    flag: flg,
                    quantity: qty,
                    singleton: qty == 1 && flg == InventoryFlag::HiSlot0,
                    ..Default::default()
                },
            );
        }
        snap.module_online_states.insert(999_100, true);
    });
    ctx.invalidate_ship_fitting(pir_ship);
    assert!(rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(999_100)]).await.is_ok());
    assert!(rpc_call(&ctx, &mut s_pir, "structureDeployment", "Anchor", &[PyRep::Integer(999_102), PyRep::String("Bubble".into())]).await.is_ok());

    // Switch pirate ship to Manticore (`12032`) with Bomb Launcher I (`27914`) + Concussion Bomb (`27912`) in Low-Sec Stage 5!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&pir_ship).unwrap().type_id = 12_032;
        snap.items.insert(
            999_110,
            InventoryItemRecord {
                item_id: 999_110,
                type_id: 27_914,
                owner_id: pir_cid,
                location_id: pir_ship,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            999_111,
            InventoryItemRecord {
                item_id: 999_111,
                type_id: 27_912,
                owner_id: pir_cid,
                location_id: pir_ship,
                flag: InventoryFlag::HiSlot1,
                quantity: 5,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(999_110, true);
    });
    ctx.invalidate_ship_fitting(pir_ship);
    let _ = sys_front_h.spawn_ball(make_ball(pir_ship, 12_032, DVec3::new(15_000.0, 0.0, 0.0))).await;
    assert!(rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(999_110)]).await.is_ok());

    // 4. High-Sec (`0.5..=0.8`) Stage 5 Corruption: live beacon pulses corrupt a High-Sec system to Stage 5 ->
    //    ship PvP gives `Suspect` without CONCORD, whereas Capsule (`670`) PvP still gives `Criminal` + CONCORD!
    let hs_sys = ctx.sde.solar_systems.values()
        .find(|s| (0.5..=0.8).contains(&s.security_status) && s.solar_system_id < 31_000_000)
        .unwrap()
        .solar_system_id;
    ctx.db.with_mut_snapshot(|s| {
        let c = s.insurgency_runtime.campaigns.get_mut(&camp_id).unwrap();
        if !c.systems.contains(&hs_sys) {
            c.systems.push(hs_sys);
        }
        c.corruption_points.insert(hs_sys, 0);
        c.suppression_points.insert(hs_sys, 0);
        s.system_states.entry(hs_sys).or_default().fw_complex_position = Some([0.0, 0.0, 0.0]);
        s.characters.get_mut(&pir_cid).unwrap().solar_system_id = hs_sys;
        s.items.get_mut(&pir_ship).unwrap().type_id = 639;
        s.items.get_mut(&pir_ship).unwrap().location_id = hs_sys as i64;
    });
    s_pir.solar_system_id = Some(hs_sys);
    fit_siege_artillery(&ctx, pir_cid, pir_ship, 999_200);
    let hs_h = ctx.systems.get_or_spawn(hs_sys);
    let cs_pir_hs = ctx.build_ship_combat_state(pir_cid, pir_ship, 19_722);
    let _ = hs_h.spawn_ball_with_combat(make_ball(pir_ship, 19_722, DVec3::ZERO), cs_pir_hs).await;
    for _ in 0..5 {
        hs_h.step_manual(60.0).await.unwrap();
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    }
    assert!(is_concord_suppressed_in_system(&ctx, hs_sys));

    let neutral_cid = 90_000_033i64;
    let neutral_ship = 100_000_033i64;
    let _s_neut = register_online_pilot(
        &ctx, 83_003, "demo3", "demo3-secret", neutral_cid, 98_000_033, None,
        hs_sys, neutral_ship, 621, DVec3::new(2_500.0, 0.0, 0.0), 0,
    ).await;
    lock_target_via_rpc(&ctx, &mut s_pir, hs_sys, pir_ship, neutral_ship).await;
    rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(999_200), PyRep::Integer(neutral_ship)])
        .await.expect("attack ship in High-Sec Stage 5");
    let now_hs = ctx.now_unix();
    let (ship_susp, ship_crim, ship_concord) = ctx.crimewatch.with_crimewatch(|cw| {
        let st = &cw.states[&pir_cid];
        (st.is_suspect(now_hs), st.is_criminal(now_hs), st.pending_concord_arrival.is_some())
    });
    assert_eq!((ship_susp, ship_crim, ship_concord), (true, false, false), "High-Sec Stage 5 ship attack triggers Suspect without CONCORD");

    // Attack a Capsule (`670`) in High-Sec Stage 5 -> triggers Criminal + CONCORD!
    let pod_cid = 90_000_034i64;
    let pod_id = 100_000_034i64;
    let _s_pod = register_online_pilot(
        &ctx, 83_004, "demo4", "demo4-secret", pod_cid, 98_000_034, None,
        hs_sys, pod_id, 670, DVec3::new(2_800.0, 0.0, 0.0), 0,
    ).await;
    ctx.advance_secs(25);
    lock_target_via_rpc(&ctx, &mut s_pir, hs_sys, pir_ship, pod_id).await;
    rpc_call(&ctx, &mut s_pir, "dogmaIM", "Activate", &[PyRep::Integer(999_200), PyRep::Integer(pod_id)])
        .await.expect("attack capsule in High-Sec Stage 5");
    let now_pod = ctx.now_unix();
    let (pod_crim, pod_concord) = ctx.crimewatch.with_crimewatch(|cw| {
        let st = &cw.states[&pir_cid];
        (st.is_criminal(now_pod), st.pending_concord_arrival.is_some())
    });
    assert_eq!((pod_crim, pod_concord), (true, true), "High-Sec Stage 5 capsule attack still triggers Criminal + CONCORD");

    // 5. Advance past 7d Active + 48h WindDown -> campaign ends, clearing all Corruption stages and emitting `OnInsurgencyStateChangedForFaction`!
    let _ = drain_notif_events(&ctx, pir_cid);
    ctx.advance_secs(ACTIVE_DURATION_SECS + WINDDOWN_DURATION_SECS + 120);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let end_notifs = drain_notif_events(&ctx, pir_cid);
    assert!(end_notifs.iter().any(|e| e == "OnInsurgencyStateChangedForFaction"));
    assert_eq!(ctx.db.with_snapshot(|s| s.system_states[&hs_sys].insurgency_corruption_stage), 0);
}
