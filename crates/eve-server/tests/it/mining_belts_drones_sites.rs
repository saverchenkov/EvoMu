//! Cosmic Mining Sites Integration Tests (`MN-15`): Gas Sites, Ore Anomalies, Rare Signatures & Sov Ore Sites.
//!

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::mining_sites;
use eve_server::services::mining_sites::seed_system_mining_sites_from_sde;
use glam::DVec3;
use std::collections::HashMap;

async fn create_auth_session(ctx: &ServerContext, session_id: u64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await.expect("Login failed");
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.expect("SelectCharacterID failed");
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    sess.region_id = Some(10_000_002);
    sess
}

async fn bind_service_oid(ctx: &ServerContext, sess: &mut SessionState, service: &str, bind_param: PyRep) -> u64 {
    let kw = HashMap::new();
    let res = ctx.dispatch_rpc(sess, service, None, "MachoBindObject", &[bind_param, PyRep::None], &kw, None).await.expect("MachoBindObject failed");
    let PyRep::Tuple(outer) = res else { panic!("Expected Tuple from MachoBindObject") };
    outer[0].as_i64().expect("bound_id integer") as u64
}

async fn activate_in_range(
    ctx: &ServerContext,
    sess: &mut SessionState,
    handle: &eve_server::system_actor::SystemHandle,
    _dogma_oid: u64,
    beyonce_oid: u64,
    module_id: i64,
    target_id: i64,
) -> eve_server::services::mining_cycle::MiningCycleOutcome {
    let kw = HashMap::new();
    for _attempt in 0..3 {
        match crate::mining_cycle_engine::mine_once(ctx, sess, module_id, target_id).await {
            Ok(res) => return res,
            Err(code) if code == "TargetOutOfRange" || code == "TargetingFailed" => {
                ctx.dispatch_rpc(sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(target_id), PyRep::Real(1_000.0)], &kw, None)
                    .await
                    .expect("CmdFollowBall toward out-of-range rock");
                for _ in 0..60 {
                    handle.step_manual(1.0).await.unwrap();
                }
            }
            Err(e) => panic!("mining cycle on {target_id} failed: {e:?}"),
        }
    }
    panic!("rock {target_id} still out of range after approaching");
}

// ============================================================================
// MN-15: Mining Sites: Gas Sites, Ore Anomalies, Rare Signatures & Sov Ore Sites
// ============================================================================
#[tokio::test]
async fn test_mn15_mining_sites_gas_anomalies_signatures_sov() {
    // parity: feat:mining.gas-sites, feat:mining.ore-anomalies, feat:mining.ore-signatures, feat:mining.sov-ore-sites, rpc:sovMgr.AcquireSkyhooks
    eve_testkit::attest(&["feat:mining.gas-sites", "feat:mining.ore-anomalies", "feat:mining.ore-signatures", "feat:mining.sov-ore-sites"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = create_auth_session(&ctx, 1003).await;
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_180_u32; // Osmon (The Forge), true sec 0.680 -> displayed 0.7
    let au = 149_597_870_700.0_f64;

    let ship_id = 700_001_500_i64;
    let miner_mod_id = 700_001_501_i64;
    let gas_mod_id = 700_001_502_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(3386, eve_db::TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 });
        sk.insert(25544, eve_db::TrainedSkillRecord { type_id: 25544, level: 5, skill_points: 256_000.0 });
        for (iid, tid, loc, flag) in [
            (ship_id, 32880u32, sys_id as i64, InventoryFlag::Hangar),
            (miner_mod_id, 483u32, ship_id, InventoryFlag::HiSlot0),
            (gas_mod_id, 25266u32, ship_id, InventoryFlag::HiSlot1),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord { item_id: iid, type_id: tid, owner_id: char_id, location_id: loc, flag, quantity: 1, singleton: true, version: 1, ..Default::default() },
            );
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.ship_id = Some(ship_id);

    let handle = ctx.systems.get_or_spawn(sys_id);
    handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 32880,
            position: DVec3::new(100_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 335.0,
            inertia: 2.8,
            mass: 1_200_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    let beyonce_oid = bind_service_oid(&ctx, &mut sess, "beyonce", PyRep::Integer(sys_id as i64)).await;
    let dogma_oid = bind_service_oid(&ctx, &mut sess, "dogmaIM", PyRep::Tuple(vec![PyRep::Integer(sys_id as i64), PyRep::Integer(5)])).await;

    // ------------------------------------------------------------------------
    // 0. Site selection by security band, from real SDE data
    // ------------------------------------------------------------------------
    for def in mining_sites::COSMIC_ORE_SITES {
        let site = mining_sites::resolve_ore_site(&ctx.sde, def)
            .unwrap_or_else(|e| panic!("{} must resolve: {e}", def.dungeon_name));
        let layout = &ctx.sde.dungeon_asteroids[&site.dungeon_id];
        let layout_total: u32 = layout.iter().map(|e| e.asteroid_count).sum();
        assert_eq!(site.asteroids.len(), layout_total as usize, "{}", def.dungeon_name);
        let wiki_total: i64 = def.rows.iter().map(|r| r.total_units).sum();
        let site_total: i64 = site.asteroids.iter().map(|a| a.units).sum();
        assert_eq!(site_total, wiki_total, "{}", def.dungeon_name);
    }
    for def in mining_sites::GAS_SITES {
        mining_sites::resolve_gas_site(&ctx.sde, def)
            .unwrap_or_else(|e| panic!("{} must resolve: {e}", def.dungeon_name));
    }
    for def in mining_sites::WORMHOLE_GAS_SITES {
        let (did, clouds) = mining_sites::resolve_wormhole_gas_site(&ctx.sde, def)
            .unwrap_or_else(|e| panic!("{} must resolve: {e}", def.dungeon_name));
        assert!(did >= 3555);
        assert_eq!(clouds.len(), 2);
        assert!(def.guard_npc_count >= 5);
        assert_eq!(def.guard_spawn_delay_secs, 900);
    }
    for def in mining_sites::SOV_PROSPECTING_DEPOSITS {
        mining_sites::resolve_ore_site(&ctx.sde, &def.site)
            .unwrap_or_else(|e| panic!("{} must resolve: {e}", def.site.dungeon_name));
    }
    assert!(!mining_sites::is_ore_anomaly_expired(mining_sites::ORE_ANOMALY_EXPIRY_SECS - 1));
    assert!(mining_sites::is_ore_anomaly_expired(mining_sites::ORE_ANOMALY_EXPIRY_SECS));

    // Jita (true sec 0.946, displayed 0.9) gets the high-sec Hidden Omber Deposit signature but no ore
    // anomaly: high-sec ore anomalies only spawn at 0.5-0.8. Wormhole space gets no k-space ore anomaly,
    // and gets the guarded W-space Fullerite gas reservoir (`Barren Perimeter Reservoir`).
    let jita_id = 30_000_142_u32;
    seed_system_mining_sites_from_sde(&ctx, jita_id).await;
    seed_system_mining_sites_from_sde(&ctx, 31_000_005).await;
    let (jita_anoms, jita_sigs, wh_ore_anoms, wh_gas_sigs) = ctx.db.with_snapshot(|snap| {
        let anoms: Vec<String> = snap
            .cosmic_anomalies
            .get(&jita_id)
            .map(|l| l.iter().map(|a| a.name.clone()).collect())
            .unwrap_or_default();
        let sigs: Vec<(String, String)> = snap
            .cosmic_signatures
            .get(&jita_id)
            .map(|l| l.iter().map(|s| (s.sig_group.clone(), s.type_name.clone())).collect())
            .unwrap_or_default();
        let wh_ore = snap.cosmic_anomalies.get(&31_000_005).map_or(0, |l| l.len());
        let wh_gas: Vec<(String, String)> = snap
            .cosmic_signatures
            .get(&31_000_005)
            .map(|l| {
                l.iter()
                    .filter(|s| s.sig_group == "Gas")
                    .map(|s| (s.sig_group.clone(), s.type_name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        (anoms, sigs, wh_ore, wh_gas)
    });
    assert!(jita_anoms.is_empty(), "No ore anomaly in a 0.9 system, got {jita_anoms:?}");
    assert_eq!(
        jita_sigs,
        vec![("Ore".to_string(), "Hidden Omber Deposit".to_string())]
    );
    assert_eq!(wh_ore_anoms, 0, "No k-space ore anomaly in wormhole space");
    assert_eq!(
        wh_gas_sigs,
        vec![("Gas".to_string(), "Barren Perimeter Reservoir".to_string())]
    );

    // ------------------------------------------------------------------------
    // 1. Ore Anomalies via V24 scanSvc (feat:mining.ore-anomalies)
    // ------------------------------------------------------------------------
    let scan_mgr_res = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "GetSystemScanMgr",
            &[],
            &kw,
            None,
        )
        .await
        .expect("scanSvc.GetSystemScanMgr");
    let PyRep::List(anom_list) = scan_mgr_res
        .dict_get("anomalies")
        .expect("anomalies in GetSystemScanMgr")
    else {
        panic!("Expected List of anomalies");
    };
    let ore_anom = anom_list
        .iter()
        .find(|a| {
            a.dict_get("siteName").and_then(|v| v.as_str()) == Some("Small Kernite and Omber Deposit")
        })
        .expect("Small Kernite and Omber Deposit anomaly in Osmon");
    let ore_anom_id = ore_anom
        .dict_get("anomalyID")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    assert_eq!(ore_anom.dict_get("signalStrength").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(ore_anom.dict_get("probeRequired").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        ore_anom.dict_get("oreTypes"),
        Some(&PyRep::List(
            [20, 1227, 17452, 17453, 17867, 17868]
                .into_iter()
                .map(PyRep::Integer)
                .collect()
        ))
    );
    let PyRep::List(anom_ast_reps) = ore_anom.dict_get("asteroidBallIDs").unwrap() else {
        panic!("Expected asteroidBallIDs");
    };
    let anom_ast_ids: Vec<i64> = anom_ast_reps.iter().map(|v| v.as_i64().unwrap()).collect();
    assert_eq!(anom_ast_ids.len(), 18, "4+7+2+2+1+2 asteroids from the client layout");

    let per_type = ctx.db.with_snapshot(|snap| {
        let mut m: HashMap<u32, (u32, i64)> = HashMap::new();
        for id in &anom_ast_ids {
            let a = &snap.asteroids[id];
            let e = m.entry(a.type_id).or_default();
            e.0 += 1;
            e.1 += a.quantity;
        }
        m
    });
    let expected: HashMap<u32, (u32, i64)> = [
        (20, (4, 29_726)),
        (1227, (7, 47_013)),
        (17452, (2, 16_965)),
        (17453, (2, 11_811)),
        (17867, (1, 5_754)),
        (17868, (2, 12_921)),
    ]
    .into_iter()
    .collect();
    assert_eq!(per_type, expected);

    // Warp to the ore anomaly via beyonce.CmdWarpToStuff("scan", ore_anom_id)
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_oid),
        "CmdWarpToStuff",
        &[
            PyRep::String("scan".into()),
            PyRep::String(ore_anom_id.clone()),
        ],
        &kw,
        None,
    )
    .await
    .expect("beyonce.CmdWarpToStuff to ore anomaly");
    for _ in 0..40 {
        handle.step_manual(1.0).await.unwrap();
        let (_, b) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        if matches!(b.mode, MovementMode::Stop) {
            break;
        }
    }

    // Directional scan at the anomaly detects its asteroid balls
    let dscan_anom = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "ConeScan",
            &[PyRep::Real(360.0), PyRep::Real(50_000.0)],
            &kw,
            None,
        )
        .await
        .expect("scanSvc.ConeScan at ore anomaly");
    let PyRep::List(dscan_items) = dscan_anom else {
        panic!("Expected List from ConeScan");
    };
    assert!(anom_ast_ids.iter().all(|id| dscan_items
        .iter()
        .any(|it| it.dict_get("itemID").and_then(|v| v.as_i64()) == Some(*id))));

    // One real Miner I cycle on the first asteroid extracts ore and leaves the rock with less ore.
    let first_ast = anom_ast_ids[0];
    let before_units = ctx.db.with_snapshot(|snap| snap.asteroids[&first_ast].quantity);
    let first_cycle = activate_in_range(
        &ctx, &mut sess, &handle, dogma_oid, beyonce_oid, miner_mod_id, first_ast,
    )
    .await;
    let mined = first_cycle.units_removed;
    assert!(mined > 0, "Miner I cycle must extract ore: {first_cycle:?}");
    assert_eq!(first_cycle.stop, None);
    let after_units = ctx.db.with_snapshot(|snap| snap.asteroids[&first_ast].quantity);
    assert_eq!(after_units, before_units - mined - first_cycle.residue_units);

    ctx.db.with_mut_snapshot(|snap| {
        for id in &anom_ast_ids {
            snap.asteroids.get_mut(id).unwrap().quantity = 1;
        }
    });
    for (i, id) in anom_ast_ids.iter().enumerate() {
        let res = activate_in_range(
            &ctx, &mut sess, &handle, dogma_oid, beyonce_oid, miner_mod_id, *id,
        )
        .await;
        assert_eq!(res.units_removed, 1, "rock {i}");
        assert_eq!(res.stop, Some(eve_server::services::mining_cycle::MiningStopReason::AsteroidDepleted), "rock {i}");
        let still_listed = ctx.db.with_snapshot(|snap| {
            snap.cosmic_anomalies[&sys_id]
                .iter()
                .any(|a| a.anomaly_id == ore_anom_id && !a.cleared)
        });
        assert_eq!(
            still_listed,
            i + 1 < anom_ast_ids.len(),
            "The anomaly clears exactly when its last rock is emptied (rock {i})"
        );
    }

    // scanSvc.GetScanResults no longer lists the cleared anomaly
    let post_mine_scan = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "GetScanResults",
            &[],
            &kw,
            None,
        )
        .await
        .expect("scanSvc.GetScanResults after depleting anomaly");
    let PyRep::List(remaining_anoms) = post_mine_scan.dict_get("anomalies").unwrap() else {
        panic!("Expected anomalies list");
    };
    assert!(
        !remaining_anoms
            .iter()
            .any(|a| a.dict_get("anomalyID").and_then(|v| v.as_str()) == Some(&ore_anom_id)),
        "Depleted ore anomaly must despawn from scanSvc.GetScanResults"
    );

    // ------------------------------------------------------------------------
    // 2. Hidden ore signature via 2-pass probe scanning (feat:mining.ore-signatures)
    // ------------------------------------------------------------------------
    let mut wide_probes = Vec::new();
    for &sx in &[-1.0_f64, 1.0] {
        for &sy in &[-1.0_f64, 1.0] {
            for &sz in &[-1.0_f64, 1.0] {
                wide_probes.push(PyRep::Tuple(vec![
                    PyRep::Real(sx * 6.0 * au),
                    PyRep::Real(sy * 4.0 * au),
                    PyRep::Real(sz * 6.0 * au),
                    PyRep::Real(12.0 * au),
                ]));
            }
        }
    }
    let pass1_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "RequestScans",
            &[PyRep::List(wide_probes)],
            &kw,
            None,
        )
        .await
        .expect("scanSvc.RequestScans wide pass 1");
    let PyRep::Rowset {
        rows: pass1_rows, ..
    } = pass1_rep
    else {
        panic!("Expected Rowset from scanSvc.RequestScans");
    };
    let ore_pass1 = pass1_rows
        .iter()
        .find(|r| r[2].as_str() == Some("Ore"))
        .expect("Wide scan must detect Ore signature group");
    let ore_sig_id = ore_pass1[1].as_str().unwrap().to_string();
    let ore_x = ore_pass1[6].as_f64().expect("Rough X revealed at >= 0.25");
    let ore_y = ore_pass1[7].as_f64().expect("Rough Y revealed at >= 0.25");
    let ore_z = ore_pass1[8].as_f64().expect("Rough Z revealed at >= 0.25");

    let pre_pinpoint = ctx
        .dispatch_rpc(&mut sess, "scanSvc", None, "GetScanResults", &[], &kw, None)
        .await
        .expect("scanSvc.GetScanResults before pinpoint");
    let PyRep::List(pre_sigs) = pre_pinpoint.dict_get("signatures").unwrap() else {
        panic!("Expected signatures list");
    };
    let pre_sig = pre_sigs
        .iter()
        .find(|s| s.dict_get("signatureID").and_then(|v| v.as_str()) == Some(&ore_sig_id))
        .unwrap();
    assert_eq!(pre_sig.dict_get("warpable").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(pre_sig.dict_get("oreTypes"), Some(&PyRep::List(vec![])));

    let mut pinpoint_ore_probes = Vec::new();
    for &sx in &[-1.0_f64, 1.0] {
        for &sy in &[-1.0_f64, 1.0] {
            for &sz in &[-1.0_f64, 1.0] {
                pinpoint_ore_probes.push(PyRep::Tuple(vec![
                    PyRep::Real(ore_x + sx * 2_000.0),
                    PyRep::Real(ore_y + sy * 2_000.0),
                    PyRep::Real(ore_z + sz * 2_000.0),
                    PyRep::Real(0.5 * au),
                ]));
            }
        }
    }
    let pass2_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "RequestScans",
            &[PyRep::List(pinpoint_ore_probes)],
            &kw,
            None,
        )
        .await
        .expect("scanSvc.RequestScans pinpoint pass 2");
    let PyRep::Rowset {
        rows: pass2_rows, ..
    } = pass2_rep
    else {
        panic!("Expected Rowset from scanSvc.RequestScans pass 2");
    };
    let ore_pass2 = pass2_rows
        .iter()
        .find(|r| r[1].as_str() == Some(&ore_sig_id))
        .unwrap();
    assert_eq!(ore_pass2[5].as_f64(), Some(1.0), "Pinpoint scan resolves the signature to 100%");
    assert_eq!(ore_pass2[3].as_str(), Some("Ore"));
    assert_eq!(ore_pass2[4].as_str(), Some("Hidden Omber Deposit"));

    let sig_results = ctx
        .dispatch_rpc(&mut sess, "scanSvc", None, "GetScanResults", &[], &kw, None)
        .await
        .expect("scanSvc.GetScanResults");
    let PyRep::List(sigs_list) = sig_results.dict_get("signatures").unwrap() else {
        panic!("Expected signatures list");
    };
    let resolved_ore_sig = sigs_list
        .iter()
        .find(|s| s.dict_get("signatureID").and_then(|v| v.as_str()) == Some(&ore_sig_id))
        .unwrap();
    assert_eq!(resolved_ore_sig.dict_get("warpable").and_then(|v| v.as_bool()), Some(true));
    let PyRep::List(sig_types) = resolved_ore_sig.dict_get("oreTypes").unwrap() else {
        panic!("Expected oreTypes list");
    };
    let count_of = |tid: i64| sig_types.iter().filter(|v| v.as_i64() == Some(tid)).count();
    assert_eq!((count_of(17867), count_of(17868), count_of(46684)), (6, 6, 10));
    assert_eq!(sig_types.len(), 22);
    let PyRep::List(ore_ball_ids) = resolved_ore_sig.dict_get("siteBallIDs").unwrap() else {
        panic!("Expected siteBallIDs");
    };
    let sig_rock = ore_ball_ids[0].as_i64().unwrap();

    // Warp to the probed signature and mine one of its rocks
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_oid),
        "CmdWarpToStuff",
        &[
            PyRep::String("scan".into()),
            PyRep::String(ore_sig_id.clone()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Warp to probed Ore signature");
    for _ in 0..40 {
        handle.step_manual(1.0).await.unwrap();
        let (_, b) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        if matches!(b.mode, MovementMode::Stop) {
            break;
        }
    }
    let sig_cycle = activate_in_range(
        &ctx, &mut sess, &handle, dogma_oid, beyonce_oid, miner_mod_id, sig_rock,
    )
    .await;
    assert!(sig_cycle.units_removed > 0);

    // ------------------------------------------------------------------------
    // 3. Low-sec gas site with gas harvesting (feat:mining.gas-sites)
    // ------------------------------------------------------------------------
    let lowsec_sys_id = 30_002_537_u32; // Amamake (low-sec 0.4)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = lowsec_sys_id;
        }
        if let Some(ship_it) = snap.items.get_mut(&ship_id) {
            ship_it.location_id = lowsec_sys_id as i64;
        }
    });
    sess.solar_system_id = Some(lowsec_sys_id);

    let lowsec_handle = ctx.systems.get_or_spawn(lowsec_sys_id);
    lowsec_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 32880,
            position: DVec3::new(100_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 335.0,
            inertia: 2.8,
            mass: 1_200_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    let lowsec_beyonce_oid = bind_service_oid(
        &ctx,
        &mut sess,
        "beyonce",
        PyRep::Integer(lowsec_sys_id as i64),
    )
    .await;
    let _lowsec_dogma_oid = bind_service_oid(
        &ctx,
        &mut sess,
        "dogmaIM",
        PyRep::Tuple(vec![
            PyRep::Integer(lowsec_sys_id as i64),
            PyRep::Integer(5),
        ]),
    )
    .await;

    let (ls_anoms, ls_sigs) = ctx.db.with_snapshot(|snap| {
        (
            snap.cosmic_anomalies[&lowsec_sys_id]
                .iter()
                .map(|a| a.name.clone())
                .collect::<Vec<_>>(),
            snap.cosmic_signatures[&lowsec_sys_id]
                .iter()
                .filter(|s| s.sig_group != "OreAnomaly")
                .map(|s| (s.sig_group.clone(), s.type_name.clone()))
                .collect::<Vec<_>>(),
        )
    });
    assert_eq!(ls_anoms, vec!["Average Hedbergite, Hemorphite and Jaspet Deposit".to_string()]);
    assert_eq!(
        ls_sigs,
        vec![
            ("Ore".to_string(), "Hidden Crokite, Dark Ochre and Gneiss Deposit".to_string()),
            ("Gas".to_string(), "Saintly Nebula".to_string()),
        ]
    );

    let mut ls_wide_probes = Vec::new();
    for &sx in &[-1.0_f64, 1.0] {
        for &sy in &[-1.0_f64, 1.0] {
            for &sz in &[-1.0_f64, 1.0] {
                ls_wide_probes.push(PyRep::Tuple(vec![
                    PyRep::Real(sx * 6.0 * au),
                    PyRep::Real(sy * 4.0 * au),
                    PyRep::Real(sz * 6.0 * au),
                    PyRep::Real(12.0 * au),
                ]));
            }
        }
    }
    let ls_pass1 = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "RequestScans",
            &[PyRep::List(ls_wide_probes)],
            &kw,
            None,
        )
        .await
        .expect("Low-sec wide scan pass 1");
    let PyRep::Rowset {
        rows: ls_p1_rows, ..
    } = ls_pass1
    else {
        panic!("Expected Rowset");
    };
    let gas_p1 = ls_p1_rows
        .iter()
        .find(|r| r[2].as_str() == Some("Gas"))
        .expect("Wide scan must detect Gas signature");
    let gas_sig_id = gas_p1[1].as_str().unwrap().to_string();
    let gas_x = gas_p1[6].as_f64().unwrap();
    let gas_y = gas_p1[7].as_f64().unwrap();
    let gas_z = gas_p1[8].as_f64().unwrap();

    let mut pinpoint_gas_probes = Vec::new();
    for &sx in &[-1.0_f64, 1.0] {
        for &sy in &[-1.0_f64, 1.0] {
            for &sz in &[-1.0_f64, 1.0] {
                pinpoint_gas_probes.push(PyRep::Tuple(vec![
                    PyRep::Real(gas_x + sx * 2_000.0),
                    PyRep::Real(gas_y + sy * 2_000.0),
                    PyRep::Real(gas_z + sz * 2_000.0),
                    PyRep::Real(0.5 * au),
                ]));
            }
        }
    }
    let ls_pass2 = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "RequestScans",
            &[PyRep::List(pinpoint_gas_probes)],
            &kw,
            None,
        )
        .await
        .expect("Low-sec pinpoint scan pass 2");
    let PyRep::Rowset {
        rows: ls_p2_rows, ..
    } = ls_pass2
    else {
        panic!("Expected Rowset");
    };
    let gas_p2 = ls_p2_rows
        .iter()
        .find(|r| r[1].as_str() == Some(&gas_sig_id))
        .unwrap();
    assert_eq!(gas_p2[5].as_f64(), Some(1.0), "Gas site signature must resolve to 100%");
    assert_eq!(gas_p2[4].as_str(), Some("Saintly Nebula"));

    let pre_gas_scan = ctx
        .dispatch_rpc(&mut sess, "scanSvc", None, "GetScanResults", &[], &kw, None)
        .await
        .expect("scanSvc.GetScanResults before gas harvesting");
    let PyRep::List(ls_sigs_rep) = pre_gas_scan.dict_get("signatures").unwrap() else {
        panic!("Expected signatures list");
    };
    let resolved_gas_sig = ls_sigs_rep
        .iter()
        .find(|s| s.dict_get("signatureID").and_then(|v| v.as_str()) == Some(&gas_sig_id))
        .unwrap();
    assert_eq!(
        resolved_gas_sig.dict_get("oreTypes"),
        Some(&PyRep::List(vec![PyRep::Integer(25278), PyRep::Integer(25278)])),
        "Two Vermillion Cytoserocin clouds"
    );
    let PyRep::List(gas_ball_ids) = resolved_gas_sig.dict_get("siteBallIDs").unwrap() else {
        panic!("Expected siteBallIDs for gas site");
    };
    let gas_cloud_ids: Vec<i64> = gas_ball_ids.iter().map(|v| v.as_i64().unwrap()).collect();
    let mut cloud_units: Vec<i64> = ctx.db.with_snapshot(|snap| {
        gas_cloud_ids.iter().map(|id| snap.asteroids[id].quantity).collect()
    });
    cloud_units.sort_unstable();
    assert_eq!(cloud_units, vec![200, 300]);
    let gas_cloud_ball_id = gas_cloud_ids[0];

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(lowsec_beyonce_oid),
        "CmdWarpToStuff",
        &[
            PyRep::String("scan".into()),
            PyRep::String(gas_sig_id.clone()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Warp to probed Gas site");
    for _ in 0..40 {
        lowsec_handle.step_manual(1.0).await.unwrap();
        let (_, b) = lowsec_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        if matches!(b.mode, MovementMode::Stop) {
            break;
        }
    }
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(lowsec_beyonce_oid),
        "CmdFollowBall",
        &[PyRep::Integer(gas_cloud_ball_id), PyRep::Real(500.0)],
        &kw,
        None,
    )
    .await
    .expect("Approach gas cloud within 1500m harvester range");
    for _ in 0..15 {
        lowsec_handle.step_manual(1.0).await.unwrap();
    }

    let gas_before = ctx.db.with_snapshot(|snap| snap.asteroids[&gas_cloud_ball_id].quantity);
    let harvest_res = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, gas_mod_id, gas_cloud_ball_id)
        .await
        .expect("Gas Cloud Harvester I cycle on gas cloud");
    let harvested = harvest_res.units_removed;
    assert!(harvested > 0, "Gas Cloud Harvester must extract Vermillion Cytoserocin units");
    assert_eq!(harvest_res.ore_type_id, 25278);
    let gas_after = ctx.db.with_snapshot(|snap| snap.asteroids[&gas_cloud_ball_id].quantity);
    assert_eq!(gas_after, gas_before - harvested - harvest_res.residue_units);

    let post_gas_scan = ctx
        .dispatch_rpc(&mut sess, "scanSvc", None, "GetScanResults", &[], &kw, None)
        .await
        .expect("scanSvc.GetScanResults after gas harvesting");
    assert_eq!(
        post_gas_scan.dict_get("gasSiteNpcSpawned").and_then(|v| v.as_bool()),
        Some(false)
    );

    // 3b. W-space Fullerite gas site (`Barren Perimeter Reservoir` in 31_000_005) with delayed Sleeper NPC spawn (900s)
    // and ore anomaly 3-day expiry despawn (`feat:mining.gas-sites`, `feat:mining.ore-anomalies`):
    ctx.advance_time_seconds(mining_sites::ORE_ANOMALY_EXPIRY_SECS);
    let expired_ls_scan = ctx
        .dispatch_rpc(&mut sess, "scanSvc", None, "GetScanResults", &[], &kw, None)
        .await
        .expect("scanSvc.GetScanResults after 3d ore anomaly expiry");
    let PyRep::List(expired_ls_anoms) = expired_ls_scan.dict_get("anomalies").unwrap() else { panic!() };
    assert!(
        expired_ls_anoms.is_empty(),
        "Unmined ore anomaly must expire and despawn from scanSvc.GetScanResults after ORE_ANOMALY_EXPIRY_SECS (3d)"
    );

    let wh_sys_id = 31_000_007_u32; // Wormhole system
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = wh_sys_id;
        }
        if let Some(ship_it) = snap.items.get_mut(&ship_id) {
            ship_it.location_id = wh_sys_id as i64;
        }
    });
    sess.solar_system_id = Some(wh_sys_id);
    let wh_handle = ctx.systems.get_or_spawn(wh_sys_id);
    wh_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 32880,
            position: DVec3::new(100_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 335.0,
            inertia: 2.8,
            mass: 1_200_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    let mut wh_wide_probes = Vec::new();
    for &sx in &[-1.0_f64, 1.0] {
        for &sy in &[-1.0_f64, 1.0] {
            for &sz in &[-1.0_f64, 1.0] {
                wh_wide_probes.push(PyRep::Tuple(vec![
                    PyRep::Real(sx * 6.0 * au),
                    PyRep::Real(sy * 4.0 * au),
                    PyRep::Real(sz * 6.0 * au),
                    PyRep::Real(12.0 * au),
                ]));
            }
        }
    }
    let PyRep::Rowset { rows: wh_p1_rows, .. } = ctx
        .dispatch_rpc(&mut sess, "scanSvc", None, "RequestScans", &[PyRep::List(wh_wide_probes)], &kw, None)
        .await
        .expect("W-space wide scan")
    else {
        panic!();
    };
    let wh_gas_p1 = wh_p1_rows.iter().find(|r| r[2].as_str() == Some("Gas")).expect("W-space Gas signature");
    let wh_gas_sig_id = wh_gas_p1[1].as_str().unwrap().to_string();
    let (wx, wy, wz) = (wh_gas_p1[6].as_f64().unwrap(), wh_gas_p1[7].as_f64().unwrap(), wh_gas_p1[8].as_f64().unwrap());
    let mut wh_pinpoint = Vec::new();
    for &sx in &[-1.0_f64, 1.0] {
        for &sy in &[-1.0_f64, 1.0] {
            for &sz in &[-1.0_f64, 1.0] {
                wh_pinpoint.push(PyRep::Tuple(vec![
                    PyRep::Real(wx + sx * 2_000.0),
                    PyRep::Real(wy + sy * 2_000.0),
                    PyRep::Real(wz + sz * 2_000.0),
                    PyRep::Real(0.5 * au),
                ]));
            }
        }
    }
    ctx.dispatch_rpc(&mut sess, "scanSvc", None, "RequestScans", &[PyRep::List(wh_pinpoint)], &kw, None)
        .await
        .expect("W-space pinpoint scan");
    let wh_pre_delay = ctx
        .dispatch_rpc(&mut sess, "scanSvc", None, "GetScanResults", &[], &kw, None)
        .await
        .expect("W-space GetScanResults before 900s Sleeper delay");
    assert_eq!(
        wh_pre_delay.dict_get("gasSiteNpcSpawned").and_then(|v| v.as_bool()),
        Some(false),
        "Sleeper NPC guards must not spawn before the 900s (15 min) delay elapses"
    );
    ctx.advance_time_seconds(900);
    let wh_post_delay = ctx
        .dispatch_rpc(&mut sess, "scanSvc", None, "GetScanResults", &[], &kw, None)
        .await
        .expect("W-space GetScanResults after 900s Sleeper delay");
    assert_eq!(
        wh_post_delay.dict_get("gasSiteNpcSpawned").and_then(|v| v.as_bool()),
        Some(true),
        "Guarded W-space gas reservoir must spawn Sleeper NPCs (Emergent Patroller 30209) after 900s delay"
    );
    assert!(wh_post_delay.dict_get("gasSiteNpcBallID").and_then(|v| v.as_i64()).is_some());
    let _ = wh_gas_sig_id;

    // ------------------------------------------------------------------------
    // 4. Sov Hub Prospecting Upgrades & Sov Ore Sites via V24 sovMgr / sovHubSvc (feat:mining.sov-ore-sites)
    // ------------------------------------------------------------------------
    let sov_sys_id = 30_004_759_u32; // Delve null-sec system (1DQ1-A)
    let player_corp_id = 98_000_055_i64;
    let player_alliance_id = 99_000_055_i64;
    let sov_struct_id = 1_045_000_000_055_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            player_corp_id,
            eve_db::CorporationRecord {
                corporation_id: player_corp_id,
                name: "Sov Mining Corp".into(),
                ticker: "SOVMN".into(),
                ceo_id: char_id,
                alliance_id: Some(player_alliance_id),
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(100_000_000),
                wallet_divisions: Vec::new(),
                description: String::new(),
                url: String::new(),
                hq_station_id: Some(60_003_760),
                members: [char_id].into_iter().collect(),
                member_roles: [(char_id, u64::MAX)].into_iter().collect(),
                applications: Default::default(),
            },
        );
        snap.alliances.insert(
            player_alliance_id,
            eve_db::AllianceRecord {
                alliance_id: player_alliance_id,
                name: "Sov Mining Alliance".into(),
                short_name: "SVMA".into(),
                executor_corp_id: player_corp_id,
                member_corp_ids: [player_corp_id].into_iter().collect(),
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = player_corp_id;
            ch.alliance_id = Some(player_alliance_id);
            ch.solar_system_id = sov_sys_id;
        }
        snap.structures.insert(
            sov_struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: sov_struct_id,
                type_id: 35835,
                owner_corp_id: player_corp_id,
                solar_system_id: sov_sys_id,
                name: "Delve Sov Hub Citadel".into(),
                position: [100_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        snap.system_states
            .entry(sov_sys_id)
            .or_insert_with(|| eve_db::SystemDynamicState {
                solar_system_id: sov_sys_id,
                dbs_multiplier: 1.0,
                ess_main_bank: IskCents(0),
                ess_reserve_bank: IskCents(0),
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                sov_alliance_id: None,
                military_index: 1,
                industrial_index: 1,
                strategic_index: 1,
                fw_contested_pct: 0.0,
                insurgency_corruption_stage: 0,
                insurgency_suppression_stage: 0,
                wormhole_remaining_mass_kg: 0.0,
                wormhole_destination_system_id: None,
                signature_position: None,
                fw_complex_position: None,
                fw_capture_seconds_elapsed: 0,
                fw_capture_duration_seconds: 600,
                last_fw_capture_tick: 0,
                last_sov_claim_tick: 0,
            });
    });
    sess.corporation_id = Some(player_corp_id);
    sess.alliance_id = Some(player_alliance_id);
    sess.solar_system_id = Some(sov_sys_id);

    let sov_handle = ctx.systems.get_or_spawn(sov_sys_id);
    sov_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 32880,
            position: DVec3::new(100_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 335.0,
            inertia: 2.8,
            mass: 1_200_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    sov_handle.step_manual(1.0).await.unwrap();

    let unclaim_err = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "SaveUpgradeConfiguration",
            &[PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect_err("SaveUpgradeConfiguration before sovereignty claim must fail");
    assert_eq!(unclaim_err.code, "SovereigntyNotClaimed");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.system_states.get_mut(&sov_sys_id) {
            st.sov_alliance_id = Some(player_alliance_id);
        }
    });

    let claim_res = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "AcquireSkyhooks",
            &[],
            &kw,
            None,
        )
        .await
        .expect("sovMgr.AcquireSkyhooks");
    assert_eq!(
        claim_res.dict_get("allianceID").and_then(|v| v.as_i64()),
        Some(player_alliance_id)
    );

    let missing_up_err = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "SaveUpgradeConfiguration",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("SaveUpgradeConfiguration without upgrade item must fail");
    assert_eq!(missing_up_err.code, "UpgradeItemNotFound");

    let spoof_bind_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(30_002_187), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect_err("MachoBindObject(beyonce, foreign_sys_id) must reject");
    assert_eq!(spoof_bind_err.code, "BindAuthorizationDenied");
    let remote_seeded = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&30_002_187)
            .is_some_and(|list| !list.is_empty())
            || snap
                .cosmic_signatures
                .get(&30_002_187)
                .is_some_and(|list| !list.is_empty())
    });
    assert!(
        !remote_seeded,
        "MachoBindObject(beyonce, foreign_sys_id) must not seed mining sites in a remote system"
    );

    let other_struct_id = 1_045_000_000_056_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            720_000_053,
            InventoryItemRecord {
                item_id: 720_000_053,
                type_id: 2042, // Deprecated Ore Prospecting Array 3 (group 1020)
                owner_id: char_id,
                location_id: sov_struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            720_000_054,
            InventoryItemRecord {
                item_id: 720_000_054,
                type_id: 87702,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut other = snap.structures[&sov_struct_id].clone();
        other.structure_id = other_struct_id;
        other.name = "Delve Second Citadel".into();
        snap.structures.insert(other_struct_id, other);
        snap.items.insert(
            720_000_056,
            InventoryItemRecord {
                item_id: 720_000_056,
                type_id: 87702,
                owner_id: char_id,
                location_id: other_struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let remote_up_err = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "SaveUpgradeConfiguration",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("Deprecated, remote and other-structure upgrade items must all be rejected");
    assert_eq!(remote_up_err.code, "UpgradeItemNotFound");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.remove(&other_struct_id);
        snap.items.remove(&720_000_056);
    });

    // A prospecting upgrade without a modelled deposit (Tritanium Prospecting Array 1, 82579) is refused
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            720_000_057,
            InventoryItemRecord {
                item_id: 720_000_057,
                type_id: 82579,
                owner_id: char_id,
                location_id: sov_struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let unmodelled_err = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "SaveUpgradeConfiguration",
            &[PyRep::List(vec![PyRep::Integer(82579)])],
            &kw,
            None,
        )
        .await
        .expect_err("Unmodelled prospecting upgrade must be refused");
    assert_eq!(unmodelled_err.code, "ProspectingDepositNotModelled");
    assert_eq!(
        ctx.db.with_snapshot(|snap| snap.items[&720_000_057].flag),
        InventoryFlag::Hangar
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&720_000_057);
    });

    // Tritanium Prospecting Array 3 (87702) locked in the hub -> UpgradeItemLocked (`R72-P1-2`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            720_000_055,
            InventoryItemRecord {
                item_id: 720_000_055,
                type_id: 87702, // Tritanium Prospecting Array 3
                owner_id: char_id,
                location_id: sov_struct_id,
                flag: InventoryFlag::Locked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let locked_up_err = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "SaveUpgradeConfiguration",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("SaveUpgradeConfiguration with locked upgrade item must fail");
    assert_eq!(locked_up_err.code, "UpgradeItemLocked");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&720_000_055) {
            it.flag = InventoryFlag::Hangar;
        }
    });

    // Install Tritanium Prospecting Array 3 -> Large Veldspar Deposit
    let upgrade_res = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "SaveUpgradeConfiguration",
            &[PyRep::List(vec![PyRep::Integer(87702)])],
            &kw,
            None,
        )
        .await
        .expect("sovHubSvc.SaveUpgradeConfiguration");
    let PyRep::KeyVal(up_kv) = upgrade_res else {
        panic!("Expected KeyVal from SaveUpgradeConfiguration");
    };
    assert_eq!(up_kv["solarSystemID"].as_i64(), Some(sov_sys_id as i64));
    assert_eq!(up_kv["prospectingLevel"].as_i64(), Some(3));
    assert_eq!(up_kv["upgradeTypeID"].as_i64(), Some(87702));
    assert_eq!(up_kv["online"], PyRep::Bool(true));
    let installed_item = ctx.db.with_snapshot(|snap| snap.items[&720_000_055].clone());
    assert_eq!(installed_item.location_id, sov_struct_id);
    assert_eq!(installed_item.flag, InventoryFlag::SubSystemSlot0);

    let sov_sites_res = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "GetInstalledUpgradesLocal",
            &[],
            &kw,
            None,
        )
        .await
        .expect("sovHubSvc.GetInstalledUpgradesLocal");
    let PyRep::List(installed) = sov_sites_res.dict_get("upgrades").unwrap() else {
        panic!("Expected upgrades list");
    };
    assert_eq!(installed.len(), 1);
    assert_eq!(installed[0].dict_get("typeID").and_then(|v| v.as_i64()), Some(87702));
    let PyRep::List(sov_anoms) = sov_sites_res.dict_get("anomalies").unwrap() else {
        panic!("Expected anomalies list from GetInstalledUpgradesLocal");
    };
    assert_eq!(sov_anoms.len(), 1);
    assert_eq!(
        sov_anoms[0].dict_get("siteName").and_then(|v| v.as_str()),
        Some("Large Veldspar Deposit")
    );

    let sov_anom_id = sov_anoms[0]
        .dict_get("anomalyID")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    let sov_pos = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies[&sov_sys_id]
            .iter()
            .find(|a| a.anomaly_id == sov_anom_id)
            .map(|a| DVec3::from_array(a.position))
            .unwrap()
    });
    let per_group = ctx.db.with_snapshot(|snap| {
        let mut m: HashMap<u32, (u32, i64)> = HashMap::new();
        for a in snap.asteroids.values().filter(|a| {
            a.solar_system_id == sov_sys_id
                && DVec3::new(a.position.0, a.position.1, a.position.2).distance(sov_pos) <= 50_000.0
        }) {
            let gid = ctx.sde.types[&a.type_id].group_id;
            let e = m.entry(gid).or_default();
            e.0 += 1;
            e.1 += a.quantity;
        }
        m
    });
    let expected_groups: HashMap<u32, (u32, i64)> = [
        (462, (31, 114_769_310)),
        (460, (6, 14_166_663)),
        (458, (4, 4_614_271)),
    ]
    .into_iter()
    .collect();
    assert_eq!(per_group, expected_groups);

    let sov_beyonce_oid = bind_service_oid(
        &ctx,
        &mut sess,
        "beyonce",
        PyRep::Integer(sov_sys_id as i64),
    )
    .await;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(sov_beyonce_oid),
        "CmdWarpToStuff",
        &[
            PyRep::String("scan".into()),
            PyRep::String(sov_anom_id.clone()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Warp to Sov Ore Anomaly");
    for _ in 0..40 {
        sov_handle.step_manual(1.0).await.unwrap();
        let (_, b) = sov_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        if matches!(b.mode, MovementMode::Stop) {
            break;
        }
    }
    let dscan_sov = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "ConeScan",
            &[PyRep::Real(360.0), PyRep::Real(50_000.0)],
            &kw,
            None,
        )
        .await
        .expect("scanSvc.ConeScan at Sov Ore Anomaly");
    let PyRep::List(sov_grid_items) = dscan_sov else {
        panic!("Expected List from ConeScan");
    };
    let veld_ball_id = sov_grid_items
        .iter()
        .find(|it| it.dict_get("typeID").and_then(|v| v.as_i64()) == Some(1230))
        .and_then(|it| it.dict_get("itemID").and_then(|v| v.as_i64()))
        .expect("Veldspar (1230) asteroid ball must be on grid at the Large Veldspar Deposit");

    let veld_units = ctx.db.with_snapshot(|snap| snap.asteroids[&veld_ball_id].quantity) as u64;
    sov_handle
        .mine_asteroid(veld_ball_id, veld_units, 0.0)
        .await
        .unwrap();
    let repeat = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "SaveUpgradeConfiguration",
            &[],
            &kw,
            None,
        )
        .await
        .expect("Idempotent SaveUpgradeConfiguration");
    assert_eq!(
        repeat.dict_get("spawnedAnomalies"),
        Some(&PyRep::List(vec![PyRep::String(sov_anom_id.clone())]))
    );
    let veld_after = sov_handle
        .get_asteroid_state(veld_ball_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        veld_after.depleted && veld_after.remaining_units == 0,
        "Repeat SaveUpgradeConfiguration must not resurrect depleted SovOre asteroids"
    );
}
