//! Mining ledger (`MN-03`, `MN-16`) and Track 1 regression tests (split from `space_mining.rs`).

#![forbid(unsafe_code)]

use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_mn03_mining_ledger_recording_and_char_mining_ledger_service() {
    // parity: feat:mining.character-ledger, rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID, rpc:characterMiningLedger.GetCharacterLogs
    eve_testkit::attest(&["feat:mining.character-ledger"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(963);

    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let undock = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(undock_kv) = &undock else { panic!("Expected KeyVal") };
    let beyonce_id = undock_kv["beyonceBoundID"].as_i64().unwrap() as u64;
    let ship_id = undock_kv["shipID"].as_i64().unwrap();

    let jita = ctx.systems.get_or_spawn(30_000_142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // Spawn and mine Scordite asteroid (type 1228)
    let scordite_ast_id = 750_000_228i64;
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: scordite_ast_id,
            belt_id: 40_000_001,
            type_id: 1228,
            initial_units: 1_000,
            remaining_units: 1_000,
            unit_volume: 0.15,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: scordite_ast_id,
            type_id: 1228,
            position: ship_ball.position + DVec3::new(1_200.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e10,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();

    let mine_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdMineRepeatedly",
            &[PyRep::Integer(scordite_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdMine");
    let PyRep::KeyVal(mine_kv) = &mine_res else { panic!("Expected KeyVal") };
    let mined_units = mine_kv["minedUnits"].as_i64().unwrap();

    // Execute a second mining cycle on the same asteroid on Day 1 so we can verify per-day/per-type/per-system aggregation
    let mine_res_2 = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdMineRepeatedly",
            &[PyRep::Integer(scordite_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdMine #2");
    let PyRep::KeyVal(mine_kv_2) = &mine_res_2 else { panic!("Expected KeyVal") };
    let mined_units_2 = mine_kv_2["minedUnits"].as_i64().unwrap();
    let expected_day1_scordite = mined_units + mined_units_2;

    // Advance virtual clock by 1 day (Day 2) and mine Scordite ore (1228), Clear Icicle ice (16262), and Fullerite-C50 gas (30370)
    ctx.advance_time_seconds(86_400);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 654; // Kryos (Specialized Ice Hold 30,000 m3)
        }
    });
    let ice_ast_id = 750_000_229i64;
    let gas_ast_id = 750_000_230i64;
    for (aid, tid, units, uvol) in [
        (ice_ast_id, 16262u32, 50u64, 1_000.0_f64),
        (gas_ast_id, 30370u32, 500u64, 1.0_f64),
    ] {
        jita.spawn_asteroid(
            AsteroidState {
                asteroid_id: aid,
                belt_id: 40_000_001,
                type_id: tid,
                initial_units: units,
                remaining_units: units,
                unit_volume: uvol,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: aid,
                type_id: tid,
                position: ship_ball.position + DVec3::new(1_100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e10,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await
        .unwrap();
    }
    let day2_ore_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdMineRepeatedly",
            &[PyRep::Integer(scordite_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Day 2 ore mine");
    let day2_scordite = day2_ore_res.dict_get("minedUnits").unwrap().as_i64().unwrap();
    let day2_ice_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdMineRepeatedly",
            &[PyRep::Integer(ice_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Day 2 ice mine");
    let day2_ice = day2_ice_res.dict_get("minedUnits").unwrap().as_i64().unwrap();
    let day2_gas_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdMineRepeatedly",
            &[PyRep::Integer(gas_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("Day 2 gas mine");
    let day2_gas = day2_gas_res.dict_get("minedUnits").unwrap().as_i64().unwrap();

    // Query mining ledger via canonical V24 RPC service characterMiningLedger.GetCharacterLogs
    let ledger_res = ctx
        .dispatch_rpc(
            &mut sess,
            "characterMiningLedger",
            None,
            "GetCharacterLogs",
            &[],
            &kw,
            None,
        )
        .await
        .expect("characterMiningLedger.GetCharacterLogs");
    let PyRep::List(entries) = &ledger_res else { panic!("Expected List of ledger entries") };
    assert!(
        entries.len() >= 4,
        "Mining ledger must contain Day 1 Scordite + Day 2 Scordite + Day 2 Ice + Day 2 Gas"
    );

    let scordite_rows: Vec<&HashMap<String, PyRep>> = entries
        .iter()
        .filter_map(|e| {
            if let PyRep::KeyVal(kv) = e {
                if kv.get("typeID").and_then(|v| v.as_i64()) == Some(1228)
                    && kv.get("solarSystemID").and_then(|v| v.as_i64()) == Some(30_000_142)
                {
                    return Some(kv);
                }
            }
            None
        })
        .collect();
    assert_eq!(
        scordite_rows.len(),
        2,
        "Day 1 (2 cycles aggregated) and Day 2 Scordite must appear as 2 distinct daily rows"
    );
    assert_eq!(
        scordite_rows[0].get("quantity").and_then(|v| v.as_i64()),
        Some(expected_day1_scordite),
        "Day 1 aggregated ledger row must sum both mining cycles ({mined_units} + {mined_units_2} = {expected_day1_scordite})"
    );
    assert_eq!(
        scordite_rows[1].get("quantity").and_then(|v| v.as_i64()),
        Some(day2_scordite)
    );
    assert!(entries.iter().any(|e| {
        e.dict_get("typeID").and_then(|v| v.as_i64()) == Some(16262)
            && e.dict_get("groupID").and_then(|v| v.as_i64()) == Some(465)
            && e.dict_get("quantity").and_then(|v| v.as_i64()) == Some(day2_ice)
    }), "Character mining ledger must record harvested Ice (16262)");
    assert!(entries.iter().any(|e| {
        e.dict_get("typeID").and_then(|v| v.as_i64()) == Some(30370)
            && e.dict_get("groupID").and_then(|v| v.as_i64()) == Some(711)
            && e.dict_get("quantity").and_then(|v| v.as_i64()) == Some(day2_gas)
    }), "Character mining ledger must record harvested Gas (30370)");

    // Advance virtual clock by another 90 days (so Day 1 is now 91 days old and outside the 90-day rolling window, while Day 2 is 90 days old)
    ctx.advance_time_seconds(90 * 86_400 + 60);
    let ledger_after_91d = ctx
        .dispatch_rpc(
            &mut sess,
            "characterMiningLedger",
            None,
            "GetCharacterLogs",
            &[],
            &kw,
            None,
        )
        .await
        .expect("characterMiningLedger.GetCharacterLogs after 91d");
    let PyRep::List(entries_after) = &ledger_after_91d else { panic!("Expected List") };
    let has_day1_entry = entries_after.iter().any(|e| {
        e.dict_get("typeID").and_then(|v| v.as_i64()) == Some(1228)
            && e.dict_get("quantity").and_then(|v| v.as_i64()) == Some(expected_day1_scordite)
    });
    assert!(
        !has_day1_entry,
        "Mining ledger entries older than 90 days must age out of characterMiningLedger.GetCharacterLogs"
    );
}

#[tokio::test]
async fn test_r3_track1_mining_pve_fw_regressions() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(964);

    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
    let char_id = 90_000_001_i64;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let undock = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let PyRep::KeyVal(undock_kv) = &undock else { panic!("Expected KeyVal") };
    let beyonce_id = undock_kv["beyonceBoundID"].as_i64().unwrap() as u64;
    let ship_id = undock_kv["shipID"].as_i64().unwrap();

    // #111: Cross-system DB asteroid must not be cloned into caller's system
    let foreign_ast_id = 755_000_999_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.asteroids.insert(
            foreign_ast_id,
            eve_db::AsteroidRecord {
                asteroid_id: foreign_ast_id,
                belt_id: 40_000_999,
                solar_system_id: 30_002_187, // Amarr
                type_id: 1230,
                quantity: 5_000,
                initial_quantity: 5_000,
                position: (0.0, 0.0, 0.0),
                radius: 50.0,
            },
        );
    });
    let cross_sys_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(beyonce_id),
            "CmdMineRepeatedly",
            &[PyRep::Integer(foreign_ast_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Cross-system asteroid must be rejected");
    assert_eq!(cross_sys_err.code, "TargetNotInSystem");

    // #112: warpVectorMgr.UseVector rejects self-targeting and caps displacement
    let self_vec_err = ctx
        .dispatch_rpc(
            &mut sess,
            "warpVectorMgr",
            None,
            "UseVector",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Self-targeting UseVector must fail");
    assert_eq!(self_vec_err.code, "InvalidArgument");

    // #113: Data/Relic/Ghost sites cannot be completed via EnterCombatAnomaly while containers are unhacked
    eve_server::services::pve_exploration::ensure_system_pve_sites_seeded(&ctx, 30_000_142).await;
    let unhacked_err = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "EnterCombatAnomaly",
            &[PyRep::String("DAT-30000142".into()), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect_err("Data site with unhacked cans must reject wave completion");
    assert_eq!(unhacked_err.code, "SiteContainersUnhacked");

    // #115: Capsules cannot join Proving Grounds queue
    let fil_id = 755_001_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 670; // Capsule
        }
        snap.items.insert(
            fil_id,
            eve_db::InventoryItemRecord {
                item_id: fil_id,
                type_id: 56_303,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let pod_pvp_err = ctx
        .dispatch_rpc(
            &mut sess,
            "pvpFilamentMgr",
            None,
            "JoinPVPQueue",
            &[PyRep::Integer(fil_id), PyRep::Integer(56_303)],
            &kw,
            None,
        )
        .await
        .expect_err("Capsule must be rejected from JoinPVPQueue");
    assert_eq!(pod_pvp_err.code, "InvalidShipForFilament");

    // #116: GetExtractionsForCorp rejects foreign corporation IDs
    let foreign_corp_err = ctx
        .dispatch_rpc(
            &mut sess,
            "moonExtractions",
            None,
            "GetExtractionsForCorp",
            &[PyRep::Integer(98_999_999)],
            &kw,
            None,
        )
        .await
        .expect_err("Foreign corp moon extraction query must be denied");
    assert_eq!(foreign_corp_err.code, "AccessDenied");

    // #117: award_fw_kill_lp does not double-credit LP or credit unenlisted pilots as Caldari
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = None;
            ch.loyalty_points.clear();
        }
    });
    let _ = eve_server::services::fw_services::award_fw_kill_lp(&ctx, char_id, 30_002_537, 100_000_000.0);
    let unenlisted_lp_empty = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].loyalty_points.is_empty());
    assert!(unenlisted_lp_empty, "Unenlisted character must not receive Caldari LP");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_002); // Minmatar Republic
        }
    });
    let awarded = eve_server::services::fw_services::award_fw_kill_lp(&ctx, char_id, 30_002_537, 100_000_000.0);
    let total_lp: i64 = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].loyalty_points.values().copied().sum());
    assert_eq!(total_lp, awarded, "Enlisted pilot must receive single militia corp LP without double-crediting");
}
