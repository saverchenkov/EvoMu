//! Acceptance test suite for Advanced Mining, Moons, Sites & Ledgers (`Phase 5M`: `MN-13`..`MN-16`).
//!
//! Validates canonical tasks `plan:MN-13`, `plan:MN-14`, `plan:MN-15`, `plan:MN-16`,
//! and `notify:OnMoonExtractionUpdate` against the authoritative "Done when" criteria in `action_plan.md`.

#![forbid(unsafe_code)]

use crate::action_plan_mining::{get_dict_val, make_item, make_structure, setup_pilot_session};
use crate::common;
use eve_db::{AsteroidRecord, InventoryFlag, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::mining_sites;
use glam::DVec3;
use std::collections::HashMap;

fn decode_crowset_rows(rep: &PyRep) -> Vec<Vec<PyRep>> {
    if let PyRep::ObjectEx { list, .. } = rep {
        return list
            .iter()
            .filter_map(|r| match r {
                PyRep::DBRow { columns, .. } => {
                    Some(columns.iter().map(|(_, v)| v.clone()).collect())
                }
                PyRep::List(values) | PyRep::Tuple(values) => Some(values.clone()),
                _ => None,
            })
            .collect();
    }
    Vec::new()
}

#[tokio::test]
async fn test_action_plan_mn13_to_mn16_moons_metenox_sites_ledgers() {
    // parity: plan:MN-13, plan:MN-14, plan:MN-15, plan:MN-16, notify:OnMoonExtractionUpdate
    eve_testkit::attest(&[
        "plan:MN-13",
        "plan:MN-14",
        "plan:MN-15",
        "plan:MN-16",
        "notify:OnMoonExtractionUpdate",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001i64;
    let mut sess = setup_pilot_session(&ctx, char_id).await;

    let undock = ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
    let jita = ctx.systems.get_or_spawn(30000142);

    // 1. plan:MN-13 & notify:OnMoonExtractionUpdate: Moon extraction start, cancel, fracture, ore mix, jackpot & corp ledger
    let moon_structure_id = 1_000_000_001_030i64;
    let jackpot_structure_id = 1_000_000_001_032i64;
    let metenox_id = 1_000_000_001_031i64;
    let char_corp_id = ctx.db.with_snapshot(|snap| snap.characters[&char_id].corporation_id);

    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3386u32, 3410, 17940, 32918, 3429] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        let corp = snap.corporations.entry(char_corp_id).or_insert_with(|| eve_db::CorporationRecord {
            corporation_id: char_corp_id,
            name: "Test Mining Corp".into(),
            ticker: "TMC".into(),
            ceo_id: char_id,
            alliance_id: None,
            tax_rate: 0.0,
            master_wallet: eve_db::IskCents(0),
            wallet_divisions: Vec::new(),
            description: String::new(),
            url: String::new(),
            hq_station_id: Some(60003760),
            members: Default::default(),
            member_roles: Default::default(),
            applications: Default::default(),
        });
        corp.ceo_id = char_id;
        corp.member_roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
        snap.structures.insert(
            moon_structure_id,
            make_structure(moon_structure_id, char_corp_id, 35835, 30000142, "Moon Athanor", [1000.0, 2000.0, 3000.0]),
        );
        snap.structures.insert(
            jackpot_structure_id,
            make_structure(jackpot_structure_id, char_corp_id, 35835, 31000007, "Wormhole Athanor", [1000.0, 2000.0, 3000.0]),
        );
        let mut metenox = make_structure(metenox_id, char_corp_id, 81826, 30000012, "Asabona Metenox", [200_000.0, 0.0, -100_000.0]);
        metenox.fuel_blocks = 200;
        snap.structures.insert(metenox_id, metenox);
        snap.items.insert(
            800_000_701,
            make_item(800_000_701, 81143, char_corp_id, metenox_id, InventoryFlag::Hangar, 500, false), // 500 Magmatic Gas
        );
        let mut outsider_ch = snap.characters[&char_id].clone();
        outsider_ch.character_id = 90_000_002;
        outsider_ch.corporation_id = 98_999_999;
        outsider_ch.structure_id = Some(moon_structure_id);
        snap.characters.insert(90_000_002, outsider_ch);
        let mut unpriv_ch = snap.characters[&char_id].clone();
        unpriv_ch.character_id = 90_000_003;
        unpriv_ch.structure_id = Some(moon_structure_id);
        snap.characters.insert(90_000_003, unpriv_ch);
        snap.characters.get_mut(&char_id).unwrap().structure_id = Some(moon_structure_id);
        snap.corporations.get_mut(&char_corp_id).unwrap().member_roles.insert(90_000_003, 0);
    });

    // Corp ACL denial: outsider (foreign corp) and unprivileged member (role 0) must be denied StartNewExtraction & FractureChunkWithStructure
    for (denied_cid, denied_corp) in [(90_000_002i64, 98_999_999i64), (90_000_003i64, char_corp_id)] {
        let mut d_sess = sess.clone();
        d_sess.character_id = Some(denied_cid);
        d_sess.corporation_id = Some(denied_corp);
        d_sess.structure_id = Some(moon_structure_id);
        d_sess.corp_role = 0;
        assert_eq!(ctx.dispatch_rpc(&mut d_sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(518_400)], &kw, None).await.unwrap_err().code, "AccessDenied");
        assert_eq!(ctx.dispatch_rpc(&mut d_sess, "moonExtractions", None, "FractureChunkWithStructure", &[], &kw, None).await.unwrap_err().code, "AccessDenied");
    }

    sess.structure_id = Some(moon_structure_id);
    ctx.drain_notifications_for_char(char_id);
    let start_cancel = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(518_400)], &kw, None)
        .await
        .expect("StartNewExtraction before cancel");
    assert_eq!(get_dict_val(&start_cancel, "status").unwrap().as_str(), Some("Started"));
    assert!(
        ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnMoonExtractionUpdate"),
        "StartNewExtraction must emit OnMoonExtractionUpdate"
    );

    let cancel_res = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "CancelExtraction", &[], &kw, None)
        .await
        .expect("0-arg CancelExtraction from session structure_id");
    assert_eq!(cancel_res, PyRep::Bool(true));
    assert!(
        ctx.drain_notifications_for_char(char_id)
            .iter()
            .any(|n| n.event == "OnMoonExtractionUpdate" && n.args == vec![PyRep::None]),
        "CancelExtraction must emit OnMoonExtractionUpdate(None)"
    );

    let dur_secs = 864_000u64; // 10 days (240h -> 7,200,000 m³ chunk)
    let start_res = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(dur_secs as i64)], &kw, None)
        .await
        .expect("StartNewExtraction 10d");
    let PyRep::List(planned_ores) = get_dict_val(&start_res, "moonOres").unwrap() else { panic!() };
    assert_eq!(planned_ores.len(), 3, "Moon composition must contain 3 SDE moon ores");

    // Firing moon drill laser before chunkArrivalTime must fail with ChunkNotReadyToFracture
    let early_err = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "FractureChunkWithStructure", &[], &kw, None)
        .await
        .expect_err("0-arg FractureChunkWithStructure before chunkArrivalTime must fail");
    assert_eq!(early_err.code, "ChunkNotReadyToFracture");

    ctx.advance_secs(dur_secs + 1);
    let non_jp_seed = (1u64..10_000)
        .find(|&s| eve_server::rng::MechanicRng::seeded(s).try_unit_f64().unwrap() >= 0.15)
        .unwrap();
    ctx.set_mechanic_rng_seed(Some(non_jp_seed));
    ctx.drain_notifications_for_char(char_id);
    let frac_res = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "FractureChunkWithStructure", &[], &kw, None)
        .await
        .expect("0-arg FractureChunkWithStructure");
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "CancelExtraction", &[], &kw, None).await.unwrap_err().code,
        "CannotCancelExtraction",
        "CancelExtraction must be rejected after chunk has fractured"
    );
    let frac_notifs = ctx.drain_notifications_for_char(char_id);
    let frac_update = frac_notifs
        .iter()
        .find(|n| n.event == "OnMoonExtractionUpdate")
        .expect("FractureChunkWithStructure must emit OnMoonExtractionUpdate");
    assert_eq!(get_dict_val(&frac_update.args[0], "status").and_then(|v| v.as_str()), Some("Fractured"));
    assert!(get_dict_val(&frac_update.args[0], "moonID").and_then(|v| v.as_i64()).is_some(), "OnMoonExtractionUpdate must include moonID");
    assert!(get_dict_val(&frac_update.args[0], "extractionStartTime").and_then(|v| v.as_i64()).is_some(), "OnMoonExtractionUpdate must include extractionStartTime");

    let PyRep::List(ast_ids) = get_dict_val(&frac_res, "asteroidIDs").unwrap() else { panic!() };
    let PyRep::List(ore_types) = get_dict_val(&frac_res, "oreTypes").unwrap() else { panic!() };
    assert_eq!(ast_ids.len(), 6, "Non-jackpot fracture spawns 6 asteroids (base + +15% Brimful variant for each of the 3 moon ores)");
    assert!(ore_types.iter().any(|t| ctx.sde.types[&(t.as_i64().unwrap() as u32)].name.starts_with("Brimful ")));

    // Mine the fractured moon asteroid 2 cycles and 2 belt ores (Veldspar + Scordite) so 3 distinct ore types are logged
    let first_moon_ast = ast_ids[0].as_i64().unwrap();
    let first_moon_type = ore_types[0].as_i64().unwrap() as u32;
    let miner_mod_id = 800_000_702i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 32880; // Venture
        }
        snap.items.retain(|_, i| i.location_id != ship_id);
        snap.items.insert(miner_mod_id, make_item(miner_mod_id, 483, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
    });
    ctx.invalidate_ship_fitting(ship_id);

    let moon_ast_pos = jita.get_ball_snapshot(first_moon_ast).await.unwrap().unwrap().1.position;
    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = moon_ast_pos + DVec3::new(500.0, 0.0, 0.0);
        let _ = jita.spawn_ball(sb).await;
    }
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().structure_id = None);
    sess.structure_id = None;
    let moon_c1 = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, miner_mod_id, first_moon_ast).await.expect("Moon cycle 1");
    let moon_c2 = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, miner_mod_id, first_moon_ast).await.expect("Moon cycle 2");
    let total_moon_mined = moon_c1.units_delivered + moon_c2.units_delivered;
    assert!(total_moon_mined > 0);

    // Advance past V24 48h (172,800s) natural decay window, run production tick_background_cluster, and verify moon asteroid belt despawns
    ctx.advance_secs(172_800);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().structure_id = Some(moon_structure_id));
    sess.structure_id = Some(moon_structure_id);
    let PyRep::Tuple(decayed_tup) = ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "GetExtractionAndEventIDFromStructureID", &[], &kw, None).await.expect("0-arg GetExtractionAndEventIDFromStructureID after 48h decay") else { panic!("Expected 2-tuple (extraction, eventID)") };
    assert_eq!(decayed_tup.len(), 2);
    assert_eq!(get_dict_val(&decayed_tup[0], "status").and_then(|v| v.as_str()), Some("Decayed"));
    assert_eq!(get_dict_val(&decayed_tup[0], "activeAsteroidCount").and_then(|v| v.as_i64()), Some(0));
    assert!(decayed_tup[1].as_i64().is_some());
    assert!(ctx.db.get_asteroid(first_moon_ast).is_none() && jita.get_ball_snapshot(first_moon_ast).await.unwrap().is_none(), "Expired moon asteroid field must despawn after 48h V24 duration");

    // Also verify Jackpot (+100% variants) and auto-fracture after chunkArrivalTime + 3h (10,800s) on jackpot_structure_id via tick_background_cluster
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().structure_id = Some(jackpot_structure_id));
    sess.structure_id = Some(jackpot_structure_id);
    ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(518_400)], &kw, None)
        .await
        .expect("StartNewExtraction on jackpot_structure_id for auto-fracture");
    ctx.advance_secs(518_400 + 10_801);
    ctx.drain_notifications_for_char(char_id);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let PyRep::Tuple(auto_tup) = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "GetExtractionAndEventIDFromStructureID", &[], &kw, None)
        .await
        .expect("0-arg GetExtractionAndEventIDFromStructureID after 3h auto-fracture delay")
    else {
        panic!("Expected 2-tuple");
    };
    assert_eq!(get_dict_val(&auto_tup[0], "status").and_then(|v| v.as_str()), Some("Fractured"));
    assert_eq!(get_dict_val(&auto_tup[0], "autoFractured").and_then(|v| v.as_bool()), Some(true));
    let PyRep::List(auto_ast_ids) = get_dict_val(&auto_tup[0], "asteroidIDs").unwrap() else { panic!() };
    let first_auto_ast = auto_ast_ids[0].as_i64().unwrap();
    assert!(
        ctx.drain_notifications_for_char(char_id)
            .iter()
            .any(|n| n.event == "OnMoonExtractionUpdate" && get_dict_val(&n.args[0], "autoFractured").and_then(|v| v.as_bool()) == Some(true)),
        "Auto-fracture after 3h delay must emit OnMoonExtractionUpdate with autoFractured == true"
    );

    ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(518_400)], &kw, None)
        .await
        .expect("StartNewExtraction on jackpot_structure_id");
    ctx.advance_secs(518_401);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    assert!(ctx.db.get_asteroid(first_auto_ast).is_none(), "Previous auto-fractured field must still despawn after its 48h window even after a new extraction starts");
    let jp_seed = (1u64..10_000)
        .find(|&s| eve_server::rng::MechanicRng::seeded(s).try_unit_f64().unwrap() < 0.15)
        .unwrap();
    ctx.set_mechanic_rng_seed(Some(jp_seed));
    let jp_frac = ctx
        .dispatch_rpc(&mut sess, "moonExtractions", None, "FractureChunkWithStructure", &[], &kw, None)
        .await
        .expect("0-arg FractureChunkWithStructure jackpot");
    assert_eq!(get_dict_val(&jp_frac, "isJackpot").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(get_dict_val(&jp_frac, "jackpotBonus").and_then(|v| v.as_f64()), Some(2.0));
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().structure_id = None);
    sess.structure_id = None;

    // 2. plan:MN-14: Metenox Moon Drill — server-driven hourly harvesting into SpecializedMineralHold (40% yield, 5 fuel + 10 Magmatic Gas/h)
    // Reset Metenox fuel/gas and clear SpecializedMineralHold so we measure exact 0h -> 24h -> fuel-out transitions on the production background cluster tick
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&metenox_id).unwrap().fuel_blocks = 200;
        snap.items.retain(|_, i| !(i.location_id == metenox_id && i.flag == InventoryFlag::SpecializedMineralHold));
        snap.items.insert(800_000_701, make_item(800_000_701, 81143, char_corp_id, metenox_id, InventoryFlag::Hangar, 500, false));
        snap.auto_moon_miners.remove(&metenox_id);
        snap.service_state_blobs.insert("metenox_last_tick_unix".into(), ctx.now_unix().to_string());
    });

    // Independently compute expected hourly 40% moon material yield from SDE type_materials for the Metenox moon's 3 ores (727 units/h)
    let PyRep::List(met_ores) = ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "GetMoonResourcesForStructure", &[PyRep::Integer(metenox_id)], &kw, None).await.expect("GetMoonResourcesForStructure Metenox") else { panic!() };
    let mut sde_mat_totals: std::collections::BTreeMap<u32, f64> = std::collections::BTreeMap::new();
    for kv in &met_ores {
        let tid = get_dict_val(kv, "typeID").unwrap().as_i64().unwrap() as u32;
        let frac = get_dict_val(kv, "yield").unwrap().as_f64().unwrap();
        let portions = (30_000.0 * frac) / (ctx.sde.types[&tid].volume.max(0.1) * (ctx.sde.types[&tid].portion_size.max(1) as f64));
        for m in ctx.sde.type_materials[&tid].iter().filter(|m| ctx.sde.types[&m.material_type_id].group_id == 427) {
            *sde_mat_totals.entry(m.material_type_id).or_insert(0.0) += portions * (m.quantity as f64) * 0.40;
        }
    }
    let expected_hourly_sde: i64 = sde_mat_totals.values().map(|q| q.round() as i64).sum();
    assert_eq!(expected_hourly_sde, 727);

    let details_0h = ctx
        .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(metenox_id)], &kw, None)
        .await
        .expect("RequestMiningDetails at 0h");
    assert_eq!(get_dict_val(&details_0h, "efficiency").and_then(|v| v.as_f64()), Some(0.40));
    assert_eq!(get_dict_val(&details_0h, "fuelRemaining").and_then(|v| v.as_i64()), Some(200));
    assert_eq!(get_dict_val(&details_0h, "magmaticGasRemaining").and_then(|v| v.as_i64()), Some(500));
    assert!(get_dict_val(&details_0h, "availableFuel").is_some() && get_dict_val(&details_0h, "nextHarvestTime").is_some());
    assert!(get_dict_val(&details_0h, "outputMaterials").is_some() && get_dict_val(&details_0h, "remainingOutputBayCapacity").is_some());
    let hourly_yield = get_dict_val(&details_0h, "hourlyYield").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(hourly_yield, expected_hourly_sde, "hourlyYield must match independently computed SDE 40% moon material yield");

    // RequestMiningCycleOutput is a read-only query returning a list of (type_id, qty, vol) tuples and does NOT trigger harvesting
    let cyc_rate_res = ctx
        .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningCycleOutput", &[PyRep::Integer(metenox_id)], &kw, None)
        .await
        .expect("RequestMiningCycleOutput read-only query");
    let PyRep::List(rate_tuples) = &cyc_rate_res else { panic!("Expected List of (type_id, qty, vol) tuples") };
    let rate_sum: i64 = rate_tuples
        .iter()
        .map(|t| match t {
            PyRep::Tuple(el) => el[1].as_i64().unwrap(),
            _ => panic!("Expected Tuple"),
        })
        .sum();
    assert_eq!(rate_sum, expected_hourly_sde, "Read-only RequestMiningCycleOutput per-cycle quantities sum to SDE hourlyYield");
    let bay_qty_0h: i32 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == metenox_id && i.flag == InventoryFlag::SpecializedMineralHold)
            .map(|i| i.quantity)
            .sum()
    });
    assert_eq!(bay_qty_0h, 0, "Zero output exists before advancing the server clock, even after calling RequestMiningCycleOutput");

    // Advance 24 simulated hours (86,400 seconds) and run production tick_background_cluster: harvests 24 hourly cycles into SpecializedMineralHold
    ctx.clock().advance_secs(24 * 3_600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let details_24h = ctx
        .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(metenox_id)], &kw, None)
        .await
        .expect("RequestMiningDetails after 24h");
    assert_eq!(get_dict_val(&details_24h, "fuelRemaining").and_then(|v| v.as_i64()), Some(80), "24h consumes 120 fuel blocks (5/h -> 80 left)");
    assert_eq!(get_dict_val(&details_24h, "magmaticGasRemaining").and_then(|v| v.as_i64()), Some(260), "24h consumes 240 Magmatic Gas (10/h -> 260 left)");
    let bay_qty_24h: i64 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == metenox_id && i.flag == InventoryFlag::SpecializedMineralHold)
            .map(|i| {
                assert_eq!(ctx.sde.types[&i.type_id].group_id, 427, "Deposited item must be group 427 Moon Material");
                i.quantity as i64
            })
            .sum()
    });
    assert_eq!(bay_qty_24h, expected_hourly_sde * 24, "24 simulated hours deposit 24x SDE hourly 40% yield into SpecializedMineralHold");
    assert!(
        ctx.db.with_snapshot(|snap| snap.observer_mining_ledger.get(&metenox_id).is_none_or(|v| v.is_empty())),
        "Automated Metenox harvesting must not write pilot character rows to observer_mining_ledger"
    );

    // Advance another 20 hours and run production tick_background_cluster: only 16 cycles run (80 / 5 = 16h) before fuel blocks hit 0 and harvesting halts with status OutOfFuel
    ctx.clock().advance_secs(20 * 3_600);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let details_exhausted = ctx
        .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(metenox_id)], &kw, None)
        .await
        .expect("RequestMiningDetails after fuel exhaustion");
    assert_eq!(get_dict_val(&details_exhausted, "fuelRemaining").and_then(|v| v.as_i64()), Some(0));
    assert_eq!(get_dict_val(&details_exhausted, "status").and_then(|v| v.as_str()), Some("OutOfFuel"));
    let bay_qty_44h: i64 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == metenox_id && i.flag == InventoryFlag::SpecializedMineralHold)
            .map(|i| i.quantity as i64)
            .sum()
    });
    assert_eq!(bay_qty_44h, expected_hourly_sde * 40, "Harvesting halts after 40 total funded hours when fuel is exhausted");

    // 3. plan:MN-15: Mining Sites — Ore anomaly, ice belt, and gas site spawn with SDE ore/ice/gas sets, probe scan resolution & despawn after depletion
    let osmon_id = 30_000_180u32; // Osmon (0.7 high-sec)
    mining_sites::seed_system_mining_sites_from_sde(&ctx, osmon_id).await;
    mining_sites::seed_system_mining_sites_from_sde(&ctx, 30_002_537).await; // Amamake (Low-Sec -> Saintly Nebula gas site)
    let ice_belt = mining_sites::resolve_regional_ice_belt(&ctx.sde, osmon_id).unwrap().expect("Blue Ice Belt in Osmon");
    assert_eq!((ice_belt.name.as_str(), ice_belt.asteroids.len(), ice_belt.ore_types(), mining_sites::ICE_BELT_RESPAWN_DELAY_SECS), ("Blue Ice Belt", 21, vec![16264], 14_400));
    let (gas_sig_id, gas_pos) = ctx.db.with_snapshot(|snap| snap.cosmic_signatures[&30_002_537].iter().find(|s| s.sig_group == "Gas" && s.type_name == "Saintly Nebula").map(|s| (s.signature_id.clone(), DVec3::from_array(s.position))).expect("Saintly Nebula gas sig"));
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().solar_system_id = 30_002_537);
    sess.solar_system_id = Some(30_002_537);
    let ls_scan_rows = crate::common::probe_scan(&ctx, &mut sess, gas_pos, 0.25 * 149_597_870_700.0).await;
    assert_eq!(crate::common::scan_row_field(&ls_scan_rows, &gas_sig_id, "signalStrength").as_f64(), Some(1.0), "scanMgr.RequestScans must resolve Saintly Nebula gas signature to 100%");
    assert_eq!(crate::common::scan_row_field(&ls_scan_rows, &gas_sig_id, "type").as_str(), Some("Saintly Nebula"));
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().solar_system_id = osmon_id);
    sess.solar_system_id = Some(osmon_id);
    let osmon = ctx.systems.get_or_spawn(osmon_id);

    let scan_before = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr");
    let PyRep::List(anoms_before) = get_dict_val(&scan_before, "anomalies").unwrap() else { panic!() };
    let ore_anom = anoms_before
        .iter()
        .find(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Small Kernite and Omber Deposit"))
        .expect("Small Kernite and Omber Deposit anomaly in Osmon");
    assert!(ctx.db.with_snapshot(|snap| snap.cosmic_signatures[&osmon_id].iter().any(|s| s.sig_group == "Ore" && s.type_name == "Hidden Omber Deposit")), "Hidden Omber Deposit ore signature in Osmon");
    let PyRep::List(anom_ast_reps) = get_dict_val(ore_anom, "asteroidBallIDs").unwrap() else { panic!() };
    let anom_ast_ids: Vec<i64> = anom_ast_reps.iter().map(|v| v.as_i64().unwrap()).collect();
    assert_eq!(anom_ast_ids.len(), 18, "Osmon ore anomaly spawns 18 SDE dungeon asteroids");
    let mut anom_ore_types: Vec<u32> = ctx.db.with_snapshot(|snap| anom_ast_ids.iter().map(|id| snap.asteroids[id].type_id).collect());
    anom_ore_types.sort_unstable();
    anom_ore_types.dedup();
    assert_eq!(anom_ore_types, vec![20, 1227, 17452, 17453, 17867, 17868], "Small Kernite and Omber Deposit must spawn exact SDE Kernite & Omber variant set");

    // Clear all but the last anomaly rock, then mine the final rock to 0 and verify the anomaly despawns
    let last_anom_rock = *anom_ast_ids.last().unwrap();
    let last_rock_pos = ctx.db.with_snapshot(|snap| {
        let p = snap.asteroids[&last_anom_rock].position;
        DVec3::new(p.0, p.1, p.2)
    });
    ctx.db.with_mut_snapshot(|snap| {
        for &id in &anom_ast_ids[..anom_ast_ids.len() - 1] {
            if let Some(a) = snap.asteroids.get_mut(&id) {
                a.quantity = 0;
            }
        }
        if let Some(a) = snap.asteroids.get_mut(&last_anom_rock) {
            a.quantity = 10;
        }
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold));
    });
    osmon
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 32880,
            position: last_rock_pos + DVec3::new(500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_200_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    let clear_cycle = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, miner_mod_id, last_anom_rock)
        .await
        .expect("Mine final anomaly asteroid to 0");
    assert_eq!(clear_cycle.asteroid_remaining, 0);
    let scan_after = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr after clear");
    let PyRep::List(anoms_after) = get_dict_val(&scan_after, "anomalies").unwrap() else { panic!() };
    assert!(
        !anoms_after.iter().any(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Small Kernite and Omber Deposit")),
        "Cleared ore anomaly must despawn from scanSvc.GetSystemScanMgr"
    );

    // Verify regional Ice Belt ("Blue Ice Belt") spawned in Osmon cosmic_anomalies, depletes & despawns, and respawns after 4h (14,400s)
    let ice_anom = anoms_after
        .iter()
        .find(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Blue Ice Belt"))
        .expect("Blue Ice Belt cosmic anomaly spawned in Osmon");
    let PyRep::List(ice_ast_reps) = get_dict_val(ice_anom, "asteroidBallIDs").unwrap() else { panic!() };
    let ice_ast_ids: Vec<i64> = ice_ast_reps.iter().map(|v| v.as_i64().unwrap()).collect();
    assert_eq!(ice_ast_ids.len(), 21, "Blue Ice Belt spawns 21 SDE Blue Ice (16264) asteroids");
    assert!(ctx.db.with_snapshot(|snap| ice_ast_ids.iter().all(|id| snap.asteroids[id].type_id == 16264 && snap.asteroids[id].quantity == 1_000)));
    let last_ice_rock = *ice_ast_ids.last().unwrap();
    let last_ice_pos = ctx.db.with_snapshot(|snap| {
        let p = snap.asteroids[&last_ice_rock].position;
        DVec3::new(p.0, p.1, p.2)
    });
    let ice_harv_id = 800_000_720i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 22544; // Hulk (can fit Ice Harvester I)
        snap.items.insert(ice_harv_id, make_item(ice_harv_id, 16278, char_id, ship_id, InventoryFlag::HiSlot1, 1, true));
        snap.skills.get_mut(&char_id).unwrap().insert(16281, eve_db::TrainedSkillRecord { type_id: 16281, level: 5, skill_points: 256_000.0 });
        for &id in &ice_ast_ids[..ice_ast_ids.len() - 1] {
            if let Some(a) = snap.asteroids.get_mut(&id) {
                a.quantity = 0;
            }
        }
        if let Some(a) = snap.asteroids.get_mut(&last_ice_rock) {
            a.quantity = 1;
        }
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold));
    });
    ctx.invalidate_ship_fitting(ship_id);
    osmon.try_set_ball_position(ship_id, last_ice_pos + DVec3::new(500.0, 0.0, 0.0));
    let ice_clear = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, ice_harv_id, last_ice_rock)
        .await
        .expect("Mine final Blue Ice asteroid to 0");
    assert_eq!(ice_clear.asteroid_remaining, 0);
    let scan_ice_cleared = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr after ice clear");
    let PyRep::List(anoms_ice_cleared) = get_dict_val(&scan_ice_cleared, "anomalies").unwrap() else { panic!() };
    assert!(
        !anoms_ice_cleared.iter().any(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Blue Ice Belt")),
        "Depleted Blue Ice Belt must despawn from scanSvc"
    );
    ctx.advance_secs(14_399);
    let scan_before_4h = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr at 4h - 1s");
    let PyRep::List(anoms_before_4h) = get_dict_val(&scan_before_4h, "anomalies").unwrap() else { panic!() };
    assert!(
        !anoms_before_4h.iter().any(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Blue Ice Belt")),
        "Blue Ice Belt must remain despawned at 14,399s (< 4h)"
    );
    ctx.advance_secs(1);
    let scan_at_4h = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr at 4h");
    let PyRep::List(anoms_at_4h) = get_dict_val(&scan_at_4h, "anomalies").unwrap() else { panic!() };
    let respawned_ice = anoms_at_4h
        .iter()
        .find(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Blue Ice Belt"))
        .expect("Blue Ice Belt must respawn 4 hours (14,400s) after depletion");
    let PyRep::List(respawned_ice_balls) = get_dict_val(respawned_ice, "asteroidBallIDs").unwrap() else { panic!() };
    assert_eq!(respawned_ice_balls.len(), 21, "Respawned Blue Ice Belt has 21 fresh Blue Ice asteroids");

    // 4. plan:MN-16: Character & Corp Mining Ledgers (90-day retention filter: 60d retained, 91d pruned + waste tracking + outsider CrpAccessDenied)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 32880;
        snap.items.remove(&ice_harv_id);
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold));
    });
    ctx.invalidate_ship_fitting(ship_id);
    let scordite_rock = 750_000_710i64;
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: scordite_rock,
        solar_system_id: osmon_id,
        belt_id: 40_000_001,
        type_id: 1228, // Scordite
        quantity: 10_000,
        initial_quantity: 10_000,
        position: (last_ice_pos.x + 600.0, last_ice_pos.y, last_ice_pos.z),
        radius: 60.0,
    });
    let _ = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, miner_mod_id, scordite_rock).await.expect("Mine Scordite");
    let retained_60d_ts = ctx.now_unix().saturating_sub(60 * 86_400);
    let expired_ts = ctx.now_unix().saturating_sub(91 * 86_400);
    ctx.db.record_mining_ledger_entry_with_waste(char_id, osmon_id, 17452, 777, 111, retained_60d_ts).expect("Record 60d Silvery Omber with waste");
    ctx.db.with_mut_snapshot(|snap| {
        snap.mining_ledger.push(eve_db::MiningLedgerRecord {
            character_id: char_id, solar_system_id: osmon_id, type_id: 18, quantity: 999, date: "2026-01-01".into(), timestamp_unix: expired_ts,
        });
        snap.observer_mining_ledger.entry(moon_structure_id).or_default().push(eve_db::ObserverMiningRecord {
            observer_id: moon_structure_id, character_id: char_id, recorded_corporation_id: char_corp_id, type_id: 18, quantity: 999, last_updated_date: "2026-01-01".into(), timestamp_unix: expired_ts,
        });
    });

    let char_ledger = ctx
        .dispatch_rpc(&mut sess, "characterMiningLedger", None, "GetCharacterLogs", &[], &kw, None)
        .await
        .expect("GetCharacterLogs");
    let PyRep::List(char_entries) = &char_ledger else { panic!() };
    assert!(
        !char_entries.iter().any(|e| get_dict_val(e, "typeID").and_then(|v| v.as_i64()) == Some(18)),
        "GetCharacterLogs must prune 91-day-old Plagioclase (18) entry"
    );
    let omber_60d_row = char_entries
        .iter()
        .find(|e| get_dict_val(e, "typeID").and_then(|v| v.as_i64()) == Some(17452))
        .expect("60-day-old Silvery Omber (17452) entry must be retained within 90-day ledger window");
    assert_eq!(get_dict_val(omber_60d_row, "quantity").and_then(|v| v.as_i64()), Some(777));
    assert_eq!(get_dict_val(omber_60d_row, "quantityWasted").and_then(|v| v.as_i64()), Some(111));
    assert_eq!(get_dict_val(omber_60d_row, "waste").and_then(|v| v.as_i64()), Some(111));
    let moon_char_row = char_entries
        .iter()
        .find(|e| get_dict_val(e, "typeID").and_then(|v| v.as_i64()) == Some(first_moon_type as i64))
        .expect("Aggregated moon ore row in character ledger");
    assert_eq!(get_dict_val(moon_char_row, "quantity").and_then(|v| v.as_i64()), Some(total_moon_mined));

    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let corp_ledger = ctx
        .dispatch_rpc(&mut sess, "corpMiningLedger", None, "GetObserverLedger", &[PyRep::Integer(moon_structure_id)], &kw, None)
        .await
        .expect("GetObserverLedger for owner corp director");
    assert!(corp_ledger.is_crowset());
    let obs_rows = decode_crowset_rows(&corp_ledger);
    assert!(!obs_rows.iter().any(|r| r.get(2).and_then(|v| v.as_i64()) == Some(18)), "GetObserverLedger must exclude 91-day-old expired entries");
    let moon_obs_rows: Vec<_> = obs_rows
        .iter()
        .filter(|r| r.first().and_then(|v| v.as_i64()) == Some(char_id) && r.get(2).and_then(|v| v.as_i64()) == Some(first_moon_type as i64))
        .collect();
    assert_eq!(moon_obs_rows.len(), 1, "Both moon mining cycles aggregate into 1 observer ledger row");
    assert_eq!(moon_obs_rows[0].get(3).and_then(|v| v.as_i64()), Some(total_moon_mined));

    // Outsider character (different corporation) must be denied access to corp observer ledger
    let outsider_char = 90_000_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&char_id].clone();
        ch.character_id = outsider_char;
        ch.corporation_id = 98_999_999;
        snap.characters.insert(outsider_char, ch);
    });
    let mut outsider_sess = sess.clone();
    outsider_sess.character_id = Some(outsider_char);
    outsider_sess.corporation_id = Some(98_999_999);
    let outsider_err = ctx
        .dispatch_rpc(&mut outsider_sess, "corpMiningLedger", None, "GetObserverLedger", &[PyRep::Integer(moon_structure_id)], &kw, None)
        .await
        .expect_err("Outsider must be denied GetObserverLedger");
    assert_eq!(outsider_err.code, "CrpAccessDenied");
}
